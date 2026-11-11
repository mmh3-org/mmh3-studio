use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, Hasher};
use std::path::PathBuf;
use std::sync::Arc;

use crate::api::{self, Generation, ImageRequest, Status};
use crate::power::{Issue, Power};
use crate::sizes::{self, PRESETS, SizeError};
use rune::machine::Effects;
use rune::{Image, ImageCache, PointerPress, TextEdit, Tx};
use rune_kit::image_zoom::StageZoom;
use rune_kit::shell::PaneState;

use crate::net::Net;

/// Thumbnails kept decoded, newest first; older rows show a placeholder.
pub const MAX_THUMBS: usize = 240;
pub const NOTE_SECONDS: f64 = 4.0;

pub enum Conn {
    Connecting,
    Connected(Status),
    /// The server answered, but not in a way the studio could read.
    Unreadable(String),
    Disconnected(String),
}

#[derive(Clone, Debug, PartialEq)]
pub enum Phase {
    /// Posted, waiting for the server's 202.
    Sending,
    Queued,
    /// `step` is how many steps are behind it: none while it reads the prompt, all of them
    /// while the picture decodes.
    Running {
        step: Option<u32>,
        steps: u32,
        since: f64,
    },
    Done {
        seconds: f64,
    },
    Failed(String),
}

impl Phase {
    pub fn is_active(&self) -> bool {
        matches!(self, Phase::Sending | Phase::Queued | Phase::Running { .. })
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Effect {
    Watch,
    FetchImage,
}

/// Feeds what the server says about a generation into its phase.
pub fn step(phase: &mut Phase, g: &Generation, now: f64) -> Effects<Effect> {
    let was = phase.clone();
    *phase = match g.state {
        api::State::Queued => Phase::Queued,
        api::State::Running => {
            let since = match was {
                Phase::Running { since, .. } => since,
                _ => now,
            };
            Phase::Running { step: g.step, steps: g.steps.unwrap_or(api::IMAGE_STEPS), since }
        }
        api::State::Done => Phase::Done { seconds: g.seconds.unwrap_or_default() },
        api::State::Failed => Phase::Failed(g.error.clone().unwrap_or_else(|| "The server gave no reason.".into())),
    };
    match (&was, &*phase) {
        (Phase::Done { .. }, _) => Effects::none(),
        (_, Phase::Done { .. }) => Effects::one(Effect::FetchImage),
        (Phase::Sending, p) if p.is_active() => Effects::one(Effect::Watch),
        _ => Effects::none(),
    }
}

/// What the server says of a generation the studio did not ask for.
#[derive(Clone, Debug, PartialEq)]
pub enum Probe {
    /// It writes a picture, and this is where it is now.
    Picture(Generation),
    /// It writes a video, or is gone.
    NotAPicture,
    Unanswered,
}

pub enum Picture {
    None,
    Loading,
    Ready { file: PathBuf, thumb: Option<Image> },
    Failed(String),
}

pub struct Gen {
    pub id: String,
    /// What the studio asked for; the server never says, so a picture asked for elsewhere has
    /// none.
    pub request: Option<ImageRequest>,
    /// When the server took it, which orders the gallery; none before it has.
    pub queued: Option<u64>,
    pub phase: Phase,
    pub picture: Picture,
    pub revision: u64,
}

impl Gen {
    pub fn from_server(g: &Generation, request: Option<ImageRequest>, now: f64) -> Gen {
        let mut item = Gen { id: g.id.clone(), request, queued: Some(g.queued), phase: Phase::Queued, picture: Picture::None, revision: 0 };
        let _ = step(&mut item.phase, g, now);
        item
    }

    pub fn prompt(&self) -> &str {
        self.request.as_ref().map_or("", |r| r.prompt.as_str())
    }

    pub fn seed(&self) -> Option<u64> {
        self.request.as_ref().map(|r| r.seed)
    }

    pub fn size(&self) -> Option<(u32, u32)> {
        self.request.as_ref().map(|r| (r.width, r.height))
    }

    pub fn references(&self) -> &[PathBuf] {
        self.request.as_ref().map_or(&[], |r| r.references.as_slice())
    }

    pub fn file(&self) -> Option<&PathBuf> {
        match &self.picture {
            Picture::Ready { file, .. } => Some(file),
            _ => None,
        }
    }

    pub fn is_local(&self) -> bool {
        self.id.starts_with("local-")
    }
}

/// The file types `mmh3 image` reads a reference from.
pub const REFERENCE_EXTENSIONS: [&str; 3] = ["png", "jpg", "jpeg"];

/// A picture the composer will ask the server to edit.
pub struct Reference {
    pub path: PathBuf,
    pub thumb: Thumb,
}

pub enum Thumb {
    Loading,
    Ready(Image),
    Failed(String),
}

/// The `<Picture n>` a prompt mentions that are not attached, in order and once each.
pub fn missing_pictures(prompt: &str, attached: usize) -> Vec<usize> {
    let mut missing: Vec<usize> = prompt
        .split("<Picture ")
        .skip(1)
        .filter_map(|rest| rest.split_once('>').and_then(|(n, _)| n.trim().parse().ok()))
        .filter(|&n: &usize| n == 0 || n > attached)
        .collect();
    missing.sort_unstable();
    missing.dedup();
    missing
}

pub fn picture_tag(n: usize) -> String {
    format!("<Picture {n}>")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SizeChoice {
    Preset(usize),
    Custom,
}

/// The one pane a compact window shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Pane {
    Gallery,
    #[default]
    Picture,
    Compose,
}

impl Pane {
    pub const ALL: [Pane; 3] = [Pane::Gallery, Pane::Picture, Pane::Compose];

    pub fn label(self) -> &'static str {
        match self {
            Pane::Gallery => "Generations",
            Pane::Picture => "Picture",
            Pane::Compose => "Compose",
        }
    }
}

pub struct Note {
    pub id: u64,
    pub title: String,
    pub detail: Option<String>,
    pub danger: bool,
    /// Stamped from the frame clock by the first poll that sees the note, so it stays up its
    /// full time however long the window sat idle before it arrived.
    pub until: Option<f64>,
}

pub struct State {
    pub net: Option<Net>,
    pub images: Arc<ImageCache>,
    pub server: String,
    pub conn: Conn,
    pub gens: Vec<Gen>,
    pub selected: Option<String>,
    pub prompt: TextEdit,
    pub size: SizeChoice,
    pub custom_w: TextEdit,
    pub custom_h: TextEdit,
    pub seed: TextEdit,
    pub references: Vec<Reference>,
    /// Files are being dragged over the composer.
    pub dropping: bool,
    /// The window's width in points, once laid out; below [`crate::view::COMPACT_BELOW`] the
    /// panes take turns.
    pub width: f32,
    pub pane: Pane,
    pub preview: Option<(String, Image)>,
    pub stage: StageZoom,
    pub sidebar: PaneState,
    pub inspector: PaneState,
    pub menu: Option<(String, PointerPress)>,
    pub deleting: Option<String>,
    pub notes: Vec<Note>,
    pub clock: f64,
    pub tick: u64,
    pub watching: HashSet<String>,
    /// What the studio asked each of its generations for, by the server's id.
    pub requests: HashMap<String, ImageRequest>,
    /// Generations the server accepted from the studio that no list has shown yet. A list
    /// fetched before the server took one can land after its answer, and must not read as the
    /// server having dropped it.
    pub unlisted: HashSet<String>,
    /// Generations the studio did not ask for, by whether they write a picture, once the
    /// server has said.
    pub elsewhere: HashMap<String, bool>,
    /// Generations the studio did not ask for and is asking the server about.
    pub probing: HashSet<String>,
    /// How the Mac is powered, once read; the web build never reads it.
    pub power: Option<Power>,
    /// What the power banner was closed on; it comes back when something else holds the GPU
    /// back.
    pub power_dismissed: Vec<Issue>,
    #[cfg(not(target_arch = "wasm32"))]
    pub power_watch: Option<crate::power::Watch>,
    next_local: u64,
}

pub fn random_seed() -> u64 {
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(entropy());
    h.finish() % 4_294_967_296
}

#[cfg(not(target_arch = "wasm32"))]
fn entropy() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos())
}

/// The browser has no system clock for std and `RandomState` is unseeded there, so ask it.
#[cfg(target_arch = "wasm32")]
fn entropy() -> u128 {
    (js_sys::Math::random() * 2f64.powi(53)) as u128
}

impl State {
    pub fn new(net: Option<Net>, images: Arc<ImageCache>, server: String) -> State {
        State {
            requests: net.as_ref().map(Net::requests).unwrap_or_default(),
            net,
            images,
            server,
            conn: Conn::Connecting,
            gens: Vec::new(),
            selected: None,
            prompt: TextEdit::new(""),
            size: SizeChoice::Preset(0),
            custom_w: TextEdit::new("1536"),
            custom_h: TextEdit::new("1024"),
            seed: TextEdit::new(random_seed().to_string()),
            references: Vec::new(),
            dropping: false,
            width: 0.0,
            pane: Pane::default(),
            preview: None,
            stage: StageZoom::default(),
            sidebar: PaneState::new(300.0).range(220.0, 480.0),
            inspector: PaneState::new(340.0).range(300.0, 460.0),
            menu: None,
            deleting: None,
            notes: Vec::new(),
            clock: 0.0,
            tick: 0,
            watching: HashSet::new(),
            unlisted: HashSet::new(),
            elsewhere: HashMap::new(),
            probing: HashSet::new(),
            power: None,
            power_dismissed: Vec::new(),
            #[cfg(not(target_arch = "wasm32"))]
            power_watch: None,
            next_local: 0,
        }
    }

    pub fn offline() -> State {
        State::new(None, Arc::new(ImageCache::new(Arc::new(|| {}))), api::DEFAULT_SERVER.to_owned())
    }

    pub fn item(&self, id: &str) -> Option<&Gen> {
        self.gens.iter().find(|g| g.id == id)
    }

    fn item_mut(&mut self, id: &str) -> Option<&mut Gen> {
        self.gens.iter_mut().find(|g| g.id == id)
    }

    pub fn selected_gen(&self) -> Option<&Gen> {
        self.selected.as_deref().and_then(|id| self.item(id))
    }

    pub fn connected(&self) -> bool {
        matches!(self.conn, Conn::Connected(_) | Conn::Unreadable(_))
    }

    fn reachable(&self) -> Result<(), String> {
        if self.connected() { Ok(()) } else { Err("Start mmh3 server first.".into()) }
    }

    pub fn active_count(&self) -> usize {
        self.gens.iter().filter(|g| g.phase.is_active()).count()
    }

    pub fn host(&self) -> &str {
        self.server.split("://").nth(1).unwrap_or(&self.server).trim_end_matches('/')
    }

    pub fn chosen_size(&self) -> Result<(u32, u32), SizeError> {
        match self.size {
            SizeChoice::Preset(i) => Ok((PRESETS[i].width, PRESETS[i].height)),
            SizeChoice::Custom => sizes::custom(&self.custom_w.text, &self.custom_h.text),
        }
    }

    pub fn seed_value(&self) -> Option<u64> {
        let t = self.seed.text.trim();
        if t.is_empty() { Some(random_seed()) } else { t.parse().ok() }
    }

    /// What Generate would send, or why it can't: the one rule the button, its help and the
    /// keyboard shortcut share.
    pub fn request(&self) -> Result<ImageRequest, String> {
        self.reachable()?;
        let prompt = self.prompt.text.trim();
        if prompt.is_empty() {
            return Err("Write a prompt first.".into());
        }
        let (width, height) = self.chosen_size().map_err(|e| e.to_string())?;
        let seed = self.seed_value().ok_or("The seed must be a whole number.")?;
        if let Some(Reference { path, .. }) = self.references.iter().find(|r| matches!(r.thumb, Thumb::Failed(_))) {
            let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
            return Err(format!("{name} isn't a picture mmh3 can read. Remove it first."));
        }
        let references = self.references.iter().map(|r| r.path.clone()).collect();
        Ok(ImageRequest { prompt: prompt.to_owned(), seed, width, height, min_generation_area: None, references })
    }

    pub fn generate(&mut self) {
        match self.request() {
            Ok(r) => self.submit(r),
            Err(why) => self.note(why, None, true),
        }
    }

    pub fn rerun(&mut self, id: &str) {
        if let Err(why) = self.reachable() {
            return self.note(why, None, true);
        }
        let Some(r) = self.item(id).and_then(|g| g.request.clone()) else { return };
        self.submit(ImageRequest { seed: random_seed(), ..r });
    }

    pub fn submit(&mut self, r: ImageRequest) {
        self.next_local += 1;
        let id = format!("local-{}", self.next_local);
        self.gens.insert(0, Gen { id: id.clone(), request: Some(r.clone()), queued: None, phase: Phase::Sending, picture: Picture::None, revision: 0 });
        self.select(&id);
        self.pane = Pane::Picture;
        if let Some(net) = &self.net {
            net.submit(id, r);
        }
    }

    /// The server's answer to a submission: the local placeholder takes the server's id, or
    /// gives way to the row a list refresh already made for it.
    pub fn submitted(&mut self, local: &str, answer: Result<Generation, api::Error>, now: f64) {
        let created = match answer {
            Ok(created) => created,
            Err(e) => {
                if let Some(g) = self.item_mut(local) {
                    g.phase = Phase::Failed(e.to_string());
                    g.revision += 1;
                }
                return;
            }
        };
        let was_selected = self.selected.as_deref() == Some(local);
        let request = self.item(local).and_then(|g| g.request.clone());
        if self.item(&created.id).is_none() {
            self.unlisted.insert(created.id.clone());
        }
        if let Some(r) = &request {
            self.requests.insert(created.id.clone(), r.clone());
            self.remember();
        }
        if let Some(g) = self.item_mut(&created.id) {
            g.request = request;
            g.revision += 1;
            self.gens.retain(|g| g.id != local);
        } else {
            let Some(g) = self.item_mut(local) else { return };
            g.id.clone_from(&created.id);
            g.queued = Some(created.queued);
            g.revision += 1;
            self.apply(&created, now);
        }
        if was_selected {
            self.selected = None;
            self.select(&created.id);
        }
    }

    pub fn apply(&mut self, g: &Generation, now: f64) {
        let Some(item) = self.item_mut(&g.id) else { return };
        let effects = step(&mut item.phase, g, now);
        item.revision += 1;
        for e in effects {
            self.run(&g.id, e);
        }
    }

    fn run(&mut self, id: &str, effect: Effect) {
        match effect {
            Effect::Watch => self.watch(id),
            Effect::FetchImage => self.fetch_image(id, true),
        }
    }

    fn remember(&self) {
        if let Some(net) = &self.net {
            net.remember(self.requests.clone());
        }
    }

    pub fn watch(&mut self, id: &str) {
        let Some(net) = &self.net else { return };
        if self.watching.insert(id.to_owned()) {
            net.watch(id.to_owned());
        }
    }

    /// Asks for a finished generation's picture: `urgent` for one the user waits for (just
    /// finished, or selected), which goes ahead of the gallery's backlog.
    pub fn fetch_image(&mut self, id: &str, urgent: bool) {
        let Some(net) = &self.net else { return };
        let Some(g) = self.gens.iter_mut().find(|g| g.id == id) else { return };
        if !matches!(g.phase, Phase::Done { .. }) {
            return;
        }
        match g.picture {
            Picture::None | Picture::Failed(_) => {
                g.picture = Picture::Loading;
                g.revision += 1;
                net.fetch_image(id, urgent);
            }
            Picture::Loading if urgent => net.fetch_image(id, true),
            _ => {}
        }
    }

    pub fn watch_ended(&mut self, id: &str) {
        self.watching.remove(id);
    }

    /// The server's list, most recent first: keeps what the app already holds for each, adds
    /// the pictures the studio asked for and drops what the server no longer has. Pictures are
    /// fetched for the rows that keep a thumbnail; older ones wait until they are selected.
    pub fn merge(&mut self, list: &[Generation], now: f64) {
        let mut old: Vec<Gen> = std::mem::take(&mut self.gens);
        let mut gens: Vec<Gen> = Vec::new();
        let listed: HashSet<&str> = list.iter().map(|g| g.id.as_str()).collect();
        self.unlisted.retain(|id| !listed.contains(id.as_str()));
        let mut i = 0;
        while i < old.len() {
            if old[i].is_local() || self.unlisted.contains(&old[i].id) { gens.push(old.remove(i)) } else { i += 1 }
        }
        let mut effects = Vec::new();
        let mut probe = Vec::new();
        for g in list {
            let item = match old.iter().position(|o| o.id == g.id) {
                Some(at) => {
                    let mut item = old.remove(at);
                    let fx = step(&mut item.phase, g, now);
                    item.revision += 1;
                    effects.extend(fx.into_iter().map(|e| (g.id.clone(), e)));
                    item
                }
                None => match (self.requests.get(&g.id), self.elsewhere.get(&g.id)) {
                    (Some(r), _) => Gen::from_server(g, Some(r.clone()), now),
                    (None, Some(true)) => Gen::from_server(g, None, now),
                    (None, Some(false)) => continue,
                    (None, None) => {
                        probe.push(g.id.clone());
                        continue;
                    }
                },
            };
            if item.phase.is_active() {
                effects.push((g.id.clone(), Effect::Watch));
            }
            gens.push(item);
        }
        self.gens = gens;
        let before = self.requests.len();
        self.requests.retain(|id, _| listed.contains(id.as_str()) || self.unlisted.contains(id));
        if self.requests.len() != before {
            self.remember();
        }
        self.elsewhere.retain(|id, _| listed.contains(id.as_str()));
        self.probing.retain(|id| listed.contains(id.as_str()));
        if let Some(net) = &self.net {
            for id in probe {
                if self.probing.insert(id.clone()) {
                    net.probe(id);
                }
            }
        }
        let gone: Vec<String> = old.into_iter().map(|o| o.id).collect();
        if let (Some(net), false) = (&self.net, gone.is_empty()) {
            net.forget(&gone);
        }
        let backlog: Vec<String> = self.gens.iter().filter(|g| matches!(g.phase, Phase::Done { .. })).take(MAX_THUMBS).map(|g| g.id.clone()).collect();
        if let Some(sel) = &self.selected
            && self.item(sel).is_none()
        {
            self.selected = None;
            self.preview = None;
        }
        for (id, e) in effects {
            self.run(&id, e);
        }
        if let Some(sel) = self.selected.clone() {
            self.fetch_image(&sel, true);
        }
        for id in backlog {
            self.fetch_image(&id, false);
        }
        self.trim_thumbs();
    }

    /// What the server said of a generation the studio did not ask for. One it could not
    /// answer for is asked about again with the next list.
    pub fn probed(&mut self, id: &str, answer: Probe, now: f64) {
        if !self.probing.remove(id) {
            return;
        }
        let g = match answer {
            Probe::Unanswered => return,
            Probe::NotAPicture => {
                self.elsewhere.insert(id.to_owned(), false);
                return;
            }
            Probe::Picture(g) => g,
        };
        self.elsewhere.insert(id.to_owned(), true);
        if self.item(id).is_some() {
            return;
        }
        let item = Gen::from_server(&g, None, now);
        let at = self.gens.iter().position(|o| o.queued.is_some_and(|q| q < g.queued)).unwrap_or(self.gens.len());
        let active = item.phase.is_active();
        let done = matches!(item.phase, Phase::Done { .. });
        self.gens.insert(at, item);
        if active {
            self.watch(id);
        }
        if done {
            self.fetch_image(id, false);
        }
    }

    pub fn picture_ready(&mut self, id: &str, file: PathBuf, thumb: Image) {
        let Some(g) = self.item_mut(id) else { return };
        g.picture = Picture::Ready { file, thumb: Some(thumb) };
        g.revision += 1;
        if self.selected.as_deref() == Some(id) {
            self.load_preview();
        }
        self.trim_thumbs();
    }

    pub fn picture_failed(&mut self, id: &str, why: String) {
        if let Some(g) = self.item_mut(id) {
            g.picture = Picture::Failed(why);
            g.revision += 1;
        }
    }

    fn trim_thumbs(&mut self) {
        let mut kept = 0;
        for g in &mut self.gens {
            if let Picture::Ready { thumb, .. } = &mut g.picture
                && thumb.is_some()
            {
                kept += 1;
                if kept > MAX_THUMBS {
                    *thumb = None;
                    g.revision += 1;
                }
            }
        }
    }

    pub fn select(&mut self, id: &str) {
        if self.selected.as_deref() == Some(id) {
            return;
        }
        self.selected = Some(id.to_owned());
        self.stage = StageZoom::default();
        self.fetch_image(id, true);
        self.load_preview();
    }

    /// The selected image at full size, decoded on the image cache's workers. Only one is held,
    /// so a 16 MP image's pixels are freed when the selection moves on.
    fn load_preview(&mut self) {
        let wanted = self.selected_gen().and_then(|g| g.file().map(|f| (g.id.clone(), f.clone())));
        self.preview = match wanted {
            Some((id, _)) if self.preview.as_ref().is_some_and(|(p, _)| *p == id) => self.preview.take(),
            Some((id, file)) => Some((id, self.images.file(file, 0.0, 2.0))),
            None => None,
        };
    }

    pub fn deleted(&mut self, id: &str) {
        self.gens.retain(|g| g.id != id);
        self.unlisted.remove(id);
        if self.requests.remove(id).is_some() {
            self.remember();
        }
        if self.selected.as_deref() == Some(id) {
            self.selected = None;
            self.preview = None;
        }
        if self.deleting.as_deref() == Some(id) {
            self.deleting = None;
        }
    }

    /// Asks before deleting what the server holds; a placeholder that never reached it goes
    /// at once.
    pub fn delete(&mut self, id: &str) {
        let Some(g) = self.item(id) else { return };
        if matches!(g.phase, Phase::Running { .. } | Phase::Sending) {
            return self.note("A running generation can't be deleted.", Some("Wait for it to finish, then delete it.".into()), true);
        }
        if g.is_local() {
            return self.deleted(id);
        }
        self.deleting = Some(id.to_owned());
    }

    pub fn confirm_delete(&mut self) {
        let Some(id) = self.deleting.take() else { return };
        match &self.net {
            Some(net) => net.delete(id),
            None => self.deleted(&id),
        }
    }

    /// What holds the GPU back that the user hasn't closed the banner on yet: all of it, so
    /// the banner reads whole, or nothing.
    pub fn power_warning(&self) -> Vec<Issue> {
        let issues = self.power.as_ref().map(Power::issues).unwrap_or_default();
        if issues.iter().all(|i| self.power_dismissed.contains(i)) { Vec::new() } else { issues }
    }

    pub fn dismiss_power(&mut self) {
        self.power_dismissed = self.power.as_ref().map(Power::issues).unwrap_or_default();
    }

    /// Reads the power state again now, as on returning to the window.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn check_power(&self) {
        if let Some(w) = &self.power_watch {
            w.check();
        }
    }

    pub fn note(&mut self, title: impl Into<String>, detail: Option<String>, danger: bool) {
        let id = self.notes.iter().map(|n| n.id + 1).max().unwrap_or(1);
        self.notes.push(Note { id, title: title.into(), detail, danger, until: None });
        if self.notes.len() > 3 {
            self.notes.remove(0);
        }
    }

    pub fn set_status(&mut self, status: Result<Status, api::Error>) {
        self.conn = match status {
            Ok(s) => Conn::Connected(s),
            Err(e) if e.is_unreachable() => Conn::Disconnected(e.to_string()),
            Err(e) => Conn::Unreadable(e.to_string()),
        };
    }

    /// Attaches pictures to edit, after the ones already attached, leaving out what is attached
    /// already and what is past the server's limit.
    pub fn add_references(&mut self, paths: &[PathBuf]) {
        let mut over = 0;
        for path in paths {
            if self.references.iter().any(|r| r.path == *path) {
                continue;
            }
            if self.references.len() >= api::MAX_REFERENCES {
                over += 1;
                continue;
            }
            if let Some(net) = &self.net {
                net.reference_thumb(path.clone());
            }
            self.references.push(Reference { path: path.clone(), thumb: Thumb::Loading });
        }
        if over > 0 {
            let detail = format!("{over} more {} left out.", if over == 1 { "was" } else { "were" });
            self.note(format!("mmh3 edits up to {} pictures at once.", api::MAX_REFERENCES), Some(detail), true);
        }
    }

    pub fn reference_thumb(&mut self, path: &std::path::Path, thumb: Result<Image, String>) {
        if let Some(r) = self.references.iter_mut().find(|r| r.path == path) {
            r.thumb = match thumb {
                Ok(img) => Thumb::Ready(img),
                Err(why) => Thumb::Failed(why),
            };
        }
    }

    /// Takes picture `i` away; the ones after it move up, so the prompt's tags past it would
    /// point at the next picture, which the composer's warning shows.
    pub fn remove_reference(&mut self, i: usize) {
        if i < self.references.len() {
            self.references.remove(i);
        }
    }

    /// Puts `<Picture n>` into the prompt where the cursor is.
    pub fn insert_picture_tag(&mut self, n: usize) {
        self.prompt.insert(&picture_tag(n));
    }

    /// Attaches a finished generation's picture, to edit it further. The picture is kept apart
    /// from the generation's own file first, which goes when the generation does, so the edit
    /// and a rerun of it still have it then.
    pub fn edit_picture(&mut self, id: &str) {
        let Some(file) = self.item(id).and_then(|g| g.file().cloned()) else { return };
        match &self.net {
            Some(net) => net.keep_for_editing(id, file),
            None => self.add_references(&[file]),
        }
        self.show_composer();
    }

    /// Brings the composer into view, wide or compact.
    pub fn show_composer(&mut self) {
        self.inspector.set_open(true);
        self.pane = Pane::Compose;
    }

    pub fn randomise_seed(&mut self) {
        self.seed.replace_text(random_seed().to_string());
    }

    pub fn save(&mut self, id: &str, tx: &mut Tx) {
        let Some(file) = self.item(id).and_then(|g| g.file().cloned()) else { return };
        crate::net::save(self, &file, &format!("mmh3-{id}.png"), tx);
    }

    pub fn ask_for_references(&mut self, tx: &mut Tx) {
        crate::net::ask_for_references(self, tx);
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub fn reveal(&mut self, id: &str) {
        if let Some(file) = self.item(id).and_then(|g| g.file().cloned()) {
            crate::net::reveal(file);
        }
    }
}

#[cfg(test)]
mod tests;

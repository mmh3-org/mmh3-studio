use std::collections::{HashMap, HashSet};
use std::hash::{BuildHasher, Hasher};
use std::path::PathBuf;
use std::sync::Arc;

use crate::api::{self, Asked, Generation, ImageRequest, Kind, Status, Timings};
use crate::estimate::{self, Model, Sample};
use crate::power::{Issue, Power};
use crate::sizes::{self, ASPECTS, Quality, SizeError};
use crate::stage::Zoom;
use rune::machine::Effects;
use rune::{Image, ImageCache, PointerPress, TextEdit, Tx};
use rune_kit::shell::PaneState;

use crate::net::{Feed, Net};

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
        timings: Option<Timings>,
    },
    Failed(String),
    /// Stopped on request; `step` is how many steps it took.
    Cancelled {
        step: Option<u32>,
    },
}

impl Phase {
    pub fn is_active(&self) -> bool {
        matches!(self, Phase::Sending | Phase::Queued | Phase::Running { .. })
    }

    /// Waiting, or running with a step still to take: the server stops a run only at a step's
    /// boundary, so one decoding or writing its picture can no longer be stopped.
    pub fn stoppable(&self) -> bool {
        match self {
            Phase::Queued => true,
            Phase::Running { step, steps, .. } => step.is_none_or(|k| k < *steps),
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Effect {
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
        api::State::Done => Phase::Done { seconds: g.seconds.unwrap_or_default(), timings: g.timings.clone() },
        api::State::Failed => Phase::Failed(g.error.clone().unwrap_or_else(|| "The server gave no reason.".into())),
        api::State::Cancelled => Phase::Cancelled { step: g.preview_step.or(g.timings.as_ref().map(|t| t.steps.len() as u32)).filter(|k| *k > 0) },
    };
    match (&was, &*phase) {
        (Phase::Done { .. }, _) => Effects::none(),
        (_, Phase::Done { .. }) => Effects::one(Effect::FetchImage),
        _ => Effects::none(),
    }
}

/// What the server says of a generation the studio did not ask for.
#[derive(Clone, Debug, PartialEq)]
pub enum Probe {
    /// It writes a picture, and this is where it is now.
    Picture(Box<Generation>),
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

/// The latest step preview of a picture, decoded.
pub struct Live {
    pub step: u32,
    pub image: Image,
}

pub struct Gen {
    pub id: String,
    /// What the studio asked for, with the files of its references, which only the studio has.
    /// It is kept for a server that does not say what was asked for, and to run it again.
    pub request: Option<ImageRequest>,
    /// What the server says was asked for; an upstream server never says.
    pub asked: Option<Asked>,
    /// When the server took it, which orders the gallery; none before it has.
    pub queued: Option<u64>,
    pub phase: Phase,
    pub picture: Picture,
    /// The last step the server has a preview of.
    pub preview_step: Option<u32>,
    /// The latest preview fetched, shown while it runs and once it was cancelled.
    pub live: Option<Live>,
    /// The step of the preview being fetched.
    pub fetching_preview: Option<u32>,
    /// The step whose preview could not be fetched, which is not asked for again until the server
    /// has a newer one or says everything again.
    pub missed_preview: Option<u32>,
    /// The studio asked the server to stop it.
    pub stopping: bool,
    pub revision: u64,
}

impl Gen {
    pub fn new(id: String, request: Option<ImageRequest>, phase: Phase) -> Gen {
        Gen {
            id,
            request,
            asked: None,
            queued: None,
            phase,
            picture: Picture::None,
            preview_step: None,
            live: None,
            fetching_preview: None,
            missed_preview: None,
            stopping: false,
            revision: 0,
        }
    }

    pub fn from_server(g: &Generation, request: Option<ImageRequest>, now: f64) -> Gen {
        let mut item = Gen::new(g.id.clone(), request, Phase::Queued);
        let _ = item.take(g, now);
        item
    }

    /// The step of the preview to fetch next, if any: the latest of a picture that is running or
    /// was cancelled, newer than the one shown and not the one that could not be fetched.
    pub fn preview_wanted(&self) -> Option<u32> {
        if !matches!(self.phase, Phase::Running { .. } | Phase::Cancelled { .. }) || self.fetching_preview.is_some() {
            return None;
        }
        self.preview_step.filter(|k| self.live.as_ref().is_none_or(|l| l.step < *k) && self.missed_preview != Some(*k))
    }

    /// Takes in what the server says of it now.
    fn take(&mut self, g: &Generation, now: f64) -> Effects<Effect> {
        let effects = step(&mut self.phase, g, now);
        self.queued = Some(g.queued);
        if g.request.is_some() {
            self.asked.clone_from(&g.request);
        }
        self.preview_step = self.preview_step.max(g.preview_step);
        if !self.phase.is_active() {
            self.stopping = false;
        }
        self.revision += 1;
        effects
    }

    /// What was asked for, from the server when it says and from the studio's memory otherwise.
    pub fn prompt(&self) -> &str {
        match (&self.asked, &self.request) {
            (Some(Asked { prompt: Some(p), .. }), _) => p,
            (_, Some(r)) => &r.prompt,
            _ => "",
        }
    }

    pub fn seed(&self) -> Option<u64> {
        self.asked.as_ref().and_then(|a| a.seed).or(self.request.as_ref().map(|r| r.seed))
    }

    pub fn size(&self) -> Option<(u32, u32)> {
        let asked = self.asked.as_ref().and_then(|a| a.width.zip(a.height));
        asked.or(self.request.as_ref().map(|r| (r.width, r.height)))
    }

    /// Whether the prompt is known, from the server or from the studio's memory.
    pub fn is_known(&self) -> bool {
        self.asked.as_ref().is_some_and(|a| a.prompt.is_some()) || self.request.is_some()
    }

    /// How many pictures it edits.
    pub fn reference_count(&self) -> usize {
        self.asked.as_ref().map_or_else(|| self.request.as_ref().map_or(0, |r| r.references.len()), |a| a.references.len())
    }

    /// Whether it reads pictures besides the prompt.
    pub fn has_references(&self) -> bool {
        self.reference_count() > 0
    }

    /// What running it again sends: what the server says was asked for, with the studio's files
    /// of its references. One that edits pictures the studio has no files of can't be sent again.
    pub fn rerun_request(&self) -> Option<ImageRequest> {
        let local = self.request.clone();
        let Some(a) = &self.asked else { return local };
        let references = match &local {
            Some(r) if r.references.len() == a.references.len() => r.references.clone(),
            _ if a.references.is_empty() => Vec::new(),
            _ => return None,
        };
        let (width, height) = self.size()?;
        Some(ImageRequest {
            prompt: a.prompt.clone().or(local.as_ref().map(|r| r.prompt.clone()))?,
            seed: self.seed().unwrap_or_default(),
            width,
            height,
            min_generation_area: a.min_generation_area.or(local.and_then(|r| r.min_generation_area)),
            references,
        })
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

/// A size from the picker: a quality and one of [`ASPECTS`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SizeChoice {
    pub quality: Quality,
    pub aspect: usize,
}

impl SizeChoice {
    pub fn size(self) -> (u32, u32) {
        ASPECTS[self.aspect].size(self.quality)
    }
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
    /// The custom size is open, and asked for instead of [`State::size`].
    pub custom_size: bool,
    pub custom_w: TextEdit,
    pub custom_h: TextEdit,
    pub seed: TextEdit,
    pub references: Vec<Reference>,
    /// Files are being dragged over the composer.
    pub dropping: bool,
    /// The window's width in points, once laid out; below [`crate::view::COMPACT_BELOW`], or
    /// with too little room for the picture beside the open panes, the panes take turns.
    pub width: f32,
    pub pane: Pane,
    pub preview: Option<(String, Image)>,
    pub zoom: Zoom,
    /// The selected picture's timing details are open.
    pub timing_open: bool,
    pub sidebar: PaneState,
    pub inspector: PaneState,
    pub menu: Option<(String, PointerPress)>,
    pub deleting: Option<String>,
    pub notes: Vec<Note>,
    pub clock: f64,
    pub tick: u64,
    /// How the studio hears of the server's generations, which says whether it can stop one.
    pub feed: Feed,
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
            size: SizeChoice::default(),
            custom_size: false,
            custom_w: TextEdit::new("1536"),
            custom_h: TextEdit::new("1024"),
            seed: TextEdit::new(random_seed().to_string()),
            references: Vec::new(),
            dropping: false,
            width: 0.0,
            pane: Pane::default(),
            preview: None,
            zoom: Zoom::default(),
            timing_open: false,
            sidebar: PaneState::new(300.0).range(220.0, 480.0),
            inspector: PaneState::new(340.0).range(300.0, 460.0),
            menu: None,
            deleting: None,
            notes: Vec::new(),
            clock: 0.0,
            tick: 0,
            feed: Feed::default(),
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
        if self.custom_size { sizes::custom(&self.custom_w.text, &self.custom_h.text) } else { Ok(self.size.size()) }
    }

    pub fn pick_size(&mut self, size: SizeChoice) {
        self.size = size;
        self.custom_size = false;
    }

    /// Opens the custom size on the size picked so far, or closes it on the picker's.
    pub fn toggle_custom_size(&mut self) {
        self.custom_size = !self.custom_size;
        if self.custom_size {
            let (w, h) = self.size.size();
            self.custom_w.replace_text(w.to_string());
            self.custom_h.replace_text(h.to_string());
        }
    }

    /// How long pictures take here, fitted from the finished generations, newest first. An edit
    /// is left out: its references are tokens every step attends to, so its steps cost more than
    /// a prompt's at the same size and would scale every estimate up.
    pub fn time_model(&self) -> Model {
        estimate::fit(self.gens.iter().filter(|g| g.reference_count() == 0).filter_map(|g| match (&g.phase, g.size()) {
            (Phase::Done { seconds, timings }, Some((w, h))) => Some(Sample { mp: sizes::megapixels(w, h), seconds: *seconds, timings: timings.clone() }),
            _ => None,
        }))
    }

    /// The server answers but holds no `dit` model, so the next generation loads it first.
    pub fn cold(&self) -> bool {
        matches!(&self.conn, Conn::Connected(Status { models: Some(m), generating: None, .. }) if !m.iter().any(|m| m == "dit"))
    }

    /// macOS keeps the GPU at a low clock, which the estimates do not know about.
    pub fn gpu_held_back(&self) -> bool {
        self.power.as_ref().is_some_and(|p| p.issues().iter().any(Issue::is_clock))
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
        let Some(r) = self.item(id).and_then(Gen::rerun_request) else { return };
        self.submit(ImageRequest { seed: random_seed(), ..r });
    }

    pub fn submit(&mut self, r: ImageRequest) {
        self.next_local += 1;
        let id = format!("local-{}", self.next_local);
        self.gens.insert(0, Gen::new(id.clone(), Some(r.clone()), Phase::Sending));
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
            self.apply(&created, now);
        }
        if was_selected {
            self.selected = None;
            self.select(&created.id);
        }
    }

    /// What the server says of a generation the studio holds; others are left alone.
    pub fn apply(&mut self, g: &Generation, now: f64) {
        let Some(item) = self.item_mut(&g.id) else { return };
        let effects = item.take(g, now);
        for e in effects {
            self.run(&g.id, e);
        }
        self.want_preview(&g.id);
    }

    /// One generation, as the event stream says it: one the studio holds is updated, and a new
    /// picture joins the gallery where the queue puts it.
    pub fn upsert(&mut self, g: &Generation, now: f64) {
        if self.item(&g.id).is_some() {
            return self.apply(g, now);
        }
        let Some(item) = self.newcomer(g, now) else { return };
        let done = matches!(item.phase, Phase::Done { .. });
        self.place(item);
        if done {
            self.fetch_image(&g.id, false);
        }
        self.want_preview(&g.id);
    }

    /// A generation the studio does not hold yet, as a row, when it is a picture and it is known
    /// to be one: from the server's `kind`, from what the studio asked for, or once a probe of an
    /// upstream server has said. An unknown one is probed.
    fn newcomer(&mut self, g: &Generation, now: f64) -> Option<Gen> {
        let request = self.requests.get(&g.id).cloned();
        match (g.kind, &request, self.elsewhere.get(&g.id)) {
            (Some(Kind::Video), ..) | (None, None, Some(false)) => None,
            (Some(Kind::Image), ..) | (None, Some(_), _) | (None, None, Some(true)) => Some(Gen::from_server(g, request, now)),
            (None, None, None) => {
                if let Some(net) = &self.net
                    && self.probing.insert(g.id.clone())
                {
                    net.probe(g.id.clone());
                }
                None
            }
        }
    }

    /// Puts `item` in the gallery in queue order, the most recent first.
    fn place(&mut self, item: Gen) {
        let queued = item.queued.unwrap_or(u64::MAX);
        let at = self.gens.iter().position(|o| !o.is_local() && o.queued.is_some_and(|q| q < queued)).unwrap_or(self.gens.len());
        self.gens.insert(at, item);
    }

    fn run(&mut self, id: &str, effect: Effect) {
        match effect {
            Effect::FetchImage => self.fetch_image(id, true),
        }
    }

    fn remember(&self) {
        if let Some(net) = &self.net {
            net.remember(self.requests.clone());
        }
    }

    /// Fetches the latest preview of a picture that is running or was cancelled, one at a time.
    fn want_preview(&mut self, id: &str) {
        let Some(net) = &self.net else { return };
        let Some(g) = self.gens.iter_mut().find(|g| g.id == id) else { return };
        let Some(step) = g.preview_wanted() else { return };
        g.fetching_preview = Some(step);
        net.fetch_preview(id.to_owned(), step);
    }

    /// The server has a preview of step `step` of generation `id`.
    pub fn preview_available(&mut self, id: &str, step: u32) {
        let Some(g) = self.item_mut(id) else { return };
        g.preview_step = g.preview_step.max(Some(step));
        self.want_preview(id);
    }

    pub fn preview_ready(&mut self, id: &str, step: u32, image: Result<Image, String>) {
        let Some(g) = self.item_mut(id) else { return };
        g.fetching_preview = None;
        // A picture already shown has no use for its preview, and one missed is not worth a note:
        // the next step brings another.
        match (image, &g.picture) {
            (Ok(image), Picture::None | Picture::Loading) if g.live.as_ref().is_none_or(|l| l.step < step) => {
                g.live = Some(Live { step, image });
                g.revision += 1;
            }
            (Ok(_), _) => {}
            (Err(_), _) => g.missed_preview = Some(step),
        }
        self.want_preview(id);
    }

    /// Asks the server to stop a generation that is waiting or running.
    pub fn stop(&mut self, id: &str) {
        let Some(g) = self.gens.iter_mut().find(|g| g.id == id) else { return };
        if !g.phase.stoppable() || g.stopping {
            return;
        }
        g.stopping = true;
        g.revision += 1;
        if let Some(net) = &self.net {
            net.cancel(id.to_owned());
        }
    }

    /// The server's answer to [`State::stop`]. A 409 says it got there first: it has ended, or
    /// has taken its last step and is writing the picture, which the event stream brings.
    pub fn cancelled(&mut self, id: &str, answer: Result<Generation, api::Error>, now: f64) {
        match answer {
            Ok(g) => self.apply(&g, now),
            Err(e) => {
                if let Some(g) = self.item_mut(id) {
                    g.stopping = false;
                    g.revision += 1;
                }
                if !matches!(e, api::Error::Refused { status: 409, .. }) {
                    self.note("Couldn't stop the generation.", Some(e.to_string()), true);
                }
            }
        }
    }

    /// Whether the server can stop a generation: only the fork's can, which is the one with the
    /// event stream.
    pub fn can_stop(&self, g: &Gen) -> bool {
        self.feed == Feed::Streaming && g.phase.stoppable() && !g.stopping
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

    /// The server's list, most recent first: keeps what the app already holds for each, adds
    /// the pictures among the rest and drops what the server no longer has. Pictures are
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
        for g in list {
            let item = match old.iter().position(|o| o.id == g.id) {
                Some(at) => {
                    let mut item = old.remove(at);
                    item.missed_preview = None;
                    effects.extend(item.take(g, now).into_iter().map(|e| (g.id.clone(), e)));
                    item
                }
                None => match self.newcomer(g, now) {
                    Some(item) => item,
                    None => continue,
                },
            };
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
        let previewed: Vec<String> = self.gens.iter().filter(|g| g.preview_step.is_some()).map(|g| g.id.clone()).collect();
        for id in previewed {
            self.want_preview(&id);
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
        let done = matches!(item.phase, Phase::Done { .. });
        self.place(item);
        if done {
            self.fetch_image(id, false);
        }
    }

    pub fn picture_ready(&mut self, id: &str, file: PathBuf, thumb: Image) {
        let Some(g) = self.item_mut(id) else { return };
        g.picture = Picture::Ready { file, thumb: Some(thumb) };
        g.live = None;
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
        self.zoom = Zoom::default();
        self.timing_open = false;
        self.fetch_image(id, true);
        self.load_preview();
    }

    /// The selected image at full size, decoded on the image cache's workers. Only one is held,
    /// so a 16 MP image's pixels are freed when the selection moves on.
    fn load_preview(&mut self) {
        let wanted = self.selected_gen().and_then(|g| g.file().map(|f| (g.id.clone(), f.clone())));
        self.preview = match wanted {
            Some((id, _)) if self.preview.as_ref().is_some_and(|(p, _)| *p == id) => self.preview.take(),
            Some((id, file)) => {
                let source = self.net.as_ref().map_or_else(|| file.clone(), |n| n.preview_source(&id, &file));
                Some((id, self.images.file(source, 0.0, 2.0)))
            }
            None => None,
        };
    }

    /// The server took a generation away, as asked by this studio or by another client.
    pub fn removed(&mut self, id: &str) {
        if let Some(net) = &self.net {
            net.forget(&[id.to_owned()]);
        }
        self.deleted(id);
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
            let how =
                if self.feed == Feed::Streaming { "Stop it, or wait for it to finish, then delete it." } else { "Wait for it to finish, then delete it." };
            return self.note("A running generation can't be deleted.", Some(how.into()), true);
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

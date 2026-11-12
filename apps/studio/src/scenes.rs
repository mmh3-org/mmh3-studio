//! Fixture states for headless snapshots (`--scene NAME`) and tests.

use std::path::PathBuf;

use crate::api::{Generation, ImageRequest, Memory, State as Remote, Status, Timings};
use crate::mock::{paint, preview};
use crate::net::Feed;
use crate::power::{Chip, Mode, Power, Source};

use crate::effort::{Custom, Effort};
use crate::sizes::Quality;
use crate::state::{Conn, Gen, Live, Pane, Phase, Picture, Reference, SizeChoice, State, Thumb};

type Scene = (&'static str, fn(&mut State));

pub const SCENES: &[Scene] = &[
    ("empty", empty),
    ("disconnected", disconnected),
    ("running", running),
    ("previewing", previewing),
    ("cancelled", cancelled),
    ("gallery", gallery),
    ("failed", failed),
    ("custom-size", custom_size),
    ("offline-selected", offline_selected),
    ("confirm-delete", confirm_delete),
    ("timing", timing),
    ("references", references),
    ("power", power),
    ("effort", effort),
    ("effort-small", effort_small),
    ("effort-custom", effort_custom),
];

pub fn status(busy: bool, queued: u32) -> Status {
    Status {
        generating: busy.then(|| "7c01aa2e0005".to_owned()),
        generations: 5,
        memory: Some(Memory { free: if busy { 61_400_000_000 } else { 84_100_000_000 }, total: 137_400_000_000 }),
        models: Some(if busy { vec!["text_encoder".into(), "video_vae".into()] } else { vec!["text_encoder".into(), "video_vae".into(), "dit".into()] }),
        queued,
    }
}

/// Scenes are fixtures: they never reach a server, whatever the run was started with.
fn offline(s: &mut State) {
    s.net = None;
}

pub fn empty(s: &mut State) {
    offline(s);
    s.conn = Conn::Connected(status(false, 0));
}

pub fn disconnected(s: &mut State) {
    offline(s);
    s.conn = Conn::Disconnected("Connection refused (os error 61)".into());
}

fn remote(id: &str, state: Remote) -> Generation {
    Generation { id: id.into(), queued: 1_762_849_000, state, ..Generation::default() }
}

/// What the studio remembers asking for `id`, which the server never repeats.
fn asked(s: &mut State, id: &str, prompt: &str, seed: u64, (width, height): (u32, u32)) {
    let r = ImageRequest { prompt: prompt.into(), seed, width, height, min_generation_area: None, references: Vec::new(), ..ImageRequest::default() };
    s.requests.insert(id.into(), r);
}

fn done(s: &mut State, id: &str, prompt: &str, seed: u64, size: (u32, u32), seconds: f64) -> Generation {
    asked(s, id, prompt, seed, size);
    Generation { seconds: Some(seconds), ..remote(id, Remote::Done) }
}

/// Paints a fixture at a fraction of the generation's size, with a decoded thumbnail.
fn picture(s: &mut State, id: &str, preview: bool) {
    let Some(g) = s.gens.iter().find(|g| g.id == id) else { return };
    let Some(((w, h), seed)) = g.size().zip(g.seed()) else { return };
    let (w, h) = (w / 4, h / 4);
    let png = paint(w - w % 8, h - h % 8, seed);
    let thumb = crate::thumb::thumbnail(&png).ok().map(|t| s.images.rgba(t.width(), t.height(), t.into_raw()));
    if let Some(t) = &thumb {
        let _ = t.wait();
    }
    if preview {
        let full = s.images.load(png);
        let _ = full.wait();
        s.preview = Some((id.to_owned(), full));
    }
    if let Some(g) = s.gens.iter_mut().find(|g| g.id == id) {
        g.picture = Picture::Ready { file: PathBuf::from(format!("/tmp/mmh3-studio-fixture-{id}.png")), thumb };
    }
}

fn library(s: &mut State) {
    offline(s);
    let list = vec![
        done(s, "7c01aa2e0004", "Paper boats drifting across a rain puddle on cobblestones, reflections of neon signs", 912, (4736, 3552), 236.4),
        Generation {
            timings: Some(Timings {
                encode: Some(1.6),
                steps: vec![37.4, 37.9, 38.0, 37.8, 38.1, 38.2],
                decode_read: Some(0.0004),
                decode: Some(5.8),
                write: Some(0.21),
                ..Timings::default()
            }),
            ..done(s, "7c01aa2e0003", "A lighthouse on a basalt cliff at dusk, long exposure, sea mist", 4127, (5472, 3072), 241.8)
        },
        done(s, "7c01aa2e0002", "Isometric illustration of a tiny greenhouse full of succulents", 77, (2048, 2048), 38.7),
        done(s, "7c01aa2e0001", "Portrait of a red fox in fresh snow, shallow depth of field", 31_337, (3072, 5472), 239.1),
    ];
    s.merge(&list, 0.0);
    for id in ["7c01aa2e0004", "7c01aa2e0003", "7c01aa2e0002", "7c01aa2e0001"] {
        picture(s, id, false);
    }
}

pub fn running(s: &mut State) {
    s.conn = Conn::Connected(status(true, 1));
    library(s);
    asked(s, "7c01aa2e0005", "A cathedral made of glass in a pine forest, morning fog, volumetric light", 2026, (4096, 4096));
    asked(s, "7c01aa2e0006", "Macro photograph of frost ferns on a window pane", 8, (2048, 2048));
    let running = Generation { step: Some(2), steps: Some(6), ..remote("7c01aa2e0005", Remote::Running) };
    let queued = remote("7c01aa2e0006", Remote::Queued);
    let known = |s: &State, id: &str| s.requests.get(id).cloned();
    let mut gens = vec![Gen::from_server(&queued, known(s, "7c01aa2e0006"), 0.0), Gen::from_server(&running, known(s, "7c01aa2e0005"), 0.0)];
    if let Phase::Running { since, .. } = &mut gens[1].phase {
        *since = -84.2;
    }
    gens.append(&mut s.gens);
    s.gens = gens;
    s.prompt.replace_text("A cathedral made of glass in a pine forest, morning fog, volumetric light");
    s.size = SizeChoice { quality: Quality::Final, aspect: 0 };
    s.selected = Some("7c01aa2e0005".into());
}

/// A step preview of `id`, painted at an eighth of its size as the server writes them.
fn live(s: &mut State, id: &str, step: u32) {
    let Some(g) = s.gens.iter().find(|g| g.id == id) else { return };
    let Some(((w, h), seed)) = g.size().zip(g.seed()) else { return };
    let png = preview(w / 8, h / 8, seed, step, crate::api::IMAGE_STEPS);
    let Ok(img) = image::load_from_memory(&png) else { return };
    let rgba = img.into_rgba8();
    let image = s.images.rgba(rgba.width(), rgba.height(), rgba.into_raw());
    let _ = image.wait();
    if let Some(g) = s.gens.iter_mut().find(|g| g.id == id) {
        g.preview_step = Some(step);
        g.live = Some(Live { step, image });
    }
}

/// The fork's server: the running picture shows its latest preview and can be stopped.
pub fn previewing(s: &mut State) {
    running(s);
    s.feed = Feed::Streaming;
    if let Some(g) = s.gens.iter_mut().find(|g| g.id == "7c01aa2e0005")
        && let Phase::Running { step, .. } = &mut g.phase
    {
        *step = Some(3);
    }
    live(s, "7c01aa2e0005", 3);
}

/// A picture stopped after its fourth step, with the preview it had got to.
pub fn cancelled(s: &mut State) {
    s.conn = Conn::Connected(status(false, 0));
    s.feed = Feed::Streaming;
    library(s);
    asked(s, "7c01aa2e0008", "A night market under paper lanterns, rain on the awnings", 404, (4096, 2304));
    let stopped = Generation { seconds: Some(141.0), preview_step: Some(4), ..remote("7c01aa2e0008", Remote::Cancelled) };
    let request = s.requests.get("7c01aa2e0008").cloned();
    s.gens.insert(0, Gen::from_server(&stopped, request, 0.0));
    live(s, "7c01aa2e0008", 4);
    s.selected = Some("7c01aa2e0008".into());
}

pub fn gallery(s: &mut State) {
    s.conn = Conn::Connected(status(false, 0));
    library(s);
    s.selected = Some("7c01aa2e0003".into());
    picture(s, "7c01aa2e0003", true);
}

pub fn failed(s: &mut State) {
    s.conn = Conn::Connected(status(false, 0));
    library(s);
    asked(s, "7c01aa2e0007", "<Picture 1> as a city skyline made of stacked books", 5, (2048, 2048));
    if let Some(r) = s.requests.get_mut("7c01aa2e0007") {
        r.references.push(PathBuf::from("/Users/me/Desktop/skyline.heic"));
    }
    let error = "/Users/me/.cache/mmh3/jobs/7c01aa2e0007/picture-1.heic: The image format could not be determined";
    let f = Generation { error: Some(error.into()), seconds: Some(0.4), ..remote("7c01aa2e0007", Remote::Failed) };
    let request = s.requests.get("7c01aa2e0007").cloned();
    s.gens.insert(0, Gen::from_server(&f, request, 0.0));
    s.selected = Some("7c01aa2e0007".into());
}

pub fn custom_size(s: &mut State) {
    empty(s);
    s.prompt.replace_text("Wide panorama of the Dolomites at sunrise");
    s.custom_size = true;
    s.custom_w.replace_text("6000");
    s.custom_h.replace_text("2000");
}

pub fn offline_selected(s: &mut State) {
    gallery(s);
    disconnected(s);
}

pub fn confirm_delete(s: &mut State) {
    gallery(s);
    s.delete("7c01aa2e0003");
}

/// Where the selected picture's time went, as the server timed it.
pub fn timing(s: &mut State) {
    gallery(s);
    s.timing_open = true;
}

/// The composer with pictures attached to edit, their thumbnails painted in place.
pub fn references(s: &mut State) {
    gallery(s);
    s.prompt.replace_text("<Picture 1> at night, the street lamps lit, with the fox from <Picture 2> sitting on the steps of <Picture 3>");
    for (i, (name, seed)) in [("street.jpg", 21_u64), ("fox.png", 31_337), ("house.png", 912)].into_iter().enumerate() {
        let png = paint(240, 160 + 40 * i as u32, seed);
        let thumb = crate::thumb::reference(&png).ok().map(|t| s.images.rgba(t.width(), t.height(), t.into_raw()));
        if let Some(t) = &thumb {
            let _ = t.wait();
        }
        let thumb = thumb.map_or(Thumb::Loading, Thumb::Ready);
        s.references.push(Reference { path: PathBuf::from(format!("/Users/me/Pictures/{name}")), thumb });
    }
}

/// The composer at Final 1:1 on Low: two half-size draft steps and Sol-Attn.
pub fn effort(s: &mut State) {
    gallery(s);
    s.pane = Pane::Compose;
    s.prompt.replace_text("A cathedral made of glass in a pine forest, morning fog, volumetric light");
    s.size = SizeChoice { quality: Quality::Final, aspect: 0 };
    s.effort = Effort::Low;
}

/// Medium at Draft 4:3, too few tokens for Sol-Attn, so it says it is the same as High.
pub fn effort_small(s: &mut State) {
    effort(s);
    s.size = SizeChoice { quality: Quality::Draft, aspect: 1 };
    s.effort = Effort::Medium;
}

/// Custom at Final 1:1: Sol-Attn from the fourth step and two draft steps.
pub fn effort_custom(s: &mut State) {
    effort(s);
    s.effort = Effort::Custom;
    s.custom_effort = Custom { sol: true, sol_from: 4, draft_steps: 2, ..Custom::default() };
}

/// A 14-inch laptop with a Max chip in Automatic Energy Mode on a 90 W adapter, which
/// can't keep its battery up under a generation, while another process keeps the CPU busy.
pub fn power(s: &mut State) {
    running(s);
    s.power = Some(Power {
        mode: Some(Mode::Automatic),
        high_available: true,
        chip: Chip::Large { capped_case: true },
        source: Some(Source::Adapter { watts: Some(90), draining: true }),
        busy: Some("bun".into()),
        ..Power::default()
    });
}

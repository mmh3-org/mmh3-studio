//! A stand-in for `mmh3 server`, so the studio can be exercised without the engine. It answers
//! the routes of the studio's fork of mmh3's `src/server.rs` with the same JSON, status codes and
//! event streams: what each generation asked for, its stage timings, a preview after each step,
//! cancelling and `GET /v1/events`. Started [`Config::upstream`], it answers only the routes and
//! fields of upstream mmh3, as a server without the fork does. A picture takes six timed steps
//! and is a painted PNG of the requested size; its fields are checked as `mmh3 image` checks
//! them, so a bad value fails the generation rather than the request. A video is there only so
//! that a client meets one: it takes its steps and writes nothing worth watching.

use std::collections::{HashSet, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::api::{Asked, Generation, IMAGE_STEPS, Kind, MAX_REFERENCES, Memory, Refusal, State, Status, Timings};

/// The slots of the server's shared model table, in the order `/v1/status` lists them. A
/// picture keeps its decoder in the video VAE's slot.
const SLOTS: [&str; 5] = ["text_encoder", "video_vae", "audio_vae", "latent_upscaler", "dit"];
/// What a video leaves in the table.
const VIDEO_MODELS: [&str; 4] = ["text_encoder", "video_vae", "audio_vae", "dit"];
const IMAGE_OPTIONS: [&str; 8] = ["prompt", "prompt-file", "reference", "width", "height", "min-generation-area", "seed", "attention-precision"];
const IMAGE_FILE_OPTIONS: [&str; 2] = ["prompt-file", "reference"];
const MACHINE_OPTIONS: [&str; 10] = ["models", "dit", "text-encoder", "video-vae", "audio-vae", "devices", "vram-budget", "worker", "worker-units", "token"];
const VIDEO_FILE_OPTIONS: [&str; 7] = ["prompt-file", "context", "first-frame", "last-frame", "reference", "reference-audio", "reference-video"];
/// The fields the fork adds to a generation, which an upstream server never writes.
const FORK_FIELDS: [&str; 7] = ["kind", "request", "created_at", "started_at", "finished_at", "timings", "preview_step"];
const TOTAL_MEMORY: u64 = 128 << 30;
const MAX_BODY: usize = 1 << 30;
const KEEP_ALIVE: Duration = Duration::from_secs(15);
/// How many announcements of `GET /v1/events` are kept for clients that read slowly; one further
/// behind is sent a new snapshot, as the fork's broadcast channel does.
const ANNOUNCED: usize = 256;
const CANVAS_MULTIPLE: usize = 32;
const DEFAULT_SIDE: usize = 2048;
/// A preview is mapped from the latent at an eighth of the canvas.
const PREVIEW_SCALE: usize = 8;
/// What the mock says its `DiT`'s attention runs at once a picture starts.
const ATTENTION_PRECISION: &str = "fp16";

#[derive(Clone, Debug)]
pub struct Config {
    /// Reading the prompt and the references, before the first step.
    pub text: Duration,
    pub step: Duration,
    pub decode: Duration,
    /// Answer as upstream mmh3 does: no event stream for every generation, no previews, no
    /// cancelling, and none of the fork's fields.
    pub upstream: bool,
}

impl Default for Config {
    fn default() -> Config {
        Config { text: Duration::from_millis(400), step: Duration::from_millis(600), decode: Duration::from_millis(700), upstream: false }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Output {
    Video,
    Picture,
}

impl Output {
    fn kind(self) -> Kind {
        match self {
            Output::Video => Kind::Video,
            Output::Picture => Kind::Image,
        }
    }
}

/// One field of a request's form.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub filename: Option<String>,
    pub bytes: Vec<u8>,
}

impl Field {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).into_owned()
    }
}

/// Why a run ended without writing anything.
enum Stop {
    Failed(String),
    Cancelled,
}

impl From<String> for Stop {
    fn from(why: String) -> Stop {
        Stop::Failed(why)
    }
}

struct Job {
    id: String,
    output: Output,
    state: State,
    queued: SystemTime,
    started_at: Option<SystemTime>,
    finished_at: Option<SystemTime>,
    seconds: Option<f64>,
    step: Option<u32>,
    steps: Option<u32>,
    failure: Option<String>,
    fields: Vec<Field>,
    request: Asked,
    timings: Option<Timings>,
    /// Each step's preview, the first step's first.
    previews: Vec<Arc<Vec<u8>>>,
    /// Asked to stop at the next step.
    cancel: bool,
    written: Option<Arc<Vec<u8>>>,
    /// Every description the job has had, so a stream says each change however slowly it reads.
    said: Vec<Generation>,
}

fn unix(t: SystemTime) -> f64 {
    t.duration_since(UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64())
}

impl Job {
    fn describe(&self) -> Generation {
        let running = self.state == State::Running;
        Generation {
            error: self.failure.clone(),
            id: self.id.clone(),
            queued: self.queued.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs()),
            seconds: self.seconds,
            state: self.state,
            step: if running { self.step } else { None },
            steps: if running { self.steps } else { None },
            kind: Some(self.output.kind()),
            request: Some(self.request.clone()),
            created_at: Some(unix(self.queued)),
            started_at: self.started_at.map(unix),
            finished_at: self.finished_at.map(unix),
            timings: self.timings.clone(),
            preview_step: (!self.previews.is_empty()).then_some(self.previews.len() as u32),
        }
    }
}

/// What the request asked for, as the fork's server says it: a picture's defaults where the
/// request left an option out, and none where a video's are its own.
fn asked(fields: &[Field], output: Output) -> Asked {
    let given = |name: &str| fields.iter().rev().find(|f| f.name == name).map(Field::text);
    let picture = output == Output::Picture;
    let number = |name: &str, default: Option<u64>| given(name).map_or(default, |v| v.parse().ok());
    let side = picture.then_some(DEFAULT_SIDE as u64);
    Asked {
        prompt: given("prompt").or_else(|| given("prompt-file")),
        width: number("width", side).and_then(|v| u32::try_from(v).ok()),
        height: number("height", side).and_then(|v| u32::try_from(v).ok()),
        seed: number("seed", Some(0)),
        min_generation_area: number("min-generation-area", picture.then_some((DEFAULT_SIDE * DEFAULT_SIDE) as u64)),
        attention_precision: given("attention-precision"),
        attention: given("attention").or_else(|| picture.then(|| "dense".to_owned())),
        references: fields.iter().filter(|f| f.name == "reference").filter_map(|f| f.filename.clone()).collect(),
    }
}

/// An announcement of `GET /v1/events`: its event's name and data.
type Announcement = (&'static str, String);

#[derive(Default)]
struct Jobs {
    list: Vec<Job>,
    waiting: VecDeque<String>,
    count: u64,
    /// The shared table `/v1/status` lists. A picture adds what it loads to it and takes the
    /// transformer out while it steps, leaving the rest as it found it.
    table: HashSet<&'static str>,
    /// The table is busy, so `/v1/status` says `null`: while a picture decodes, and throughout
    /// a video, which lends every model to its worker.
    lent: bool,
    /// The latest announcements, the oldest first, and how many were ever made.
    announced: VecDeque<Announcement>,
    announcements: u64,
}

impl Jobs {
    fn announce(&mut self, name: &'static str, data: String) {
        self.announced.push_back((name, data));
        self.announcements += 1;
        if self.announced.len() > ANNOUNCED {
            self.announced.pop_front();
        }
    }

    /// Every generation, the most recent first.
    fn generations(&self) -> Vec<Generation> {
        let mut jobs: Vec<(SystemTime, Generation)> = self.list.iter().map(|j| (j.queued, j.describe())).collect();
        jobs.sort_by(|(a, _), (b, _)| b.cmp(a));
        jobs.into_iter().map(|(_, g)| g).collect()
    }
}

struct Shared {
    jobs: Mutex<Jobs>,
    queued: Condvar,
    changed: Condvar,
    config: Config,
    started: u64,
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, Jobs> {
        self.jobs.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn keep(&self, models: &[&'static str]) {
        self.lock().table.extend(models);
    }

    fn take(&self, model: &str) {
        self.lock().table.remove(model);
    }

    fn holds(&self, model: &str) -> bool {
        self.lock().table.contains(model)
    }

    fn lend(&self, lent: bool) {
        self.lock().lent = lent;
    }

    fn cancelled(&self, id: &str) -> bool {
        self.lock().list.iter().any(|j| j.id == id && j.cancel)
    }

    /// The generation as this server writes it: the fork's fields as they are, `null` included,
    /// or none of them from an upstream server.
    fn wire(&self, g: &Generation) -> serde_json::Value {
        let mut value = serde_json::to_value(g).unwrap_or_default();
        if let Some(fields) = value.as_object_mut() {
            for name in FORK_FIELDS {
                if self.config.upstream {
                    fields.remove(name);
                } else {
                    fields.entry(name).or_insert(serde_json::Value::Null);
                }
            }
        }
        value
    }

    fn list(&self, generations: &[Generation]) -> String {
        let generations: Vec<serde_json::Value> = generations.iter().map(|g| self.wire(g)).collect();
        serde_json::json!({ "generations": generations }).to_string()
    }

    /// Changes job `id` and says so: to its own event stream, and to `GET /v1/events` with a
    /// `generation` and, when it brought one, a `preview`.
    fn update(&self, id: &str, f: impl FnOnce(&mut Job)) {
        let mut jobs = self.lock();
        if let Some(job) = jobs.list.iter_mut().find(|j| j.id == id) {
            let before = job.said.last().and_then(|g| g.preview_step);
            f(job);
            let now = job.describe();
            if job.said.last() != Some(&now) {
                job.said.push(now.clone());
                let generation = self.wire(&now).to_string();
                jobs.announce("generation", generation);
                if let Some(step) = now.preview_step.filter(|s| before != Some(*s)) {
                    let url = format!("/v1/generations/{id}/preview/{step}");
                    let preview = serde_json::json!({ "id": id, "step": step, "steps": now.steps, "url": url });
                    jobs.announce("preview", preview.to_string());
                }
            }
        }
        drop(jobs);
        self.changed.notify_all();
    }

    /// Changes the timings of job `id`, which it reports stage by stage.
    fn time(&self, id: &str, f: impl FnOnce(&mut Timings)) {
        self.update(id, |j| f(j.timings.get_or_insert_with(Timings::default)));
    }
}

/// Binds `addr` and serves it on background threads; returns the address it bound.
pub fn spawn(addr: &str, config: Config) -> std::io::Result<SocketAddr> {
    let listener = TcpListener::bind(addr)?;
    let bound = listener.local_addr()?;
    let started = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let jobs = Jobs::default();
    let shared = Arc::new(Shared { jobs: Mutex::new(jobs), queued: Condvar::new(), changed: Condvar::new(), config, started });
    let worker = shared.clone();
    std::thread::Builder::new().name("mock-worker".into()).spawn(move || work(&worker))?;
    std::thread::Builder::new().name("mock-accept".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            let shared = shared.clone();
            let _ = std::thread::Builder::new().name("mock-conn".into()).spawn(move || {
                let _ = handle(stream, &shared);
            });
        }
    })?;
    Ok(bound)
}

fn work(shared: &Shared) {
    loop {
        let (id, output, fields) = {
            let mut jobs = shared.lock();
            let id = loop {
                if let Some(id) = jobs.waiting.pop_front() {
                    break id;
                }
                jobs = shared.queued.wait(jobs).unwrap_or_else(std::sync::PoisonError::into_inner);
            };
            let Some(job) = jobs.list.iter().find(|j| j.id == id && j.state == State::Queued) else { continue };
            (id, job.output, job.fields.clone())
        };
        let started = Instant::now();
        shared.update(&id, |j| {
            j.state = State::Running;
            j.started_at = Some(SystemTime::now());
            if output == Output::Picture && j.request.attention_precision.is_none() {
                j.request.attention_precision = Some(ATTENTION_PRECISION.to_owned());
            }
        });
        let ended = match output {
            Output::Picture => picture(shared, &id, &fields),
            Output::Video => video(shared, &id, &fields),
        };
        shared.lend(false);
        shared.update(&id, |j| {
            j.seconds = Some(started.elapsed().as_secs_f64());
            j.finished_at = Some(SystemTime::now());
            match ended {
                Ok(written) => {
                    j.state = State::Done;
                    j.written = Some(Arc::new(written));
                }
                Err(Stop::Failed(why)) => {
                    j.state = State::Failed;
                    j.failure = Some(why);
                }
                Err(Stop::Cancelled) => j.state = State::Cancelled,
            }
        });
    }
}

/// Takes `steps` timed steps, each of a picture previewed with `preview`, and stops at a step's
/// boundary when the job is cancelled.
fn steps(shared: &Shared, id: &str, steps: u32, preview: Option<&dyn Fn(u32) -> Vec<u8>>) -> Result<(), Stop> {
    for k in 1..=steps {
        if shared.cancelled(id) {
            return Err(Stop::Cancelled);
        }
        let t = Instant::now();
        std::thread::sleep(shared.config.step);
        let png = preview.map(|p| Arc::new(p(k)));
        let seconds = t.elapsed().as_secs_f64();
        shared.update(id, |j| {
            j.step = Some(k);
            j.steps = Some(steps);
            if let Some(png) = png {
                j.timings.get_or_insert_with(Timings::default).steps.push(seconds);
                j.previews.push(png);
            }
        });
    }
    Ok(())
}

/// `option_number` of mmh3's command line.
fn number(fields: &[Field], name: &str, default: usize) -> Result<usize, String> {
    match fields.iter().rev().find(|f| f.name == name) {
        Some(f) => f.text().replace('_', "").parse().map_err(|_| format!("--{name} must be a number")),
        None => Ok(default),
    }
}

/// Times `f` as stage `stage` of job `id`'s timings.
fn timed<T>(shared: &Shared, id: &str, stage: impl FnOnce(&mut Timings) -> &mut Option<f64>, f: impl FnOnce() -> T) -> T {
    let t = Instant::now();
    let out = f();
    let seconds = t.elapsed().as_secs_f64();
    shared.time(id, |timings| *stage(timings) = Some(seconds));
    out
}

/// What `mmh3 image` would make of the fields, in the order it checks them.
fn picture(shared: &Shared, id: &str, fields: &[Field]) -> Result<Vec<u8>, Stop> {
    let width = number(fields, "width", DEFAULT_SIDE)?;
    let height = number(fields, "height", DEFAULT_SIDE)?;
    let min_area = number(fields, "min-generation-area", DEFAULT_SIDE * DEFAULT_SIDE)?;
    let seed = number(fields, "seed", 0)? as u64;
    if width < CANVAS_MULTIPLE || height < CANVAS_MULTIPLE {
        return Err(format!("--width and --height must be at least {CANVAS_MULTIPLE}").into());
    }
    if !(1024..=16_777_216).contains(&min_area) {
        return Err("--min-generation-area must be from 1024 to 16777216".to_owned().into());
    }
    let has = |name: &str| fields.iter().any(|f| f.name == name);
    let prompt = match (has("prompt"), has("prompt-file")) {
        (true, true) => return Err("pass either --prompt or --prompt-file".to_owned().into()),
        (false, false) => return Err("pass a prompt or --context, not both".to_owned().into()),
        _ => fields.iter().rev().find(|f| f.name == "prompt" || f.name == "prompt-file").map(Field::text).unwrap_or_default(),
    };
    let references: Vec<&Field> = fields.iter().filter(|f| f.name == "reference").collect();
    for r in &references {
        let name = r.filename.as_deref().unwrap_or_default();
        let picture = image::load_from_memory(&r.bytes).map_err(|e| format!("{name}: {e}"))?;
        if picture.width().min(picture.height()) < CANVAS_MULTIPLE as u32 {
            return Err(
                format!("{name}: a reference needs at least {CANVAS_MULTIPLE} pixels on each side, not {}×{}", picture.width(), picture.height()).into()
            );
        }
    }
    if references.len() > MAX_REFERENCES {
        return Err(format!("pass at most {MAX_REFERENCES} --reference pictures").into());
    }
    if shared.cancelled(id) {
        return Err(Stop::Cancelled);
    }
    if !shared.holds("text_encoder") {
        timed(shared, id, |t| &mut t.load_text, || shared.keep(&["text_encoder"]));
    }
    timed(shared, id, |t| &mut t.encode, || std::thread::sleep(shared.config.text));
    if shared.cancelled(id) {
        return Err(Stop::Cancelled);
    }
    if !shared.holds("dit") {
        timed(shared, id, |t| &mut t.load_dit, || {});
    }
    shared.take("dit");
    let (w, h) = ((width / PREVIEW_SCALE).max(1) as u32, (height / PREVIEW_SCALE).max(1) as u32);
    let preview = |k: u32| preview(w, h, seed, k, IMAGE_STEPS);
    let stepped = steps(shared, id, IMAGE_STEPS, Some(&preview));
    shared.keep(&["dit"]);
    stepped?;
    if prompt.contains("[fail]") {
        return Err("The mock server was asked to fail.".to_owned().into());
    }
    if shared.cancelled(id) {
        return Err(Stop::Cancelled);
    }
    shared.lend(true);
    shared.keep(&["video_vae"]);
    shared.time(id, |t| t.decode_read = Some(0.0));
    let png = timed(
        shared,
        id,
        |t| &mut t.decode,
        || {
            let t = Instant::now();
            let png = paint(width as u32, height as u32, seed);
            std::thread::sleep(shared.config.decode.saturating_sub(t.elapsed()));
            png
        },
    );
    shared.time(id, |t| t.write = Some(0.0));
    Ok(png)
}

fn video(shared: &Shared, id: &str, fields: &[Field]) -> Result<Vec<u8>, Stop> {
    let n = number(fields, "steps", 8)? as u32;
    shared.lend(true);
    shared.keep(&VIDEO_MODELS);
    std::thread::sleep(shared.config.text);
    steps(shared, id, n, None)?;
    std::thread::sleep(shared.config.decode);
    Ok(b"not really an mp4".to_vec())
}

/// What step `k` of `steps` predicts: the picture to come, washed towards grey the earlier the
/// step, as a real preview sharpens towards the decode.
pub fn preview(width: u32, height: u32, seed: u64, k: u32, steps: u32) -> Vec<u8> {
    let mut rgb = paint_rgb(width, height, seed);
    let t = k as f32 / steps.max(1) as f32;
    for v in &mut rgb {
        *v = (f32::from(*v) * t + 128.0 * (1.0 - t)) as u8;
    }
    png(&rgb, width, height)
}

/// A seed-coloured diagonal gradient with rings, so each generation looks different.
pub fn paint(width: u32, height: u32, seed: u64) -> Vec<u8> {
    png(&paint_rgb(width, height, seed), width, height)
}

fn paint_rgb(width: u32, height: u32, seed: u64) -> Vec<u8> {
    let hue = |n: u64| (n % 360) as f32;
    let (h1, h2) = (hue(seed.wrapping_mul(2_654_435_761)), hue(seed.wrapping_mul(40_503).wrapping_add(137)));
    let (a, b) = (hsv(h1, 0.55, 0.95), hsv(h2, 0.75, 0.35));
    let (w, h) = (width as f32, height as f32);
    let (cx, cy) = (w * (0.3 + (seed % 5) as f32 * 0.1), h * (0.35 + (seed % 3) as f32 * 0.1));
    let ring = w.min(h) / 9.0;
    let mut rgb = vec![0u8; width as usize * height as usize * 3];
    for (y, line) in rgb.chunks_exact_mut(width as usize * 3).enumerate() {
        let fy = y as f32;
        for (x, px) in line.as_chunks_mut::<3>().0.iter_mut().enumerate() {
            let fx = x as f32;
            let t = f32::midpoint(fx / w, fy / h);
            let d = ((fx - cx).powi(2) + (fy - cy).powi(2)).sqrt() / ring;
            let glow = (1.0 - (d.fract() - 0.5).abs() * 2.0).powi(6) * (1.0 - (d / 6.0).min(1.0)) * 0.35;
            for c in 0..3 {
                let v = a[c] * (1.0 - t) + b[c] * t + glow;
                px[c] = (v.clamp(0.0, 1.0) * 255.0) as u8;
            }
        }
    }
    rgb
}

fn png(rgb: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut png = Vec::new();
    let encoder = image::codecs::png::PngEncoder::new_with_quality(&mut png, image::codecs::png::CompressionType::Fast, image::codecs::png::FilterType::Sub);
    image::ImageEncoder::write_image(encoder, rgb, width, height, image::ExtendedColorType::Rgb8).map(|()| png).unwrap_or_default()
}

fn hsv(h: f32, s: f32, v: f32) -> [f32; 3] {
    let c = v * s;
    let x = c * (1.0 - ((h / 60.0) % 2.0 - 1.0).abs());
    let (r, g, b) = match (h / 60.0) as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    [r + v - c, g + v - c, b + v - c]
}

struct Request {
    method: String,
    path: String,
    content_type: String,
    body: Vec<u8>,
}

fn read_request(reader: &mut BufReader<TcpStream>) -> std::io::Result<Request> {
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().split('?').next().unwrap_or_default().to_owned();
    let (mut length, mut content_type) = (0usize, String::new());
    loop {
        let mut header = String::new();
        if reader.read_line(&mut header)? == 0 || header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':') {
            match name.trim().to_ascii_lowercase().as_str() {
                "content-length" => length = value.trim().parse().unwrap_or(0),
                "content-type" => value.trim().clone_into(&mut content_type),
                _ => {}
            }
        }
    }
    let mut body = Vec::new();
    reader.take(length.min(MAX_BODY) as u64).read_to_end(&mut body)?;
    Ok(Request { method, path, content_type, body })
}

fn respond(stream: &mut TcpStream, status: u16, headers: &[(&str, &str)], body: &[u8]) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        202 => "Accepted",
        204 => "No Content",
        400 => "Bad Request",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        _ => "Internal Server Error",
    };
    write!(stream, "HTTP/1.1 {status} {reason}\r\n")?;
    for (name, value) in headers {
        write!(stream, "{name}: {value}\r\n")?;
    }
    write!(stream, "content-length: {}\r\nconnection: close\r\n\r\n", body.len())?;
    stream.write_all(body)?;
    stream.flush()
}

fn generation(stream: &mut TcpStream, shared: &Shared, status: u16, g: &Generation, headers: &[(&str, &str)]) -> std::io::Result<()> {
    let body = shared.wire(g).to_string();
    let mut headers = headers.to_vec();
    headers.insert(0, ("content-type", "application/json"));
    respond(stream, status, &headers, body.as_bytes())
}

fn json(stream: &mut TcpStream, status: u16, value: &impl serde::Serialize) -> std::io::Result<()> {
    respond(stream, status, &[("content-type", "application/json")], &serde_json::to_vec(value).unwrap_or_default())
}

/// How the server answers when it does not answer with what was asked for.
fn refuse(stream: &mut TcpStream, status: u16, message: &str) -> std::io::Result<()> {
    json(stream, status, &Refusal { error: message.to_owned() })
}

fn handle(stream: TcpStream, shared: &Shared) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let request = read_request(&mut reader)?;
    let mut stream = stream;
    let segments: Vec<&str> = request.path.trim_matches('/').split('/').collect();
    let method = request.method.as_str();
    let fork = !shared.config.upstream;
    match (method, segments.as_slice()) {
        ("GET", ["v1", "status"]) => json(&mut stream, 200, &status(shared)),
        ("GET", ["v1", "generations"]) => {
            let list = shared.list(&shared.lock().generations());
            respond(&mut stream, 200, &[("content-type", "application/json")], list.as_bytes())
        }
        ("POST", ["v1", "generations"]) => create(&mut stream, shared, &request, Output::Video),
        ("POST", ["v1", "images"]) => create(&mut stream, shared, &request, Output::Picture),
        ("GET", ["v1", "generations", id]) => match find(shared, id) {
            Some(g) => generation(&mut stream, shared, 200, &g, &[]),
            None => refuse(&mut stream, 404, "no such generation"),
        },
        ("GET", ["v1", "events"]) if fork => all_events(stream, shared),
        ("POST", ["v1", "generations", id, "cancel"]) if fork => cancel(&mut stream, shared, id),
        ("GET", ["v1", "generations", id, "preview"]) if fork => preview_png(&mut stream, shared, id, None),
        ("GET", ["v1", "generations", id, "preview", step]) if fork => match step.parse() {
            Ok(step) => preview_png(&mut stream, shared, id, Some(step)),
            Err(_) => respond(&mut stream, 400, &[], b""),
        },
        // Upstream mmh3 has none of the fork's routes, which its router answers as any path it
        // doesn't know.
        (_, ["v1", "events"] | ["v1", "generations", _, "cancel" | "preview", ..]) if !fork => respond(&mut stream, 404, &[], b""),
        ("DELETE", ["v1", "generations", id]) => {
            let mut jobs = shared.lock();
            match jobs.list.iter().position(|j| j.id == *id) {
                None => refuse(&mut stream, 404, "no such generation"),
                Some(i) if jobs.list[i].state == State::Running => refuse(&mut stream, 409, "that generation is running"),
                Some(i) => {
                    jobs.list.remove(i);
                    jobs.waiting.retain(|w| w != id);
                    jobs.announce("removed", serde_json::json!({ "id": id }).to_string());
                    drop(jobs);
                    shared.changed.notify_all();
                    respond(&mut stream, 204, &[], b"")
                }
            }
        }
        ("GET", ["v1", "generations", id, "image"]) => written(&mut stream, shared, id, Output::Picture),
        ("GET", ["v1", "generations", id, "video"]) => written(&mut stream, shared, id, Output::Video),
        ("GET", ["v1", "generations", id, "events"]) => events(stream, shared, id),
        (_, ["v1", "status" | "images"] | ["v1", "generations", ..]) if segments.len() <= 4 => respond(&mut stream, 405, &[], b""),
        _ => respond(&mut stream, 404, &[], b""),
    }
}

fn find(shared: &Shared, id: &str) -> Option<Generation> {
    shared.lock().list.iter().find(|j| j.id == id).map(Job::describe)
}

fn written(stream: &mut TcpStream, shared: &Shared, id: &str, wanted: Output) -> std::io::Result<()> {
    let found = shared.lock().list.iter().find(|j| j.id == id).map(|j| (j.output, j.state, j.written.clone()));
    match found {
        None => refuse(stream, 404, "no such generation"),
        Some((Output::Video, ..)) if wanted == Output::Picture => refuse(stream, 404, "that generation writes a video"),
        Some((Output::Picture, ..)) if wanted == Output::Video => refuse(stream, 404, "that generation writes a picture"),
        Some((output, State::Done, Some(bytes))) => {
            let kind = if output == Output::Picture { "image/png" } else { "video/mp4" };
            respond(stream, 200, &[("content-type", kind)], &bytes)
        }
        Some((_, State::Failed, _)) => refuse(stream, 409, "that generation failed"),
        Some((_, State::Cancelled, _)) => refuse(stream, 409, "that generation was cancelled"),
        Some(_) => refuse(stream, 409, "that generation has not written anything yet"),
    }
}

/// Stops job `id`: one waiting is dropped at once, one running stops at its next step.
fn cancel(stream: &mut TcpStream, shared: &Shared, id: &str) -> std::io::Result<()> {
    // A run stops only at a step's boundary, so one past its last step goes on to its picture.
    let state = shared.lock().list.iter().find(|j| j.id == id).map(|j| (j.state, j.step.zip(j.steps).is_some_and(|(k, n)| k >= n)));
    match state.map(|(state, _)| state) {
        None => return refuse(stream, 404, "no such generation"),
        Some(State::Done | State::Failed | State::Cancelled) => return refuse(stream, 409, "that generation has ended"),
        Some(State::Running) if state.is_some_and(|(_, past)| past) => {
            return refuse(stream, 409, "that generation has taken its last step and is writing what it made");
        }
        Some(State::Queued) => {
            shared.lock().waiting.retain(|w| w != id);
            shared.update(id, |j| {
                j.state = State::Cancelled;
                j.finished_at = Some(SystemTime::now());
            });
        }
        Some(State::Running) => shared.update(id, |j| j.cancel = true),
    }
    match find(shared, id) {
        Some(g) => generation(stream, shared, 202, &g, &[]),
        None => refuse(stream, 404, "no such generation"),
    }
}

/// The preview of step `step`, counted from 1, or the latest.
fn preview_png(stream: &mut TcpStream, shared: &Shared, id: &str, step: Option<usize>) -> std::io::Result<()> {
    let found = shared.lock().list.iter().find(|j| j.id == id).map(|j| j.previews.clone());
    let Some(previews) = found else { return refuse(stream, 404, "no such generation") };
    let png = match step {
        None => previews.last().ok_or("no preview yet".to_owned()),
        Some(k) => k.checked_sub(1).and_then(|i| previews.get(i)).ok_or(format!("no preview of step {k}")),
    };
    match png {
        Ok(png) => respond(stream, 200, &[("content-type", "image/png")], png),
        Err(why) => refuse(stream, 404, &why),
    }
}

fn status(shared: &Shared) -> Status {
    let jobs = shared.lock();
    let running = jobs.list.iter().find(|j| j.state == State::Running);
    let busy = running.is_some();
    let held = (!jobs.lent).then(|| SLOTS.iter().filter(|m| jobs.table.contains(*m)).map(|m| (*m).to_owned()).collect());
    Status {
        generating: running.map(|j| j.id.clone()),
        generations: jobs.list.len() as u32,
        memory: Some(Memory { free: TOTAL_MEMORY - (38 << 30) - if busy { 21 << 30 } else { 0 }, total: TOTAL_MEMORY }),
        models: held,
        queued: jobs.list.iter().filter(|j| j.state == State::Queued).count() as u32,
    }
}

/// Why the server would refuse field `name`, as `image_field_kind` and `field_kind` say.
fn refused(output: Output, field: &Field) -> Option<String> {
    let name = field.name.as_str();
    let file = match output {
        Output::Picture if !IMAGE_OPTIONS.contains(&name) => return Some(format!("{name} is not an option of a picture")),
        Output::Picture => IMAGE_FILE_OPTIONS.contains(&name),
        Output::Video if MACHINE_OPTIONS.contains(&name) => return Some(format!("{name} is the server's to set, not a request's")),
        Output::Video => VIDEO_FILE_OPTIONS.contains(&name),
    };
    match (file, &field.filename) {
        (true, None) => Some(format!("{name} takes an uploaded file")),
        (false, Some(_)) => Some(format!("{name} takes text, not a file")),
        (true, Some(f)) if f.is_empty() || f.starts_with('.') => Some("a file name that names no file".into()),
        _ => None,
    }
}

fn create(stream: &mut TcpStream, shared: &Shared, request: &Request, output: Output) -> std::io::Result<()> {
    let Some(boundary) = request.content_type.split("boundary=").nth(1).map(|b| b.trim_matches('"')) else {
        return refuse(stream, 400, "Invalid `boundary` for `multipart/form-data` request");
    };
    let fields = parse_multipart(&request.body, boundary);
    if let Some(why) = fields.iter().find_map(|f| refused(output, f)) {
        return refuse(stream, 400, &why);
    }
    let mut jobs = shared.lock();
    let id = format!("{:x}{:04x}", shared.started, jobs.count & 0xffff);
    jobs.count += 1;
    let mut job = Job {
        id: id.clone(),
        output,
        state: State::Queued,
        queued: SystemTime::now(),
        started_at: None,
        finished_at: None,
        seconds: None,
        step: None,
        steps: None,
        failure: None,
        request: asked(&fields, output),
        fields,
        timings: None,
        previews: Vec::new(),
        cancel: false,
        written: None,
        said: Vec::new(),
    };
    let described = job.describe();
    job.said.push(described.clone());
    jobs.list.push(job);
    jobs.waiting.push_back(id.clone());
    let announced = shared.wire(&described).to_string();
    jobs.announce("generation", announced);
    drop(jobs);
    shared.queued.notify_all();
    shared.changed.notify_all();
    let location = format!("/v1/generations/{id}");
    generation(stream, shared, 202, &described, &[("location", &location)])
}

fn find_bytes(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    haystack.get(from..)?.windows(needle.len()).position(|w| w == needle).map(|i| i + from)
}

/// The fields of a `multipart/form-data` body, in the order they came.
pub fn parse_multipart(body: &[u8], boundary: &str) -> Vec<Field> {
    let delimiter = format!("\r\n--{boundary}").into_bytes();
    let mut fields = Vec::new();
    let Some(first) = find_bytes(body, &delimiter[2..], 0) else { return fields };
    let mut at = first + delimiter.len() - 2;
    while body.get(at..at + 2) == Some(b"\r\n") {
        let start = at + 2;
        let Some(head_end) = find_bytes(body, b"\r\n\r\n", start) else { break };
        let Some(end) = find_bytes(body, &delimiter, head_end + 4) else { break };
        let head = String::from_utf8_lossy(&body[start..head_end]);
        let param = |key: &str| head.split(&format!("{key}=\"")).nth(1).and_then(|s| s.split('"').next()).map(str::to_owned);
        if let Some(name) = param(" name").or_else(|| param(";name")) {
            fields.push(Field { name, filename: param("filename"), bytes: body[head_end + 4..end].to_vec() });
        }
        at = end + delimiter.len();
    }
    fields
}

fn events(mut stream: TcpStream, shared: &Shared, id: &str) -> std::io::Result<()> {
    let Some(start) = shared.lock().list.iter().find(|j| j.id == id).map(|j| j.said.len() - 1) else {
        return refuse(&mut stream, 404, "no such generation");
    };
    write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\nconnection: close\r\n\r\n")?;
    stream.flush()?;
    let mut next = start;
    loop {
        let event = {
            let mut jobs = shared.lock();
            loop {
                let Some(job) = jobs.list.iter().find(|j| j.id == id) else { return Ok(()) };
                if let Some(event) = job.said.get(next) {
                    break Some(event.clone());
                }
                let (guard, waited) = shared.changed.wait_timeout(jobs, KEEP_ALIVE).unwrap_or_else(std::sync::PoisonError::into_inner);
                jobs = guard;
                if waited.timed_out() {
                    break None;
                }
            }
        };
        let Some(event) = event else {
            write!(stream, ":\n\n")?;
            stream.flush()?;
            continue;
        };
        next += 1;
        write!(stream, "data: {}\n\n", shared.wire(&event))?;
        stream.flush()?;
        if event.is_finished() {
            return Ok(());
        }
    }
}

/// `GET /v1/events`: a snapshot of every generation, then each announcement as it is made, and
/// a fresh snapshot for a client that fell further behind than the announcements kept.
fn all_events(mut stream: TcpStream, shared: &Shared) -> std::io::Result<()> {
    let (snapshot, mut next) = {
        let jobs = shared.lock();
        (shared.list(&jobs.generations()), jobs.announcements)
    };
    write!(stream, "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\nconnection: close\r\n\r\n")?;
    write!(stream, "event: snapshot\ndata: {snapshot}\n\n")?;
    stream.flush()?;
    loop {
        let event = {
            let mut jobs = shared.lock();
            loop {
                let oldest = jobs.announcements - jobs.announced.len() as u64;
                if next < oldest {
                    next = jobs.announcements;
                    break Some(("snapshot", shared.list(&jobs.generations())));
                }
                if next < jobs.announcements {
                    let event = jobs.announced[(next - oldest) as usize].clone();
                    next += 1;
                    break Some(event);
                }
                let (guard, waited) = shared.changed.wait_timeout(jobs, KEEP_ALIVE).unwrap_or_else(std::sync::PoisonError::into_inner);
                jobs = guard;
                if waited.timed_out() {
                    break None;
                }
            }
        };
        match event {
            Some((name, data)) => write!(stream, "event: {name}\ndata: {data}\n\n")?,
            None => write!(stream, ": keep-alive\n\n")?,
        }
        stream.flush()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Part, multipart};

    #[test]
    fn multipart_from_the_client_round_trips_text_and_files() {
        let parts = [
            Part::Text { name: "prompt", value: "a\r\nb".into() },
            Part::File { name: "reference", filename: "picture-1.png".into(), bytes: b"\r\n--Xy-not-quite\0".to_vec() },
            Part::File { name: "reference", filename: "picture-2.jpg".into(), bytes: vec![255, 216] },
        ];
        let (content_type, body) = multipart(&parts, "XyZ");
        assert!(content_type.ends_with("boundary=XyZ"));
        let fields = parse_multipart(&body, "XyZ");
        assert_eq!(fields.len(), 3);
        assert_eq!((fields[0].name.as_str(), fields[0].filename.as_deref(), fields[0].text().as_str()), ("prompt", None, "a\r\nb"));
        assert_eq!((fields[1].filename.as_deref(), &fields[1].bytes[..]), (Some("picture-1.png"), &b"\r\n--Xy-not-quite\0"[..]));
        assert_eq!(fields[2].bytes, [255, 216]);
    }

    #[test]
    fn paint_makes_a_png_of_the_size_asked() {
        let png = paint(320, 192, 7);
        let img = image::load_from_memory_with_format(&png, image::ImageFormat::Png).unwrap();
        assert_eq!((img.width(), img.height()), (320, 192));
    }

    #[test]
    fn fields_are_refused_as_the_server_refuses_them() {
        let text = |name: &str| Field { name: name.into(), filename: None, bytes: Vec::new() };
        let file = |name: &str| Field { filename: Some("a.png".into()), ..text(name) };
        assert_eq!(refused(Output::Picture, &text("steps")).as_deref(), Some("steps is not an option of a picture"));
        assert_eq!(refused(Output::Picture, &text("reference")).as_deref(), Some("reference takes an uploaded file"));
        assert_eq!(refused(Output::Picture, &file("width")).as_deref(), Some("width takes text, not a file"));
        assert_eq!(refused(Output::Picture, &file("reference")), None);
        assert_eq!(refused(Output::Video, &text("steps")), None);
        assert_eq!(refused(Output::Video, &text("models")).as_deref(), Some("models is the server's to set, not a request's"));
    }
}

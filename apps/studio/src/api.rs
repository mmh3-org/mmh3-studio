//! `mmh3 server`'s API, as `docs/server.md` in mmh3 describes it: pictures are asked for at
//! `POST /v1/images` and watched like any other generation. The studio's fork of mmh3 adds what
//! a generation asked for, its stage timings, step previews, cancelling and one event stream for
//! every generation; an upstream server leaves those out, and the studio reads either. The types
//! and the form are shared; [`Client`] is the native, blocking client, so run its calls off the
//! UI thread. The web build speaks the same routes through the browser's `fetch` and
//! `EventSource`.

#[cfg(not(target_arch = "wasm32"))]
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::effort::Effort;

pub const DEFAULT_SERVER: &str = "http://127.0.0.1:8833";
/// The picture model is distilled to six steps, which a request cannot change.
pub const IMAGE_STEPS: u32 = 6;
/// The most reference pictures `mmh3 image` edits.
pub const MAX_REFERENCES: usize = 16;
#[cfg(not(target_arch = "wasm32"))]
const MAX_PNG_BYTES: u64 = 512 * 1024 * 1024;
/// How long a stream or a picture may go without a byte before its connection is taken for dead.
/// The server comments on an idle event stream every 15 s, so a live one never stays this quiet.
#[cfg(not(target_arch = "wasm32"))]
const SILENCE: Duration = Duration::from_secs(60);

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    #[default]
    Queued,
    Running,
    Done,
    Failed,
    /// Stopped on request before it ended; only the fork's server cancels.
    Cancelled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Image,
    Video,
}

/// A generation as the server describes it. An upstream server says nothing of what was asked
/// for, nor whether it writes a picture or a video; the fork's says both, and the fields it adds
/// are `None` from an upstream one.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Generation {
    pub error: Option<String>,
    pub id: String,
    pub queued: u64,
    pub seconds: Option<f64>,
    pub state: State,
    pub step: Option<u32>,
    pub steps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<Kind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<Asked>,
    /// Unix times in seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<f64>,
    /// Where a picture's time went, filled in stage by stage as it runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timings: Option<Timings>,
    /// The last step with a preview, counted from 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_step: Option<u32>,
}

/// What a generation asked for, as the fork's server says it: a picture's defaults where the
/// request left an option out, and the names its references were uploaded under. A value the
/// server could not read as a number stays as it was sent, which reads here as none.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Asked {
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default, deserialize_with = "number")]
    pub width: Option<u32>,
    #[serde(default, deserialize_with = "number")]
    pub height: Option<u32>,
    #[serde(default, deserialize_with = "number")]
    pub seed: Option<u64>,
    #[serde(default, deserialize_with = "number")]
    pub min_generation_area: Option<u64>,
    #[serde(default)]
    pub attention_precision: Option<String>,
    #[serde(default)]
    pub attention: Option<String>,
    /// Sol-Attn's threshold and start, and the draft steps, which a server may also say.
    #[serde(default, deserialize_with = "decimal")]
    pub sparse_tau: Option<f64>,
    #[serde(default, deserialize_with = "decimal")]
    pub sparse_start: Option<f64>,
    #[serde(default, deserialize_with = "number")]
    pub draft_steps: Option<u32>,
    #[serde(default)]
    pub references: Vec<String>,
}

impl Asked {
    /// What the server says was asked for of the options in [`Tuning`], each `None` where it
    /// says nothing or says the default.
    pub fn tuning(&self) -> Tuning {
        Tuning {
            attention: self.attention.as_deref().and_then(Attention::parse).filter(|a| *a != Attention::Dense),
            sparse_tau: self.sparse_tau,
            sparse_start: self.sparse_start,
            draft_steps: self.draft_steps.filter(|n| *n > 0),
            attention_precision: self.attention_precision.as_deref().and_then(Precision::parse).filter(|p| *p != Precision::Fp16),
        }
    }
}

fn number<'de, D: serde::Deserializer<'de>, T: TryFrom<u64>>(d: D) -> Result<Option<T>, D::Error> {
    Ok(serde_json::Value::deserialize(d)?.as_u64().and_then(|n| T::try_from(n).ok()))
}

/// A number the server may write as one or as the text it was sent as.
fn decimal<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<f64>, D::Error> {
    let v = serde_json::Value::deserialize(d)?;
    Ok(v.as_f64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())).filter(|x: &f64| x.is_finite()))
}

fn list<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<f64>, D::Error> {
    Ok(Option::<Vec<f64>>::deserialize(d)?.unwrap_or_default())
}

/// A picture's stages, in seconds, as the fork's server times them. A stage not reached yet, or
/// skipped (the text encoder's when the server kept the encoded prompt), is `None`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Timings {
    /// Reading the text encoder.
    #[serde(default)]
    pub load_text: Option<f64>,
    /// Encoding the prompt and the references.
    #[serde(default)]
    pub encode: Option<f64>,
    /// Reading the `DiT`.
    #[serde(default)]
    pub load_dit: Option<f64>,
    /// Each denoising step, in order.
    #[serde(default, deserialize_with = "list")]
    pub steps: Vec<f64>,
    /// How long the decode waited for the picture VAE, which is read during the steps.
    #[serde(default)]
    pub decode_read: Option<f64>,
    #[serde(default)]
    pub decode: Option<f64>,
    /// Writing the PNG.
    #[serde(default)]
    pub write: Option<f64>,
}

impl Timings {
    /// Reading models that were not resident, or `None` when none of it was timed.
    pub fn load(&self) -> Option<f64> {
        [self.load_text, self.load_dit, self.decode_read].into_iter().flatten().reduce(|a, b| a + b)
    }
}

impl Generation {
    pub fn is_finished(&self) -> bool {
        matches!(self.state, State::Done | State::Failed | State::Cancelled)
    }
}

/// A preview a picture has written, as `GET /v1/events` announces it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preview {
    pub id: String,
    pub step: u32,
    #[serde(default)]
    pub steps: Option<u32>,
    pub url: String,
}

/// One event of `GET /v1/events`.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// Every generation, the most recent first: on connecting, and again for a client that fell
    /// behind.
    Snapshot(Vec<Generation>),
    /// One generation whose state or step changed.
    Generation(Box<Generation>),
    Preview(Preview),
    /// The id of a generation taken away, by this studio or another client.
    Removed(String),
}

#[derive(Deserialize)]
struct Removed {
    id: String,
}

impl Event {
    /// The event named `name` with `data`; `None` for a name the studio doesn't know.
    pub fn parse(name: &str, data: &str) -> Result<Option<Event>, Error> {
        let malformed = |e: serde_json::Error| Error::Malformed(e.to_string());
        Ok(Some(match name {
            "snapshot" => Event::Snapshot(serde_json::from_str::<Generations>(data).map_err(malformed)?.generations),
            "generation" => Event::Generation(Box::new(serde_json::from_str(data).map_err(malformed)?)),
            "preview" => Event::Preview(serde_json::from_str(data).map_err(malformed)?),
            "removed" => Event::Removed(serde_json::from_str::<Removed>(data).map_err(malformed)?.id),
            _ => return Ok(None),
        }))
    }
}

/// Whether an answer says the server has no such route: an upstream server, which has no event
/// stream for every generation, no previews and no cancelling.
pub fn is_missing_route(e: &Error) -> bool {
    matches!(e, Error::Refused { status: 404 | 405, .. })
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Generations {
    pub generations: Vec<Generation>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Memory {
    pub free: u64,
    pub total: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub generating: Option<String>,
    pub generations: u32,
    pub memory: Option<Memory>,
    pub models: Option<Vec<String>>,
    pub queued: u32,
}

/// Which attention a picture's full-size steps run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Attention {
    Dense,
    /// Sol-Attn, a block-sparse attention that leaves out what matters least.
    Sol,
}

impl Attention {
    pub fn field(self) -> &'static str {
        match self {
            Attention::Dense => "dense",
            Attention::Sol => "sol",
        }
    }

    pub fn parse(text: &str) -> Option<Attention> {
        [Attention::Dense, Attention::Sol].into_iter().find(|a| a.field() == text)
    }
}

/// The attention precision of a server on Metal, the only ones the studio offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Precision {
    Fp16,
    /// FP16, with the scores from INT8 queries and keys.
    Int8Fp16,
}

impl Precision {
    pub fn field(self) -> &'static str {
        match self {
            Precision::Fp16 => "fp16",
            Precision::Int8Fp16 => "int8-fp16",
        }
    }

    pub fn parse(text: &str) -> Option<Precision> {
        [Precision::Fp16, Precision::Int8Fp16].into_iter().find(|p| p.field() == text)
    }
}

/// The options of a picture that trade its quality for time. Each is `None` where the request
/// leaves the server's default, so a request that changes nothing sends none of them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Tuning {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention: Option<Attention>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sparse_tau: Option<f64>,
    /// The fraction of the schedule where Sol-Attn starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sparse_start: Option<f64>,
    /// How many of the first steps run on a canvas of half the width and height.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_steps: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attention_precision: Option<Precision>,
}

impl Tuning {
    pub fn is_empty(&self) -> bool {
        *self == Tuning::default()
    }

    /// The form fields, in the order they are sent.
    pub fn fields(&self) -> Vec<(&'static str, String)> {
        let mut fields = Vec::new();
        if let Some(a) = self.attention {
            fields.push(("attention", a.field().to_owned()));
        }
        if let Some(tau) = self.sparse_tau {
            fields.push(("sparse-tau", tau.to_string()));
        }
        if let Some(start) = self.sparse_start {
            fields.push(("sparse-start", start.to_string()));
        }
        if let Some(n) = self.draft_steps {
            fields.push(("draft-steps", n.to_string()));
        }
        if let Some(p) = self.attention_precision {
            fields.push(("attention-precision", p.field().to_owned()));
        }
        fields
    }
}

/// What the studio asks `POST /v1/images` for. It is also what the studio remembers of a
/// generation, since the server hands none of it back.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ImageRequest {
    pub prompt: String,
    pub seed: u64,
    pub width: u32,
    pub height: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_generation_area: Option<u64>,
    /// The effort the composer was set to, which only the studio knows; `None` for a request
    /// remembered before there was one, or run again from what a server said.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    /// What that effort sent.
    #[serde(default, skip_serializing_if = "Tuning::is_empty")]
    pub tuning: Tuning,
    /// Pictures to edit, `<Picture 1>` first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<PathBuf>,
}

/// One field of a `multipart/form-data` body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Part {
    Text { name: &'static str, value: String },
    File { name: &'static str, filename: String, bytes: Vec<u8> },
}

impl ImageRequest {
    /// The form's text fields, in the order they are sent; the references follow them.
    pub fn fields(&self) -> Vec<(&'static str, String)> {
        let mut fields =
            vec![("prompt", self.prompt.clone()), ("seed", self.seed.to_string()), ("width", self.width.to_string()), ("height", self.height.to_string())];
        if let Some(area) = self.min_generation_area {
            fields.push(("min-generation-area", area.to_string()));
        }
        fields.extend(self.tuning.fields());
        fields
    }

    /// The form's fields, with each reference read from its file.
    pub fn parts(&self) -> std::io::Result<Vec<Part>> {
        let mut parts: Vec<Part> = self.fields().into_iter().map(|(name, value)| Part::Text { name, value }).collect();
        for (i, path) in self.references.iter().enumerate() {
            let bytes = std::fs::read(path).map_err(|e| std::io::Error::new(e.kind(), format!("{}: {e}", path.display())))?;
            parts.push(Part::File { name: REFERENCE_FIELD, filename: reference_name(i, path), bytes });
        }
        Ok(parts)
    }
}

/// The form field each reference picture is sent in.
pub const REFERENCE_FIELD: &str = "reference";

/// The name reference `i` is uploaded under. The server writes every upload of a request into
/// one folder under the name it was sent with, so two references called `photo.jpg` would become
/// one file there.
pub fn reference_name(i: usize, path: &Path) -> String {
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("png").to_ascii_lowercase();
    format!("picture-{}.{extension}", i + 1)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// Nothing answered: the server isn't running, or the address is wrong.
    Unreachable(String),
    /// The server answered with an error status; its body says why.
    Refused { status: u16, message: String },
    /// The answer wasn't what the server's documentation says.
    Malformed(String),
    /// A reference couldn't be read, so nothing was sent.
    Unreadable(String),
}

impl Error {
    pub fn is_unreachable(&self) -> bool {
        matches!(self, Error::Unreachable(_))
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unreachable(why) => write!(f, "The server didn't answer: {why}"),
            Error::Refused { status, message } if message.is_empty() => write!(f, "The server refused it (HTTP {status})."),
            Error::Refused { status, message } => write!(f, "{message} (HTTP {status})"),
            Error::Malformed(why) => write!(f, "The server's answer couldn't be read: {why}"),
            Error::Unreadable(why) => write!(f, "Couldn't read a reference picture: {why}"),
        }
    }
}

impl std::error::Error for Error {}

/// The body of every error the server answers with.
#[derive(Serialize, Deserialize)]
pub struct Refusal {
    pub error: String,
}

/// The error for a response with a status outside 2xx and this body. A gateway's 502, 503 or
/// 504 means whatever proxies the server (the web build's host, `tailscale serve`) couldn't
/// reach it, which is the server not running, as a refused connection is natively.
pub fn refusal(status: u16, body: &str) -> Error {
    match serde_json::from_str::<Refusal>(body) {
        Ok(r) => Error::Refused { status, message: r.error },
        Err(_) if matches!(status, 502..=504) => Error::Unreachable(format!("the proxy in front of it couldn't reach it (HTTP {status})")),
        Err(_) => Error::Refused { status, message: body.trim().to_owned() },
    }
}

/// Builds a `multipart/form-data` body; returns it with its content type.
pub fn multipart(parts: &[Part], boundary: &str) -> (String, Vec<u8>) {
    let mut body = Vec::new();
    for part in parts {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        match part {
            Part::Text { name, value } => {
                body.extend_from_slice(format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes());
                body.extend_from_slice(value.as_bytes());
            }
            Part::File { name, filename, bytes } => {
                let png = Path::new(filename).extension().is_some_and(|e| e.eq_ignore_ascii_case("png"));
                let kind = if png { "image/png" } else { "image/jpeg" };
                body.extend_from_slice(
                    format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\nContent-Type: {kind}\r\n\r\n").as_bytes(),
                );
                body.extend_from_slice(bytes);
            }
        }
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (format!("multipart/form-data; boundary={boundary}"), body)
}

/// One server-sent event: its name (`message` when it gave none) and its data.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SseEvent {
    pub name: String,
    pub data: String,
}

/// Reads server-sent events line by line: `event:` names the event, `data:` lines gather, and a
/// blank line ends it. Comments, the stream's keep-alives, are skipped.
#[derive(Default)]
pub struct SseParser {
    name: Option<String>,
    data: Vec<String>,
}

impl SseParser {
    pub fn line(&mut self, line: &str) -> Option<SseEvent> {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            let name = self.name.take();
            if self.data.is_empty() {
                return None;
            }
            return Some(SseEvent { name: name.unwrap_or_else(|| "message".to_owned()), data: std::mem::take(&mut self.data).join("\n") });
        }
        let value = |rest: &str| rest.strip_prefix(' ').unwrap_or(rest).to_owned();
        if let Some(rest) = line.strip_prefix("data:") {
            self.data.push(value(rest));
        } else if let Some(rest) = line.strip_prefix("event:") {
            self.name = Some(value(rest));
        }
        None
    }

    pub fn finish(&mut self) -> Option<SseEvent> {
        self.line("")
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[derive(Clone)]
pub struct Client {
    base: String,
    quick: ureq::Agent,
    upload: ureq::Agent,
    long: ureq::Agent,
}

#[cfg(not(target_arch = "wasm32"))]
impl Client {
    pub fn new(base: impl Into<String>) -> Client {
        Client::with_silence(base, SILENCE)
    }

    pub(crate) fn with_silence(base: impl Into<String>, silence: Duration) -> Client {
        let base = base.into().trim_end_matches('/').to_owned();
        let agent = |global: Option<Duration>, per_read: Option<Duration>| -> ureq::Agent {
            ureq::Agent::config_builder()
                .http_status_as_error(false)
                .timeout_connect(Some(Duration::from_secs(2)))
                .timeout_global(global)
                .timeout_per_read(per_read)
                .build()
                .into()
        };
        Client { base, quick: agent(Some(Duration::from_secs(5)), None), upload: agent(Some(Duration::from_secs(120)), None), long: agent(None, Some(silence)) }
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }

    pub fn status(&self) -> Result<Status, Error> {
        json(self.quick.get(&self.url("/v1/status")).call())
    }

    /// Every generation on the server, pictures and videos alike, the most recent first.
    pub fn list(&self) -> Result<Vec<Generation>, Error> {
        json::<Generations>(self.quick.get(&self.url("/v1/generations")).call()).map(|l| l.generations)
    }

    pub fn generation(&self, id: &str) -> Result<Generation, Error> {
        json(self.quick.get(&self.url(&format!("/v1/generations/{id}"))).call())
    }

    pub fn create_image(&self, request: &ImageRequest) -> Result<Generation, Error> {
        let parts = request.parts().map_err(|e| Error::Unreadable(e.to_string()))?;
        let boundary = format!("mmh3-studio-{:016x}", request.seed ^ 0x9e37_79b9_7f4a_7c15);
        let (content_type, body) = multipart(&parts, &boundary);
        json(self.upload.post(&self.url("/v1/images")).header("Content-Type", &content_type).send(&body[..]))
    }

    pub fn delete(&self, id: &str) -> Result<(), Error> {
        checked(self.quick.delete(&self.url(&format!("/v1/generations/{id}"))).call()).map(drop)
    }

    /// The picture a generation wrote. Refused with 409 while the picture is still to come or
    /// will never come, and with 404 when the generation writes a video or is gone.
    pub fn image(&self, id: &str) -> Result<Vec<u8>, Error> {
        let mut response = checked(self.long.get(&self.url(&format!("/v1/generations/{id}/image"))).call())?;
        response.body_mut().with_config().limit(MAX_PNG_BYTES).read_to_vec().map_err(|e| Error::Unreachable(e.to_string()))
    }

    /// Stops a generation: one queued at once, one running at its next step. Answers with the
    /// generation as it is now. An upstream server has no such route and refuses with 404.
    pub fn cancel(&self, id: &str) -> Result<Generation, Error> {
        json(self.quick.post(&self.url(&format!("/v1/generations/{id}/cancel"))).send_empty())
    }

    /// The PNG a picture previewed after step `step`, counted from 1.
    pub fn preview(&self, id: &str, step: u32) -> Result<Vec<u8>, Error> {
        let mut response = checked(self.long.get(&self.url(&format!("/v1/generations/{id}/preview/{step}"))).call())?;
        response.body_mut().with_config().limit(MAX_PNG_BYTES).read_to_vec().map_err(|e| Error::Unreachable(e.to_string()))
    }

    /// Follows a generation's events until the server closes the stream, handing each to `each`.
    pub fn events(&self, id: &str, mut each: impl FnMut(Generation)) -> Result<(), Error> {
        let response = checked(self.long.get(&self.url(&format!("/v1/generations/{id}/events"))).header("Accept", "text/event-stream").call())?;
        read_events(response.into_body().into_reader(), |event| {
            each(serde_json::from_str(&event.data).map_err(|e| Error::Malformed(e.to_string()))?);
            Ok(())
        })
    }

    /// Follows every generation's events from `GET /v1/events`, which never ends by itself, handing
    /// each to `each`. An upstream server has no such route and refuses with 404.
    pub fn feed(&self, mut each: impl FnMut(Event)) -> Result<(), Error> {
        let response = checked(self.long.get(&self.url("/v1/events")).header("Accept", "text/event-stream").call())?;
        read_events(response.into_body().into_reader(), |event| {
            if let Some(event) = Event::parse(&event.name, &event.data)? {
                each(event);
            }
            Ok(())
        })
    }
}

/// Hands each event of the stream `reader` to `each` until the stream ends or `each` fails.
#[cfg(not(target_arch = "wasm32"))]
pub fn read_events(reader: impl Read, mut each: impl FnMut(SseEvent) -> Result<(), Error>) -> Result<(), Error> {
    let mut parser = SseParser::default();
    for line in BufReader::new(reader).lines() {
        let line = line.map_err(|e| Error::Unreachable(e.to_string()))?;
        if let Some(event) = parser.line(&line) {
            each(event)?;
        }
    }
    if let Some(event) = parser.finish() {
        each(event)?;
    }
    Ok(())
}

#[cfg(not(target_arch = "wasm32"))]
type Answer = Result<ureq::http::Response<ureq::Body>, ureq::Error>;

#[cfg(not(target_arch = "wasm32"))]
fn checked(answer: Answer) -> Result<ureq::http::Response<ureq::Body>, Error> {
    let mut response = answer.map_err(|e| Error::Unreachable(e.to_string()))?;
    let status = response.status().as_u16();
    if (200..300).contains(&status) {
        return Ok(response);
    }
    let body = response.body_mut().read_to_string().unwrap_or_default();
    Err(refusal(status, &body))
}

#[cfg(not(target_arch = "wasm32"))]
fn json<T: serde::de::DeserializeOwned>(answer: Answer) -> Result<T, Error> {
    let mut response = checked(answer)?;
    let text = response.body_mut().read_to_string().map_err(|e| Error::Unreachable(e.to_string()))?;
    serde_json::from_str(&text).map_err(|e| Error::Malformed(e.to_string()))
}

#[cfg(test)]
mod tests;

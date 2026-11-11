//! `mmh3 server`'s API, as `docs/server.md` in mmh3 describes it: pictures are asked for at
//! `POST /v1/images` and watched like any other generation. The types and the form are shared;
//! [`Client`] is the native, blocking client, so run its calls off the UI thread. The web build
//! speaks the same routes through the browser's `fetch` and `EventSource`.

#[cfg(not(target_arch = "wasm32"))]
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
#[cfg(not(target_arch = "wasm32"))]
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub const DEFAULT_SERVER: &str = "http://127.0.0.1:8833";
/// The picture model is distilled to six steps, which a request cannot change.
pub const IMAGE_STEPS: u32 = 6;
/// The most reference pictures `mmh3 image` edits.
pub const MAX_REFERENCES: usize = 16;
#[cfg(not(target_arch = "wasm32"))]
const MAX_PNG_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Queued,
    Running,
    Done,
    Failed,
}

/// A generation as the server describes it. It says nothing of what was asked for, nor whether
/// it writes a picture or a video.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Generation {
    pub error: Option<String>,
    pub id: String,
    pub queued: u64,
    pub seconds: Option<f64>,
    pub state: State,
    pub step: Option<u32>,
    pub steps: Option<u32>,
}

impl Generation {
    pub fn is_finished(&self) -> bool {
        matches!(self.state, State::Done | State::Failed)
    }
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

/// What the studio asks `POST /v1/images` for. It is also what the studio remembers of a
/// generation, since the server hands none of it back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageRequest {
    pub prompt: String,
    pub seed: u64,
    pub width: u32,
    pub height: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_generation_area: Option<u64>,
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

/// Reads server-sent events line by line: `data:` lines gather until a blank line ends the event.
#[derive(Default)]
pub struct SseParser {
    data: Vec<String>,
}

impl SseParser {
    pub fn line(&mut self, line: &str) -> Option<String> {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.is_empty() {
            if self.data.is_empty() {
                return None;
            }
            return Some(std::mem::take(&mut self.data).join("\n"));
        }
        if let Some(rest) = line.strip_prefix("data:") {
            self.data.push(rest.strip_prefix(' ').unwrap_or(rest).to_owned());
        }
        None
    }

    pub fn finish(&mut self) -> Option<String> {
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
        let base = base.into().trim_end_matches('/').to_owned();
        let agent = |global: Option<Duration>| -> ureq::Agent {
            ureq::Agent::config_builder().http_status_as_error(false).timeout_connect(Some(Duration::from_secs(2))).timeout_global(global).build().into()
        };
        Client { base, quick: agent(Some(Duration::from_secs(5))), upload: agent(Some(Duration::from_secs(120))), long: agent(None) }
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

    /// Follows a generation's events until the server closes the stream, handing each to `each`.
    pub fn events(&self, id: &str, each: impl FnMut(Generation)) -> Result<(), Error> {
        let response = checked(self.long.get(&self.url(&format!("/v1/generations/{id}/events"))).header("Accept", "text/event-stream").call())?;
        read_events(response.into_body().into_reader(), each)
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn read_events(reader: impl Read, mut each: impl FnMut(Generation)) -> Result<(), Error> {
    let mut parser = SseParser::default();
    let mut emit = |data: String| -> Result<(), Error> {
        let g = serde_json::from_str(&data).map_err(|e| Error::Malformed(e.to_string()))?;
        each(g);
        Ok(())
    };
    for line in BufReader::new(reader).lines() {
        let line = line.map_err(|e| Error::Unreachable(e.to_string()))?;
        if let Some(data) = parser.line(&line) {
            emit(data)?;
        }
    }
    if let Some(data) = parser.finish() {
        emit(data)?;
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
    let message = serde_json::from_str::<Refusal>(&body).map_or_else(|_| body.trim().to_owned(), |r| r.error);
    Err(Error::Refused { status, message })
}

#[cfg(not(target_arch = "wasm32"))]
fn json<T: serde::de::DeserializeOwned>(answer: Answer) -> Result<T, Error> {
    let mut response = checked(answer)?;
    let text = response.body_mut().read_to_string().map_err(|e| Error::Unreachable(e.to_string()))?;
    serde_json::from_str(&text).map_err(|e| Error::Malformed(e.to_string()))
}

#[cfg(test)]
mod tests;

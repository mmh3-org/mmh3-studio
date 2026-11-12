//! A tiny host for the web build: the built folder at `/` and mmh3 server's `/v1` routes passed
//! through to it, so the page and the API share an origin and the browser needs no CORS. For
//! local testing only; `tailscale serve` or any reverse proxy does the same for real use.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// The longest request head the proxy reads; a browser's are far shorter.
const MAX_HEAD: usize = 64 * 1024;
/// How long a client may take to send its request's head.
const HEAD_TIMEOUT: Duration = Duration::from_secs(30);

/// Serves `dir` and passes `/v1/*` to `upstream` on `addr`; returns the address it listens on.
pub fn spawn(addr: &str, dir: PathBuf, upstream: String) -> std::io::Result<SocketAddr> {
    let listener = TcpListener::bind(addr)?;
    let bound = listener.local_addr()?;
    let shared = Arc::new((dir, upstream));
    std::thread::Builder::new().name("proxy-accept".into()).spawn(move || {
        for stream in listener.incoming().flatten() {
            let shared = shared.clone();
            let _ = std::thread::Builder::new().name("proxy-conn".into()).spawn(move || {
                let _ = handle(stream, &shared.0, &shared.1);
            });
        }
    })?;
    Ok(bound)
}

enum Head {
    Read(Vec<String>),
    TooLarge,
    Closed,
}

/// The request's head, up to and including its blank line, in at most [`MAX_HEAD`] bytes.
fn read_head(reader: &mut BufReader<TcpStream>) -> std::io::Result<Head> {
    let mut head = Vec::new();
    let mut left = MAX_HEAD as u64;
    loop {
        let mut line = String::new();
        let read = reader.by_ref().take(left).read_line(&mut line)?;
        if read == 0 {
            return Ok(if left == 0 { Head::TooLarge } else { Head::Closed });
        }
        left -= read as u64;
        if !line.ends_with('\n') {
            return Ok(if left == 0 { Head::TooLarge } else { Head::Closed });
        }
        let end = line.trim().is_empty();
        head.push(line);
        if end {
            return Ok(Head::Read(head));
        }
    }
}

fn handle(mut stream: TcpStream, dir: &Path, upstream: &str) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    stream.set_read_timeout(Some(HEAD_TIMEOUT))?;
    let head = match read_head(&mut reader)? {
        Head::Read(head) => head,
        Head::TooLarge => {
            return respond(&mut stream, "431 Request Header Fields Too Large", "text/plain", b"The request's head is too large.\n", false);
        }
        Head::Closed => return Ok(()),
    };
    // A request's body and an event stream may be slow by right; only the head has to be prompt.
    stream.set_read_timeout(None)?;
    let mut request = head[0].split_whitespace();
    let (method, target) = (request.next().unwrap_or_default().to_owned(), request.next().unwrap_or_default().to_owned());
    let path = target.split(['?', '#']).next().unwrap_or_default();
    if path == "/v1" || path.starts_with("/v1/") {
        return pass(reader, stream, &head, upstream);
    }
    if method != "GET" && method != "HEAD" {
        return respond(&mut stream, "405 Method Not Allowed", "text/plain", b"Only GET is served here.\n", false);
    }
    match file(dir, path).and_then(|f| std::fs::read(&f).ok().map(|b| (f, b))) {
        Some((file, bytes)) => respond(&mut stream, "200 OK", content_type(&file), &bytes, method == "HEAD"),
        None => respond(&mut stream, "404 Not Found", "text/plain", b"Not found.\n", method == "HEAD"),
    }
}

/// The file a URL path names under `dir`, `index.html` for a folder; never outside `dir`.
fn file(dir: &Path, path: &str) -> Option<PathBuf> {
    let relative = Path::new(path.trim_start_matches('/'));
    if relative.components().any(|c| !matches!(c, Component::Normal(_))) {
        return None;
    }
    let file = dir.join(relative);
    let file = if file.is_dir() { file.join("index.html") } else { file };
    file.is_file().then_some(file)
}

fn content_type(file: &Path) -> &'static str {
    match file.extension().and_then(|e| e.to_str()).unwrap_or_default() {
        "html" => "text/html; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "wasm" => "application/wasm",
        "ttf" => "font/ttf",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

fn respond(stream: &mut TcpStream, status: &str, kind: &str, body: &[u8], head_only: bool) -> std::io::Result<()> {
    // `no-cache` so a rebuilt page is picked up on the next reload.
    write!(stream, "HTTP/1.1 {status}\r\ncontent-type: {kind}\r\ncontent-length: {}\r\ncache-control: no-cache\r\nconnection: close\r\n\r\n", body.len())?;
    if !head_only {
        stream.write_all(body)?;
    }
    stream.flush()
}

/// Passes one request to the server and its answer back, byte for byte, so event streams and
/// large pictures flow as they are written. The request asks the server to close the
/// connection after its answer, which ends the copy.
fn pass(mut reader: BufReader<TcpStream>, mut client: TcpStream, head: &[String], upstream: &str) -> std::io::Result<()> {
    let server = upstream.to_socket_addrs().ok().and_then(|mut a| a.next()).and_then(|a| TcpStream::connect(a).ok());
    let Some(mut server) = server else {
        return respond(&mut client, "502 Bad Gateway", "text/plain", format!("mmh3 server isn't answering at {upstream}.\n").as_bytes(), false);
    };
    let mut forwarded = String::new();
    for line in head {
        let name = line.split(':').next().unwrap_or_default().trim().to_ascii_lowercase();
        if name == "connection" || name == "keep-alive" {
            continue;
        }
        if line.trim().is_empty() {
            forwarded.push_str("connection: close\r\n");
        }
        forwarded.push_str(line);
    }
    server.write_all(forwarded.as_bytes())?;
    let mut to_server = server.try_clone()?;
    std::thread::Builder::new().name("proxy-body".into()).spawn(move || {
        let buffered = reader.buffer().to_vec();
        let _ = to_server.write_all(&buffered);
        reader.consume(buffered.len());
        let _ = std::io::copy(&mut reader.into_inner(), &mut to_server);
    })?;
    let copied = std::io::copy(&mut server, &mut client);
    let _ = client.shutdown(std::net::Shutdown::Both);
    let _ = server.shutdown(std::net::Shutdown::Both);
    copied.map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Client, ImageRequest, State};
    use crate::mock::{self, Config};

    #[test]
    fn the_page_and_the_api_share_one_origin() {
        let ms = Duration::from_millis;
        let server = mock::spawn("127.0.0.1:0", Config { text: ms(10), step: ms(10), decode: ms(10), ..Config::default() }).unwrap();
        let dir = std::env::temp_dir().join(format!("mmh3-studio-proxy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("index.html"), "<!doctype html>studio").unwrap();
        std::fs::write(dir.join("app_bg.wasm"), b"\0asm").unwrap();
        let addr = spawn("127.0.0.1:0", dir.clone(), server.to_string()).unwrap();

        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        let mut page = agent.get(&format!("http://{addr}/")).call().unwrap();
        assert_eq!(page.body_mut().read_to_string().unwrap(), "<!doctype html>studio");
        let wasm = agent.get(&format!("http://{addr}/app_bg.wasm")).call().unwrap();
        assert_eq!(wasm.headers()["content-type"], "application/wasm");
        assert_eq!(agent.get(&format!("http://{addr}/../secret")).call().unwrap().status(), 404);

        let client = Client::new(format!("http://{addr}"));
        assert!(client.status().is_ok());
        let request = ImageRequest {
            prompt: "a fox".into(),
            seed: 1,
            width: 64,
            height: 64,
            min_generation_area: None,
            references: Vec::new(),
            ..ImageRequest::default()
        };
        let created = client.create_image(&request).unwrap();
        let mut steps = Vec::new();
        client.events(&created.id, |g| steps.push(g.state)).unwrap();
        assert_eq!(steps.last(), Some(&State::Done), "the event stream passes through to its end");
        assert!(client.image(&created.id).unwrap().starts_with(b"\x89PNG"));

        let mut endless = TcpStream::connect(addr).unwrap();
        let line = "GET /v1/status HTTP/1.1\r\nx-filler: ";
        endless.write_all(format!("{line}{}", "a".repeat(MAX_HEAD - line.len())).as_bytes()).unwrap();
        let mut answer = String::new();
        endless.read_to_string(&mut answer).unwrap();
        assert!(answer.starts_with("HTTP/1.1 431 "), "a head that never ends is refused, not passed on: {answer}");

        let gone = Client::new(format!("http://{}", spawn("127.0.0.1:0", dir.clone(), "127.0.0.1:1".into()).unwrap()));
        assert!(gone.status().unwrap_err().is_unreachable(), "a stopped server reads as one");
        std::fs::remove_dir_all(dir).unwrap();
    }
}

use std::path::PathBuf;

use mmh3_studio::proxy;

const USAGE: &str = "web-proxy [--listen ADDR] [--dir DIR] [--server ADDR]

Serves the web build in DIR (target/web by default) on ADDR (127.0.0.1:8000 by default) and
passes /v1/* to mmh3 server at --server (127.0.0.1:8833 by default), so the page and the API
share an origin. For local testing; see the README for tailscale serve.";

fn main() {
    let (mut listen, mut dir, mut server) = ("127.0.0.1:8000".to_owned(), PathBuf::from("target/web"), "127.0.0.1:8833".to_owned());
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || args.next().unwrap_or_else(|| exit(&format!("{arg} needs a value")));
        match arg.as_str() {
            "--listen" => listen = value(),
            "--dir" => dir = PathBuf::from(value()),
            "--server" => server = value(),
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            other => exit(&format!("unknown argument {other}")),
        }
    }
    let server = server.trim_start_matches("http://").trim_end_matches('/').to_owned();
    match proxy::spawn(&listen, dir.clone(), server.clone()) {
        Ok(addr) => println!("serving {} on http://{addr}, passing /v1 to {server}", dir.display()),
        Err(e) => exit(&format!("couldn't listen on {listen}: {e}")),
    }
    loop {
        std::thread::park();
    }
}

fn exit(message: &str) -> ! {
    eprintln!("{message}\n\n{USAGE}");
    std::process::exit(2)
}

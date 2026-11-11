//! The studio's background work: talking to the server, fetching pictures and keeping what was
//! asked for. Every answer lands in the state through the app handle, which commits it on the
//! next frame.
//!
//! Each build has its own `Net` with the same methods, which is all the state calls:
//! natively the blocking [`Client`](crate::api::Client) on Rune's task threads, with pictures
//! cached on disk; on the web the browser's `fetch` and `EventSource` from the page's own
//! origin, with pictures decoded by the browser.

use std::collections::{HashMap, HashSet, VecDeque};
use std::ops::ControlFlow;
use std::time::Duration;

use crate::api::{Error, Generation, ImageRequest, Status};
use crate::state::Probe;

#[cfg(not(target_arch = "wasm32"))]
mod native;
#[cfg(target_arch = "wasm32")]
mod web;

#[cfg(not(target_arch = "wasm32"))]
pub use native::{Net, ask_for_references, reveal, save, server};
#[cfg(target_arch = "wasm32")]
pub use web::{Net, ask_for_references, save, server};

/// What the studio asked a server for, by the server's id.
pub type Requests = HashMap<String, ImageRequest>;

const STATUS_EVERY: Duration = Duration::from_secs(2);
const WATCH_RETRY: Duration = Duration::from_millis(500);
const WATCH_RETRY_MAX: Duration = Duration::from_secs(5);

/// The pictures still to fetch: what the user waits for first, then the gallery's backlog.
/// `wanted` holds every id queued or being fetched, so a forgotten one is never written.
#[derive(Default)]
struct Queue {
    urgent: VecDeque<String>,
    backlog: VecDeque<String>,
    wanted: HashSet<String>,
}

impl Queue {
    fn next(&mut self) -> Option<String> {
        self.urgent.pop_front().or_else(|| self.backlog.pop_front())
    }

    fn unqueue(&mut self, id: &str) -> bool {
        let before = self.urgent.len() + self.backlog.len();
        self.urgent.retain(|q| q != id);
        self.backlog.retain(|q| q != id);
        before != self.urgent.len() + self.backlog.len()
    }

    /// Queues `id`, or moves it to the front when `urgent`; true when the fetcher has new work.
    fn want(&mut self, id: &str, urgent: bool) -> bool {
        if self.wanted.contains(id) {
            if urgent && self.unqueue(id) {
                self.urgent.push_front(id.to_owned());
            }
            return false;
        }
        self.wanted.insert(id.to_owned());
        if urgent {
            self.urgent.push_front(id.to_owned());
        } else {
            self.backlog.push_back(id.to_owned());
        }
        true
    }

    fn forget(&mut self, id: &str) {
        self.unqueue(id);
        self.wanted.remove(id);
    }
}

/// Whether a status poll should refresh the list: when the number of generations changes, or
/// whether the server answers does.
fn refresh_list(last: Option<&Result<Status, Error>>, now: &Result<Status, Error>) -> bool {
    match (last, now) {
        (Some(Ok(a)), Ok(b)) => a.generations != b.generations,
        (_, Err(e)) if e.is_unreachable() => false,
        (Some(was), _) => was != now,
        (None, _) => true,
    }
}

/// What to do once a generation's event stream has closed, from what the server says of it now.
enum AfterStream {
    /// It finished: land this and stop.
    Finished(Generation),
    /// Still going: land this and follow it again.
    Going(Generation),
    /// The server no longer has it.
    Gone,
    /// The server can't be reached: try again.
    Retry,
    /// The server answered in a way the studio can't follow.
    Lost(Error),
}

impl AfterStream {
    fn from(answer: Result<Generation, Error>) -> AfterStream {
        match answer {
            Ok(g) if g.is_finished() => AfterStream::Finished(g),
            Ok(g) => AfterStream::Going(g),
            Err(Error::Refused { status: 404, .. }) => AfterStream::Gone,
            Err(e) if e.is_unreachable() => AfterStream::Retry,
            Err(e) => AfterStream::Lost(e),
        }
    }
}

/// Whether generation `id` writes a picture, from how the server answers for its picture: a
/// picture still to come or never to come is refused with 409, a video with 404. Continue means
/// it writes a picture, and the generation should be asked for.
fn picture_kind<T>(answer: &Result<T, Error>) -> ControlFlow<Probe> {
    match answer {
        Ok(_) | Err(Error::Refused { status: 409, .. }) => ControlFlow::Continue(()),
        Err(Error::Refused { status: 404, .. }) => ControlFlow::Break(Probe::NotAPicture),
        Err(_) => ControlFlow::Break(Probe::Unanswered),
    }
}

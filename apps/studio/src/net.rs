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

use crate::api::{Error, ImageRequest, Status};
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

/// How often the status is read, and the list while no event stream says what changed.
const STATUS_EVERY: Duration = Duration::from_secs(2);
const LIST_EVERY: u32 = 15;
const FEED_RETRY: Duration = Duration::from_millis(500);
const FEED_RETRY_MAX: Duration = Duration::from_secs(5);

/// How the studio hears what the server's generations do.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Feed {
    /// Not known yet, or the event stream is being opened again.
    #[default]
    Opening,
    /// `GET /v1/events` is open: every change arrives as it happens, with previews.
    Streaming,
    /// The server has no `GET /v1/events`, an upstream one: the list is read every
    /// [`STATUS_EVERY`] instead, and there are no previews and no cancelling.
    Polling,
}

/// Whether a status poll should read the list. While no event stream is open, every poll does.
/// While one is, the stream says every change but a delete, so the list is read only when what
/// the server is working through changes, when whether it answers does, or when it has gone
/// [`LIST_EVERY`] polls unread.
fn read_list(feed: Feed, last: Option<&Result<Status, Error>>, now: &Result<Status, Error>, polls_since_list: u32) -> bool {
    if feed != Feed::Streaming {
        return !matches!(now, Err(e) if e.is_unreachable());
    }
    let work = |s: &Status| (s.generations, s.generating.clone(), s.queued);
    match (last, now) {
        (Some(Ok(a)), Ok(b)) => work(a) != work(b) || polls_since_list >= LIST_EVERY,
        (_, Err(e)) if e.is_unreachable() => false,
        (Some(was), _) => was != now,
        (None, _) => true,
    }
}

/// Whether the server has come back from not answering, when an upstream one may have been
/// replaced by the fork and the event stream is worth asking for again.
fn came_back(last: Option<&Result<Status, Error>>, now: &Result<Status, Error>) -> bool {
    let down = |s: &Result<Status, Error>| matches!(s, Err(e) if e.is_unreachable());
    last.is_some_and(down) && !down(now)
}

/// The pictures still to fetch: what the user waits for first, then the gallery's backlog.
/// `wanted` holds every id queued or being fetched, so a forgotten one is never written.
#[derive(Default)]
struct Queue {
    urgent: VecDeque<String>,
    backlog: VecDeque<String>,
    wanted: HashSet<String>,
}

impl Queue {
    /// The next picture to fetch, and whether the user waits for it.
    fn next(&mut self) -> Option<(String, bool)> {
        self.urgent.pop_front().map(|id| (id, true)).or_else(|| self.backlog.pop_front().map(|id| (id, false)))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn status(generations: u32, generating: Option<&str>, queued: u32) -> Status {
        Status { generations, generating: generating.map(str::to_owned), queued, ..Status::default() }
    }

    #[test]
    fn beside_a_stream_the_list_is_read_when_the_work_changes_even_if_the_count_does_not() {
        let refresh = |last: &Result<Status, Error>, now: Result<Status, Error>, polls| read_list(Feed::Streaming, Some(last), &now, polls);
        let idle = Ok(status(3, None, 0));
        assert!(!refresh(&idle, Ok(status(3, None, 0)), 1));
        assert!(refresh(&idle, Ok(status(3, Some("g4"), 0)), 1), "a delete and a create elsewhere");
        assert!(refresh(&idle, Ok(status(3, None, 1)), 1));
        assert!(refresh(&idle, Ok(status(4, None, 0)), 1));
        assert!(refresh(&idle, Ok(status(3, None, 0)), LIST_EVERY), "a quiet list is read now and then");
        let mut busy = status(3, Some("g4"), 0);
        busy.memory = Some(crate::api::Memory { free: 1, total: 2 });
        assert!(!refresh(&Ok(status(3, Some("g4"), 0)), Ok(busy), 1), "memory moving is no reason");
        assert!(read_list(Feed::Streaming, None, &idle, 0));
    }

    #[test]
    fn without_a_stream_every_poll_reads_the_list_while_the_server_answers() {
        let idle = Ok(status(3, None, 0));
        for feed in [Feed::Opening, Feed::Polling] {
            assert!(read_list(feed, Some(&idle), &Ok(status(3, None, 0)), 1));
            assert!(!read_list(feed, Some(&idle), &Err(Error::Unreachable("refused".into())), 1));
        }
    }

    #[test]
    fn the_stream_is_asked_for_again_once_the_server_comes_back() {
        let down: Result<Status, Error> = Err(Error::Unreachable("refused".into()));
        let up = Ok(status(0, None, 0));
        assert!(came_back(Some(&down), &up));
        assert!(!came_back(Some(&up), &up));
        assert!(!came_back(None, &up));
        assert!(!came_back(Some(&up), &down));
    }
}

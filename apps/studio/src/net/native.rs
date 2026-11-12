//! The native build's background work: every request runs on its own thread, and pictures are
//! cached on disk.

use std::collections::HashSet;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Sender, channel};
use std::sync::{Arc, Mutex, MutexGuard};

use rune::{AppHandle, ImageCache, Tx};

use super::{FEED_RETRY, FEED_RETRY_MAX, Feed, Queue, Requests, STATUS_EVERY, came_back, picture_kind, read_list};
use crate::api::{self, Client, Error, Event, ImageRequest, Status};
use crate::state::{Probe, REFERENCE_EXTENSIONS, State};
use crate::thumb::{self, thumbnail};

struct Pictures {
    dir: PathBuf,
    queue: Mutex<Queue>,
}

impl Pictures {
    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn file(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.png"))
    }

    /// Drops `ids` from the queue and their cached files, under the lock a fetch writes under.
    fn forget<'a>(&self, ids: impl IntoIterator<Item = &'a str>) {
        let mut q = self.lock();
        for id in ids {
            q.forget(id);
            let _ = std::fs::remove_file(self.file(id));
        }
    }

    /// Removes cached files of generations the server no longer lists.
    fn prune(&self, listed: &HashSet<String>) {
        let Ok(entries) = std::fs::read_dir(&self.dir) else { return };
        let q = self.lock();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(id) = name.to_str().and_then(|n| n.strip_suffix(".png").or_else(|| n.strip_suffix(".png.part"))) else { continue };
            if !listed.contains(id) && !q.wanted.contains(id) {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }

    /// Keeps a PNG the server sent, unless the generation was forgotten meanwhile.
    fn store(&self, id: &str, png: &[u8]) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let part = self.dir.join(format!("{id}.png.part"));
        std::fs::write(&part, png)?;
        let _q = self.lock();
        std::fs::rename(&part, self.file(id))
    }

    /// The generation's PNG, from the cache folder or the server, and its thumbnail; `None`
    /// when the generation was forgotten while it was being fetched.
    fn fetch(&self, client: &Client, id: &str) -> Result<Option<(PathBuf, image::RgbaImage)>, String> {
        let file = self.file(id);
        if let Ok(png) = std::fs::read(&file) {
            match thumbnail(&png) {
                Ok(thumb) => return Ok(Some((file, thumb))),
                Err(_) => {
                    let _ = std::fs::remove_file(&file);
                }
            }
        }
        let png = client.image(id).map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&self.dir).map_err(|e| e.to_string())?;
        let part = self.dir.join(format!("{id}.png.part"));
        std::fs::write(&part, &png).map_err(|e| e.to_string())?;
        {
            let q = self.lock();
            if !q.wanted.contains(id) {
                let _ = std::fs::remove_file(&part);
                return Ok(None);
            }
            std::fs::rename(&part, &file).map_err(|e| e.to_string())?;
        }
        Ok(Some((file, thumbnail(&png)?)))
    }
}

pub struct Net {
    handle: AppHandle<State>,
    client: Client,
    pictures: Arc<Pictures>,
    wake: Sender<()>,
    /// Where what the studio asked this server for is kept between runs.
    requests_file: PathBuf,
    /// Generated pictures attached to edit, kept apart from the cache, which drops a picture
    /// with its generation.
    kept_dir: PathBuf,
    remember: Sender<Requests>,
    probes: Sender<String>,
    images: Arc<ImageCache>,
}

impl Net {
    pub fn new(handle: AppHandle<State>, server: &str, images: Arc<ImageCache>) -> Net {
        let client = Client::new(server);
        let dirs = rune::dirs::AppDirs::new("dev.mmh3.studio");
        let (cache, data) = match &dirs {
            Some(d) => (d.cache().to_owned(), d.data().to_owned()),
            None => (std::env::temp_dir().join("mmh3-studio"), std::env::temp_dir().join("mmh3-studio")),
        };
        let cache = cache.join("images");
        let folder = folder_for(client.base());
        let pictures = Arc::new(Pictures { dir: cache.join(&folder), queue: Mutex::default() });
        let requests_file = data.join("requests").join(format!("{folder}.json"));
        let kept_dir = data.join("references").join(&folder);
        let (dir, file) = (kept_dir.clone(), requests_file.clone());
        let _ = rune::task::spawn("mmh3-kept", move || prune_kept(&dir, &read_requests(&file)));
        let (remember, kept) = channel::<Requests>();
        let file = requests_file.clone();
        let _ = rune::task::serve("mmh3-requests", kept, move |requests| {
            let _ = write_requests(&file, &requests);
            ControlFlow::Continue(())
        });
        let (wake, rx) = channel::<()>();
        let (h, c, p, images_for_pictures) = (handle.clone(), client.clone(), pictures.clone(), images.clone());
        let _ = rune::task::serve("mmh3-pictures", rx, move |()| {
            let Some((id, _)) = p.lock().next() else { return ControlFlow::Continue(()) };
            let answer = p.fetch(&c, &id);
            p.lock().wanted.remove(&id);
            let answer = match answer {
                Ok(None) => return ControlFlow::Continue(()),
                Ok(Some((file, thumb))) => Ok((file, images_for_pictures.rgba(thumb.width(), thumb.height(), thumb.into_raw()))),
                Err(why) => Err(why),
            };
            h.update(move |s, _| match answer {
                Ok((file, thumb)) => s.picture_ready(&id, file, thumb),
                Err(why) => s.picture_failed(&id, why),
            });
            ControlFlow::Continue(())
        });
        let (probes, asked) = channel::<String>();
        let (h, c, p) = (handle.clone(), client.clone(), pictures.clone());
        let _ = rune::task::serve("mmh3-probe", asked, move |id| {
            let answer = probe(&c, &p, &id);
            h.update(move |s, tx| {
                let now = tx.clock();
                s.probed(&id, answer, now);
            });
            ControlFlow::Continue(())
        });
        Net { handle, client, pictures, wake, requests_file, kept_dir, remember, probes, images }
    }

    /// What the studio asked this server for in earlier runs, by the server's id.
    pub fn requests(&self) -> Requests {
        read_requests(&self.requests_file)
    }

    /// Decodes a small copy of a picture the composer will send, off the UI thread.
    pub fn reference_thumb(&self, path: PathBuf) {
        let (h, images) = (self.handle.clone(), self.images.clone());
        let _ = rune::task::spawn("mmh3-reference", move || {
            let thumb = std::fs::read(&path).map_err(|e| e.to_string()).and_then(|bytes| thumb::reference(&bytes));
            let thumb = thumb.map(|t| images.rgba(t.width(), t.height(), t.into_raw()));
            h.update(move |s, _| s.reference_thumb(&path, thumb));
        });
    }

    /// Copies generation `id`'s picture out of the cache and attaches the copy.
    pub fn keep_for_editing(&self, id: &str, file: PathBuf) {
        let (h, kept) = (self.handle.clone(), self.kept_dir.join(format!("{id}.png")));
        let _ = rune::task::spawn("mmh3-keep", move || {
            let answer = kept.parent().map_or(Ok(()), std::fs::create_dir_all).and_then(|()| std::fs::copy(&file, &kept));
            h.update(move |s, _| match answer {
                Ok(_) => s.add_references(&[kept]),
                Err(e) => s.note("Couldn't attach the picture.", Some(e.to_string()), true),
            });
        });
    }

    /// What the preview of generation `id` decodes: its file in the cache.
    #[expect(clippy::unused_self, reason = "the web Net hands over a picture it holds")]
    pub fn preview_source(&self, _id: &str, file: &Path) -> PathBuf {
        file.to_owned()
    }

    /// Asks the server whether a generation the studio did not ask for writes a picture.
    pub fn probe(&self, id: String) {
        let _ = self.probes.send(id);
    }

    /// Keeps `requests` for the next run, written in the order they were handed over.
    pub fn remember(&self, requests: Requests) {
        let _ = self.remember.send(requests);
    }

    /// Follows the server: its status every two seconds, and its generations through
    /// `GET /v1/events`, or through the list read with each status from a server without it.
    pub fn start(&self) {
        let link = Arc::new(Link::default());
        let (h, c, p, l) = (self.handle.clone(), self.client.clone(), self.pictures.clone(), link.clone());
        let _ = rune::task::spawn("mmh3-status", move || {
            let mut last: Option<Result<Status, Error>> = None;
            let mut polls_since_list = 0;
            loop {
                let now = c.status();
                if came_back(last.as_ref(), &now) {
                    l.reprobe.store(true, Ordering::Relaxed);
                }
                let wanted = read_list(l.feed(), last.as_ref(), &now, polls_since_list);
                polls_since_list += 1;
                if last.as_ref() != Some(&now) {
                    let status = now.clone();
                    h.update(move |s, tx| {
                        tx.instant();
                        s.set_status(status);
                    });
                }
                if wanted && let Ok(list) = c.list() {
                    polls_since_list = 0;
                    land(&h, &p, Event::Snapshot(list));
                }
                last = Some(now);
                rune::task::sleep(STATUS_EVERY);
            }
        });
        let (h, c, p) = (self.handle.clone(), self.client.clone(), self.pictures.clone());
        let _ = rune::task::spawn("mmh3-events", move || follow(&h, &c, &p, &link));
    }

    pub fn submit(&self, local: String, request: ImageRequest) {
        let (h, c) = (self.handle.clone(), self.client.clone());
        let _ = rune::task::spawn("mmh3-submit", move || {
            let answer = c.create_image(&request);
            h.update(move |s, tx| {
                let now = tx.clock();
                s.submitted(&local, answer, now);
            });
        });
    }

    /// Asks the server to stop generation `id`.
    pub fn cancel(&self, id: String) {
        let (h, c) = (self.handle.clone(), self.client.clone());
        let _ = rune::task::spawn("mmh3-cancel", move || {
            let answer = c.cancel(&id);
            h.update(move |s, tx| {
                let now = tx.clock();
                s.cancelled(&id, answer, now);
            });
        });
    }

    /// Fetches and decodes the preview generation `id` wrote after step `step`.
    pub fn fetch_preview(&self, id: String, step: u32) {
        let (h, c, images) = (self.handle.clone(), self.client.clone(), self.images.clone());
        let _ = rune::task::spawn("mmh3-preview", move || {
            let decoded = c.preview(&id, step).map_err(|e| e.to_string()).and_then(|png| image::load_from_memory(&png).map_err(|e| e.to_string()));
            let answer = decoded.map(|img| {
                let rgba = img.into_rgba8();
                images.rgba(rgba.width(), rgba.height(), rgba.into_raw())
            });
            h.update(move |s, _| s.preview_ready(&id, step, answer));
        });
    }

    /// Queues the generation's picture, or moves it to the front when `urgent`.
    pub fn fetch_image(&self, id: &str, urgent: bool) {
        let fresh = self.pictures.lock().want(id, urgent);
        if fresh {
            let _ = self.wake.send(());
        }
    }

    pub fn forget(&self, ids: &[String]) {
        self.pictures.forget(ids.iter().map(String::as_str));
    }

    pub fn delete(&self, id: String) {
        let (h, c, p) = (self.handle.clone(), self.client.clone(), self.pictures.clone());
        let _ = rune::task::spawn("mmh3-delete", move || {
            let answer = c.delete(&id);
            if answer.is_ok() {
                p.forget([id.as_str()]);
            }
            h.update(move |s, _| match answer {
                Ok(()) => s.deleted(&id),
                Err(e) => s.note("Couldn't delete the generation.", Some(e.to_string()), true),
            });
        });
    }

    fn copy(&self, from: PathBuf, to: PathBuf) {
        let h = self.handle.clone();
        let _ = rune::task::spawn("mmh3-save", move || {
            let answer = std::fs::copy(&from, &to);
            h.update(move |s, _| match answer {
                Ok(_) => s.note("Image saved", Some(to.display().to_string()), false),
                Err(e) => s.note("Couldn't save the image.", Some(e.to_string()), true),
            });
        });
    }
}

/// What the status poll and the event stream share.
#[derive(Default)]
struct Link {
    feed: Mutex<Feed>,
    /// The server answers again after it didn't, so a server without the event stream may have
    /// been replaced by one with it.
    reprobe: AtomicBool,
}

impl Link {
    fn feed(&self) -> Feed {
        *self.feed.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn set(&self, feed: Feed, handle: &AppHandle<State>) {
        let was = std::mem::replace(&mut *self.feed.lock().unwrap_or_else(std::sync::PoisonError::into_inner), feed);
        if was != feed {
            handle.update(move |s, _| s.feed = feed);
        }
    }
}

/// Keeps `GET /v1/events` open, opening it again when it drops. A server that has no such route
/// is asked again only once it has stopped answering and come back.
fn follow(h: &AppHandle<State>, c: &Client, p: &Pictures, link: &Link) {
    let mut wait = FEED_RETRY;
    loop {
        let mut opened = false;
        let answer = c.feed(|event| {
            if !opened {
                opened = true;
                link.set(Feed::Streaming, h);
            }
            land(h, p, event);
        });
        if opened {
            wait = FEED_RETRY;
        }
        if let Err(e) = &answer
            && api::is_missing_route(e)
        {
            link.set(Feed::Polling, h);
            while !link.reprobe.swap(false, Ordering::Relaxed) {
                rune::task::sleep(STATUS_EVERY);
            }
            continue;
        }
        link.set(Feed::Opening, h);
        rune::task::sleep(wait);
        wait = (wait * 2).min(FEED_RETRY_MAX);
    }
}

/// Lands what the server said of its generations in the state.
fn land(h: &AppHandle<State>, p: &Pictures, event: Event) {
    if let Event::Snapshot(list) = &event {
        p.prune(&list.iter().map(|g| g.id.clone()).collect());
    }
    h.update(move |s, tx| {
        tx.instant();
        let now = tx.clock();
        match event {
            Event::Snapshot(list) => s.merge(&list, now),
            Event::Generation(g) => s.upsert(&g, now),
            Event::Preview(p) => s.preview_available(&p.id, p.step),
            Event::Removed(id) => s.removed(&id),
        }
    });
}

/// Whether generation `id` writes a picture, from how the server answers for its picture: a
/// picture still to come or never to come is refused with 409, a video with 404. A picture
/// already written is kept, since it was sent anyway.
fn probe(client: &Client, pictures: &Pictures, id: &str) -> Probe {
    let answer = client.image(id);
    if let ControlFlow::Break(probe) = picture_kind(&answer) {
        return probe;
    }
    if let Ok(png) = answer {
        let _ = pictures.store(id, &png);
    }
    client.generation(id).map_or(Probe::Unanswered, |g| Probe::Picture(Box::new(g)))
}

/// An unreadable file is as good as none: it only costs the prompts of old generations.
fn read_requests(file: &Path) -> Requests {
    std::fs::read(file).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default()
}

fn write_requests(file: &Path, requests: &Requests) -> std::io::Result<()> {
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let part = file.with_extension("json.part");
    std::fs::write(&part, serde_json::to_vec_pretty(requests).map_err(std::io::Error::other)?)?;
    std::fs::rename(part, file)
}

/// Removes kept pictures no remembered request edits any more. It runs as the studio starts,
/// when the composer holds nothing.
fn prune_kept(dir: &Path, requests: &Requests) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    let used: HashSet<&Path> = requests.values().flat_map(|r| r.references.iter().map(PathBuf::as_path)).collect();
    for entry in entries.flatten() {
        if !used.contains(entry.path().as_path()) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// One cache folder a server, since ids are only unique on the server that made them.
fn folder_for(base: &str) -> String {
    let host = base.split("://").nth(1).unwrap_or(base);
    host.chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' { c } else { '-' }).collect()
}

/// The server named by `--server`, then `MMH3_SERVER`, then the default address.
pub fn server() -> String {
    let args = rune::extra_args();
    let flag = args.iter().position(|a| a == "--server").and_then(|i| args.get(i + 1)).cloned();
    flag.or_else(|| std::env::var("MMH3_SERVER").ok()).unwrap_or_else(|| api::DEFAULT_SERVER.to_owned())
}

/// Asks for a picture to edit with the system's Open dialog.
pub fn ask_for_references(_: &State, tx: &mut Tx) {
    let options = rune::FileDialogOptions { title: "Add a picture to edit".into(), ..Default::default() }.filter("PNG or JPEG picture", &REFERENCE_EXTENSIONS);
    tx.open_file(options, |s: &mut State, answer, _tx| match answer {
        Ok(Some(path)) => s.add_references(&[path]),
        Ok(None) => {}
        Err(e) => s.note("Couldn't open the file dialog.", Some(format!("{e:?}")), true),
    });
}

/// Asks where to save the PNG at `file` with the system's Save dialog, then copies it there.
pub fn save(_: &State, file: &Path, name: &str, tx: &mut Tx) {
    let file = file.to_owned();
    let options = rune::FileDialogOptions { title: "Save image".into(), file_name: Some(name.to_owned()), ..Default::default() }.filter("PNG image", &["png"]);
    tx.save_file(options, move |s: &mut State, answer, _tx| match answer {
        Ok(Some(to)) => match &s.net {
            Some(net) => net.copy(file.clone(), to),
            None => s.note("Saving needs the server connection.", None, true),
        },
        Ok(None) => {}
        Err(e) => s.note("Couldn't open the save dialog.", Some(format!("{e:?}")), true),
    });
}

pub fn reveal(file: PathBuf) {
    let _ = rune::task::spawn("mmh3-reveal", move || {
        let _ = std::process::Command::new("/usr/bin/open").arg("-R").arg(&file).status();
    });
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn requests_survive_a_restart_and_a_broken_file_reads_as_none() {
        let file = std::env::temp_dir().join(format!("mmh3-studio-requests-{}/127.0.0.1-8833.json", std::process::id()));
        let request = ImageRequest {
            prompt: "<Picture 1> at night".into(),
            seed: 3,
            width: 4096,
            height: 4096,
            min_generation_area: None,
            references: vec![PathBuf::from("/tmp/street.jpg")],
        };
        let requests: Requests = [("6912f3a40003".to_owned(), request)].into();
        write_requests(&file, &requests).unwrap();
        assert_eq!(read_requests(&file), requests);
        std::fs::write(&file, b"{not json").unwrap();
        assert!(read_requests(&file).is_empty());
        std::fs::remove_dir_all(file.parent().unwrap()).unwrap();
    }

    #[test]
    fn kept_pictures_go_once_no_remembered_request_edits_them() {
        let dir = std::env::temp_dir().join(format!("mmh3-studio-kept-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (used, unused) = (dir.join("a.png"), dir.join("b.png"));
        std::fs::write(&used, b"png").unwrap();
        std::fs::write(&unused, b"png").unwrap();
        let request =
            ImageRequest { prompt: "<Picture 1> at night".into(), seed: 3, width: 64, height: 64, min_generation_area: None, references: vec![used.clone()] };
        prune_kept(&dir, &[("g1".to_owned(), request)].into());
        assert!(used.is_file());
        assert!(!unused.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_probe_tells_pictures_from_videos_and_keeps_a_finished_png() {
        use crate::api::{Generation, Part, multipart};
        use crate::mock::{self, Config};

        let ms = Duration::from_millis;
        let addr = mock::spawn("127.0.0.1:0", Config { text: ms(10), step: ms(10), decode: ms(10), upstream: true }).unwrap();
        let client = Client::new(format!("http://{addr}"));
        let dir = std::env::temp_dir().join(format!("mmh3-studio-probe-{}", std::process::id()));
        let pictures = Pictures { dir: dir.clone(), queue: Mutex::default() };
        let request = ImageRequest { prompt: "a fox".into(), seed: 1, width: 64, height: 64, min_generation_area: None, references: Vec::new() };

        let done = client.create_image(&request).unwrap();
        client.events(&done.id, |_| {}).unwrap();
        let pending = client.create_image(&request).unwrap();
        let (content_type, body) = multipart(&[Part::Text { name: "prompt", value: "a fox".into() }], "B");
        let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
        let mut answer = agent.post(&format!("{}/v1/generations", client.base())).header("Content-Type", &content_type).send(&body[..]).unwrap();
        let video: Generation = serde_json::from_str(&answer.body_mut().read_to_string().unwrap()).unwrap();

        assert!(matches!(probe(&client, &pictures, &done.id), Probe::Picture(g) if g.state == api::State::Done));
        assert!(pictures.file(&done.id).is_file(), "the PNG it was sent is kept");
        assert!(matches!(probe(&client, &pictures, &pending.id), Probe::Picture(_)));
        assert_eq!(probe(&client, &pictures, &video.id), Probe::NotAPicture);
        assert_eq!(probe(&Client::new("http://127.0.0.1:1"), &pictures, "x"), Probe::Unanswered);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

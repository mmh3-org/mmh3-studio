//! The web build's background work. The page is served from the same origin as the server's
//! `/v1` routes, so every request is a same-origin `fetch` relative to the page, a generation is
//! followed with `EventSource`, and the browser decodes pictures off the main thread. Work runs
//! as futures on the page's one thread.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use js_sys::{Function, Promise, Uint8Array};
use rune::{AppHandle, ImageCache, Tx};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::{JsFuture, spawn_local};
use web_sys::{
    AbortSignal, Blob, DragEvent, EventSource, FileList, FormData, HtmlAnchorElement, HtmlInputElement, ImageBitmap, ImageBitmapOptions, ImageOrientation,
    MessageEvent, OffscreenCanvas, OffscreenCanvasRenderingContext2d, RequestInit, ResizeQuality, Response,
};

use super::{AfterStream, Queue, Requests, STATUS_EVERY, WATCH_RETRY, WATCH_RETRY_MAX, picture_kind, refresh_list};
use crate::api::{self, Error, Generation, Generations, ImageRequest, Status};
use crate::state::{Probe, REFERENCE_EXTENSIONS, State};
use crate::thumb;

/// Where the page keeps what the studio asked for; the browser keeps it per origin, which is
/// per server here.
const REQUESTS_KEY: &str = "mmh3-studio.requests";
/// How long a quick request may take before the server counts as unreachable.
const QUICK: Duration = Duration::from_secs(5);
/// The folder every picked or dropped picture is kept under, by the order it arrived in.
const PICKED: &str = "picked";

pub struct Net {
    inner: Rc<Inner>,
}

struct Inner {
    handle: AppHandle<State>,
    images: Arc<ImageCache>,
    queue: RefCell<Queue>,
    fetching: Cell<bool>,
    /// Pictures picked or dropped in this page, by the name the state knows them by. They live
    /// only as long as the page: a reload forgets them.
    files: RefCell<HashMap<PathBuf, Blob>>,
    picked: Cell<u64>,
    dropping: Cell<bool>,
}

impl Net {
    pub fn new(handle: AppHandle<State>, _server: &str, images: Arc<ImageCache>) -> Net {
        let inner = Rc::new(Inner {
            handle,
            images,
            queue: RefCell::default(),
            fetching: Cell::new(false),
            files: RefCell::default(),
            picked: Cell::new(0),
            dropping: Cell::new(false),
        });
        listen_for_drops(&inner);
        Net { inner }
    }

    /// What the studio asked this server for in earlier visits, by the server's id.
    #[expect(clippy::unused_self, reason = "the native Net reads them from its own server's file")]
    pub fn requests(&self) -> Requests {
        storage().and_then(|s| s.get_item(REQUESTS_KEY).ok().flatten()).and_then(|json| serde_json::from_str(&json).ok()).unwrap_or_default()
    }

    #[expect(clippy::unused_self, clippy::needless_pass_by_value, reason = "the native Net hands them to a writer thread")]
    pub fn remember(&self, requests: Requests) {
        if let (Some(storage), Ok(json)) = (storage(), serde_json::to_string(&requests)) {
            let _ = storage.set_item(REQUESTS_KEY, &json);
        }
    }

    /// Decodes a small copy of a picture the composer will send.
    pub fn reference_thumb(&self, path: PathBuf) {
        let inner = self.inner.clone();
        spawn_local(async move {
            let thumb = match inner.blob(&path).await {
                Ok(blob) => reference(&blob).await.map(|(w, h, rgba)| inner.images.rgba(w, h, rgba)),
                Err(e) => Err(e.to_string()),
            };
            inner.handle.update(move |s, _| s.reference_thumb(&path, thumb));
        });
    }

    /// Asks the server whether a generation the studio did not ask for writes a picture.
    pub fn probe(&self, id: String) {
        let inner = self.inner.clone();
        spawn_local(async move {
            let answer = probe(&inner, &id).await;
            inner.handle.update(move |s, tx| {
                let now = tx.clock();
                s.probed(&id, answer, now);
            });
        });
    }

    /// Polls the server's status every two seconds, as the native build does.
    pub fn start(&self) {
        let inner = self.inner.clone();
        spawn_local(async move {
            let mut last: Option<Result<Status, Error>> = None;
            loop {
                let now = inner.json::<Status>("v1/status").await;
                let refresh = refresh_list(last.as_ref(), &now);
                if last.as_ref() != Some(&now) {
                    let status = now.clone();
                    inner.handle.update(move |s, tx| {
                        tx.instant();
                        s.set_status(status);
                    });
                }
                if refresh && let Ok(list) = inner.json::<Generations>("v1/generations").await {
                    inner.handle.update(move |s, tx| {
                        tx.instant();
                        let now = tx.clock();
                        s.merge(&list.generations, now);
                    });
                }
                last = Some(now);
                sleep(STATUS_EVERY).await;
            }
        });
    }

    pub fn submit(&self, local: String, request: ImageRequest) {
        let inner = self.inner.clone();
        spawn_local(async move {
            let answer = inner.create_image(&request).await;
            inner.handle.update(move |s, tx| {
                let now = tx.clock();
                s.submitted(&local, answer, now);
            });
        });
    }

    /// Follows a generation until it finishes, as the native build does: a stream that ends
    /// early is picked up again and an unreachable server retried.
    pub fn watch(&self, id: String) {
        let inner = self.inner.clone();
        spawn_local(async move {
            let handle = inner.handle.clone();
            let land = move |g: Generation| {
                handle.update(move |s, tx| {
                    let now = tx.clock();
                    s.apply(&g, now);
                });
            };
            let mut wait = WATCH_RETRY;
            let lost = loop {
                if events(&url(&format!("v1/generations/{id}/events")), land.clone()).await {
                    wait = WATCH_RETRY;
                }
                match AfterStream::from(inner.json::<Generation>(&format!("v1/generations/{id}")).await) {
                    AfterStream::Finished(g) => {
                        land(g);
                        break None;
                    }
                    AfterStream::Going(g) => land(g),
                    AfterStream::Gone => {
                        inner.queue.borrow_mut().forget(&id);
                        let gone = id.clone();
                        inner.handle.update(move |s, _| s.deleted(&gone));
                        break None;
                    }
                    AfterStream::Retry => {}
                    AfterStream::Lost(e) => break Some(e),
                }
                sleep(wait).await;
                wait = (wait * 2).min(WATCH_RETRY_MAX);
            };
            inner.handle.update(move |s, _| {
                s.watch_ended(&id);
                if let Some(e) = lost {
                    s.note("Lost track of a generation.", Some(e.to_string()), true);
                }
            });
        });
    }

    /// Queues the generation's picture, or moves it to the front when `urgent`.
    pub fn fetch_image(&self, id: &str, urgent: bool) {
        if self.inner.queue.borrow_mut().want(id, urgent) {
            fetch_pictures(&self.inner);
        }
    }

    pub fn forget(&self, ids: &[String]) {
        let mut q = self.inner.queue.borrow_mut();
        for id in ids {
            q.forget(id);
        }
    }

    pub fn delete(&self, id: String) {
        let inner = self.inner.clone();
        spawn_local(async move {
            let answer = inner.fetch("DELETE", &format!("v1/generations/{id}"), None, None).await.map(drop);
            if answer.is_ok() {
                inner.queue.borrow_mut().forget(&id);
            }
            inner.handle.update(move |s, _| match answer {
                Ok(()) => s.deleted(&id),
                Err(e) => s.note("Couldn't delete the generation.", Some(e.to_string()), true),
            });
        });
    }

    /// Opens the browser's file chooser for pictures to edit. The chooser needs the press that
    /// asked for it, so this runs inside the press handler.
    fn pick(&self) {
        let Some(document) = web_sys::window().and_then(|w| w.document()) else { return };
        let Ok(input) = document.create_element("input") else { return };
        let input: HtmlInputElement = input.unchecked_into();
        input.set_type("file");
        input.set_multiple(true);
        input.set_accept(".png,.jpg,.jpeg,image/png,image/jpeg");
        let (inner, chosen) = (self.inner.clone(), input.clone());
        let on_change = Closure::once_into_js(move || {
            if let Some(files) = chosen.files() {
                add_files(&inner, &files);
            }
        });
        input.set_onchange(Some(on_change.unchecked_ref()));
        input.click();
    }
}

fn url(path: &str) -> String {
    format!("{}{path}", server())
}

impl Inner {
    /// A request to the server: refused statuses carry the server's own words, and a request
    /// that never got an answer (or a quick one that took too long) counts as unreachable.
    async fn fetch(&self, method: &str, path: &str, body: Option<&JsValue>, timeout: Option<Duration>) -> Result<Response, Error> {
        let window = web_sys::window().ok_or_else(|| Error::Unreachable("no window".into()))?;
        let init = RequestInit::new();
        init.set_method(method);
        if let Some(body) = body {
            init.set_body(body);
        }
        if let Some(t) = timeout {
            init.set_signal(Some(&AbortSignal::timeout_with_u32(t.as_millis() as u32)));
        }
        let response = JsFuture::from(window.fetch_with_str_and_init(&url(path), &init)).await.map_err(|e| Error::Unreachable(js_error(&e)))?;
        let response: Response = response.unchecked_into();
        let status = response.status();
        if (200..300).contains(&status) {
            return Ok(response);
        }
        let body = text(&response).await.unwrap_or_default();
        Err(api::refusal(status, &body))
    }

    async fn json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T, Error> {
        let response = self.fetch("GET", path, None, Some(QUICK)).await?;
        let body = text(&response).await?;
        serde_json::from_str(&body).map_err(|e| Error::Malformed(e.to_string()))
    }

    /// The picture at `path`: one picked or dropped in this page, or a picture on the server.
    async fn blob(&self, path: &Path) -> Result<Blob, Error> {
        if let Some(blob) = self.files.borrow().get(path) {
            return Ok(blob.clone());
        }
        let name = path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().into_owned());
        if path.starts_with(PICKED) {
            return Err(Error::Unreadable(format!("{name} was attached before the page was reloaded; attach it again")));
        }
        let response = self.fetch("GET", &path.to_string_lossy(), None, None).await?;
        let blob = response.blob().map_err(|e| Error::Unreachable(js_error(&e)))?;
        Ok(JsFuture::from(blob).await.map_err(|e| Error::Unreachable(js_error(&e)))?.unchecked_into())
    }

    async fn create_image(&self, request: &ImageRequest) -> Result<Generation, Error> {
        let form = FormData::new().map_err(|e| Error::Unreadable(js_error(&e)))?;
        for (name, value) in request.fields() {
            form.append_with_str(name, &value).map_err(|e| Error::Unreadable(js_error(&e)))?;
        }
        for (i, path) in request.references.iter().enumerate() {
            let blob = self.blob(path).await.map_err(|e| match e {
                Error::Unreadable(why) => Error::Unreadable(why),
                other => Error::Unreadable(format!("{}: {other}", path.display())),
            })?;
            form.append_with_blob_and_filename(api::REFERENCE_FIELD, &blob, &api::reference_name(i, path)).map_err(|e| Error::Unreadable(js_error(&e)))?;
        }
        let response = self.fetch("POST", "v1/images", Some(&form), None).await?;
        let body = text(&response).await?;
        serde_json::from_str(&body).map_err(|e| Error::Malformed(e.to_string()))
    }
}

/// The page's own folder, with a trailing slash: the server's routes are relative to it, so the
/// studio can be served under a path as long as `v1/` sits beside it.
pub fn server() -> String {
    let base = web_sys::window().and_then(|w| w.document()).and_then(|d| d.base_uri().ok().flatten()).unwrap_or_default();
    let base = base.split(['?', '#']).next().unwrap_or_default();
    match base.rfind('/') {
        Some(at) => base[..=at].to_owned(),
        None => base.to_owned(),
    }
}

/// Picks pictures to edit with the browser's file chooser.
pub fn ask_for_references(s: &State, _: &mut Tx) {
    if let Some(net) = &s.net {
        net.pick();
    }
}

/// Downloads the PNG at `file`, a route on the page's own origin, under `name`.
pub fn save(_: &State, file: &Path, name: &str, _: &mut Tx) {
    let Some(document) = web_sys::window().and_then(|w| w.document()) else { return };
    let Ok(a) = document.create_element("a") else { return };
    let a: HtmlAnchorElement = a.unchecked_into();
    a.set_href(&file.to_string_lossy());
    a.set_download(name);
    a.click();
}

fn storage() -> Option<web_sys::Storage> {
    web_sys::window().and_then(|w| w.local_storage().ok().flatten())
}

fn js_error(e: &JsValue) -> String {
    e.dyn_ref::<js_sys::Error>().map_or_else(|| e.as_string().unwrap_or_else(|| format!("{e:?}")), |e| String::from(e.message()))
}

async fn text(response: &Response) -> Result<String, Error> {
    let promise = response.text().map_err(|e| Error::Unreachable(js_error(&e)))?;
    JsFuture::from(promise).await.map_err(|e| Error::Unreachable(js_error(&e)))?.as_string().ok_or_else(|| Error::Malformed("not text".into()))
}

async fn sleep(wait: Duration) {
    let promise = Promise::new(&mut |resolve, _| {
        if let Some(w) = web_sys::window() {
            let _ = w.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, wait.as_millis() as i32);
        }
    });
    let _ = JsFuture::from(promise).await;
}

/// Follows the event stream at `url` until the server closes it, or it fails, handing each
/// generation to `each`; true when anything arrived. `EventSource` reconnects by itself, so it
/// is closed at the first error and the caller decides whether to follow again.
async fn events(url: &str, mut each: impl FnMut(Generation) + 'static) -> bool {
    let Ok(source) = EventSource::new(url) else { return false };
    let mut resolve: Option<Function> = None;
    let closed = Promise::new(&mut |r, _| resolve = Some(r));
    let Some(resolve) = resolve else { return false };
    let heard = Rc::new(Cell::new(false));
    let end = {
        let (source, resolve) = (source.clone(), resolve.clone());
        move || {
            source.close();
            let _ = resolve.call0(&JsValue::NULL);
        }
    };
    let on_message = {
        let (heard, end) = (heard.clone(), end.clone());
        Closure::<dyn FnMut(MessageEvent)>::new(move |event: MessageEvent| {
            let Some(data) = event.data().as_string() else { return };
            match serde_json::from_str::<Generation>(&data) {
                Ok(g) => {
                    heard.set(true);
                    let finished = g.is_finished();
                    each(g);
                    if finished {
                        end();
                    }
                }
                Err(_) => end(),
            }
        })
    };
    let on_error = Closure::<dyn FnMut()>::new(end);
    source.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
    source.set_onerror(Some(on_error.as_ref().unchecked_ref()));
    let _ = JsFuture::from(closed).await;
    source.set_onmessage(None);
    source.set_onerror(None);
    heard.get()
}

/// Fetches queued pictures one at a time, the user's first, until the queue is empty.
fn fetch_pictures(inner: &Rc<Inner>) {
    if inner.fetching.replace(true) {
        return;
    }
    let inner = inner.clone();
    spawn_local(async move {
        loop {
            let next = inner.queue.borrow_mut().next();
            let Some(id) = next else { break };
            let path = format!("v1/generations/{id}/image");
            let answer = match inner.blob(Path::new(&path)).await {
                Ok(blob) => thumbnail(&blob).await.map(|(w, h, rgba)| inner.images.rgba(w, h, rgba)),
                Err(e) => Err(e.to_string()),
            };
            if !inner.queue.borrow_mut().wanted.remove(&id) {
                continue;
            }
            inner.handle.update(move |s, _| match answer {
                Ok(thumb) => s.picture_ready(&id, PathBuf::from(path), thumb),
                Err(why) => s.picture_failed(&id, why),
            });
        }
        inner.fetching.set(false);
    });
}

async fn probe(inner: &Inner, id: &str) -> Probe {
    let answer = inner.fetch("GET", &format!("v1/generations/{id}/image"), None, None).await;
    if let ControlFlow::Break(probe) = picture_kind(&answer) {
        return probe;
    }
    // Only the status was wanted; the picture is fetched when the gallery gets to it.
    if let Some(body) = answer.ok().and_then(|r| r.body()) {
        let _ = body.cancel();
    }
    inner.json::<Generation>(&format!("v1/generations/{id}")).await.map_or(Probe::Unanswered, Probe::Picture)
}

/// A gallery thumbnail: the browser decodes the picture and scales it down off the main
/// thread, so a 16 MP PNG costs the page only the small copy it reads back.
async fn thumbnail(blob: &Blob) -> Result<(u32, u32, Vec<u8>), String> {
    let window = web_sys::window().ok_or("no window")?;
    let options = ImageBitmapOptions::new();
    options.set_image_orientation(ImageOrientation::FromImage);
    let promise = window.create_image_bitmap_with_blob_and_image_bitmap_options(blob, &options).map_err(|e| js_error(&e))?;
    let full: ImageBitmap = JsFuture::from(promise).await.map_err(|e| js_error(&e))?.unchecked_into();
    let (w, h) = thumb::fit(full.width(), full.height(), thumb::SIDE);
    let options = ImageBitmapOptions::new();
    options.set_resize_width(w);
    options.set_resize_height(h);
    options.set_resize_quality(ResizeQuality::High);
    let small = window.create_image_bitmap_with_image_bitmap_and_image_bitmap_options(&full, &options).map_err(|e| js_error(&e));
    let small = match small {
        Ok(promise) => JsFuture::from(promise).await.map_err(|e| js_error(&e)),
        Err(e) => Err(e),
    };
    full.close();
    let small: ImageBitmap = small?.unchecked_into();
    let canvas = OffscreenCanvas::new(w, h).map_err(|e| js_error(&e))?;
    let context: OffscreenCanvasRenderingContext2d = canvas.get_context("2d").map_err(|e| js_error(&e))?.ok_or("no 2D canvas")?.unchecked_into();
    context.draw_image_with_image_bitmap(&small, 0.0, 0.0).map_err(|e| js_error(&e))?;
    small.close();
    let pixels = context.get_image_data(0.0, 0.0, f64::from(w), f64::from(h)).map_err(|e| js_error(&e))?;
    Ok((w, h, pixels.data().0))
}

/// A reference's thumbnail, refused unless it is a PNG or JPEG: mmh3 reads only those, though
/// the browser would show more.
async fn reference(blob: &Blob) -> Result<(u32, u32, Vec<u8>), String> {
    let head = blob.slice_with_i32_and_i32(0, 4).map_err(|e| js_error(&e))?;
    let head = JsFuture::from(head.array_buffer()).await.map_err(|e| js_error(&e))?;
    let head = Uint8Array::new(&head).to_vec();
    if !head.starts_with(b"\x89PNG") && !head.starts_with(&[0xff, 0xd8, 0xff]) {
        return Err("Not a PNG or JPEG picture.".into());
    }
    thumbnail(blob).await
}

/// Keeps picked or dropped pictures for this page and attaches them, leaving out what mmh3
/// can't read.
fn add_files(inner: &Rc<Inner>, files: &FileList) {
    let mut paths = Vec::new();
    let mut rejected = 0;
    for file in (0..files.length()).filter_map(|i| files.get(i)) {
        let name = file.name();
        let accepted = Path::new(&name).extension().and_then(|e| e.to_str()).is_some_and(|e| REFERENCE_EXTENSIONS.iter().any(|x| x.eq_ignore_ascii_case(e)));
        if !accepted {
            rejected += 1;
            continue;
        }
        let n = inner.picked.get() + 1;
        inner.picked.set(n);
        let path = Path::new(PICKED).join(n.to_string()).join(&name);
        inner.files.borrow_mut().insert(path.clone(), file.into());
        paths.push(path);
    }
    inner.handle.update(move |s, _| {
        s.dropping = false;
        if !paths.is_empty() {
            s.add_references(&paths);
            s.show_composer();
        }
        if rejected > 0 {
            s.note("mmh3 edits PNG and JPEG pictures.", None, true);
        }
    });
}

/// Rune has no file drops on the web, so the page takes them: a drop anywhere attaches the
/// pictures to the composer, which lights up while files are over the page.
fn listen_for_drops(inner: &Rc<Inner>) {
    let Some(document) = web_sys::window().and_then(|w| w.document()) else { return };
    let hover = |inner: Rc<Inner>, over: bool| {
        if inner.dropping.replace(over) != over {
            inner.handle.update(move |s, _| s.dropping = over);
        }
    };
    let carries_files = |event: &DragEvent| event.data_transfer().is_some_and(|d| d.types().includes(&JsValue::from_str("Files"), 0));
    let i = inner.clone();
    let over = Closure::<dyn FnMut(DragEvent)>::new(move |event: DragEvent| {
        if !carries_files(&event) {
            return;
        }
        event.prevent_default();
        if let Some(d) = event.data_transfer() {
            d.set_drop_effect("copy");
        }
        hover(i.clone(), true);
    });
    let i = inner.clone();
    let leave = Closure::<dyn FnMut(DragEvent)>::new(move |event: DragEvent| {
        // Leaving one element for another inside the page names the other; leaving the page
        // names none.
        if event.related_target().is_none() {
            hover(i.clone(), false);
        }
    });
    let i = inner.clone();
    let drop = Closure::<dyn FnMut(DragEvent)>::new(move |event: DragEvent| {
        let Some(files) = event.data_transfer().and_then(|d| d.files()) else { return };
        event.prevent_default();
        i.dropping.set(false);
        add_files(&i, &files);
    });
    for (name, handler) in [("dragover", over), ("dragleave", leave), ("drop", drop)] {
        let _ = document.add_event_listener_with_callback(name, handler.as_ref().unchecked_ref());
        // The listeners stay for the page's whole life, as the studio does.
        handler.forget();
    }
}

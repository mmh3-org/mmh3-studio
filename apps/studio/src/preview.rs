use std::fmt::Write as _;

use crate::api::Timings;
use crate::sizes::megapixels;
use rune::{Color, Cx, El, SurfaceFit, col, el, icon, row, text};
use rune_kit::{Button, ButtonVariant, ControlSize, IconButton, IconButtonVariant, Kit, Placement, Popover, Progress, Spinner};

use crate::gallery::{elapsed, stage as step_stage, stopped_after};
use crate::state::{Conn, Gen, Live, Phase, Picture, State};
use crate::{icons, stage};

pub fn main(s: &State, k: &Kit, cx: &mut Cx<State>, layer: &mut Vec<El>) -> El {
    let body = match s.selected_gen() {
        Some(g) => generation(s, g, k, cx, layer),
        None => match &s.conn {
            Conn::Disconnected(why) => disconnected(s, why, k),
            _ => empty(s, k),
        },
    };
    let mut pane = col().key("main").w_full().h_full().min_h(0.0);
    if let (Conn::Disconnected(_), Some(_)) = (&s.conn, s.selected_gen()) {
        pane = pane.child(banner(s, k));
    }
    pane.child(body)
}

fn centred(content: El) -> El {
    col().key("centred").w_full().grow(1.0).min_h(0.0).center().padding(32.0).child(content)
}

fn empty(s: &State, k: &Kit) -> El {
    let c = &k.c;
    let hint = if s.connected() { "Describe an image in the composer, pick a size and press Generate (⌘↩)." } else { "Looking for mmh3 server…" };
    centred(
        col()
            .key("empty")
            .max_w(420.0)
            .gap(10.0)
            .items_center()
            .child(row().size(44.0, 44.0).radius(12.0).fill(c.layer_02).center().child(icon(&icons::SPARKLE, 20.0).color(c.icon_muted)))
            .child(text("No image selected").font(k.ty.body_medium).color(c.text_base))
            .child(text(hint).key("empty-hint").font(k.ty.ui).color(c.text_muted).wrap().align(rune::Align::Center)),
    )
}

pub fn start_command(s: &State) -> String {
    let mut cmd = String::from("export MMH3_MODELS=$PWD/models\ntarget/release/mmh3 server");
    // The web build's server is the page's own origin, which proxies to wherever mmh3 listens.
    if cfg!(not(target_arch = "wasm32")) && s.server != crate::api::DEFAULT_SERVER {
        let _ = write!(cmd, " --listen {}", s.host());
    }
    cmd
}

fn code_block(cmd: &str, k: &Kit) -> El {
    col()
        .key("start-command")
        .w_full()
        .padding(12.0)
        .radius(8.0)
        .fill(k.c.layer_02)
        .border(0.5, k.c.border_muted)
        .child(text(cmd.to_owned()).font(k.ty.mono).color(k.c.text_base).wrap().selectable(true).selection_color(k.selection()))
}

fn disconnected(s: &State, why: &str, k: &Kit) -> El {
    let c = &k.c;
    let cmd = start_command(s);
    let copy = cmd.clone();
    centred(
        col()
            .key("disconnected")
            .role("alert")
            .label("mmh3 server isn't running")
            .w_full()
            .max_w(520.0)
            .gap(14.0)
            .padding(24.0)
            .radius(12.0)
            .fill(c.bg_base)
            .border(0.5, c.border_base)
            .child(
                row()
                    .gap(10.0)
                    .center_y()
                    .child(row().size(32.0, 32.0).radius(8.0).fill(c.danger.bg).center().child(icon(&icons::TERMINAL, 16.0).color(c.danger.icon)))
                    .child(text("mmh3 server isn't running").font(k.ty.body_medium).color(c.text_base)),
            )
            .child(
                text(format!(
                    "mmh3 studio is a client of a resident mmh3 server at {}, which keeps the text encoder, DiT and VAE loaded so no generation pays a cold start. Start it in a terminal from your mmh3 checkout:",
                    s.host()
                ))
                .font(k.ty.ui)
                .color(c.text_muted)
                .wrap(),
            )
            .child(code_block(&cmd, k))
            .child(
                row()
                    .key("retry-row")
                    .w_full()
                    .gaps(8.0, 8.0)
                    .center_y()
                    .justify_between()
                    .flex_wrap()
                    .child(text("Trying again every 2 seconds.").key("retry-note").font(k.ty.small).color(c.text_faint))
                    .child(Button::new("Copy command").icon(&icons::COPY).on_press_tx(move |_: &mut State, tx| tx.copy(copy.clone())).build(k).key("copy-command")),
            )
            .child(text(format!("Last error: {why}")).key("last-error").font(k.ty.small).color(c.text_faint).wrap()),
    )
}

fn banner(s: &State, k: &Kit) -> El {
    let c = &k.c;
    let cmd = start_command(s);
    row()
        .key("offline-banner")
        .role("alert")
        .w_full()
        .px(16.0)
        .py(8.0)
        .gap(8.0)
        .center_y()
        .fill(c.danger.bg)
        .child(el().size(8.0, 8.0).radius(4.0).fill(c.danger.icon).a11y_hidden())
        .child(
            text(format!("Not connected to mmh3 server at {}. The studio reconnects by itself once it's running.", s.host()))
                .key("offline-note")
                .font(k.ty.small)
                .color(c.danger.fg)
                .wrap()
                .grow(1.0)
                .shrink()
                .min_w(0.0),
        )
        .child(
            Button::new("Copy start command")
                .variant(ButtonVariant::Outline)
                .icon(&icons::COPY)
                .on_press_tx(move |_: &mut State, tx| tx.copy(cmd.clone()))
                .build(k)
                .key("copy-start-command"),
        )
}

fn placeholder(g: &Gen, k: &Kit, content: El) -> El {
    frame(g, k, None, vec![content])
}

/// The picture's frame at its aspect ratio, with `backdrop` (a step preview) filling it under
/// `content`.
fn frame(g: &Gen, k: &Kit, backdrop: Option<El>, content: Vec<El>) -> El {
    let (w, h) = g.size().or(g.live.as_ref().and_then(|l| l.image.size()).map(|[w, h]| (w, h))).unwrap_or((1, 1));
    let ratio = w.max(1) as f32 / h.max(1) as f32;
    let frame = col()
        .key("frame")
        .aspect_fit(ratio)
        .radius(10.0)
        .fill(k.c.layer_02)
        .border(1.0, k.c.border_base)
        .center()
        .padding(24.0)
        .children(backdrop)
        .children(content);
    col()
        .key("canvas")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .padding(24.0)
        .center()
        .child(col().key("frame-room").w_full().max_w(900.0).grow(1.0).min_h(0.0).center().child(frame))
}

/// A step preview filling the frame.
fn backdrop(live: &Live, dim: bool) -> El {
    let img = el()
        .key("live-preview")
        .role("img")
        .label(format!("Preview after step {}", live.step))
        .pin(Some(0.0), Some(0.0), Some(0.0), Some(0.0))
        .radius(10.0)
        .pixels_fit(live.image.pixels(), SurfaceFit::Fill);
    if dim { img.opacity(0.5) } else { img }
}

/// Words over a preview, on a backing that keeps them legible whatever the picture is.
fn chip(key: &str, k: &Kit, content: El) -> El {
    row()
        .key(key.to_owned())
        .pin(Some(12.0), None, Some(12.0), Some(12.0))
        .padding(12.0)
        .gaps(12.0, 8.0)
        .center_y()
        .flex_wrap()
        .radius(8.0)
        .fill(k.c.bg_base.alpha(0.9))
        .border(0.5, k.c.border_muted)
        .child(content)
}

/// Stops a generation that is waiting or running, on a server that can.
fn stop_button(s: &State, g: &Gen, k: &Kit) -> Option<El> {
    if !matches!(g.phase, Phase::Queued | Phase::Running { .. }) || !(s.can_stop(g) || g.stopping) {
        return None;
    }
    let id = g.id.clone();
    Some(
        Button::new(if g.stopping { "Stopping…" } else { "Stop" })
            .variant(ButtonVariant::Outline)
            .icon(&icons::STOP)
            .disabled(g.stopping)
            .help("Stop at the end of this step")
            .on_press(move |s: &mut State| s.stop(&id))
            .build(k)
            .key("stop"),
    )
}

fn progress(s: &State, g: &Gen, k: &Kit) -> El {
    let c = &k.c;
    let (headline, detail, value) = match &g.phase {
        Phase::Sending => ("Sending to the server…".to_owned(), String::new(), None),
        Phase::Queued if g.stopping => ("Cancelling…".to_owned(), String::new(), None),
        Phase::Running { since, .. } if g.stopping => ("Stopping after this step…".to_owned(), format!("{} elapsed", elapsed(*since, s.clock)), None),
        Phase::Queued => {
            let ahead = s.gens.iter().filter(|o| o.phase.is_active() && o.id != g.id).count();
            ("Waiting in the queue".to_owned(), if ahead > 0 { format!("{ahead} ahead of it") } else { String::new() }, None)
        }
        Phase::Running { step, steps, since } => {
            let (headline, value) = step_stage(g, *step, *steps);
            let value = step.filter(|k| k < steps).map(|_| value);
            (headline, format!("{} elapsed", elapsed(*since, s.clock)), value)
        }
        _ => (String::new(), String::new(), None),
    };
    let bar = match value {
        Some(v) => Progress::new(Some(v)).label(headline).show_value().build(k),
        None => row()
            .gap(8.0)
            .center_y()
            .child(Spinner::new().size(14.0).build(k))
            .child(text(headline).key("progress-headline").font(k.ty.ui_medium).color(c.text_base)),
    };
    let card = col()
        .key("progress-card")
        .w_full()
        .max_w(320.0)
        .min_w(0.0)
        .gap(8.0)
        .child(bar)
        .child(text(detail).key("progress-detail").font(k.ty.small).color(c.text_muted));
    if let Some(live) = &g.live {
        let steps = match g.phase {
            Phase::Running { steps, .. } => steps,
            _ => crate::api::IMAGE_STEPS,
        };
        let badge =
            row().key("preview-step").pin(Some(12.0), Some(12.0), None, None).px(8.0).py(3.0).radius(6.0).fill(Color::BLACK.alpha(0.62)).child(
                text(format!("Preview · step {}/{steps}", live.step)).key("preview-step-text").font(k.ty.small.features("'tnum' 1")).color(Color::WHITE),
            );
        let over = row().w_full().gaps(12.0, 8.0).center_y().justify_between().flex_wrap().child(card.grow(1.0).shrink()).children(stop_button(s, g, k));
        return frame(g, k, Some(backdrop(live, false)), vec![badge, chip("progress-chip", k, over)]);
    }
    let card = card.child(text(g.prompt().to_owned()).font(k.ty.small).color(c.text_faint).max_lines(3).wrap()).children(stop_button(s, g, k));
    placeholder(g, k, card)
}

fn cancelled(s: &State, g: &Gen, step: Option<u32>, k: &Kit) -> El {
    let c = &k.c;
    let id = g.id.clone();
    let retry = Button::new("Try again with a new seed")
        .icon(&icons::RERUN)
        .disabled(!s.connected() || g.rerun_request().is_none())
        .on_press(move |s: &mut State| s.rerun(&id))
        .build(k)
        .key("retry");
    let headline = text(stopped_after(step)).key("cancelled-headline").font(k.ty.body_medium).color(c.text_base);
    if let Some(live) = &g.live {
        let words = col()
            .key("cancelled")
            .grow(1.0)
            .shrink()
            .min_w(0.0)
            .gap(2.0)
            .child(headline)
            .child(text(format!("This is its preview after step {}.", live.step)).font(k.ty.small).color(c.text_muted).wrap());
        let over = row().w_full().gaps(12.0, 8.0).center_y().justify_between().flex_wrap().child(words).child(retry);
        return frame(g, k, Some(backdrop(live, true)), vec![chip("cancelled-chip", k, over)]);
    }
    let card = col().key("cancelled").w_full().max_w(420.0).min_w(0.0).gap(10.0).items_center().child(headline).child(retry);
    placeholder(g, k, card)
}

fn failed(s: &State, g: &Gen, why: &str, k: &Kit) -> El {
    let c = &k.c;
    let id = g.id.clone();
    let card = col()
        .key("failed")
        .role("alert")
        .label("Generation failed")
        .w_full()
        .max_w(420.0)
        .min_w(0.0)
        .gap(10.0)
        .items_center()
        .child(text("Generation failed").font(k.ty.body_medium).color(c.danger.fg))
        .child(text(why.to_owned()).key("failure").font(k.ty.ui).color(c.text_muted).wrap().align(rune::Align::Center))
        .child(
            Button::new("Try again with a new seed")
                .icon(&icons::RERUN)
                .disabled(!s.connected())
                .on_press(move |s: &mut State| s.rerun(&id))
                .build(k)
                .key("retry"),
        );
    placeholder(g, k, card)
}

fn image(s: &State, g: &Gen, k: &Kit) -> El {
    match ready_preview(s, g) {
        Some(img) if img.error().is_none() => {
            stage::stage(&s.zoom, el().key("preview-image").role("img").label(crate::gallery::title(g)).pixels(img.pixels()), k)
        }
        Some(img) => placeholder(g, k, text(format!("Couldn't show the image: {:?}", img.error())).font(k.ty.ui).color(k.c.danger.fg).wrap()),
        None => {
            let label = match &g.picture {
                Picture::Failed(why) => format!("Couldn't fetch the image: {why}"),
                _ => "Loading image…".to_owned(),
            };
            let loading = row()
                .key("loading")
                .gap(8.0)
                .center_y()
                .child(Spinner::new().size(14.0).build(k))
                .child(text(label).font(k.ty.ui).color(k.c.text_muted).wrap());
            match &g.live {
                Some(live) => frame(g, k, Some(backdrop(live, false)), vec![chip("loading-chip", k, loading)]),
                None => placeholder(g, k, loading),
            }
        }
    }
}

/// The selected picture, decoded, once it is.
fn ready_preview<'a>(s: &'a State, g: &Gen) -> Option<&'a rune::Image> {
    s.preview.as_ref().filter(|(id, _)| *id == g.id).map(|(_, img)| img).filter(|img| img.is_ready())
}

fn generation(s: &State, g: &Gen, k: &Kit, cx: &mut Cx<State>, layer: &mut Vec<El>) -> El {
    let picture = ready_preview(s, g).filter(|img| img.error().is_none()).and_then(rune::Image::size);
    if let Some([w, h]) = picture {
        s.zoom.measure([w as f32, h as f32], cx.layout_size(stage::KEY).unwrap_or([800.0, 600.0]));
    }
    let canvas = match &g.phase {
        Phase::Done { .. } => image(s, g, k),
        Phase::Failed(why) => failed(s, g, why, k),
        Phase::Cancelled { step } => cancelled(s, g, *step, k),
        _ => progress(s, g, k),
    };
    col().key("generation").w_full().grow(1.0).min_h(0.0).child(bar(s, g, picture.is_some(), k, layer)).child(canvas)
}

type Action = Box<dyn Fn(&mut State, &mut rune::Tx)>;

fn tool(icon: &'static rune::Icon, key: &str, label: &str, disabled: bool, k: &Kit, f: Action) -> El {
    IconButton::new(icon)
        .variant(IconButtonVariant::GhostMuted)
        .label(label)
        .help(label)
        .disabled(disabled)
        .on_press_tx(move |s: &mut State, tx: &mut rune::Tx| f(s, tx))
        .build(k)
        .key(key.to_owned())
}

/// What can be done with the selected generation.
fn actions(s: &State, g: &Gen, k: &Kit) -> El {
    let done = g.file().is_some();
    let running = matches!(g.phase, Phase::Running { .. } | Phase::Sending);
    let known = g.is_known();
    let rerunnable = g.rerun_request().is_some();
    let id = g.id.clone();
    let mut actions = row().key("picture-actions").role("group").label("Picture").gap(2.0).center_y();
    let i = id.clone();
    actions = actions.child(tool(&icons::SAVE, "save", "Save PNG…", !done, k, Box::new(move |s, tx| s.save(&i, tx))));
    #[cfg(not(target_arch = "wasm32"))]
    {
        let i = id.clone();
        actions = actions.child(tool(&icons::REVEAL, "reveal", "Reveal in Finder", !done, k, Box::new(move |s, _| s.reveal(&i))));
    }
    let i = id.clone();
    actions = actions.child(tool(&icons::PICTURE, "edit-picture", "Edit this picture", !done, k, Box::new(move |s, _| s.edit_picture(&i))));
    let prompt = g.prompt().to_owned();
    actions = actions.child(tool(&icons::COPY, "copy-prompt", "Copy prompt", !known, k, Box::new(move |_, tx| tx.copy(prompt.clone()))));
    let seed = g.seed().map(|s| s.to_string()).unwrap_or_default();
    actions = actions.child(tool(&icons::HASH, "copy-seed", "Copy seed", !known, k, Box::new(move |_, tx| tx.copy(seed.clone()))));
    let i = id.clone();
    actions = actions.child(tool(&icons::RERUN, "rerun", "Re-run with a new seed", !s.connected() || !rerunnable, k, Box::new(move |s, _| s.rerun(&i))));
    if matches!(g.phase, Phase::Queued | Phase::Running { .. }) && (s.can_stop(g) || g.stopping) {
        let i = id.clone();
        actions = actions.child(tool(&icons::STOP, "stop-tool", "Stop", !s.can_stop(g), k, Box::new(move |s, _| s.stop(&i))));
    }
    actions.child(tool(&icons::TRASH, "delete", "Delete", running, k, Box::new(move |s, _| s.delete(&id))))
}

/// Zoom out, the level (which shows the picture at 100%), zoom in and fit.
fn zoom(s: &State, shown: bool, k: &Kit) -> El {
    let z = &s.zoom;
    let level = if shown { format!("{}%", z.level()) } else { "–".to_owned() };
    let tnum = k.ty.small.features("'tnum' 1");
    let actual = Button::new("Actual size")
        .variant(ButtonVariant::GhostMuted)
        .control_size(ControlSize::Small)
        .help("Actual size (100%)")
        .disabled(!shown || z.is_actual())
        .label_with(move |_, ink| text(level).font(tnum).color(ink))
        .on_press_tx(|s: &mut State, tx| s.zoom.actual(tx))
        .build(k)
        .key("zoom-level")
        .min_w(52.0);
    row()
        .key("zoom-controls")
        .role("group")
        .label("Zoom")
        .gap(2.0)
        .center_y()
        .child(tool(&icons::ZOOM_OUT, "zoom-out", "Zoom out", !shown || !z.can_zoom_out(), k, Box::new(|s, tx| s.zoom.zoom_by(1.0 / stage::STEP, tx))))
        .child(actual)
        .child(tool(&icons::ZOOM_IN, "zoom-in", "Zoom in", !shown || !z.can_zoom_in(), k, Box::new(|s, tx| s.zoom.zoom_by(stage::STEP, tx))))
        .child(tool(&icons::FIT, "zoom-fit", "Fit", !shown || z.is_fit(), k, Box::new(|s, tx| s.zoom.fit(tx))))
}

/// Size · seed · time, the time opening where it went.
fn info(s: &State, g: &Gen, k: &Kit, layer: &mut Vec<El>) -> El {
    let c = &k.c;
    let decoded = ready_preview(s, g).and_then(rune::Image::size);
    let mut facts = Vec::new();
    if let Some((w, h)) = g.size().or(decoded.map(|[w, h]| (w, h))) {
        facts.push(format!("{w} × {h} · {:.1} MP", megapixels(w, h)));
    }
    if let Some(seed) = g.seed() {
        facts.push(format!("seed {seed}"));
    }
    match g.reference_count() {
        0 => {}
        1 => facts.push("edits 1 picture".into()),
        n => facts.push(format!("edits {n} pictures")),
    }
    let mut out = row().key("facts").gap(4.0).center_y().shrink().min_w(0.0);
    if !facts.is_empty() {
        out = out.child(text(facts.join(" · ")).key("facts-text").font(k.ty.small).color(c.text_muted).truncate().shrink().min_w(0.0));
    }
    if let Phase::Done { seconds, timings } = &g.phase {
        let trigger = Button::new(format!("{seconds:.1} s"))
            .variant(ButtonVariant::GhostMuted)
            .control_size(ControlSize::Small)
            .help("Where the time went")
            .on_press(|s: &mut State| s.timing_open = !s.timing_open)
            .build(k)
            .key("took");
        let popover = Popover::new("timing")
            .trigger(trigger)
            .title(format!("Took {seconds:.1} s"))
            .placement(Placement::BottomEnd)
            .open(s.timing_open)
            .on_close(|s: &mut State| s.timing_open = false)
            .child(timing(s, g, *seconds, timings.as_ref(), k))
            .build(k, layer);
        out = out.child(popover);
    }
    out
}

/// One stage of a generation, as the timing details list it.
struct Part {
    name: String,
    seconds: f64,
    ink: Color,
}

/// Where the time went: the server's stage timings, or the studio's estimate of them when the
/// server reports only the total.
fn timing(s: &State, g: &Gen, total: f64, timings: Option<&Timings>, k: &Kit) -> El {
    let c = &k.c;
    let part = |name: String, seconds: f64, ink| Part { name, seconds, ink };
    let (parts, note) = if let Some(t) = timings {
        // A stage too short to see in the bar would only be a row of zeroes.
        let stage = |name: &str, seconds: Option<f64>, ink| seconds.filter(|v| *v >= 0.05).map(|v| part(name.into(), v, ink));
        let steps = (!t.steps.is_empty()).then(|| {
            let sum: f64 = t.steps.iter().sum();
            part(format!("{} steps, {:.1} s each", t.steps.len(), sum / t.steps.len() as f64), sum, c.icon_accent)
        });
        let mut parts: Vec<Part> = [
            stage("Loading the text encoder", t.load_text, c.border_strong),
            stage("Reading the prompt", t.encode, c.icon_faint),
            stage("Loading the DiT", t.load_dit, c.border_strong),
            steps,
            stage("Waiting for the VAE", t.decode_read, c.border_strong),
            stage("Decoding", t.decode, c.icon_muted),
            stage("Writing the PNG", t.write, c.icon_faint),
        ]
        .into_iter()
        .flatten()
        .collect();
        let rest = total - parts.iter().map(|p| p.seconds).sum::<f64>();
        if rest > 0.05 {
            parts.push(part("Everything else".into(), rest, c.border_base));
        }
        (parts, None)
    } else {
        let model = s.time_model();
        let mp = g.size().map_or(0.0, |(w, h)| megapixels(w, h));
        let scale = total / model.warm(mp).max(f64::EPSILON);
        let parts = vec![
            part(format!("{} steps, ≈ {:.1} s each", crate::api::IMAGE_STEPS, model.step_at(mp) * scale), model.steps_at(mp) * scale, c.icon_accent),
            part("Decoding".into(), model.decode_at(mp) * scale, c.icon_muted),
            part("Everything else".into(), model.fixed * scale, c.border_base),
        ];
        (parts, Some("The server reports only the total, so this splits it as the studio's estimate does."))
    };
    let mut bar = row().key("timing-bar").w_full().h(8.0).gap(2.0).a11y_hidden();
    for p in &parts {
        bar = bar.child(el().h_full().grow(p.seconds.max(0.0) as f32).radius(2.0).fill(p.ink));
    }
    let mut rows = col().key("timing-rows").w_full().gap(6.0);
    for p in &parts {
        let value = if timings.is_some() { format!("{:.1} s", p.seconds) } else { format!("≈ {:.1} s", p.seconds) };
        rows = rows.child(
            row()
                .w_full()
                .gap(8.0)
                .center_y()
                .child(el().size(8.0, 8.0).radius(2.0).fill(p.ink).a11y_hidden())
                .child(text(p.name.clone()).font(k.ty.small).color(c.text_muted).grow(1.0).shrink().min_w(0.0).truncate())
                .child(text(value).font(k.ty.small.features("'tnum' 1")).color(c.text_base)),
        );
    }
    let mut out = col().key("timing").w_full().gap(10.0).child(bar).child(rows);
    if let Some(n) = note {
        out = out.child(text(n).key("timing-note").font(k.ty.small).color(c.text_faint).wrap());
    }
    out
}

/// The row under the title: what can be done with the picture, its zoom, and what it is.
fn bar(s: &State, g: &Gen, shown: bool, k: &Kit, layer: &mut Vec<El>) -> El {
    let divider = || el().size(1.0, 16.0).mx(4.0).fill(k.c.border_base).a11y_hidden();
    row()
        .key("picture-bar")
        .role("toolbar")
        .label("Picture")
        .w_full()
        .px(10.0)
        .py(4.0)
        .gaps(4.0, 2.0)
        .center_y()
        .flex_wrap()
        .border_sides([0.0, 0.0, 1.0, 0.0], k.c.border_muted)
        .child(actions(s, g, k))
        .child(divider())
        .child(zoom(s, shown, k))
        .child(el().grow(1.0).a11y_hidden())
        .child(info(s, g, k, layer))
}

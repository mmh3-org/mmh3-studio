use std::fmt::Write as _;

use crate::sizes::megapixels;
use rune::{Cx, El, col, el, icon, row, text};
use rune_kit::image_zoom::Stage;
use rune_kit::{Button, ButtonVariant, Kit, Progress, Spinner};

use crate::gallery::{elapsed, stage};
use crate::icons;
use crate::state::{Conn, Gen, Phase, Picture, State};

pub fn main(s: &State, k: &Kit, cx: &mut Cx<State>) -> El {
    let body = match s.selected_gen() {
        Some(g) => generation(s, g, k, cx),
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
    if s.server != crate::api::DEFAULT_SERVER {
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

fn caption(label: &str, value: String, k: &Kit) -> El {
    col().gap(2.0).child(text(label.to_owned()).font(k.ty.small).color(k.c.text_faint)).child(text(value).font(k.ty.ui_medium).color(k.c.text_base))
}

/// What the studio knows of a generation: what it asked for, and how long the server says it
/// took. The server times the whole generation, not its stages.
fn facts(s: &State, g: &Gen, k: &Kit) -> El {
    let mut facts = row().key("facts").gap(24.0).flex_wrap();
    let decoded = s.preview.as_ref().filter(|(id, _)| *id == g.id).and_then(|(_, img)| img.size());
    if let Some((w, h)) = g.size().or(decoded.map(|[w, h]| (w, h))) {
        facts = facts.child(caption("Size", format!("{w} × {h} · {:.1} MP", megapixels(w, h)), k));
    }
    if let Some(seed) = g.seed() {
        facts = facts.child(caption("Seed", seed.to_string(), k));
    }
    match g.references().len() {
        0 => {}
        1 => facts = facts.child(caption("Edits", "1 picture".into(), k)),
        n => facts = facts.child(caption("Edits", format!("{n} pictures"), k)),
    }
    if let Phase::Done { seconds } = g.phase {
        facts = facts.child(caption("Took", format!("{seconds:.1} s · {} steps", crate::api::IMAGE_STEPS), k).key("took"));
    }
    if g.request.is_none() {
        facts = facts.child(caption("Prompt", "Asked for outside the studio".into(), k));
    }
    facts
}

fn footer(s: &State, g: &Gen, k: &Kit) -> El {
    col().key("details").w_full().px(20.0).py(14.0).gap(12.0).border_sides([0.0, 1.0, 0.0, 0.0], k.c.border_muted).child(facts(s, g, k))
}

fn placeholder(g: &Gen, k: &Kit, content: El) -> El {
    let (w, h) = g.size().unwrap_or((1, 1));
    let ratio = w.max(1) as f32 / h.max(1) as f32;
    let frame = col().key("frame").aspect_fit(ratio).radius(10.0).fill(k.c.layer_02).border(1.0, k.c.border_base).center().padding(24.0).child(content);
    col()
        .key("canvas")
        .w_full()
        .grow(1.0)
        .min_h(0.0)
        .padding(24.0)
        .center()
        .child(col().key("frame-room").w_full().max_w(900.0).grow(1.0).min_h(0.0).center().child(frame))
}

fn progress(s: &State, g: &Gen, k: &Kit) -> El {
    let c = &k.c;
    let (headline, detail, value) = match &g.phase {
        Phase::Sending => ("Sending to the server…".to_owned(), String::new(), None),
        Phase::Queued => {
            let ahead = s.gens.iter().filter(|o| o.phase.is_active() && o.id != g.id).count();
            ("Waiting in the queue".to_owned(), if ahead > 0 { format!("{ahead} ahead of it") } else { String::new() }, None)
        }
        Phase::Running { step, steps, since } => {
            let (headline, value) = stage(g, *step, *steps);
            let value = step.filter(|k| k < steps).map(|_| value);
            (headline, format!("{} elapsed", elapsed(*since, s.clock)), value)
        }
        _ => (String::new(), String::new(), None),
    };
    let bar = match value {
        Some(v) => Progress::new(Some(v)).label(headline).show_value().build(k),
        None => row().gap(8.0).center_y().child(Spinner::new().size(14.0).build(k)).child(text(headline).font(k.ty.ui_medium).color(c.text_base)),
    };
    let card = col()
        .key("progress-card")
        .w_full()
        .max_w(320.0)
        .min_w(0.0)
        .gap(8.0)
        .child(bar)
        .child(text(detail).key("progress-detail").font(k.ty.small).color(c.text_muted))
        .child(text(g.prompt().to_owned()).font(k.ty.small).color(c.text_faint).max_lines(3).wrap());
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

fn image(s: &State, g: &Gen, k: &Kit, cx: &mut Cx<State>) -> El {
    let ready = s.preview.as_ref().filter(|(id, _)| *id == g.id).map(|(_, img)| img).filter(|img| img.is_ready());
    match ready {
        Some(img) if img.error().is_none() => {
            let [w, h] = img.size().unwrap_or([1, 1]);
            let size = cx.layout_size("artifact-stage").unwrap_or([800.0, 600.0]);
            Stage::new(
                |s: &mut State| &mut s.stage,
                [w as f32, h as f32],
                el().key("preview-image").role("img").label(crate::gallery::title(g)).pixels(img.pixels()),
            )
            .icons(&icons::ZOOM_IN, &icons::ZOOM_OUT, &icons::FIT)
            .labels("Zoom in", "Zoom out", "Fit", "Zoom controls")
            .build(&s.stage, k, size)
        }
        Some(img) => placeholder(g, k, text(format!("Couldn't show the image: {:?}", img.error())).font(k.ty.ui).color(k.c.danger.fg).wrap()),
        None => {
            let label = match &g.picture {
                Picture::Failed(why) => format!("Couldn't fetch the image: {why}"),
                _ => "Loading image…".to_owned(),
            };
            placeholder(
                g,
                k,
                row()
                    .key("loading")
                    .gap(8.0)
                    .center_y()
                    .child(Spinner::new().size(14.0).build(k))
                    .child(text(label).font(k.ty.ui).color(k.c.text_muted).wrap()),
            )
        }
    }
}

fn generation(s: &State, g: &Gen, k: &Kit, cx: &mut Cx<State>) -> El {
    let canvas = match &g.phase {
        Phase::Done { .. } => image(s, g, k, cx),
        Phase::Failed(why) => failed(s, g, why, k),
        _ => progress(s, g, k),
    };
    col().key("generation").w_full().grow(1.0).min_h(0.0).child(canvas).child(footer(s, g, k))
}

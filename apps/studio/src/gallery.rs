use std::borrow::Cow;

#[cfg(not(target_arch = "wasm32"))]
use rune::DragItem;

use rune::{Color, El, SurfaceFit, col, el, memo, row, text};
use rune_kit::shell::Sidebar;
use rune_kit::{Kit, Progress, Spinner};

use crate::state::{Gen, Pane, Phase, Picture, State};
use crate::view::kit;

pub const ROW: f32 = 72.0;
const THUMB_PT: f32 = 56.0;

pub fn sidebar(s: &State, k: &Kit, dark: bool) -> El {
    let count = s.gens.len();
    let header = row()
        .key("gallery-header")
        .w_full()
        .h(28.0)
        .px(8.0)
        .center_y()
        .justify_between()
        .child(text("Generations").font(k.ty.small_medium).color(k.c.text_faint))
        .child(text(if count == 0 { String::new() } else { count.to_string() }).key("gallery-count").font(k.ty.small).color(k.c.text_faint));
    let sb = Sidebar::new().label("Generations").inset(0.0).header_padding(0.0, 6.0).row_gap(0.0).header(header);
    if count == 0 {
        let empty = col()
            .key("gallery-empty")
            .w_full()
            .padding(12.0)
            .gap(4.0)
            .child(text("Nothing yet").font(k.ty.ui_medium).color(k.c.text_muted))
            .child(text("Finished images and the queue show up here.").font(k.ty.small).color(k.c.text_faint).wrap());
        return sb.child(empty).build(k);
    }
    sb.row_height(ROW).virtual_rows(count, |s: &State, i| Cow::Borrowed(s.gens[i].id.as_str()), move |s: &State, i| gallery_row(s, &s.gens[i], dark)).build(k)
}

fn gallery_row(s: &State, g: &Gen, dark: bool) -> El {
    let selected = s.selected.as_deref() == Some(g.id.as_str());
    let tick = if matches!(g.phase, Phase::Running { .. }) { s.tick } else { 0 };
    let clock = s.clock;
    memo(format!("row-{}", g.id), (g.revision, selected, tick, dark), || build_row(g, selected, clock, kit(dark)))
}

pub fn elapsed(since: f64, clock: f64) -> String {
    format!("{:.1} s", (clock - since).max(0.0))
}

/// What a running generation is doing, from the steps the server says are behind it, and how
/// far along that is.
pub fn stage(g: &Gen, step: Option<u32>, steps: u32) -> (String, f32) {
    match step {
        None if !g.has_references() => ("Reading the prompt".into(), 0.0),
        None => ("Reading the prompt and pictures".into(), 0.0),
        Some(k) if k < steps => (format!("Step {} of {steps}", k + 1), k as f32 / steps.max(1) as f32),
        Some(_) => ("Decoding the picture".into(), 1.0),
    }
}

pub fn title(g: &Gen) -> String {
    match g.prompt() {
        _ if !g.is_known() => "Asked for elsewhere".to_owned(),
        "" => "Untitled".to_owned(),
        prompt => prompt.replace('\n', " "),
    }
}

/// How far a cancelled generation got.
pub fn stopped_after(step: Option<u32>) -> String {
    match step {
        None => "Cancelled before its first step".to_owned(),
        Some(1) => "Cancelled after 1 step".to_owned(),
        Some(k) => format!("Cancelled after {k} steps"),
    }
}

fn status_line(g: &Gen, clock: f64, k: &Kit) -> (String, Color) {
    let c = &k.c;
    match &g.phase {
        Phase::Sending => ("Sending…".into(), c.text_faint),
        Phase::Queued if g.stopping => ("Cancelling…".into(), c.text_faint),
        Phase::Queued => ("Queued".into(), c.text_faint),
        Phase::Running { since, .. } if g.stopping => (format!("Stopping after this step · {}", elapsed(*since, clock)), c.text_muted),
        Phase::Running { step, steps, since } => (format!("{} · {}", stage(g, *step, *steps).0, elapsed(*since, clock)), c.text_muted),
        Phase::Done { seconds, .. } => match (g.size(), g.effort()) {
            (Some((w, h)), Some(e)) => (format!("{w}×{h} · {} · {seconds:.1} s", e.label()), c.text_faint),
            (Some((w, h)), None) => (format!("{w}×{h} · {seconds:.1} s"), c.text_faint),
            (None, _) => (format!("{seconds:.1} s"), c.text_faint),
        },
        Phase::Failed(_) => ("Failed".into(), c.danger.fg),
        Phase::Cancelled { .. } => ("Cancelled".into(), c.text_faint),
    }
}

fn thumb(g: &Gen, k: &Kit) -> El {
    let c = &k.c;
    let mut t = el().key("thumb").size(THUMB_PT, THUMB_PT).radius(6.0).fill(c.layer_02).center().border(0.5, c.border_muted);
    let image = |img: &rune::Image| el().key("thumb-image").size(THUMB_PT, THUMB_PT).radius(6.0).pixels_fit(img.pixels(), SurfaceFit::Fill);
    t = match (&g.phase, &g.picture, &g.live) {
        (_, Picture::Ready { thumb: Some(img), .. }, _) => t.child(image(img)),
        (Phase::Running { steps, .. }, _, Some(live)) => t.child(image(&live.image)).child(step_badge(live.step, *steps, k)),
        (Phase::Cancelled { .. }, _, Some(live)) => t.child(image(&live.image).opacity(0.55)),
        (Phase::Done { .. }, _, Some(live)) => t.child(image(&live.image)),
        (Phase::Running { .. } | Phase::Sending | Phase::Queued, ..) | (_, Picture::Loading, _) => {
            t.child(Spinner::new().size(14.0).color(c.icon_faint).build(k))
        }
        (Phase::Failed(_), ..) => t.child(text("!").font(k.ty.ui_medium).color(c.danger.fg)),
        (Phase::Cancelled { .. }, ..) => t.child(text("–").font(k.ty.ui_medium).color(c.text_faint)),
        _ => t,
    };
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(file) = g.file().cloned() {
        t = t.drag_out(move |_: &State| Some(DragItem::new().file(file.clone())));
    }
    t.a11y_hidden()
}

/// Which step a preview shows, over its corner.
pub fn step_badge(step: u32, steps: u32, k: &Kit) -> El {
    row()
        .key("step-badge")
        .pin(None, None, Some(3.0), Some(3.0))
        .px(4.0)
        .py(1.0)
        .radius(4.0)
        .fill(Color::BLACK.alpha(0.62))
        .child(text(format!("{step}/{steps}")).font(k.ty.small.features("'tnum' 1")).color(Color::WHITE))
}

fn build_row(g: &Gen, selected: bool, clock: f64, k: &Kit) -> El {
    let c = &k.c;
    let (status, ink) = status_line(g, clock, k);
    let title = title(g);
    let mut lines = col()
        .key("lines")
        .grow(1.0)
        .shrink()
        .min_w(0.0)
        .gap(3.0)
        .child(
            text(title.clone())
                .key("row-prompt")
                .font(k.ty.ui_compact)
                .color(if selected { c.text_base } else { c.text_muted })
                .max_lines(2)
                .wrap()
                .a11y_hidden(),
        )
        .child(text(status.clone()).key("row-status").font(k.ty.small).color(ink).truncate().a11y_hidden());
    if let Phase::Running { step, steps, .. } = g.phase {
        lines = lines.child(Progress::new(Some(stage(g, step, steps).1)).build(k).key("row-progress").a11y_hidden());
    }
    let id = g.id.clone();
    let menu_id = g.id.clone();
    let mut r = row()
        .key(format!("gen-{}", g.id))
        .role("listitem")
        .label(title)
        .description(status)
        .w_full()
        .h(ROW)
        .px(8.0)
        .gap(10.0)
        .center_y()
        .radius(k.radius.md)
        .focusable(true)
        .focus_selected(selected)
        .hover_group()
        .on_press(move |s: &mut State| {
            s.select(&id);
            s.pane = Pane::Picture;
        })
        .on_secondary_press(move |s: &mut State, p, _| {
            s.select(&menu_id);
            s.menu = Some((menu_id.clone(), p));
        })
        .child(thumb(g, k))
        .child(lines);
    r = k.interactive(r);
    if selected {
        r = r.fill(c.pressed);
    }
    r
}

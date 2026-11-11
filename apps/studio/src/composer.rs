use crate::api::MAX_REFERENCES;
use crate::estimate::{Model, approx};
use crate::sizes::{ASPECTS, MULTIPLE, Quality, megapixels};
use rune::{El, Key, KeyEvent, SurfaceFit, Tx, col, el, icon, row, text};
use rune_kit::{Button, ButtonVariant, ControlSize, IconButton, IconButtonVariant, Kit, Spinner, TextArea, TextInput};

use crate::icons;
use crate::state::{REFERENCE_EXTENSIONS, Reference, SizeChoice, State, Thumb, missing_pictures, picture_tag};

pub const GENERATE_LABEL: &str = "⌘↩";

pub fn is_generate(ev: &KeyEvent) -> bool {
    ev.key == Key::Enter && ev.mods.cmd
}

fn section(title: &str, k: &Kit) -> El {
    text(title.to_owned()).font(k.ty.small_medium).color(k.c.text_faint)
}

/// One choice in a radio group: a bordered tile, raised while it is the one chosen.
fn option(key: String, label: String, on: bool, k: &Kit, content: El, pick: impl Fn(&mut State) + 'static) -> El {
    let c = &k.c;
    let t = content
        .key(key)
        .role("radio")
        .label(label)
        .focusable(true)
        .focus_selected(on)
        .w_full()
        .radius(8.0)
        .border(if on { 1.5 } else { 1.0 }, if on { c.border_focus } else { c.border_base })
        .fill(if on { c.layer_02 } else { c.bg_base })
        .on_press(pick);
    k.interactive(t)
}

/// What a size is, and how long it should take here: "2048 × 2048 · 4.2 MP · ≈ 30 s".
fn estimate(s: &State, model: &Model, (w, h): (u32, u32)) -> String {
    let seconds = model.seconds(megapixels(w, h), s.cold());
    let slower = if s.gpu_held_back() { " · may be slower" } else { "" };
    format!("{w} × {h} · {:.1} MP · ≈ {}{slower}", megapixels(w, h), approx(seconds))
}

fn quality(s: &State, model: &Model, k: &Kit) -> El {
    let c = &k.c;
    let mut group = row().key("quality").role("radiogroup").focus_group().label("Quality").w_full().equal_columns(2).gaps(8.0, 8.0);
    for (i, q) in Quality::ALL.into_iter().enumerate() {
        let pick = SizeChoice { quality: q, ..s.size };
        let (w, h) = pick.size();
        let time = format!("≈ {}", approx(model.seconds(megapixels(w, h), s.cold())));
        let on = !s.custom_size && s.size.quality == q;
        let content = col()
            .px(10.0)
            .py(8.0)
            .gap(2.0)
            .child(text(q.label()).font(k.ty.ui_medium).color(c.text_base).a11y_hidden())
            .child(text(format!("{} · {time}", q.about())).key("quality-time").font(k.ty.small).color(c.text_muted).truncate().a11y_hidden());
        let label = format!("{}, {}, {time}", q.label(), q.about());
        group = group.child(
            option(format!("quality-{}", q.label().to_lowercase()), label, on, k, content, move |s: &mut State| s.pick_size(pick)).position_in_set(i + 1, 2),
        );
    }
    group
}

/// A rectangle in the aspect's shape, 18 points on its long side.
fn shape(w: u32, h: u32, ink: rune::Color) -> El {
    let long = w.max(h) as f32;
    let side = |v: u32| (18.0 * v as f32 / long).round();
    row().size(18.0, 18.0).center().a11y_hidden().child(el().size(side(w), side(h)).radius(2.0).border(1.25, ink))
}

fn aspects(s: &State, k: &Kit) -> El {
    let c = &k.c;
    let mut group = row().key("aspects").role("radiogroup").focus_group().label("Aspect ratio").w_full().equal_columns(4).gaps(6.0, 6.0);
    for (i, a) in ASPECTS.iter().enumerate() {
        let pick = SizeChoice { aspect: i, ..s.size };
        let on = !s.custom_size && s.size.aspect == i;
        let (w, h) = a.full;
        let ink = if on { c.icon_base } else { c.icon_muted };
        let content = col()
            .py(8.0)
            .gap(4.0)
            .items_center()
            .child(shape(w, h, ink))
            .child(text(a.name).font(k.ty.small_medium).color(if on { c.text_base } else { c.text_muted }).a11y_hidden());
        group = group.child(
            option(format!("aspect-{i}"), a.name.to_owned(), on, k, content, move |s: &mut State| s.pick_size(pick)).position_in_set(i + 1, ASPECTS.len()),
        );
    }
    group
}

fn custom_size(s: &State, k: &Kit) -> El {
    let error = s.chosen_size().err();
    let field = |edit: &rune::TextEdit, label: &str, key: &str, lens: fn(&mut State) -> &mut rune::TextEdit| {
        TextInput::new(edit).label(label).placeholder("1024").invalid(error.is_some()).on_edit(lens).build(k).key(key.to_owned()).grow(1.0).shrink().min_w(0.0)
    };
    row()
        .key("custom-size")
        .w_full()
        .gap(8.0)
        .center_y()
        .child(field(&s.custom_w, "Width", "custom-width", |s| &mut s.custom_w))
        .child(text("×").font(k.ty.ui).color(k.c.text_faint))
        .child(field(&s.custom_h, "Height", "custom-height", |s| &mut s.custom_h))
}

fn sizes(s: &State, k: &Kit) -> El {
    let c = &k.c;
    let model = s.time_model();
    let open = s.custom_size;
    let disclosure = Button::new("Custom size…")
        .variant(ButtonVariant::GhostMuted)
        .control_size(ControlSize::Small)
        .icon(if open { k.glyph.triangle_down } else { k.glyph.triangle_right })
        .help(format!("Any width and height, in multiples of {MULTIPLE}"))
        .expanded(open)
        .on_press(State::toggle_custom_size)
        .build(k)
        .key("custom-toggle");
    let mut custom = col().w_full().gap(8.0).child(row().w_full().child(disclosure));
    if open {
        custom = custom.child(custom_size(s, k));
    }
    let out = col()
        .w_full()
        .gap(14.0)
        .child(col().w_full().gap(8.0).child(section("Quality", k)).child(quality(s, &model, k)))
        .child(col().w_full().gap(8.0).child(section("Aspect ratio", k)).child(aspects(s, k)))
        .child(custom);
    let summary = match s.chosen_size() {
        Ok(size) => {
            text(estimate(s, &model, size)).key("size-summary").font(k.ty.small).color(if s.gpu_held_back() { c.warning.fg } else { c.text_muted }).wrap()
        }
        Err(e) => text(e.to_string()).key("size-error").role("alert").font(k.ty.small).color(c.danger.fg).wrap(),
    };
    out.child(summary)
}

fn seed(s: &State, k: &Kit) -> El {
    let invalid = s.seed_value().is_none();
    let field = TextInput::new(&s.seed)
        .label("Seed")
        .placeholder("Random")
        .invalid(invalid)
        .on_edit(|s: &mut State| &mut s.seed)
        .build(k)
        .key("seed")
        .grow(1.0)
        .shrink()
        .min_w(0.0);
    let dice = IconButton::new(&icons::DICE)
        .variant(IconButtonVariant::Ghost)
        .control_size(ControlSize::Large)
        .label("Randomise seed")
        .help("Randomise seed")
        .on_press(|s: &mut State| s.randomise_seed())
        .build(k)
        .key("randomise-seed");
    let mut out = col().w_full().gap(8.0).child(section("Seed", k)).child(row().w_full().gap(6.0).center_y().child(field).child(dice));
    if invalid {
        out = out.child(text("The seed must be a whole number.").key("seed-error").font(k.ty.small).color(k.c.danger.fg));
    }
    out
}

fn reference_tile(i: usize, r: &Reference, k: &Kit) -> El {
    let c = &k.c;
    let n = i + 1;
    let name = r.path.file_name().map_or_else(|| r.path.display().to_string(), |n| n.to_string_lossy().into_owned());
    let mut picture = col().key("reference-thumb").w_full().aspect_ratio(1.0).radius(6.0).fill(c.layer_02).border(0.5, c.border_muted).center();
    picture = match &r.thumb {
        Thumb::Ready(img) => picture.child(el().key("reference-image").w_full().h_full().pixels_fit(img.pixels(), SurfaceFit::Fill)),
        Thumb::Loading => picture.child(Spinner::new().size(12.0).color(c.icon_faint).build(k)),
        Thumb::Failed(why) => picture.border(1.0, c.danger.icon).child(text("!").font(k.ty.ui_medium).color(c.danger.fg)).help(why.clone()),
    };
    let remove = IconButton::new(&icons::CLOSE)
        .variant(IconButtonVariant::Contrast)
        .control_size(ControlSize::Mini)
        .label(format!("Remove picture {n}"))
        .help(format!("Remove {name}"))
        .on_press(move |s: &mut State| s.remove_reference(i))
        .build(k)
        .key("reference-remove")
        .pin(None, Some(4.0), Some(4.0), None);
    let tag = Button::new(picture_tag(n))
        .variant(ButtonVariant::GhostMuted)
        .control_size(ControlSize::Mini)
        .help(format!("Put {} in the prompt ({name})", picture_tag(n)))
        .on_press(move |s: &mut State| s.insert_picture_tag(n))
        .build(k)
        .key("reference-tag")
        .w_full();
    col()
        .key(format!("reference-{i}"))
        .role("listitem")
        .label(format!("Picture {n}, {name}"))
        .w_full()
        .min_w(0.0)
        .gap(2.0)
        .child(picture.child(remove))
        .child(tag)
}

fn references(s: &State, k: &Kit) -> El {
    let c = &k.c;
    let count = s.references.len();
    let header = row().w_full().center_y().justify_between().child(section("Pictures to edit", k)).child(
        text(if count == 0 { String::new() } else { format!("{count} of {MAX_REFERENCES}") }).key("reference-count").font(k.ty.small).color(c.text_faint),
    );
    let add = |label: &str| {
        Button::new(label)
            .variant(ButtonVariant::Outline)
            .control_size(ControlSize::Small)
            .icon(&icons::PLUS)
            .disabled(count >= MAX_REFERENCES)
            .on_press_tx(|s: &mut State, tx: &mut Tx| s.ask_for_references(tx))
            .build(k)
            .key("add-reference")
    };
    let mut zone = col().key("references").role("list").label("Pictures to edit").w_full().gap(10.0).padding(10.0).radius(8.0);
    zone = if s.dropping {
        zone.border(1.5, c.border_focus).fill(c.layer_02)
    } else if count == 0 {
        zone.border(1.0, c.border_base).border_dash(4.0, 3.0)
    } else {
        zone.border(1.0, c.border_muted)
    };
    if count == 0 {
        zone = zone.items_center().child(icon(&icons::PICTURE, 18.0).color(c.icon_faint)).child(
            text(format!("Drop PNG or JPEG pictures here to edit them, up to {MAX_REFERENCES}."))
                .key("reference-empty")
                .font(k.ty.small)
                .color(c.text_muted)
                .wrap()
                .align(rune::Align::Center),
        );
        zone = zone.child(add("Add picture…"));
    } else {
        let mut grid = row().key("reference-grid").w_full().equal_columns(3).gaps(8.0, 8.0);
        for (i, r) in s.references.iter().enumerate() {
            grid = grid.child(reference_tile(i, r, k));
        }
        zone = zone.child(grid).child(row().w_full().child(add("Add…")));
    }
    let mut out = col().w_full().gap(8.0).child(header).child(zone);
    if count > 0 {
        out = out.child(
            text("The prompt refers to them as <Picture 1>, <Picture 2> and so on. Click a tag to put it in the prompt.")
                .key("reference-hint")
                .font(k.ty.small)
                .color(c.text_faint)
                .wrap(),
        );
    }
    let missing = missing_pictures(&s.prompt.text, count);
    if !missing.is_empty() {
        let tags: Vec<String> = missing.iter().map(|&n| picture_tag(n)).collect();
        let what = if count == 0 { "no pictures are attached".to_owned() } else { format!("only {count} attached") };
        out = out.child(
            text(format!("The prompt mentions {}, but {what}.", tags.join(", ")))
                .key("reference-missing")
                .role("alert")
                .font(k.ty.small)
                .color(c.warning.fg)
                .wrap(),
        );
    }
    out
}

pub fn composer(s: &State, k: &Kit) -> El {
    let prompt = TextArea::new(&s.prompt)
        .label("Prompt")
        .placeholder("Describe the picture, or how to change the pictures below…")
        .width(None)
        .auto_grow(5, 14)
        .on_edit(|s: &mut State| &mut s.prompt)
        .on_key(|s: &mut State, ev: &KeyEvent, _tx: &mut Tx| {
            if is_generate(ev) {
                s.generate();
                return true;
            }
            if ev.key == Key::Enter && !ev.mods.cmd && !ev.mods.ctrl && !ev.mods.alt && !ev.mods.shift {
                s.prompt.insert("\n");
                return true;
            }
            false
        })
        .build(k)
        .key("prompt");
    let ready = s.request();
    let help = match &ready {
        Ok(_) => "Generate an image".to_owned(),
        Err(why) => why.clone(),
    };
    let generate = Button::new("Generate")
        .variant(ButtonVariant::Submit)
        .control_size(ControlSize::Large)
        .icon(&icons::SPARKLE)
        .shortcut(GENERATE_LABEL)
        .help(help)
        .disabled(ready.is_err())
        .on_press(|s: &mut State| s.generate())
        .build(k)
        .key("generate")
        .w_full();
    let active = s.active_count();
    let queue = match active {
        0 => "Nothing queued. Generate as often as you like; requests queue on the server.".to_owned(),
        1 => "1 generation in progress.".to_owned(),
        n => format!("{n} generations queued or running."),
    };
    col()
        .key("composer")
        .w_full()
        .h_full()
        .min_h(0.0)
        .padding(16.0)
        .gap(18.0)
        .scroll(rune::ScrollSpec { stick_bottom: false })
        .on_drop_files(|s: &mut State, files, _tx| s.add_references(files))
        .accepts_extensions(&REFERENCE_EXTENSIONS)
        .on_drop_hover(|s: &mut State, over, _tx| s.dropping = over)
        .on_drop_rejected(|s: &mut State, _files, _tx| s.note("mmh3 edits PNG and JPEG pictures.", None, true))
        .child(col().w_full().gap(8.0).child(section("Prompt", k)).child(prompt))
        .child(references(s, k))
        .child(sizes(s, k))
        .child(seed(s, k))
        .child(col().w_full().gap(8.0).child(generate).child(text(queue).key("queue-note").font(k.ty.small).color(k.c.text_faint).wrap()))
}

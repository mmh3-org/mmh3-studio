use rune::{Env, Key, KeyEvent, Mods, Runtime};

use super::*;

fn runtime(scene: fn(&mut State), dark: bool) -> Runtime<State> {
    let mut s = State::offline();
    scene(&mut s);
    let mut rt = Runtime::new(s, view::view, (1320.0, 860.0), Env { system_dark: dark, scale: 2.0 });
    rt.poll = Some(poll);
    rt.frame(0.0);
    for _ in 0..30 {
        rt.frame(1.0 / 60.0);
    }
    rt
}

fn text_of(rt: &Runtime<State>, key: &str) -> String {
    rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).and_then(|e| e.text).unwrap_or_default()
}

#[test]
fn every_scene_builds_in_both_appearances() {
    for (name, scene) in scenes::SCENES {
        for dark in [false, true] {
            let rt = runtime(*scene, dark);
            assert!(rt.find("status-bar").is_some(), "{name}: no status bar");
        }
    }
}

#[test]
fn the_disconnected_state_shows_how_to_start_the_server() {
    let rt = runtime(scenes::disconnected, false);
    assert!(rt.find("disconnected").is_some());
    assert!(rt.find("start-command").is_some());
    assert!(text_of(&rt, "status-connection").starts_with("Not connected"));
}

#[test]
fn cmd_return_in_the_prompt_generates_and_return_types_a_newline() {
    let mut rt = runtime(scenes::empty, false);
    let (x, y) = rt.find("prompt").unwrap();
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    rt.frame(1.0 / 60.0);
    rt.update(|s, _| s.prompt.replace_text("a fox"));
    rt.frame(1.0 / 60.0);
    rt.key(KeyEvent { key: Key::Enter, mods: Mods::default() });
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.state.prompt.text, "a fox\n");
    assert!(rt.state.gens.is_empty());
    rt.key(KeyEvent { key: Key::Enter, mods: Mods { cmd: true, ..Mods::default() } });
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.state.gens.len(), 1);
    assert_eq!(rt.state.gens[0].prompt(), "a fox");
}

#[test]
fn quality_and_aspect_ratio_pick_the_size_together() {
    let mut rt = runtime(scenes::empty, false);
    assert_eq!(rt.state.chosen_size(), Ok((2048, 2048)));
    tap(&mut rt, "aspect-6");
    assert_eq!(rt.state.chosen_size(), Ok((1536, 2720)));
    tap(&mut rt, "quality-final");
    assert_eq!(rt.state.chosen_size(), Ok((3072, 5472)));
    tap(&mut rt, "aspect-7");
    assert_eq!(rt.state.chosen_size(), Ok((6272, 2688)));
    assert!(text_of(&rt, "size-summary").starts_with("6272 × 2688 · 16.9 MP · ≈ "), "{}", text_of(&rt, "size-summary"));
}

#[test]
fn the_custom_size_starts_from_the_picked_size_and_a_pick_closes_it() {
    let mut rt = runtime(scenes::empty, false);
    tap(&mut rt, "aspect-5");
    tap(&mut rt, "custom-toggle");
    assert!(rt.state.custom_size);
    assert_eq!((rt.state.custom_w.text.as_str(), rt.state.custom_h.text.as_str()), ("2720", "1536"));
    assert!(rt.find("custom-width").is_some());
    tap(&mut rt, "aspect-0");
    assert!(!rt.state.custom_size);
    assert!(rt.find("custom-width").is_none());
    assert_eq!(rt.state.chosen_size(), Ok((2048, 2048)));
}

#[test]
fn the_estimate_says_a_held_back_gpu_may_be_slower() {
    let rt = runtime(scenes::power, false);
    assert!(text_of(&rt, "size-summary").ends_with("· may be slower"), "{}", text_of(&rt, "size-summary"));
    let rt = runtime(scenes::empty, false);
    assert!(!text_of(&rt, "size-summary").contains("slower"));
}

#[test]
fn the_custom_size_warns_until_it_is_a_multiple_of_32() {
    let mut rt = runtime(scenes::custom_size, false);
    assert!(text_of(&rt, "size-error").contains("multiples of 32"));
    rt.update(|s, _| {
        s.custom_w.replace_text("6016");
        s.custom_h.replace_text("2016");
    });
    for _ in 0..3 {
        rt.frame(1.0 / 60.0);
    }
    assert!(rt.find("size-error").is_none());
    assert_eq!(rt.state.chosen_size(), Ok((6016, 2016)));
}

#[test]
fn running_rows_show_their_step_and_the_preview_its_progress() {
    let rt = runtime(scenes::running, false);
    assert!(rt.find("progress-card").is_some());
    assert!(text_of(&rt, "progress-detail").ends_with("elapsed"));
}

#[test]
fn an_upstream_server_s_running_picture_has_no_preview_and_no_stop() {
    let rt = runtime(scenes::running, false);
    assert!(rt.find("live-preview").is_none());
    assert!(rt.find("stop").is_none() && rt.find("stop-tool").is_none());
}

#[test]
fn a_running_picture_shows_its_latest_preview_and_stops_on_request() {
    let mut rt = runtime(scenes::previewing, false);
    assert!(rt.find("live-preview").is_some());
    assert_eq!(text_of(&rt, "preview-step-text"), "Preview · step 3/6");
    assert!(rt.find("step-badge").is_some(), "the row's thumbnail says its step");
    tap(&mut rt, "stop");
    let g = rt.state.item("7c01aa2e0005").unwrap();
    assert!(g.stopping);
    for _ in 0..3 {
        rt.frame(1.0 / 60.0);
    }
    assert_eq!(text_of(&rt, "progress-headline"), "Stopping after this step…");
    assert!(rt.find("stop").is_some(), "the button stays, saying it is stopping");
}

#[test]
fn a_cancelled_picture_shows_how_far_it_got() {
    let rt = runtime(scenes::cancelled, false);
    assert_eq!(text_of(&rt, "cancelled-headline"), "Cancelled after 4 steps");
    assert!(rt.find("live-preview").is_some());
    assert!(rt.find("retry").is_some());
    assert!(rt.find("stop-tool").is_none());
}

#[test]
fn the_gallery_scene_shows_the_preview_stage_and_how_long_it_took() {
    let rt = runtime(scenes::gallery, false);
    assert!(rt.find(stage::KEY).is_some());
    assert!(rt.find("took").is_some());
    assert!(text_of(&rt, "facts-text").starts_with("5472 × 3072 · 16.8 MP · seed 4127"));
    assert!(rt.find("details").is_none(), "nothing is left under the picture");
}

#[test]
fn the_time_opens_where_it_went_and_a_press_outside_closes_it() {
    let mut rt = runtime(scenes::gallery, false);
    assert!(rt.find("timing").is_none());
    tap(&mut rt, "took");
    assert!(rt.state.timing_open);
    assert!(rt.find("timing-bar").is_some());
    let rows = rt.elements().into_iter().filter_map(|e| e.text).collect::<Vec<_>>();
    assert!(rows.iter().any(|t| t == "6 steps, 37.9 s each"), "{rows:?}");
    tap(&mut rt, "timing-scrim");
    assert!(!rt.state.timing_open);
}

#[test]
fn without_stage_timings_the_details_say_the_split_is_an_estimate() {
    let mut rt = runtime(scenes::gallery, false);
    rt.update(|s, _| {
        s.select("7c01aa2e0002");
        s.timing_open = true;
    });
    for _ in 0..3 {
        rt.frame(1.0 / 60.0);
    }
    assert!(text_of(&rt, "timing-note").contains("only the total"));
}

#[test]
fn the_zoom_controls_in_the_toolbar_zoom_show_the_level_and_fit() {
    let mut rt = runtime(scenes::gallery, false);
    let fitted = rt.state.zoom.level();
    assert!(fitted < 100, "a 16 MP picture fits below its pixels: {fitted}%");
    tap(&mut rt, "zoom-in");
    assert!(rt.state.zoom.level() > fitted);
    tap(&mut rt, "zoom-level");
    assert_eq!(rt.state.zoom.level(), 100);
    tap(&mut rt, "zoom-fit");
    assert_eq!(rt.state.zoom.level(), fitted);
    assert!(rt.find("preview-controls").is_none(), "no controls float over the picture");
}

#[test]
fn gallery_thumbnails_are_decoded() {
    let rt = runtime(scenes::gallery, false);
    for g in &rt.state.gens {
        match &g.picture {
            state::Picture::Ready { thumb: Some(t), .. } => {
                assert!(t.is_ready() && t.error().is_none() && t.size().is_some(), "{}: {:?} {:?}", g.id, t.size(), t.error());
            }
            _ => panic!("{} has no thumbnail", g.id),
        }
    }
}

fn focused(rt: &Runtime<State>) -> Option<String> {
    rt.elements().into_iter().find(|e| e.focused).and_then(|e| e.key)
}

fn tap(rt: &mut Runtime<State>, key: &str) {
    let (x, y) = rt.find(key).unwrap_or_else(|| panic!("no {key}"));
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    rt.frame(1.0 / 60.0);
}

#[test]
fn cmd_return_while_disconnected_adds_nothing() {
    let mut rt = runtime(scenes::disconnected, false);
    tap(&mut rt, "prompt");
    rt.update(|s, _| s.prompt.replace_text("a fox"));
    rt.frame(1.0 / 60.0);
    rt.key(KeyEvent { key: Key::Enter, mods: Mods { cmd: true, ..Mods::default() } });
    rt.frame(1.0 / 60.0);
    assert!(rt.state.gens.is_empty());
    assert_eq!(rt.state.notes.last().map(|n| n.title.as_str()), Some("Start mmh3 server first."));
}

#[test]
fn a_note_from_the_background_stays_up_after_an_idle_spell() {
    let mut rt = runtime(scenes::empty, false);
    rt.update(|s, _| s.note("Couldn't delete the generation.", None, true));
    rt.frame(60.0);
    assert_eq!(rt.state.notes.len(), 1, "a note isn't timed from a clock the window left behind");
    rt.frame(state::NOTE_SECONDS + 0.1);
    assert!(rt.state.notes.is_empty());
}

#[test]
fn the_aspect_ratios_are_one_tab_stop_and_arrows_move_between_them() {
    let mut rt = runtime(scenes::empty, false);
    tap(&mut rt, "aspect-0");
    assert_eq!(focused(&rt).as_deref(), Some("aspect-0"));
    rt.key(KeyEvent { key: Key::Right, mods: Mods::default() });
    rt.frame(1.0 / 60.0);
    assert_eq!(focused(&rt).as_deref(), Some("aspect-1"));
    rt.key(KeyEvent { key: Key::Tab, mods: Mods::default() });
    rt.frame(1.0 / 60.0);
    assert!(!focused(&rt).is_some_and(|k| k.starts_with("aspect-")), "Tab leaves the group: {:?}", focused(&rt));
}

#[test]
fn delete_asks_before_it_removes_anything() {
    let mut rt = runtime(scenes::gallery, false);
    let before = rt.state.gens.len();
    tap(&mut rt, "delete");
    for _ in 0..20 {
        rt.frame(1.0 / 60.0);
    }
    assert!(rt.find("delete-dialog").is_some());
    assert_eq!(rt.state.gens.len(), before);
    tap(&mut rt, "delete-cancel");
    assert_eq!(rt.state.deleting, None);
    assert_eq!(rt.state.gens.len(), before);
    tap(&mut rt, "delete");
    for _ in 0..20 {
        rt.frame(1.0 / 60.0);
    }
    tap(&mut rt, "delete-confirm");
    assert_eq!(rt.state.gens.len(), before - 1);
}

#[test]
fn the_status_bar_leaves_out_memory_the_server_could_not_measure() {
    let mut rt = runtime(scenes::empty, false);
    rt.update(|s, _| {
        if let state::Conn::Connected(st) = &mut s.conn {
            st.memory = None;
        }
    });
    rt.frame(1.0 / 60.0);
    assert!(rt.find("status-memory").is_none());
    assert!(text_of(&rt, "status-connection").starts_with("Connected"));
}

#[test]
fn the_offline_banner_offers_the_same_start_command_as_the_card() {
    let rt = runtime(scenes::offline_selected, false);
    assert!(rt.find("copy-start-command").is_some());
    assert!(!text_of(&rt, "offline-note").contains('`'));
}

#[test]
fn placeholder_cards_stay_inside_their_frame_at_the_smallest_window() {
    for scene in [scenes::failed as fn(&mut State), scenes::running] {
        let mut s = State::offline();
        scene(&mut s);
        let mut rt = Runtime::new(s, view::view, (1040.0, 680.0), Env { system_dark: false, scale: 1.0 });
        rt.poll = Some(poll);
        for _ in 0..30 {
            rt.frame(1.0 / 60.0);
        }
        let rect = |key: &str| rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).map(|e| e.rect).unwrap();
        let (canvas, frame) = (rect("canvas"), rect("frame"));
        assert!(frame[0] >= canvas[0] && frame[0] + frame[2] <= canvas[0] + canvas[2], "frame {frame:?} spills out of {canvas:?}");
        let card = rt.elements().into_iter().find(|e| matches!(e.key.as_deref(), Some("failed" | "progress-card"))).unwrap().rect;
        assert!(card[0] >= frame[0] && card[0] + card[2] <= frame[0] + frame[2], "card {card:?} spills out of {frame:?}");
    }
}

#[test]
fn the_picture_toolbar_fits_its_pane_at_every_window_width() {
    for width in [720.0, 760.0, 900.0, 1000.0, 1040.0, 1320.0] {
        let mut s = State::offline();
        scenes::timing(&mut s);
        let mut rt = Runtime::new(s, view::view, (width, 640.0), Env { system_dark: false, scale: 1.0 });
        rt.poll = Some(poll);
        for _ in 0..30 {
            rt.frame(1.0 / 60.0);
        }
        let rect = |key: &str| rt.elements().into_iter().find(|e| e.key.as_deref() == Some(key)).map(|e| e.rect);
        let bar = rect("picture-bar").unwrap_or_else(|| panic!("{width}: no picture bar"));
        for key in ["picture-actions", "zoom-controls", "delete", "zoom-fit"] {
            let r = rect(key).unwrap_or_else(|| panic!("{width}: no {key}"));
            assert!(r[0] >= bar[0] && r[0] + r[2] <= bar[0] + bar[2], "{width}: {key} {r:?} spills out of the bar {bar:?}");
        }
        let actions = rect("picture-actions").unwrap();
        let zoom = rect("zoom-controls").unwrap();
        assert_eq!(actions[1], zoom[1], "{width}: the actions and the zoom share a row, so no divider is left hanging");
        assert_eq!(rt.find("pane-picker").is_some(), width < 1040.0, "{width}");
    }
}

fn picture_files(n: usize) -> (std::path::PathBuf, Vec<std::path::PathBuf>) {
    let dir = std::env::temp_dir().join(format!("mmh3-studio-drop-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let files = (0..n)
        .map(|i| {
            let file = dir.join(format!("p{i}.png"));
            std::fs::write(&file, mock::paint(64, 64, i as u64)).unwrap();
            file
        })
        .collect();
    (dir, files)
}

#[test]
fn pictures_dropped_on_the_composer_are_attached_in_order_and_sent_with_the_request() {
    let mut rt = runtime(scenes::empty, false);
    assert!(rt.find("reference-empty").is_some());
    let (dir, files) = picture_files(2);
    let at = rt.find("references").unwrap();
    rt.drop_files(files.clone(), Some(at));
    rt.frame(1.0 / 60.0);
    let attached: Vec<_> = rt.state.references.iter().map(|r| r.path.clone()).collect();
    assert_eq!(attached, files);
    assert_eq!(text_of(&rt, "reference-count"), "2 of 16");
    rt.update(|s, _| s.prompt.replace_text("Put "));
    rt.frame(1.0 / 60.0);
    let tags: Vec<_> = rt.elements().into_iter().filter(|e| e.key.as_deref() == Some("reference-tag")).map(|e| e.rect).collect();
    let second = tags[1];
    rt.pointer_down(second[0] + second[2] / 2.0, second[1] + second[3] / 2.0);
    rt.pointer_up(second[0] + second[2] / 2.0, second[1] + second[3] / 2.0);
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.state.prompt.text, "Put <Picture 2>");
    assert_eq!(rt.state.request().unwrap().references, files);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_dropped_file_that_is_not_a_picture_is_refused_with_a_reason() {
    let mut rt = runtime(scenes::empty, false);
    let at = rt.find("references").unwrap();
    rt.drop_files(vec!["/tmp/notes.txt".into()], Some(at));
    rt.frame(1.0 / 60.0);
    assert!(rt.state.references.is_empty());
    assert_eq!(rt.state.notes.last().map(|n| n.title.as_str()), Some("mmh3 edits PNG and JPEG pictures."));
}

#[test]
fn no_more_than_sixteen_pictures_are_attached() {
    let mut rt = runtime(scenes::empty, false);
    let (dir, files) = picture_files(17);
    rt.update(|s, _| s.add_references(&files));
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.state.references.len(), 16);
    assert_eq!(rt.state.notes.last().and_then(|n| n.detail.as_deref()), Some("1 more was left out."));
    rt.update(|s, _| s.add_references(&files[..1]));
    assert_eq!(rt.state.references.len(), 16, "an attached picture is not attached twice");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn removing_a_picture_warns_about_the_tags_it_leaves_behind() {
    let mut rt = runtime(scenes::references, false);
    assert!(rt.find("reference-missing").is_none());
    let removes: Vec<_> = rt.elements().into_iter().filter(|e| e.key.as_deref() == Some("reference-remove")).map(|e| e.rect).collect();
    let last = removes[2];
    rt.pointer_down(last[0] + last[2] / 2.0, last[1] + last[3] / 2.0);
    rt.pointer_up(last[0] + last[2] / 2.0, last[1] + last[3] / 2.0);
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.state.references.len(), 2);
    assert_eq!(text_of(&rt, "reference-missing"), "The prompt mentions <Picture 3>, but only 2 attached.");
}

#[test]
fn a_picture_that_cannot_be_read_holds_generate_back() {
    let mut rt = runtime(scenes::references, false);
    let path = rt.state.references[0].path.clone();
    rt.update(move |s, _| s.reference_thumb(&path, Err("unsupported format".into())));
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.state.request().unwrap_err(), "street.jpg isn't a picture mmh3 can read. Remove it first.");
}

#[test]
fn edit_this_picture_attaches_the_finished_png() {
    let mut rt = runtime(scenes::gallery, false);
    tap(&mut rt, "edit-picture");
    assert_eq!(rt.state.references.len(), 1);
    assert_eq!(rt.state.references[0].path, rt.state.selected_gen().and_then(|g| g.file().cloned()).unwrap());
}

#[test]
fn a_phone_wide_window_shows_one_pane_at_a_time() {
    let mut s = State::offline();
    scenes::gallery(&mut s);
    let mut rt = Runtime::new(s, view::view, (390.0, 844.0), Env { system_dark: false, scale: 3.0 });
    for _ in 0..5 {
        rt.frame(1.0 / 60.0);
    }
    assert!(rt.find("pane-picker").is_some());
    assert!(rt.find("main").is_some(), "the picture shows first");
    assert!(rt.find("composer").is_none() && rt.find("gallery-header").is_none());
    rt.update(|s, _| s.pane = state::Pane::Compose);
    rt.frame(1.0 / 60.0);
    assert!(rt.find("composer").is_some() && rt.find("main").is_none());
    rt.update(|s, _| s.pane = state::Pane::Gallery);
    rt.frame(1.0 / 60.0);
    let (x, y) = rt.find("7c01aa2e0002").unwrap();
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.state.pane, state::Pane::Picture, "choosing a picture shows it");
    assert_eq!(rt.state.selected.as_deref(), Some("7c01aa2e0002"));

    let wide = runtime(scenes::gallery, false);
    assert!(wide.find("pane-picker").is_none());
    assert!(wide.find("composer").is_some() && wide.find("main").is_some());
}

#[test]
fn the_power_banner_names_the_causes_offers_the_fixes_and_stays_closed() {
    let mut rt = runtime(scenes::power, false);
    let clipboard = rune::clipboard::memory();
    rt.use_clipboard(&clipboard);
    let detail = text_of(&rt, "power-detail");
    assert!(detail.contains("Energy Mode is Automatic"), "{detail}");
    assert!(detail.contains("90 W adapter"), "{detail}");
    assert!(detail.contains("whatever the adapter"), "{detail}");
    assert!(detail.contains("bun is using a lot of CPU"), "{detail}");
    assert!(text_of(&rt, "status-power-text").contains("Automatic power, 90 W adapter"));
    let (x, y) = rt.find("power-copy-command").unwrap();
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    rt.frame(1.0 / 60.0);
    assert_eq!(clipboard.text().as_deref().map(String::as_str), Some(power::HIGH_POWER_COMMAND));
    let (x, y) = rt.find("power-dismiss").unwrap();
    rt.pointer_down(x, y);
    rt.pointer_up(x, y);
    rt.frame(1.0 / 60.0);
    assert!(rt.find("power-banner").is_none());
    assert!(rt.find("status-power").is_some(), "the status bar still says");
    rt.update(|s, _| {
        if let Some(p) = &mut s.power {
            p.source = Some(power::Source::Battery);
        }
    });
    rt.frame(1.0 / 60.0);
    assert!(rt.find("power-banner").is_some(), "a new cause brings it back");
}

#[test]
fn a_busy_process_alone_names_itself_without_the_low_clock_warning() {
    let mut rt = runtime(scenes::power, false);
    rt.update(|s, _| {
        s.power = Some(power::Power {
            mode: Some(power::Mode::High),
            high_available: true,
            chip: power::Chip::Large { capped_case: false },
            source: Some(power::Source::Adapter { watts: Some(96), draining: false }),
            busy: Some("bun".into()),
            ..Default::default()
        });
    });
    rt.frame(1.0 / 60.0);
    assert_eq!(text_of(&rt, "power-detail"), "bun is using a lot of CPU, which takes power from the GPU; closing it can speed up generation.");
    assert!(rt.find("power-copy-command").is_none());
}

#[test]
fn a_small_adapter_alone_does_not_claim_a_low_clock() {
    let mut rt = runtime(scenes::power, false);
    rt.update(|s, _| {
        s.power = Some(power::Power {
            mode: Some(power::Mode::High),
            high_available: true,
            chip: power::Chip::Large { capped_case: true },
            source: Some(power::Source::Adapter { watts: Some(90), draining: false }),
            ..Default::default()
        });
    });
    rt.frame(1.0 / 60.0);
    let detail = text_of(&rt, "power-detail");
    assert!(detail.contains("whatever the adapter"), "{detail}");
    assert!(!detail.contains("low clock") && !detail.contains("two to three times"), "{detail}");
    assert_eq!(text_of(&rt, "power-title"), "Generations may run a little slower");
    assert!(rt.find("power-copy-command").is_none());
}

#[test]
fn a_mac_at_full_speed_shows_no_banner_and_no_command() {
    let mut rt = runtime(scenes::power, false);
    rt.update(|s, _| {
        s.power = Some(power::Power {
            mode: Some(power::Mode::High),
            high_available: true,
            chip: power::Chip::Large { capped_case: false },
            source: Some(power::Source::Adapter { watts: Some(140), draining: false }),
            ..Default::default()
        });
    });
    rt.frame(1.0 / 60.0);
    assert!(rt.find("power-banner").is_none());
    assert_eq!(text_of(&rt, "status-power-text"), "High Power Mode, 140 W adapter");
}

#[test]
fn the_effort_starts_at_the_server_s_defaults_and_low_sends_what_helps_at_the_size() {
    use crate::api::{Attention, Tuning};
    use crate::effort::Effort;
    let mut rt = runtime(scenes::empty, false);
    rt.update(|s, _| s.prompt.replace_text("a fox"));
    tap(&mut rt, "quality-final");
    let request = rt.state.request().unwrap();
    assert_eq!((request.effort, request.tuning), (Some(Effort::High), Tuning::default()), "High sends nothing");
    let high = text_of(&rt, "size-summary");
    tap(&mut rt, "effort-low");
    assert_eq!(rt.state.request().unwrap().tuning, Tuning { attention: Some(Attention::Sol), draft_steps: Some(2), ..Tuning::default() });
    assert!(text_of(&rt, "effort-hint").contains("another composition"), "{}", text_of(&rt, "effort-hint"));
    assert_ne!(text_of(&rt, "size-summary"), high, "the estimate follows the effort");
    tap(&mut rt, "aspect-1");
    assert_eq!(rt.state.request().unwrap().tuning, Tuning { attention: Some(Attention::Sol), ..Tuning::default() }, "4736 × 3552 takes no draft");
    assert!(text_of(&rt, "effort-hint").contains("multiples of 64"), "{}", text_of(&rt, "effort-hint"));
}

#[test]
fn medium_says_it_is_high_where_sol_attn_has_too_few_tokens() {
    let rt = runtime(scenes::effort_small, false);
    assert!(rt.state.request().unwrap().tuning.is_empty());
    assert!(text_of(&rt, "effort-hint").starts_with("Same as High at this size"), "{}", text_of(&rt, "effort-hint"));
}

#[test]
fn custom_shows_only_what_applies_and_holds_generate_back_on_a_bad_threshold() {
    let mut rt = runtime(scenes::effort_custom, false);
    for key in ["custom-attention", "custom-sol-from", "custom-tau", "custom-draft", "custom-precision"] {
        assert!(rt.find(key).is_some(), "no {key} at 4096 × 4096");
    }
    let sent = rt.state.request().unwrap().tuning;
    assert_eq!((sent.sparse_start, sent.draft_steps), (Some(0.5), Some(2)));
    rt.update(|s, _| s.sparse_tau.replace_text("fast"));
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.state.request().unwrap_err(), "Sol-Attn's tau must be a positive number.");
    rt.update(|s, _| s.sparse_tau.replace_text("1.5"));
    rt.frame(1.0 / 60.0);
    assert_eq!(rt.state.request().unwrap().tuning.sparse_tau, Some(1.5));
    tap(&mut rt, "quality-draft");
    assert!(rt.find("custom-attention").is_none() && rt.find("custom-no-sol").is_some(), "2048 × 2048 has too few tokens for Sol-Attn");
    assert!(rt.find("custom-draft").is_some());
    let sent = rt.state.request().unwrap().tuning;
    assert_eq!((sent.attention, sent.sparse_tau, sent.draft_steps), (None, None, Some(2)), "what does not apply is not sent");
}

#[test]
fn a_picture_s_row_and_details_say_what_effort_it_was_asked_at() {
    let rt = runtime(scenes::effort_details, false);
    assert!(text_of(&rt, "facts-text").ends_with("Medium effort"), "{}", text_of(&rt, "facts-text"));
    assert!(rt.find("asked-for").is_some());
    assert!(text_of(&rt, "asked-note").starts_with("The server reports the attention"), "{}", text_of(&rt, "asked-note"));
    let statuses: Vec<String> = rt.elements().into_iter().filter(|e| e.key.as_deref() == Some("row-status")).filter_map(|e| e.text).collect();
    assert!(statuses.iter().any(|t| t == "5472×3072 · Medium · 241.8 s"), "{statuses:?}");
    assert!(statuses.iter().any(|t| t == "2048×2048 · 38.7 s"), "High says nothing: {statuses:?}");
}

#[test]
fn use_prompt_loads_the_selected_picture_s_prompt_and_seed_into_the_composer() {
    let mut rt = runtime(scenes::gallery, false);
    let g = rt.state.selected_gen().unwrap();
    let (prompt, seed, size) = (g.prompt().to_owned(), g.seed().unwrap().to_string(), g.size().unwrap());
    tap(&mut rt, "use-prompt");
    assert_eq!((rt.state.prompt.text.as_str(), rt.state.seed.text.as_str()), (prompt.as_str(), seed.as_str()));
    assert_eq!(rt.state.chosen_size(), Ok(size));
    assert!(rt.find("prompt").is_some());
}

#[test]
fn cmd_return_with_a_count_of_four_generates_four_pictures_with_seeds_one_after_another() {
    let mut rt = runtime(scenes::empty, false);
    rt.update(|s, _| {
        s.prompt.replace_text("a fox");
        s.seed.replace_text("41");
    });
    rt.frame(1.0 / 60.0);
    tap(&mut rt, "count-3");
    assert_eq!(rt.state.count, 4);
    tap(&mut rt, "prompt");
    rt.key(KeyEvent { key: Key::Enter, mods: Mods { cmd: true, ..Mods::default() } });
    rt.frame(1.0 / 60.0);
    let seeds: Vec<u64> = rt.state.gens.iter().filter_map(state::Gen::seed).collect();
    assert_eq!(seeds, [44, 43, 42, 41], "sent in order, newest first");
    assert_eq!(rt.state.selected_gen().and_then(state::Gen::seed), Some(41));
}

#[test]
fn the_generate_button_and_count_note_say_how_many_and_which_seeds() {
    let mut rt = runtime(scenes::empty, false);
    rt.update(|s, _| {
        s.prompt.replace_text("a fox");
        s.seed.replace_text("4294967295");
    });
    rt.frame(1.0 / 60.0);
    let button = |rt: &Runtime<State>| {
        let e = rt.elements().into_iter().find(|e| e.key.as_deref() == Some("generate")).unwrap();
        (e.label.unwrap_or_default(), e.help.unwrap_or_default())
    };
    let (label, help) = button(&rt);
    assert_eq!(label, "Generate");
    assert!(help.contains("Generate an image"), "{help}");
    assert!(rt.find("count-note").is_none(), "one picture needs no note");

    tap(&mut rt, "count-3");
    let (label, help) = button(&rt);
    assert_eq!(label, "Generate 4");
    assert!(help.contains("Generate 4 images"), "{help}");
    let note = text_of(&rt, "count-note");
    assert!(note.starts_with("4 images, seeds 4294967295 to 2, ≈ ") && note.ends_with(" in all."), "{note}");

    rt.update(|s, _| s.seed.replace_text(""));
    rt.frame(1.0 / 60.0);
    assert!(text_of(&rt, "count-note").starts_with("4 images, each with a random seed, ≈ "), "{}", text_of(&rt, "count-note"));
}

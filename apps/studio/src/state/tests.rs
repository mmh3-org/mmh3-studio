use crate::api::{self, Generation};

use super::*;

fn online() -> State {
    let mut s = State::offline();
    s.conn = Conn::Connected(Status::default());
    s
}

fn server(id: &str, state: api::State) -> Generation {
    Generation { id: id.into(), queued: 1, state, ..Generation::default() }
}

/// A picture as the fork's server says it, with what it asked for.
fn fork(id: &str, state: api::State, prompt: &str) -> Generation {
    let request = Asked { prompt: Some(prompt.into()), width: Some(1024), height: Some(768), seed: Some(9), ..Asked::default() };
    Generation { kind: Some(Kind::Image), request: Some(request), ..server(id, state) }
}

fn running(id: &str, step: u32) -> Generation {
    Generation { step: Some(step), steps: Some(6), ..server(id, api::State::Running) }
}

fn asked(prompt: &str) -> ImageRequest {
    ImageRequest { prompt: prompt.into(), seed: 7, width: 2048, height: 2048, min_generation_area: None, references: Vec::new() }
}

/// A state that asked for each of `ids` itself.
fn knowing(mut s: State, ids: &[&str]) -> State {
    for id in ids {
        s.requests.insert((*id).to_owned(), asked("a fox"));
    }
    s
}

#[test]
fn running_keeps_the_time_it_started_and_done_asks_for_the_picture() {
    let mut phase = Phase::Queued;
    assert!(step(&mut phase, &server("a", api::State::Running), 1.0).is_empty());
    assert_eq!(phase, Phase::Running { step: None, steps: 6, since: 1.0 }, "running before the first step reads the prompt");
    let _ = step(&mut phase, &running("a", 1), 2.0);
    assert_eq!(phase, Phase::Running { step: Some(1), steps: 6, since: 1.0 });
    let _ = step(&mut phase, &running("a", 4), 5.0);
    assert_eq!(phase, Phase::Running { step: Some(4), steps: 6, since: 1.0 });

    let done = Generation { seconds: Some(8.4), ..server("a", api::State::Done) };
    let fx: Vec<Effect> = step(&mut phase, &done, 9.0).into_iter().collect();
    assert_eq!(fx, [Effect::FetchImage]);
    assert_eq!(phase, Phase::Done { seconds: 8.4, timings: None });
    assert!(step(&mut phase, &done, 9.5).is_empty(), "a repeated end asks for nothing");
}

#[test]
fn a_cancelled_generation_keeps_how_far_it_got() {
    let mut phase = Phase::Running { step: Some(2), steps: 6, since: 0.0 };
    let cancelled = Generation { preview_step: Some(2), ..server("a", api::State::Cancelled) };
    assert!(step(&mut phase, &cancelled, 1.0).is_empty(), "a cancelled picture has nothing to fetch");
    assert_eq!(phase, Phase::Cancelled { step: Some(2) });
    assert!(!phase.is_active());
    let _ = step(&mut phase, &server("a", api::State::Cancelled), 1.0);
    assert_eq!(phase, Phase::Cancelled { step: None }, "cancelled before its first step");
}

#[test]
fn a_failure_keeps_its_reason() {
    let mut phase = Phase::Queued;
    let failed = Generation { error: Some("out of memory".into()), ..server("a", api::State::Failed) };
    let _ = step(&mut phase, &failed, 0.0);
    assert_eq!(phase, Phase::Failed("out of memory".into()));
}

#[test]
fn submitting_adds_a_selected_placeholder_that_takes_the_server_id() {
    let mut s = online();
    s.prompt.replace_text("a fox");
    s.generate();
    assert_eq!(s.gens.len(), 1);
    let local = s.gens[0].id.clone();
    assert!(local.starts_with("local-"));
    assert_eq!(s.selected.as_deref(), Some(local.as_str()));
    s.submitted(&local, Ok(server("srv1", api::State::Queued)), 1.0);
    assert_eq!(s.gens[0].id, "srv1");
    assert_eq!(s.gens[0].phase, Phase::Queued);
    assert_eq!(s.selected.as_deref(), Some("srv1"));
    assert_eq!(s.requests.get("srv1").map(|r| r.prompt.as_str()), Some("a fox"), "the studio remembers what it asked for");
}

#[test]
fn a_refused_submission_fails_with_the_servers_words() {
    let mut s = online();
    s.prompt.replace_text("a fox");
    s.generate();
    let local = s.gens[0].id.clone();
    s.submitted(&local, Err(api::Error::Refused { status: 400, message: "width: bad".into() }), 1.0);
    assert_eq!(s.gens[0].phase, Phase::Failed("width: bad (HTTP 400)".into()));
}

#[test]
fn merging_the_list_keeps_local_placeholders_and_drops_what_the_studio_did_not_ask_for_and_vanished_items() {
    let mut s = knowing(online(), &["a", "b"]);
    s.prompt.replace_text("pending");
    s.generate();
    s.merge(&[running("b", 2), server("a", api::State::Done), server("v", api::State::Done)], 3.0);
    let ids: Vec<&str> = s.gens.iter().map(|g| g.id.as_str()).collect();
    assert_eq!(ids, ["local-1", "b", "a"]);
    s.merge(&[running("b", 3)], 4.0);
    let ids: Vec<&str> = s.gens.iter().map(|g| g.id.as_str()).collect();
    assert_eq!(ids, ["local-1", "b"]);
    assert_eq!(s.gens[1].phase, Phase::Running { step: Some(3), steps: 6, since: 3.0 });
    assert!(!s.requests.contains_key("a"), "what the server forgot, the studio forgets");
}

#[test]
fn a_running_generation_is_not_deleted() {
    let mut s = knowing(State::offline(), &["a", "b"]);
    s.merge(&[running("b", 2), server("a", api::State::Done)], 0.0);
    s.delete("b");
    assert_eq!(s.gens.len(), 2);
    assert_eq!(s.notes.len(), 1);
    assert_eq!(s.deleting, None);
}

#[test]
fn deleting_asks_first_and_cancelling_keeps_the_generation() {
    let mut s = knowing(State::offline(), &["a"]);
    s.merge(&[server("a", api::State::Done)], 0.0);
    s.delete("a");
    assert_eq!(s.deleting.as_deref(), Some("a"));
    assert_eq!(s.gens.len(), 1, "nothing is deleted before it is confirmed");
    s.deleting = None;
    s.confirm_delete();
    assert_eq!(s.gens.len(), 1);
    s.delete("a");
    s.confirm_delete();
    assert!(s.gens.is_empty());
    assert_eq!(s.deleting, None);
}

#[test]
fn a_failed_placeholder_that_never_reached_the_server_goes_at_once() {
    let mut s = online();
    s.prompt.replace_text("a fox");
    s.generate();
    let local = s.gens[0].id.clone();
    s.submitted(&local, Err(api::Error::Unreachable("refused".into())), 1.0);
    s.delete(&local);
    assert!(s.gens.is_empty());
    assert_eq!(s.deleting, None);
}

#[test]
fn generating_while_disconnected_adds_nothing_and_says_why() {
    let mut s = knowing(State::offline(), &["a"]);
    s.conn = Conn::Disconnected("refused".into());
    s.prompt.replace_text("a fox");
    s.generate();
    assert!(s.gens.is_empty());
    assert_eq!(s.notes.last().map(|n| n.title.as_str()), Some("Start mmh3 server first."));
    s.merge(&[server("a", api::State::Done)], 0.0);
    s.rerun("a");
    assert_eq!(s.gens.len(), 1);
}

#[test]
fn a_list_that_lands_before_the_submit_answer_leaves_one_row() {
    let mut s = online();
    s.prompt.replace_text("a fox");
    s.generate();
    let local = s.gens[0].id.clone();
    s.merge(&[running("srv1", 1)], 1.0);
    assert_eq!(s.gens.len(), 1, "a generation the studio has not heard it asked for waits for the answer");
    s.submitted(&local, Ok(server("srv1", api::State::Queued)), 1.5);
    let ids: Vec<&str> = s.gens.iter().map(|g| g.id.as_str()).collect();
    assert_eq!(ids, ["srv1"]);
    assert_eq!(s.selected.as_deref(), Some("srv1"));
    assert_eq!(s.gens[0].prompt(), "a fox");
    s.merge(&[running("srv1", 2)], 2.0);
    assert_eq!(s.gens[0].phase, Phase::Running { step: Some(2), steps: 6, since: 2.0 });
}

#[test]
fn a_list_fetched_before_a_submission_lands_after_it_without_dropping_it() {
    let mut s = online();
    s.prompt.replace_text("a fox");
    s.generate();
    let local = s.gens[0].id.clone();
    s.submitted(&local, Ok(server("srv1", api::State::Queued)), 1.0);
    s.merge(&[server("old", api::State::Done)], 1.5);
    assert!(s.item("srv1").is_some(), "a list from before the server took it says nothing of it");
    assert_eq!(s.requests.get("srv1").map(|r| r.prompt.as_str()), Some("a fox"));
    s.merge(&[server("srv1", api::State::Queued), server("old", api::State::Done)], 2.0);
    s.merge(&[server("old", api::State::Done)], 3.0);
    assert!(s.item("srv1").is_none(), "once listed, leaving the list means the server dropped it");
    assert!(s.requests.is_empty());
}

#[test]
fn the_fork_says_which_generations_are_pictures_so_nothing_is_probed() {
    let mut s = online();
    let video = Generation { kind: Some(Kind::Video), ..server("v", api::State::Running) };
    s.merge(&[video, fork("theirs", api::State::Done, "a lighthouse")], 0.0);
    let ids: Vec<&str> = s.gens.iter().map(|g| g.id.as_str()).collect();
    assert_eq!(ids, ["theirs"]);
    assert!(s.probing.is_empty());
    assert_eq!(crate::gallery::title(&s.gens[0]), "a lighthouse");
    assert_eq!((s.gens[0].size(), s.gens[0].seed()), (Some((1024, 768)), Some(9)));
}

#[test]
fn the_server_s_word_on_what_was_asked_wins_over_the_studio_s_memory() {
    let mut s = knowing(online(), &["a", "b"]);
    s.merge(&[fork("a", api::State::Done, "said by the server"), server("b", api::State::Done)], 0.0);
    assert_eq!(s.item("a").unwrap().prompt(), "said by the server");
    assert_eq!(s.item("a").unwrap().size(), Some((1024, 768)));
    assert_eq!(s.item("b").unwrap().prompt(), "a fox", "an upstream server says nothing, so the studio's memory stands");
    assert_eq!(s.item("b").unwrap().size(), Some((2048, 2048)));
}

#[test]
fn a_rerun_sends_what_the_server_says_with_the_studio_s_files_of_its_references() {
    let mut s = online();
    let file = PathBuf::from("/tmp/street.jpg");
    s.requests.insert("a".into(), ImageRequest { references: vec![file.clone()], ..asked("old words") });
    let mut edit = fork("a", api::State::Done, "<Picture 1> at night");
    if let Some(r) = &mut edit.request {
        r.references = vec!["picture-1.jpg".into()];
    }
    let mut theirs = fork("b", api::State::Done, "<Picture 1> by day");
    if let Some(r) = &mut theirs.request {
        r.references = vec!["picture-1.jpg".into()];
    }
    s.merge(&[edit, theirs, fork("c", api::State::Done, "a fox")], 0.0);
    let again = s.item("a").unwrap().rerun_request().unwrap();
    assert_eq!((again.prompt.as_str(), again.width, again.references.as_slice()), ("<Picture 1> at night", 1024, &[file][..]));
    assert_eq!(s.item("b").unwrap().rerun_request(), None, "its pictures are only on the server");
    assert_eq!(s.item("b").unwrap().reference_count(), 1);
    assert!(s.item("c").unwrap().rerun_request().is_some(), "a prompt alone is enough");
}

#[test]
fn an_event_updates_a_row_or_adds_a_picture_where_the_queue_puts_it() {
    let mut s = online();
    let at = |id: &str, queued, state| Generation { queued, ..fork(id, state, id) };
    s.merge(&[at("new", 30, api::State::Queued), at("old", 10, api::State::Done)], 0.0);
    s.upsert(&at("mid", 20, api::State::Queued), 1.0);
    s.upsert(&Generation { kind: Some(Kind::Video), ..server("video", api::State::Queued) }, 1.0);
    s.upsert(&Generation { step: Some(1), steps: Some(6), ..at("new", 30, api::State::Running) }, 2.0);
    let ids: Vec<&str> = s.gens.iter().map(|g| g.id.as_str()).collect();
    assert_eq!(ids, ["new", "mid", "old"]);
    assert_eq!(s.gens[0].phase, Phase::Running { step: Some(1), steps: 6, since: 2.0 });
}

#[test]
fn a_preview_shows_until_the_picture_does_and_an_older_one_never_replaces_a_newer() {
    let mut s = knowing(online(), &["a"]);
    s.merge(&[Generation { step: Some(2), steps: Some(6), preview_step: Some(2), ..server("a", api::State::Running) }], 0.0);
    let image = |s: &State| s.images.rgba(1, 1, vec![0, 0, 0, 255]);
    let two = image(&s);
    s.preview_ready("a", 2, Ok(two));
    assert_eq!(s.item("a").unwrap().live.as_ref().map(|l| l.step), Some(2));
    let one = image(&s);
    s.preview_ready("a", 1, Ok(one));
    assert_eq!(s.item("a").unwrap().live.as_ref().map(|l| l.step), Some(2), "a late answer for an older step is dropped");
    s.preview_available("a", 3);
    s.preview_ready("a", 3, Err("gone".into()));
    assert_eq!(s.item("a").unwrap().live.as_ref().map(|l| l.step), Some(2), "a failed fetch keeps the last one");
    assert_eq!(s.item("a").unwrap().preview_wanted(), None, "a failed fetch is not tried again at once");
    s.preview_available("a", 4);
    assert_eq!(s.item("a").unwrap().preview_step, Some(4));
    assert_eq!(s.item("a").unwrap().preview_wanted(), Some(4), "a newer preview is");
    let thumb = image(&s);
    s.picture_ready("a", PathBuf::from("/tmp/a.png"), thumb);
    assert!(s.item("a").unwrap().live.is_none(), "the picture replaces its preview");
}

#[test]
fn a_preview_that_could_not_be_fetched_is_asked_for_again_only_with_the_next_snapshot() {
    let mut s = knowing(online(), &["a"]);
    let cancelled = Generation { preview_step: Some(2), ..server("a", api::State::Cancelled) };
    s.merge(std::slice::from_ref(&cancelled), 0.0);
    assert_eq!(s.item("a").unwrap().preview_wanted(), Some(2));
    s.preview_ready("a", 2, Err("404".into()));
    assert_eq!(s.item("a").unwrap().preview_wanted(), None);
    s.upsert(&cancelled, 1.0);
    assert_eq!(s.item("a").unwrap().preview_wanted(), None, "an event about it is no reason");
    s.merge(&[cancelled], 2.0);
    assert_eq!(s.item("a").unwrap().preview_wanted(), Some(2));
}

#[test]
fn only_a_server_with_the_event_stream_is_asked_to_stop_one() {
    let mut s = knowing(online(), &["r", "d"]);
    s.merge(&[running("r", 2), server("d", api::State::Done)], 0.0);
    assert!(!s.can_stop(s.item("r").unwrap()), "an upstream server can't");
    s.feed = Feed::Streaming;
    assert!(s.can_stop(s.item("r").unwrap()));
    assert!(!s.can_stop(s.item("d").unwrap()), "a finished one has nothing to stop");
    s.stop("r");
    assert!(s.item("r").unwrap().stopping);
    assert!(!s.can_stop(s.item("r").unwrap()), "asked once is enough");
    s.cancelled("r", Err(api::Error::Refused { status: 409, message: "that generation has ended".into() }), 1.0);
    assert!(!s.item("r").unwrap().stopping);
    assert_eq!(s.notes.last().map(|n| n.title.as_str()), Some("Couldn't stop the generation."));
    s.stop("r");
    s.cancelled("r", Ok(running("r", 2)), 1.0);
    assert!(s.item("r").unwrap().stopping, "still running until its step ends");
    s.upsert(&Generation { preview_step: Some(3), ..server("r", api::State::Cancelled) }, 2.0);
    let r = s.item("r").unwrap();
    assert_eq!((&r.phase, r.stopping), (&Phase::Cancelled { step: Some(3) }, false));
    s.delete("r");
    assert_eq!(s.deleting.as_deref(), Some("r"), "a cancelled generation can be deleted");
}

#[test]
fn a_status_the_studio_cannot_read_still_counts_as_connected() {
    let mut s = State::offline();
    s.set_status(Err(api::Error::Malformed("missing field `queued`".into())));
    assert!(matches!(s.conn, Conn::Unreadable(_)));
    assert!(s.connected());
    s.set_status(Err(api::Error::Unreachable("refused".into())));
    assert!(matches!(s.conn, Conn::Disconnected(_)));
    assert!(!s.connected());
}

#[test]
fn generate_needs_a_prompt_and_a_valid_size_and_seed() {
    let mut s = State::offline();
    s.prompt.replace_text("a fox");
    assert_eq!(s.request().unwrap_err(), "Start mmh3 server first.");
    s = online();
    assert_eq!(s.request().unwrap_err(), "Write a prompt first.");
    s.prompt.replace_text("  a fox ");
    s.custom_size = true;
    s.custom_w.replace_text("1000");
    assert!(s.request().unwrap_err().contains("multiples of 32"));
    s.custom_w.replace_text("1024");
    s.seed.replace_text("abc");
    assert_eq!(s.request().unwrap_err(), "The seed must be a whole number.");
    s.seed.replace_text("42");
    let r = s.request().unwrap();
    assert_eq!((r.prompt.as_str(), r.width, r.height, r.seed), ("a fox", 1024, 1024, 42));
    s.seed.replace_text("");
    assert!(s.request().is_ok(), "an empty seed is a random one");
}

#[test]
fn a_picture_asked_for_elsewhere_joins_the_gallery_in_queue_order_and_a_video_never_does() {
    let mut s = knowing(online(), &["new", "old"]);
    let at = |id: &str, queued| Generation { queued, ..server(id, api::State::Done) };
    let list = [at("new", 30), at("theirs", 20), at("video", 15), at("old", 10)];
    s.merge(&list, 0.0);
    assert_eq!(s.gens.len(), 2);
    s.probing.extend(["theirs".to_owned(), "video".to_owned()]);
    s.probed("theirs", Probe::Picture(Box::new(at("theirs", 20))), 1.0);
    s.probed("video", Probe::NotAPicture, 1.0);
    let ids: Vec<&str> = s.gens.iter().map(|g| g.id.as_str()).collect();
    assert_eq!(ids, ["new", "theirs", "old"]);
    assert_eq!(s.gens[1].request, None);
    assert_eq!(crate::gallery::title(&s.gens[1]), "Asked for elsewhere");

    s.merge(&list, 2.0);
    let ids: Vec<&str> = s.gens.iter().map(|g| g.id.as_str()).collect();
    assert_eq!(ids, ["new", "theirs", "old"], "the next list keeps it and still leaves the video out");
    s.merge(&[at("new", 30)], 3.0);
    assert!(s.elsewhere.is_empty(), "what the server forgot, the studio forgets");
}

#[test]
fn an_unanswered_probe_is_asked_again_and_a_stale_one_is_ignored() {
    let mut s = online();
    s.probing.insert("x".into());
    s.probed("x", Probe::Unanswered, 0.0);
    assert!(s.probing.is_empty() && s.elsewhere.is_empty() && s.gens.is_empty());
    s.probed("y", Probe::Picture(Box::new(server("y", api::State::Queued))), 0.0);
    assert!(s.gens.is_empty(), "an answer nobody waits for is dropped");
}

#[test]
fn the_tags_a_prompt_mentions_without_a_picture_are_found_once_each() {
    assert_eq!(missing_pictures("no tags", 0), Vec::<usize>::new());
    assert_eq!(missing_pictures("<Picture 1> and <Picture 2>", 2), Vec::<usize>::new());
    assert_eq!(missing_pictures("<Picture 3> by <Picture 1>, then <Picture 3> and <Picture 0>", 1), [0, 3]);
    assert_eq!(missing_pictures("<Picture x> and <Picture 2", 0), Vec::<usize>::new(), "only whole tags count");
}

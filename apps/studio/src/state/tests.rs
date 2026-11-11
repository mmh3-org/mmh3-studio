use crate::api::{self, Generation};

use super::*;

fn online() -> State {
    let mut s = State::offline();
    s.conn = Conn::Connected(Status::default());
    s
}

fn server(id: &str, state: api::State) -> Generation {
    Generation { error: None, id: id.into(), queued: 1, seconds: None, state, step: None, steps: None }
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
    assert_eq!(phase, Phase::Done { seconds: 8.4 });
    assert!(step(&mut phase, &done, 9.5).is_empty(), "a repeated end asks for nothing");
}

#[test]
fn an_accepted_submission_starts_watching() {
    let mut phase = Phase::Sending;
    let fx: Vec<Effect> = step(&mut phase, &server("a", api::State::Queued), 0.0).into_iter().collect();
    assert_eq!(fx, [Effect::Watch]);
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
    s.size = SizeChoice::Custom;
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
    s.probed("theirs", Probe::Picture(at("theirs", 20)), 1.0);
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
    s.probed("y", Probe::Picture(server("y", api::State::Queued)), 0.0);
    assert!(s.gens.is_empty(), "an answer nobody waits for is dropped");
}

#[test]
fn the_tags_a_prompt_mentions_without_a_picture_are_found_once_each() {
    assert_eq!(missing_pictures("no tags", 0), Vec::<usize>::new());
    assert_eq!(missing_pictures("<Picture 1> and <Picture 2>", 2), Vec::<usize>::new());
    assert_eq!(missing_pictures("<Picture 3> by <Picture 1>, then <Picture 3> and <Picture 0>", 1), [0, 3]);
    assert_eq!(missing_pictures("<Picture x> and <Picture 2", 0), Vec::<usize>::new(), "only whole tags count");
}

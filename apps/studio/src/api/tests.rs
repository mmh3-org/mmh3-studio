use super::*;

const QUEUED: &str = include_str!("../../fixtures/generation-queued.json");
const DONE: &str = include_str!("../../fixtures/generation-done.json");
const LIST: &str = include_str!("../../fixtures/generations.json");
const STATUS_IDLE: &str = include_str!("../../fixtures/status-idle.json");
const STATUS_BUSY: &str = include_str!("../../fixtures/status-busy.json");
const REFUSAL: &str = include_str!("../../fixtures/refusal.json");
const EVENTS_VIDEO: &str = include_str!("../../fixtures/events-video.txt");
const EVENTS_IMAGE: &str = include_str!("../../fixtures/events-image.txt");
const FORK: &str = include_str!("../../fixtures/generation-fork.json");
const EVENTS_ALL: &str = include_str!("../../fixtures/events-all.txt");

/// The generations a stream of one generation's events says, in order.
fn generations(stream: &str) -> Vec<Generation> {
    let mut seen = Vec::new();
    read_events(stream.as_bytes(), |event| {
        seen.push(serde_json::from_str(&event.data).unwrap());
        Ok(())
    })
    .unwrap();
    seen
}

#[test]
fn the_documented_answer_to_a_request_reads() {
    let g: Generation = serde_json::from_str(QUEUED).unwrap();
    assert_eq!(g.id, "6ab1cf3b0000");
    assert_eq!(g.state, State::Queued);
    assert_eq!((g.seconds, g.step, g.steps, g.error), (None, None, None, None));
    assert_eq!((g.kind, g.request, g.timings, g.preview_step), (None, None, None, None), "an upstream server says none of the fork's fields");
}

#[test]
fn the_fork_says_what_was_asked_for_and_where_the_time_went() {
    let g: Generation = serde_json::from_str(FORK).unwrap();
    assert_eq!(g.kind, Some(Kind::Image));
    let asked = g.request.unwrap();
    assert_eq!(
        (asked.prompt.as_deref(), asked.width, asked.height, asked.seed),
        (Some("<Picture 1> at night, the street lamps lit."), Some(4096), Some(2304), Some(7))
    );
    assert_eq!(asked.references, ["picture-1.jpg"]);
    let t = g.timings.unwrap();
    assert_eq!(t.steps.len(), 6);
    assert!((t.load().unwrap() - (1.07 + 7.48 + 0.0004)).abs() < 1e-9);
    assert_eq!(g.preview_step, Some(6));
    assert_eq!((g.created_at, g.finished_at), (Some(1_790_037_819.52), Some(1_790_037_835.3)));
    assert_eq!(Timings::default().load(), None, "nothing timed is no load");
}

#[test]
fn a_finished_generation_carries_only_its_seconds() {
    let g: Generation = serde_json::from_str(DONE).unwrap();
    assert_eq!(g.state, State::Done);
    assert_eq!(g.seconds, Some(38.71));
    assert!(g.is_finished());
}

#[test]
fn a_generation_writes_back_as_the_server_writes_it() {
    let g: Generation = serde_json::from_str(DONE).unwrap();
    assert_eq!(serde_json::to_string(&g).unwrap(), DONE.trim());
}

#[test]
fn the_list_is_an_object_of_generations_most_recent_first() {
    let list: Generations = serde_json::from_str(LIST).unwrap();
    let states: Vec<State> = list.generations.iter().map(|g| g.state).collect();
    assert_eq!(states, [State::Running, State::Failed, State::Done, State::Done]);
    assert_eq!((list.generations[0].step, list.generations[0].steps), (Some(3), Some(6)));
    assert_eq!(list.generations[1].error.as_deref(), Some("--width must be a number"));
    assert!(list.generations.windows(2).all(|w| w[0].queued >= w[1].queued));
}

#[test]
fn status_reads_models_or_their_absence_while_busy() {
    let idle: Status = serde_json::from_str(STATUS_IDLE).unwrap();
    assert_eq!(idle.models.as_ref().map(Vec::len), Some(4));
    assert_eq!(idle.memory, Some(Memory { free: 57_481_990_144, total: 130_660_909_056 }));
    let busy: Status = serde_json::from_str(STATUS_BUSY).unwrap();
    assert_eq!(busy.models, None);
    assert_eq!(busy.generating.as_deref(), Some("6ab1cf3b0000"));
    assert_eq!(busy.queued, 1);
}

#[test]
fn status_reads_without_memory_on_a_build_with_no_device() {
    let json = STATUS_IDLE.replace(r#"{"free": 57481990144, "total": 130660909056}"#, "null");
    let status: Status = serde_json::from_str(&json).unwrap();
    assert_eq!(status.memory, None);
}

#[test]
fn events_from_the_docs_parse_one_per_step_then_the_end() {
    let seen: Vec<(State, Option<u32>)> = generations(EVENTS_VIDEO).iter().map(|g| (g.state, g.step)).collect();
    assert_eq!(seen, [(State::Running, Some(3)), (State::Done, None)]);
}

#[test]
fn picture_events_start_where_the_generation_is_and_skip_keep_alives() {
    let seen = generations(EVENTS_IMAGE);
    let states: Vec<(State, Option<u32>)> = seen.iter().map(|g| (g.state, g.step)).collect();
    assert_eq!(states[..2], [(State::Queued, None), (State::Running, None)]);
    assert_eq!(seen.iter().filter_map(|g| g.step).collect::<Vec<_>>(), [1, 2, 3, 4, 5, 6]);
    assert!(seen.iter().filter(|g| g.step.is_some()).all(|g| g.steps == Some(IMAGE_STEPS)));
    assert_eq!(seen.last().map(|g| (g.state, g.seconds)), Some((State::Done, Some(39.12))));
}

#[test]
fn events_with_crlf_line_ends_parse_the_same() {
    assert_eq!(generations(&EVENTS_IMAGE.replace('\n', "\r\n")).len(), 9);
}

#[test]
fn the_stream_of_every_generation_says_a_snapshot_then_changes_and_previews() {
    let mut seen = Vec::new();
    read_events(EVENTS_ALL.as_bytes(), |event| {
        seen.extend(Event::parse(&event.name, &event.data)?);
        Ok(())
    })
    .unwrap();
    assert_eq!(seen.len(), 5, "the keep-alive and an unknown event are skipped");
    let Event::Snapshot(list) = &seen[0] else { panic!("{:?}", seen[0]) };
    assert_eq!(list.iter().map(|g| g.kind).collect::<Vec<_>>(), [Some(Kind::Video), Some(Kind::Image)]);
    assert_eq!(list[0].request.as_ref().map(|r| (r.width, r.seed)), Some((None, Some(2))), "a video's own defaults are null");
    assert!(matches!(&seen[1], Event::Generation(g) if g.step == Some(2) && g.preview_step == Some(2)));
    assert_eq!(seen[2], Event::Preview(Preview { id: "6af4fadf0000".into(), step: 2, steps: Some(6), url: "/v1/generations/6af4fadf0000/preview/2".into() }));
    let Event::Generation(cancelled) = &seen[3] else { panic!("{:?}", seen[3]) };
    assert_eq!(cancelled.state, State::Cancelled);
    assert!(cancelled.is_finished());
    assert_eq!(cancelled.request.as_ref().and_then(|r| r.width), None, "a value the server kept as text reads as none");
    assert_eq!(seen[4], Event::Removed("6af4fadf0001".into()));
}

#[test]
fn a_missing_route_is_an_upstream_server() {
    assert!(is_missing_route(&refusal(404, "")));
    assert!(is_missing_route(&Error::Refused { status: 405, message: String::new() }));
    assert!(!is_missing_route(&refusal(409, r#"{"error":"that generation has ended"}"#)));
    assert!(!is_missing_route(&Error::Unreachable("refused".into())));
}

#[test]
fn an_event_split_over_data_lines_joins_with_newlines() {
    let event = |name: &str, data: &str| Some(SseEvent { name: name.into(), data: data.into() });
    let mut p = SseParser::default();
    assert_eq!(p.line("data: {\"a\":"), None);
    assert_eq!(p.line("data:1}"), None);
    assert_eq!(p.line("event: snapshot"), None);
    assert_eq!(p.line(""), event("snapshot", "{\"a\":\n1}"));
    assert_eq!(p.line(""), None);
    assert_eq!(p.line(": keep-alive"), None);
    assert_eq!(p.line("data: tail"), None);
    assert_eq!(p.finish(), event("message", "tail"), "a name lasts one event");
}

fn request() -> ImageRequest {
    ImageRequest { prompt: "a fox".into(), seed: 42, width: 2048, height: 1024, min_generation_area: None, references: Vec::new(), ..ImageRequest::default() }
}

fn names(parts: &[Part]) -> Vec<&str> {
    parts
        .iter()
        .map(|p| match p {
            Part::Text { name, .. } | Part::File { name, .. } => *name,
        })
        .collect()
}

#[test]
fn the_request_gives_only_options_of_a_picture() {
    let parts = request().parts().unwrap();
    assert_eq!(names(&parts), ["prompt", "seed", "width", "height"]);
    let with_area = ImageRequest { min_generation_area: Some(4_194_304), ..request() };
    assert_eq!(with_area.parts().unwrap().last(), Some(&Part::Text { name: "min-generation-area", value: "4194304".into() }));
}

#[test]
fn the_tuning_sends_only_what_it_changes_under_mmh3_s_names() {
    let high = ImageRequest { effort: Some(Effort::High), ..request() };
    assert_eq!(names(&high.parts().unwrap()), ["prompt", "seed", "width", "height"]);
    let tuning = Tuning {
        attention: Some(Attention::Sol),
        sparse_tau: Some(1.5),
        sparse_start: Some(0.5),
        draft_steps: Some(2),
        attention_precision: Some(Precision::Int8Fp16),
    };
    let fields = ImageRequest { tuning, ..request() }.fields();
    assert_eq!(
        fields[4..],
        [
            ("attention", "sol".to_owned()),
            ("sparse-tau", "1.5".to_owned()),
            ("sparse-start", "0.5".to_owned()),
            ("draft-steps", "2".to_owned()),
            ("attention-precision", "int8-fp16".to_owned()),
        ]
    );
}

#[test]
fn what_the_server_says_of_the_tuning_reads_with_its_defaults_left_out() {
    let asked: Asked =
        serde_json::from_str(r#"{"attention":"dense","attention_precision":"fp16","sparse_tau":"1.5","sparse_start":0.5,"draft_steps":2}"#).unwrap();
    assert_eq!((asked.sparse_tau, asked.sparse_start, asked.draft_steps), (Some(1.5), Some(0.5), Some(2)));
    assert_eq!(asked.tuning(), Tuning { sparse_tau: Some(1.5), sparse_start: Some(0.5), draft_steps: Some(2), ..Tuning::default() });
    let sol: Asked = serde_json::from_str(r#"{"attention":"sol","attention_precision":"int8-fp16"}"#).unwrap();
    assert_eq!(sol.tuning(), Tuning { attention: Some(Attention::Sol), attention_precision: Some(Precision::Int8Fp16), ..Tuning::default() });
}

#[test]
fn references_upload_in_order_under_names_that_cannot_collide() {
    let dir = std::env::temp_dir().join(format!("mmh3-studio-refs-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("a")).unwrap();
    std::fs::create_dir_all(dir.join("b")).unwrap();
    std::fs::write(dir.join("a/photo.JPG"), b"first").unwrap();
    std::fs::write(dir.join("b/photo.png"), b"second").unwrap();
    let r = ImageRequest { references: vec![dir.join("a/photo.JPG"), dir.join("b/photo.png")], ..request() };
    let files: Vec<(String, Vec<u8>)> = r
        .parts()
        .unwrap()
        .into_iter()
        .filter_map(|p| match p {
            Part::File { name: "reference", filename, bytes } => Some((filename, bytes)),
            _ => None,
        })
        .collect();
    assert_eq!(files, [("picture-1.jpg".to_owned(), b"first".to_vec()), ("picture-2.png".to_owned(), b"second".to_vec())]);
    let missing = ImageRequest { references: vec![dir.join("gone.png")], ..request() };
    assert!(missing.parts().unwrap_err().to_string().contains("gone.png"));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn multipart_bodies_frame_text_and_files() {
    let parts =
        [Part::Text { name: "prompt", value: "a fox\nin snow".into() }, Part::File { name: "reference", filename: "picture-1.png".into(), bytes: vec![0, 1] }];
    let (content_type, body) = multipart(&parts, "B");
    assert_eq!(content_type, "multipart/form-data; boundary=B");
    let mut expected = b"--B\r\nContent-Disposition: form-data; name=\"prompt\"\r\n\r\na fox\nin snow\r\n\
         --B\r\nContent-Disposition: form-data; name=\"reference\"; filename=\"picture-1.png\"\r\nContent-Type: image/png\r\n\r\n"
        .to_vec();
    expected.extend_from_slice(&[0, 1]);
    expected.extend_from_slice(b"\r\n--B--\r\n");
    assert_eq!(body, expected);
}

#[test]
fn a_refusal_reads_as_the_servers_words() {
    let r: Refusal = serde_json::from_str(REFUSAL).unwrap();
    assert_eq!(r.error, "steps is not an option of a picture");
    assert_eq!(Error::Refused { status: 400, message: r.error }.to_string(), "steps is not an option of a picture (HTTP 400)");
    assert_eq!(Error::Refused { status: 500, message: String::new() }.to_string(), "The server refused it (HTTP 500).");
    assert!(Error::Unreachable("refused".into()).is_unreachable());
}

#[test]
fn a_gateway_that_cannot_reach_the_server_reads_as_unreachable() {
    assert!(refusal(502, "Bad Gateway").is_unreachable());
    assert!(refusal(504, "").is_unreachable());
    assert_eq!(refusal(503, REFUSAL), Error::Refused { status: 503, message: "steps is not an option of a picture".into() });
    assert_eq!(refusal(500, " boom\n"), Error::Refused { status: 500, message: "boom".into() });
}

#[test]
fn an_unreachable_server_is_reported_as_such() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let err = Client::new(format!("http://127.0.0.1:{port}")).status().unwrap_err();
    assert!(err.is_unreachable(), "{err:?}");
}

#[test]
fn a_stream_that_falls_silent_is_given_up_as_unreachable() {
    use std::io::{Read, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut head = [0; 1024];
        let _ = stream.read(&mut head);
        stream.write_all(b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\n\r\n").unwrap();
        std::thread::sleep(Duration::from_secs(2));
    });
    let client = Client::with_silence(format!("http://127.0.0.1:{port}"), Duration::from_millis(300));
    let err = client.events("g1", |_| {}).unwrap_err();
    assert!(err.is_unreachable(), "{err:?}");
    server.join().unwrap();
}

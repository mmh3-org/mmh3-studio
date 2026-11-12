use std::time::Duration;

use std::sync::mpsc;

use mmh3_studio::api::{Client, Error, Event, Generation, ImageRequest, Kind, State};
use mmh3_studio::mock::{self, Config};

fn quick_server() -> Client {
    let ms = Duration::from_millis;
    server(Config { text: ms(20), step: ms(30), decode: ms(20), ..Config::default() })
}

fn server(config: Config) -> Client {
    let addr = mock::spawn("127.0.0.1:0", config).unwrap();
    Client::new(format!("http://{addr}"))
}

/// Follows `GET /v1/events` on a thread of its own, handing each event over.
fn follow(client: &Client) -> mpsc::Receiver<Event> {
    let (tx, rx) = mpsc::channel();
    let client = client.clone();
    std::thread::spawn(move || {
        let _ = client.feed(|event| {
            let _ = tx.send(event);
        });
    });
    rx
}

/// The events up to and including the first that `last` says is the end.
fn until(rx: &mpsc::Receiver<Event>, last: impl Fn(&Event) -> bool) -> Vec<Event> {
    let mut seen = Vec::new();
    loop {
        let event = rx.recv_timeout(Duration::from_secs(10)).expect("an event");
        let end = last(&event);
        seen.push(event);
        if end {
            return seen;
        }
    }
}

fn ended(id: &str) -> impl Fn(&Event) -> bool + '_ {
    move |event| matches!(event, Event::Generation(g) if g.id == id && g.is_finished())
}

fn request(prompt: &str, seed: u64) -> ImageRequest {
    ImageRequest { prompt: prompt.into(), seed, width: 320, height: 256, min_generation_area: None, references: Vec::new() }
}

fn watch(client: &Client, id: &str) -> Vec<Generation> {
    let mut seen = Vec::new();
    client.events(id, |g| seen.push(g)).unwrap();
    seen
}

#[test]
fn a_picture_streams_from_the_queue_through_six_steps_then_serves_its_png() {
    let client = quick_server();
    assert_eq!(client.status().unwrap().models, Some(Vec::new()), "a server just started holds nothing");
    let created = client.create_image(&request("a red fox in snow", 11)).unwrap();
    assert_eq!(created.state, State::Queued);

    let seen = watch(&client, &created.id);
    assert!(seen.iter().any(|g| g.state == State::Running && g.step.is_none()), "running before the first step");
    let mut steps: Vec<u32> = seen.iter().filter_map(|g| g.step).collect();
    steps.dedup();
    assert_eq!(steps, [1, 2, 3, 4, 5, 6], "the last step is said again as the decode's timings come in");
    let finished = seen.last().unwrap();
    assert_eq!((finished.state, finished.step, finished.steps), (State::Done, None, None));
    assert!(finished.seconds.is_some_and(|s| s > 0.0));

    let png = client.image(&created.id).unwrap();
    let img = image::load_from_memory(&png).unwrap();
    assert_eq!((img.width(), img.height()), (320, 256));
    let kept = ["text_encoder", "video_vae", "dit"].map(str::to_owned);
    assert_eq!(client.status().unwrap().models.as_deref(), Some(&kept[..]), "the picture's decoder sits in the video VAE's slot");

    assert_eq!(client.list().unwrap().len(), 1);
    client.delete(&created.id).unwrap();
    assert_eq!(client.list().unwrap(), []);
}

#[test]
fn a_stream_opened_after_the_end_says_only_the_end() {
    let client = quick_server();
    let g = client.create_image(&request("quick", 1)).unwrap();
    let _ = watch(&client, &g.id);
    let again = watch(&client, &g.id);
    assert_eq!(again.len(), 1);
    assert_eq!(again[0].state, State::Done);
}

#[test]
fn references_upload_as_files_and_more_than_sixteen_fail_the_generation() {
    let client = quick_server();
    let dir = std::env::temp_dir().join(format!("mmh3-studio-mock-refs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("street.png");
    std::fs::write(&file, mock::paint(64, 48, 3)).unwrap();

    let two = client.create_image(&ImageRequest { references: vec![file.clone(), file.clone()], ..request("<Picture 1> beside <Picture 2>", 4) }).unwrap();
    assert_eq!(watch(&client, &two.id).last().map(|g| g.state), Some(State::Done));

    let many = client.create_image(&ImageRequest { references: vec![file.clone(); 17], ..request("too many", 5) }).unwrap();
    let end = watch(&client, &many.id).pop().unwrap();
    assert_eq!((end.state, end.error.as_deref()), (State::Failed, Some("pass at most 16 --reference pictures")));

    std::fs::write(dir.join("broken.png"), b"not a picture").unwrap();
    let broken = client.create_image(&ImageRequest { references: vec![dir.join("broken.png")], ..request("broken", 6) }).unwrap();
    let end = watch(&client, &broken.id).pop().unwrap();
    assert_eq!(end.state, State::Failed);
    assert!(end.error.unwrap().starts_with("picture-1.png: "));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_bad_value_fails_the_generation_rather_than_the_request() {
    let client = quick_server();
    let g = client.create_image(&ImageRequest { width: 16, ..request("tiny", 1) }).unwrap();
    let end = watch(&client, &g.id).pop().unwrap();
    assert_eq!((end.state, end.error.as_deref()), (State::Failed, Some("--width and --height must be at least 32")));
    assert!(matches!(client.image(&g.id), Err(Error::Refused { status: 409, ref message }) if message == "that generation failed"));
}

#[test]
fn the_server_refuses_what_its_routes_refuse() {
    let client = quick_server();
    let g = client.create_image(&request("slow", 2)).unwrap();
    assert!(matches!(client.image(&g.id), Err(Error::Refused { status: 409, ref message }) if message == "that generation has not written anything yet"));
    while client.generation(&g.id).unwrap().state != State::Running {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(matches!(client.delete(&g.id), Err(Error::Refused { status: 409, ref message }) if message == "that generation is running"));
    assert!(matches!(client.generation("nope"), Err(Error::Refused { status: 404, ref message }) if message == "no such generation"));
    assert!(matches!(client.events("nope", |_| {}), Err(Error::Refused { status: 404, .. })));
}

#[test]
fn a_failing_prompt_ends_failed_with_its_reason() {
    let client = quick_server();
    let g = client.create_image(&request("please [fail]", 3)).unwrap();
    let last = watch(&client, &g.id).pop().unwrap();
    assert_eq!(last.state, State::Failed);
    assert!(last.error.unwrap().contains("fail"));
}

#[test]
fn queued_generations_wait_their_turn_and_list_most_recent_first() {
    // Steps long enough that the status read after the first step lands within the steps,
    // however loaded the machine is.
    let ms = Duration::from_millis;
    let addr = mock::spawn("127.0.0.1:0", Config { text: ms(20), step: ms(150), decode: ms(20), ..Config::default() }).unwrap();
    let client = Client::new(format!("http://{addr}"));
    let a = client.create_image(&request("first", 1)).unwrap();
    let b = client.create_image(&request("second", 2)).unwrap();
    while client.generation(&a.id).unwrap().step.is_none() {
        std::thread::sleep(ms(5));
    }
    let status = client.status().unwrap();
    assert_eq!(status.generating.as_deref(), Some(a.id.as_str()));
    assert_eq!(status.queued, 1);
    assert_eq!(status.models.as_deref(), Some(&["text_encoder".to_owned()][..]), "a first picture's steps hold no DiT and no decoder yet");
    let list = client.list().unwrap();
    assert_eq!(list[0].id, b.id, "most recent first");
    let mut first = None;
    client.events(&b.id, |g| _ = first.get_or_insert(g)).unwrap();
    assert_eq!(first.map(|g| g.state), Some(State::Queued), "the first event is where it is now");
    assert!(a.id < b.id);
}

fn post(client: &Client, path: &str, parts: &[mmh3_studio::api::Part]) -> (u16, Option<String>, String) {
    let (content_type, body) = mmh3_studio::api::multipart(parts, "B");
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
    let mut answer = agent.post(&format!("{}{path}", client.base())).header("Content-Type", &content_type).send(&body[..]).unwrap();
    let location = answer.headers().get("location").and_then(|l| l.to_str().ok()).map(str::to_owned);
    (answer.status().as_u16(), location, answer.body_mut().read_to_string().unwrap())
}

#[test]
fn a_field_that_is_not_an_option_of_a_picture_is_refused_by_name() {
    use mmh3_studio::api::Part;
    let client = quick_server();
    let (status, _, body) = post(&client, "/v1/images", &[Part::Text { name: "steps", value: "4".into() }]);
    assert_eq!((status, body.as_str()), (400, r#"{"error":"steps is not an option of a picture"}"#));
    let (status, location, body) = post(&client, "/v1/images", &[Part::Text { name: "prompt", value: "a fox".into() }]);
    let g: Generation = serde_json::from_str(&body).unwrap();
    assert_eq!((status, location), (202, Some(format!("/v1/generations/{}", g.id))));
    assert!(client.list().unwrap().iter().all(|l| l.id == g.id), "a refused request leaves nothing behind");
}

#[test]
fn a_video_has_no_picture() {
    use mmh3_studio::api::Part;
    // A text phase long enough that the status read once it runs lands within it.
    let ms = Duration::from_millis;
    let addr = mock::spawn("127.0.0.1:0", Config { text: ms(300), step: ms(10), decode: ms(10), ..Config::default() }).unwrap();
    let client = Client::new(format!("http://{addr}"));
    let (status, _, body) =
        post(&client, "/v1/generations", &[Part::Text { name: "prompt", value: "a fox".into() }, Part::Text { name: "steps", value: "1".into() }]);
    assert_eq!(status, 202);
    let g: Generation = serde_json::from_str(&body).unwrap();
    assert!(matches!(client.image(&g.id), Err(Error::Refused { status: 404, ref message }) if message == "that generation writes a video"));
    while client.generation(&g.id).unwrap().state != State::Running {
        std::thread::sleep(ms(5));
    }
    assert_eq!(client.status().unwrap().models, None, "a video lends every model to its worker");
    assert_eq!(watch(&client, &g.id).last().map(|g| g.state), Some(State::Done));
    assert!(matches!(client.image(&g.id), Err(Error::Refused { status: 404, .. })));

    let after_video = client.status().unwrap().models;
    let refused = client.create_image(&ImageRequest { width: 16, ..request("too narrow", 1) }).unwrap();
    assert_eq!(watch(&client, &refused.id).last().map(|g| g.state), Some(State::Failed));
    assert_eq!(client.status().unwrap().models, after_video, "a picture refused before it loads anything leaves the table be");
    let picture = client.create_image(&request("a fox", 1)).unwrap();
    assert_eq!(watch(&client, &picture.id).last().map(|g| g.state), Some(State::Done));
    let kept = ["text_encoder", "video_vae", "audio_vae", "dit"].map(str::to_owned);
    assert_eq!(client.status().unwrap().models.as_deref(), Some(&kept[..]), "a picture leaves the video's audio VAE where it was");
}

#[test]
fn every_generation_streams_from_one_snapshot_with_a_preview_after_each_step() {
    let client = quick_server();
    let before = client.create_image(&request("already there", 1)).unwrap();
    let _ = watch(&client, &before.id);
    let rx = follow(&client);
    let Event::Snapshot(list) = rx.recv_timeout(Duration::from_secs(5)).unwrap() else { panic!("a snapshot first") };
    assert_eq!(list.iter().map(|g| (g.id.as_str(), g.state)).collect::<Vec<_>>(), [(before.id.as_str(), State::Done)]);

    let created = client.create_image(&request("a red fox in snow", 11)).unwrap();
    let seen = until(&rx, ended(&created.id));
    let previews: Vec<(u32, String)> = seen.iter().filter_map(|e| if let Event::Preview(p) = e { Some((p.step, p.url.clone())) } else { None }).collect();
    assert_eq!(previews.iter().map(|(k, _)| *k).collect::<Vec<_>>(), [1, 2, 3, 4, 5, 6]);
    assert_eq!(previews[1].1, format!("/v1/generations/{}/preview/2", created.id));
    for (i, e) in seen.iter().enumerate() {
        if let Event::Preview(p) = e {
            assert!(
                matches!(&seen[i - 1], Event::Generation(g) if g.preview_step == Some(p.step)),
                "a generation's change comes before the preview it brought"
            );
        }
    }

    let Some(Event::Generation(done)) = seen.last() else { unreachable!() };
    assert_eq!((done.state, done.kind), (State::Done, Some(Kind::Image)));
    let asked = done.request.clone().unwrap();
    assert_eq!((asked.prompt.as_deref(), asked.width, asked.height, asked.seed), (Some("a red fox in snow"), Some(320), Some(256), Some(11)));
    assert_eq!(asked.attention_precision.as_deref(), Some("fp16"), "the precision the run took");
    let timings = done.timings.clone().unwrap();
    assert_eq!(timings.steps.len(), 6);
    assert!(timings.encode.is_some() && timings.decode.is_some() && timings.write.is_some());
    assert!(timings.load_text.is_none() && timings.load_dit.is_none(), "the first picture left its models resident");
    assert!(done.created_at.is_some() && done.started_at.is_some() && done.finished_at.is_some());

    let png = client.preview(&created.id, 2).unwrap();
    let small = image::load_from_memory(&png).unwrap();
    assert_eq!((small.width(), small.height()), (40, 32), "an eighth of the canvas");
    assert!(matches!(client.preview(&created.id, 7), Err(Error::Refused { status: 404, ref message }) if message == "no preview of step 7"));
}

#[test]
fn a_queued_generation_is_cancelled_at_once_and_a_running_one_at_its_next_step() {
    let ms = Duration::from_millis;
    let client = server(Config { text: ms(20), step: ms(150), decode: ms(20), ..Config::default() });
    let running = client.create_image(&request("first", 1)).unwrap();
    let queued = client.create_image(&request("second", 2)).unwrap();
    while client.generation(&running.id).unwrap().step.is_none() {
        std::thread::sleep(ms(5));
    }
    assert!(matches!(client.preview(&queued.id, 1), Err(Error::Refused { status: 404, .. })), "no preview before its first step");

    let answer = client.cancel(&queued.id).unwrap();
    assert_eq!(answer.state, State::Cancelled, "one waiting never runs");
    let answer = client.cancel(&running.id).unwrap();
    assert_eq!(answer.state, State::Running, "one running finishes its step first");
    let mut end = None;
    client.events(&running.id, |g| end = Some(g)).unwrap();
    let end = end.unwrap();
    assert_eq!(end.state, State::Cancelled);
    assert!(end.preview_step.is_some_and(|k| k < 6), "{:?}", end.preview_step);
    assert!(matches!(client.image(&running.id), Err(Error::Refused { status: 409, ref message }) if message == "that generation was cancelled"));
    assert!(client.preview(&running.id, 1).is_ok(), "a cancelled picture keeps its previews");

    let done = client.create_image(&request("third", 3)).unwrap();
    assert_eq!(watch(&client, &done.id).last().map(|g| g.state), Some(State::Done), "the queue moves on");
    assert!(matches!(client.cancel(&done.id), Err(Error::Refused { status: 409, ref message }) if message == "that generation has ended"));
    assert!(matches!(client.cancel("nope"), Err(Error::Refused { status: 404, .. })));
    client.delete(&running.id).unwrap();
}

#[test]
fn an_upstream_server_has_none_of_the_fork_s_routes_or_fields() {
    let ms = Duration::from_millis;
    let client = server(Config { text: ms(10), step: ms(10), decode: ms(10), upstream: true });
    let g = client.create_image(&request("a fox", 1)).unwrap();
    let end = watch(&client, &g.id).pop().unwrap();
    assert_eq!(end.state, State::Done);
    assert_eq!((end.kind, end.request, end.timings, end.preview_step, end.created_at), (None, None, None, None, None));
    let missing = |e: Option<Error>| e.as_ref().is_some_and(mmh3_studio::api::is_missing_route);
    assert!(missing(client.feed(|_| {}).err()));
    assert!(missing(client.cancel(&g.id).err()));
    assert!(missing(client.preview(&g.id, 1).err()));
    let raw = ureq::get(&format!("{}/v1/generations/{}", client.base(), g.id)).call().unwrap().into_body().read_to_string().unwrap();
    assert!(!raw.contains("kind"), "{raw}");
}

#[test]
fn the_fork_writes_its_fields_null_until_they_are_known() {
    let client = quick_server();
    let g = client.create_image(&request("a fox", 1)).unwrap();
    let raw = ureq::get(&format!("{}/v1/generations/{}", client.base(), g.id)).call().unwrap().into_body().read_to_string().unwrap();
    let value: serde_json::Value = serde_json::from_str(&raw).unwrap();
    for field in ["kind", "request", "created_at", "started_at", "finished_at", "timings", "preview_step"] {
        assert!(value.get(field).is_some(), "{field} in {raw}");
    }
    let _ = watch(&client, &g.id);
}

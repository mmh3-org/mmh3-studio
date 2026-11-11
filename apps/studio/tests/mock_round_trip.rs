use std::time::Duration;

use mmh3_studio::api::{Client, Error, Generation, ImageRequest, State};
use mmh3_studio::mock::{self, Config};

fn quick_server() -> Client {
    let ms = Duration::from_millis;
    let addr = mock::spawn("127.0.0.1:0", Config { text: ms(20), step: ms(30), decode: ms(20) }).unwrap();
    Client::new(format!("http://{addr}"))
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
    let steps: Vec<u32> = seen.iter().filter_map(|g| g.step).collect();
    assert_eq!(steps, [1, 2, 3, 4, 5, 6]);
    let finished = seen.last().unwrap();
    assert_eq!((finished.state, finished.step, finished.steps), (State::Done, None, None));
    assert!(finished.seconds.is_some_and(|s| s > 0.0));

    let png = client.image(&created.id).unwrap();
    let img = image::load_from_memory(&png).unwrap();
    assert_eq!((img.width(), img.height()), (320, 256));
    assert_eq!(client.status().unwrap().models.map(|m| m.len()), Some(3));

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
    let addr = mock::spawn("127.0.0.1:0", Config { text: ms(20), step: ms(150), decode: ms(20) }).unwrap();
    let client = Client::new(format!("http://{addr}"));
    let a = client.create_image(&request("first", 1)).unwrap();
    let b = client.create_image(&request("second", 2)).unwrap();
    while client.generation(&a.id).unwrap().step.is_none() {
        std::thread::sleep(ms(5));
    }
    let status = client.status().unwrap();
    assert_eq!(status.generating.as_deref(), Some(a.id.as_str()));
    assert_eq!(status.queued, 1);
    assert_eq!(status.models.as_deref(), Some(&["text_encoder".to_owned(), "video_vae".to_owned()][..]), "a picture's steps hold no DiT in the table");
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
    let addr = mock::spawn("127.0.0.1:0", Config { text: ms(300), step: ms(10), decode: ms(10) }).unwrap();
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
}

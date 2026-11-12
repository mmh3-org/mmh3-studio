//! Runs the studio's client against a real `mmh3 server` with the picture models, named by
//! `MMH3_SERVER`. It generates on the GPU, so it only runs when asked:
//! `MMH3_SERVER=http://127.0.0.1:8833 cargo test --release --test real_server -- --ignored`.

use std::time::Duration;

use mmh3_studio::api::{Client, Error, Generation, ImageRequest, Part, State, multipart};

fn server() -> Client {
    Client::new(std::env::var("MMH3_SERVER").expect("MMH3_SERVER names the server to test"))
}

fn small(prompt: &str, references: Vec<std::path::PathBuf>) -> ImageRequest {
    ImageRequest { prompt: prompt.into(), seed: 7, width: 1024, height: 1024, min_generation_area: Some(1024), references, ..ImageRequest::default() }
}

fn watch(client: &Client, id: &str) -> Vec<Generation> {
    let mut seen = Vec::new();
    client
        .events(id, |g| {
            eprintln!("{:?} step {:?}/{:?} seconds {:?} error {:?}", g.state, g.step, g.steps, g.seconds, g.error);
            seen.push(g);
        })
        .unwrap();
    seen
}

#[test]
#[ignore = "needs a real mmh3 server and its GPU"]
fn a_picture_and_an_edit_of_it_round_trip_through_a_real_server() {
    let client = server();
    eprintln!("status before: {:?}", client.status().unwrap());

    let created =
        client.create_image(&small("A red panda in a knitted scarf sits on a mossy log in a misty pine forest at dawn, photorealistic", Vec::new())).unwrap();
    assert_eq!(created.state, State::Queued);
    let seen = watch(&client, &created.id);
    let end = seen.last().unwrap();
    assert_eq!(end.state, State::Done, "{:?}", end.error);
    assert!(end.seconds.is_some());
    assert_eq!(seen.iter().filter_map(|g| g.step).max(), Some(6));
    assert!(seen.iter().filter(|g| g.step.is_some()).all(|g| g.steps == Some(6)));

    let png = client.image(&created.id).unwrap();
    let picture = image::load_from_memory(&png).unwrap();
    assert_eq!((picture.width(), picture.height()), (1024, 1024));
    eprintln!("status after: {:?}", client.status().unwrap());

    let dir = std::env::temp_dir().join(format!("mmh3-studio-real-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let reference = dir.join("panda.png");
    std::fs::write(&reference, &png).unwrap();
    let edit = client.create_image(&small("<Picture 1> at night, lit by a paper lantern hanging from a branch", vec![reference])).unwrap();
    let end = watch(&client, &edit.id).pop().unwrap();
    assert_eq!(end.state, State::Done, "{:?}", end.error);
    let edited = image::load_from_memory(&client.image(&edit.id).unwrap()).unwrap();
    assert_eq!((edited.width(), edited.height()), (1024, 1024));
    let out = std::env::var("MMH3_STUDIO_KEEP").map(std::path::PathBuf::from).unwrap_or(dir.clone());
    std::fs::create_dir_all(&out).unwrap();
    std::fs::write(out.join("real-text.png"), &png).unwrap();
    edited.save(out.join("real-edit.png")).unwrap();

    let listed: Vec<String> = client.list().unwrap().into_iter().map(|g| g.id).collect();
    assert!(listed.starts_with(&[edit.id.clone(), created.id.clone()]), "{listed:?}");
    client.delete(&edit.id).unwrap();
    assert!(matches!(client.generation(&edit.id), Err(Error::Refused { status: 404, .. })));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
#[ignore = "needs a real mmh3 server"]
fn a_real_server_refuses_as_the_mock_does() {
    let client = server();
    let (content_type, body) = multipart(&[Part::Text { name: "steps", value: "4".into() }], "B");
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).timeout_global(Some(Duration::from_secs(5))).build().into();
    let mut answer = agent.post(&format!("{}/v1/images", client.base())).header("Content-Type", &content_type).send(&body[..]).unwrap();
    assert_eq!(answer.status().as_u16(), 400);
    assert_eq!(answer.body_mut().read_to_string().unwrap(), r#"{"error":"steps is not an option of a picture"}"#);
    assert!(matches!(client.generation("nope"), Err(Error::Refused { status: 404, ref message }) if message == "no such generation"));
}

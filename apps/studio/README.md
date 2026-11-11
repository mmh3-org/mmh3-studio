# mmh3 studio

A native macOS client for a resident `mmh3 server` running MiniMax-H3-Image. The server keeps the text encoder, DiT and VAE loaded, so a generation never pays a cold start. The studio never loads a model itself. It is built with [Rune](https://github.com/anomalyco/rune), pinned to a `dev` revision in `Cargo.toml`.

The studio is a member of the repository's Cargo workspace. The root flake's dev shell supplies Rust 1.99.1 and `just`; with direnv it loads on entering the repository.

## Running it

Start the server. The flake builds mmh3 with Metal, so `just server` runs it with the models in `$MMH3_MODELS`, or in `./models` when that is unset:

```sh
MMH3_MODELS=/path/to/models just server
```

Then start the studio, or run it without a checkout through `nix run github:ryoppippi/mmh3-studio`:

```sh
just studio::run
```

The studio connects to `http://127.0.0.1:8833`. To use another address, pass `--server http://host:port` or set `MMH3_SERVER`. If the server isn't running, the studio shows the command that starts it and tries again every two seconds.

The studio speaks the API in mmh3's `docs/server.md`: it asks for a picture at `POST /v1/images`, follows `GET /v1/generations/{id}/events`, and fetches the PNG from `GET /v1/generations/{id}/image`. The server answers with an id, a state, the step a generation is on and the seconds it took, and nothing of what was asked, so the studio keeps the prompt, size and seed of each generation it asked for itself.

To try the studio without the engine, run the mock server in place of `mmh3 server`. It answers the same routes with the same JSON, status codes and event stream: six timed steps, then a painted PNG. It checks a picture's fields as `mmh3 image` does, so a bad value fails the generation, and a prompt that contains `[fail]` fails after its steps.

```sh
just studio::mock-server   # in place of `just server`
just studio::run-mock      # or the mock and the studio together
```

## Using it

- **Composer:** write a prompt, then choose a size. The presets are 2048×2048 for a 4 MP draft, and 16 MP finals at 1:1, 4:3, 3:4, 16:9 and 9:16. A custom size takes multiples of 32, from 256 to 8192 pixels on each side. Set a seed or randomise it. To edit pictures, drop up to 16 PNG or JPEG files on the composer or add them with **Add picture…**; the prompt refers to them as `<Picture 1>`, `<Picture 2>` and so on, and clicking a picture's tag puts it in the prompt. The composer warns when the prompt mentions a picture that isn't attached. **Edit this picture** in the toolbar or a row's menu attaches a finished picture. ⌘↩ generates; plain Return starts a new line. You can queue as many generations as you like.
- **Gallery** (left pane): shows the pictures on the server, newest first, and leaves its videos out. A picture someone asked for with `curl` or another studio shows as asked for elsewhere, without a prompt or seed, since the server keeps neither. Each row shows what the generation is doing (reading the prompt, step k of 6, decoding) and the time since it started. Right-click a row to save the PNG, reveal it in Finder, copy the prompt or seed, re-run it with a new seed, or delete it. You can also drag a thumbnail out to Finder.
- **Preview:** the selected image at full size, with its size, seed and how long the server says the generation took. The server times the whole generation, not its stages. To zoom about the pointer, click, pinch or use Ctrl+wheel. To pan, drag or scroll. The buttons zoom in, zoom out and fit.
- **Status bar:** shows whether the server is connected, which models are resident, free and total memory, and the queue.

What the studio asked each server for is kept in `~/Library/Application Support/dev.mmh3.studio/requests`, one file a server, and forgotten once the server no longer lists the generation. Generated PNGs go into the app's cache folder (`~/Library/Caches/dev.mmh3.studio/images`). Each PNG is downloaded once. Thumbnails are decoded off the UI thread and scaled down to 256 px. Only the selected image is kept at full resolution, and Rune decodes it with mipmaps on its own workers.

## Checks

```sh
just studio::test        # client, mock, state machine and UI tests
just check               # formatting, typos, clippy and the tests, through Nix
just studio::screenshots # headless PNGs and element trees in screenshots/
just studio::test-real   # the client against a real server with the picture models
```

`test-real` generates two 1024×1024 pictures, the second an edit of the first, so it needs a server that serves pictures, and it uses the GPU.

The screenshot script first renders the fixture scenes, which never touch the network:

- `empty`
- `disconnected`
- `running`
- `gallery`
- `failed`
- `custom-size`
- `offline-selected`
- `confirm-delete`
- `references`

It then runs the studio headless with `--realtime` against the mock server. In that run, `scripts/generate.json` queues two 16 MP generations. To render a single scene:

```sh
../../target/release/mmh3-studio --snap out.png --scene gallery --theme dark --scale 2
```

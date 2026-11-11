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

## In a browser

The same app runs in a browser through Rune's experimental WebGPU target, so it needs a browser with WebGPU (current Chrome, Edge, Safari or Firefox) and a secure page: HTTPS, or plain HTTP on `localhost`. The page talks to the server on its own origin, at `v1/` beside the page, so it must be served from the same host as mmh3's `/v1` routes; mmh3 sends no CORS headers.

Build the static folder with Nix or with `just`:

```sh
nix build .#studio-web       # into ./result
just studio::web-build       # into target/web
```

To try it locally, serve the folder at `/` and pass `/v1/*` to the server with the small proxy in this repository, then open http://localhost:8000:

```sh
just server                  # or `just studio::mock-server` for the mock
just studio::web-build
just studio::web-serve       # target/web on 127.0.0.1:8000, /v1 to 127.0.0.1:8833
```

`web-serve` takes the folder, the address to listen on and the server's address, in that order, relative to `apps/studio`: `just studio::web-serve ../../result 127.0.0.1:8000 127.0.0.1:8833`. While working on the web build, `just studio::web-dev` rebuilds an unoptimised build into `target/web-live` on every change and reloads the page; serve that folder with `just studio::web-serve ../../target/web-live`.

To use it from your other devices, let [Tailscale Serve](https://tailscale.com/kb/1312/serve) put both on your tailnet's HTTPS name, the folder at `/` and the server's routes at `/v1`:

```sh
tailscale serve --bg --set-path / "$(nix build .#studio-web --print-out-paths)"
tailscale serve --bg --set-path /v1 http://127.0.0.1:8833/v1
tailscale serve status       # prints the https://<machine>.<tailnet>.ts.net/ address
```

If your Tailscale can't serve a folder (the sandboxed macOS app may not), serve the local proxy instead: run `just studio::web-serve` and then `tailscale serve --bg 8000`. `tailscale serve reset` takes either away. Serve stays inside your tailnet; don't use `tailscale funnel`, since the server has no authentication.

What changes in a browser:

- There is no file system. The first visit downloads each finished picture once to make its thumbnail, which the browser decodes and scales off the main thread; the thumbnails are kept in the browser's Cache Storage, so a later visit downloads none of them. Selecting a picture downloads it at full size for the preview, unless it was just downloaded for you. The browser decodes the preview off the main thread too, but copying a 16 MP picture's pixels into the page still takes the main thread a moment. Cache Storage needs a secure context, which `localhost` and `tailscale serve` are. What the studio asked for is kept in the browser's local storage for that address.
- **Save PNG** downloads the picture, and there is no **Reveal in Finder** or dragging out.
- Add pictures to edit with **Add picture…** or by dropping them anywhere on the page. They are kept for as long as the page is open, as is a picture attached with **Edit this picture**, so a re-run after a reload asks you to attach them again.
- In a window narrower than 720 points, such as a phone, the gallery, the picture and the composer take turns, under a switcher at the top.

## Using it

- **Composer:** write a prompt, then choose a size. The presets are 2048×2048 for a 4 MP draft, and 16 MP finals at 1:1, 4:3, 3:4, 16:9 and 9:16. A custom size takes multiples of 32, from 256 to 8192 pixels on each side. Set a seed or randomise it. To edit pictures, drop up to 16 PNG or JPEG files on the composer or add them with **Add picture…**; the prompt refers to them as `<Picture 1>`, `<Picture 2>` and so on, and clicking a picture's tag puts it in the prompt. The composer warns when the prompt mentions a picture that isn't attached. **Edit this picture** in the toolbar or a row's menu attaches a finished picture. ⌘↩ generates; plain Return starts a new line. You can queue as many generations as you like.
- **Gallery** (left pane): shows the pictures on the server, newest first, and leaves its videos out. A picture someone asked for with `curl` or another studio shows as asked for elsewhere, without a prompt or seed, since the server keeps neither. Each row shows what the generation is doing (reading the prompt, step k of 6, decoding) and the time since it started. Right-click a row to save the PNG, reveal it in Finder, copy the prompt or seed, re-run it with a new seed, or delete it. You can also drag a thumbnail out to Finder.
- **Preview:** the selected image at full size, with its size, seed and how long the server says the generation took. The server times the whole generation, not its stages. To zoom about the pointer, click, pinch or use Ctrl+wheel. To pan, drag or scroll. The buttons zoom in, zoom out and fit.
- **Status bar:** shows whether the server is connected, which models are resident, free and total memory, and the queue. On the right it shows the Mac's Energy Mode, its power source and any heat.
- **Power banner:** macOS holds the GPU at a low clock in Low Power or Automatic Energy Mode (on a Mac that offers High Power Mode), on battery, on an adapter under 140 W with a Max chip, while the battery drains on the adapter, or when it reports a serious thermal state. A generation then often takes two to three times as long, so a banner above the status bar says why. **Open Energy settings** opens the Battery pane of System Settings, and **Copy command** copies `sudo pmset -a powermode 2`, which turns on High Power Mode; the studio never runs it itself. The studio reads `pmset` and `NSProcessInfo` every 30 seconds and when its window comes to the front, without root. Closing the banner keeps it closed until something else slows the GPU. The web build doesn't read the power state.

What the studio asked each server for is kept in `~/Library/Application Support/dev.mmh3.studio/requests`, one file a server, and forgotten once the server no longer lists the generation. Generated PNGs go into the app's cache folder (`~/Library/Caches/dev.mmh3.studio/images`) and leave it with their generation. A generated picture attached with **Edit this picture** is copied to `~/Library/Application Support/dev.mmh3.studio/references` first, so the edit and its re-runs keep it, and the copy is removed at the next start once no remembered request uses it. Each PNG is downloaded once. Thumbnails are decoded off the UI thread and scaled down to 256 px. Only the selected image is kept at full resolution, and Rune decodes it with mipmaps on its own workers.

## Checks

The flake's checks build the web studio as well, which is what checks the web-only code compiles.

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

#!/bin/sh
# Renders every scene, then a live run against the mock server, into screenshots/.
# Run from the app's folder after `cargo build --release`.
set -eu
bin=${CARGO_TARGET_DIR:-../../target}/release
app=$bin/mmh3-studio
root=$bin/screenshot-dirs
mkdir -p screenshots
for scene in empty disconnected running previewing cancelled gallery failed custom-size offline-selected confirm-delete timing references power effort effort-small effort-custom effort-details; do
  for theme in light dark; do
    $app --snap "screenshots/scene-$scene-$theme.png" --scene "$scene" --size 1320x860 --scale 2 --theme "$theme" --at 1.0 >/dev/null
  done
done
$app --tree screenshots/scene-gallery-tree.json --scene gallery --at 1.0 >/dev/null
for scene in disconnected running previewing cancelled gallery timing failed offline-selected power effort-custom; do
  $app --snap "screenshots/small-$scene.png" --scene "$scene" --size 1040x680 --scale 2 --at 1.0 >/dev/null
done
# A phone-wide window shows the composer alone; tall enough to show the whole of Custom.
for scene in effort effort-small effort-custom; do
  $app --snap "screenshots/phone-$scene.png" --scene "$scene" --size 390x1500 --scale 2 --at 1.0 >/dev/null
done

live() {
  name=$1
  mock=$2
  shift 2
  # shellcheck disable=SC2086 # the mock's flags are words of their own
  "$bin/mock-server" --listen 127.0.0.1:8899 $mock >/dev/null &
  pid=$!
  sleep 0.4
  RUNE_DIRS_ROOT="$root/$name-$$" $app --server http://127.0.0.1:8899 --realtime --scale 2 "$@" >/dev/null
  kill "$pid"
}
for theme in light dark; do
  live "progress-$theme" "" --script scripts/generate.json --snap "screenshots/live-progress-$theme.png" --tree "screenshots/live-progress-$theme-tree.json" --at mark:sent+2.0 --theme "$theme"
  live "gallery-$theme" "" --script scripts/generate.json --snap "screenshots/live-gallery-$theme.png" --at mark:sent+14.0 --theme "$theme"
  live "stopped-$theme" "" --script scripts/stop.json --snap "screenshots/live-stopped-$theme.png" --at mark:stopped+2.0 --theme "$theme"
  live "upstream-$theme" "--upstream" --script scripts/generate.json --snap "screenshots/live-upstream-$theme.png" --at mark:sent+2.0 --theme "$theme"
  RUNE_DIRS_ROOT="$root/off-$$" $app --server http://127.0.0.1:1 --realtime --snap "screenshots/live-disconnected-$theme.png" --at 2.0 --theme "$theme" --scale 2 >/dev/null
done

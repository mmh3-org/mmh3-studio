# The studio's web build as a static folder: the wasm, its JS glue, the page
# and the fonts Rune fetches after the first frame. It does what Rune's
# scripts/web-build.sh does, which isn't part of the vendored crates.
{
  craneLib,
  lib,
  studio,
  wasmBindgenCli,
}:
let
  inherit (studio.passthru) commonArgs;
  webArgs = commonArgs // {
    pname = "mmh3-studio-web";
    cargoExtraArgs = "--locked -p mmh3-studio-web --lib";
    CARGO_BUILD_TARGET = "wasm32-unknown-unknown";
    # web-build.sh's size settings for a release build.
    CARGO_PROFILE_RELEASE_OPT_LEVEL = "s";
    CARGO_PROFILE_RELEASE_LTO = "thin";
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS = "1";
    CARGO_PROFILE_RELEASE_DEBUG = "0";
    CARGO_PROFILE_RELEASE_STRIP = "debuginfo";
    doCheck = false;
  };
in
craneLib.buildPackage (
  webArgs
  // {
    cargoArtifacts = craneLib.buildDepsOnly webArgs;
    nativeBuildInputs = [ wasmBindgenCli ];
    installPhaseCommand = ''
      mkdir -p $out
      wasm-bindgen --target web --out-dir $out --out-name app --remove-name-section \
        target/wasm32-unknown-unknown/release/mmh3_studio_web.wasm
      cp ${../apps/studio-web/index.html} $out/index.html
      fonts=$(dirname "$(find -L ${commonArgs.cargoVendorDir} -path '*/assets/web/Inter-Regular.ttf' -not -path '*/first/*' | head -n 1)")
      cp "$fonts"/Inter-*.ttf $out/
    '';
    meta.description = "mmh3 studio for the browser, to serve beside mmh3 server's /v1 routes";
  }
)

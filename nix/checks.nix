# Formatting is checked by treefmt-nix and typos by the git-hooks check; this
# adds what needs the workspace compiled. Neither runs anything that wants a
# GPU or a window: the studio's tests drive its UI headless.
{
  perSystem =
    { config, craneLib, ... }:
    let
      inherit (config.packages.studio.passthru) commonArgs cargoArtifacts;
    in
    {
      checks = {
        studio-clippy = craneLib.cargoClippy (
          commonArgs
          // {
            inherit cargoArtifacts;
            cargoClippyExtraArgs = "--all-targets -- -D warnings";
          }
        );
        studio-test = craneLib.cargoTest (
          commonArgs
          // {
            inherit cargoArtifacts;
            # The client, proxy and mock tests talk over loopback sockets,
            # which Darwin's sandbox denies unless the build asks for them.
            __darwinAllowLocalNetworking = true;
          }
        );
        # The wasm build is the only check that the web-only code compiles.
        studio-web = config.packages.studio-web;
      };
    };
}

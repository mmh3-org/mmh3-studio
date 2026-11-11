# Formatting is checked by treefmt-nix and typos by the git-hooks check; this
# adds what needs the workspace compiled. Neither runs anything that wants a
# GPU or a window: the studio's tests drive its UI headless.
{ inputs, ... }:
{
  perSystem =
    {
      config,
      craneLib,
      pkgs,
      ...
    }:
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
        # The flake's mmh3 is the server the README tells people to run, so
        # it must answer every route the studio calls.
        mmh3-routes = pkgs.runCommand "mmh3-routes" { } ''
          for route in /v1/status /v1/generations /v1/images '/v1/generations/{id}' \
            '/v1/generations/{id}/events' '/v1/generations/{id}/image'; do
            grep -qF ".route(\"$route\"" ${inputs.mmh3}/src/server.rs \
              || { echo "the pinned mmh3 server has no $route route" >&2; exit 1; }
          done
          touch $out
        '';
        # The wasm build is the only check that the web-only code compiles.
        studio-web = config.packages.studio-web;
      };
    };
}

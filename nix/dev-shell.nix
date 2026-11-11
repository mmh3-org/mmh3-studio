{
  perSystem =
    {
      config,
      lib,
      pkgs,
      rustToolchain,
      ...
    }:
    let
      # wasm-bindgen refuses to process a module built by another version of
      # the library, so the CLI follows whatever Cargo.lock resolved.
      lockedPackages = (lib.importTOML ../Cargo.lock).package;
      wasmBindgen = lib.findFirst (
        p: p.name == "wasm-bindgen"
      ) (throw "Cargo.lock has no wasm-bindgen") lockedPackages;
      wasmBindgenVersion = wasmBindgen.version;
      wasmBindgenCli = pkgs."wasm-bindgen-cli_${lib.replaceStrings [ "." ] [ "_" ] wasmBindgenVersion}";
    in
    {
      devShells.default = pkgs.mkShell {
        packages = [
          rustToolchain
          wasmBindgenCli
          config.treefmt.build.wrapper
          pkgs.just
          pkgs.miniserve
          pkgs.nixfmt
          pkgs.typos
        ]
        ++ config.pre-commit.settings.enabledPackages;

        shellHook = config.pre-commit.shellHook;
      };
    };
}

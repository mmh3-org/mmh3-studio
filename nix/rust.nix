# One nixpkgs with rust-overlay and one Crane library on the repository's
# pinned toolchain, shared by every other module through module arguments.
{ inputs, ... }:
{
  perSystem =
    { lib, system, ... }:
    let
      pkgs = import inputs.nixpkgs {
        inherit system;
        overlays = [ inputs.rust-overlay.overlays.default ];
      };
      rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ../rust-toolchain.toml;
      # wasm-bindgen refuses to process a module built by another version of
      # the library, so the CLI follows whatever Cargo.lock resolved.
      wasmBindgen = lib.findFirst (
        p: p.name == "wasm-bindgen"
      ) (throw "Cargo.lock has no wasm-bindgen") (lib.importTOML ../Cargo.lock).package;
      wasmBindgenCli = pkgs."wasm-bindgen-cli_${lib.replaceStrings [ "." ] [ "_" ] wasmBindgen.version}";
    in
    {
      _module.args = {
        inherit pkgs rustToolchain wasmBindgenCli;
        craneLib = (inputs.crane.mkLib pkgs).overrideToolchain rustToolchain;
      };
    };
}

# One nixpkgs with rust-overlay and one Crane library on the repository's
# pinned toolchain, shared by every other module through module arguments.
{ inputs, ... }:
{
  perSystem =
    { system, ... }:
    let
      pkgs = import inputs.nixpkgs {
        inherit system;
        overlays = [ inputs.rust-overlay.overlays.default ];
      };
      rustToolchain = pkgs.rust-bin.fromRustupToolchainFile ../rust-toolchain.toml;
    in
    {
      _module.args = {
        inherit pkgs rustToolchain;
        craneLib = (inputs.crane.mkLib pkgs).overrideToolchain rustToolchain;
      };
    };
}

{
  description = "mmh3 studio: a Rune client for mmh3's MiniMax-H3-Image server";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixpkgs-unstable";
    crane.url = "github:ipetkov/crane";
    flake-parts.url = "github:hercules-ci/flake-parts";
    git-hooks = {
      url = "github:cachix/git-hooks.nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    # The engine the studio talks to. Upstream generates pictures on CUDA
    # only; metal-image is the fork's work-in-progress port to Metal, kept on
    # top of upstream master and its /v1/images API.
    mmh3 = {
      url = "github:ryoppippi/mmh3/metal-image";
      flake = false;
    };
    rust-overlay = {
      url = "github:oxalica/rust-overlay";
      inputs.nixpkgs.follows = "nixpkgs";
    };
    treefmt-nix = {
      url = "github:numtide/treefmt-nix";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    inputs@{ flake-parts, ... }:
    flake-parts.lib.mkFlake { inherit inputs; } {
      # The studio is a macOS app and mmh3 is built for Metal here.
      systems = [ "aarch64-darwin" ];

      imports = [
        inputs.treefmt-nix.flakeModule
        inputs.git-hooks.flakeModule
        ./nix/rust.nix
        ./nix/packages.nix
        ./nix/treefmt.nix
        ./nix/git-hooks.nix
        ./nix/checks.nix
        ./nix/dev-shell.nix
      ];
    };
}

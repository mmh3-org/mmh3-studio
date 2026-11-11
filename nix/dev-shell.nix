{
  perSystem =
    {
      config,
      pkgs,
      rustToolchain,
      wasmBindgenCli,
      ...
    }:
    {
      devShells.default = pkgs.mkShell {
        packages = [
          rustToolchain
          wasmBindgenCli
          config.treefmt.build.wrapper
          pkgs.jq
          pkgs.just
          pkgs.miniserve
          # Rune's web-build.sh reads Cargo.lock with tomllib.
          pkgs.python3
          pkgs.nixfmt
          pkgs.typos
        ]
        ++ config.pre-commit.settings.enabledPackages;

        shellHook = config.pre-commit.shellHook;
      };
    };
}

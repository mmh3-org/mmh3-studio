{
  perSystem =
    { config, pkgs, ... }:
    {
      # nixpkgs' pre-commit pulls in a .NET SDK that Darwin builds from
      # source; prek runs the same configuration without it.
      pre-commit.settings.package = pkgs.prek;
      pre-commit.settings.hooks = {
        treefmt = {
          enable = true;
          package = config.treefmt.build.wrapper;
        };
        typos.enable = true;
      };
    };
}

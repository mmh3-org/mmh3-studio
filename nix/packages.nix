{ inputs, ... }:
{
  perSystem =
    {
      config,
      craneLib,
      lib,
      pkgs,
      rustToolchain,
      ...
    }:
    {
      packages = {
        default = config.packages.studio;
        studio = import ./studio.nix { inherit craneLib lib; };
        mmh3 = import ./mmh3.nix {
          inherit
            craneLib
            lib
            pkgs
            rustToolchain
            ;
          src = inputs.mmh3;
        };
      };

      apps = {
        default = config.apps.studio;
        studio = {
          type = "app";
          program = lib.getExe config.packages.studio;
          meta.description = "The mmh3 studio window";
        };
        mmh3 = {
          type = "app";
          program = lib.getExe config.packages.mmh3;
          meta.description = "mmh3, built with Metal and its server";
        };
      };
    };
}

{
  craneLib,
  lib,
}:
let
  root = ./..;
  src = lib.fileset.toSource {
    inherit root;
    fileset = lib.fileset.unions [
      (craneLib.fileset.commonCargoSources root)
      (root + /apps/studio/fixtures)
    ];
  };
  # Rune vendors wgpu-hal as published, Cargo.toml.orig included, and
  # `cargo package` (which Crane uses to pick each crate's files out of a git
  # checkout) refuses a source holding that reserved name.
  cargoVendorDir = craneLib.vendorCargoDeps {
    inherit src;
    overrideVendorGitCheckout =
      packages: drv:
      if lib.any (p: lib.hasPrefix "git+https://github.com/anomalyco/rune" p.source) packages then
        drv.overrideAttrs { postPatch = "rm vendor/wgpu-hal/Cargo.toml.orig"; }
      else
        drv;
  };
  commonArgs = {
    inherit src cargoVendorDir;
    pname = "mmh3-studio";
    version = (lib.importTOML (root + /Cargo.toml)).workspace.package.version;
    strictDeps = true;
    cargoExtraArgs = "--locked -p mmh3-studio";
  };
  cargoArtifacts = craneLib.buildDepsOnly commonArgs;
in
craneLib.buildPackage (
  commonArgs
  // {
    inherit cargoArtifacts;
    # The checks run the tests; the package only has to build.
    doCheck = false;
    passthru = { inherit commonArgs cargoArtifacts; };
    meta.mainProgram = "mmh3-studio";
  }
)

{
  craneLib,
  lib,
  pkgs,
  rustToolchain,
  src,
}:
let
  # A source that pins its own toolchain is built with it; otherwise it gets
  # the repository's.
  toolchainFile = src + /rust-toolchain.toml;
  mmh3CraneLib =
    if builtins.pathExists toolchainFile then
      craneLib.overrideToolchain (pkgs.rust-bin.fromRustupToolchainFile toolchainFile)
    else
      craneLib.overrideToolchain rustToolchain;
  systemXcrun = pkgs.runCommand "system-xcrun" { } ''
    mkdir -p "$out/bin"
    ln -s /usr/bin/xcrun "$out/bin/xcrun"
  '';
  commonArgs = {
    inherit src;
    pname = "mmh3";
    version = (lib.importTOML (src + /Cargo.toml)).workspace.package.version;
    strictDeps = true;
    cargoExtraArgs = "--locked --features metal,server --bin mmh3";
    # rustc strips release proc macros with a rust-objcopy whose output
    # macOS 27's dyld refuses to load; they never ship, so leave them be.
    CARGO_PROFILE_RELEASE_BUILD_OVERRIDE_STRIP = "false";
    # The Nix linker does not find the Swift runtime libraries in Xcode's SDK,
    # so Xcode's own clang links the binary, as it does in a native build.
    CARGO_TARGET_AARCH64_APPLE_DARWIN_LINKER = "/usr/bin/cc";
    # The Metal and Swift runtimes are compiled by Xcode's swiftc through
    # xcrun, which needs the real Xcode rather than the Nix Apple SDK. Only
    # xcrun is taken from /usr/bin: the rest of it would shadow GNU tools.
    preBuild = ''
      # xcode-select reports DEVELOPER_DIR when it is set, and the Apple SDK's
      # setup hook points it into the store.
      unset SDKROOT DEVELOPER_DIR
      DEVELOPER_DIR="$(/usr/bin/xcode-select -p)"
      export DEVELOPER_DIR
      export PATH="${systemXcrun}/bin:$PATH"
    '';
  };
in
mmh3CraneLib.buildPackage (
  commonArgs
  // {
    cargoArtifacts = mmh3CraneLib.buildDepsOnly commonArgs;
    doCheck = false;
    meta.mainProgram = "mmh3";
  }
)

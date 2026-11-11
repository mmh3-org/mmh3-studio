# Task runner for the repository.
#
# Each app has its own justfile, imported below as a module; whole-repo jobs
# that the Nix flake owns (formatting, checks, the engine) stay here.
#
# Run `just --list` (or `just --list <module>`) to see everything.

mod studio 'apps/studio'

[private]
default:
    @just --list

# Format the whole tree (Rust, Nix, TOML) via treefmt
fmt:
    nix fmt

# Run every flake check (treefmt, typos, clippy, tests)
check:
    nix flake check

# Run the Rust workspace tests
test:
    cargo test --workspace --release

# Build the studio and the engine through Nix
build:
    nix build .#studio .#mmh3

# Start the engine's server with the models in $MMH3_MODELS (default: ./models)
[positional-arguments]
server *args:
    MMH3_MODELS="${MMH3_MODELS:-$PWD/models}" nix run .#mmh3 -- server "$@"

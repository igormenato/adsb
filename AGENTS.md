# trck

## Nix

`nix develop` opens the shell.

`nix flake check` builds the Pi Zero binary and runs the host tests.

`nix build` writes `result/bin/trck-pi-zero-w`.

## Cargo

In that shell, in this order:

1. `cargo fmt --check`
2. `cargo clippy --locked --all-targets -- -D warnings`
3. `cargo test --locked`

These run on the host. They do not build the Pi Zero binary. `cargo fmt` writes the format when the check fails.

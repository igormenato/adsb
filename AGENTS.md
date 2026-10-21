# trck

## Setup commands

Enter the shell with `nix develop`.

That shell loads stable Rust from `rust-toolchain.toml`, including the `arm-unknown-linux-musleabihf` target. It points that target's linker at the Nix `rust-lld`. It sets rustflags to `crt-static`, `ld.lld`, and `link-self-contained`. The release does not need a C sysroot.

In that shell, `cargo build --release --target arm-unknown-linux-musleabihf` uses those settings.

## Testing instructions

Prove a change with `cargo test --locked`.

That runs the host tests. It does not build or run the Pi Zero binary.

## Release

`nix build` writes `result/bin/trck-pi-zero-w`. That file is the package binary `trck`. The build does not run tests.

`nix flake check` builds the Pi Zero binary and runs the host tests.

`.github/workflows/release.yml` runs `nix flake check` and `nix build` on workflow dispatch.

## Cargo

The edition is 2024. `Cargo.lock` is checked in. `cargo test --locked` requires that lockfile to match `Cargo.toml`.

`clap` enables the `derive` feature. `serialport` sets `default-features` to false. The package lists no features of its own.

## Frame rules

`FRAME_LEN` is 30. `pack_frame` is the only encoder.

The live read does not block on the UART. Finished frames wait in a queue of 64.

`07700` is not `7700`.

`--check` conflicts with `--net-json`.

Do not bring back `aircraft.json`.

The release is one static musl binary, `trck-pi-zero-w`. Do not add a QEMU suite.

The field layout is in `README.md`, `docs/esp32-alert.md`, and `pack_frame`.

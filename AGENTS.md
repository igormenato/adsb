# trck

Follow @docs/rust-guidelines.md

## Cargo

In this order:

1. `cargo fmt --check`
2. `cargo clippy --locked --all-targets -- -D warnings`
3. `cargo test --locked`

The Pi binary, on an aarch64 host:

`cargo build --release --locked --target aarch64-unknown-linux-musl`

Copy `target/aarch64-unknown-linux-musl/release/trck`. The release asset is `trck-aarch64`. On x86, use `cross` or a similar toolchain.

## Release

A push to `main` opens or updates a release PR. Merging it tags `v` plus the crate version and attaches `trck-aarch64`. Do not push tags by hand.

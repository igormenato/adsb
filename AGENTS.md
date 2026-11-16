# trck

Follow @docs/rust-guidelines.md

## Cargo

In this order:

1. `cargo fmt --check`
2. `cargo clippy --locked --all-targets -- -D warnings`
3. `cargo test --locked`

The Pi binary (Zig plus `cargo-zigbuild`):

`cargo zigbuild --release --locked --target aarch64-unknown-linux-musl`

Copy `target/aarch64-unknown-linux-musl/release/trck`. The release asset is `trck-aarch64`.

## Release

A push to `trck-v2` opens or updates a release PR. Merging it tags `v` plus the crate version and attaches `trck-aarch64`. Do not push tags by hand.

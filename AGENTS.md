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

Bump the crate version on `main`. Tag `v` plus that version and push the tag. GitHub Actions publishes `trck-aarch64`. Do not `workflow_dispatch`; a tag push is enough.

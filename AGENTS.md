# trck

Follow docs/rust-guidelines.md

## Cargo

In this order:

1. `cargo fmt --check`
2. `cargo clippy --locked --all-targets -- -D warnings`
3. `cargo test --locked`

The Pi binaries:

`cargo build --release --locked --target arm-unknown-linux-musleabihf`

`cargo build --release --locked --target aarch64-unknown-linux-gnu`

Copy `target/<triple>/release/trck`. Release assets are `trck-arm` and `trck-aarch64`.

## Release

Bump the crate version on `main`. Tag `v` plus that version and push the tag. GitHub Actions publishes `trck-arm` and `trck-aarch64`. Do not `workflow_dispatch`; a tag push is enough.

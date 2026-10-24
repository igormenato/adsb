# trck

## Cargo

In this order:

1. `cargo fmt --check`
2. `cargo clippy --locked --all-targets -- -D warnings`
3. `cargo test --locked`

The Pi binaries:

`cargo build --release --locked --target arm-unknown-linux-musleabihf`

`cargo build --release --locked --target aarch64-unknown-linux-musl`

Copy `target/<triple>/release/trck`.

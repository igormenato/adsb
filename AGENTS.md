# trck

## Cargo

On Fedora, in this order:

1. `cargo fmt --check`
2. `cargo clippy --locked --all-targets -- -D warnings`
3. `cargo test --locked`

The Pi Zero W binary:

`cargo build --release --locked --target arm-unknown-linux-musleabihf`

Copy `target/arm-unknown-linux-musleabihf/release/trck` to the Pi as `trck-pi-zero-w`.

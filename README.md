# trck

`trck` writes one UART frame for each airborne position decoded from 1090 MHz ADS-B.

## Build

Run the host checks in this order.

1. `cargo fmt --check`
2. `cargo clippy --locked --all-targets -- -D warnings`
3. `cargo test --locked`

Build the Pi binaries. Those targets compile C (zstd, from rs1090). CI uses Zig as `zig cc`.

```
cargo build --release --locked --target arm-unknown-linux-musleabihf
cargo build --release --locked --target aarch64-unknown-linux-musl
```

Copy `target/<triple>/release/trck`. `arm-unknown-linux-musleabihf` is the Zero W. `aarch64-unknown-linux-musl` is the Pi 3B on a 64-bit OS. Release assets are `trck-arm` and `trck-aarch64`.

## Run

Disable the serial login shell and leave the UART enabled. The console shares the port if you do not.

```
./trck
```

That opens RTL-SDR device 0 at 1090 MHz and writes frames to `/dev/serial0`.

## Flags

`--rtlsdr N` selects the dongle. Default is `0`.

`--hex HOST:PORT` reads newline hex Mode S frames instead of the dongle.

`--uart` defaults to `/dev/serial0`. Pass `--uart -` to write frames to stdout.

`--baud` defaults to 115200.

`--check` sends known frames instead of reading the radio, to check the UART. It repeats those frames every second.

## Frame

The layout is in [ESP32 satellite telemetry](docs/esp32-alert.md).

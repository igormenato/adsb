# trck

`trck` writes one UART frame for each airborne position decoded from 1090 MHz ADS-B.

## Build

Run the host checks in this order.

1. `cargo fmt --check`
2. `cargo clippy --locked --all-targets -- -D warnings`
3. `cargo test --locked`

Build the Pi binaries. Raspberry Pi OS Lite is Debian 13 (glibc 2.41). The Zero W is ARMv6, so `trck-arm` stays musl; rustc's `arm-unknown-linux-gnueabihf` is ARMv7 and would SIGILL on that chip. The Pi 3B 64-bit Lite binary is glibc, built on Ubuntu 24.04 (glibc 2.39). CI uses Zig as `zig cc` for the Zero musl C (zstd) and `gcc-aarch64-linux-gnu` for the 3B.

```
cargo build --release --locked --target arm-unknown-linux-musleabihf
cargo build --release --locked --target aarch64-unknown-linux-gnu
```

Copy `target/<triple>/release/trck`. Release assets are `trck-arm` and `trck-aarch64`.

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

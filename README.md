# trck

`trck` writes one UART frame for each position readsb accepts on `--net-json-port`.

## Build

Run the host checks in this order.

1. `cargo fmt --check`
2. `cargo clippy --locked --all-targets -- -D warnings`
3. `cargo test --locked`

Build the Pi binaries.

```
cargo build --release --locked --target arm-unknown-linux-musleabihf
cargo build --release --locked --target aarch64-unknown-linux-musl
```

Copy `target/<triple>/release/trck`. `arm-unknown-linux-musleabihf` is the Zero W. `aarch64-unknown-linux-musl` is the Pi 3B on a 64-bit OS. Release assets are `trck-arm` and `trck-aarch64`.

## Run

Disable the serial login shell and leave the UART enabled. The console shares the port if you do not.

```
readsb --net-json-port 30005
./trck --net-json 127.0.0.1:30005
```

`30005` is an example. Pass the same host and port readsb is listening on.

## Flags

`--net-json HOST:PORT` is the live input. Required unless `--check`.

`--uart` defaults to `/dev/serial0`. Pass `--uart -` to write frames to stdout.

`--baud` defaults to 115200.

`--check` sends known frames instead of reading readsb, to check the UART. It repeats those frames every second.

## Frame

The layout is in [ESP32 satellite telemetry](docs/esp32-alert.md).

## ESP32 UART ingest

`esp32/trck_uart` is an Arduino-ESP32 3.3.12 sketch for an ESP32-S3. It reads those frames and prints ICAO, lat, lon, alt, and emergency on `Serial`. Flash it, wire Pi GPIO 14 (pin 8) to GPIO 18, 3.3 V, common ground, and run `trck --check`. That firmware is UART ingest, not the OBC radio. Board settings are in [esp32/README.md](esp32/README.md).

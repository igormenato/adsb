# trck

`trck` decodes 1090 MHz ADS-B on a Raspberry Pi and writes one UART frame for each airborne position. Default live path is RTL-SDR device 0 at 1090 MHz into `/dev/serial0` at 115200 baud. The 34-byte `TRCK` layout is in [ESP32 satellite telemetry](docs/esp32-alert.md). Every flag is in `trck --help`.

## Hardware

```
./trck doctor
```

The command prints one line per host check and exits non-zero if a required check fails. It does not send UART frames.

Wire 3.3 V UART with a common ground. Do not use a 5 V adapter. Leave the serial login shell off so the console does not share `/dev/serial0`.

| Pi | ESP32-S3 |
| --- | --- |
| GPIO 14 (pin 8) TX | GPIO 11 RX |
| GPIO 15 (pin 10) RX | GPIO 10 TX |
| GND (pin 6) | GND |

## Build

```
cargo zigbuild --release --locked --target aarch64-unknown-linux-musl
```

Install Zig, then `cargo install --locked cargo-zigbuild`. Copy `target/aarch64-unknown-linux-musl/release/trck`. That static musl binary is the Zero 2 W and the Pi 3B on 64-bit Lite. The release asset is `trck-aarch64`.

## Run on the Pi

```
./trck
```

Tuner gain defaults to 49.6 dB. `--gain auto` is tuner AGC. `--uart -` writes frames to stdout. `--table` defaults to `aircraft.csv`. `--allow-power-commands` lets a UART `REBOOT` or `SHUTDOWN` run `systemctl reboot` or `systemctl poweroff`.

```
./trck --check
```

Known frames repeat every second. Use this to prove the UART without a radio.

```
nc localhost 30002 | ./trck --hex - --uart /dev/serial0
```

`--hex` reads newline hex or AVR Mode S (`*8D...;` and `@` timestamp lines) from dump1090 `--net` port 30002, readsb, or rtl_adsb. `--hex HOST:PORT` opens that socket. `--hex -` reads stdin.

```
./trck --iq capture.cu8 --uart -
```

`--iq` is an RTL-SDR cu8 capture on the same `demodulate2400` path as the dongle. `--iq-rate` is the file sample rate and defaults to 2400000.

## ESP32

```
pio run -d esp32/pio -t upload
```

Board id is `esp32-s3-devkitc-1`. Arduino IDE settings, Serial words, and firmware trees are in [ESP32-S3 UART ingest](esp32/README.md).

```
make -C esp32/host test
```

## Test

```
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

```
scripts/fetch-iq.sh
cargo test --locked -- --ignored
```

The script puts public recordings in gitignored `tests/data/`. Sources are [antirez/dump1090](https://github.com/antirez/dump1090) `testfiles/modes1.bin` and the Porto AWEC 2026 set ([DOI 10.5281/zenodo.23342597](https://doi.org/10.5281/zenodo.23342597)).

```
./trck sim examples/scenarios/crossing.toml
./trck replay FILE --uart -
./trck stress --uart - --aircraft 32 --rate 0 --seconds 10
```

`sim` writes `ts,hex` lines. `replay` runs those lines through the live decoder. `stress` generates Mode S in-process. `--speed` on `replay` scales timing. `0` or `max` is as fast as possible.

## Release

A push to `main` opens or updates a release PR. Merging that PR tags `v` plus the crate version and attaches `trck-aarch64`. Do not push tags by hand.

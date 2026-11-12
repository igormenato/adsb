# trck

`trck` writes one UART frame for each airborne position decoded from 1090 MHz ADS-B.

## Build

Run the host checks in this order.

1. `cargo fmt --check`
2. `cargo clippy --locked --all-targets -- -D warnings`
3. `cargo test --locked`

Build the Pi binaries. Both are static musl so they copy onto Raspberry Pi OS Lite without matching glibc. Those targets compile C (zstd, from rs1090). Install Zig, then `cargo install --locked cargo-zigbuild`. CI uses `mlugg/setup-zig`.

```
cargo zigbuild --release --locked --target arm-unknown-linux-musleabihf
cargo zigbuild --release --locked --target aarch64-unknown-linux-musl
```

Copy `target/<triple>/release/trck`. `arm-unknown-linux-musleabihf` is the Zero W. `aarch64-unknown-linux-musl` is the Pi 3B on 64-bit Lite. Release assets are `trck-arm` and `trck-aarch64`.

## Run

Disable the serial login shell and leave the UART enabled. The console shares the port if you do not.

```
./trck
```

That opens RTL-SDR device 0 at 1090 MHz and writes frames to `/dev/serial0`.

`./trck doctor` probes this machine: RTL-SDR USB (nusb, not readsb), `/dev/serial0`, serial console, `aircraft.csv`, plugdev, and the clock. One line per check. Non-zero exit if a required check fails. It does not send UART frames; `--check` does that.

## Flags

`--rtlsdr N` selects the dongle. Default is `0`.

`--hex HOST:PORT` reads newline hex Mode S frames instead of the dongle.

`--uart` defaults to `/dev/serial0`. Pass `--uart -` to write frames to stdout.

`--baud` defaults to 115200.

`--check` sends known frames instead of reading the radio, to check the UART. It repeats those frames every second.

`--table PATH` appends one CSV row for each position queued for UART. Default is `aircraft.csv` in the working directory. The file stays append-only for the process. Timestamp is the Pi clock, not contest reception.

`doctor` is a host probe. It does not open the radio or the UART.

## IQ

`--iq FILE` reads an RTL-SDR cu8 capture (interleaved unsigned 8-bit I/Q) and runs the same `demodulate2400` path as the dongle. `--iq-rate` is the file's sample rate. Default is 2400000. Any other rate is resampled to 2.4 MS/s before demodulation.

```
rtl_sdr -f 1090e6 -s 2.4e6 capture.cu8
./trck --iq capture.cu8 --uart -
./trck --iq capture.cu8 --iq-rate 2000000 --uart -
```

`trck sim --iq-out FILE` writes a synthetic cu8 file from a scenario. `--iq-snr` adds seeded Gaussian noise.

`scripts/fetch-iq.sh` downloads two public recordings into gitignored `tests/data/`. Then `cargo test -- --ignored` runs them. [antirez/dump1090](https://github.com/antirez/dump1090) `testfiles/modes1.bin` is cu8 at 2 MS/s, packed with `--snip`, BSD-3-Clause, Copyright (c) 2012-present Salvatore Sanfilippo. The Porto AWEC 2026 set ([DOI 10.5281/zenodo.23342597](https://doi.org/10.5281/zenodo.23342597)) is CC BY 4.0 by Conrado Guimarães da Costa, Luís Tiago Paiva, and Fernando Fontes (FEUP).

## Stress

`trck stress` generates Mode S / ADS-B in-process and runs the same decode → queue → UART/CSV path as live. It does not open the RTL-SDR.

```
./trck stress --uart - --aircraft 32 --rate 0 --seconds 10 --emergency 0.1
```

`--rate 0` is as fast as possible. `--messages N` stops after N generated frames. Omit both `--seconds` and `--messages` to run until Ctrl-C.

`--uart` and `--table` are the same as live. On exit it prints `messages`, `fixes`, `dropped`, `csv`, and `elapsed` so you can see the ceiling on a Zero W 1.

The mix is airborne even/odd CPR, BDS 0,8 callsign (`--ident`), BDS 0,9 velocity (`--velocity`), and a `--junk` fraction of CRC-bad or truncated frames. The rest is position. `--emergency` is the fraction of ICAOs that advertise squawk 7700; ordinary traffic never evicts those from the 64-deep UART queue.

## Sim

`trck sim` writes a scripted Mode S scenario as `ts,hex` lines. Timestamp is seconds. One frame per line. It does not open the RTL-SDR or the UART.

```
./trck sim examples/scenarios/crossing.toml
./trck sim examples/scenarios/crossing.toml --out frames.csv --truth truth.csv
```

Each aircraft has an ICAO, callsign, start lat/lon/alt, heading, ground speed, and a message rate. Squawk is optional. One optional heading change (`turn_at` + `heading_to_deg`) or altitude change (`climb_at` + `alt_to_ft`) per aircraft. The same file yields the same bytes.

`--truth` is `ts,icao,lat,lon,alt_ft` for each encoded position pair.

`--iq-out FILE` writes those frames as RTL-SDR cu8. `--iq-rate` defaults to 2400000. `--iq-snr` is optional Gaussian noise in dB.

`examples/scenarios/crossing.toml` is a 7700 aircraft and a crossing pair near GRU. `examples/scenarios/climb.toml` is a single climb in the northern hemisphere. Feed the lines to `trck replay` when you want the live decode path without live traffic.

`--trace` encodes one or more readsb globe_history JSON files with the same scheduler. Ground altitude and missing fields are skipped. `--from` / `--until` are Unix seconds (`timestamp` + `dt`). A day of traces is `traces/xx/trace_full_<icao>.json` inside an [adsblol/globe_history_YYYY](https://github.com/adsblol/globe_history_2026) release tarball (`gzip -dc` the file, then `./trck sim --trace trace_full_<icao>.json --out frames.csv --truth truth.csv`).

## Frame

The layout is in [ESP32 satellite telemetry](docs/esp32-alert.md).

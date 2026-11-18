# trck

`trck` decodes 1090 MHz ADS-B on a Raspberry Pi and writes one UART frame for each airborne position. Default live path is RTL-SDR device 0 at 1090 MHz into `/dev/serial0` at 115200 baud. The 34-byte `TRCK` layout is in [ESP32 satellite telemetry](docs/esp32-alert.md). Every flag is in `trck --help`.

## Hardware

Wire 3.3 V UART with a common ground. Do not use a 5 V adapter. Leave the serial login shell off so the console does not share `/dev/serial0`.

| Pi | ESP32-S3 |
| --- | --- |
| GPIO 14 (pin 8) TX | GPIO 11 RX |
| GPIO 15 (pin 10) RX | GPIO 10 TX |
| GND (pin 6) | GND |

## Build

Install musl-tools, then on an aarch64 host:

```
cargo build --release --locked --target aarch64-unknown-linux-musl
```

Copy `target/aarch64-unknown-linux-musl/release/trck`. The static musl binary runs on the Zero 2 W and the Pi 3B with 64-bit Pi OS Lite. The release asset is `trck-aarch64`. On x86, use `cross` or a similar toolchain.

## Update

```
./trck update
```

Needs `curl` (`sudo apt install curl` on Raspberry Pi OS Lite if it is missing). Downloads `trck-aarch64` from the latest GitHub release of [igormenato/adsb](https://github.com/igormenato/adsb) when that tag is newer than this binary. It writes a temp file next to the running binary and renames it over the file. `trck update --version 0.3.2` installs that tag, including a downgrade. The file must match the listed size and start with an ELF header. When GitHub sends an asset `digest`, `sha256sum` checks it. After a successful replace it prints `restart` and exits 1 so systemd `Restart=always` can start the new binary.

## Autostart

On the Pi, as root:

```
sudo ./trck install-service
```

Writes `/etc/systemd/system/trck.service`, replaces `/usr/local/bin/trck` (temp file + rename), creates `/var/lib/trck` for `aircraft.csv`, then `systemctl daemon-reload` and `systemctl enable --now trck`. `ExecStart` is `/usr/local/bin/trck --allow-power-commands`. To change flags, `sudo systemctl edit trck` (clear `ExecStart=` then set a new one); that drop-in survives reinstall. The unit runs as root (`Restart=always`, `RestartSec=1`, `StartLimitIntervalSec=0`, `ProtectHome=yes`, `PrivateTmp=yes`, no `ProtectSystem`) so UART reboot/shutdown and `trck update` can work. Logs:

```
journalctl -u trck -f
```

## Run on the Pi

```
./trck doctor
```

The command prints one line per host check and exits non-zero if a required check fails. It does not send UART frames.

```
./trck
```

Tuner gain defaults to the tuner's maximum table step (49.6 dB on R820T/R828D, 19.7 dB on FC0013). `--gain auto` is tuner AGC. `--uart -` writes frames to stdout. `--table` defaults to `aircraft.csv`. `--allow-power-commands` lets a UART `REBOOT` or `SHUTDOWN` run `systemctl reboot` or `systemctl poweroff`.

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

`--iq` is an RTL-SDR cu8 capture on the same dump1090-2400 path as the dongle. `--iq-rate` is the file sample rate and defaults to 2400000. 1-bit CRC correction is on; `--no-fix` turns it off.

## ESP32

```
pio run -d esp32/pio -t upload
```

Board id is `esp32-s3-devkitc-1`. Arduino IDE settings, Serial words, and firmware trees are in [ESP32-S3 UART ingest](esp32/README.md).

```
make -C esp32/host test
```

## Simulate flights

`trck sim` writes `ts,hex` Mode S lines. `trck replay` runs those lines through the live decoder and UART writer. Use this to drive Pi to ESP32 to RF to base with fake aircraft.

### Scenario TOML

Unknown keys fail. `duration` and at least one `[[aircraft]]` are required. Examples: [crossing.toml](examples/scenarios/crossing.toml), [climb.toml](examples/scenarios/climb.toml).

Top-level:

| Field | Type | Required | Notes |
| --- | --- | --- | --- |
| `duration` | float, seconds | yes | Must be finite and `> 0`. |

Each `[[aircraft]]`:

| Field | Type | Required | Notes |
| --- | --- | --- | --- |
| `icao` | string | yes | Six ASCII hex digits. |
| `callsign` | string | yes | 1 to 8 characters. `A-Z`, `0-9`, space. |
| `squawk` | string | no | Four octal digits (`0-7`). Omit to skip DF5. |
| `lat` | float, degrees | yes | Finite, `-90` through `90`. |
| `lon` | float, degrees | yes | Finite, `-180` through `180`. |
| `alt_ft` | integer, feet | yes | `-1000` through `50000`. |
| `heading_deg` | float, degrees | yes | Finite. Instant heading after `turn_at`. |
| `speed_kt` | float, knots | yes | Finite, `>= 0`. Constant for the run. |
| `rate_hz` | float, Hz | yes | Finite, `> 0`. Sample period is `1 / rate_hz`. |
| `turn_at` | float, seconds | with `heading_to_deg` | Finite, `>= 0`. Both fields or neither. |
| `heading_to_deg` | float, degrees | with `turn_at` | Finite. Used at and after `turn_at`. |
| `climb_at` | float, seconds | with `alt_to_ft` | Finite, `>= 0`. Both fields or neither. |
| `alt_to_ft` | integer, feet | with `climb_at` | `-1000` through `50000`. Used at and after `climb_at`. |

Position advances on a rhumb-style heading and speed. Heading and altitude jump at `turn_at` and `climb_at`. They do not interpolate.

Stdout is `ts,hex` (seconds with three decimals, 14-byte Mode S hex). `--out FILE` writes that instead.

```
./trck sim examples/scenarios/crossing.toml --out FILE
```

### Pi UART end to end

The `trck` systemd unit holds `/dev/serial0`. Stop it before replay. Replay defaults are `/dev/serial0` and `115200`. `--uart -` writes frames to stdout. `--speed` scales playback. `0` or `max` is as fast as possible. Default `--speed` is `1`.

```
sudo systemctl stop trck
./trck sim examples/scenarios/crossing.toml --out FILE
./trck replay FILE
./trck replay FILE --speed max
sudo systemctl start trck
```

### readsb traces

`--trace` takes one or more readsb `globe_history` JSON files. Do not pass a scenario with it. `--from` and `--until` are Unix times (`timestamp + dt`). They keep samples at or after `--from` and before `--until`. Both require `--trace`. `--from` must be less than `--until`.

```
./trck sim --trace examples/traces/trace_full_e4917f.json --from 1794509787 --until 1794509800 --out FILE
```

Fixtures: [examples/traces/](examples/traces/).

### Other sim outputs

`--truth FILE` writes CSV `ts,icao,lat,lon,alt_ft` for each encoded position pair. `--iq-out FILE` writes RTL-SDR cu8 of the generated frames. `--iq-rate` defaults to `2400000`. `--iq-amp` defaults to `0.8`. `--iq-snr` (dB) adds Gaussian noise and requires `--iq-out`. `--iq-seed` defaults to `1`.

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

Sim and replay flags, scenario fields, and the Pi UART recipe are in [Simulate flights](#simulate-flights).

```
./trck stress --uart - --aircraft 32 --rate 0 --seconds 10
```

`stress` generates Mode S in-process.

## Release

A push to `main` opens or updates a release PR. Merging that PR tags `v` plus the crate version and attaches `trck-aarch64`. Do not push tags by hand.

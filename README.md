# trck

## UART

Disable the serial login shell and leave the UART enabled, or the console will share the port.

## Run

`trck` reads readsb `--net-json-port`. That flag's default is `0`, which is off. Pick a port and start readsb with it. Do not add `--net-json-port-include-noposition`. Do not add `--net-json-port-wind-triggered`.

```
readsb --net-json-port 30005
./trck --net-json 127.0.0.1:30005
```

`30005` is an example. `trck` has no default port. Pass the same host and port readsb is listening on.

Copy `trck-pi-zero-w` from a [Release](https://github.com/igormenato/adsb/releases) onto the Pi Zero W. That asset is `result/bin/trck-pi-zero-w` from `nix build`. It is static musl, so it does not ask a dynamic loader to start it. The file is ARMv6 with VFPv2 hard float.

Nix is the build. `nix develop` loads stable Rust from `rust-toolchain.toml`. `cargo test --locked` runs on this machine. `nix build` writes the Zero binary.

```
nix develop
cargo test --locked
nix build
```

Inside that shell, `cargo build --release --target arm-unknown-linux-musleabihf` is that same link, and Cargo reuses the incremental cache.

## Flags

`--net-json HOST:PORT` is the live input. Each accepted position is one JSON object and a newline. `trck` writes one UART frame for that line and does not wait.

`--sample` sends the built-in `aircraft.json` instead. That file is not the live input.

`--uart` defaults to `/dev/serial0`. `--uart -` writes the frames to stdout. `--baud` defaults to 115200.

`--interval` is the pause between `--sample` passes. A value below 0.2 seconds is treated as 0.2. The JSON port ignores it.

A dead socket writes nothing. `trck` connects again after one second, then two, then four, and so on, up to 30 seconds. If the peer dies without closing, `trck` probes after 30 seconds of silence, then every 10 seconds, three times. Failed probes count as a dead socket. A quiet sky stays connected. A line with a missing or bad `now`, or with no latitude or longitude, is skipped. The process stays up.

## Frame

Each frame is 30 bytes, little-endian, and one aircraft. There is no count, no version, and no position age. Magic is the ASCII bytes `TRCK`, so the ESP32 can find the next frame.

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | `TRCK` |
| 4 | u32 | Unix time, seconds, the line's `now` truncated |
| 8 | 3 | ICAO address, 24 bits |
| 11 | i32 | latitude, degrees × 1e7 |
| 15 | i32 | longitude, degrees × 1e7 |
| 19 | i32 | altitude, feet |
| 23 | u16 | ground speed, knots |
| 25 | u16 | track, degrees, from readsb `track` |
| 27 | u8 | emergency, `0` when none |
| 28 | u16 | CRC-16/CCITT-FALSE over the first 28 bytes |

The CRC uses poly `0x1021`, init `0xFFFF`, not reflected, xorout `0`. `CRC("123456789") = 0x29B1`.

Unknown altitude is the minimum signed 32-bit integer. Unknown ground speed is 65535. A ground speed that rounds to 65535 or more is sent as 65534. Unknown track is 65535. A track of 360 is sent as 0.

On this port `seen_pos` is 0. The frame does not carry it. The time field is the line's `now`.

This is one accepted position. The bytes below are ICAO `40621d`, latitude `522572021`, longitude `39193726`, altitude `38000`, ground speed `450`, track `271`, no emergency, and Unix time `1700000000`.

```
54 52 43 4B 00 F1 53 65 1D 62 40 F5 D0 25 1F 7E 0C 56 02 70 94 00 00 C2 01 0F 01 00 73 81
```

## Emergency

The eight names are `none`, `general`, `lifeguard`, `minfuel`, `nordo`, `unlawful`, `downed`, and `reserved`. Squawk 7700, 7600, and 7500 map to `general`, `nordo`, and `unlawful` when readsb leaves the field unset, empty, or `none`, and when the name is any word that is not one of the eight. A known name wins over the squawk. `reserved` is 7, and 7700, 7600, or 7500 still replaces it. `reserved` with no emergency squawk stays 7. In the frame the byte at offset 27 is `0` none, `1` general, `2` lifeguard, `3` minfuel, `4` nordo, `5` unlawful, `6` downed, `7` reserved. See `docs/esp32-alert.md`.

The same position with `emergency` `general`, or with squawk 7700 and no name, is the frame above with byte 27 set to `01`. The CRC changes with it.

```
54 52 43 4B 00 F1 53 65 1D 62 40 F5 D0 25 1F 7E 0C 56 02 70 94 00 00 C2 01 0F 01 01 52 91
```

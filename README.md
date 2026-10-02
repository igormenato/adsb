# adsb

A Raspberry Pi running [readsb](https://github.com/wiedehopf/readsb) sends one aircraft-track snapshot out a UART each second. The snapshot is taken from readsb's `aircraft.json`.

## UART

Default device `/dev/serial0`, 115200 8N1. Override with `--uart` and `--baud`. `--uart -` writes the bytes to stdout. A write that has not left the port within 30 seconds fails.

Disable the serial login shell and leave the UART enabled (`raspi-config` → Interface Options → Serial Port), or the console will share the port.

## readsb

readsb writes `aircraft.json` about once a second when `--write-json` is set. The default path is `/run/readsb/aircraft.json`.

```
readsb --device-type rtlsdr --net --write-json /run/readsb
```

## Run

From the repo root (`rustup` stable is enough):

```
cargo build --release
./target/release/adsb-uart-sender --json /run/readsb/aircraft.json --uart /dev/serial0 --baud 115200
```

`--interval` defaults to 1 second. Stderr prints one line per snapshot. A missing file is retried. An empty sky is a snapshot with zero aircraft.

## Snapshot

Little-endian. Magic is the ASCII bytes `TRCK`.

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | `TRCK` |
| 4 | u8 | version `1` |
| 5 | u8 | reserved `0` |
| 6 | u16 | aircraft count |
| 8 | u32 | Unix time, seconds, from the file's `now` |

Then one 28-byte record per aircraft:

| Offset | Size | Field |
| --- | --- | --- |
| 0 | u32 | ICAO address in the low 24 bits |
| 4 | 8 | callsign, ASCII, space-padded |
| 12 | i32 | latitude, degrees × 1e7 |
| 16 | i32 | longitude, degrees × 1e7 |
| 20 | i32 | altitude, feet |
| 24 | u16 | ground speed, knots |
| 26 | u16 | heading, degrees, 0–359 |

A CRC-16/CCITT-FALSE over every preceding byte follows the records (poly `0x1021`, init `0xFFFF`, not reflected, xorout `0`). `CRC("123456789") = 0x29B1`.

Only aircraft with a latitude, longitude, and `seen_pos` of at most 2 seconds are included, at most 64, freshest first. The header time is the file's `now`. A fix in the packet can be up to 2 seconds older than that. Unknown speed or heading is `65535`. Unknown altitude is `0x80000000`. A missing callsign is eight spaces. `alt_baro` of `"ground"` is 0 feet.

## Host test

No radio and no UART:

```
cargo test
```

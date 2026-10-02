# adsb

## readsb

```
readsb --device-type rtlsdr --net --write-json /run/readsb
```

`--write-json` writes `aircraft.json` in that directory.

## UART

Disable the serial login shell and leave the UART enabled (`raspi-config` → Interface Options → Serial Port), or the console will share the port.

## Run

On the Pi:

```
cargo build --release
./target/release/adsb-uart-sender
```

`--sample` sends two built-in aircraft instead of the live file.

From another machine, for a 64-bit Pi (`gcc-aarch64-linux-gnu`):

```
cargo build --release --target aarch64-unknown-linux-gnu
```

The binary is `target/aarch64-unknown-linux-gnu/release/adsb-uart-sender`.

`--format json` sends one JSON object per line instead of the binary snapshot.

## Snapshot

Little-endian. Magic is the ASCII bytes `TRCK`.

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | `TRCK` |
| 4 | u8 | version `1` |
| 5 | u8 | reserved `0` |
| 6 | u16 | aircraft count |
| 8 | u32 | Unix time, seconds, from the file's `now` |

Then one 20-byte record per aircraft:

| Offset | Size | Field |
| --- | --- | --- |
| 0 | u32 | ICAO address in the low 24 bits |
| 4 | i32 | latitude, degrees × 1e7 |
| 8 | i32 | longitude, degrees × 1e7 |
| 12 | i32 | altitude, feet |
| 16 | u16 | ground speed, knots |
| 18 | u16 | heading, degrees, 0–359 |

A CRC-16/CCITT-FALSE over every preceding byte follows the records (poly `0x1021`, init `0xFFFF`, not reflected, xorout `0`). `CRC("123456789") = 0x29B1`.

`--format json` sends the same snapshot as one JSON object per line:

```
{"unix_s":1700000000,"aircraft":[{"icao":4219421,"latitude_e7":522572021,"longitude_e7":39193726,"altitude_ft":38000,"ground_speed_kt":450,"heading_deg":271}]}
```

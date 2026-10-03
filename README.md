# trck

## UART

Disable the serial login shell and leave the UART enabled, or the console will share the port.

## Run

Copy `trck` from a [Release](https://github.com/igormenato/adsb/releases) onto the Pi, then:

```
./trck
```

To build it (`gcc-aarch64-linux-gnu`):

```
cargo build --release --target aarch64-unknown-linux-gnu
```

## Flags

`--sample` sends two built-in aircraft instead of the live file.

`--format json` is the default.

`--format struct` sends the binary packet.

## Snapshot

The sender writes one JSON object per line. Nothing is written when the snapshot has no aircraft.

```
{"unix_s":1700000000,"aircraft":[{"icao":4219421,"latitude_e7":522572021,"longitude_e7":39193726,"altitude_ft":38000,"ground_speed_kt":450,"heading_deg":271}]}
```

`--format struct` sends that snapshot as a little-endian binary packet. Magic is the ASCII bytes `TRCK`.

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

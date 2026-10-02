# adsb

## readsb

```
readsb --device-type rtlsdr --net --write-json /run/readsb
```

`--write-json` writes `aircraft.json` in that directory.

## UART

Disable the serial login shell and leave the UART enabled (`raspi-config` → Interface Options → Serial Port), or the console will share the port.

## Run

```
cargo build --release
./target/release/adsb-uart-sender
```

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

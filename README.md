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

The sender writes one JSON object per line. An empty aircraft list is still one snapshot.

```
{"unix_s":10,"aircraft":[]}
```

No bytes means the link sent nothing. A count of 0 means the receiver is alive and the sky has no aircraft to report. The ground uses a count of 0 to clear tracks and emergency highlights.

```
{"unix_s":1700000000,"aircraft":[{"icao":4219421,"latitude_e7":522572021,"longitude_e7":39193726,"altitude_ft":38000,"ground_speed_kt":450,"heading_deg":271,"position_age_s":1}]}
```

`--format struct` sends that snapshot as a little-endian binary packet. Magic is the ASCII bytes `TRCK`.

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | `TRCK` |
| 4 | u8 | aircraft count |
| 5 | u32 | Unix time, seconds, from the file's `now` |

Then one 21-byte record per aircraft:

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 3 | ICAO address, 24 bits |
| 3 | i32 | latitude, degrees × 1e7 |
| 7 | i32 | longitude, degrees × 1e7 |
| 11 | i32 | altitude, feet |
| 15 | u16 | ground speed, knots |
| 17 | u16 | heading, degrees, 0–359 |
| 19 | u8 | emergency, `0` when none |
| 20 | u8 | position age, whole seconds, at most 255 |

A CRC-16/CCITT-FALSE over every preceding byte follows the records (poly `0x1021`, init `0xFFFF`, not reflected, xorout `0`). `CRC("123456789") = 0x29B1`.

## Emergency

`"emergency"` is present on an aircraft object only while one is active. The names are `general`, `lifeguard`, `minfuel`, `nordo`, `unlawful`, and `downed`. Squawk 7700, 7600, and 7500 map to `general`, `nordo`, and `unlawful` when readsb leaves the field unset or `none`. The first snapshot that includes the aircraft carries it. In the binary record the same state is the byte at offset 19: `0` none, `1` general, `2` unlawful, `3` nordo, `4` lifeguard, `5` minfuel, `6` downed. See `docs/esp32-alert.md`.

`"position_age_s"` is on every aircraft. It is the whole seconds of `seen_pos`, and the same value is the last byte of the binary record. It saturates at 255. An emergency that still has a latitude and longitude stays while `seen_pos` is above 2 seconds and at most 30. An emergency with no latitude and longitude is left out. Other aircraft are left out once `seen_pos` is above 2. When more than 64 aircraft qualify, emergencies are kept first, then the freshest positions.

```
{"unix_s":1700000000,"aircraft":[{"icao":4219421,"latitude_e7":522572021,"longitude_e7":39193726,"altitude_ft":38000,"ground_speed_kt":450,"heading_deg":271,"position_age_s":1,"emergency":"general"}]}
```

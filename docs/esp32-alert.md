# ESP32 snapshot forwarding

The Raspberry Pi sends one aircraft snapshot per second. An emergency is a field on an aircraft in that snapshot. There is one packet type. The ESP32 does not decode ADS-B. It forwards each snapshot unchanged.

JSON is the default. `--format struct` is the binary snapshot. Do not mix the two on one link.

## What you do

1. Read the UART at 115200 baud, 8 data bits, no parity, 1 stop bit.
2. Forward each snapshot unchanged, as one telemetry message.
3. Forward a snapshot whose aircraft count is 0. No bytes means the link sent nothing. A count of 0 means the receiver is alive and the sky has no aircraft to report. The ground uses a count of 0 to clear tracks and emergency highlights.

The ground station highlights an aircraft whose emergency field is set, and clears that highlight when a later snapshot has no emergency for that ICAO.

## JSON lines

This is the default (`--format json`). Each snapshot is one line ending in `0x0A`. A line starts with `{"unix_s":`.

An empty sky is one line, `{"unix_s":N,"aircraft":[]}`.

`"emergency"` is present on an aircraft object only while an emergency is active. The names are `general`, `lifeguard`, `minfuel`, `nordo`, `unlawful`, and `downed`. When there is no emergency, the key is absent.

`"position_age_s"` is on every aircraft. It is the whole seconds of the position age, saturating at 255. An emergency that still has a latitude and longitude can be up to 30 seconds old. Other aircraft are omitted once the position is older than 2 seconds.

```
{"unix_s":1700000000,"aircraft":[{"icao":4219421,"latitude_e7":522572021,"longitude_e7":39193726,"altitude_ft":38000,"ground_speed_kt":450,"heading_deg":271,"position_age_s":1,"emergency":"general"},{"icao":4197052,"latitude_e7":105000000,"longitude_e7":-202500000,"altitude_ft":12000,"ground_speed_kt":280,"heading_deg":90,"position_age_s":0}]}
```

Forward the whole line, including the newline. Squawk 7700, 7600, and 7500 become `general`, `nordo`, and `unlawful` when readsb leaves `emergency` unset or `none`. A readsb emergency name is used as written.

## Struct packets

Only when the Pi is started with `--format struct`. Each snapshot starts with `TRCK` (`54 52 43 4B`).

The header stays 12 bytes. The version byte is `2`. A count of 0 is that header plus the CRC and no records. For Unix time 10 the bytes are `54 52 43 4B 02 00 00 00 0A 00 00 00 DC AD`. Each aircraft record is 22 bytes: the previous 20-byte track, then the emergency byte, then the position age in whole seconds. The age saturates at 255. Emergencies are kept ahead of other aircraft.

The same two aircraft as the JSON line above:

```
54 52 43 4B 02 00 02 00 00 F1 53 65 1D 62 40 00 F5 D0 25 1F 7E 0C 56 02 70 94 00 00 C2 01 0F 01 01 01 BC 0A 40 00 40 2C 42 06 60 18 EE F3 E0 2E 00 00 18 01 5A 00 00 00 ED 1C
```

Forward those bytes unchanged. The length is `14 + 22N` for `N` aircraft.

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | `TRCK` |
| 4 | u8 | version `2` |
| 5 | u8 | reserved `0` |
| 6 | u16 | aircraft count |
| 8 | u32 | Unix time, seconds |

Then one record per aircraft:

| Offset | Size | Field |
| --- | --- | --- |
| 0 | u32 | ICAO address in the low 24 bits |
| 4 | i32 | latitude, degrees × 1e7 |
| 8 | i32 | longitude, degrees × 1e7 |
| 12 | i32 | altitude, feet |
| 16 | u16 | ground speed, knots |
| 18 | u16 | heading, degrees, 0–359 |
| 20 | u8 | emergency |
| 21 | u8 | position age, whole seconds, at most 255 |

Little-endian. CRC-16/CCITT-FALSE covers the header and the records (poly `0x1021`, init `0xFFFF`, not reflected, xorout `0`) and follows them as a `u16`. Drop a packet whose CRC fails. The next `TRCK` realigns the stream.

| Code | Name |
| --- | --- |
| 0 | none |
| 1 | general (squawk 7700) |
| 2 | unlawful (squawk 7500) |
| 3 | nordo (squawk 7600, radio failure) |
| 4 | lifeguard |
| 5 | minfuel |
| 6 | downed |

You do not need the code to forward the packet. The ground station reads it.

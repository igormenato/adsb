# ESP32 alert forwarding

The Raspberry Pi detects aircraft emergencies. The ESP32 does not decode ADS-B. It recognizes an alert message, forwards that message unchanged, and sends it ahead of the routine snapshot.

JSON is the default. `--format struct` is the binary snapshot. Do not mix the two on one link.

## What you do

1. Read the UART at 115200 baud, 8 data bits, no parity, 1 stop bit.
2. Recognize an alert by its prefix only. Do not read the ICAO, the emergency name, the position, or the altitude.
3. Forward the alert bytes unchanged, as their own telemetry message.
4. Send every alert before the routine snapshot from the same cycle. The Pi already writes alerts first. If you buffer a cycle, drain the alerts before the snapshot.
5. Leave the aircraft in the snapshot. Do not merge the alert into the snapshot, and do not split the alert across snapshot slices.

A cycle may be one or more alerts and then one snapshot, alerts only, or a snapshot only. The first cycle an emergency appears, there is no alert yet. The alert starts on the second consecutive cycle.

## JSON lines

This is the default (`--format json`). Each message is one line ending in `0x0A`.

An alert line starts with `{"alert":`. A snapshot line starts with `{"unix_s":`.

Active alert, sent when the alert opens and again every cycle while it stays open:

```
{"alert":{"icao":4219421,"emergency":"general","latitude_e7":522572021,"longitude_e7":39193726,"altitude_ft":38000,"unix_s":1700000000,"cleared":false}}
```

Cleared alert, sent once when the emergency ends or the aircraft leaves the snapshot:

```
{"alert":{"icao":4219421,"emergency":"general","latitude_e7":522572021,"longitude_e7":39193726,"altitude_ft":38000,"unix_s":1700000002,"cleared":true}}
```

Forward the whole line, including the newline. The cleared line is still an alert: same prefix, `"cleared":true`.

| Bytes | Message |
| --- | --- |
| `{"alert":` | alert, including the cleared line |
| `{"unix_s":` | routine snapshot |

## Struct packets

Only when the Pi is started with `--format struct`. Messages are back to back, with no extra bytes between them.

An alert starts with `ALRT` (`41 4C 52 54`) and is always 30 bytes. A snapshot starts with `TRCK` and keeps its existing layout.

Active alert, the same aircraft as the JSON line above:

```
41 4C 52 54 01 00 01 00 1D 62 40 00 F5 D0 25 1F 7E 0C 56 02 70 94 00 00 00 F1 53 65 C9 DD
```

Cleared alert (`unix_s` 1700000002):

```
41 4C 52 54 01 01 01 00 1D 62 40 00 F5 D0 25 1F 7E 0C 56 02 70 94 00 00 02 F1 53 65 AA 55
```

Forward those 30 bytes unchanged.

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | `ALRT` |
| 4 | u8 | version `1` |
| 5 | u8 | `0` active, `1` cleared |
| 6 | u8 | emergency type |
| 7 | u8 | reserved `0` |
| 8 | u32 | ICAO address in the low 24 bits |
| 12 | i32 | latitude, degrees × 1e7 |
| 16 | i32 | longitude, degrees × 1e7 |
| 20 | i32 | altitude, feet |
| 24 | u32 | Unix time, seconds |
| 28 | u16 | CRC-16/CCITT-FALSE over the preceding bytes |

Little-endian. The CRC is the same one as `TRCK`: poly `0x1021`, init `0xFFFF`, not reflected, xorout `0`. Drop a packet whose CRC fails. The next `ALRT` or `TRCK` realigns the stream.

| Code | Name |
| --- | --- |
| 1 | general (squawk 7700) |
| 2 | lifeguard |
| 3 | minfuel |
| 4 | nordo (squawk 7600, radio failure) |
| 5 | unlawful (squawk 7500) |
| 6 | downed |
| 7 | reserved |

You do not need the code to forward the packet. The ground station reads it.

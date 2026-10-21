# ESP32 satellite telemetry

The Raspberry Pi writes one 30-byte UART frame for each position readsb accepts on `--net-json-port`. The frame is one aircraft. It is not a list. An emergency is one byte in that frame. The ESP32 reads the frame and copies the aircraft fields into the satellite telemetry that goes to the ground. It does not cast the frame to a struct. It does not forward the raw frame as the radio payload. It does not decode 1090 MHz ADS-B.

`trck --sample` writes those same frames on the UART. The ESP32 can check reception against that. Those frames are the live layout.

## What you do

1. Read the UART at 115200 baud, 8 data bits, no parity, 1 stop bit.
2. Find `TRCK` (`54 52 43 4B`). The frame is the next 30 bytes, including those four.
3. Check CRC-16/CCITT-FALSE (poly `0x1021`, init `0xFFFF`, not reflected, xorout `0`) over the first 28 bytes. The last two bytes are that CRC, little-endian.
4. If the CRC matches, copy the fields below into the telemetry. If it does not match, drop the frame. The next `TRCK` is the next frame.

No bytes means the Pi sent nothing. There is no count of 0 and no heartbeat. Do not treat a quiet UART as an empty list.

## Frame

30 bytes. Little-endian. No version. No aircraft count. No position age.

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | `TRCK` |
| 4 | u32 | Unix time, seconds |
| 8 | 3 | ICAO address, 24 bits |
| 11 | i32 | latitude, degrees × 1e7 |
| 15 | i32 | longitude, degrees × 1e7 |
| 19 | i32 | altitude, feet |
| 23 | u16 | ground speed, knots |
| 25 | u16 | track, degrees, 0 to 359 |
| 27 | u8 | emergency |
| 28 | u16 | CRC over bytes 0..27 |

Unknown altitude is the minimum signed 32-bit integer. Unknown ground speed is 65535. Unknown track is 65535.

| Code | Name |
| --- | --- |
| 0 | none |
| 1 | general (squawk 7700) |
| 2 | lifeguard |
| 3 | minfuel |
| 4 | nordo (squawk 7600, radio failure) |
| 5 | unlawful (squawk 7500) |
| 6 | downed |
| 7 | reserved |

Squawk 7700, 7600, and 7500 become `general`, `nordo`, and `unlawful` when readsb leaves `emergency` unset, empty, or `none`, and when the name is any word that is not one of the eight (`none`, `general`, `lifeguard`, `minfuel`, `nordo`, `unlawful`, `downed`, `reserved`). A known readsb emergency name is the byte that is sent. `reserved` with squawk 7700, 7600, or 7500 is that squawk's code. `reserved` with no emergency squawk is 7.

One position, no emergency, Unix time `1700000000`:

```
54 52 43 4B 00 F1 53 65 1D 62 40 F5 D0 25 1F 7E 0C 56 02 70 94 00 00 C2 01 0F 01 00 73 81
```

The same position with emergency `general`:

```
54 52 43 4B 00 F1 53 65 1D 62 40 F5 D0 25 1F 7E 0C 56 02 70 94 00 00 C2 01 0F 01 01 52 91
```

## Radio payload

One plane on the radio is 24 bytes. Copy bytes 8 through 27 of the frame (the 20-byte aircraft record), then copy the Unix time at bytes 4 through 7. Write each field yourself, low byte first. Do not cast the UART buffer. Do not walk the frame as a list of aircraft. There is one aircraft in it.

The 24 bytes for the first example are `1D 62 40 F5 D0 25 1F 7E 0C 56 02 70 94 00 00 C2 01 0F 01 00 00 F1 53 65`.

The nRF24L01 payload max is 32 bytes. These 24 bytes are the aircraft content. The other bytes of the radio packet are the radio header the radio team already uses.

The ESP32 reads the frame and copies the aircraft fields into the satellite telemetry. The ground station reads that telemetry.

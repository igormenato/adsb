# ESP32 satellite telemetry

The Raspberry Pi writes one 34-byte UART frame for each airborne position it decodes. One aircraft per frame. Emergency is one byte. The ESP32 copies those fields into the satellite telemetry. It does not overlay a struct on the UART bytes, forward the raw frame as the radio payload, or decode 1090 MHz ADS-B.

`trck --check` writes the same frames as the live sender.

## What you do

1. Read the UART at 115200 baud, 8 data bits, no parity, 1 stop bit.
2. Find `TRCK` (`54 52 43 4B`). The frame is 34 bytes including those four.
3. Check CRC-16/CCITT-FALSE (poly `0x1021`, init `0xFFFF`, not reflected, xorout `0`) over the first 32 bytes. The last two bytes are that CRC, little-endian.
4. Copy the fields below when the CRC matches. Drop a mismatch. The next `TRCK` is the next frame.

No bytes means the Pi sent nothing. There is no count of 0 and no heartbeat. A quiet UART is not an empty sky.

## Frame

34 bytes. Little-endian. No version. No aircraft count. No position age. No time field. Contest time is ground arrival.

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | `TRCK` |
| 4 | 3 | ICAO address, 24 bits |
| 7 | 8 | callsign, ASCII, space-padded |
| 15 | i32 | latitude, degrees × 1e7 |
| 19 | i32 | longitude, degrees × 1e7 |
| 23 | i32 | altitude, feet |
| 27 | u16 | ground speed, knots |
| 29 | u16 | track, degrees, 0 to 359 |
| 31 | u8 | emergency |
| 32 | u16 | CRC over bytes 0..31 |

Callsign is the ADS-B identification (BDS 0,8) last seen for that ICAO. Shorter text is padded with spaces on the right. Longer text is cut to eight. Missing identification is eight spaces. The position is still sent.

Unknown altitude is the minimum signed 32-bit integer. Unknown ground speed is 65535. Unknown track is 65535. Ground speed and track are the last airborne velocity (BDS 0,9) for that ICAO. Track is whole degrees: `182.88` is `183`. `360` wraps to `0`.

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

Emergency is the ADS-B status (BDS 6,1) last seen for that ICAO. Squawk 7700, 7600, and 7500 become `general`, `nordo`, and `unlawful` when the status is none or reserved. `reserved` with no emergency squawk is 7.

One position, callsign `BAW123`, no emergency:

```
54 52 43 4B 1D 62 40 42 41 57 31 32 33 20 20 F5 D0 25 1F 7E 0C 56 02 70 94 00 00 C2 01 0F 01 00 60 EF
```

The same position with emergency `general`:

```
54 52 43 4B 1D 62 40 42 41 57 31 32 33 20 20 F5 D0 25 1F 7E 0C 56 02 70 94 00 00 C2 01 0F 01 01 41 FF
```

## Radio payload

One plane on the radio is 28 bytes. Copy bytes 4 through 31 of the frame (the 28-byte aircraft record). Write each field low byte first. Do not overlay a struct on the UART buffer. There is no time field. Contest time is ground arrival.

The 28 bytes for the first example are `1D 62 40 42 41 57 31 32 33 20 20 F5 D0 25 1F 7E 0C 56 02 70 94 00 00 C2 01 0F 01 00`.

The nRF24L01 payload max is 32 bytes. These 28 bytes are the aircraft content. Four bytes remain in that payload. The UART frame stays 34 bytes.

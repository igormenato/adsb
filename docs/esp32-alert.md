# ESP32 satellite telemetry

The Raspberry Pi writes one 38-byte UART frame for each position readsb accepts on `--net-json-port`. One aircraft per frame. Emergency is one byte. The ESP32 copies those fields into the satellite telemetry. It does not overlay a struct on the UART bytes, forward the raw frame as the radio payload, or decode 1090 MHz ADS-B.

`trck --check` writes the same frames as the live sender.

## What you do

1. Read the UART at 115200 baud, 8 data bits, no parity, 1 stop bit.
2. Find `TRCK` (`54 52 43 4B`). The frame is 38 bytes including those four.
3. Check CRC-16/CCITT-FALSE (poly `0x1021`, init `0xFFFF`, not reflected, xorout `0`) over the first 36 bytes. The last two bytes are that CRC, little-endian.
4. Copy the fields below when the CRC matches. Drop a mismatch. The next `TRCK` is the next frame.

No bytes means the Pi sent nothing. There is no count of 0 and no heartbeat. A quiet UART is not an empty sky.

## Frame

38 bytes. Little-endian. No version. No aircraft count. No position age.

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | `TRCK` |
| 4 | u32 | Unix time, seconds |
| 8 | 3 | ICAO address, 24 bits |
| 11 | 8 | callsign, ASCII, space-padded |
| 19 | i32 | latitude, degrees × 1e7 |
| 23 | i32 | longitude, degrees × 1e7 |
| 27 | i32 | altitude, feet |
| 31 | u16 | ground speed, knots |
| 33 | u16 | track, degrees, 0 to 359 |
| 35 | u8 | emergency |
| 36 | u16 | CRC over bytes 0..35 |

Callsign is readsb `flight`. Shorter text is padded with spaces on the right. Longer text is cut to eight. Missing `flight` is eight spaces. The position is still sent.

Unknown altitude is the minimum signed 32-bit integer. readsb `alt_baro` of `ground` is not 0. The frame uses `alt_geom` when that is present, otherwise unknown. Unknown ground speed is 65535. Unknown track is 65535. Track is whole degrees: `271.4` is `271`. `360` wraps to `0`.

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

A known readsb emergency name is the byte that is sent. Squawk 7700, 7600, and 7500 become `general`, `nordo`, and `unlawful` when the name is missing, empty, `none`, or any word outside the eight in the table. `reserved` with one of those squawks uses the squawk. `reserved` with no emergency squawk is 7.

One position, callsign `BAW123`, no emergency, Unix time `1700000000`:

```
54 52 43 4B 00 F1 53 65 1D 62 40 42 41 57 31 32 33 20 20 F5 D0 25 1F 7E 0C 56 02 70 94 00 00 C2 01 0F 01 00 06 BE
```

The same position with emergency `general`:

```
54 52 43 4B 00 F1 53 65 1D 62 40 42 41 57 31 32 33 20 20 F5 D0 25 1F 7E 0C 56 02 70 94 00 00 C2 01 0F 01 01 27 AE
```

## Radio payload

One plane on the radio is 32 bytes. Copy bytes 8 through 35 of the frame (the 28-byte aircraft record), then copy the Unix time at bytes 4 through 7. Write each field low byte first. Do not overlay a struct on the UART buffer.

The 32 bytes for the first example are `1D 62 40 42 41 57 31 32 33 20 20 F5 D0 25 1F 7E 0C 56 02 70 94 00 00 C2 01 0F 01 00 00 F1 53 65`.

The nRF24L01 payload max is 32 bytes. These 32 bytes are the aircraft content. There is no spare byte for a radio header inside that payload. The UART frame stays 38 bytes.

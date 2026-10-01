# adsb bench

Bench check that a Raspberry Pi running [readsb](https://github.com/wiedehopf/readsb) can send ADS-B over its own UART. Prove it with a wire from the Pi's TX pin to its RX pin. No ESP32 is required. Processing split, 1U/2U/3U, and physical-versus-equivalent are still open.

Two modes, selected on the sender with `--mode`:

| Mode | What the sender writes | What the receiver accepts |
| --- | --- | --- |
| `raw` | readsb Beast bytes, unchanged | resync on Beast `0x1A` framing |
| `struct` | one 32-byte record per DF17/DF18 squitter | resync on the magic header and checksum |

The receiver takes either framing, so leave it running and only restart the sender to switch modes.

```
common/adsb_struct.h      layout source of truth
common/*.c                Beast, struct, and link parsers (host test and optional ESP32)
pi/adsb_uart_sender.py    Pi sender
pi/adsb_uart_receiver.py  Pi receiver, same framing
esp32/                    optional PlatformIO sketch, same framing
test/test_bench.py        host test, no hardware
```

## Pi loopback

`/dev/serial0` is one UART. GPIO14 is TX and GPIO15 is RX. Jumper them so the sender's bytes come back into the receiver.

| Header pin | GPIO | Role |
| --- | --- | --- |
| pin 8 | GPIO14 TXD | sender writes |
| pin 10 | GPIO15 RXD | receiver reads |

3.3 V logic. Do not add a 5 V jumper. Disable the serial login shell and leave the UART enabled (`raspi-config` → Interface Options → Serial Port), or the console will share the wire.

Default device `/dev/serial0`, 115200 8N1. Both processes take `--uart` and `--baud`. `--uart -` is stdout on the sender and stdin on the receiver.

Start the receiver first, in one terminal, from the repo root:

```
python3 pi/adsb_uart_receiver.py --uart /dev/serial0 --baud 115200
```

In a second terminal, with readsb already serving Beast on `127.0.0.1:30005`, run raw mode:

```
python3 pi/adsb_uart_sender.py --mode raw --uart /dev/serial0 --baud 115200
```

The receiver prints a `beast` line per frame (type, DF, ICAO when the squitter has one) and a `beat` line every 5 seconds with the counts. Stop the sender with Ctrl-C. Leave the receiver up. Start struct mode in that same second terminal:

```
python3 pi/adsb_uart_sender.py --mode struct --uart /dev/serial0 --baud 115200
```

The receiver then prints `struct` lines (ICAO, flags, and whichever of lat/lon, altitude, and velocity that squitter carried). Framing and checksum errors print as `err ...`. One process writes; the other reads. They share the device path and the baud.

## readsb

Beast binary output is TCP `127.0.0.1:30005` when networking is on. The sender dials that port (it is not a server).

```
readsb --device-type rtlsdr --net --net-bo-port 30005
```

`--net-bo-port` defaults to 30005. If readsb is already running with `--net`, start the sender and it will connect. The sender retries once a second until the port is up. `--beast-host` and `--beast-port` change that address. No extra Python packages.

Raw mode is a byte copy of the TCP stream, including `0x1A` escaping. Struct mode decodes on the Pi and writes the record below. The sender logs a one-line count on stderr about once a second.

## Struct layout

`common/adsb_struct.h` is the layout. `pi/struct_frame.py` packs `<HBBIiiiHQH` and the host test checks the bytes against the C encoder. 32 bytes, packed, little-endian.

| Offset | Size | Field | Unit |
| --- | --- | --- | --- |
| 0 | u16 | magic | `0xAD5B` (wire bytes `5B AD`) |
| 2 | u8 | version | `1` |
| 3 | u8 | flags | bit0 position, bit1 altitude, bit2 velocity |
| 4 | u32 | icao | address in the low 24 bits |
| 8 | i32 | latitude_e7 | degrees × 1e7 |
| 12 | i32 | longitude_e7 | degrees × 1e7 |
| 16 | i32 | altitude_ft | feet |
| 20 | u16 | velocity_kt | ground speed, knots |
| 22 | u64 | timestamp_us | Unix epoch, microseconds, set on the Pi |
| 30 | u16 | checksum | CRC-16/CCITT-FALSE over bytes 0..29 |

CRC-16/CCITT-FALSE: poly `0x1021`, init `0xFFFF`, xorout `0`, not reflected. `CRC("123456789") = 0x29B1`.

A cleared field uses a sentinel, because 0 is a real latitude and a real altitude: latitude and longitude `0x80000000`, altitude `0x80000000`, velocity `65535`. Honor the flags. One record is one squitter, not a fused track, so a position frame and a velocity frame arrive as two structs.

DF18 addresses that are not ICAO still occupy the `icao` field.

## Beast framing

readsb types on port 30005:

| Type | After the `0x1A` |
| --- | --- |
| `'1'` (0x31) | 6-byte MLAT timestamp, 1 signal byte, 2-byte Mode A/C |
| `'2'` (0x32) | 6-byte MLAT, 1 signal byte, 7-byte short Mode S |
| `'3'` (0x33) | 6-byte MLAT, 1 signal byte, 14-byte long Mode S |

A `0x1A` inside the timestamp, signal, or payload is sent twice. The receiver collapses those pairs and, if a frame breaks, hunts for the next `0x1A` followed by `'1'`, `'2'`, or `'3'`. DF17/DF18 CRC failures are counted and still logged. Struct mode on the sender drops a squitter whose Mode S CRC is bad.

## What struct mode decodes

- DF17 and DF18 only.
- Airborne position, type codes 9–18: barometric altitude in feet (25 ft coding). Q=0 Gillham altitude is left invalid.
- Type codes 20–22: the 12-bit GNSS height in meters, converted to feet.
- Latitude and longitude only after an even and an odd CPR frame for that address, less than 10 seconds apart. Until then the position flag stays clear and altitude can still be set.
- Airborne velocity, type code 19 subtypes 1 and 2: ground speed in knots.
- Other DF17/DF18 squitters: ICAO and timestamp only.
- Surface position is not decoded.

## Host test

No radio and no jumper:

```
python3 test/test_bench.py
```

Needs `gcc` and Python 3. It checks that the Python and C framers produce the same bytes, that raw mode does not alter a Beast stream, that the Pi receiver reports the same frames and error counts as the shared parser, and that a published CPR pair decodes to 52.257202° N, 3.919373° E at 38000 ft.

## Optional ESP32

`esp32/` is a PlatformIO Arduino sketch for a classic ESP32, same framing and the same log lines, if a board is added later. It is not part of the loopback. Pins and baud are the constants at the top of `esp32/src/main.cpp` (UART2 RX GPIO16). The board id is `esp32dev` in `esp32/platformio.ini`.

```
cd esp32
pio run -t upload
pio device monitor
```

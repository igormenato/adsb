# adsb bench

Bench check that a Raspberry Pi running [readsb](https://github.com/wiedehopf/readsb) can hand ADS-B to an ESP32 OBC over UART. This is the link test only. Processing split, 1U/2U/3U, and physical-versus-equivalent are still open.

Two modes, selected on the Pi with `--mode`:

| Mode | What the Pi writes | What the ESP32 does |
| --- | --- | --- |
| `raw` | readsb Beast bytes, unchanged | resync on Beast `0x1A` framing |
| `struct` | one 32-byte record per DF17/DF18 squitter | resync on the magic header and checksum |

The ESP32 tells the modes apart, so it does not need a rebuild to switch. The repo had no embedded project, so the receiver is a PlatformIO Arduino sketch for a classic ESP32.

```
common/adsb_struct.h   layout source of truth
common/*.c             Beast, struct, and link parsers (host test and ESP32)
pi/adsb_uart_sender.py Pi sender
esp32/                 PlatformIO sketch
test/test_bench.py     host test, no hardware
```

## Wiring

3.3 V logic. Do not connect a 5 V pin to the ESP32.

| Raspberry Pi | ESP32 (classic, UART2) |
| --- | --- |
| GPIO14 TXD, header pin 8 | GPIO16 RX |
| GND, header pin 6 | GND |

Pi RX is unused. Disable the serial login shell and leave the UART enabled (`raspi-config` → Interface Options → Serial Port), or the console will share the wire.

Default device `/dev/serial0`, 115200 8N1. Override with `--uart` and `--baud`. `--uart -` writes the bytes to stdout.

## readsb

Beast binary output is TCP `127.0.0.1:30005` when networking is on. The sender dials that port (it is not a server).

```
readsb --device-type rtlsdr --net --net-bo-port 30005
```

`--net-bo-port` defaults to 30005. If readsb is already running with `--net`, start the sender and it will connect. The sender retries once a second until the port is up.

## Run a mode

From the repo root, on the Pi:

```
python3 pi/adsb_uart_sender.py --mode raw --uart /dev/serial0 --baud 115200
python3 pi/adsb_uart_sender.py --mode struct --uart /dev/serial0 --baud 115200
```

Stop one before starting the other. `--beast-host` and `--beast-port` change the readsb address (default `127.0.0.1:30005`). No extra Python packages.

Raw mode is a byte copy of the TCP stream, including `0x1A` escaping. Struct mode decodes on the Pi and writes the record below. Stderr prints a one-line count about once a second.

## ESP32

Pins and baud are the constants at the top of `esp32/src/main.cpp`. The board id is `esp32dev` in `esp32/platformio.ini`; change it if the module is not a classic ESP32, and change the pins to that chip's UART RX.

```
cd esp32
pio run -t upload
pio device monitor
```

USB serial is the log, also 115200. Each accepted frame prints a line (`beast` with type, DF, ICAO when the squitter has one, or `struct` with ICAO and whichever fields are valid). Framing and checksum errors print as `err ...`. A `beat` line every 5 seconds shows the counters, including while the wire is idle.

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

A `0x1A` inside the timestamp, signal, or payload is sent twice. The ESP32 collapses those pairs and, if a frame breaks, hunts for the next `0x1A` followed by `'1'`, `'2'`, or `'3'`. DF17/DF18 CRC failures are counted; the raw bytes are still delivered to the log. Struct mode on the Pi drops a squitter whose Mode S CRC is bad.

## What struct mode decodes

- DF17 and DF18 only.
- Airborne position, type codes 9–18: barometric altitude in feet (25 ft coding). Q=0 Gillham altitude is left invalid.
- Type codes 20–22: the 12-bit GNSS height in meters, converted to feet.
- Latitude and longitude only after an even and an odd CPR frame for that address, less than 10 seconds apart. Until then the position flag stays clear and altitude can still be set.
- Airborne velocity, type code 19 subtypes 1 and 2: ground speed in knots.
- Other DF17/DF18 squitters: ICAO and timestamp only.
- Surface position is not decoded.

## Host test

No radio, Pi, or ESP32:

```
python3 test/test_bench.py
```

Needs `gcc` and Python 3. It compiles `common/` and checks that the Python and C framers produce the same bytes, that both parsers resync, that raw mode does not alter a Beast stream, and that a published CPR pair decodes to 52.257202° N, 3.919373° E at 38000 ft.

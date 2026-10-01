# adsb

A Raspberry Pi running [readsb](https://github.com/wiedehopf/readsb) sends ADS-B out a UART. Two modes, selected with `--mode`:

| Mode | UART bytes |
| --- | --- |
| `raw` | readsb Beast output, unchanged |
| `struct` | one 32-byte record per DF17/DF18 squitter |

```
common/adsb_struct.h    layout of the struct record
pi/adsb_uart_sender.py  sender
test/test_bench.py      host test, no Pi required
```

## UART

Default device `/dev/serial0`, 115200 8N1. Override with `--uart` and `--baud`. `--uart -` writes the bytes to stdout.

Disable the serial login shell and leave the UART enabled (`raspi-config` → Interface Options → Serial Port), or the console will share the port.

## readsb

Beast binary output is TCP `127.0.0.1:30005` when networking is on. The sender connects to that port.

```
readsb --device-type rtlsdr --net --net-bo-port 30005
```

`--net-bo-port` defaults to 30005. The sender retries once a second until the port is open. `--beast-host` and `--beast-port` change the address. No extra Python packages.

## Run a mode

From the repo root:

```
python3 pi/adsb_uart_sender.py --mode raw --uart /dev/serial0 --baud 115200
python3 pi/adsb_uart_sender.py --mode struct --uart /dev/serial0 --baud 115200
```

Stop one before starting the other. Stderr prints a one-line count about once a second.

Raw mode is a byte copy of the TCP stream, including `0x1A` escaping. Struct mode decodes on the Pi and writes the record below.

## Beast framing

What raw mode puts on the wire. Each frame starts with `0x1A`.

| Type | After the `0x1A` |
| --- | --- |
| `'1'` (0x31) | 6-byte MLAT timestamp, 1 signal byte, 2-byte Mode A/C |
| `'2'` (0x32) | 6-byte MLAT, 1 signal byte, 7-byte short Mode S |
| `'3'` (0x33) | 6-byte MLAT, 1 signal byte, 14-byte long Mode S |

A `0x1A` inside the timestamp, signal, or payload is sent twice. Raw mode does not unescape or drop frames.

## Struct layout

`common/adsb_struct.h` is the layout. `pi/struct_frame.py` packs `<HBBIiiiHQH`. The host test checks those bytes against the header. 32 bytes, packed, little-endian.

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
| 22 | u64 | timestamp_us | Unix epoch, microseconds, set when the Pi sends the record |
| 30 | u16 | checksum | CRC-16/CCITT-FALSE over bytes 0..29 |

CRC-16/CCITT-FALSE: poly `0x1021`, init `0xFFFF`, xorout `0`, not reflected. `CRC("123456789") = 0x29B1`.

A cleared field uses a sentinel, because 0 is a real latitude and a real altitude: latitude and longitude `0x80000000`, altitude `0x80000000`, velocity `65535`. One record is one squitter, not a fused track, so a position message and a velocity message are two records.

DF18 addresses that are not ICAO still occupy the `icao` field. A squitter whose Mode S CRC is bad is not sent.

Struct mode fills fields as follows:

- DF17 and DF18 only.
- Airborne position, type codes 9–18: barometric altitude in feet (25 ft coding). Q=0 Gillham altitude is left invalid.
- Type codes 20–22: the 12-bit GNSS height in meters, converted to feet.
- Latitude and longitude only after an even and an odd CPR frame for that address, less than 10 seconds apart. Until then the position flag stays clear and altitude can still be set.
- Airborne velocity, type code 19 subtypes 1 and 2: ground speed in knots.
- Other DF17/DF18 squitters: ICAO and timestamp only.
- Surface position is not decoded.

## Host test

No radio and no UART:

```
python3 test/test_bench.py
```

Needs `gcc` and Python 3. It checks that raw mode does not alter a Beast stream, that struct records match `common/adsb_struct.h`, and that a published CPR pair encodes as 52.257202° N, 3.919373° E at 38000 ft.

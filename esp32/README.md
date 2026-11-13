# ESP32-S3 UART ingest

Arduino sketch that reads the 34-byte `TRCK` frames `trck` writes. It prints ICAO, callsign, lat, lon, alt, ground speed, track, emergency, and running CRC-fail and resync counts on USB `Serial`.

Open `esp32/trck_uart/trck_uart.ino`. Host check: `make -C esp32/host test`.

Board package: esp32 by Espressif Systems 3.3.x.

Arduino IDE board settings:

- Board: ESP32S3 Dev Module
- USB CDC On Boot: Enabled
- USB Mode: Hardware CDC and JTAG
- PSRAM: Disabled
- Upload Speed: 921600
- Serial Monitor: 115200 baud

If the monitor is empty because you are on the DevKit UART/COM port, set USB CDC On Boot to Disabled so `Serial` is UART0.

## Wire

3.3 V UART. Common ground. Pi TX is GPIO 14 (header pin 8). Firmware RX is GPIO 18 (`Serial1`).

| Pi | ESP32-S3 |
| --- | --- |
| GPIO 14 (pin 8) TX | GPIO 18 RX |
| GND (pin 6) | GND |

Do not use a 5 V adapter. Leave the Pi serial login shell off so `/dev/serial0` is the UART `trck` opens.

## What you should see

On the Pi, send the three known frames every second:

```
./trck --check
```

`--uart` defaults to `/dev/serial0`. `--baud` defaults to 115200. `trck replay FILE` and `trck stress` write the same frame layout.

On the Serial Monitor, after the banner, the same three lines each second (CRC-fail and resync stay 0 on a clean link):

```
icao=400ABC callsign=SIM0001 lat=10.5000000 lon=-20.2500000 alt=12000 gs=280 track=90 emergency=0 none crc_fail=0 resync=0
icao=40621D callsign=BAW123 lat=52.2572021 lon=3.9193726 alt=38000 gs=450 track=271 emergency=0 none crc_fail=0 resync=0
icao=4CA123 callsign=EZY456 lat=51.4700000 lon=-0.4500000 alt=8000 gs=220 track=180 emergency=1 general crc_fail=0 resync=0
```

Change `kPiRxGpio` in `trck_uart.ino` if GPIO 18 is not free on the board.

# ESP32-S3 UART ingest

Arduino sketch that reads the 30-byte `TRCK` frames `trck` writes, including `trck --check`. Prints ICAO, latitude, longitude, altitude, and emergency on `Serial`. Stays in that receive loop.

This is not the OBC radio firmware. It does not overlay a packed struct on the UART buffer. It does not forward the raw frame as a radio payload.

Open `esp32/trck_uart/trck_uart.ino`. That file is the whole sketch. Host CRC tests are `esp32/host/`, not this file.

Board package: **esp32 by Espressif Systems 3.3.12** ([arduino-esp32 3.3.12](https://github.com/espressif/arduino-esp32/releases/tag/3.3.12)).

Arduino IDE board settings:

- Board: **ESP32S3 Dev Module**
- USB CDC On Boot: **Enabled**
- USB Mode: **Hardware CDC and JTAG**
- PSRAM: **Disabled**
- Upload Speed: **921600**
- Serial Monitor: **115200 baud**

If the monitor is empty because you are on the DevKit UART/COM port, set USB CDC On Boot to **Disabled** so `Serial` is UART0.

Decoder only, no board package:

```
make -C esp32/host test
```

## Wire

3.3 V UART. Common ground. Pi TX is GPIO 14 (header pin 8). Firmware RX is GPIO 18 (`HardwareSerial` UART1).

| Pi | ESP32-S3 |
| --- | --- |
| GPIO 14 (pin 8) TX | GPIO 18 RX |
| GND (pin 6) | GND |

Do not use a 5 V adapter. Leave the Pi serial login shell off so `/dev/serial0` is the UART `trck` opens.

## What you should see

On the Pi:

```
./trck --check
```

`--check` writes the three known positions, pauses one second, and repeats.

On the Serial Monitor, after the banner, the same three lines each second:

```
icao=400ABC lat=10.5000000 lon=-20.2500000 alt=12000 emergency=0 none
icao=40621D lat=52.2572021 lon=3.9193726 alt=38000 emergency=0 none
icao=4CA123 lat=51.4700000 lon=-0.4500000 alt=8000 emergency=1 general
```

Change `kPiRxGpio` in `trck_uart.ino` if GPIO 18 is not free on the board.

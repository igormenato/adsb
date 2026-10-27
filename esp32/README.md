# ESP32-S3 UART ingest

Reads the 30-byte `TRCK` frames `trck` writes, including `trck --check`. Logs ICAO, latitude, longitude, altitude, and emergency on the USB-UART console. Stays in that receive loop.

This is not the OBC radio firmware. It does not overlay a packed struct on the UART buffer. It does not forward the raw frame as a radio payload.

Pinned ESP-IDF: **v5.5.6**.

```
git clone -b v5.5.6 --recursive https://github.com/espressif/esp-idf.git
```

Install that tree with its `install.sh` / `export.sh`, then:

```
cd esp32
idf.py set-target esp32s3
idf.py build
idf.py -p /dev/ttyACM0 flash monitor
```

Use `/dev/ttyUSB0` if that is the DevKit USB-UART port. Board: ESP32-S3-DevKitC-1 or another ESP32-S3 with USB-UART on UART0. Console is 115200 8N1 on that port.

Decoder only, no IDF:

```
make -C esp32/host test
```

## Wire

3.3 V UART. Common ground. Pi TX is GPIO 14 (header pin 8). Firmware RX is GPIO 18.

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

On the ESP32 monitor, after the banner, the same three lines each second:

```
icao=400ABC lat=10.5000000 lon=-20.2500000 alt=12000 emergency=0 none
icao=40621D lat=52.2572021 lon=3.9193726 alt=38000 emergency=0 none
icao=4CA123 lat=51.4700000 lon=-0.4500000 alt=8000 emergency=1 general
```

Change `PI_UART_RX_GPIO` in `main/main.c` if GPIO 18 is not free on the board.

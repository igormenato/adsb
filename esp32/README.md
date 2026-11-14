# ESP32-S3 UART ingest

Arduino sketch that reads the 34-byte `TRCK` frames `trck` writes. It prints ICAO, callsign, lat, lon, alt, ground speed, track, emergency, and running CRC-fail and resync counts on USB `Serial`. Type `ping`, `status`, `reboot`, `shutdown`, `pause`, `resume`, or `restart` on that Serial to send a 7-byte `TCMD` frame to the Pi. Replies are 21-byte `TACK` frames.

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

3.3 V UART. Common ground. Pi TX is GPIO 14 (header pin 8). Pi RX is GPIO 15 (header pin 10). Firmware RX is GPIO 11, TX is GPIO 10 (`Serial1`). The RF team uses ESP32 GPIO 11 for RX from the Pi.

| Pi | ESP32-S3 |
| --- | --- |
| GPIO 14 (pin 8) TX | GPIO 11 RX |
| GPIO 15 (pin 10) RX | GPIO 10 TX |
| GND (pin 6) | GND |

Do not use a 5 V adapter. Leave the Pi serial login shell off so `/dev/serial0` is the UART `trck` opens.

## Commands

On the USB Serial Monitor, send one word and a newline.

| Word | Pi action |
| --- | --- |
| `ping` | Reply `ok` with run state |
| `status` | Reply `ok` with run state, uptime seconds, frames sent, and last-fix age |
| `pause` | Stop CSV rows and `TRCK` frames. The radio stays open. Replies still go out |
| `resume` | Start sending `TRCK` frames again. `trck` starts running; there is no start command |
| `restart` | Reply, drain the UART, then `exec` the same binary and args |
| `reboot` | `systemctl reboot` if `trck` was started with `--allow-power-commands` |
| `shutdown` | `systemctl poweroff` with the same flag |

Without `--allow-power-commands`, reboot and shutdown are ignored and the reply status is `disabled`. `trck` must run as a user that may call those `systemctl` verbs, typically root. There is no systemd unit in this repo. Pause, resume, and restart do not need the flag. Restart uses `exec` so the process comes back without a unit. While paused, new positions are discarded at the UART/CSV entry. `last_fix_age` keeps growing. Resume does not flood the wire.

Change `kPiRxGpio` / `kPiTxGpio` in `trck_uart.ino` if those pins are not free on the board.

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


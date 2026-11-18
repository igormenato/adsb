# ESP32-S3 UART ingest

Arduino sketch that reads the 34-byte `TRCK` frames `trck` writes. It prints ICAO, callsign, lat, lon, alt, ground speed, track, emergency, and running CRC-fail and resync counts on USB `Serial`. Type `ping`, `status`, `reboot`, `shutdown`, `pause`, `resume`, `restart`, or `simulation` on that Serial (any case) to send `PING`, `STATUS`, `REBOOT`, `SHUTDOWN`, `PAUSE`, `RESUME`, `RESTART`, or `SIMULATION` plus `\r\n` on UART1. Replies are 9-byte `TACK` frames (`TACK`, cmd, status, health, CRC-16 over bytes 0..6). Health is `ok`, `paused`, or `sim`. Every 3 s the sketch also sends `STATUS_ADSB\r\n`. The Pi replies with a STATUS `TACK`. The sketch does not wait for that heartbeat. It matches a user command's TACK on the cmd byte, so a STATUS heartbeat cannot complete `PAUSE`. USB prints a heartbeat TACK only when health changes, and prints `trck down` once if no TACK arrives for 10 s. `trck_health()` returns the last health, or `down` (255). Type `stats` to print frames received, `crc_fail`, `resync`, and `qdrop` (`qdrop` is events dropped when the print queue is full). UART1 RX runs in its own FreeRTOS task. After a command line, the sketch waits up to 1 s for the matching `TACK` and retries ping/status/pause/resume/simulation once. For reboot/shutdown/restart it sends `STATUS` instead of repeating the command. Both attempts failing prints `no reply`.

Two firmware trees: Arduino sketch `esp32/trck_uart/trck_uart.ino` (self-contained, protocol inlined) and PlatformIO `esp32/pio` (`src/main.cpp`, protocol in `include/trck_frame.h`). Host check includes the Arduino `.ino` (same as before the shared header, simpler than pointing at the PlatformIO header): `make -C esp32/host test`.

Board package: esp32 by Espressif Systems 3.3.x. PlatformIO board id: `esp32-s3-devkitc-1` (ESP32-S3-DevKitC-1).

## Arduino IDE

- Board: ESP32S3 Dev Module
- USB CDC On Boot: Enabled
- USB Mode: Hardware CDC and JTAG
- PSRAM: Disabled
- Upload Speed: 921600
- Serial Monitor: 115200 baud

If the monitor is empty because you are on the DevKit UART/COM port, set USB CDC On Boot to Disabled so `Serial` is UART0.

## PlatformIO

Standalone PlatformIO C++ project in `esp32/pio`. From that directory:

```
pio run
pio run -t upload
pio device monitor
```

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
| `ping` | Reply `ok` with health |
| `status` | Reply `ok` with health |
| `pause` | Stop CSV rows and `TRCK` frames. The radio stays open. Replies still go out. Pauses live or simulation |
| `resume` | Return to live radio/hex. Leaves simulation. Resume from pause-during-sim also returns to live |
| `restart` | Reply, drain the UART, then exit 1 so systemd starts trck again (live) |
| `simulation` | Stop forwarding radio positions. Loop a scenario through the decoder onto UART/CSV. Radio stays open |
| `reboot` | `systemctl reboot` if `trck` was started with `--allow-power-commands` |
| `shutdown` | `systemctl poweroff` with the same flag |
| `stats` | ESP32 only: print `frames`, `crc_fail`, `resync`, `qdrop`. No UART command |

Those USB words (except `stats`) go to the Pi as the uppercase line plus `\r\n`. `STATUS_ADSB\r\n` is sent every 3 s. The Pi STATUS TACK is not waited on for that send.

Without `--allow-power-commands`, reboot and shutdown are ignored and the reply status is `disabled`. Pause, resume, restart, and simulation do not need the flag. `sudo trck install-service` installs a root systemd unit with `--allow-power-commands` (`Restart=always`, `StartLimitIntervalSec=0`) so reboot, shutdown, `trck update`, and UART restart work without ssh. While paused, new positions are discarded at the UART/CSV entry and health is `paused` (paused wins over sim). Resume does not flood the wire. A second `SIMULATION` while already running is `ok` and a no-op. While paused during sim, `SIMULATION` unpauses and continues.

The RF firmware (ESP32 TX GPIO 10 to Pi RX GPIO 15) sends the same commands as uppercase ASCII lines. `trck` accepts those next to `TCMD` on the same RX stream. Pi TX still writes binary `TRCK` frames and binary `TACK` replies.

| Line | Pi action |
| --- | --- |
| `PING` | Same `TACK` as TCMD ping |
| `STATUS` | Same `TACK` as TCMD status |
| `PAUSE` | Same as TCMD pause |
| `RESUME` | Same as TCMD resume |
| `RESTART` | Same as TCMD restart |
| `SIMULATION` | Same as TCMD simulation (cmd 7) |
| `REBOOT` | Same as TCMD reboot, including `--allow-power-commands` |
| `SHUTDOWN` | Same as TCMD shutdown, including `--allow-power-commands` |
| `STATUS_ADSB` | Heartbeat every 3 s. Same `TACK` as `STATUS`. ESP treats a missing reply as trck down |

`TACK` is 9 bytes, little-endian. CRC-16/CCITT-FALSE covers bytes 0..6.

| Offset | Size | Field |
| --- | --- | --- |
| 0 | 4 | `TACK` |
| 4 | 1 | cmd |
| 5 | 1 | status |
| 6 | 1 | health: `ok` 0, `paused` 1, `sim` 2 |
| 7 | 2 | CRC |

Lines end in `\r\n` or `\n`. Empty lines, unknown lines, and lines longer than 32 bytes are ignored. `trck` may print a rate-limited stderr note.

Change `kPiRxGpio` / `kPiTxGpio` in the sketch and in `esp32/pio/src/main.cpp` if those pins are not free on the board.

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

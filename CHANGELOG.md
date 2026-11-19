# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- `sudo trck update` replaces `/usr/local/bin/trck` and `systemctl try-restart trck` when the service is installed. Success exits 0.

## [0.5.1](https://github.com/igormenato/adsb/compare/v0.5.0...v0.5.1) - 2026-11-19

### Added

- *(uart)* drop ASCII STATUS and STATUS_ADSB ([#127](https://github.com/igormenato/adsb/pull/127))

### Changed

- UART ASCII `STATUS` and `STATUS_ADSB` are ignored with no `TACK`. ESP32 sends `PING` only on USB `ping`. No TACK after one retry is health `down` (255). Binary TCMD status is unchanged.

## [0.5.0](https://github.com/igormenato/adsb/compare/v0.4.0...v0.5.0) - 2026-11-18

### Added

- *(uart)* SIMULATION command loops scenario onto UART ([#123](https://github.com/igormenato/adsb/pull/123))
- *(uart)* shrink TACK to a health byte ([#122](https://github.com/igormenato/adsb/pull/122))

### Fixed

- *(uart)* keep inbound bytes across hangup reopen ([#126](https://github.com/igormenato/adsb/pull/126))

### Fixed

- UART reopen no longer flushes a command that already arrived on the new port, so hangup then Pause still TACKs.

### Added

- UART `SIMULATION` (ASCII line and TCMD 7) loops a scenario through the live decoder onto UART/CSV. `sim.toml` in the cwd overrides the built-in UTFPR Curitiba file. Health `sim` is 2. `RESUME` returns to live.

### Changed

- TACK is 9 bytes: `TACK`, cmd, status, health, CRC-16/CCITT-FALSE over bytes 0..6. Health is `ok` (0), `paused` (1), or `sim` (2). Uptime, frames sent, last-fix age, and the paused byte are gone.
- ESP32 heartbeat STATUS TACKs print only when health changes. USB prints health as `ok`, `paused`, or `sim`. `trck_health()` returns the last health, or `down` (255) when no TACK has arrived or when 10 s have passed without one.

## [0.4.0](https://github.com/igormenato/adsb/compare/v0.3.5...v0.4.0) - 2026-11-18

### Added

- *(esp32)* send ASCII command lines instead of TCMD ([#119](https://github.com/igormenato/adsb/pull/119))
- *(uart)* accept ASCII command lines next to TCMD ([#117](https://github.com/igormenato/adsb/pull/117))
- *(pi)* install a root systemd unit and exit 1 to restart ([#116](https://github.com/igormenato/adsb/pull/116))

### Fixed

- *(esp32)* unpack TACK status Failed ([#118](https://github.com/igormenato/adsb/pull/118))
- *(uart)* pause under one lock, queue TACKs, ACK after systemctl ([#115](https://github.com/igormenato/adsb/pull/115))
- *(decode)* ingest 7-byte Mode S, DF18 CF=0, and idle 7700 ([#112](https://github.com/igormenato/adsb/pull/112))

### Other

- Document trck flight sim for Pi UART end-to-end ([#121](https://github.com/igormenato/adsb/pull/121))
- *(radio)* cu8 mag LUT, 256 KiB URBs, and USB drop count ([#113](https://github.com/igormenato/adsb/pull/113))
- Fix FC0013 manual gain test after DEFAULT_GAIN removal ([#110](https://github.com/igormenato/adsb/pull/110))

### Added

- UART RX accepts uppercase ASCII command lines (`PING`, `STATUS`, `PAUSE`, `RESUME`, `RESTART`, `REBOOT`, `SHUTDOWN`) next to `TCMD`. `STATUS_ADSB` replies with a STATUS `TACK`.
- `trck install-service` installs a root systemd unit (`Restart=always`, `StartLimitIntervalSec=0`, `--allow-power-commands`). Field flags go in `systemctl edit trck`.

### Fixed

- ESP32 `trck_unpack_ack` accepts TACK status `Failed` (3).

### Changed

- ESP32 UART to the Pi sends uppercase ASCII command lines plus a `STATUS_ADSB` heartbeat every 3 s, not binary `TCMD`. Heartbeat STATUS TACKs print only on pause change. Missing TACK for 10 s prints `trck down` once.
- UART `restart` and `trck update` exit 1 after success so systemd starts the process again.

## [0.3.5](https://github.com/igormenato/adsb/compare/v0.3.4...v0.3.5) - 2026-11-17

### Added

- *(trck)* add trck update from GitHub Releases ([#108](https://github.com/igormenato/adsb/pull/108))

### Other

- Default --gain to the tuner's maximum table step ([#109](https://github.com/igormenato/adsb/pull/109))
- Fix FC0013 manual gain AGC (ADSB-35) ([#106](https://github.com/igormenato/adsb/pull/106))

### Changed

- `--gain` defaults to the tuner's maximum advertised table step instead of a hardcoded 49.6 dB.

### Added

- `trck update` installs `trck-aarch64` from GitHub Releases with `curl`. `--version X.Y.Z` selects that tag.

### Fixed

- FC0013 `--gain` now switches the tuner out of AGC (0x0d bit 3 and IF 0x13 = 0x0a) before writing LNA, matching librtlsdr.

## [0.3.4](https://github.com/igormenato/adsb/compare/v0.3.3...v0.3.4) - 2026-11-17

### Added

- log short Mode S frames as seven bytes ([#103](https://github.com/igormenato/adsb/pull/103))
- recover single-bit Mode S CRC errors ([#94](https://github.com/igormenato/adsb/pull/94))

### Other

- Remove PlatformIO job from GitHub Actions ([#105](https://github.com/igormenato/adsb/pull/105))
- Demodulate 2.4 MS/s with readsb phase slicers ([#104](https://github.com/igormenato/adsb/pull/104))
- Raise CPR position yield with local decode ([#98](https://github.com/igormenato/adsb/pull/98))

### Added

- 1-bit Mode S CRC correction on the IQ path (DF17/DF18, and known-ICAO DF11). `--no-fix` turns it off.
- CPR tracking uses a 10 s even/odd global pair, then a local decode from the last good position, and rejects a jump that fails dump1090-fa's speed check.
- IQ `msg` lines print 7 bytes for short Mode S frames and 14 for long frames. UART frames are unchanged.
- 2.4 MS/s demodulation follows readsb `demod_2400.c` (phase slicers, DF early-out, skip). 1-bit PI correction is unchanged.

## [0.3.3](https://github.com/igormenato/adsb/compare/v0.3.2...v0.3.3) - 2026-11-16

### Added

- *(rtlsdr)* add FC0013 tuner support ([#93](https://github.com/igormenato/adsb/pull/93))

### Other

- *(release)* build aarch64 musl natively on ubuntu-24.04-arm ([#92](https://github.com/igormenato/adsb/pull/92))
- rewrite the README into short task sections ([#90](https://github.com/igormenato/adsb/pull/90))

### Added

- FC0013 tuner support for generic RTL2832U dongles

## [0.3.2](https://github.com/igormenato/adsb/compare/v0.3.1...v0.3.2) - 2026-11-16

### Added

- add --gain for tuner AGC or a dB value ([#85](https://github.com/igormenato/adsb/pull/85))
- *(esp32)* keep UART RX alive under load ([#84](https://github.com/igormenato/adsb/pull/84))
- *(uart)* add pause, resume, and restart commands ([#83](https://github.com/igormenato/adsb/pull/83))
- *(uart)* accept host commands from the ESP32 ([#82](https://github.com/igormenato/adsb/pull/82))

### Other

- run GitHub Actions on main ([#89](https://github.com/igormenato/adsb/pull/89))
- *(release)* run release-plz on trck-v2 ([#87](https://github.com/igormenato/adsb/pull/87))

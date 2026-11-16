# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

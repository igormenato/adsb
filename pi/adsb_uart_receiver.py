#!/usr/bin/env python3
"""Read the bench UART and print Beast frames and packed structs.

This is the Pi-side peer of adsb_uart_sender.py. It accepts both framings
on one stream, the same way the optional ESP32 sketch does: Beast frames
resync on 0x1A, and struct frames resync on the magic header plus CRC.
"""

from __future__ import annotations

import argparse
import os
import select
import sys
import time
from dataclasses import dataclass

from beast import BEAST_ESC, BeastMessage, BeastParser
from struct_frame import (
    ADSB_FLAG_ALTITUDE,
    ADSB_FLAG_POSITION,
    ADSB_FLAG_VELOCITY,
    ADSB_STRUCT_SIZE,
    ADSB_STRUCT_VERSION,
    ADSB_WIRE_MAGIC,
    TrackStruct,
    crc16_ccitt_false,
    unpack_message,
)
from uart_port import open_serial

DEFAULT_UART = "/dev/serial0"
DEFAULT_BAUD = 115200

_HUNT = "hunt"
_BEAST = "beast"
_STRUCT_MAGIC = "struct-magic"
_STRUCT_BODY = "struct-body"


@dataclass
class RxStats:
    beast_ok: int = 0
    beast_crc_bad: int = 0
    beast_framing_errors: int = 0
    struct_ok: int = 0
    struct_checksum_errors: int = 0
    struct_version_errors: int = 0
    junk_bytes: int = 0


def address_of(msg: BeastMessage) -> int | None:
    """DF11/17/18 address, or None when the squitter has no AA field."""
    if len(msg.payload) < 7:
        return None
    df = msg.payload[0] >> 3
    if df not in (11, 17, 18):
        return None
    return (msg.payload[1] << 16) | (msg.payload[2] << 8) | msg.payload[3]


def format_e7(value: int) -> str:
    sign = "-" if value < 0 else ""
    mag = -value if value < 0 else value
    return f"{sign}{mag // 10_000_000}.{mag % 10_000_000:07d}"


def format_beast(msg: BeastMessage, stats: RxStats) -> str:
    line = (
        f"beast type={chr(msg.type)} crc={msg.crc_ok} "
        f"count={stats.beast_ok} frame_err={stats.beast_framing_errors} "
        f"crc_err={stats.beast_crc_bad}"
    )
    if msg.payload:
        line += f" df={msg.payload[0] >> 3}"
    addr = address_of(msg)
    if addr is not None:
        line += f" icao={addr:06X}"
    return line


def format_struct(msg: TrackStruct, stats: RxStats) -> str:
    if msg.flags & ADSB_FLAG_POSITION:
        latlon = f"lat={format_e7(msg.latitude_e7)} lon={format_e7(msg.longitude_e7)} "
    else:
        latlon = "lat=- lon=- "
    alt = f"alt={msg.altitude_ft}ft " if msg.flags & ADSB_FLAG_ALTITUDE else "alt=- "
    vel = f"vel={msg.velocity_kt}kt " if msg.flags & ADSB_FLAG_VELOCITY else "vel=- "
    return (
        f"struct icao={msg.icao:06X} flags=0x{msg.flags:02X} "
        f"count={stats.struct_ok} crc_err={stats.struct_checksum_errors} "
        f"{latlon}{alt}{vel}t={msg.timestamp_us}"
    )


def format_beat(stats: RxStats) -> str:
    return (
        f"beat beast={stats.beast_ok} struct={stats.struct_ok} "
        f"frame_err={stats.beast_framing_errors} beast_crc={stats.beast_crc_bad} "
        f"struct_crc={stats.struct_checksum_errors} ver_err={stats.struct_version_errors} "
        f"junk={stats.junk_bytes}"
    )


class BenchReceiver:
    """Byte stream demux. feed() returns the log lines for the bytes consumed."""

    def __init__(self) -> None:
        self.stats = RxStats()
        self._mode = _HUNT
        self._beast: BeastParser | None = None
        self._window = bytearray()
        self._replay = bytearray()
        self._replay_i = 0

    def beat_line(self) -> str:
        return format_beat(self.stats)

    def feed(self, data: bytes) -> list[str]:
        lines: list[str] = []
        index = 0
        guard = 0
        limit = max(1000, len(data) * 64)
        while (index < len(data) or self._replay_i < len(self._replay)) and guard < limit:
            guard += 1
            if self._replay_i < len(self._replay):
                byte = self._replay[self._replay_i]
                self._replay_i += 1
            else:
                byte = data[index]
                index += 1
            lines.extend(self._step(byte))
        if guard >= limit and (index < len(data) or self._replay_i < len(self._replay)):
            self.stats.beast_framing_errors += 1
            self._replay = bytearray()
            self._replay_i = 0
            self._mode = _HUNT
            lines.append(f"err beast_framing frame_err={self.stats.beast_framing_errors}")
        return lines

    def _replay_now(self, data: bytes) -> None:
        left = self._replay[self._replay_i :]
        merged = bytes(data) + bytes(left)
        if len(merged) > 128:
            self.stats.beast_framing_errors += 1
            self._replay = bytearray()
            self._replay_i = 0
            self._mode = _HUNT
            return
        self._replay = bytearray(merged)
        self._replay_i = 0

    def _step(self, byte: int) -> list[str]:
        if self._mode == _BEAST:
            return self._step_beast(byte)
        if self._mode == _STRUCT_MAGIC:
            return self._step_magic(byte)
        if self._mode == _STRUCT_BODY:
            return self._step_body(byte)
        if byte == BEAST_ESC:
            self._beast = BeastParser()
            self._mode = _BEAST
            self._beast.push(byte)
            return []
        if byte == ADSB_WIRE_MAGIC[0]:
            self._mode = _STRUCT_MAGIC
            return []
        self.stats.junk_bytes += 1
        return []

    def _step_beast(self, byte: int) -> list[str]:
        assert self._beast is not None
        before = self._beast.framing_errors
        status, frames = self._beast.push(byte)
        lines: list[str] = []
        if self._beast.framing_errors != before:
            self.stats.beast_framing_errors += self._beast.framing_errors - before
            lines.append(f"err beast_framing frame_err={self.stats.beast_framing_errors}")
        if frames:
            msg = frames[0]
            self.stats.beast_ok += 1
            if msg.crc_ok == 0:
                self.stats.beast_crc_bad += 1
            lines.append(format_beast(msg, self.stats))
            self._mode = _HUNT
            self._beast = None
            return lines
        if status == "retry":
            self._mode = _HUNT
            self._beast = None
            self._replay_now(bytes((byte,)))
        return lines

    def _step_magic(self, byte: int) -> list[str]:
        if byte == ADSB_WIRE_MAGIC[1]:
            self._window = bytearray(ADSB_WIRE_MAGIC)
            self._mode = _STRUCT_BODY
            return []
        self.stats.junk_bytes += 1
        self._mode = _HUNT
        self._replay_now(bytes((byte,)))
        return []

    def _step_body(self, byte: int) -> list[str]:
        self._window.append(byte)
        if len(self._window) < ADSB_STRUCT_SIZE:
            return []
        frame = bytes(self._window)
        kind = _classify_struct(frame)
        if isinstance(kind, TrackStruct):
            self._window = bytearray()
            self._mode = _HUNT
            self.stats.struct_ok += 1
            return [format_struct(kind, self.stats)]
        return self._fail_struct(kind)

    def _fail_struct(self, kind: str) -> list[str]:
        if kind == "version":
            self.stats.struct_version_errors += 1
            line = f"err struct_version ver_err={self.stats.struct_version_errors}"
        else:
            self.stats.struct_checksum_errors += 1
            line = f"err struct_checksum crc_err={self.stats.struct_checksum_errors}"
        tail = bytes(self._window[1:])
        self._window = bytearray()
        self._mode = _HUNT
        if tail:
            self._replay_now(tail)
        return [line]


def _classify_struct(frame: bytes) -> TrackStruct | str:
    import struct

    if len(frame) != ADSB_STRUCT_SIZE or frame[:2] != ADSB_WIRE_MAGIC:
        return "checksum"
    (got,) = struct.unpack_from("<H", frame, 30)
    if crc16_ccitt_false(frame[:30]) != got:
        return "checksum"
    if frame[2] != ADSB_STRUCT_VERSION:
        return "version"
    decoded = unpack_message(frame)
    if decoded is None:
        return "checksum"
    return decoded


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Read Beast or packed-struct ADS-B from a UART and print each frame."
    )
    parser.add_argument("--uart", default=DEFAULT_UART, help=f"UART device, or - for stdin (default {DEFAULT_UART})")
    parser.add_argument("--baud", type=int, default=DEFAULT_BAUD)
    return parser.parse_args(argv)


def _read_chunk(fd: int, is_tty: bool) -> bytes | None:
    """Return bytes, b'' when a tty has nothing yet, or None on EOF."""
    readable, _, _ = select.select([fd], [], [], 0.2)
    if not readable:
        return b""
    chunk = os.read(fd, 4096)
    if chunk:
        return chunk
    if is_tty:
        return b""
    return None


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    own_fd = False
    if args.uart == "-":
        fd = sys.stdin.buffer.fileno()
        is_tty = False
    else:
        fd = open_serial(args.uart, args.baud)
        own_fd = True
        is_tty = True
    receiver = BenchReceiver()
    print("adsb bench pi: raw Beast (0x1A) and struct (0x5B 0xAD)", flush=True)
    print(f"uart={args.uart} baud={args.baud}", flush=True)
    last_beat = time.monotonic()
    try:
        while True:
            chunk = _read_chunk(fd, is_tty)
            if chunk is None:
                break
            if chunk:
                for line in receiver.feed(chunk):
                    print(line, flush=True)
            now = time.monotonic()
            if now - last_beat >= 5.0:
                print(receiver.beat_line(), flush=True)
                last_beat = now
    except KeyboardInterrupt:
        print("stopped", file=sys.stderr)
    finally:
        if own_fd:
            os.close(fd)
    return 0


if __name__ == "__main__":
    sys.exit(main())

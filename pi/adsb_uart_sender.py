#!/usr/bin/env python3
"""Send readsb Beast output to a UART, either unchanged or as packed structs.

Raw mode writes the TCP bytes to the UART with no framing changes.
Struct mode uses pyModeS to decode DF17/DF18 and writes one 32-byte
record per squitter. The record layout is the table in the README.
"""

from __future__ import annotations

import argparse
import math
import socket
import sys
import time

from pyModeS import decode as modes_decode
from pyModeS.cli._source import _REMAINDER_CAP, _parse_beast_buffer
from struct_frame import (
    ADSB_FLAG_ALTITUDE,
    ADSB_FLAG_POSITION,
    ADSB_FLAG_VELOCITY,
    ADSB_VEL_INVALID,
    TrackStruct,
    pack_message,
)
from uart_port import drain, open_serial

DEFAULT_HOST = "127.0.0.1"
DEFAULT_PORT = 30005
DEFAULT_UART = "/dev/serial0"
DEFAULT_BAUD = 115200


def forward_raw(chunk: bytes) -> bytes:
    """Raw Beast mode is the readsb bytes, unchanged."""
    return chunk


def _clock() -> tuple[float, int]:
    return time.time(), time.time_ns() // 1000


def _deg_e7(degrees: float) -> int:
    scaled = math.floor(abs(degrees) * 10_000_000 + 0.5)
    return -scaled if degrees < 0 else scaled


class StructForwarder:
    """One UART record per DF17/DF18 squitter. Position is this squitter only."""

    def __init__(self) -> None:
        self._pending = b""
        self._even: dict[str, tuple[str, float]] = {}
        self._odd: dict[str, tuple[str, float]] = {}
        self.crc_drops = 0
        self.sent = 0

    def feed(self, chunk: bytes, clock=_clock) -> list[bytes]:
        frames, remainder = _parse_beast_buffer(self._pending + chunk)
        self._pending = remainder[-_REMAINDER_CAP:]
        encoded: list[bytes] = []
        for _mlat, payload_hex in frames:
            record = self._record(payload_hex, clock)
            if record is None:
                continue
            encoded.append(pack_message(record))
            self.sent += 1
        return encoded

    def _record(self, payload_hex: str, clock) -> TrackStruct | None:
        result = modes_decode(payload_hex)
        if result.get("df") not in (17, 18):
            return None
        if not result.get("crc_valid"):
            self.crc_drops += 1
            return None
        now_s, now_us = clock()
        track = TrackStruct.empty(int(result["icao"], 16), now_us)
        tc = result.get("typecode")
        if isinstance(tc, int) and (9 <= tc <= 18 or 20 <= tc <= 22):
            alt = result.get("altitude")
            if alt is not None:
                track.altitude_ft = int(alt)
                track.flags |= ADSB_FLAG_ALTITUDE
            lat, lon = self._position(result, payload_hex, now_s)
            if lat is not None and lon is not None:
                track.latitude_e7 = _deg_e7(lat)
                track.longitude_e7 = _deg_e7(lon)
                track.flags |= ADSB_FLAG_POSITION
        elif tc == 19:
            gs = result.get("groundspeed")
            if gs is not None:
                track.velocity_kt = min(math.floor(gs + 0.5), ADSB_VEL_INVALID - 1)
                track.flags |= ADSB_FLAG_VELOCITY
        return track

    def _position(self, result: dict, payload_hex: str, now_s: float) -> tuple[float | None, float | None]:
        icao = result["icao"]
        odd = result.get("cpr_format") == 1
        slot = self._odd if odd else self._even
        other = self._even if odd else self._odd
        prev = other.get(icao)
        slot[icao] = (payload_hex, now_s)
        if prev is None:
            return None, None
        paired = modes_decode([prev[0], payload_hex], timestamps=[prev[1], now_s])
        current = paired[-1]
        return current.get("latitude"), current.get("longitude")


class _Stdout:
    def write(self, data: bytes) -> None:
        sys.stdout.buffer.write(data)
        sys.stdout.buffer.flush()

    def close(self) -> None:
        pass


class _Uart:
    def __init__(self, port) -> None:
        self._port = port

    def write(self, data: bytes) -> None:
        view = memoryview(data)
        while view:
            wrote = self._port.write(view)
            if not wrote:
                raise OSError("UART write failed")
            view = view[wrote:]
        drain(self._port)

    def close(self) -> None:
        self._port.close()


def open_output(path: str, baud: int):
    if path == "-":
        return _Stdout()
    return _Uart(open_serial(path, baud))


def connect(host: str, port: int) -> socket.socket:
    while True:
        try:
            sock = socket.create_connection((host, port), timeout=5)
        except OSError as exc:
            print(f"waiting for readsb at {host}:{port}: {exc}", file=sys.stderr)
            time.sleep(1)
            continue
        sock.settimeout(1.0)
        print(f"connected to readsb at {host}:{port}", file=sys.stderr)
        return sock


def parse_args(argv: list[str] | None = None) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Forward readsb Beast output to a UART as raw Beast or packed structs."
    )
    parser.add_argument("--mode", choices=("raw", "struct"), required=True)
    parser.add_argument("--uart", default=DEFAULT_UART, help=f"UART device, or - for stdout (default {DEFAULT_UART})")
    parser.add_argument("--baud", type=int, default=DEFAULT_BAUD)
    parser.add_argument("--beast-host", default=DEFAULT_HOST)
    parser.add_argument("--beast-port", type=int, default=DEFAULT_PORT)
    return parser.parse_args(argv)


def main(argv: list[str] | None = None) -> int:
    args = parse_args(argv)
    output = open_output(args.uart, args.baud)
    forwarder = StructForwarder() if args.mode == "struct" else None
    total_raw = 0
    last_log = time.monotonic()
    print(
        f"mode={args.mode} uart={args.uart} baud={args.baud} beast={args.beast_host}:{args.beast_port}",
        file=sys.stderr,
    )
    try:
        while True:
            sock = connect(args.beast_host, args.beast_port)
            try:
                while True:
                    try:
                        chunk = sock.recv(4096)
                    except socket.timeout:
                        chunk = None
                    if chunk == b"":
                        print("readsb closed the connection", file=sys.stderr)
                        break
                    if chunk:
                        if args.mode == "raw":
                            output.write(forward_raw(chunk))
                            total_raw += len(chunk)
                        else:
                            frames = forwarder.feed(chunk)
                            if frames:
                                output.write(b"".join(frames))
                    now = time.monotonic()
                    if now - last_log >= 1.0:
                        if args.mode == "raw":
                            print(f"raw forwarded {total_raw} bytes", file=sys.stderr)
                        else:
                            print(
                                f"struct sent {forwarder.sent} crc_dropped {forwarder.crc_drops}",
                                file=sys.stderr,
                            )
                        last_log = now
            except OSError as exc:
                print(f"beast connection lost: {exc}", file=sys.stderr)
            finally:
                sock.close()
    except KeyboardInterrupt:
        print("stopped", file=sys.stderr)
    finally:
        output.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())

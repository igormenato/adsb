#!/usr/bin/env python3
"""Send readsb Beast output to a UART, either unchanged or as packed structs.

Raw mode writes the TCP bytes to the UART with no framing changes.
Struct mode decodes DF17/DF18 on the Pi and writes one 32-byte record
per squitter. The record layout is the table in the README.
"""

from __future__ import annotations

import argparse
import os
import socket
import sys
import time

from adsb_decode import CprCache, decode_adsb
from beast import BeastParser
from struct_frame import pack_message
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


class StructForwarder:
    def __init__(self) -> None:
        self.parser = BeastParser()
        self.cache = CprCache()
        self.crc_drops = 0
        self.sent = 0

    def feed(self, chunk: bytes, clock=_clock) -> list[bytes]:
        encoded: list[bytes] = []
        for msg in self.parser.feed(chunk):
            if msg.crc_ok != 1:
                if msg.crc_ok == 0:
                    self.crc_drops += 1
                continue
            now_s, now_us = clock()
            decoded = decode_adsb(msg, self.cache, now_s, now_us)
            if decoded is None:
                continue
            encoded.append(pack_message(decoded))
            self.sent += 1
        return encoded


class _Stdout:
    def write(self, data: bytes) -> None:
        sys.stdout.buffer.write(data)
        sys.stdout.buffer.flush()

    def close(self) -> None:
        pass


class _Uart:
    def __init__(self, fd: int) -> None:
        self._fd = fd

    def write(self, data: bytes) -> None:
        view = memoryview(data)
        while view:
            wrote = os.write(self._fd, view)
            if wrote <= 0:
                raise OSError("UART write failed")
            view = view[wrote:]
        drain(self._fd)

    def close(self) -> None:
        os.close(self._fd)


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

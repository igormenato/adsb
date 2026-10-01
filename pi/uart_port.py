"""UART open and baud settings for the Pi sender."""

from __future__ import annotations

import os
import termios


def drain(fd: int) -> None:
    """Wait until the UART accepts the bytes already written."""
    termios.tcdrain(fd)


def open_serial(path: str, baud: int) -> int:
    """Open a raw 8N1 UART and return the file descriptor."""
    rates = {
        9600: termios.B9600,
        19200: termios.B19200,
        38400: termios.B38400,
        57600: termios.B57600,
        115200: termios.B115200,
        230400: termios.B230400,
    }
    for name in ("B460800", "B921600"):
        value = getattr(termios, name, None)
        if value is not None:
            rates[int(name[1:])] = value
    if baud not in rates:
        known = ", ".join(str(rate) for rate in sorted(rates))
        raise SystemExit(f"unsupported baud {baud}; choose one of: {known}")
    try:
        fd = os.open(path, os.O_RDWR | os.O_NOCTTY)
    except OSError as exc:
        raise SystemExit(f"cannot open {path}: {exc}") from exc
    attrs = termios.tcgetattr(fd)
    attrs[0] = 0
    attrs[1] = 0
    attrs[2] = termios.CS8 | termios.CREAD | termios.CLOCAL
    attrs[3] = 0
    attrs[4] = rates[baud]
    attrs[5] = rates[baud]
    attrs[6][termios.VMIN] = 0
    attrs[6][termios.VTIME] = 0
    termios.tcsetattr(fd, termios.TCSANOW, attrs)
    termios.tcflush(fd, termios.TCIOFLUSH)
    return fd

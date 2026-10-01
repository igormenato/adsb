"""UART open and baud settings for the Pi sender."""

from __future__ import annotations

import serial


def drain(port: serial.Serial) -> None:
    """Wait until the UART accepts the bytes already written."""
    port.flush()


def open_serial(path: str, baud: int) -> serial.Serial:
    """Open a raw 8N1 UART."""
    try:
        port = serial.Serial(
            path,
            baudrate=baud,
            bytesize=serial.EIGHTBITS,
            parity=serial.PARITY_NONE,
            stopbits=serial.STOPBITS_ONE,
            timeout=0,
            xonxoff=False,
            rtscts=False,
            dsrdtr=False,
        )
    except (serial.SerialException, OSError, ValueError) as exc:
        raise SystemExit(f"cannot open {path}: {exc}") from exc
    port.reset_input_buffer()
    port.reset_output_buffer()
    return port

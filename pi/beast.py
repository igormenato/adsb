"""Beast binary frames, as readsb writes them on TCP port 30005.

A frame is 0x1A, a type byte ('1', '2', or '3'), then a 6-byte MLAT
timestamp, a signal byte, and the Mode S payload. Any 0x1A after the
opening marker is escaped as 0x1A 0x1A. Raw mode on the Pi copies these
bytes unchanged; this module is the parser the struct path and the host
test use.
"""

from __future__ import annotations

from dataclasses import dataclass

BEAST_ESC = 0x1A
TYPE_LEN = {0x31: 2, 0x32: 7, 0x33: 14}

_HUNT = 0
_TYPE = 1
_DATA = 2
_ESC = 3


def modes_crc24(msg: bytes) -> int:
    """Mode S CRC-24. A valid DF17/DF18 squitter returns 0."""
    crc = 0
    for byte in msg:
        crc ^= byte << 16
        for _ in range(8):
            if crc & 0x800000:
                crc = ((crc << 1) ^ 0xFFF409) & 0xFFFFFF
            else:
                crc = (crc << 1) & 0xFFFFFF
    return crc


@dataclass(frozen=True)
class BeastMessage:
    type: int
    mlat: bytes
    signal: int
    payload: bytes
    crc_ok: int  # 1 good DF17/18, 0 bad DF17/18, -1 not checked


def encode(msg_type: int, mlat: bytes, signal: int, payload: bytes) -> bytes:
    if msg_type not in TYPE_LEN:
        raise ValueError(f"unsupported beast type {msg_type:#x}")
    if len(mlat) != 6:
        raise ValueError("MLAT timestamp must be 6 bytes")
    if len(payload) != TYPE_LEN[msg_type]:
        raise ValueError("payload length does not match beast type")
    raw = bytes((msg_type,)) + mlat + bytes((signal & 0xFF,)) + payload
    out = bytearray((BEAST_ESC,))
    for byte in raw:
        out.append(byte)
        if byte == BEAST_ESC:
            out.append(BEAST_ESC)
    return bytes(out)


class BeastParser:
    def __init__(self) -> None:
        self.state = _HUNT
        self.data = bytearray()
        self.expect = 0

    def feed(self, blob: bytes) -> list[BeastMessage]:
        out: list[BeastMessage] = []
        for byte in blob:
            self._byte(byte, out)
        return out

    def _byte(self, byte: int, out: list[BeastMessage]) -> None:
        if self.state == _HUNT:
            if byte == BEAST_ESC:
                self.state = _TYPE
            return

        if self.state == _TYPE:
            plen = TYPE_LEN.get(byte)
            if plen is not None:
                self.data.clear()
                self.data.append(byte)
                self.expect = 1 + 6 + 1 + plen
                self.state = _DATA
                return
            if byte != BEAST_ESC:
                self.state = _HUNT
            return

        if self.state == _ESC:
            self.state = _DATA
            if byte != BEAST_ESC:
                self.state = _TYPE
                self.data.clear()
                self.expect = 0
                self._byte(byte, out)
                return
            byte = BEAST_ESC
        elif byte == BEAST_ESC:
            self.state = _ESC
            return

        self.data.append(byte)
        if len(self.data) < self.expect:
            return
        out.append(self._finish())

    def _finish(self) -> BeastMessage:
        data = self.data
        msg_type = data[0]
        mlat = bytes(data[1:7])
        signal = data[7]
        payload = bytes(data[8:])
        crc_ok = -1
        if len(payload) == 14 and (payload[0] >> 3) in (17, 18):
            crc_ok = 1 if modes_crc24(payload) == 0 else 0
        self.state = _HUNT
        self.data = bytearray()
        self.expect = 0
        return BeastMessage(msg_type, mlat, signal, payload, crc_ok)

"""Little-endian packed struct. Offsets must match common/adsb_struct.h."""

from __future__ import annotations

import struct
from dataclasses import dataclass

# common/adsb_struct.h
ADSB_STRUCT_MAGIC = 0xAD5B
ADSB_STRUCT_VERSION = 1
ADSB_STRUCT_SIZE = 32
ADSB_STRUCT_CRC_LEN = 30
ADSB_WIRE_MAGIC = bytes((0x5B, 0xAD))

ADSB_FLAG_POSITION = 0x01
ADSB_FLAG_ALTITUDE = 0x02
ADSB_FLAG_VELOCITY = 0x04

ADSB_LATLON_INVALID = -2147483648
ADSB_ALT_INVALID = -2147483648
ADSB_VEL_INVALID = 0xFFFF

STRUCT_FORMAT = "<HBBIiiiHQH"


def crc16_ccitt_false(data: bytes) -> int:
    """CRC-16/CCITT-FALSE. crc16_ccitt_false(b'123456789') == 0x29B1."""
    crc = 0xFFFF
    for byte in data:
        crc ^= byte << 8
        for _ in range(8):
            if crc & 0x8000:
                crc = ((crc << 1) ^ 0x1021) & 0xFFFF
            else:
                crc = (crc << 1) & 0xFFFF
    return crc


@dataclass
class TrackStruct:
    icao: int
    flags: int
    latitude_e7: int
    longitude_e7: int
    altitude_ft: int
    velocity_kt: int
    timestamp_us: int

    @classmethod
    def empty(cls, icao: int, timestamp_us: int = 0) -> TrackStruct:
        return cls(
            icao=icao & 0xFFFFFF,
            flags=0,
            latitude_e7=ADSB_LATLON_INVALID,
            longitude_e7=ADSB_LATLON_INVALID,
            altitude_ft=ADSB_ALT_INVALID,
            velocity_kt=ADSB_VEL_INVALID,
            timestamp_us=timestamp_us,
        )


def pack_message(msg: TrackStruct) -> bytes:
    raw = struct.pack(
        STRUCT_FORMAT,
        ADSB_STRUCT_MAGIC,
        ADSB_STRUCT_VERSION,
        msg.flags & 0xFF,
        msg.icao & 0xFFFFFF,
        msg.latitude_e7,
        msg.longitude_e7,
        msg.altitude_ft,
        msg.velocity_kt & 0xFFFF,
        msg.timestamp_us & 0xFFFFFFFFFFFFFFFF,
        0,
    )
    if len(raw) != ADSB_STRUCT_SIZE:
        raise RuntimeError(f"struct packed to {len(raw)} bytes, expected {ADSB_STRUCT_SIZE}")
    return raw[:ADSB_STRUCT_CRC_LEN] + struct.pack("<H", crc16_ccitt_false(raw[:ADSB_STRUCT_CRC_LEN]))


def unpack_message(frame: bytes) -> TrackStruct | None:
    """Return the struct, or None if the magic, checksum, or version fails."""
    if len(frame) != ADSB_STRUCT_SIZE or frame[:2] != ADSB_WIRE_MAGIC:
        return None
    if crc16_ccitt_false(frame[:ADSB_STRUCT_CRC_LEN]) != struct.unpack_from("<H", frame, 30)[0]:
        return None
    magic, version, flags, icao, lat, lon, alt, vel, ts, _crc = struct.unpack(STRUCT_FORMAT, frame)
    if magic != ADSB_STRUCT_MAGIC or version != ADSB_STRUCT_VERSION:
        return None
    return TrackStruct(icao & 0xFFFFFF, flags, lat, lon, alt, vel, ts)


class StructParser:
    """Hunt for the magic header and drop frames whose checksum does not match."""

    def __init__(self) -> None:
        self._buf = bytearray()
        self._magic = False
        self.checksum_errors = 0
        self.version_errors = 0
        self.frames = 0

    def feed(self, blob: bytes) -> list[TrackStruct]:
        out: list[TrackStruct] = []
        pending = bytearray(blob)
        index = 0
        guard = 0
        limit = max(64, len(blob) * ADSB_STRUCT_SIZE)
        while index < len(pending):
            guard += 1
            if guard > limit:
                raise RuntimeError("struct parser did not advance")
            byte = pending[index]
            if not self._magic:
                if byte == ADSB_WIRE_MAGIC[0]:
                    self._magic = True
                    self._buf = bytearray()
                index += 1
                continue
            if not self._buf:
                if byte != ADSB_WIRE_MAGIC[1]:
                    self._magic = False
                    continue  # reprocess this byte as a possible new magic
                self._buf = bytearray(ADSB_WIRE_MAGIC)
                index += 1
                continue
            self._buf.append(byte)
            index += 1
            if len(self._buf) < ADSB_STRUCT_SIZE:
                continue
            frame = bytes(self._buf)
            self._buf = bytearray()
            self._magic = False
            decoded = unpack_message(frame)
            if decoded is None:
                if frame[2] != ADSB_STRUCT_VERSION and crc16_ccitt_false(frame[:30]) == struct.unpack_from("<H", frame, 30)[0]:
                    self.version_errors += 1
                else:
                    self.checksum_errors += 1
                # Slide one byte and rescan.
                pending = frame[1:] + pending[index:]
                index = 0
                continue
            self.frames += 1
            out.append(decoded)
        return out

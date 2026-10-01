#!/usr/bin/env python3
"""Host test: the Pi sender's bytes for raw Beast and struct mode.

Raw mode must be a byte copy of the Beast stream. Struct records must match
the layout in the README: 32 bytes, little-endian, magic 0xAD5B.
"""

from __future__ import annotations

import struct
import sys
from pathlib import Path

from pyModeS.util import crc

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "pi"))

import adsb_uart_sender  # noqa: E402
import struct_frame  # noqa: E402

EVEN = bytes.fromhex("8D40621D58C382D690C8AC2863A7")
ODD = bytes.fromhex("8D40621D58C386435CC412692AD6")
BEAST_EXAMPLE = bytes.fromhex("1a32083e27b6cb6a1a1a00a1841a1ac3b31d")


def fail(message: str) -> None:
    raise SystemExit(message)


def check_layout() -> None:
    if struct_frame.STRUCT_FORMAT != "<HBBIiiiHQH":
        fail(f"pack format is {struct_frame.STRUCT_FORMAT}, want <HBBIiiiHQH")
    if struct.calcsize(struct_frame.STRUCT_FORMAT) != 32:
        fail("Python struct format is not 32 bytes")
    if struct_frame.ADSB_STRUCT_SIZE != 32 or struct_frame.ADSB_STRUCT_CRC_LEN != 30:
        fail("struct size or checksum span does not match the documented layout")
    if struct_frame.ADSB_STRUCT_MAGIC != 0xAD5B or struct_frame.ADSB_WIRE_MAGIC != b"\x5b\xad":
        fail("magic is not 0xAD5B (wire 5B AD)")
    if struct_frame.ADSB_STRUCT_VERSION != 1:
        fail("struct version is not 1")


def with_parity(data: bytes) -> bytes:
    parity = crc(data.hex() + "000000")
    return data + parity.to_bytes(3, "big")


def beast_encode(msg_type: int, mlat: bytes, signal: int, payload: bytes) -> bytes:
    raw = bytes((msg_type,)) + mlat + bytes((signal & 0xFF,)) + payload
    out = bytearray(b"\x1a")
    for byte in raw:
        out.append(byte)
        if byte == 0x1A:
            out.append(0x1A)
    return bytes(out)


def check_crc() -> None:
    if crc(EVEN.hex()) != 0 or crc(ODD.hex()) != 0:
        fail("published DF17 samples failed the Mode S CRC")
    if struct_frame.crc16_ccitt_false(b"123456789") != 0x29B1:
        fail("CRC-16/CCITT-FALSE check value mismatch")
    if with_parity(EVEN[:11]) != EVEN:
        fail("Mode S parity append did not reproduce the sample")


def check_beast_example() -> None:
    """The published escaped frame is copied, not rewritten."""
    if b"\x1a\x1a" not in BEAST_EXAMPLE:
        fail("test fixture lost its escaped 0x1A")
    if adsb_uart_sender.forward_raw(BEAST_EXAMPLE) != BEAST_EXAMPLE:
        fail("raw mode changed the escaped Beast example")
    again = beast_encode(0x32, bytes.fromhex("083e27b6cb6a"), 0x1A, bytes.fromhex("00a1841ac3b31d"))
    if again != BEAST_EXAMPLE:
        fail("Beast escape helper does not rebuild the published frame")


def check_raw_copy() -> None:
    samples = [
        (0x31, bytes.fromhex("010203040506"), 0x00, bytes.fromhex("00ab")),
        (0x32, bytes.fromhex("1a1a1a1a1a1a"), 0x1A, bytes.fromhex("0011223344551a")),
        (0x33, bytes.fromhex("000000000000"), 0xFF, EVEN),
        (0x33, bytes.fromhex("ffffffffffff"), 0x10, ODD),
    ]
    stream = bytearray(b"\x00\xff")
    for msg_type, mlat, signal, payload in samples:
        stream += beast_encode(msg_type, mlat, signal, payload)
    blob = bytes(stream)
    if adsb_uart_sender.forward_raw(blob) != blob:
        fail("raw mode changed Beast bytes")
    escaped = beast_encode(0x33, b"\x00\x1a\x00\x00\x00\x00", 0x00, EVEN)
    if escaped.count(b"\x1a") < 2 or adsb_uart_sender.forward_raw(escaped) != escaped:
        fail("raw mode dropped an escaped 0x1A in the MLAT timestamp")


def velocity_squitter() -> bytes:
    # TC 19 subtype 1, 300 kt east and 400 kt north. Speed is 500 kt.
    me = bytes((0x99, 0x01, 0x2D, 0x32, 0x20, 0x00, 0x00))
    return with_parity(bytes((0x8D, 0xAB, 0xC1, 0x23)) + me)


def gnss_squitter() -> bytes:
    # TC 20, GNSS height 1000 m. pyModeS converts with int(meters * 3.28084).
    me = bytes((0xA0, 0x3E, 0x80, 0x00, 0x00, 0x00, 0x00))
    return with_parity(bytes((0x8D, 0x00, 0x00, 0x01)) + me)


def assert_packed(frame: bytes, msg: struct_frame.TrackStruct) -> None:
    """Each documented field is at its offset, and the checksum covers bytes 0..29."""
    if len(frame) != 32:
        fail(f"struct record is {len(frame)} bytes, want 32")
    if frame[0:2] != b"\x5b\xad" or frame[2] != 1:
        fail(f"magic/version bytes are {frame[:3].hex()}, want 5bad 01")
    if frame[3] != (msg.flags & 0xFF):
        fail("flags byte does not match the record")
    if struct.unpack_from("<I", frame, 4)[0] != (msg.icao & 0xFFFFFF):
        fail("icao is not the little-endian word at offset 4")
    if struct.unpack_from("<i", frame, 8)[0] != msg.latitude_e7:
        fail("latitude is not the little-endian int32 at offset 8")
    if struct.unpack_from("<i", frame, 12)[0] != msg.longitude_e7:
        fail("longitude is not the little-endian int32 at offset 12")
    if struct.unpack_from("<i", frame, 16)[0] != msg.altitude_ft:
        fail("altitude is not the little-endian int32 at offset 16")
    if struct.unpack_from("<H", frame, 20)[0] != (msg.velocity_kt & 0xFFFF):
        fail("velocity is not the little-endian uint16 at offset 20")
    if struct.unpack_from("<Q", frame, 22)[0] != (msg.timestamp_us & 0xFFFFFFFFFFFFFFFF):
        fail("timestamp is not the little-endian uint64 at offset 22")
    expect_crc = struct_frame.crc16_ccitt_false(frame[:30])
    if struct.unpack_from("<H", frame, 30)[0] != expect_crc:
        fail("checksum is not CRC-16/CCITT-FALSE over bytes 0..29")
    back = struct_frame.unpack_message(frame)
    if back != msg:
        fail(f"unpack did not recover the packed record: {back} vs {msg}")


def _feed(payloads: list[bytes], times: list[tuple[float, int]]) -> list[struct_frame.TrackStruct]:
    wire = b""
    for payload in payloads:
        wire += beast_encode(0x33, b"\x00" * 6, 0x00, payload)
    pending = list(times)

    def clock():
        return pending.pop(0)

    forwarder = adsb_uart_sender.StructForwarder()
    frames = forwarder.feed(wire, clock)
    if len(frames) != len(payloads):
        fail(f"struct forwarder emitted {len(frames)} frames, want {len(payloads)}")
    out = []
    for frame in frames:
        decoded = struct_frame.unpack_message(frame)
        if decoded is None:
            fail("sender struct did not unpack")
        assert_packed(frame, decoded)
        out.append(decoded)
    return out


def check_decode_and_struct() -> None:
    if crc(velocity_squitter().hex()) != 0 or crc(gnss_squitter().hex()) != 0:
        fail("synthetic squitter CRC is not zero")

    odd_wire = beast_encode(0x33, b"\x11\x22\x33\x44\x55\x66", 0x1A, ODD)
    even_wire = beast_encode(0x33, b"\x00" * 6, 0x00, EVEN)
    if odd_wire.count(b"\x1a") < 2:
        fail("position fixture was not escaped")
    times = [(1000.0, 1000_000_000), (1005.0, 1005_000_000)]

    def clock():
        return times.pop(0)

    forwarder = adsb_uart_sender.StructForwarder()
    # Split the first frame so a TCP chunk boundary still reassembles.
    head = bytes([0x99, 0x1A, 0x00]) + odd_wire[:5]
    tail = odd_wire[5:] + even_wire
    if forwarder.feed(head, clock):
        fail("an incomplete Beast frame was emitted")
    frames = forwarder.feed(tail, clock)
    if len(frames) != 2:
        fail(f"struct forwarder emitted {len(frames)} frames, want 2")
    if adsb_uart_sender.forward_raw(odd_wire + even_wire) != odd_wire + even_wire:
        fail("raw forward changed a decoded-path fixture")

    first = struct_frame.unpack_message(frames[0])
    second = struct_frame.unpack_message(frames[1])
    if first is None or second is None:
        fail("sender struct did not unpack")
    assert_packed(frames[0], first)
    assert_packed(frames[1], second)
    if first.flags != struct_frame.ADSB_FLAG_ALTITUDE or first.altitude_ft != 38000:
        fail(f"odd frame should carry 38000 ft and no position yet: {first}")
    if (second.flags & struct_frame.ADSB_FLAG_POSITION) == 0:
        fail(f"even/odd pair did not decode a position: {second}")
    lat = second.latitude_e7 / 1e7
    lon = second.longitude_e7 / 1e7
    if abs(lat - 52.2572021484375) > 1e-6 or abs(lon - 3.91937255859375) > 1e-6:
        fail(f"CPR position {lat}, {lon} is not the published example")
    if second.latitude_e7 != 522572021 or second.longitude_e7 != 39193726:
        fail(f"CPR e7 fields are {second.latitude_e7}, {second.longitude_e7}")
    if second.altitude_ft != 38000 or second.icao != 0x40621D or second.timestamp_us != 1005_000_000:
        fail(f"position struct fields wrong: {second}")

    late = _feed([ODD, EVEN], [(1000.0, 1), (1011.0, 2)])[1]
    if (late.flags & struct_frame.ADSB_FLAG_POSITION) != 0 or late.altitude_ft != 38000:
        fail("CPR pair older than 10 s was accepted")

    speed = _feed([velocity_squitter()], [(1.0, 50)])[0]
    if speed.velocity_kt != 500 or speed.flags != struct_frame.ADSB_FLAG_VELOCITY:
        fail(f"velocity decode failed: {speed}")
    if speed.icao != 0xABC123:
        fail(f"velocity ICAO wrong: {speed.icao:#x}")

    height = _feed([gnss_squitter()], [(1.0, 60)])[0]
    if height.altitude_ft != 3280 or height.flags != struct_frame.ADSB_FLAG_ALTITUDE:
        fail(f"GNSS height decode failed: {height}")

    bad = bytearray(EVEN)
    bad[-1] ^= 0xFF
    dropped = adsb_uart_sender.StructForwarder()
    emitted = dropped.feed(beast_encode(0x33, b"\x00" * 6, 0, bytes(bad)))
    if emitted or dropped.crc_drops != 1:
        fail("bad Mode S CRC was forwarded as a struct")


def check_struct_bytes() -> None:
    samples = [
        struct_frame.TrackStruct(0x40621D, 0x07, 522572021, 39193726, 38000, 450, 1005_000_000),
        struct_frame.TrackStruct.empty(0x1A1A1A, 0x1A),
        struct_frame.TrackStruct(
            0xABCDEF,
            0x04,
            struct_frame.ADSB_LATLON_INVALID,
            struct_frame.ADSB_LATLON_INVALID,
            struct_frame.ADSB_ALT_INVALID,
            500,
            2**40 + 26,
        ),
        struct_frame.TrackStruct(
            1, 0x02, -338687000, -706690000, -1000, struct_frame.ADSB_VEL_INVALID, 0
        ),
    ]
    for msg in samples:
        assert_packed(struct_frame.pack_message(msg), msg)

    good = struct_frame.pack_message(samples[0])
    bad = bytearray(good)
    bad[-1] ^= 0x5A
    if struct_frame.unpack_message(bytes(bad)) is not None:
        fail("checksum mismatch was accepted")
    back = struct_frame.unpack_message(good)
    if back is None or back.icao != 0x40621D:
        fail(f"good record was rejected: {back}")


def main() -> int:
    check_layout()
    check_crc()
    check_beast_example()
    check_raw_copy()
    check_decode_and_struct()
    check_struct_bytes()
    print("host test passed")
    print("raw: forward_raw() is a byte copy of the Beast stream, including 0x1A escaping")
    print("struct: packed records match the documented 32-byte layout")
    print("decode: published CPR example is 52.257202N 3.919373E at 38000 ft; velocity 500 kt")
    return 0


if __name__ == "__main__":
    sys.exit(main())

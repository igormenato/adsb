#!/usr/bin/env python3
"""Host test: the Pi sender's bytes for raw Beast and struct mode.

Raw mode must be a byte copy of the Beast stream. Struct records must match
the layout in the README: 32 bytes, little-endian, magic 0xAD5B.
"""

from __future__ import annotations

import struct
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "pi"))

import adsb_decode  # noqa: E402
import adsb_uart_sender  # noqa: E402
import beast  # noqa: E402
import struct_frame  # noqa: E402

EVEN = bytes.fromhex("8D40621D58C382D690C8AC2863A7")
ODD = bytes.fromhex("8D40621D58C386435CC412692AD6")
BEAST_EXAMPLE = bytes.fromhex("1a32083e27b6cb6a1a1a00a1841a1ac3b31d")

def fail(message: str) -> None:
    raise SystemExit(message)


def check_layout() -> None:
    """Packed bytes sit at the offsets documented in the README."""
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


def check_crc() -> None:
    if beast.modes_crc24(EVEN) != 0 or beast.modes_crc24(ODD) != 0:
        fail("published DF17 samples failed the Mode S CRC")
    if struct_frame.crc16_ccitt_false(b"123456789") != 0x29B1:
        fail("CRC-16/CCITT-FALSE check value mismatch")
    if append_modes_parity(EVEN[:11]) != EVEN:
        fail("Mode S parity append did not reproduce the sample")


def check_beast_example() -> None:
    """The published escaped frame round-trips, and raw mode copies it."""
    parsed = beast.BeastParser().feed(BEAST_EXAMPLE)
    if len(parsed) != 1 or parsed[0].type != 0x32:
        fail("Python beast parser missed the escaped example")
    if parsed[0].mlat.hex() != "083e27b6cb6a" or parsed[0].signal != 0x1A:
        fail(f"escaped example fields wrong: {parsed[0]}")
    if parsed[0].payload.hex() != "00a1841ac3b31d":
        fail(f"escaped example payload wrong: {parsed[0].payload.hex()}")
    again = beast.encode(parsed[0].type, parsed[0].mlat, parsed[0].signal, parsed[0].payload)
    if again != BEAST_EXAMPLE:
        fail("Python re-encode changed the escaped Beast frame")
    if adsb_uart_sender.forward_raw(BEAST_EXAMPLE) != BEAST_EXAMPLE:
        fail("raw mode changed the escaped Beast example")


def check_raw_copy() -> None:
    samples = [
        (0x31, bytes.fromhex("010203040506"), 0x00, bytes.fromhex("00ab")),
        (0x32, bytes.fromhex("1a1a1a1a1a1a"), 0x1A, bytes.fromhex("0011223344551a")),
        (0x33, bytes.fromhex("000000000000"), 0xFF, EVEN),
        (0x33, bytes.fromhex("ffffffffffff"), 0x10, ODD),
    ]
    wire = bytearray()
    for msg_type, mlat, signal, payload in samples:
        encoded = beast.encode(msg_type, mlat, signal, payload)
        wire += encoded

    stream = bytes([0x00, 0xFF, 0x1A, 0x00]) + bytes(wire)
    python_msgs = []
    parser = beast.BeastParser()
    for index in range(len(stream)):
        python_msgs.extend(parser.feed(stream[index : index + 1]))
    if len(python_msgs) != len(samples):
        fail(f"Beast parser count {len(python_msgs)}, want {len(samples)}")
    for index, (py_msg, sample) in enumerate(zip(python_msgs, samples)):
        msg_type, mlat, signal, payload = sample
        if py_msg.type != msg_type or py_msg.payload != payload or py_msg.mlat != mlat:
            fail(f"Beast message {index} disagreed")
        if py_msg.signal != signal:
            fail(f"Beast signal disagreed on message {index}")

    if adsb_uart_sender.forward_raw(stream) != stream:
        fail("raw mode changed Beast bytes")
    if b"\x1a\x1a" not in BEAST_EXAMPLE:
        fail("test fixture lost its escaped 0x1A")
    # A 0x1A in the MLAT field must survive as an escaped pair, then as a copy.
    escaped = beast.encode(0x33, b"\x00\x1a\x00\x00\x00\x00", 0x00, EVEN)
    if escaped.count(b"\x1a") < 2 or adsb_uart_sender.forward_raw(escaped) != escaped:
        fail("raw mode dropped an escaped 0x1A in the MLAT timestamp")


def append_modes_parity(data: bytes) -> bytes:
    parity = beast.modes_crc24(data)
    return data + bytes(((parity >> 16) & 0xFF, (parity >> 8) & 0xFF, parity & 0xFF))


def velocity_squitter() -> bytes:
    # TC 19 subtype 1, 300 kt east and 400 kt north. Speed is 500 kt.
    # V_ew = 301, V_ns = 401. See pi/adsb_decode.py for the bit positions.
    me = bytes((0x99, 0x01, 0x2D, 0x32, 0x20, 0x00, 0x00))
    body = bytes((0x8D, 0xAB, 0xC1, 0x23)) + me
    return append_modes_parity(body)


def gnss_squitter() -> bytes:
    # TC 20, GNSS height 1000 m. 1000 = 0x3E8 across the 12-bit altitude field.
    me = bytes((0xA0, 0x3E, 0x80, 0x00, 0x00, 0x00, 0x00))
    body = bytes((0x8D, 0x00, 0x00, 0x01)) + me
    return append_modes_parity(body)


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


def check_decode_and_struct() -> None:
    if beast.modes_crc24(velocity_squitter()) != 0 or beast.modes_crc24(gnss_squitter()) != 0:
        fail("synthetic squitter CRC is not zero")

    cache = adsb_decode.CprCache()
    odd_msg = beast.BeastMessage(0x33, b"\x00" * 6, 0xFF, ODD, 1)
    even_msg = beast.BeastMessage(0x33, b"\x00" * 6, 0xFF, EVEN, 1)
    first = adsb_decode.decode_adsb(odd_msg, cache, 1000.0, 1000_000_000)
    if first is None or first.flags != struct_frame.ADSB_FLAG_ALTITUDE or first.altitude_ft != 38000:
        fail(f"odd frame should carry 38000 ft and no position yet: {first}")
    second = adsb_decode.decode_adsb(even_msg, cache, 1005.0, 1005_000_000)
    if second is None or (second.flags & struct_frame.ADSB_FLAG_POSITION) == 0:
        fail(f"even/odd pair did not decode a position: {second}")
    lat = second.latitude_e7 / 1e7
    lon = second.longitude_e7 / 1e7
    if abs(lat - 52.2572021484375) > 1e-6 or abs(lon - 3.91937255859375) > 1e-6:
        fail(f"CPR position {lat}, {lon} is not the published example")
    if second.altitude_ft != 38000 or second.icao != 0x40621D:
        fail(f"position struct fields wrong: {second}")

    stale = adsb_decode.CprCache()
    adsb_decode.decode_adsb(odd_msg, stale, 1000.0, 1)
    late = adsb_decode.decode_adsb(even_msg, stale, 1011.0, 2)
    if late is None or (late.flags & struct_frame.ADSB_FLAG_POSITION) != 0:
        fail("CPR pair older than 10 s was accepted")

    speed = adsb_decode.decode_adsb(
        beast.BeastMessage(0x33, b"\x00" * 6, 1, velocity_squitter(), 1),
        adsb_decode.CprCache(),
        1.0,
        50,
    )
    if speed is None or speed.velocity_kt != 500 or speed.flags != struct_frame.ADSB_FLAG_VELOCITY:
        fail(f"velocity decode failed: {speed}")
    if speed.icao != 0xABC123:
        fail(f"velocity ICAO wrong: {speed.icao:#x}")

    height = adsb_decode.decode_adsb(
        beast.BeastMessage(0x33, b"\x00" * 6, 1, gnss_squitter(), 1),
        adsb_decode.CprCache(),
        1.0,
        60,
    )
    if height is None or height.altitude_ft != 3281 or height.flags != struct_frame.ADSB_FLAG_ALTITUDE:
        fail(f"GNSS height decode failed: {height}")

    forwarder = adsb_uart_sender.StructForwarder()
    times = [(1000.0, 1000_000_000), (1005.0, 1005_000_000)]

    def clock():
        return times.pop(0)

    odd_wire = beast.encode(0x33, b"\x11\x22\x33\x44\x55\x66", 0x1A, ODD)
    even_wire = beast.encode(0x33, b"\x00" * 6, 0x00, EVEN)
    if odd_wire.count(b"\x1a") < 2:
        fail("position fixture was not escaped")
    frames = forwarder.feed(bytes([0x99, 0x1A, 0x00]) + odd_wire + even_wire, clock)
    if len(frames) != 2:
        fail(f"struct forwarder emitted {len(frames)} frames, want 2")
    if adsb_uart_sender.forward_raw(odd_wire + even_wire) != odd_wire + even_wire:
        fail("raw forward changed a decoded-path fixture")

    decoded = struct_frame.unpack_message(frames[1])
    if decoded is None or decoded.timestamp_us != 1005_000_000 or decoded.altitude_ft != 38000:
        fail(f"packed struct did not round-trip: {decoded}")
    assert_packed(frames[1], decoded)
    if decoded.latitude_e7 != second.latitude_e7 or decoded.icao != 0x40621D:
        fail("sender struct bytes do not carry the decoded position")

    bad = bytearray(EVEN)
    bad[-1] ^= 0xFF
    dropped = adsb_uart_sender.StructForwarder()
    emitted = dropped.feed(beast.encode(0x33, b"\x00" * 6, 0, bytes(bad)))
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

#!/usr/bin/env python3
"""Host test: Beast and struct framers agree, without a Pi or an ESP32.

Compiles test/frame_tool.c against common/ and checks it against the Python
sender. Raw mode must be a byte copy. Struct mode must match common/adsb_struct.h.
"""

from __future__ import annotations

import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "pi"))

import adsb_decode  # noqa: E402
import adsb_uart_receiver  # noqa: E402
import adsb_uart_sender  # noqa: E402
import beast  # noqa: E402
import struct_frame  # noqa: E402

EVEN = bytes.fromhex("8D40621D58C382D690C8AC2863A7")
ODD = bytes.fromhex("8D40621D58C386435CC412692AD6")
BEAST_EXAMPLE = bytes.fromhex("1a32083e27b6cb6a1a1a00a1841a1ac3b31d")

SOURCES = [
    ROOT / "common" / "crc16.c",
    ROOT / "common" / "modes_crc.c",
    ROOT / "common" / "beast_frame.c",
    ROOT / "common" / "struct_frame.c",
    ROOT / "common" / "link_parser.c",
    ROOT / "test" / "frame_tool.c",
]


def fail(message: str) -> None:
    raise SystemExit(message)


def compile_tool(path: Path) -> None:
    cmd = [
        "gcc",
        "-std=c11",
        "-Wall",
        "-Wextra",
        "-Werror",
        "-I",
        str(ROOT / "common"),
        "-o",
        str(path),
        *[str(source) for source in SOURCES],
    ]
    proc = subprocess.run(cmd, capture_output=True, text=True)
    if proc.returncode != 0:
        fail(proc.stderr or proc.stdout or "gcc failed")


def run(tool: Path, *args: str, stdin: str = "") -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        [str(tool), *args],
        input=stdin,
        text=True,
        capture_output=True,
        check=False,
    )


def parse_kv(text: str) -> dict[str, str]:
    out: dict[str, str] = {}
    for part in text.split():
        key, value = part.split("=", 1)
        out[key] = value
    return out


def parse_link(stdout: str) -> tuple[list[dict[str, str]], list[dict[str, str]], list[str], dict[str, str]]:
    beasts: list[dict[str, str]] = []
    structs: list[dict[str, str]] = []
    errors: list[str] = []
    stats: dict[str, str] = {}
    for line in stdout.splitlines():
        if line.startswith("B "):
            beasts.append(parse_kv(line[2:]))
        elif line.startswith("S "):
            structs.append(parse_kv(line[2:]))
        elif line.startswith("E "):
            errors.append(line[2:].strip())
        elif line.startswith("STATS "):
            stats = parse_kv(line[6:])
    return beasts, structs, errors, stats


def check_layout(tool: Path) -> None:
    proc = run(tool, "layout")
    if proc.returncode != 0:
        fail(proc.stderr)
    got = dict(line.split() for line in proc.stdout.splitlines())
    expect = {
        "size": "32",
        "magic": "0",
        "version": "2",
        "flags": "3",
        "icao": "4",
        "latitude_e7": "8",
        "longitude_e7": "12",
        "altitude_ft": "16",
        "velocity_kt": "20",
        "timestamp_us": "22",
        "checksum": "30",
        "magic_value": f"{struct_frame.ADSB_STRUCT_MAGIC:04x}",
        "version_value": str(struct_frame.ADSB_STRUCT_VERSION),
        "wire0": "5b",
        "wire1": "ad",
    }
    if got != expect:
        fail(f"C layout does not match Python/header constants:\n{got}\n{expect}")
    if struct_frame.struct.calcsize(struct_frame.STRUCT_FORMAT) != 32:
        fail("Python struct format is not 32 bytes")


def check_crc() -> None:
    if beast.modes_crc24(EVEN) != 0 or beast.modes_crc24(ODD) != 0:
        fail("published DF17 samples failed the Mode S CRC")
    if struct_frame.crc16_ccitt_false(b"123456789") != 0x29B1:
        fail("CRC-16/CCITT-FALSE check value mismatch")
    if beast.append_modes_parity(EVEN[:11]) != EVEN:
        fail("Mode S parity append did not reproduce the sample")


def check_beast_example(tool: Path) -> None:
    """The published escaped frame must round-trip in both languages."""
    proc = run(tool, "beast-decode", stdin=BEAST_EXAMPLE.hex())
    if proc.returncode != 0:
        fail(proc.stderr)
    beasts, _, _, _ = parse_link(proc.stdout)
    if len(beasts) != 1:
        fail(f"C beast decode of the escaped example returned {beasts}")
    if beasts[0]["type"] != "2" or beasts[0]["signal"] != "1a":
        fail(f"escaped example fields wrong: {beasts[0]}")
    if beasts[0]["payload"] != "00a1841ac3b31d" or beasts[0]["mlat"] != "083e27b6cb6a":
        fail(f"escaped example payload wrong: {beasts[0]}")

    parsed = beast.BeastParser().feed(BEAST_EXAMPLE)
    if len(parsed) != 1 or parsed[0].signal != 0x1A or parsed[0].payload.hex() != "00a1841ac3b31d":
        fail("Python beast parser disagreed with the escaped example")
    again = beast.encode(parsed[0].type, parsed[0].mlat, parsed[0].signal, parsed[0].payload)
    if again != BEAST_EXAMPLE:
        fail("Python re-encode changed the escaped Beast frame")

    c_hex = run(
        tool,
        "beast-encode",
        "2",
        parsed[0].mlat.hex(),
        f"{parsed[0].signal:02x}",
        parsed[0].payload.hex(),
    )
    if c_hex.returncode != 0 or bytes.fromhex(c_hex.stdout.strip()) != BEAST_EXAMPLE:
        fail(f"C beast encoder disagreed:\n{c_hex.stdout}\n{c_hex.stderr}")


def check_beast_agreement(tool: Path) -> None:
    samples = [
        (0x31, bytes.fromhex("010203040506"), 0x00, bytes.fromhex("00ab")),
        (0x32, bytes.fromhex("1a1a1a1a1a1a"), 0x1A, bytes.fromhex("0011223344551a")),
        (0x33, bytes.fromhex("000000000000"), 0xFF, EVEN),
        (0x33, bytes.fromhex("ffffffffffff"), 0x10, ODD),
    ]
    wire = bytearray()
    for msg_type, mlat, signal, payload in samples:
        encoded = beast.encode(msg_type, mlat, signal, payload)
        c_hex = run(tool, "beast-encode", chr(msg_type), mlat.hex(), f"{signal:02x}", payload.hex())
        if c_hex.returncode != 0:
            fail(c_hex.stderr)
        if bytes.fromhex(c_hex.stdout.strip()) != encoded:
            fail(f"Beast encoders disagree for type {chr(msg_type)}")
        wire += encoded

    # Garbage, a false start, then the real frames, split one byte at a time.
    stream = bytes([0x00, 0xFF, 0x1A, 0x00]) + bytes(wire)
    python_msgs = []
    parser = beast.BeastParser()
    for index in range(len(stream)):
        python_msgs.extend(parser.feed(stream[index : index + 1]))
    c_msgs, _, _, _ = parse_link(run(tool, "beast-decode", stdin=stream.hex()).stdout)
    if len(python_msgs) != len(c_msgs):
        fail(f"Beast parser count Python={len(python_msgs)} C={len(c_msgs)}")
    for index, (py_msg, c_msg) in enumerate(zip(python_msgs, c_msgs)):
        if c_msg["type"] != chr(py_msg.type) or c_msg["payload"] != py_msg.payload.hex():
            fail(f"Beast message {index} disagreed: {py_msg} vs {c_msg}")
        if int(c_msg["crc"]) != py_msg.crc_ok:
            fail(f"CRC flag disagreed on message {index}")

    if adsb_uart_sender.forward_raw(stream) != stream:
        fail("raw mode changed Beast bytes")
    if b"\x1a\x1a" not in BEAST_EXAMPLE:
        fail("test fixture lost its escaped 0x1A")


def velocity_squitter() -> bytes:
    # TC 19 subtype 1, 300 kt east and 400 kt north. Speed is 500 kt.
    # V_ew = 301, V_ns = 401. See pi/adsb_decode.py for the bit positions.
    me = bytes((0x99, 0x01, 0x2D, 0x32, 0x20, 0x00, 0x00))
    body = bytes((0x8D, 0xAB, 0xC1, 0x23)) + me
    return beast.append_modes_parity(body)


def gnss_squitter() -> bytes:
    # TC 20, GNSS height 1000 m. 1000 = 0x3E8 across the 12-bit altitude field.
    me = bytes((0xA0, 0x3E, 0x80, 0x00, 0x00, 0x00, 0x00))
    body = bytes((0x8D, 0x00, 0x00, 0x01)) + me
    return beast.append_modes_parity(body)


def check_decode_and_struct(tool: Path) -> None:
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
    # A byte inside the MLAT field is 0x1A, so the wire form is escaped.
    if odd_wire.count(b"\x1a") < 2:
        fail("position fixture was not escaped")
    frames = forwarder.feed(bytes([0x99, 0x1A, 0x00]) + odd_wire + even_wire, clock)
    if len(frames) != 2:
        fail(f"struct forwarder emitted {len(frames)} frames, want 2")
    if adsb_uart_sender.forward_raw(odd_wire + even_wire) != odd_wire + even_wire:
        fail("raw forward changed a decoded-path fixture")

    decoded = struct_frame.unpack_message(frames[1])
    if decoded is None or decoded.timestamp_us != 1005_000_000 or decoded.altitude_ft != 38000:
        fail(f"packed struct did not round-trip in Python: {decoded}")
    c_dec = run(tool, "struct-decode", stdin=frames[1].hex())
    if c_dec.returncode != 0:
        fail(c_dec.stderr)
    got = parse_kv(c_dec.stdout.strip()[2:])
    if int(got["lat"]) != decoded.latitude_e7 or int(got["alt"]) != 38000 or int(got["icao"], 16) != 0x40621D:
        fail(f"C struct decoder disagreed with Python pack: {got}")

    bad = bytearray(EVEN)
    bad[-1] ^= 0xFF
    dropped = adsb_uart_sender.StructForwarder()
    emitted = dropped.feed(beast.encode(0x33, b"\x00" * 6, 0, bytes(bad)))
    if emitted or dropped.crc_drops != 1:
        fail("bad Mode S CRC was forwarded as a struct")


def check_struct_agreement(tool: Path) -> None:
    samples = [
        struct_frame.TrackStruct(0x40621D, 0x07, 522572021, 39193726, 38000, 450, 1005_000_000),
        struct_frame.TrackStruct.empty(0x1A1A1A, 0x1A),
        struct_frame.TrackStruct(0xABCDEF, 0x04, struct_frame.ADSB_LATLON_INVALID,
                                  struct_frame.ADSB_LATLON_INVALID, struct_frame.ADSB_ALT_INVALID,
                                  500, 2**40 + 26),
        struct_frame.TrackStruct(1, 0x02, -338687000, -706690000, -1000, struct_frame.ADSB_VEL_INVALID, 0),
    ]
    blob = bytearray(b"\x00\xff")
    for msg in samples:
        packed = struct_frame.pack_message(msg)
        c_hex = run(
            tool,
            "struct-encode",
            str(msg.flags),
            str(msg.icao),
            str(msg.latitude_e7),
            str(msg.longitude_e7),
            str(msg.altitude_ft),
            str(msg.velocity_kt),
            str(msg.timestamp_us),
        )
        if c_hex.returncode != 0:
            fail(c_hex.stderr)
        if bytes.fromhex(c_hex.stdout.strip()) != packed:
            fail(f"struct encoders disagree for icao {msg.icao:#x}")
        back = struct_frame.unpack_message(bytes.fromhex(c_hex.stdout.strip()))
        if back != msg and not _same(back, msg):
            fail(f"Python unpack of C struct failed: {back} vs {msg}")
        blob += packed

    # Corrupt one checksum, then a good frame. Both parsers must keep the good one.
    good = struct_frame.pack_message(samples[0])
    bad = bytearray(good)
    bad[-1] ^= 0x5A
    stream = bytes(bad) + good
    py = struct_frame.StructParser()
    got = []
    for index in range(len(stream)):
        got.extend(py.feed(stream[index : index + 1]))
    if py.checksum_errors < 1 or len(got) != 1 or got[0].icao != 0x40621D:
        fail(f"Python struct resync failed: errors={py.checksum_errors} frames={got}")

    beasts, structs, errors, stats = parse_link(run(tool, "link-decode", stdin=(bytes(blob) + stream).hex()).stdout)
    if beasts:
        fail(f"struct stream was parsed as Beast: {beasts}")
    icaos = [int(item["icao"], 16) for item in structs]
    if icaos[:4] != [msg.icao for msg in samples] or icaos[-1] != 0x40621D:
        fail(f"ESP32 link parser lost struct frames: {icaos}")
    if "struct_checksum" not in errors or int(stats["struct_checksum"]) < 1:
        fail(f"ESP32 link parser did not report a checksum error: {errors} {stats}")

    mixed = beast.encode(0x33, b"\x00" * 6, 0x20, EVEN) + good + beast.encode(0x32, b"\x01" * 6, 0x02, b"\x00" * 7)
    mixed_beasts, mixed_structs, _, _ = parse_link(run(tool, "link-decode", stdin=mixed.hex()).stdout)
    if [item["type"] for item in mixed_beasts] != ["3", "2"] or len(mixed_structs) != 1:
        fail(f"mixed stream not split into beast/struct: {mixed_beasts} {mixed_structs}")
    if mixed_beasts[0]["payload"] != EVEN.hex() or int(mixed_beasts[0]["crc"]) != 1:
        fail("mixed stream changed the Beast payload")


def check_pi_receiver(tool: Path) -> None:
    """The Pi receiver parses sender bytes with no UART."""
    raw_stream = bytes([0x00, 0xFF, 0x1A, 0x00]) + BEAST_EXAMPLE
    raw_stream += beast.encode(0x33, b"\x00" * 6, 0x20, EVEN)
    if adsb_uart_sender.forward_raw(raw_stream) != raw_stream:
        fail("raw sender changed bytes before the Pi receiver")

    good = struct_frame.pack_message(
        struct_frame.TrackStruct(0x40621D, 0x07, 522572021, 39193726, 38000, 450, 1005_000_000)
    )
    bad = bytearray(good)
    bad[-1] ^= 0x5A
    struct_stream = bytes(bad) + good

    forwarder = adsb_uart_sender.StructForwarder()
    times = [(1000.0, 1000_000_000), (1005.0, 1005_000_000)]

    def clock():
        return times.pop(0)

    odd_wire = beast.encode(0x33, b"\x00" * 6, 0x10, ODD)
    even_wire = beast.encode(0x33, b"\x00" * 6, 0x11, EVEN)
    packed = b"".join(forwarder.feed(odd_wire + even_wire, clock))

    stream = raw_stream + struct_stream + packed
    rx = adsb_uart_receiver.BenchReceiver()
    lines: list[str] = []
    for index in range(len(stream)):
        lines.extend(rx.feed(stream[index : index + 1]))

    proc = run(tool, "link-decode", stdin=stream.hex())
    if proc.returncode != 0:
        fail(proc.stderr)
    c_beasts, c_structs, c_errors, c_stats = parse_link(proc.stdout)
    beast_lines = [line for line in lines if line.startswith("beast ")]
    struct_lines = [line for line in lines if line.startswith("struct ")]
    if len(beast_lines) != len(c_beasts) or len(struct_lines) != len(c_structs):
        fail(
            f"Pi receiver lines beast={len(beast_lines)} struct={len(struct_lines)} "
            f"C beast={len(c_beasts)} struct={len(c_structs)}\n{lines}"
        )
    if rx.stats.beast_ok != int(c_stats["beast_ok"]) or rx.stats.struct_ok != int(c_stats["struct_ok"]):
        fail(f"Pi receiver counts {rx.stats} vs C {c_stats}")
    if rx.stats.struct_checksum_errors != int(c_stats["struct_checksum"]):
        fail(f"checksum errors differ: {rx.stats} vs {c_stats}")
    if "struct_checksum" not in c_errors or not any(line.startswith("err struct_checksum") for line in lines):
        fail(f"Pi receiver did not log a struct checksum error: {lines}")

    if "type=2" not in beast_lines[0] or "crc=-1" not in beast_lines[0]:
        fail(f"escaped Beast example was not logged: {beast_lines[0]}")
    if "type=3" not in beast_lines[1] or "icao=40621D" not in beast_lines[1] or "crc=1" not in beast_lines[1]:
        fail(f"DF17 Beast frame was not logged: {beast_lines[1]}")
    if c_beasts[1]["payload"] != EVEN.hex():
        fail("C parser did not recover the raw Beast payload")

    packed_line = next(line for line in struct_lines if "flags=0x07" in line)
    if "icao=40621D" not in packed_line or "vel=450kt" not in packed_line or "t=1005000000" not in packed_line:
        fail(f"packed struct line wrong: {packed_line}")
    if "lat=52.2572021" not in packed_line or "lon=3.9193726" not in packed_line or "alt=38000ft" not in packed_line:
        fail(f"packed struct position wrong: {packed_line}")
    if "lat=- lon=-" not in struct_lines[-2] or "alt=38000ft" not in struct_lines[-2]:
        fail(f"odd squitter should be altitude only: {struct_lines[-2]}")
    position = struct_lines[-1]
    if "flags=0x03" not in position or "lat=52.2572021" not in position or "vel=-" not in position:
        fail(f"even squitter should carry position without velocity: {position}")

    beat = rx.beat_line()
    if f"beast={rx.stats.beast_ok}" not in beat or f"struct={rx.stats.struct_ok}" not in beat:
        fail(f"beat line missing counts: {beat}")


def _same(left: struct_frame.TrackStruct | None, right: struct_frame.TrackStruct) -> bool:
    return left is not None and left == right


def main() -> int:
    with tempfile.TemporaryDirectory() as tmp:
        tool = Path(tmp) / "frame_tool"
        compile_tool(tool)
        check = run(tool, "self-check")
        if check.returncode != 0:
            fail(check.stderr or check.stdout)
        check_layout(tool)
        check_crc()
        check_beast_example(tool)
        check_beast_agreement(tool)
        check_decode_and_struct(tool)
        check_struct_agreement(tool)
        check_pi_receiver(tool)
    print("host test passed")
    print("beast: python and C encoders match, including 0x1A escaping; both parsers resync")
    print("struct: python pack matches common/adsb_struct.h and the C decoder")
    print("raw: forward_raw() is a byte copy of the Beast stream")
    print("pi receiver: same bytes, same frames and error counts as the C link parser")
    print("decode: published CPR example is 52.257202N 3.919373E at 38000 ft; velocity 500 kt")
    return 0


if __name__ == "__main__":
    sys.exit(main())

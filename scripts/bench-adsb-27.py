#!/usr/bin/env python3
"""Decoder-vs-decoder bench: trck vs dump1090-fa vs readsb. Same IQ bytes after 2.4 MS/s upsample."""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from collections import Counter, defaultdict
from pathlib import Path

import numpy as np

REPO = Path(__file__).resolve().parents[1]
ROOT = Path(os.environ.get("ADSB27_ROOT", "/tmp/adsb-27"))
BIN = Path(os.environ.get("ADSB27_BIN", ROOT / "bin"))
DATA = ROOT / "data"
OUT = ROOT / "out"
MODES1 = REPO / "tests/data/modes1.bin"
AWEC = REPO / "tests/data/awec2026/AWEC2026_ADSB_Data/raw_data/recording.bin"

IN_HZ = 2_000_000
OUT_HZ = 2_400_000
PAD_S = 2.5  # dump1090-fa ifile watchdog on short files

CRC_POLY = 0xFFF409


def crc24(msg: bytes, bits: int) -> int:
    crc = 0
    for i in range(bits):
        bit = (msg[i // 8] >> (7 - (i % 8))) & 1
        crc ^= bit << 23
        if crc & 0x800000:
            crc = ((crc << 1) ^ CRC_POLY) & 0xFFFFFF
        else:
            crc = (crc << 1) & 0xFFFFFF
    return crc


def df_of(hexmsg: str) -> int:
    return int(hexmsg[0:2], 16) >> 3


def bits_of(hexmsg: str) -> int:
    df = df_of(hexmsg)
    return 112 if df & 0x10 else 56


def wire_bytes(hexmsg: str) -> bytes:
    n = bits_of(hexmsg) // 8
    return bytes.fromhex(hexmsg[: n * 2])


def crc_ok(hexmsg: str) -> bool:
    raw = wire_bytes(hexmsg)
    return crc24(raw, bits_of(hexmsg)) == 0


def aa(hexmsg: str) -> str | None:
    df = df_of(hexmsg)
    if df in (11, 17, 18):
        return hexmsg[2:8].upper()
    if df in (0, 4, 5, 16, 20, 21):
        return f"{crc24(wire_bytes(hexmsg), bits_of(hexmsg)):06X}"
    return None


def tc(hexmsg: str) -> int | None:
    if df_of(hexmsg) not in (17, 18) or len(hexmsg) < 10:
        return None
    return int(hexmsg[8:10], 16) >> 3


def is_airborne_cpr(hexmsg: str) -> bool:
    t = tc(hexmsg)
    return t is not None and 9 <= t <= 18


def cpr_odd(hexmsg: str) -> bool | None:
    if not is_airborne_cpr(hexmsg):
        return None
    raw = bytes.fromhex(hexmsg[:28])
    return bool((raw[6] >> 2) & 1)


def resample_cu8(src: Path, dst: Path) -> None:
    raw = np.fromfile(src, dtype=np.uint8)
    if raw.size % 2:
        raw = raw[:-1]
    i = raw[0::2].astype(np.float64)
    q = raw[1::2].astype(np.float64)
    n = i.size
    n_out = int(np.floor((n - 1) * OUT_HZ / IN_HZ)) + 1
    t_out = np.arange(n_out, dtype=np.float64) * (IN_HZ / OUT_HZ)
    io = np.interp(t_out, np.arange(n, dtype=np.float64), i)
    qo = np.interp(t_out, np.arange(n, dtype=np.float64), q)
    out = np.empty(n_out * 2, dtype=np.uint8)
    out[0::2] = np.clip(np.rint(io), 0, 255).astype(np.uint8)
    out[1::2] = np.clip(np.rint(qo), 0, 255).astype(np.uint8)
    dst.write_bytes(out.tobytes())


def pad_cu8(src: Path, dst: Path, min_s: float) -> None:
    data = src.read_bytes()
    samples = len(data) // 2
    need = int(min_s * OUT_HZ)
    if samples >= need:
        dst.write_bytes(data)
        return
    extra = (need - samples) * 2
    dst.write_bytes(data + bytes([127, 127]) * (extra // 2))


def parse_star(text: str) -> list[str]:
    out = []
    for line in text.splitlines():
        line = line.strip()
        if not line.startswith("*"):
            continue
        body = line[1:]
        if body.endswith(";"):
            body = body[:-1]
        body = body.upper()
        if re.fullmatch(r"[0-9A-F]{14,28}", body):
            out.append(body)
    return out


def parse_trck(text: str) -> tuple[list[str], dict]:
    msgs = []
    stats = {}
    for line in text.splitlines():
        if line.startswith("msg "):
            rest = line[4:]
            hexmsg, _, _ = rest.partition(" t=")
            hexmsg = hexmsg.strip().upper()
            if re.fullmatch(r"[0-9A-F]{14,28}", hexmsg):
                msgs.append(hexmsg)
        if line.startswith("iq messages="):
            for part in line.split():
                if "=" in part:
                    k, v = part.split("=", 1)
                    try:
                        stats[k] = float(v) if "." in v else int(v)
                    except ValueError:
                        stats[k] = v
    return msgs, stats


def parse_time_v(text: str) -> dict:
    got = {}
    for line in text.splitlines():
        if "User time (seconds):" in line:
            got["user"] = float(line.split(":")[-1])
        elif "System time (seconds):" in line:
            got["sys"] = float(line.split(":")[-1])
        elif "Maximum resident set size (kbytes):" in line:
            got["rss_kb"] = int(line.split(":")[-1])
        elif "Elapsed (wall clock) time" in line:
            got["wall"] = line.split("):")[-1].strip()
        elif "Exit status:" in line:
            got["exit"] = int(line.split(":")[-1])
    got["cpu"] = got.get("user", 0) + got.get("sys", 0)
    return got


def parse_fa_stats(text: str) -> dict:
    d = {
        "preambles": None,
        "accepted0": None,
        "accepted1": 0,
        "samples": None,
        "airborne_pos": None,
        "global_cpr": None,
        "local_cpr": None,
        "unique_tracks": None,
    }
    for line in text.splitlines():
        s = line.strip()
        m = re.search(r"(\d+) samples processed", s)
        if m:
            d["samples"] = int(m.group(1))
        m = re.search(r"(\d+) Mode-S message preambles received", s)
        if m:
            d["preambles"] = int(m.group(1))
        m = re.search(r"(\d+) accepted with correct CRC", s)
        if m:
            d["accepted0"] = int(m.group(1))
        m = re.search(r"(\d+) accepted with 1-bit error repaired", s)
        if m:
            d["accepted1"] = int(m.group(1))
        m = re.search(r"(\d+) airborne position messages received", s)
        if m:
            d["airborne_pos"] = int(m.group(1))
        m = re.search(r"(\d+) global CPR attempts with valid positions", s)
        if m:
            d["global_cpr"] = int(m.group(1))
        m = re.search(r"(\d+) local CPR attempts with valid positions", s)
        if m:
            d["local_cpr"] = int(m.group(1))
        m = re.search(r"(\d+) unique aircraft tracks", s)
        if m:
            d["unique_tracks"] = int(m.group(1))
    return d


def run(cmd: list[str], stdout_path: Path, stderr_path: Path, time_path: Path) -> dict:
    time_cmd = ["/usr/bin/time", "-v", "-o", str(time_path), "--"] + cmd
    with stdout_path.open("w") as so, stderr_path.open("w") as se:
        proc = subprocess.run(time_cmd, stdout=so, stderr=se)
    t = parse_time_v(time_path.read_text())
    t["proc_exit"] = proc.returncode
    t["cmd"] = cmd
    return t


def hamming(a: str, b: str) -> int | None:
    if len(a) != len(b):
        return None
    xa = int(a, 16)
    xb = int(b, 16)
    return (xa ^ xb).bit_count()


def classify_msg(hexmsg: str) -> str:
    df = df_of(hexmsg)
    if df in (17, 18):
        return f"DF{df}"
    if df == 11:
        return "DF11"
    if df in (0, 4, 5):
        return f"DF{df}-short"
    if df in (16, 20, 21):
        return f"DF{df}-long"
    return f"DF{df}"


def summarize(msgs: list[str]) -> dict:
    # Decoder output is already CRC-accepted (DF17 remainder 0, DF11 PI, DF0 AP).
    valid = list(msgs)
    df17_bad = sum(1 for m in msgs if df_of(m) in (17, 18) and not crc_ok(m))
    dfs = Counter(df_of(m) for m in valid)
    icaos = {aa(m) for m in valid if aa(m)}
    icaos.discard(None)
    df17_18 = sum(dfs[d] for d in (17, 18))
    cpr = [m for m in valid if is_airborne_cpr(m)]
    fixes = 0
    pending: dict[str, dict[bool, str]] = defaultdict(dict)
    for m in cpr:
        icao = aa(m)
        odd = cpr_odd(m)
        if icao is None or odd is None:
            continue
        pending[icao][odd] = m
        if True in pending[icao] and False in pending[icao]:
            fixes += 1
            pending[icao].clear()
    return {
        "n": len(msgs),
        "crc_ok": len(valid),
        "df17_18": df17_18,
        "df11": dfs[11],
        "unique_icao": len(icaos),
        "cpr_frames": len(cpr),
        "cpr_pairs": fixes,
        "df17_crc_nonzero": df17_bad,
        "df": dict(sorted(dfs.items())),
        "icaos": sorted(x for x in icaos if x is not None),
        "valid": valid,
    }


def best_of_3(cmd: list[str], tag: str) -> tuple[dict, Path, Path]:
    times = []
    last_so = last_se = None
    for i in range(3):
        so = OUT / f"{tag}.{i}.stdout"
        se = OUT / f"{tag}.{i}.stderr"
        tp = OUT / f"{tag}.{i}.time"
        t = run(cmd, so, se, tp)
        t["i"] = i
        times.append((t["cpu"], t, so, se))
        last_so, last_se = so, se
    times.sort(key=lambda x: x[0])
    best = times[0]
    return best[1], best[2], best[3]


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)
    DATA.mkdir(parents=True, exist_ok=True)

    even = bytes.fromhex("8D40058B58C901375147EFD09357")
    assert crc24(even, 112) == 0, crc24(even, 112)

    files = {}
    for name, src in (("modes1", MODES1), ("awec", AWEC)):
        assert src.is_file(), src
        raw = DATA / f"{name}.2msps.cu8"
        if not raw.exists():
            raw.write_bytes(src.read_bytes())
        rs = DATA / f"{name}.2p4msps.cu8"
        if not rs.exists():
            print(f"resample {src} -> {rs}", flush=True)
            resample_cu8(src, rs)
        pad = DATA / f"{name}.2p4msps.pad.cu8"
        samples = rs.stat().st_size // 2
        need_pad = samples < int(PAD_S * OUT_HZ)
        if not pad.exists():
            if need_pad:
                pad_cu8(rs, pad, PAD_S)
            else:
                pad.write_bytes(rs.read_bytes())
        files[name] = {
            "raw2": raw,
            "rs24": rs,
            "pad24": pad,
            "bytes2": src.stat().st_size,
            "bytes24": rs.stat().st_size,
            "bytes_pad": pad.stat().st_size,
        }

    trck = str(BIN / "trck")
    fa = str(BIN / "dump1090-fa")
    rsb = str(BIN / "readsb")
    ant = str(BIN / "dump1090-antirez")

    recs = []

    # Interleave CPU runs: for each recording, cycle decoders.
    jobs = []
    for rec, paths in files.items():
        iq24 = str(paths["pad24"])
        iq2 = str(paths["raw2"])
        jobs.append(
            (
                rec,
                "trck",
                "nofix",
                [trck, "--iq", iq2, "--iq-rate", "2000000", "--uart", "-", "--table", str(OUT / f"{rec}.trck.csv")],
            )
        )
        jobs.append(
            (
                rec,
                "dump1090-fa",
                "nofix",
                [fa, "--ifile", iq24, "--iformat", "UC8", "--raw", "--no-fix", "--stats", "--no-modeac-auto"],
            )
        )
        jobs.append(
            (
                rec,
                "readsb",
                "nofix",
                [
                    rsb,
                    "--device-type",
                    "ifile",
                    "--ifile",
                    iq24,
                    "--iformat",
                    "UC8",
                    "--raw",
                    "--no-fix",
                    "--no-interactive",
                ],
            )
        )
        jobs.append(
            (
                rec,
                "dump1090-fa",
                "fix1",
                [fa, "--ifile", iq24, "--iformat", "UC8", "--raw", "--fix", "--stats", "--no-modeac-auto"],
            )
        )
        jobs.append(
            (
                rec,
                "readsb",
                "fix1",
                [
                    rsb,
                    "--device-type",
                    "ifile",
                    "--ifile",
                    iq24,
                    "--iformat",
                    "UC8",
                    "--raw",
                    "--fix",
                    "--no-interactive",
                ],
            )
        )
        jobs.append(
            (
                rec,
                "dump1090-antirez",
                "nofix",
                [ant, "--ifile", iq2, "--raw", "--no-fix"],
            )
        )
        jobs.append(
            (
                rec,
                "dump1090-antirez",
                "fix1",
                [ant, "--ifile", iq2, "--raw"],
            )
        )

    results = {}
    for rec, dec, mode, cmd in jobs:
        tag = f"{rec}.{dec}.{mode}"
        print(f"RUN {tag}: {' '.join(cmd)}", flush=True)
        t, so, se = best_of_3(cmd, tag)
        stdout = so.read_text(errors="replace")
        stderr = se.read_text(errors="replace")
        if dec == "trck":
            msgs, extra = parse_trck(stderr)
            fa_stats = {}
        else:
            msgs = parse_star(stdout)
            extra = parse_trck(stderr)[1] if dec == "trck" else {}
            fa_stats = parse_fa_stats(stdout + "\n" + stderr)
        summ = summarize(msgs)
        row = {
            "rec": rec,
            "dec": dec,
            "mode": mode,
            "cmd": cmd,
            "time": t,
            "summary": {k: v for k, v in summ.items() if k not in ("valid", "icaos")},
            "fa_stats": fa_stats,
            "trck_extra": extra,
            "msgs": summ["valid"],
            "raw_msgs": msgs,
        }
        results[tag] = row
        print(
            f"  n={summ['n']} crc_ok={summ['crc_ok']} DF17/18={summ['df17_18']} DF11={summ['df11']} "
            f"icao={summ['unique_icao']} cpu={t['cpu']:.4f}s rss={t.get('rss_kb')}KB",
            flush=True,
        )

    # untimed readsb --stats for FEC counters
    for rec, paths in files.items():
        iq24 = str(paths["pad24"])
        for mode, extra in (("nofix", ["--no-fix"]), ("fix1", ["--fix"])):
            cmd = [
                rsb,
                "--device-type",
                "ifile",
                "--ifile",
                iq24,
                "--iformat",
                "UC8",
                "--stats",
                "--no-interactive",
                *extra,
            ]
            tag = f"{rec}.readsb.{mode}.stats"
            so = OUT / f"{tag}.stdout"
            se = OUT / f"{tag}.stderr"
            tp = OUT / f"{tag}.time"
            run(cmd, so, se, tp)
            stats = parse_fa_stats(so.read_text(errors="replace") + "\n" + se.read_text(errors="replace"))
            results[f"{rec}.readsb.{mode}"]["fa_stats"] = stats

    # diffs on no-fix 2.4-matched set
    diffs = {}
    for rec in files:
        tr = set(results[f"{rec}.trck.nofix"]["msgs"])
        fa_n = set(results[f"{rec}.dump1090-fa.nofix"]["msgs"])
        rs_n = set(results[f"{rec}.readsb.nofix"]["msgs"])
        fa_f = set(results[f"{rec}.dump1090-fa.fix1"]["msgs"])
        rs_f = set(results[f"{rec}.readsb.fix1"]["msgs"])
        both_n = fa_n & rs_n
        both_f = fa_f & rs_f
        missed_n = both_n - tr
        extra_n = tr - (fa_n | rs_n)
        extra_vs_both = tr - both_n
        missed_f = both_f - tr
        # 1-bit away from a trck message?
        def near_trck(m: str) -> bool:
            for tmsg in tr:
                h = hamming(m, tmsg)
                if h == 1:
                    return True
            return False

        missed_1bit = [m for m in missed_f if near_trck(m)]
        missed_n_cls = Counter(classify_msg(m) for m in missed_n)
        missed_f_cls = Counter(classify_msg(m) for m in missed_f)
        extra_cls = Counter(classify_msg(m) for m in extra_n)
        diffs[rec] = {
            "both_nofix": len(both_n),
            "both_fix": len(both_f),
            "trck": len(tr),
            "missed_nofix_count": len(missed_n),
            "missed_nofix_multiset": len(
                [m for m in results[f"{rec}.dump1090-fa.nofix"]["msgs"] if m in missed_n]
            ),
            "missed_nofix_class": dict(missed_n_cls),
            "missed_fix_count": len(missed_f),
            "missed_fix_class": dict(missed_f_cls),
            "trck_only_vs_union": len(extra_n),
            "trck_only_class": dict(extra_cls),
            "trck_not_in_both": len(extra_vs_both),
            "missed_fix_1bit_from_trck": len(missed_1bit),
            "missed_nofix_sample": sorted(missed_n)[:20],
            "trck_only_sample": sorted(extra_n)[:20],
            "missed_nofix_set": sorted(missed_n),
            "trck_only_set": sorted(extra_n),
            "fa_nofix": len(fa_n),
            "rs_nofix": len(rs_n),
            "fa_fix": len(fa_f),
            "rs_fix": len(rs_f),
        }

    versions = {
        "trck": subprocess.check_output(
            ["git", "-C", str(REPO), "rev-parse", "HEAD"], text=True
        ).strip(),
        "trck_crate": "0.3.3",
        "dump1090-fa": subprocess.check_output(
            ["git", "-C", "/tmp/adsb-27/src/dump1090-fa", "rev-parse", "HEAD"], text=True
        ).strip(),
        "readsb": subprocess.check_output(
            ["git", "-C", "/tmp/adsb-27/src/readsb", "rev-parse", "HEAD"], text=True
        ).strip(),
        "dump1090-antirez": subprocess.check_output(
            ["git", "-C", "/tmp/adsb-27/src/dump1090-antirez", "rev-parse", "HEAD"], text=True
        ).strip(),
        "host": subprocess.check_output(["uname", "-a"], text=True).strip(),
        "nproc": subprocess.check_output(["nproc"], text=True).strip(),
        "uptime": subprocess.check_output(["uptime"], text=True).strip(),
        "rs1090": "0.8.1",
    }

    slim = []
    for tag, row in results.items():
        slim.append(
            {
                "tag": tag,
                "rec": row["rec"],
                "dec": row["dec"],
                "mode": row["mode"],
                "cmd": row["cmd"],
                "cpu_s": row["time"]["cpu"],
                "user_s": row["time"].get("user"),
                "sys_s": row["time"].get("sys"),
                "rss_kb": row["time"].get("rss_kb"),
                "wall": row["time"].get("wall"),
                "exit": row["time"].get("proc_exit"),
                "summary": row["summary"],
                "fa_stats": row["fa_stats"],
                "trck_extra": row["trck_extra"],
            }
        )

    payload = {
        "versions": versions,
        "files": {k: {kk: (str(vv) if isinstance(vv, Path) else vv) for kk, vv in v.items()} for k, v in files.items()},
        "rows": slim,
        "diffs": {k: {kk: vv for kk, vv in v.items() if not kk.endswith("_set")} for k, v in diffs.items()},
        "missed_nofix": {k: v["missed_nofix_set"] for k, v in diffs.items()},
        "trck_only": {k: v["trck_only_set"] for k, v in diffs.items()},
    }
    (OUT / "summary.json").write_text(json.dumps(payload, indent=2))
    print("wrote", OUT / "summary.json")


if __name__ == "__main__":
    sys.exit(main())

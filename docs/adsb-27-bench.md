# ADSB-27: trck vs dump1090-fa vs readsb

Decoder vs decoder on the same IQ. No code changes. Do not merge.

## Verdict

1. On identical 2.4 MS/s bytes, trck's unique CRC-valid set matches the intersection of dump1090-fa `--no-fix` and readsb `--no-fix` (0 unique misses, 0 unique extras after truncating short frames to 7 bytes).
2. trck is behind once 1-bit CRC correction is on: dump1090-fa repaired 7 (AWEC) and 8 (modes1.bin) messages; readsb repaired 11 and 29. trck has no FEC path.
3. trck is behind native 2 MS/s dump1090 (antirez) on the Porto recording by 14 unique messages (9 of them DF17). The live demod is dump1090-2400 only.

## Builds

| Binary | Source | Commit |
| --- | --- | --- |
| trck 0.3.3 | `/agent/repos/adsb` `main` | `8fab943fc968ef9b96135496030896eac422cf16` |
| rs1090 | crate | 0.8.1 |
| dump1090-fa | github.com/flightaware/dump1090 | `465e06330372ab0586a6a5063c66b60a8750f2b6` |
| readsb 3.16.21 | github.com/wiedehopf/readsb | `88877b96dff8f82cf75a7759f67dccabc85edcfb` |
| dump1090 (antirez) | github.com/antirez/dump1090 | `efe64db3c6ba1520291331628a33c1e208e851a6` |

Host: x86_64, `Intel(R) Xeon(R) Processor` 2400 MHz, 4 cores, rustc 1.99.1. dump1090-fa and readsb built with `CC=gcc` `-O3`/`-O2`, RTLSDR=no. trck `cargo build --release --locked`.

Recordings (gitignored `tests/data/`, via `scripts/fetch-iq.sh`):

| File | Bytes | Format |
| --- | --- | --- |
| antirez `testfiles/modes1.bin` | 713736 | cu8, 2 MS/s, 0.357 s |
| Zenodo 23342598 `recording.bin` | 72876032 | cu8, 2 MS/s, 18.219 s |

dump1090-fa and readsb hardcode 2.4 MS/s. Feeding the 2 MS/s files as `--ifile` without upsample yields 0 accepted CRC (readsb: 356868 samples, 0 accepted). trck `--iq-rate 2000000` linear-interpolates to 2.4 MS/s in `src/radio.rs`. Fair same-sample comparison uses a numpy linear upsample (same ratio as trck's `Resamp`) written to `/tmp/adsb-27/data/*.2p4msps.cu8`. modes1.bin was zero-padded to 2.5 s so dump1090-fa's ifile watchdog still drains the FIFO. Pad adds no messages.

## Commands

```
# upsample 2e6 -> 2.4e6 cu8 (script does this)
# trck on original 2 MS/s (internal resample)
trck --iq FILE --iq-rate 2000000 --uart - --table /tmp/t.csv

# trck on the shared 2.4 MS/s file
trck --iq FILE.2p4msps.cu8 --iq-rate 2400000 --uart - --table /tmp/t.csv

dump1090-fa --ifile FILE.2p4msps.cu8 --iformat UC8 --raw --no-fix --stats --no-modeac-auto
dump1090-fa --ifile FILE.2p4msps.cu8 --iformat UC8 --raw --fix --stats --no-modeac-auto

readsb --device-type ifile --ifile FILE.2p4msps.cu8 --iformat UC8 --raw --no-fix --no-interactive
readsb --device-type ifile --ifile FILE.2p4msps.cu8 --iformat UC8 --raw --fix --no-interactive

dump1090 --ifile FILE.2msps.cu8 --raw --no-fix   # antirez, native 2 MS/s
dump1090 --ifile FILE.2msps.cu8 --raw            # antirez default, 1-bit fix
```

dump1090-fa default `nfix_crc` is 0. `--fix` is 1-bit. readsb default is 1-bit. trck has no flag. dump1090-fa `--ifile` exits 1 (`Abnormal exit`) after EOF; stats and `*hex;` still printed.

Message identity: DF with bit4 set is 14 bytes, else first 7 bytes. trck always prints 14 bytes (`ModeSMessage.msg: [u8; 14]`). Raw hex equality without that truncate makes every short frame look unique to trck.

## Table: totals (decoder output, not unique)

`n` is every accepted line. `uniq` is length-normalized hex. `AA` is unique ICAO from DF11/17/18 only. `FEC` is `accepted with 1-bit error repaired` from dump1090-fa/readsb stats, else 0. `CPR` for trck is `fixes=` UART airborne solves. For dump1090-fa/readsb it is `global CPR attempts with valid positions`.

### modes1.bin

| Decoder | CRC fix | n | uniq | DF17/18 n (uniq) | DF11 n (uniq) | AA | FEC | CPR | CPU s best of 3 | RSS kB |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| trck `--iq-rate 2000000` | none | 372 | 146 | 207 (113) | 110 (4) | 1 | 0 | 1 UART | 0.01 | 5532 |
| trck `--iq-rate 2400000` on shared 2.4 file | none | 369 | 144 | 203 (110) | 110 (5) | 1 | 0 | 1 UART | <0.02 | ~5.5k |
| dump1090-fa | `--no-fix` | 294 | 145 | 163 (111) | 90 (5) | 1 | 0 | 19 global | 0.00 | 6500 |
| dump1090-fa | `--fix` | 301 | 147 | 169 (113) | 91 (5) | 1 | 8 | 19 | 0.01 | 6456 |
| readsb | `--no-fix` | 309 | 156 | 172 (117) | 94 (9) | 1 | 0 | 1 | 0.01 | 5676 |
| readsb | `--fix` | 325 | 162 | 183 (123) | 99 (9) | 1 | 29 | 1 | 0.01 | 5888 |
| dump1090 antirez 2 MS/s | `--no-fix` | 217 | 112 | 120 (85) | 63 (5) | 1 | 0 | n/a | 0.00 | 3144 |
| dump1090 antirez 2 MS/s | default 1-bit | 217 | 111 | 120 (85) | 63 (4) | 1 | 0 | n/a | 0.00 | 3468 |

modes1.bin is 0.357 s. `/usr/bin/time -v` is 0.01 s resolution. Those CPU numbers are noise.

### Porto AWEC 2026 `recording.bin`

| Decoder | CRC fix | n | uniq | DF17/18 n (uniq) | DF11 n (uniq) | AA | FEC | CPR | CPU s | RSS kB |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| trck `--iq-rate 2000000` | none | 121 | 36 | 20 (12) | 11 (1) | 1 | 0 | 6 UART | 1.20 best of 3 | 5484 |
| trck `--iq-rate 2400000` on shared 2.4 file | none | 103 | 33 | 16 (10) | 10 (1) | 1 | 0 | 3 UART | 0.47 best of 5 | 5148 |
| dump1090-fa | `--no-fix` | 90 | 34 | 15 (11) | 10 (1) | 1 | 0 | 5 global | 0.37 best of 5 | 6396 |
| dump1090-fa | `--fix` | 96 | 38 | 20 (15) | 11 (1) | 1 | 7 | 5 | 0.41 best of 3 | 6504 |
| readsb | `--no-fix` | 101 | 36 | 17 (11) | 9 (1) | 1 | 0 | 3 | 0.17 best of 5 | 5772 |
| readsb | `--fix` | 112 | 43 | 26 (18) | 11 (1) | 1 | 11 | 8 | 0.19 best of 3 | 5744 |
| dump1090 antirez 2 MS/s | `--no-fix` | 141 | 50 | 31 (21) | 12 (1) | 1 | 0 | n/a | 0.30 | 3336 |
| dump1090 antirez 2 MS/s | default 1-bit | 144 | 51 | 33 (22) | 13 (1) | 1 | n/a | n/a | 0.29 | 3464 |

AWEC 2.4-file CPU, 5 alternating runs, user+sys seconds:

| | trck 2 MS/s path | trck 2.4 file | dump1090-fa no-fix | readsb no-fix |
| --- | --- | --- | --- | --- |
| runs | 1.46, 1.21, 1.24, 1.28, 1.24 | 0.58, 0.49, 0.54, 0.47, 0.49 | 0.42, 0.40, 0.46, 0.37, 0.41 | 0.19, 0.17, 0.22, 0.17, 0.22 |
| best | 1.21 | 0.47 | 0.37 | 0.17 |
| median | 1.24 | 0.49 | 0.41 | 0.19 |

Limiter: single-thread demod plus, on the 2 MS/s trck path, `Resamp` lerp of 36.4e6 complex samples (`src/radio.rs:166`). Feeding the already-2.4 file drops trck from 1.21 s to 0.47 s, so resample is about 0.7 s of the 1.2 s. Remaining gap vs dump1090-fa is f32 `magnitude_u16` and `demodulate2400` visiting every sample. stderr `msg HEX` logging is on for `--iq`.

## Unique-set diffs

Same 2.4 MS/s bytes, `--no-fix`:

| Recording | both fa and readsb | trck has all of both | trck extras vs union | fa-only vs trck | readsb-only vs trck |
| --- | --- | --- | --- | --- | --- |
| modes1 | 139 | 139 (0 miss) | 0 | 1 | 17 |
| AWEC | 33 | 33 (0 miss) | 0 | 1 | 3 |

`--fix` extras vs trck (unique, both fa and readsb, AWEC): 4 DF17 frames, including `8D407181990C2337F004454DBF8D` and `8D407181F82100020049B8CA8DA3`. Hamming distance to trck's nearby DF17 is 16 to 35 bits, not a 1-bit slice of a trck message. Recovering them needs CRC correction in scoring, not a preamble tweak.

trck `--iq-rate 2000000` vs others on numpy 2.4 (AWEC): misses `8D40718158AF86E8BDEB2E7F2D9C` which trck itself decodes when given the numpy 2.4 file. Cause is `Resamp` vs the shared upsample, not demod CRC.

antirez native 2 MS/s vs trck 2 MS/s path on AWEC: antirez-only 14 unique (9 DF17). trck-only 0. That is the largest unique loss, and it is the missing 2 MS/s demod.

## Gaps ranked by unique messages lost

1. **No 2 MS/s demod, ~14 unique on AWEC (9 DF17).** Live path is `demodulate2400` at 2.4 MS/s (`src/radio.rs:11`, `src/radio.rs:67`, rs1090 `demod2400.rs:41`). Native 2 MS/s dump1090 finds frames the 2.4 resample path never slices. Estimate: all 14 unique if a 2 MS/s demod is added. Zero of those 14 are in trck today.

2. **No 1-bit CRC correction, ~4 unique DF17 on AWEC both-others `--fix`, 7/11 raw FEC accepts.** `validate_modes_message` requires remainder 0 for DF17 (`rs1090-0.8.1/.../demod/mod.rs:271-295`). `demodulate2400` never calls a corrector (`demod2400.rs:119`). dump1090-fa `mode_s.c` + `crc.c` 1-bit tables. Estimate: the `--fix` minus `--no-fix` delta (AWEC fa +6 n / +4 uniq, readsb +11 n / +7 uniq).

3. **2 MS/s `Resamp` phase, 1 DF17 + 1 DF11 vs numpy 2.4 on AWEC/modes1.** `src/radio.rs:166-216`. Same DF17 appears in trck on the numpy 2.4 file. Estimate: 1 to 2 unique per recording if upsample matches dump1090's 2.4 clock.

4. **No skip after a hit, duplicates not losses.** `skip_count` in `demod2400.rs:49-54` is never set. dump1090-fa skips to 8 bits before end of the frame (`demod_2400.c:413-419`). trck n is 15 to 30% above dump1090-fa on the same 2.4 file with the same unique set. Fixing skip would drop n toward dump1090-fa, not raise unique.

5. **Always 14-byte `msg`.** `src/radio.rs:73-76` prints 14 bytes for DF0/4/5/11. dump1090 prints 7. Not extra CRC-valid frames after truncate. Tracker still gets the first 7 bytes plus 7 garbage. `src/decode.rs` `Message::try_from` on 14-byte DF11 may not match a 7-byte parse. UART CPR on modes1 is 1 solve vs 19 dump1090-fa global CPR. That gap is tracking/CPR, not IQ detection.

False positives (trck unique not in fa or readsb) on the shared 2.4 file: **0**.

## Pi Zero 2 W

Not measured on a Pi. Extrapolation only.

Live radio is 2.4 MS/s with no `Resamp` (`SAMPLE_HZ` in `src/radio.rs:19`). On this 2.4 GHz Xeon, that path is 0.47 s CPU for 18.219 s of 2 MS/s IQ upsampled to 21.86 s of 2.4 MS/s samples, about 46x realtime, 1.27x dump1090-fa (0.37 s) and 2.8x readsb (0.17 s). Peak RSS 5.1 MB.

dump1090-fa is commonly run on a Zero 2 W at 2.4 MS/s. trck at 1.3x dump1090-fa CPU on this IQ, plus `--iq` hex logging that live RTL-SDR disables (`log_msg` false in `run_rtlsdr`), should stay under one A53 core with margin. The 2 MS/s file path is 3.3x dump1090-fa here because of lerp. That path is not the live dongle path.

If an A53 is 8 to 15x this Xeon core, 0.47 s becomes 4 to 7 s vs 21.9 s of samples. Still realtime. This is an extrapolation.

## Re-run

```
scripts/fetch-iq.sh
# build dump1090-fa, readsb, trck into $ADSB27_BIN
python3 scripts/bench-adsb-27.py
```

IQ stays in `tests/data/` (gitignored).

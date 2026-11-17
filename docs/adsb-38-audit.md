# ADSB-38 codebase audit

Base: `main` at `f2871ea` (trck 0.3.5). Scope: Pi sender, vendor `rs-rtl`, ESP32 `.ino`. Out of scope: nRF24, HackRF, `.ino` vs PlatformIO drift.

Method: tree read, `cargo test --locked` (pass), `cargo clippy --locked --all-targets -- -D warnings` (pass), throwaway proofs in `/tmp/trck-audit-prove` (deleted after), IQ timing on public recordings. Host for timing and stripped sizes is x86_64 glibc, not a Pi and not `aarch64-unknown-linux-musl`. Pi numbers below are labeled estimates.

Bar used: smallest change, subtract before add, no extra layers.

## 1. Code quality

Clippy `-D warnings` is clean. Pedantic is advisory (casts, `must_use`, missing `# Panics` / `# Errors`). `unsafe` is three documented blocks (`src/sys.rs:17`, `tests/common/mod.rs:225-259`). Vendor has none.

**Dead**

- `Table::append` (`src/table.rs:46-48`) has no callers. Live paths use `append_at`.
- `ModeSMessage.signal_level` and `score` (`src/demod.rs:22-23, 134-137`) are written and never read.
- `rs-rtl` `dropped` / `dropped_chunks` (`vendor/rs-rtl/src/rtlsdr.rs:901-902, 1134-1161`) is never incremented. Blocking `tx.send` cannot record overflow.
- `force_direct_sampling` (`vendor/rs-rtl/src/rtlsdr.rs:375-377`) is stored from EEPROM byte 7 bit 0 and never read. `force_bias_t` is applied (`:669`).
- `FreqRange.xtal_cap20p` / `xtal_cap10p` (`vendor/rs-rtl/src/tuner.rs:210-213`) are `#[allow(dead_code)]`.
- `BLOCK_DEMOD` (`vendor/rs-rtl/src/device.rs:44-45`) unused.

**Duplication (keep `.ino` vs pio)**

- UART/CSV defaults: `src/main.rs:28-29` vs `src/doctor.rs:10-11`.
- RTL VID/PID: doctor vs `rs_rtl::RTL_USB_VID`.
- UART poll wait copied in `drain_writer` / `run_writer` / `run_check` (`src/writer.rs`).
- CRC bytes packed in `src/modes.rs:96-98`, `src/crc_fix.rs:165-167`, `src/iq.rs:268-270`.

**Abstractions**

- `Fix` is a one-field wrapper (`src/frame.rs:138-142`). Every use is `fix.aircraft`. `src/sim.rs` has a second `Aircraft`.
- `writer.rs` mixes UART I/O, TCMD, power/restart, CSV. That is the mixed-concern file, not `tuner.rs` tables.
- `Queue::unsent` / `advance` panic (`src/queue.rs:94-107`). Writer drops the lock between `has_current` and `unsent` (`src/writer.rs:208-239`). Safe today because only the writer thread mutates `current`.

**Error handling**

- CSV open failure prints and continues (`src/writer.rs:416-422`). Write errors print and return (`:163-165`). IQ `csv=` still counts enqueue, not disk success.
- `sim::render` `expect`s CSV serialize (`src/sim.rs:561`).
- Hex keepalive `setsockopt` failure is a disconnect (`src/hex.rs:222-225`).
- `now_unix` / CSV timestamps collapse clock errors to epoch (`src/main.rs:230-235`, `src/table.rs:74-77`).

**Tests**

- Surface CPR, `--no-fix` vs `--fix` on damaged IQ, UART TCMD on a PTY, DF18, altitude > 50 000 ft, and 7-byte IQ ingest are missing. In-repo tests cover airborne EVEN/ODD, ident attach, 7700 on hex/stress, queue drop policy, FC0013 AGC after `DEFAULT_GAIN` removal.

## 2. Crate replacements

Stripped x86_64-unknown-linux-gnu `--release`: **3 305 744 B**. Not musl, not aarch64. `.text` buckets from `nm`: clap 433 KB, zstd 254 KB, trck 247 KB, toml 230 KB, tokio 167 KB, rs1090 163 KB, panic/gimli 151 KB, rs-rtl 103 KB, desperado 94 KB.

| Change | License | Size (measured unless noted) | Worth it? |
| --- | --- | --- | --- |
| Drop **desperado** from the live path. Use vendored `rs-rtl` + `std::fs` for `--iq`. | MIT | zstd C **254 KB** plus desperado **94 KB**. Est. **−250 to −350 KB** `.text` | Yes. Biggest size win. `rs-rtl` is already patched. |
| Feature-gate `sim` / **toml** | MIT/Apache | toml **230 KB** | Yes for the Pi binary. `sim` is not the contest path. |
| `panic = "abort"` | — | **−484 840 B** (3 305 744 → 2 820 904) | Yes if you accept no panic backtraces on the Pi. |
| clap `default-features = false` + `std,derive,help,usage,error-context` | MIT/Apache | **−38 080 B** | Yes. Cheap. |
| Replace **tokio** in trck with the existing `Condvar` radio thread | MIT | tokio **167 KB** in this binary, still pulled by rs1090/desperado until those go | After desperado is gone. |
| Hand-write CSV rows, drop **csv** | MIT/Unlicense | **38 KB** | Maybe. Fixed columns, no quotes needed. |
| Inline CRC-16, drop **crc** | MIT/Apache | **4.4 KB** | No. ESP32 already has the poly. Size is noise. |
| Inline hex encode/decode | MIT/Apache | **21 KB**, also a rs1090 dep | No until rs1090 is gone. |
| Replace **rs1090** with a local DF17/DF11 parser | MIT | 163 KB + deku 96 KB. Est. **−150 to −250 KB** | Not yet. High effort. Default `bds-infer` is the 50 000 ft bug. Prefer `default-features = false` first. |
| **serialport** → `open` + nix termios | MPL-2.0 | **5 KB**, duplicate nix 0.26 vs 0.31 | Cleanup only. |
| **num-complex** / **futures-util** | MIT/Apache | few KB | Drop with desperado + mag LUT. |

Do not add `adsb_deku` (second deku stack). Do not crate-replace demod, CPR, or UART framing.

Hand-rolled mag LUT (`[u16; 65536]`, 128 KiB RAM) is the right dump1090 pattern. No extra crate.

## 3. Bugs

Proven means a throwaway program or in-tree test showed the failure. Suspected means code path only.

### Proven, live IQ / decode

**B1. IQ/radio ingest never applies 7-byte Mode S (DF5 squawk).**
`src/radio.rs:83` passes `&data.msg` (`[u8; 14]`). `Message::try_from` errors `"Too much data"` unless the slice length is exactly 7 or 14 (`rs1090` `decode/mod.rs:541-548`). `msg_octets` already returns 7 bytes (`src/demod.rs:29-35`) and is used only for logging (`src/radio.rs:78`).

Proof: DF5 identity 7700, ICAO `0x40058B`. `try_from(&df5[..7])` Ok, `try_from(&[u8;14])` Err. After EVEN/ODD, ingest of the 7-byte frame sets `EmergencyKind::General`. Ingest of the 14-byte pad leaves emergency `None`. Hex 7-byte AVR still works.

**B2. `--fix` rewrites AP-format DF16 into DF17.**
`src/crc_fix.rs:66-87` claims address/parity DFs are unchanged. Correction runs on any 14-byte buffer whose CRC matches a 1-bit syndrome if the trial is DF17/18 with CRC 0. DF4 unit test (`:146-155`) uses a frame that misses a syndrome.

Proof: syndrome of bit 4 is `0xC397DB`. DF16 constructed so `modes_checksum == 0xC397DB` flipped to DF17, AA `C397DB`, CRC 0.

**B3. All DF18 dropped, including ADS-B ES/NT (CF=0).**
`src/decode.rs:103`. rs1090 maps every DF18 to `ExtendedSquitterTisB` (`rs1090` `decode/mod.rs:199-205`), including `ControlFieldType::ADSB_ES_NT` (CF=0).

Proof: DF18 CF=0 with the EVEN/ODD ME fields parses as TisB. Both ingest calls return `None`. Same ME on DF17 solves.

**B4. Surface-only CPR never emits a first fix.**
`src/decode.rs:251-254` only runs `relative()` when a previous valid position exists. `src/cpr.rs` ports airborne global, not `decodeCPRsurface`. Pair windows and surface `speed_ok` exist.

Proof: even/odd BDS06 with no prior airborne fix returns `None` (decode-path audit). After an airborne solve, local surface decode works.

**B5. Local CPR fails across ±180°.**
`src/cpr.rs:125-129` wraps lon then requires `abs(rlon - ref) <= dlon/2` with no 360 wrap.

Proof: global at 10°, 179.80 then even at −179.80 (1.5 s later) → `None`. A new even/odd pair can still solve via `airborne_global`.

**B6. Altitude > 50 000 ft drops the DF17.**
rs1090 default feature `bds-infer` (`Cargo.toml` default). `bds05.rs:185-188`.

Proof: `airborne_position(..., 50175, ...)` parse error `"BDS 05 altitude 50175 ft > 50 000 ft"`. No Fix. 39 000 ft still works.

**B7. Surface MOV/TRK and BDS09 ST 3/4 never reach the UART.**
`src/decode.rs:178-184, 200-201`. Velocity only from `GroundSpeedDecoding`. BDS06 ignored in `cache_adsb`. Docs (`docs/esp32-alert.md:35`) say GS/track are last airborne velocity, so BDS06 is consistent with the wire contract. ST 3/4 (IAS/heading) is a miss versus dump1090.

**B8. Emergency never expires.**
`src/decode.rs:186-198, 302-310`. BDS61 none + squawk 1000 clears. Position-only after STATUS 7700 still reports `General` 17 s later. Only `TRACKER_CAP` eviction (256) removes it.

### Proven, UART / process

**B9. Pause vs enqueue race.**
`src/writer.rs:92-111`. `paused` is sampled, then `last_fix` / CSV / `queue.push`. `set_paused(true)` can `drop_waiting()` in that gap. Resume does not clear waiting. Status can show `paused=1` and `last_fix_age=0`. Tracker still consumes the CPR pair (`src/decode.rs:234-291`).

**B10. Restart ACK reports the old paused flag. New process is always running.**
`src/cmd.rs:111` + `Shared::new` `paused: false` (`src/writer.rs:80`) + `restart()` exec (`:344-355`). ESP32 stores `ack.paused` (`esp32/trck_uart/trck_uart.ino:206, 317-320`).

**B11. Reboot/shutdown ACK is Ok before `systemctl`. Failure still looks successful.**
`src/cmd.rs:112-116`, `src/writer.rs:269-275, 358-367`.

**B12. Second TCMD dropped while a reply is in flight.**
`src/writer.rs:47-54, 139-153`. Pause/resume/restart can vanish on a slow UART.

**B13. Command reader busy-spins on UART HUP, then dies.**
`src/writer.rs:375-378, 411-413`. `poll` waits `POLLIN` only. POSIX still reports `POLLHUP`; `read` returns 0; loop never sleeps. On a hard error the thread returns and is not respawned. Writer keeps sending.

**B14. ESP32 16-deep queue drops TRCK and TACK under load.**
`esp32/trck_uart/trck_uart.ino:263, 432-442`. Restart is not retried (`:370-383`). Pause retry is once.

**B15. `--hex -` on a TTY blocks inside `pull()`.**
`src/hex.rs:41-51, 70-77`. Non-TTY is set nonblocking. TTY `read` after the first chunk blocks, so `write_some` never runs. Pipes are fine.

### Proven, host tooling

**B16. `doctor` skips `[section]` headers and still applies the next keys.**
`src/doctor.rs:315-318`. Test at `:531-533` locks that in.

### Suspected

**S1. USB overflow is silent.** Blocking `tx.send` before URB resubmit (`vendor/rs-rtl/src/rtlsdr.rs:1134-1148`). 16 KiB URBs × 15 = **51 ms** in-flight vs librtlsdr’s 256 KiB. `dropped` never increments. desperado bridge `try_send` + 1 ms sleep (`desperado` `lifecycle.rs:21-26`) will stall that send. Not reproduced without a dongle.

**S2. EEPROM direct-sampling bit, if ever applied blindly, would bypass the FC0013 tuner at 1090 MHz.** Today the flag is dead (B-quality). Do not “fix” by applying it on open without a board that needs it.

**S3. `update.rs` ELF check is `\x7fELF` only (`src/update.rs:181-183`), not `e_machine`.** Current GitHub assets carry sha256 digest. Weak if digest is missing.

**S4. Hex keepalive failure disconnects (`src/hex.rs:222-225`).** Linux Pi should accept `TCP_KEEPIDLE`. A platform that rejects it never takes frames.

Not bugs: TRCK 34 / TCMD 7 / TACK 21, LE, magics, CRC-16/CCITT-FALSE match Pi ↔ `.ino`. FC0013 manual max still clears AGC (`tests/fc0013.rs`, `vendor/rs-rtl/src/fc0013.rs:151-158`). Airborne global CPR, NL table, 10 s pair window, dump1090-fa `speed_ok`, DF17 1-bit restore, two-bit reject. Mutex poison uses `into_inner`. `update` dest is `current_exe()`, tag is `vX.Y.Z`.

## 4. Performance

Host: x86_64 Xeon 2400 MHz, 4 cores. `rtf = wall / capture_duration`. IQ from `scripts/fetch-iq.sh`: dump1090 `modes1.bin`, AWEC Porto `recording.bin` (2.0 MS/s, 18.219 s).

| Path | rtf median | notes |
| --- | --- | --- |
| AWEC file-chunk 32768 + resample 2.0→2.4 + mag + demod | **0.0435** | 123 msgs |
| AWEC USB-sized 8192-sample chunks | 0.0459 | |
| AWEC `demodulate2400` on precomputed mag | **0.0196** | |
| dump1090 `modes1.bin` file path | 0.0897 | 332 msgs, 0.178 s capture |
| synthetic 1 s dense DF17 @ 2.4 MS/s | 0.0195 | skip-after-accept hides quiet-search cost |
| 1 s quiet cu8 @ 2.4 convert+mag+demod | 0.0196 | closest live proxy |
| 1 s USB-chunk convert+mag+drain, no demod | 0.0272 | alloc tax |

AWEC whole-pass split: cu8→`Complex<f32>` 0.152 s, resample 0.325 s, `magnitude_u16` 0.329 s, demod 0.323 s. Conversion+mag is about **2/3** of CPU on 2 MS/s files. Live 2.4 path skips resample, still pays convert+mag every sample.

**Pi Zero 2 W estimate (not measured):** 6–12× this host (clock × in-order A53 vs AVX-512 `sqrt`). Quiet live demod ~**0.12–0.24 rtf** if mag is integer LUT, **0.25–0.45 rtf** if `cu8 → f32 → sqrt` stays. dwc2 USB can add 0.2–0.4 of a core. Headroom exists only if conversion is not f32. Label: estimate.

**Hot-loop allocs**

- `radio.rs:55` `magnitude_u16(iq)` new `Vec<u16>` per chunk, then `extend_from_slice`.
- rs1090 `magnitude_u16`: `sqrt` × 65535 per sample, new Vec.
- desperado `convert_bytes_to_complex`: new `Vec<Complex<f32>>`. 8192 USB samples = 64 KiB.
- `demodulate2400` (`src/demod.rs:37-38`) new `Vec<ModeSMessage>` per chunk.
- `rtlsdr.rs:1126` `to_vec()` every URB; `:1148` `Buffer::new(16384)` instead of recycle.
- `mag.drain` vs `copy_within` is noise (~0.13 ms/s).

**USB drop risk**

Demod is synchronous on a current-thread Tokio runtime (`src/main.rs:518-532`) inside `source.next()` (`src/radio.rs:129-149`). USB thread is separate. If demod rtf > 1.0, URBs stop being resubmitted after ~51 ms and the RTL2832U FIFO overflows with no counter (S1). On this Xeon rtf is 0.02–0.05. On the Pi the conversion path is the risk.

**Optimizations**

1. **cu8 → u16 mag LUT** (dump1090 `maglut`). 1 s @ 2 MS/s: f32 path 0.0137 s, LUT 0.00182 s, **7.51×**, max u16 delta 0. 128 KiB RAM. Largest Pi win. Skip `Complex<f32>` on the live path.
2. Reuse one `Vec<u8>` + one mag `Vec<u16>`. Cuts the 0.027 vs 0.020 USB-chunk tax.
3. URB 256 KiB (librtlsdr default): 54.6 ms/URB, 15 in-flight = **819 ms** slack vs 51 ms. Recycle buffers. Count drops.
4. `panic=abort` and clap slim: size, not CPU.
5. CRC 2–3× per candidate (`src/demod.rs:289-304`) is cheap after LUT.

## 5. Feature ideas (value / effort)

1. **Integer mag LUT + rs-rtl live path, no desperado.** High / normal. Pi realtime and binary size.
2. **Ingest `msg_octets` + DF18 CF=0.** High / easy. Squawk 7700 and gliders/vehicles on 1090.
3. **Surface global CPR + optional receiver lat/lon.** Medium / normal. Ground targets at a contest airfield.
4. **USB overrun counter on UART status.** Medium / easy. Know when the Pi is losing IQ.
5. **Emergency idle expiry (dump1090-like).** Medium / easy. Stale 7700 on the satellite payload.
6. **`--fix` input DF gate + `--no-fix` IQ test.** Medium / easy. Stops ghost DF17.
7. **Feature-gated `sim`/`toml` Pi profile.** Low / easy. Size only.
8. Do not add RSSI to the 34-byte TRCK frame. Contest payload is already 28 bytes (`docs/esp32-alert.md`).

## Proposed tickets

1. **Ingest demod `msg_octets` into Tracker** — easy — B1. IQ DF5/DF11 never parse. Prove: unit test DF5 7700 through `demodulate2400` synthetic IQ → UART emergency byte 1.
2. **Gate `correct_pi` to DF11/17/18 inputs** — easy — B2. Prove: DF16 with syndrome `0xC397DB` stays DF16 with `--fix`.
3. **Accept DF18 CF=0 as ADS-B** — easy — B3. Prove: EVEN/ODD ME on DF18 CF=0 emits the same Fix as DF17.
4. **cu8 magnitude LUT, skip `Complex<f32>` on the live path** — normal — Pi CPU. Prove: LUT vs f32 max delta 0 on 256², rtf on `--iq` AWEC and a 1 s 2.4 MS/s quiet buffer, no message-count change.
5. **USB 256 KiB URBs, recycle buffers, increment `dropped`, surface in TACK** — normal — S1. Prove: unit test the counter on a full channel; on Pi, `dropped` stays 0 at 2.4 MS/s idle.
6. **Surface `decodeCPRglobal` (and optional station lat/lon)** — normal — B4. Prove: BDS06 even/odd at a known lat/lon emits a Fix with no prior airborne position.
7. **Pause: set flag and drop waiting under one lock, ignore enqueue while paused** — easy — B9. Prove: concurrent push during Pause leaves `waiting` empty.
8. **Restart ACK `paused=0`; power ACK after `systemctl` or NAK on failure** — easy — B10, B11. Prove: `decide(Restart)` paused false; power path test with a fake `systemctl`.
9. **Queue or NAK in-flight TCMD instead of drop** — easy — B12. Prove: Pause then Status both get TACK.
10. **Command reader: wait on `POLLHUP`/`POLLERR`, respawn or `Stop`** — easy — B13. Prove: PTY hangup does not spin; pause still works after reopen or process stops.
11. **Disable rs1090 `bds-infer` (or raise the cap)** — trivial — B6. Prove: 50 175 ft EVEN/ODD solves.
12. **Drop desperado from `trck` live/IQ path; feature-gate toml sim** — normal — size. Prove: stripped size delta; `cargo test` IQ + radio compile without desperado.
13. **clap slim features + `panic=abort` on release** — easy — size. Prove: two stripped builds.
14. **Expire emergency after idle (e.g. 30 s)** — easy — B8. Prove: STATUS 7700 then only positions after timeout → emergency 0.
15. **Local CPR lon wrap** — easy — B5. Prove: 179.8 then −179.8 unpaired even emits a Fix.
16. **ESP32: block or coalesce TACK when `gQ` is full; retry restart** — easy — B14. Prove: 32 TRCK + one TCMD still prints ack.
17. **doctor: honor `[all]` / model filters in `config.txt`** — easy — B16. Prove: `[pi4] enable_uart=1` does not pass a Zero 2 W.
18. **`--no-fix` vs `--fix` IQ fixture** — trivial — test gap. Prove: 1-bit damaged DF17 IQ decodes only with fix on.

Do not apply EEPROM direct sampling as a drive-by. Do not split `tuner.rs`. Do not unify `.ino` and pio.

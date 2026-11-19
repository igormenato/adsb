//! 2.4 MS/s Mode S demodulator with optional 1-bit PI CRC correction.
//!
//! Preamble search, `slice_phase0`–`slice_phase4`, DF bitset early-out, and
//! the post-accept skip are ported from readsb `demod_2400.c`
//! (wiedehopf/readsb, itself a fork of dump1090-fa).
//! The CRC gate is `score_message` (trck 1-bit PI fix), not readsb
//! `scoreModesMessage`.

use rs1090::source::demod::{MODES_LONG_MSG_BYTES, icao_filter_test, validate_modes_message};

use crate::crc_fix::{aa_bit, correct_pi};

const MIN_TRAILING_SAMPLES: usize = 326;
/// readsb `PREAMBLE_THRESHOLD_DEFAULT`.
const PREAMBLE_THRESHOLD: i32 = 58;
const DF_SHORT: u32 = (1 << 0) | (1 << 4) | (1 << 5) | (1 << 11);
const DF_LONG: u32 = (1 << 16) | (1 << 17) | (1 << 18) | (1 << 20) | (1 << 21);

#[derive(Clone, Debug)]
pub struct ModeSMessage {
    pub msg: [u8; 14],
    pub signal_level: f64,
    pub score: i32,
    pub sample_position: usize,
    pub corrected_bits: u8,
}

/// DF bit 7 of byte 0: long frames are 14 bytes, short frames are 7.
pub fn msg_octets(msg: &[u8; 14]) -> &[u8] {
    if msg[0] & 0x80 != 0 {
        &msg[..14]
    } else {
        &msg[..7]
    }
}

pub fn demodulate2400(samples: &[u16], fix: bool) -> Vec<ModeSMessage> {
    let mut results = Vec::new();
    if samples.len() < MIN_TRAILING_SAMPLES {
        return results;
    }
    let df_long = if fix {
        DF_LONG | df17_one_bit_damage()
    } else {
        DF_LONG
    };
    let last = samples.len() - MIN_TRAILING_SAMPLES;
    let mut j = 0;
    while j < last {
        if j + 16 <= last
            && let Some(window) = samples[j..].first_chunk::<31>()
        {
            let mut mask = preamble_mask16(window);
            if mask == 0 {
                j += 16;
                continue;
            }
            let mut skip_to = j + 16;
            while mask != 0 {
                let k = mask.trailing_zeros() as usize;
                mask &= mask - 1;
                let idx = j + k;
                if let Some(msg) = decode_at(&samples[idx..], idx, df_long, fix) {
                    skip_to = idx + skip_after(&msg);
                    results.push(msg);
                    break;
                }
            }
            j = skip_to;
            continue;
        }
        if !preamble_quick(samples, j) {
            j += 1;
            continue;
        }
        if let Some(msg) = decode_at(&samples[j..], j, df_long, fix) {
            j += skip_after(&msg);
            results.push(msg);
        } else {
            j += 1;
        }
    }
    results
}

fn skip_after(msg: &ModeSMessage) -> usize {
    // readsb: `pa += msglen * 8 / 4` (msglen in bits), then the loop's +1.
    let bits = if msg.msg[0] >> 3 >= 16 { 112 } else { 56 };
    bits * 2 + 1
}

/// 16 consecutive preamble quick-checks as a bitmask (bit k → offset k).
/// `&` not `&&` so LLVM emits SSE2/NEON compares instead of scalar branches.
fn preamble_mask16(window: &[u16; 31]) -> u16 {
    let mut mask = 0u16;
    for k in 0..16 {
        let pass = (window[k + 1] > window[k + 7])
            & (window[k + 12] > window[k + 14])
            & (window[k + 12] > window[k + 15]);
        mask |= u16::from(pass) << k;
    }
    mask
}

fn preamble_quick(samples: &[u16], j: usize) -> bool {
    (samples[j + 1] > samples[j + 7])
        & (samples[j + 12] > samples[j + 14])
        & (samples[j + 12] > samples[j + 15])
}

fn decode_at(pa: &[u16], sample_position: usize, df_long: u32, fix: bool) -> Option<ModeSMessage> {
    let base_noise = i32::from(pa[5])
        + i32::from(pa[8])
        + i32::from(pa[16])
        + i32::from(pa[17])
        + i32::from(pa[18]);
    let ref_level = (base_noise * PREAMBLE_THRESHOLD) >> 5;
    let mut best_msg = [0_u8; MODES_LONG_MSG_BYTES];
    let mut best_score = -42;
    let mut best_corrected = 0_u8;
    let diff_2_3 = i32::from(pa[2]) - i32::from(pa[3]);
    let sum_1_4 = i32::from(pa[1]) + i32::from(pa[4]);
    let diff_10_11 = i32::from(pa[10]) - i32::from(pa[11]);
    let common = sum_1_4 - diff_2_3 + i32::from(pa[9]) + i32::from(pa[12]);
    if common - diff_10_11 >= ref_level {
        score_phase(
            4,
            pa,
            df_long,
            fix,
            &mut best_msg,
            &mut best_score,
            &mut best_corrected,
        );
        score_phase(
            5,
            pa,
            df_long,
            fix,
            &mut best_msg,
            &mut best_score,
            &mut best_corrected,
        );
    }
    if common + diff_10_11 >= ref_level {
        score_phase(
            6,
            pa,
            df_long,
            fix,
            &mut best_msg,
            &mut best_score,
            &mut best_corrected,
        );
        score_phase(
            7,
            pa,
            df_long,
            fix,
            &mut best_msg,
            &mut best_score,
            &mut best_corrected,
        );
    }
    if sum_1_4 + 2 * diff_2_3 + diff_10_11 + i32::from(pa[12]) >= ref_level {
        score_phase(
            8,
            pa,
            df_long,
            fix,
            &mut best_msg,
            &mut best_score,
            &mut best_corrected,
        );
    }
    if best_score < 0 {
        return None;
    }
    let bits: usize = if best_msg[0] >> 3 >= 16 { 112 } else { 56 };
    let signal_len = bits * 12 / 5;
    let mut scaled_signal_power = 0_u64;
    for mag in pa[19..19 + signal_len].iter().copied() {
        let mag = u64::from(mag);
        scaled_signal_power += mag * mag;
    }
    let signal_power = scaled_signal_power as f64 / 65535.0 / 65535.0;
    Some(ModeSMessage {
        msg: best_msg,
        signal_level: signal_power / signal_len as f64,
        score: best_score,
        sample_position,
        corrected_bits: best_corrected,
    })
}

fn df17_one_bit_damage() -> u32 {
    let df = 17u32;
    let mut bits = 1 << df;
    for bit in 0..5 {
        bits |= 1 << (df ^ (1 << bit));
    }
    bits
}

fn score_phase(
    try_phase: usize,
    pa: &[u16],
    df_long: u32,
    fix: bool,
    best_msg: &mut [u8; 14],
    best_score: &mut i32,
    best_corrected: &mut u8,
) {
    let mut offset = 19 + try_phase / 5;
    let mut phase = try_phase % 5;
    let mut msg = [0_u8; MODES_LONG_MSG_BYTES];
    msg[0] = slice_byte(pa, &mut offset, &mut phase);
    let df = u32::from(msg[0] >> 3);
    let bytelen = if df_long & (1 << df) != 0 {
        14
    } else if DF_SHORT & (1 << df) != 0 {
        7
    } else {
        if -2 > *best_score {
            *best_score = -2;
        }
        return;
    };
    for slot in msg.iter_mut().take(bytelen).skip(1) {
        *slot = slice_byte(pa, &mut offset, &mut phase);
    }
    let (score, corrected) = score_message(&mut msg, fix);
    if score > *best_score {
        *best_msg = msg;
        *best_score = score;
        *best_corrected = corrected;
    }
}

fn slice_byte(m: &[u16], offset: &mut usize, phase: &mut usize) -> u8 {
    let p = *offset;
    let (byte, next_phase, advance) = match *phase {
        0 => (
            bit(slice_phase0(&m[p..]), 0x80)
                | bit(slice_phase2(&m[p + 2..]), 0x40)
                | bit(slice_phase4(&m[p + 4..]), 0x20)
                | bit(slice_phase1(&m[p + 7..]), 0x10)
                | bit(slice_phase3(&m[p + 9..]), 0x08)
                | bit(slice_phase0(&m[p + 12..]), 0x04)
                | bit(slice_phase2(&m[p + 14..]), 0x02)
                | bit(slice_phase4(&m[p + 16..]), 0x01),
            1,
            19,
        ),
        1 => (
            bit(slice_phase1(&m[p..]), 0x80)
                | bit(slice_phase3(&m[p + 2..]), 0x40)
                | bit(slice_phase0(&m[p + 5..]), 0x20)
                | bit(slice_phase2(&m[p + 7..]), 0x10)
                | bit(slice_phase4(&m[p + 9..]), 0x08)
                | bit(slice_phase1(&m[p + 12..]), 0x04)
                | bit(slice_phase3(&m[p + 14..]), 0x02)
                | bit(slice_phase0(&m[p + 17..]), 0x01),
            2,
            19,
        ),
        2 => (
            bit(slice_phase2(&m[p..]), 0x80)
                | bit(slice_phase4(&m[p + 2..]), 0x40)
                | bit(slice_phase1(&m[p + 5..]), 0x20)
                | bit(slice_phase3(&m[p + 7..]), 0x10)
                | bit(slice_phase0(&m[p + 10..]), 0x08)
                | bit(slice_phase2(&m[p + 12..]), 0x04)
                | bit(slice_phase4(&m[p + 14..]), 0x02)
                | bit(slice_phase1(&m[p + 17..]), 0x01),
            3,
            19,
        ),
        3 => (
            bit(slice_phase3(&m[p..]), 0x80)
                | bit(slice_phase0(&m[p + 3..]), 0x40)
                | bit(slice_phase2(&m[p + 5..]), 0x20)
                | bit(slice_phase4(&m[p + 7..]), 0x10)
                | bit(slice_phase1(&m[p + 10..]), 0x08)
                | bit(slice_phase3(&m[p + 12..]), 0x04)
                | bit(slice_phase0(&m[p + 15..]), 0x02)
                | bit(slice_phase2(&m[p + 17..]), 0x01),
            4,
            19,
        ),
        _ => (
            bit(slice_phase4(&m[p..]), 0x80)
                | bit(slice_phase1(&m[p + 3..]), 0x40)
                | bit(slice_phase3(&m[p + 5..]), 0x20)
                | bit(slice_phase0(&m[p + 8..]), 0x10)
                | bit(slice_phase2(&m[p + 10..]), 0x08)
                | bit(slice_phase4(&m[p + 12..]), 0x04)
                | bit(slice_phase1(&m[p + 15..]), 0x02)
                | bit(slice_phase3(&m[p + 17..]), 0x01),
            0,
            20,
        ),
    };
    *phase = next_phase;
    *offset += advance;
    byte
}

fn bit(corr: i32, mask: u8) -> u8 {
    if corr > 0 { mask } else { 0 }
}

fn slice_phase0(m: &[u16]) -> i32 {
    18 * i32::from(m[0]) - 15 * i32::from(m[1]) - 3 * i32::from(m[2])
}

fn slice_phase1(m: &[u16]) -> i32 {
    14 * i32::from(m[0]) - 5 * i32::from(m[1]) - 9 * i32::from(m[2])
}

fn slice_phase2(m: &[u16]) -> i32 {
    16 * i32::from(m[0]) + 5 * i32::from(m[1]) - 20 * i32::from(m[2])
}

fn slice_phase3(m: &[u16]) -> i32 {
    7 * i32::from(m[0]) + 11 * i32::from(m[1]) - 18 * i32::from(m[2])
}

fn slice_phase4(m: &[u16]) -> i32 {
    4 * i32::from(m[0]) + 15 * i32::from(m[1]) - 20 * i32::from(m[2]) + i32::from(m[3])
}

fn addr(msg: &[u8]) -> u32 {
    u32::from(msg[1]) << 16 | u32::from(msg[2]) << 8 | u32::from(msg[3])
}

fn score_message(msg: &mut [u8; 14], fix: bool) -> (i32, u8) {
    if !fix {
        return (validate_modes_message(msg), 0);
    }
    match correct_pi(msg) {
        None => (validate_modes_message(msg), 0),
        Some(correction) => {
            let df = msg[0] >> 3;
            let known = icao_filter_test(addr(msg));
            if df == 11 && !known {
                return (-2, 0);
            }
            if matches!(df, 17 | 18) && aa_bit(correction.bit) && !known {
                return (-2, 0);
            }
            (validate_modes_message(msg), 1)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn df11_is_seven_bytes_and_df17_is_fourteen() {
        let mut msg = [0_u8; 14];
        msg[0] = 0x58;
        assert_eq!(msg_octets(&msg).len(), 7);
        msg[0] = 0x8D;
        assert_eq!(msg_octets(&msg).len(), 14);
    }

    fn scalar_quick(samples: &[u16], j: usize) -> bool {
        samples[j + 1] > samples[j + 7]
            && samples[j + 12] > samples[j + 14]
            && samples[j + 12] > samples[j + 15]
    }

    fn assert_mask16(samples: &[u16], j: usize) {
        let window: &[u16; 31] = samples[j..j + 31].try_into().unwrap();
        let mask = preamble_mask16(window);
        for k in 0..16 {
            assert_eq!(
                mask & (1 << k) != 0,
                scalar_quick(samples, j + k),
                "j={j} k={k}"
            );
        }
    }

    #[test]
    fn preamble_mask16_matches_scalar_quick_check() {
        let mut mag = vec![0u16; 64];
        let mut seed = 0x9E37_79B9_u32;
        for _ in 0..200 {
            for slot in mag.iter_mut() {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                *slot = (seed >> 16) as u16;
            }
            assert_mask16(&mag, 0);
        }

        mag.fill(100);
        assert_mask16(&mag, 0);
        mag[1] = 101;
        mag[7] = 101;
        mag[12] = 200;
        mag[14] = 50;
        mag[15] = 50;
        assert_mask16(&mag, 0);
        mag[1] = 200;
        mag[7] = 50;
        mag[12] = 100;
        mag[14] = 100;
        mag[15] = 50;
        assert_mask16(&mag, 0);
        mag[14] = 50;
        mag[15] = 100;
        assert_mask16(&mag, 0);

        mag.resize(MIN_TRAILING_SAMPLES + 20, 0);
        seed = 1;
        for slot in mag.iter_mut() {
            seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
            *slot = (seed >> 8) as u16;
        }
        let last = mag.len() - MIN_TRAILING_SAMPLES;
        let mut j = 0;
        while j + 16 <= last {
            assert_mask16(&mag, j);
            j += 16;
        }
        while j < last {
            assert_eq!(preamble_quick(&mag, j), scalar_quick(&mag, j), "tail j={j}");
            j += 1;
        }
        let tail = last - 1;
        mag[tail + 1] = mag[tail + 7];
        mag[tail + 12] = mag[tail + 14].max(mag[tail + 15]);
        assert_eq!(
            preamble_quick(&mag, tail),
            scalar_quick(&mag, tail),
            "j=last-1"
        );
    }
}

//! 2.4 MS/s Mode S demodulator with optional PI CRC correction.
//!
//! Preamble search, `slice_phase0`–`slice_phase4`, DF bitset early-out, and
//! the post-accept skip are ported from readsb `demod_2400.c`
//! (wiedehopf/readsb, itself a fork of dump1090-fa).
//! The CRC gate is `score_message` (trck 1-bit PI fix, gated 2-bit DF17/18),
//! not readsb `scoreModesMessage`.

use rs1090::source::demod::{MODES_LONG_MSG_BYTES, icao_filter_test, validate_modes_message};

use crate::crc_fix::{
    Correction, aa_bit, correct_pi_syndrome, lookup_two_bit, prepare_tables, try_two_bit,
};

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
    if fix {
        prepare_tables();
    }
    let df_long = if fix {
        DF_LONG | df17_one_bit_damage()
    } else {
        DF_LONG
    };
    let last = samples.len() - MIN_TRAILING_SAMPLES;
    let mut j = 0;
    while j < last {
        if !(samples[j + 1] > samples[j + 7]
            && samples[j + 12] > samples[j + 14]
            && samples[j + 12] > samples[j + 15])
        {
            j += 1;
            continue;
        }
        let pa = &samples[j..];
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
            j += 1;
            continue;
        }
        let bits: usize = if best_msg[0] >> 3 >= 16 { 112 } else { 56 };
        let signal_len = bits * 12 / 5;
        let mut scaled_signal_power = 0_u64;
        for k in 0..signal_len {
            let mag = u64::from(pa[19 + k]);
            scaled_signal_power += mag * mag;
        }
        let signal_power = scaled_signal_power as f64 / 65535.0 / 65535.0;
        results.push(ModeSMessage {
            msg: best_msg,
            signal_level: signal_power / signal_len as f64,
            score: best_score,
            sample_position: j,
            corrected_bits: best_corrected,
        });
        // readsb: `pa += msglen * 8 / 4` (msglen in bits).
        j += bits * 2;
        j += 1;
    }
    results
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
    msg[0] = slice_byte(pa, &mut offset, &mut phase, None);
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
        *slot = slice_byte(pa, &mut offset, &mut phase, None);
    }
    let (mut score, mut corrected, syn) = score_message(&mut msg, fix);
    if fix
        && bytelen == 14
        && corrected == 0
        && syn != 0
        && matches!(msg[0] >> 3, 17 | 18)
        && lookup_two_bit(syn).is_some()
    {
        let mut conf = [0_i32; 112];
        let mut nbits = 0usize;
        let mut off = 19 + try_phase / 5;
        let mut ph = try_phase % 5;
        for slot in msg.iter_mut().take(14) {
            *slot = slice_byte(pa, &mut off, &mut ph, Some((&mut conf, &mut nbits)));
        }
        if let Some(fix2) = try_two_bit(&mut msg, syn, &conf[..nbits]) {
            let (s, c) = score_fixed(&mut msg, fix2);
            score = s;
            corrected = c;
        }
    }
    if score > *best_score {
        *best_msg = msg;
        *best_score = score;
        *best_corrected = corrected;
    }
}

fn slice_byte(
    m: &[u16],
    offset: &mut usize,
    phase: &mut usize,
    mut conf: Option<(&mut [i32; 112], &mut usize)>,
) -> u8 {
    let p = *offset;
    let mut bit = |corr: i32, mask: u8| -> u8 {
        if let Some((mags, i)) = conf.as_mut()
            && **i < mags.len()
        {
            mags[**i] = corr.saturating_abs();
            **i += 1;
        }
        if corr > 0 { mask } else { 0 }
    };
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

fn score_message(msg: &mut [u8; 14], fix: bool) -> (i32, u8, u32) {
    if !fix {
        return (validate_modes_message(msg), 0, 0);
    }
    let (correction, long) = correct_pi_syndrome(msg);
    match correction {
        None => (validate_modes_message(msg), 0, long),
        Some(correction) => {
            let (score, nbits) = score_fixed(msg, correction);
            (score, nbits, 0)
        }
    }
}

fn score_fixed(msg: &mut [u8; 14], correction: Correction) -> (i32, u8) {
    let df = msg[0] >> 3;
    let known = icao_filter_test(addr(msg));
    if df == 11 && !known {
        return (-2, 0);
    }
    let aa = aa_bit(correction.bit) || correction.extra.is_some_and(aa_bit);
    if matches!(df, 17 | 18) && aa && !known {
        return (-2, 0);
    }
    let nbits = if correction.extra.is_some() { 2 } else { 1 };
    (validate_modes_message(msg), nbits)
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
}

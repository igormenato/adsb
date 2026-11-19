//! 2.4 MS/s Mode S demodulator with optional 1-bit PI CRC correction.
//!
//! Preamble search, `slice_phase0`–`slice_phase4`, DF bitset early-out, and
//! the post-accept skip are ported from readsb `demod_2400.c`
//! (wiedehopf/readsb, itself a fork of dump1090-fa).
//! The CRC gate is `score_message` (trck 1-bit PI fix), not readsb
//! `scoreModesMessage`.

use rs1090::source::demod::{MODES_LONG_MSG_BYTES, icao_filter_test, validate_modes_message};

use crate::crc_fix::{FixPolicy, aa_bit, correct_pi, correct_with};

const MIN_TRAILING_SAMPLES: usize = 326;
/// readsb `PREAMBLE_THRESHOLD_DEFAULT`.
const PREAMBLE_THRESHOLD: i32 = 58;
const DF_SHORT: u32 = (1 << 0) | (1 << 4) | (1 << 5) | (1 << 11);
const DF_LONG: u32 = (1 << 16) | (1 << 17) | (1 << 18) | (1 << 20) | (1 << 21);
/// Soft-confidence gate: 1-bit (and 2-bit) repair only if flipped bits are among these.
const CONF_N: u8 = 10;
/// 20th percentile of magnitude over the capture (quiet-sample noise floor).
const NOISE_PERCENTILE: f64 = 0.20;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DemodOpts {
    pub fix: bool,
    pub conf_n: Option<u8>,
    pub two_bit: bool,
    pub adaptive_preamble: bool,
}

impl DemodOpts {
    pub fn baseline(fix: bool) -> Self {
        Self {
            fix,
            conf_n: None,
            two_bit: false,
            adaptive_preamble: false,
        }
    }

    pub fn soft_conf(fix: bool) -> Self {
        Self {
            fix,
            conf_n: Some(CONF_N),
            two_bit: false,
            adaptive_preamble: false,
        }
    }

    pub fn two_bit(fix: bool) -> Self {
        Self {
            fix,
            conf_n: Some(CONF_N),
            two_bit: true,
            adaptive_preamble: false,
        }
    }

    pub fn adaptive(fix: bool) -> Self {
        Self {
            fix,
            conf_n: None,
            two_bit: false,
            adaptive_preamble: true,
        }
    }

    fn policy(self) -> FixPolicy {
        FixPolicy {
            one_bit: self.fix,
            two_bit: self.fix && self.two_bit,
            conf_n: if self.fix { self.conf_n } else { None },
        }
    }
}

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
    demodulate2400_opts(samples, DemodOpts::baseline(fix))
}

pub fn demodulate2400_opts(samples: &[u16], opts: DemodOpts) -> Vec<ModeSMessage> {
    let mut results = Vec::new();
    if samples.len() < MIN_TRAILING_SAMPLES {
        return results;
    }
    let df_long = if opts.fix {
        DF_LONG | df17_one_bit_damage()
    } else {
        DF_LONG
    };
    let noise_floor = if opts.adaptive_preamble {
        mag_percentile(samples, NOISE_PERCENTILE)
    } else {
        0
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
        let mut base_noise = i32::from(pa[5])
            + i32::from(pa[8])
            + i32::from(pa[16])
            + i32::from(pa[17])
            + i32::from(pa[18]);
        if opts.adaptive_preamble {
            base_noise = base_noise.max(noise_floor.saturating_mul(5));
        }
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
                opts,
                &mut best_msg,
                &mut best_score,
                &mut best_corrected,
            );
            score_phase(
                5,
                pa,
                df_long,
                opts,
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
                opts,
                &mut best_msg,
                &mut best_score,
                &mut best_corrected,
            );
            score_phase(
                7,
                pa,
                df_long,
                opts,
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
                opts,
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

fn mag_percentile(samples: &[u16], p: f64) -> i32 {
    let mut bins = [0u32; 256];
    let mut n = 0u32;
    for &s in samples.iter().step_by(8) {
        bins[(s >> 8) as usize] += 1;
        n += 1;
    }
    if n == 0 {
        return 0;
    }
    let target = ((p.clamp(0.0, 1.0) * f64::from(n)) as u32).min(n.saturating_sub(1));
    let mut acc = 0u32;
    for (i, &c) in bins.iter().enumerate() {
        acc += c;
        if acc > target {
            return (i as i32) << 8;
        }
    }
    0
}

fn score_phase(
    try_phase: usize,
    pa: &[u16],
    df_long: u32,
    opts: DemodOpts,
    best_msg: &mut [u8; 14],
    best_score: &mut i32,
    best_corrected: &mut u8,
) {
    let mut offset = 19 + try_phase / 5;
    let mut phase = try_phase % 5;
    let mut msg = [0_u8; MODES_LONG_MSG_BYTES];
    let mut conf = [0_i32; 112];
    let mut bit = 0usize;
    msg[0] = slice_byte(pa, &mut offset, &mut phase, &mut conf, &mut bit);
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
        *slot = slice_byte(pa, &mut offset, &mut phase, &mut conf, &mut bit);
    }
    let (score, corrected) = score_message(&mut msg, opts, &conf[..bit]);
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
    conf: &mut [i32; 112],
    bit_i: &mut usize,
) -> u8 {
    let p = *offset;
    let (byte, next_phase, advance) = match *phase {
        0 => (
            bit(slice_phase0(&m[p..]), 0x80, conf, bit_i)
                | bit(slice_phase2(&m[p + 2..]), 0x40, conf, bit_i)
                | bit(slice_phase4(&m[p + 4..]), 0x20, conf, bit_i)
                | bit(slice_phase1(&m[p + 7..]), 0x10, conf, bit_i)
                | bit(slice_phase3(&m[p + 9..]), 0x08, conf, bit_i)
                | bit(slice_phase0(&m[p + 12..]), 0x04, conf, bit_i)
                | bit(slice_phase2(&m[p + 14..]), 0x02, conf, bit_i)
                | bit(slice_phase4(&m[p + 16..]), 0x01, conf, bit_i),
            1,
            19,
        ),
        1 => (
            bit(slice_phase1(&m[p..]), 0x80, conf, bit_i)
                | bit(slice_phase3(&m[p + 2..]), 0x40, conf, bit_i)
                | bit(slice_phase0(&m[p + 5..]), 0x20, conf, bit_i)
                | bit(slice_phase2(&m[p + 7..]), 0x10, conf, bit_i)
                | bit(slice_phase4(&m[p + 9..]), 0x08, conf, bit_i)
                | bit(slice_phase1(&m[p + 12..]), 0x04, conf, bit_i)
                | bit(slice_phase3(&m[p + 14..]), 0x02, conf, bit_i)
                | bit(slice_phase0(&m[p + 17..]), 0x01, conf, bit_i),
            2,
            19,
        ),
        2 => (
            bit(slice_phase2(&m[p..]), 0x80, conf, bit_i)
                | bit(slice_phase4(&m[p + 2..]), 0x40, conf, bit_i)
                | bit(slice_phase1(&m[p + 5..]), 0x20, conf, bit_i)
                | bit(slice_phase3(&m[p + 7..]), 0x10, conf, bit_i)
                | bit(slice_phase0(&m[p + 10..]), 0x08, conf, bit_i)
                | bit(slice_phase2(&m[p + 12..]), 0x04, conf, bit_i)
                | bit(slice_phase4(&m[p + 14..]), 0x02, conf, bit_i)
                | bit(slice_phase1(&m[p + 17..]), 0x01, conf, bit_i),
            3,
            19,
        ),
        3 => (
            bit(slice_phase3(&m[p..]), 0x80, conf, bit_i)
                | bit(slice_phase0(&m[p + 3..]), 0x40, conf, bit_i)
                | bit(slice_phase2(&m[p + 5..]), 0x20, conf, bit_i)
                | bit(slice_phase4(&m[p + 7..]), 0x10, conf, bit_i)
                | bit(slice_phase1(&m[p + 10..]), 0x08, conf, bit_i)
                | bit(slice_phase3(&m[p + 12..]), 0x04, conf, bit_i)
                | bit(slice_phase0(&m[p + 15..]), 0x02, conf, bit_i)
                | bit(slice_phase2(&m[p + 17..]), 0x01, conf, bit_i),
            4,
            19,
        ),
        _ => (
            bit(slice_phase4(&m[p..]), 0x80, conf, bit_i)
                | bit(slice_phase1(&m[p + 3..]), 0x40, conf, bit_i)
                | bit(slice_phase3(&m[p + 5..]), 0x20, conf, bit_i)
                | bit(slice_phase0(&m[p + 8..]), 0x10, conf, bit_i)
                | bit(slice_phase2(&m[p + 10..]), 0x08, conf, bit_i)
                | bit(slice_phase4(&m[p + 12..]), 0x04, conf, bit_i)
                | bit(slice_phase1(&m[p + 15..]), 0x02, conf, bit_i)
                | bit(slice_phase3(&m[p + 17..]), 0x01, conf, bit_i),
            0,
            20,
        ),
    };
    *phase = next_phase;
    *offset += advance;
    byte
}

fn bit(corr: i32, mask: u8, conf: &mut [i32; 112], i: &mut usize) -> u8 {
    if *i < conf.len() {
        conf[*i] = corr.saturating_abs();
        *i += 1;
    }
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

fn score_message(msg: &mut [u8; 14], opts: DemodOpts, conf: &[i32]) -> (i32, u8) {
    if !opts.fix {
        return (validate_modes_message(msg), 0);
    }
    let correction = if opts.two_bit || opts.conf_n.is_some() {
        correct_with(msg, opts.policy(), Some(conf))
    } else {
        correct_pi(msg)
    };
    match correction {
        None => (validate_modes_message(msg), 0),
        Some(correction) => {
            let df = msg[0] >> 3;
            let known = icao_filter_test(addr(msg));
            if df == 11 && !known {
                return (-2, 0);
            }
            let aa = aa_bit(correction.bit) || correction.extra.is_some_and(aa_bit);
            if matches!(df, 17 | 18) && aa && !known {
                return (-2, 0);
            }
            (validate_modes_message(msg), correction.nbits)
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
}

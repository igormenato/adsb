//! dump1090-2400 demodulation with optional 1-bit PI CRC correction.
//!
//! Derived from rs1090 0.8.1 `src/source/demod/demod2400.rs` under MIT.
//! Copyright (c) Xavier Olive.
//! The CRC gate goes through `score_message` instead of calling `validate_modes_message` on the raw bits.

use rs1090::source::demod::{MODES_LONG_MSG_BYTES, icao_filter_test, validate_modes_message};

use crate::crc_fix::{aa_bit, correct_pi};

const MIN_TRAILING_SAMPLES: usize = 326;

#[derive(Clone, Debug)]
pub struct ModeSMessage {
    pub msg: [u8; 14],
    pub signal_level: f64,
    pub score: i32,
    pub sample_position: usize,
    pub corrected_bits: u8,
}

pub fn demodulate2400(samples: &[u16], fix: bool) -> Vec<ModeSMessage> {
    let mut results = Vec::new();
    if samples.len() < MIN_TRAILING_SAMPLES {
        return results;
    }

    // dump1090-fa `demod_2400.c`: after an accepted message, skip to 8 bits
    // before its end so a second preamble that clobbers the last bits can
    // still decode, without scoring every overlapping sample of this frame.
    let mut j = 0;
    let last = samples.len() - MIN_TRAILING_SAMPLES;
    while j < last {
        let Some((high, base_signal, base_noise)) = check_preamble(&samples[j..j + 14]) else {
            j += 1;
            continue;
        };
        if base_signal * 2 < 3 * base_noise {
            j += 1;
            continue;
        }
        if i32::from(samples[j + 5]) >= high
            || i32::from(samples[j + 6]) >= high
            || i32::from(samples[j + 7]) >= high
            || i32::from(samples[j + 8]) >= high
            || i32::from(samples[j + 14]) >= high
            || i32::from(samples[j + 15]) >= high
            || i32::from(samples[j + 16]) >= high
            || i32::from(samples[j + 17]) >= high
            || i32::from(samples[j + 18]) >= high
        {
            j += 1;
            continue;
        }

        let mut bestmsg = ModeSMessage {
            msg: [0_u8; MODES_LONG_MSG_BYTES],
            signal_level: 0.,
            score: -2,
            sample_position: j,
            corrected_bits: 0,
        };
        let mut msg = [0_u8; MODES_LONG_MSG_BYTES];

        for try_phase in 4..9 {
            let mut slice_loc: usize = j + 19 + (try_phase / 5);
            let mut phase = Phase::from(try_phase);

            for slot in msg.iter_mut().take(MODES_LONG_MSG_BYTES) {
                let slice_this_byte: &[u16] = &samples[slice_loc..];
                let starting_phase = phase;
                let mut the_byte = 0x00;
                let mut index = 0;
                for i in 0..8 {
                    if phase.calculate_bit(&slice_this_byte[index..index + 4]) > 0 {
                        the_byte |= 1 << (7 - i);
                    }
                    index = phase.increment_index(index);
                    phase = phase.next();
                }
                *slot = the_byte;
                slice_loc += index;
                phase = starting_phase.next_start();
            }

            let (score, corrected_bits) = score_message(&mut msg, fix);
            if score > bestmsg.score {
                bestmsg.msg.clone_from_slice(&msg);
                bestmsg.score = score;
                bestmsg.corrected_bits = corrected_bits;
                let mut scaled_signal_power = 0_u64;
                let signal_len = msg.len() * 12 / 5;
                for k in 0..signal_len {
                    let mag = samples[j + 19 + k] as u64;
                    scaled_signal_power += mag * mag;
                }
                let signal_power = scaled_signal_power as f64 / 65535.0 / 65535.0;
                bestmsg.signal_level = signal_power / signal_len as f64;
            }
        }

        if bestmsg.score >= 0 {
            let bits: usize = if bestmsg.msg[0] >> 3 >= 16 { 112 } else { 56 };
            results.push(bestmsg);
            j += bits * 12 / 5;
        }
        j += 1;
    }

    results
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

fn check_preamble(preamble: &[u16]) -> Option<(i32, u32, u32)> {
    debug_assert_eq!(preamble.len(), 14);

    if !(preamble[0] < preamble[1] && preamble[12] > preamble[13]) {
        return None;
    }

    if preamble[1] > preamble[2]
        && preamble[2] < preamble[3]
        && preamble[3] > preamble[4]
        && preamble[8] < preamble[9]
        && preamble[9] > preamble[10]
        && preamble[10] < preamble[11]
    {
        let high = (i32::from(preamble[1])
            + i32::from(preamble[3])
            + i32::from(preamble[9])
            + i32::from(preamble[11])
            + i32::from(preamble[12]))
            / 4;
        let base_signal = u32::from(preamble[1]) + u32::from(preamble[3]) + u32::from(preamble[9]);
        let base_noise = u32::from(preamble[5]) + u32::from(preamble[6]) + u32::from(preamble[7]);
        Some((high, base_signal, base_noise))
    } else if preamble[1] > preamble[2]
        && preamble[2] < preamble[3]
        && preamble[3] > preamble[4]
        && preamble[8] < preamble[9]
        && preamble[9] > preamble[10]
        && preamble[11] < preamble[12]
    {
        let high = (i32::from(preamble[1])
            + i32::from(preamble[3])
            + i32::from(preamble[9])
            + i32::from(preamble[12]))
            / 4;
        let base_signal = u32::from(preamble[1])
            + u32::from(preamble[3])
            + u32::from(preamble[9])
            + u32::from(preamble[12]);
        let base_noise = u32::from(preamble[5])
            + u32::from(preamble[6])
            + u32::from(preamble[7])
            + u32::from(preamble[8]);
        Some((high, base_signal, base_noise))
    } else if preamble[1] > preamble[2]
        && preamble[2] < preamble[3]
        && preamble[4] > preamble[5]
        && preamble[8] < preamble[9]
        && preamble[10] > preamble[11]
        && preamble[11] < preamble[12]
    {
        let high = (i32::from(preamble[1])
            + i32::from(preamble[3])
            + i32::from(preamble[4])
            + i32::from(preamble[9])
            + i32::from(preamble[10])
            + i32::from(preamble[12]))
            / 4;
        let base_signal = u32::from(preamble[1]) + u32::from(preamble[12]);
        let base_noise = u32::from(preamble[6]) + u32::from(preamble[7]);
        Some((high, base_signal, base_noise))
    } else if preamble[1] > preamble[2]
        && preamble[3] < preamble[4]
        && preamble[4] > preamble[5]
        && preamble[9] < preamble[10]
        && preamble[10] > preamble[11]
        && preamble[11] < preamble[12]
    {
        let high = (i32::from(preamble[1])
            + i32::from(preamble[4])
            + i32::from(preamble[10])
            + i32::from(preamble[12]))
            / 4;
        let base_signal = u32::from(preamble[1])
            + u32::from(preamble[4])
            + u32::from(preamble[10])
            + u32::from(preamble[12]);
        let base_noise = u32::from(preamble[5])
            + u32::from(preamble[6])
            + u32::from(preamble[7])
            + u32::from(preamble[8]);
        Some((high, base_signal, base_noise))
    } else if preamble[2] > preamble[3]
        && preamble[3] < preamble[4]
        && preamble[4] > preamble[5]
        && preamble[9] < preamble[10]
        && preamble[10] > preamble[11]
        && preamble[11] < preamble[12]
    {
        let high = (i32::from(preamble[1])
            + i32::from(preamble[2])
            + i32::from(preamble[4])
            + i32::from(preamble[10])
            + i32::from(preamble[12]))
            / 4;
        let base_signal =
            u32::from(preamble[4]) + u32::from(preamble[10]) + u32::from(preamble[12]);
        let base_noise = u32::from(preamble[6]) + u32::from(preamble[7]) + u32::from(preamble[8]);
        Some((high, base_signal, base_noise))
    } else {
        None
    }
}

#[derive(Clone, Copy, Debug)]
enum Phase {
    Zero,
    One,
    Two,
    Three,
    Four,
}

impl From<usize> for Phase {
    fn from(num: usize) -> Self {
        match num % 5 {
            0 => Self::Zero,
            1 => Self::One,
            2 => Self::Two,
            3 => Self::Three,
            _ => Self::Four,
        }
    }
}

impl Phase {
    fn next_start(self) -> Self {
        match self {
            Self::Zero => Self::One,
            Self::One => Self::Two,
            Self::Two => Self::Three,
            Self::Three => Self::Four,
            Self::Four => Self::Zero,
        }
    }

    fn next(self) -> Self {
        match self {
            Self::Zero => Self::Two,
            Self::Two => Self::Four,
            Self::Four => Self::One,
            Self::One => Self::Three,
            Self::Three => Self::Zero,
        }
    }

    fn increment_index(self, index: usize) -> usize {
        index
            + match self {
                Self::Zero | Self::Two | Self::One => 2,
                Self::Four | Self::Three => 3,
            }
    }

    fn calculate_bit(self, m: &[u16]) -> i32 {
        let m0 = i32::from(m[0]);
        let m1 = i32::from(m[1]);
        let m2 = i32::from(m[2]);
        match self {
            Self::Zero => 5 * m0 - 3 * m1 - 2 * m2,
            Self::One => 4 * m0 - m1 - 3 * m2,
            Self::Two => 3 * m0 + m1 - 4 * m2,
            Self::Three => 2 * m0 + 3 * m1 - 5 * m2,
            Self::Four => m0 + 5 * m1 - 5 * m2 - i32::from(m[3]),
        }
    }
}

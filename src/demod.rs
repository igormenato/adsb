//! Mode S demodulation with optional 1-bit PI CRC correction.
//!
//! `demodulate2400` is derived from rs1090 0.8.1 `demod2400.rs` (MIT,
//! Copyright (c) Xavier Olive).
//! `demodulate2000` is derived from dump1090 `detectModeS` (BSD-3-Clause,
//! Copyright (C) 2012 Salvatore Sanfilippo).
//! Both CRC gates go through `score_message` instead of scoring raw bits.

use rs1090::source::demod::{MODES_LONG_MSG_BYTES, icao_filter_test, validate_modes_message};

use crate::crc_fix::{aa_bit, correct_pi};

const MIN_TRAILING_SAMPLES: usize = 326;
const PREAMBLE_US: usize = 8;
const LONG_MSG_BITS: usize = 112;
const SAMPLES_PER_US_2000: usize = 2;
const MIN_TRAILING_2000: usize = (PREAMBLE_US + LONG_MSG_BITS) * SAMPLES_PER_US_2000;
const DATA_SAMPLES_2000: usize = LONG_MSG_BITS * SAMPLES_PER_US_2000;
const PREAMBLE_SAMPLES_2000: usize = PREAMBLE_US * SAMPLES_PER_US_2000;

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

    for j in 0..(samples.len() - MIN_TRAILING_SAMPLES) {
        let Some((high, base_signal, base_noise)) = check_preamble(&samples[j..j + 14]) else {
            continue;
        };
        if base_signal * 2 < 3 * base_noise {
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
            results.push(bestmsg);
        }
    }

    results
}

pub fn demodulate2000(samples: &[u16], fix: bool) -> Vec<ModeSMessage> {
    let mut results = Vec::new();
    if samples.len() <= MIN_TRAILING_2000 {
        return results;
    }

    for j in 0..(samples.len() - MIN_TRAILING_2000) {
        if !preamble_2000(&samples[j..j + 15]) {
            continue;
        }
        let Some(bestmsg) = score_2000(samples, j, fix) else {
            continue;
        };
        results.push(bestmsg);
    }

    results
}

fn preamble_2000(m: &[u16]) -> bool {
    if !(m[0] > m[1]
        && m[1] < m[2]
        && m[2] > m[3]
        && m[3] < m[0]
        && m[4] < m[0]
        && m[5] < m[0]
        && m[6] < m[0]
        && m[7] > m[8]
        && m[8] < m[9]
        && m[9] > m[6])
    {
        return false;
    }
    let high = (u32::from(m[0]) + u32::from(m[2]) + u32::from(m[7]) + u32::from(m[9])) / 6;
    let high = high as u16;
    m[4] < high && m[5] < high && m[11] < high && m[12] < high && m[13] < high && m[14] < high
}

fn score_2000(samples: &[u16], j: usize, fix: bool) -> Option<ModeSMessage> {
    let frame = &samples[j..j + MIN_TRAILING_2000];
    if let Some(msg) = accept_2000(frame, fix) {
        return Some(msg_2000(msg, j, frame));
    }
    if j == 0 {
        return None;
    }
    let mut corrected = frame.to_vec();
    apply_phase_correction(&mut corrected, samples[j - 1]);
    let msg = accept_2000(&corrected, fix)?;
    let df = msg.0[0] >> 3;
    if !matches!(df, 17 | 18) {
        return None;
    }
    Some(msg_2000(msg, j, frame))
}

fn accept_2000(frame: &[u16], fix: bool) -> Option<([u8; MODES_LONG_MSG_BYTES], i32, u8)> {
    let mut msg = unpack_2000(frame)?;
    let msglen = if msg[0] & 0x80 != 0 { 14 } else { 7 };
    if mean_abs_delta(frame, msglen) < 10 * 255 {
        return None;
    }
    let (score, corrected_bits) = score_message(&mut msg, fix);
    (score >= 0).then_some((msg, score, corrected_bits))
}

fn unpack_2000(frame: &[u16]) -> Option<[u8; MODES_LONG_MSG_BYTES]> {
    let mut bits = [0_u8; LONG_MSG_BITS];
    let mut errors = 0;
    for i in 0..LONG_MSG_BITS {
        let low = i32::from(frame[PREAMBLE_SAMPLES_2000 + i * 2]);
        let high = i32::from(frame[PREAMBLE_SAMPLES_2000 + i * 2 + 1]);
        bits[i] = if low == high {
            if i < 56 {
                errors += 1;
            }
            2
        } else if low > high {
            1
        } else {
            0
        };
    }
    if errors != 0 {
        return None;
    }
    let mut msg = [0_u8; MODES_LONG_MSG_BYTES];
    for (slot, chunk) in msg.iter_mut().zip(bits.as_chunks::<8>().0) {
        *slot = chunk.iter().enumerate().fold(0_u8, |byte, (n, bit)| {
            byte | bit.wrapping_shl((7 - n) as u32)
        });
    }
    Some(msg)
}

fn mean_abs_delta(frame: &[u16], msglen: usize) -> u32 {
    let n = msglen * 8;
    let mut delta = 0_u32;
    for i in 0..n {
        let a = u32::from(frame[PREAMBLE_SAMPLES_2000 + i * 2]);
        let b = u32::from(frame[PREAMBLE_SAMPLES_2000 + i * 2 + 1]);
        delta += a.abs_diff(b);
    }
    delta / (msglen as u32 * 4)
}

fn msg_2000(
    (msg, score, corrected_bits): ([u8; MODES_LONG_MSG_BYTES], i32, u8),
    j: usize,
    frame: &[u16],
) -> ModeSMessage {
    let mut scaled_signal_power = 0_u64;
    for sample in frame
        .iter()
        .take(MIN_TRAILING_2000)
        .skip(PREAMBLE_SAMPLES_2000)
    {
        let mag = u64::from(*sample);
        scaled_signal_power += mag * mag;
    }
    let signal_power = scaled_signal_power as f64 / 65535.0 / 65535.0;
    ModeSMessage {
        msg,
        signal_level: signal_power / DATA_SAMPLES_2000 as f64,
        score,
        sample_position: j,
        corrected_bits,
    }
}

fn apply_phase_correction(m: &mut [u16], prev: u16) {
    let on_time = u32::from(m[0]) + u32::from(m[2]) + u32::from(m[7]) + u32::from(m[9]);
    let early = (u32::from(prev) + u32::from(m[6])) * 2;
    let late = (u32::from(m[3]) + u32::from(m[10])) * 2;
    let last = PREAMBLE_SAMPLES_2000 + DATA_SAMPLES_2000 - 1;
    if early > late {
        let den = early + on_time;
        if den == 0 {
            return;
        }
        let scale_up = (16384 + 16384 * early / den) as u16;
        let scale_down = (16384 - 16384 * early / den) as u16;
        m[last] = scale_sample(m[last], scale_up);
        let mut j = last - 1;
        while j > PREAMBLE_SAMPLES_2000 {
            m[j - 1] = if m[j] > m[j + 1] {
                scale_sample(m[j - 1], scale_down)
            } else {
                scale_sample(m[j - 1], scale_up)
            };
            j -= 2;
        }
    } else {
        let den = late + on_time;
        if den == 0 {
            return;
        }
        let scale_up = (16384 + 16384 * late / den) as u16;
        let scale_down = (16384 - 16384 * late / den) as u16;
        m[PREAMBLE_SAMPLES_2000] = scale_sample(m[PREAMBLE_SAMPLES_2000], scale_up);
        let mut j = PREAMBLE_SAMPLES_2000;
        while j < last - 1 {
            m[j + 2] = if m[j] > m[j + 1] {
                scale_sample(m[j + 2], scale_up)
            } else {
                scale_sample(m[j + 2], scale_down)
            };
            j += 2;
        }
    }
}

fn scale_sample(v: u16, scale: u16) -> u16 {
    let result = u32::from(v) * u32::from(scale) / 16384;
    result.min(65535) as u16
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

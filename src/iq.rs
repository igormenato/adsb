//! Mode S frames as RTL-SDR cu8 (unsigned interleaved I/Q).

use std::f64::consts::{PI, SQRT_2};

const PREAMBLE_US: f64 = 8.0;
const BIT_US: f64 = 1.0;
const HALF_US: f64 = 0.5;
const DATA_BITS: usize = 112;
const FRAME_US: f64 = PREAMBLE_US + BIT_US * DATA_BITS as f64;
const GUARD_US: f64 = 200.0;
const PREAMBLE: [f64; 4] = [0.0, 1.0, 3.5, 4.5];
/// Sample clock vs first preamble pulse.
/// -0.12 µs at 2.4 MS/s puts single-sample peaks at dump1090 phase 4
/// (indices 1,3,9,12). -0.20 µs put energy on two consecutive samples
/// (3 and 4), so noisy quiet-slots failed `samples[j+4] < high`.
const ALIGN_US: f64 = -0.12;
const CU8_CENTER: f64 = 127.5;
const CU8_SCALE: f64 = 128.0;

#[derive(Clone, Copy, Debug)]
pub struct Synth {
    pub amplitude: f64,
    pub snr_db: Option<f64>,
    pub seed: u64,
    pub sample_hz: u32,
    pub overlap_us: Option<f64>,
}

impl Default for Synth {
    fn default() -> Self {
        Self {
            amplitude: 0.8,
            snr_db: None,
            seed: 1,
            sample_hz: super::radio::SAMPLE_HZ,
            overlap_us: None,
        }
    }
}

pub fn encode_cu8(frames: &[[u8; 14]], synth: Synth) -> Result<Vec<u8>, String> {
    if frames.is_empty() {
        return Err("iq: no frames".into());
    }
    if synth.sample_hz == 0 {
        return Err("iq: sample rate must be > 0".into());
    }
    if !(synth.amplitude.is_finite() && synth.amplitude >= 0.0 && synth.amplitude <= 1.0) {
        return Err("iq: amplitude must be in 0..=1".into());
    }
    if let Some(snr) = synth.snr_db
        && !snr.is_finite()
    {
        return Err("iq: snr must be finite".into());
    }
    let starts = frame_starts(frames.len(), synth.overlap_us);
    let last = starts.last().copied().unwrap_or(0.0);
    let duration_us = last + FRAME_US + GUARD_US;
    let n = ((duration_us * 1e-6) * f64::from(synth.sample_hz)).ceil() as usize + 1;
    let (amp, sigma) = pulse_and_noise(synth.amplitude, synth.snr_db);
    let mut rng = Rng(synth.seed | 1);
    let mut out = Vec::with_capacity(n * 2);
    for i in 0..n {
        let t_us = ALIGN_US + i as f64 * 1e6 / f64::from(synth.sample_hz);
        let mut re = 0.0;
        for (frame, start) in frames.iter().zip(starts.iter()) {
            if pulse_at(t_us - start, frame) {
                re = (re + amp).clamp(-1.0, 1.0);
            }
        }
        re += sigma * rng.gaussian();
        let im = sigma * rng.gaussian();
        out.push(to_cu8(re));
        out.push(to_cu8(im));
    }
    Ok(out)
}

fn frame_starts(n: usize, overlap_us: Option<f64>) -> Vec<f64> {
    let dt = match overlap_us {
        Some(overlap) if overlap.is_finite() && overlap > 0.0 => (FRAME_US - overlap).max(1.0),
        _ => FRAME_US + GUARD_US,
    };
    (0..n).map(|i| i as f64 * dt).collect()
}

/// Pulse amplitude and per-component AWGN σ.
/// SNR is `10 log10(A² / (2σ²))`: on-chip pulse power over complex noise power.
/// A and σ are scaled together so `A + 3σ` fits in cu8 (no peak clipping).
fn pulse_and_noise(amplitude: f64, snr_db: Option<f64>) -> (f64, f64) {
    let Some(snr_db) = snr_db else {
        return (amplitude, 0.0);
    };
    if amplitude == 0.0 {
        return (0.0, 0.0);
    }
    let mut amp = amplitude;
    let mut sigma = amp / (SQRT_2 * 10f64.powf(snr_db / 20.0));
    let peak = amp + 3.0 * sigma;
    if peak > 1.0 {
        let g = 1.0 / peak;
        amp *= g;
        sigma *= g;
    }
    (amp, sigma)
}

fn pulse_at(t_us: f64, frame: &[u8; 14]) -> bool {
    if t_us < 0.0 || t_us >= FRAME_US {
        return false;
    }
    if t_us < PREAMBLE_US {
        return PREAMBLE.iter().any(|&p| t_us >= p && t_us < p + HALF_US);
    }
    let data_t = t_us - PREAMBLE_US;
    let bit_i = (data_t / BIT_US) as usize;
    if bit_i >= DATA_BITS {
        return false;
    }
    let first_half = data_t - bit_i as f64 * BIT_US < HALF_US;
    let byte = frame[bit_i / 8];
    let bit = (byte >> (7 - (bit_i % 8))) & 1 == 1;
    bit == first_half
}

fn to_cu8(v: f64) -> u8 {
    // `as u8` wraps, so 255.5 (v=1) must be clamped: that used to store 0 on peaks.
    (v * CU8_SCALE + CU8_CENTER).round().clamp(0.0, 255.0) as u8
}

struct Rng(u64);

impl Rng {
    fn next_u64(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn u01(&mut self) -> f64 {
        let x = self.next_u64() >> 11;
        (x as f64) * (1.0 / ((1u64 << 53) as f64))
    }

    fn gaussian(&mut self) -> f64 {
        let u1 = self.u01().max(f64::EPSILON);
        let u2 = self.u01();
        (-2.0 * u1.ln()).sqrt() * (2.0 * PI * u2).cos()
    }
}

#[cfg(test)]
mod tests {
    use rs1090::source::demod::demod2400::demodulate2400;
    use rs1090::source::demod::magnitude_u16;

    use super::*;
    use crate::modes::EVEN;
    fn cu8_to_complex(bytes: &[u8]) -> Vec<num_complex::Complex<f32>> {
        bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| num_complex::Complex {
                re: (f32::from(pair[0]) - 127.5) / 128.0,
                im: (f32::from(pair[1]) - 127.5) / 128.0,
            })
            .collect()
    }

    fn recovered(bytes: &[u8]) -> Vec<[u8; 14]> {
        let iq = cu8_to_complex(bytes);
        demodulate2400(&magnitude_u16(&iq))
            .into_iter()
            .map(|m| m.msg)
            .collect()
    }

    fn mag_of(bytes: &[u8]) -> Vec<u16> {
        magnitude_u16(&cu8_to_complex(bytes))
    }

    /// dump1090-2400 phase-4 preamble + 3.5 dB + quiet-slot checks at sample 0.
    fn preamble_at_zero(m: &[u16]) -> bool {
        if m.len() < 19 {
            return false;
        }
        if !(m[0] < m[1] && m[12] > m[13]) {
            return false;
        }
        if !(m[1] > m[2]
            && m[2] < m[3]
            && m[3] > m[4]
            && m[8] < m[9]
            && m[9] > m[10]
            && m[11] < m[12])
        {
            return false;
        }
        let high = (i32::from(m[1]) + i32::from(m[3]) + i32::from(m[9]) + i32::from(m[12])) / 4;
        let signal = u32::from(m[1]) + u32::from(m[3]) + u32::from(m[9]) + u32::from(m[12]);
        let noise = u32::from(m[5]) + u32::from(m[6]) + u32::from(m[7]) + u32::from(m[8]);
        if signal * 2 < 3 * noise {
            return false;
        }
        ![5, 6, 7, 8, 14, 15, 16, 17, 18]
            .into_iter()
            .any(|i| i32::from(m[i]) >= high)
    }

    #[test]
    fn ten_db_power_snr_decodes() {
        let clean = mag_of(&encode_cu8(&[EVEN], Synth::default()).expect("iq"));
        eprintln!(
            "clean mag[0..20]={:?} (phase-4 peaks at 1,3,9,12)",
            clean.iter().take(20).collect::<Vec<_>>()
        );
        let mut preamble = 0;
        let mut crc = 0;
        for seed in 1..=32 {
            let bytes = encode_cu8(
                &[EVEN],
                Synth {
                    snr_db: Some(10.0),
                    seed,
                    ..Synth::default()
                },
            )
            .expect("iq");
            let mag = mag_of(&bytes);
            if seed == 1 {
                eprintln!(
                    "10 dB mag[0..20]={:?}",
                    mag.iter().take(20).collect::<Vec<_>>()
                );
            }
            if (0..8).any(|j| mag.len() > j + 18 && preamble_at_zero(&mag[j..])) {
                preamble += 1;
            }
            if demodulate2400(&mag).iter().any(|m| m.msg == EVEN) {
                crc += 1;
            }
        }
        eprintln!(
            "10 dB preamble {preamble}/32 crc-perfect {crc}/32 (rs1090 dump1090-2400 has no bit correction)"
        );
        assert!(
            preamble >= 24,
            "10 dB preamble {preamble}/32 — generator failed the 3.5 dB check"
        );
        assert!(
            crc >= 8,
            "10 dB crc-perfect {crc}/32; expected this demod's cliff, not 0"
        );
    }

    #[test]
    fn a_clean_frame_demodulates() {
        let bytes = encode_cu8(&[EVEN], Synth::default()).expect("iq");
        let mag = mag_of(&bytes);
        assert!(
            mag[1] > mag[2] && mag[3] > mag[4] && mag[9] > mag[10] && mag[12] > mag[11],
            "phase-4 preamble mag[0..14]={:?}",
            mag.iter().take(14).collect::<Vec<_>>()
        );
        let msgs = recovered(&bytes);
        assert!(msgs.iter().any(|m| m == &EVEN), "{msgs:02x?}");
    }

    #[test]
    fn overlapping_frames_are_shorter_than_sequential() {
        let odd = crate::modes::ODD;
        let sequential = encode_cu8(&[EVEN, odd], Synth::default()).expect("seq");
        let overlap = encode_cu8(
            &[EVEN, odd],
            Synth {
                overlap_us: Some(40.0),
                ..Synth::default()
            },
        )
        .expect("overlap");
        assert!(
            overlap.len() < sequential.len(),
            "{} vs {}",
            overlap.len(),
            sequential.len()
        );
        assert_eq!(overlap.len() % 2, 0);
    }

    #[test]
    fn the_same_seed_is_byte_identical() {
        let synth = Synth {
            snr_db: Some(10.0),
            seed: 42,
            ..Synth::default()
        };
        let a = encode_cu8(&[EVEN], synth).expect("a");
        let b = encode_cu8(&[EVEN], synth).expect("b");
        assert_eq!(a, b);
        assert_eq!(a.len() % 2, 0);
    }

    #[test]
    fn two_msps_is_shorter_than_24() {
        let fast = encode_cu8(&[EVEN], Synth::default()).expect("24");
        let slow = encode_cu8(
            &[EVEN],
            Synth {
                sample_hz: 2_000_000,
                ..Synth::default()
            },
        )
        .expect("2");
        assert!(slow.len() < fast.len(), "{} vs {}", slow.len(), fast.len());
    }
}

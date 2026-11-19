//! Mode S frames as RTL-SDR cu8 (unsigned interleaved I/Q).

use std::f64::consts::{PI, SQRT_2};

use anyhow::{Result, bail};

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
    /// Sample-clock offset in µs. Default matches dump1090 phase 4.
    pub align_us: f64,
    /// Silence before the first frame, in µs.
    pub leading_us: f64,
    /// Extra gap after each frame, drawn U[0, start_jitter_us] from `seed`.
    pub start_jitter_us: f64,
}

impl Default for Synth {
    fn default() -> Self {
        Self {
            amplitude: 0.8,
            snr_db: None,
            seed: 1,
            sample_hz: 2_400_000,
            overlap_us: None,
            align_us: ALIGN_US,
            leading_us: 0.0,
            start_jitter_us: 0.0,
        }
    }
}

pub fn encode_cu8(frames: &[[u8; 14]], synth: Synth) -> Result<Vec<u8>> {
    if frames.is_empty() {
        bail!("iq: no frames");
    }
    if synth.sample_hz == 0 {
        bail!("iq: sample rate must be > 0");
    }
    if !(synth.amplitude.is_finite() && synth.amplitude >= 0.0 && synth.amplitude <= 1.0) {
        bail!("iq: amplitude must be in 0..=1");
    }
    if let Some(snr) = synth.snr_db
        && !snr.is_finite()
    {
        bail!("iq: snr must be finite");
    }
    let mut rng = Rng(synth.seed | 1);
    let starts = frame_starts(
        frames.len(),
        synth.overlap_us,
        synth.leading_us,
        synth.start_jitter_us,
        &mut rng,
    );
    let last = starts.last().copied().unwrap_or(synth.leading_us);
    let duration_us = last + FRAME_US + GUARD_US;
    let n = ((duration_us * 1e-6) * f64::from(synth.sample_hz)).ceil() as usize + 1;
    let (amp, sigma) = pulse_and_noise(synth.amplitude, synth.snr_db);
    Ok(fill_cu8(
        n,
        synth.sample_hz,
        synth.align_us,
        amp,
        sigma,
        frames,
        &starts,
        rng,
    ))
}

/// AWGN only (no Mode S pulses). `snr_db` sets σ the same way as [`encode_cu8`].
pub fn encode_noise_cu8(duration_s: f64, synth: Synth) -> Result<Vec<u8>> {
    if !(duration_s.is_finite() && duration_s > 0.0) {
        bail!("iq: noise duration must be > 0");
    }
    if synth.sample_hz == 0 {
        bail!("iq: sample rate must be > 0");
    }
    if let Some(snr) = synth.snr_db
        && !snr.is_finite()
    {
        bail!("iq: snr must be finite");
    }
    let n = (duration_s * f64::from(synth.sample_hz)).ceil() as usize;
    let (_amp, sigma) = pulse_and_noise(synth.amplitude, synth.snr_db);
    let rng = Rng(synth.seed | 1);
    Ok(fill_cu8(
        n,
        synth.sample_hz,
        synth.align_us,
        0.0,
        sigma,
        &[],
        &[],
        rng,
    ))
}

#[allow(clippy::too_many_arguments)]
fn fill_cu8(
    n: usize,
    sample_hz: u32,
    align_us: f64,
    amp: f64,
    sigma: f64,
    frames: &[[u8; 14]],
    starts: &[f64],
    mut rng: Rng,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(n * 2);
    let mut active = 0usize;
    for i in 0..n {
        let t_us = align_us + i as f64 * 1e6 / f64::from(sample_hz);
        while active < starts.len() && t_us >= starts[active] + FRAME_US {
            active += 1;
        }
        let mut re = 0.0;
        if amp > 0.0 {
            for k in active..starts.len() {
                if t_us < starts[k] {
                    break;
                }
                if pulse_at(t_us - starts[k], &frames[k]) {
                    re = (re + amp).clamp(-1.0, 1.0);
                }
            }
        }
        re += sigma * rng.gaussian();
        let im = sigma * rng.gaussian();
        out.push(to_cu8(re));
        out.push(to_cu8(im));
    }
    out
}

fn frame_starts(
    n: usize,
    overlap_us: Option<f64>,
    leading_us: f64,
    jitter_us: f64,
    rng: &mut Rng,
) -> Vec<f64> {
    let dt = match overlap_us {
        Some(overlap) if overlap.is_finite() && overlap > 0.0 => (FRAME_US - overlap).max(1.0),
        _ => FRAME_US + GUARD_US,
    };
    let mut t = leading_us.max(0.0);
    let jitter = if jitter_us.is_finite() && jitter_us > 0.0 {
        jitter_us
    } else {
        0.0
    };
    (0..n)
        .map(|_| {
            let start = t;
            t += dt;
            if jitter > 0.0 {
                t += rng.u01() * jitter;
            }
            start
        })
        .collect()
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
    use rs1090::decode::crc::modes_checksum;

    use crate::demod::{demodulate2400, msg_octets};
    use crate::mag::mag_cu8;
    use crate::modes::{EVEN, ODD, df5_identity};
    use crate::{EmergencyKind, Tracker, pack_frame};

    use super::*;

    fn recovered(bytes: &[u8]) -> Vec<[u8; 14]> {
        demodulate2400(&mag_cu8(bytes), true)
            .into_iter()
            .map(|m| m.msg)
            .collect()
    }

    fn mag_of(bytes: &[u8]) -> Vec<u16> {
        mag_cu8(bytes)
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
            if demodulate2400(&mag, true).iter().any(|m| m.msg == EVEN) {
                crc += 1;
            }
        }
        eprintln!("10 dB preamble {preamble}/32 recovered {crc}/32");
        assert!(
            preamble >= 24,
            "10 dB preamble {preamble}/32 — generator failed the 3.5 dB check"
        );
        assert!(
            crc >= 8,
            "10 dB crc-perfect {crc}/32; expected this demod's cliff, not 0"
        );
    }

    fn df11_frame() -> [u8; 14] {
        let mut frame = [0_u8; 14];
        frame[0] = 0x58;
        frame[1] = 0x40;
        frame[2] = 0x05;
        frame[3] = 0x8B;
        let crc = modes_checksum(&frame[..7], 56).expect("df11 crc");
        frame[4] = (crc >> 16) as u8;
        frame[5] = (crc >> 8) as u8;
        frame[6] = crc as u8;
        frame
    }

    #[test]
    fn df5_7700_iq_sets_uart_emergency() {
        let mut df5 = [0_u8; 14];
        df5[..7].copy_from_slice(&df5_identity(0x40058B, 0x7700));
        let bytes = encode_cu8(&[df5, EVEN, ODD], Synth::default()).expect("iq");
        let msgs = demodulate2400(&mag_of(&bytes), true);
        assert!(
            msgs.iter().any(|m| m.msg[0] >> 3 == 5),
            "df5 missing {msgs:02x?}"
        );
        let mut tracker = Tracker::new();
        let mut emergency = None;
        for (i, m) in msgs.iter().enumerate() {
            if let Some(fix) = tracker.ingest(i as f64, msg_octets(&m.msg)) {
                emergency = Some(pack_frame(&fix)[31]);
                assert_eq!(fix.aircraft.emergency, Some(EmergencyKind::General));
            }
        }
        assert_eq!(emergency, Some(1), "UART emergency byte");
    }

    #[test]
    fn one_bit_df17_needs_fix() {
        let mut damaged = EVEN;
        damaged[6] ^= 0x01;
        let bytes = encode_cu8(&[damaged], Synth::default()).expect("iq");
        let mag = mag_of(&bytes);
        let off = demodulate2400(&mag, false);
        let on = demodulate2400(&mag, true);
        assert!(
            !off.iter().any(|m| m.msg == EVEN),
            "--no-fix decoded the damaged DF17: {off:02x?}"
        );
        assert!(
            on.iter().any(|m| m.msg == EVEN && m.corrected_bits == 1),
            "--fix missed the 1-bit DF17: {on:02x?}"
        );
    }

    #[test]
    fn df11_recovers_seven_octets() {
        let frame = df11_frame();
        let bytes = encode_cu8(&[frame], Synth::default()).expect("iq");
        let msgs = demodulate2400(&mag_of(&bytes), true);
        let got = msgs.iter().find(|m| m.msg[0] >> 3 == 11).expect("df11");
        assert_eq!(msg_octets(&got.msg), &frame[..7]);
    }

    #[test]
    fn skip_does_not_repeat_a_frame() {
        let odd = crate::modes::ODD;
        let bytes = encode_cu8(&[EVEN, odd], Synth::default()).expect("iq");
        let msgs = recovered(&bytes);
        assert!(msgs.len() <= 4, "n={} {msgs:02x?}", msgs.len());
        assert!(msgs.iter().any(|m| m == &EVEN), "{msgs:02x?}");
        assert!(msgs.iter().any(|m| m == &odd), "{msgs:02x?}");
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

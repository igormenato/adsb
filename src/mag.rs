//! cu8 magnitude LUT. Same scale as rs1090 `magnitude_u16` and dump1090 maglut.

use std::sync::OnceLock;

const SCALE: f32 = 65535.0;
const CENTER: f32 = 127.5;
const DIV: f32 = 128.0;

fn lut() -> &'static [u16; 65536] {
    static LUT: OnceLock<Box<[u16; 65536]>> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut table = Box::new([0u16; 65536]);
        for i in 0..256 {
            for q in 0..256 {
                table[i << 8 | q] = mag_iq(cu8_to_f32(i as u8), cu8_to_f32(q as u8));
            }
        }
        table
    })
}

pub fn cu8_to_f32(v: u8) -> f32 {
    (f32::from(v) - CENTER) / DIV
}

pub fn mag_iq(i: f32, q: f32) -> u16 {
    let mag = f32::sqrt(q.mul_add(q, i * i));
    mag.mul_add(SCALE, 0.5) as u16
}

/// Write one magnitude per I/Q pair into `out`. `cu8` must be even length.
pub fn mag_cu8_into(cu8: &[u8], out: &mut [u16]) {
    let n = cu8.len() / 2;
    debug_assert_eq!(out.len(), n);
    let table = lut();
    for (dst, pair) in out.iter_mut().zip(cu8.as_chunks::<2>().0) {
        *dst = table[(pair[0] as usize) << 8 | pair[1] as usize];
    }
}

pub fn mag_cu8(cu8: &[u8]) -> Vec<u16> {
    let n = cu8.len() / 2;
    let mut out = vec![0; n];
    mag_cu8_into(&cu8[..n * 2], &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    use crate::demod::demodulate2400;
    use num_complex::Complex;
    use rs1090::source::demod::magnitude_u16;

    fn cu8_to_complex(cu8: &[u8]) -> Vec<Complex<f32>> {
        cu8.as_chunks::<2>()
            .0
            .iter()
            .map(|pair| Complex {
                re: cu8_to_f32(pair[0]),
                im: cu8_to_f32(pair[1]),
            })
            .collect()
    }

    #[test]
    fn lut_matches_rs1090_on_all_cu8() {
        let mut max_delta = 0u16;
        let mut out = [0u16; 1];
        for i in 0..=255u8 {
            for q in 0..=255u8 {
                mag_cu8_into(&[i, q], &mut out);
                let iq = [Complex {
                    re: cu8_to_f32(i),
                    im: cu8_to_f32(q),
                }];
                let want = magnitude_u16(&iq)[0];
                let delta = out[0].abs_diff(want);
                if delta > max_delta {
                    max_delta = delta;
                }
            }
        }
        eprintln!("mag LUT vs rs1090 magnitude_u16 max delta={max_delta} over 256x256");
        assert_eq!(max_delta, 0);
    }

    #[test]
    fn lut_rtf_and_message_count_on_1s_buffer() {
        const N: usize = 2_400_000;
        let mut cu8 = vec![127u8; N * 2];
        for (i, b) in cu8.iter_mut().enumerate() {
            if i % 64 == 0 {
                *b = 140;
            }
        }
        let iq = cu8_to_complex(&cu8);
        let t0 = Instant::now();
        let old = magnitude_u16(&iq);
        let old_s = t0.elapsed().as_secs_f64();
        let t0 = Instant::now();
        let new = mag_cu8(&cu8);
        let new_s = t0.elapsed().as_secs_f64();
        assert_eq!(old, new);
        let old_n = demodulate2400(&old, true).len();
        let new_n = demodulate2400(&new, true).len();
        assert_eq!(old_n, new_n);
        eprintln!("1s 2.4MS/s mag rtf old={old_s:.4} lut={new_s:.4} msgs={new_n} (capture=1s)");
    }
}

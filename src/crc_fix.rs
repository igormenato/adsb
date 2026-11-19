//! Mode S CRC correction for DF11/DF17/DF18 (dump1090-fa `--fix`).
//!
//! 1-bit PI repair is ungated. DF17/18 also try a 2-bit syndrome table
//! (collisions with other 1–4 bit patterns dropped) when both bits are
//! among the [`TWO_BIT_CONF_N`] lowest-confidence magnitudes.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use rs1090::decode::crc::modes_checksum;

const LONG_BITS: usize = 112;
const SHORT_BITS: usize = 56;
const AA_BITS: std::ops::RangeInclusive<usize> = 8..=31;
/// 2-bit DF17/18 repair only if both flipped bits rank in this many weakest |corr|.
const TWO_BIT_CONF_N: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Correction {
    pub bit: usize,
    pub extra: Option<usize>,
}

fn long_syndromes() -> &'static [u32; LONG_BITS] {
    static TABLE: OnceLock<[u32; LONG_BITS]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [0u32; LONG_BITS];
        for (bit, slot) in table.iter_mut().enumerate() {
            let mut msg = [0u8; 14];
            flip(&mut msg, bit);
            *slot = modes_checksum(&msg, LONG_BITS).expect("112");
        }
        table
    })
}

fn short_syndromes() -> &'static [u32; SHORT_BITS] {
    static TABLE: OnceLock<[u32; SHORT_BITS]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [0u32; SHORT_BITS];
        for (bit, slot) in table.iter_mut().enumerate() {
            let mut msg = [0u8; 7];
            flip(&mut msg, bit);
            *slot = modes_checksum(&msg, SHORT_BITS).expect("56");
        }
        table
    })
}

fn flip(msg: &mut [u8], bit: usize) {
    msg[bit / 8] ^= 1 << (7 - (bit % 8));
}

fn df(msg: &[u8]) -> u8 {
    msg[0] >> 3
}

fn long_crc(msg: &[u8; 14]) -> Option<u32> {
    modes_checksum(msg, LONG_BITS).ok()
}

fn short_crc(msg: &[u8; 14]) -> Option<u32> {
    modes_checksum(&msg[..7], SHORT_BITS).ok()
}

fn find_bit(syndrome: u32, table: &[u32]) -> Option<usize> {
    table.iter().position(|&s| s == syndrome)
}

pub fn aa_bit(bit: usize) -> bool {
    AA_BITS.contains(&bit)
}

pub(crate) fn prepare_tables() {
    let _ = long_syndromes();
    let _ = short_syndromes();
    let _ = two_bit_long();
}

/// Flip a single correctable PI bit. Input DF must be 11, 17, or 18.
pub fn correct_pi(msg: &mut [u8; 14]) -> Option<Correction> {
    correct(msg, None)
}

/// 1-bit repair, then gated 2-bit DF17/18 when `conf` is the per-bit |corr|.
pub(crate) fn correct_with_conf(msg: &mut [u8; 14], conf: &[i32]) -> Option<Correction> {
    correct(msg, Some(conf))
}

fn correct(msg: &mut [u8; 14], conf: Option<&[i32]>) -> Option<Correction> {
    let df0 = df(msg);
    if !matches!(df0, 11 | 17 | 18) {
        return None;
    }
    let long = long_crc(msg)?;
    let short = short_crc(msg)?;

    if matches!(df0, 17 | 18) && long == 0 {
        return None;
    }
    if df0 == 11 && short & 0x00ff_ff80 == 0 {
        return None;
    }

    if long != 0
        && let Some(bit) = find_bit(long, long_syndromes())
    {
        let mut trial = *msg;
        flip(&mut trial, bit);
        let df1 = df(&trial);
        if matches!(df1, 17 | 18) && long_crc(&trial) == Some(0) {
            *msg = trial;
            return Some(Correction { bit, extra: None });
        }
    }

    if short != 0
        && let Some(bit) = find_bit(short, short_syndromes())
    {
        let mut trial = *msg;
        flip(&mut trial, bit);
        if df(&trial) == 11 && short_crc(&trial).is_some_and(|c| c & 0x00ff_ff80 == 0) {
            *msg = trial;
            return Some(Correction { bit, extra: None });
        }
    }

    if let Some(conf) = conf
        && matches!(df0, 17 | 18)
        && long != 0
        && let Some(&(a, b)) = two_bit_long().get(&long)
        && among_weakest(a as usize, conf)
        && among_weakest(b as usize, conf)
    {
        let mut trial = *msg;
        flip(&mut trial, a as usize);
        flip(&mut trial, b as usize);
        let df1 = df(&trial);
        if matches!(df1, 17 | 18) && long_crc(&trial) == Some(0) {
            *msg = trial;
            return Some(Correction {
                bit: a as usize,
                extra: Some(b as usize),
            });
        }
    }

    None
}

fn among_weakest(bit: usize, conf: &[i32]) -> bool {
    if bit >= conf.len() {
        return false;
    }
    let target = conf[bit].unsigned_abs();
    let mut rank = 0usize;
    for (i, &c) in conf.iter().enumerate() {
        let mag = c.unsigned_abs();
        if mag < target || (mag == target && i < bit) {
            rank += 1;
            if rank >= TWO_BIT_CONF_N {
                return false;
            }
        }
    }
    true
}

/// Unique 2-bit syndromes on 112-bit PI; collisions with other 1–4 bit patterns dropped
/// (dump1090-fa `prepareErrorTable(..., 2, 4)`).
fn two_bit_long() -> &'static HashMap<u32, (u16, u16)> {
    static TABLE: OnceLock<HashMap<u32, (u16, u16)>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let s = long_syndromes();
        let one: HashSet<u32> = s.iter().copied().collect();
        let mut map: HashMap<u32, Option<(u16, u16)>> = HashMap::new();
        for i in 0..LONG_BITS {
            for j in (i + 1)..LONG_BITS {
                let syn = s[i] ^ s[j];
                if syn == 0 || one.contains(&syn) {
                    continue;
                }
                match map.get_mut(&syn) {
                    None => {
                        map.insert(syn, Some((i as u16, j as u16)));
                    }
                    Some(slot) => *slot = None,
                }
            }
        }
        for i in 0..LONG_BITS {
            for j in (i + 1)..LONG_BITS {
                let sj = s[i] ^ s[j];
                for k in (j + 1)..LONG_BITS {
                    let sk = sj ^ s[k];
                    if let Some(slot) = map.get_mut(&sk) {
                        *slot = None;
                    }
                    for &sl in s.iter().skip(k + 1) {
                        if let Some(slot) = map.get_mut(&(sk ^ sl)) {
                            *slot = None;
                        }
                    }
                }
            }
        }
        map.into_iter()
            .filter_map(|(syn, pair)| pair.map(|p| (syn, p)))
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modes::{EVEN, IDENT, ODD, STATUS, VELOCITY, df17, df18_cf0};

    fn unique_two_bit_pair() -> (usize, usize) {
        let s = long_syndromes();
        let table = two_bit_long();
        for a in 8..LONG_BITS {
            for b in (a + 1)..LONG_BITS {
                if table.contains_key(&(s[a] ^ s[b])) {
                    return (a, b);
                }
            }
        }
        panic!("no unique 2-bit pair");
    }

    fn colliding_two_bit_pair() -> (usize, usize) {
        let s = long_syndromes();
        let table = two_bit_long();
        for a in 8..LONG_BITS {
            for b in (a + 1)..LONG_BITS {
                let syn = s[a] ^ s[b];
                if syn != 0 && find_bit(syn, s).is_none() && !table.contains_key(&syn) {
                    return (a, b);
                }
            }
        }
        panic!("no colliding 2-bit pair");
    }

    fn weak_at(bits: &[usize]) -> [i32; LONG_BITS] {
        let mut conf = [10_000_i32; LONG_BITS];
        for &b in bits {
            conf[b] = 0;
        }
        conf
    }

    fn remainder(frame: &[u8; 14]) -> u32 {
        long_crc(frame).expect("crc")
    }

    fn flip_copy(frame: &[u8; 14], bit: usize) -> [u8; 14] {
        let mut msg = *frame;
        flip(&mut msg, bit);
        msg
    }

    #[test]
    fn one_bit_errors_on_df17_are_restored() {
        for frame in [&EVEN, &ODD, &IDENT, &VELOCITY, &STATUS] {
            assert_eq!(remainder(frame), 0);
            for bit in 5..LONG_BITS {
                let mut damaged = flip_copy(frame, bit);
                assert_eq!(df(&damaged), 17);
                assert_ne!(remainder(&damaged), 0, "bit {bit}");
                let got = correct_pi(&mut damaged).expect("correctable");
                assert_eq!(got.bit, bit);
                assert_eq!(&damaged, frame);
            }
        }
    }

    #[test]
    fn two_bit_errors_are_rejected() {
        let mut damaged = EVEN;
        flip(&mut damaged, 10);
        flip(&mut damaged, 40);
        let original = damaged;
        assert!(correct_pi(&mut damaged).is_none());
        assert_eq!(damaged, original);
        assert_ne!(remainder(&damaged), 0);
    }

    #[test]
    fn two_bit_unique_pair_is_restored_when_weak() {
        let (a, b) = unique_two_bit_pair();
        let conf = weak_at(&[a, b]);
        for frame in [&EVEN, &ODD] {
            let mut damaged = *frame;
            flip(&mut damaged, a);
            flip(&mut damaged, b);
            let got = correct_with_conf(&mut damaged, &conf).expect("2-bit");
            assert_eq!(got.extra, Some(b));
            assert_eq!(got.bit, a);
            assert_eq!(&damaged, frame);
        }
        let mut df18 = df18_cf0(0xABC001, EVEN[4..11].try_into().unwrap());
        assert_eq!(df(&df18), 18);
        assert_eq!(remainder(&df18), 0);
        let good = df18;
        flip(&mut df18, a);
        flip(&mut df18, b);
        let got = correct_with_conf(&mut df18, &conf).expect("df18 2-bit");
        assert_eq!(got.bit, a);
        assert_eq!(got.extra, Some(b));
        assert_eq!(df18, good);
    }

    #[test]
    fn two_bit_collision_is_rejected() {
        let (a, b) = colliding_two_bit_pair();
        let mut damaged = EVEN;
        flip(&mut damaged, a);
        flip(&mut damaged, b);
        let original = damaged;
        let conf = weak_at(&[a, b]);
        assert!(correct_with_conf(&mut damaged, &conf).is_none());
        assert_eq!(damaged, original);
    }

    #[test]
    fn two_bit_high_confidence_is_rejected() {
        let (a, b) = unique_two_bit_pair();
        let mut damaged = EVEN;
        flip(&mut damaged, a);
        flip(&mut damaged, b);
        let original = damaged;
        let mut conf = [0_i32; LONG_BITS];
        conf[a] = 10_000;
        conf[b] = 10_000;
        assert!(correct_with_conf(&mut damaged, &conf).is_none());
        assert_eq!(damaged, original);
    }

    #[test]
    fn two_bit_skips_non_df17_18() {
        let (a, b) = unique_two_bit_pair();
        let conf = weak_at(&[a, b]);

        let mut df4 = [0u8; 14];
        df4[0] = 0x20;
        df4[1] = 0x00;
        df4[2] = 0x10;
        flip(&mut df4, a);
        flip(&mut df4, b);
        let original = df4;
        assert!(correct_with_conf(&mut df4, &conf).is_none());
        assert_eq!(df4, original);

        let mut df11 = [0u8; 14];
        df11[0] = 0x58;
        df11[1] = 0x40;
        df11[2] = 0x05;
        df11[3] = 0x8B;
        let crc = modes_checksum(&df11[..7], SHORT_BITS).expect("df11");
        df11[4] = (crc >> 16) as u8;
        df11[5] = (crc >> 8) as u8;
        df11[6] = crc as u8;
        flip(&mut df11, 10);
        flip(&mut df11, 20);
        let original = df11;
        assert_eq!(df(&df11), 11);
        assert!(correct_with_conf(&mut df11, &conf).is_none());
        assert_eq!(df11, original);

        const SYNDROME: u32 = 0x00C3_97DB;
        let mut df16 = [0u8; 14];
        df16[0] = 16 << 3;
        df16[1] = 0x12;
        let crc = modes_checksum(&df16, LONG_BITS).expect("df16");
        let pi = crc ^ SYNDROME;
        df16[11] = (pi >> 16) as u8;
        df16[12] = (pi >> 8) as u8;
        df16[13] = pi as u8;
        let original = df16;
        assert!(correct_with_conf(&mut df16, &conf).is_none());
        assert_eq!(df16, original);
        assert_eq!(df16[0] >> 3, 16);
    }

    #[test]
    fn address_parity_frames_are_not_rewritten() {
        // DF4: altitude reply. Remainder is the ICAO, not a PI CRC.
        let mut df4 = [0u8; 14];
        df4[0] = 0x20;
        df4[1] = 0x00;
        df4[2] = 0x10;
        let original = df4;
        assert!(correct_pi(&mut df4).is_none());
        assert_eq!(df4, original);
    }

    #[test]
    fn df16_syndrome_match_stays_df16() {
        // Bit 4's 112-bit syndrome is 0xC397DB. A DF16 whose remainder equals
        // that used to be rewritten as DF17 when --fix ran on any long frame.
        const SYNDROME: u32 = 0x00C3_97DB;
        let mut df16 = [0u8; 14];
        df16[0] = 16 << 3;
        df16[1] = 0x12;
        let crc = modes_checksum(&df16, LONG_BITS).expect("df16");
        let pi = crc ^ SYNDROME;
        df16[11] = (pi >> 16) as u8;
        df16[12] = (pi >> 8) as u8;
        df16[13] = pi as u8;
        assert_eq!(remainder(&df16), SYNDROME);
        let original = df16;
        assert!(correct_pi(&mut df16).is_none());
        assert_eq!(df16[0] >> 3, 16);
        assert_eq!(df16, original);
    }

    #[test]
    fn df11_one_bit_restores_iid_overlay() {
        let mut df11 = [0u8; 14];
        df11[0] = 0x58;
        df11[1] = 0x40;
        df11[2] = 0x05;
        df11[3] = 0x8B;
        let crc = modes_checksum(&df11[..7], SHORT_BITS).expect("df11");
        df11[4] = (crc >> 16) as u8;
        df11[5] = (crc >> 8) as u8;
        df11[6] = crc as u8;
        assert_eq!(short_crc(&df11).unwrap() & 0x00ff_ff80, 0);

        let good = df11;
        // Bits 0..4 are DF. Bits 49..55 are the 7-bit IID overlay.
        for bit in 5..49 {
            let mut damaged = df11;
            flip(&mut damaged, bit);
            assert_eq!(df(&damaged), 11);
            let got = correct_pi(&mut damaged).expect("df11 bit");
            assert_eq!(got.bit, bit);
            assert_eq!(damaged[..7], good[..7]);
        }
    }

    #[test]
    fn clean_df17_is_left_alone() {
        let mut frame = df17(0xABC001, EVEN[4..11].try_into().unwrap());
        let good = frame;
        assert!(correct_pi(&mut frame).is_none());
        assert_eq!(frame, good);
    }
}

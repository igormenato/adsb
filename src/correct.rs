//! Single-bit CRC repair for DF17/DF18 extended squitter.

use std::sync::OnceLock;

use rs1090::decode::crc::modes_checksum;

const BITS: usize = 112;
const BYTES: usize = 14;

/// Syndrome of a 1 at each of the 112 bit positions, CRC over a zero frame.
fn syndromes() -> &'static [u32; BITS] {
    static TABLE: OnceLock<[u32; BITS]> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table = [0u32; BITS];
        let mut msg = [0u8; BYTES];
        for (bit, slot) in table.iter_mut().enumerate() {
            msg[bit / 8] = 1 << (7 - (bit % 8));
            *slot = checksum(&msg);
            msg[bit / 8] = 0;
        }
        table
    })
}

fn checksum(frame: &[u8]) -> u32 {
    modes_checksum(frame, BITS).expect("14-byte frame")
}

fn df17_18(frame: &[u8]) -> bool {
    matches!(frame[0] >> 3, 17 | 18)
}

/// Repair a DF17/DF18 frame whose CRC syndrome is exactly one bit.
///
/// `None` if the frame is already good, is not 112 bits, or is not a
/// correctable DF17/DF18. Address/parity formats are left untouched.
pub(crate) fn correct_extended_squitter(frame: &[u8]) -> Option<[u8; BYTES]> {
    if frame.len() != BYTES {
        return None;
    }
    let crc = checksum(frame);
    if crc == 0 {
        return None;
    }
    let bit = syndromes().iter().position(|&s| s == crc)?;
    let mut out = <[u8; BYTES]>::try_from(frame).ok()?;
    out[bit / 8] ^= 1 << (7 - (bit % 8));
    if checksum(&out) != 0 || !df17_18(&out) {
        return None;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use rs1090::decode::Message;

    use super::*;
    use crate::decode::Tracker;

    const EVEN: [u8; 14] = [
        0x8D, 0x40, 0x05, 0x8B, 0x58, 0xC9, 0x01, 0x37, 0x51, 0x47, 0xEF, 0xD0, 0x93, 0x57,
    ];
    const ODD: [u8; 14] = [
        0x8D, 0x40, 0x05, 0x8B, 0x58, 0xC9, 0x04, 0xA8, 0x7F, 0x40, 0x2D, 0x3B, 0x8C, 0x59,
    ];

    fn flip(frame: &[u8; 14], bit: usize) -> [u8; 14] {
        let mut out = *frame;
        out[bit / 8] ^= 1 << (7 - (bit % 8));
        out
    }

    fn with_pi(mut frame: [u8; 14]) -> [u8; 14] {
        frame[11] = 0;
        frame[12] = 0;
        frame[13] = 0;
        let crc = checksum(&frame);
        frame[11] = (crc >> 16) as u8;
        frame[12] = (crc >> 8) as u8;
        frame[13] = crc as u8;
        frame
    }

    #[test]
    fn syndromes_are_unique() {
        let table = syndromes();
        for i in 0..BITS {
            for j in 0..i {
                assert_ne!(table[i], table[j], "bits {i} and {j}");
            }
            assert_ne!(table[i], 0);
        }
    }

    #[test]
    fn a_good_frame_is_left_alone() {
        assert!(correct_extended_squitter(&EVEN).is_none());
        assert_eq!(checksum(&EVEN), 0);
        assert!(Message::try_from(EVEN.as_slice()).is_ok());
    }

    #[test]
    fn every_single_bit_error_is_repaired() {
        for bit in 0..BITS {
            let damaged = flip(&EVEN, bit);
            assert_ne!(damaged, EVEN);
            assert_ne!(checksum(&damaged), 0);
            let fixed = correct_extended_squitter(&damaged)
                .unwrap_or_else(|| panic!("bit {bit} not repaired, crc={:#x}", checksum(&damaged)));
            assert_eq!(fixed, EVEN, "bit {bit}");
            assert!(Message::try_from(fixed.as_slice()).is_ok(), "bit {bit}");
        }
    }

    #[test]
    fn a_repaired_even_still_pairs_with_odd() {
        let damaged = flip(&EVEN, 50);
        let mut tracker = Tracker::new();
        assert!(tracker.ingest(1.0, &damaged).is_none());
        let fix = tracker.ingest(2.0, &ODD).expect("position");
        assert_eq!(fix.aircraft.icao.to_u24(), 0x0040_058B);
    }

    #[test]
    fn two_bad_bits_are_not_repaired() {
        let table = syndromes();
        let mut pair = None;
        'outer: for i in 0..BITS {
            for j in (i + 1)..BITS {
                let syn = table[i] ^ table[j];
                if !table.contains(&syn) {
                    pair = Some((i, j));
                    break 'outer;
                }
            }
        }
        let (i, j) = pair.expect("a 2-bit syndrome outside the 1-bit table");
        let damaged = flip(&flip(&EVEN, i), j);
        assert!(
            correct_extended_squitter(&damaged).is_none(),
            "bits {i}+{j} crc={:#x}",
            checksum(&damaged)
        );
        assert!(Message::try_from(damaged.as_slice()).is_err());
        let mut tracker = Tracker::new();
        assert!(tracker.ingest(1.0, &damaged).is_none());
        assert!(tracker.ingest(2.0, &ODD).is_none());
    }

    #[test]
    fn df18_single_bit_is_repaired() {
        let mut df18 = EVEN;
        df18[0] = (18 << 3) | (df18[0] & 7);
        let df18 = with_pi(df18);
        assert_eq!(df18[0] >> 3, 18);
        assert_eq!(checksum(&df18), 0);
        for bit in 0..BITS {
            let damaged = flip(&df18, bit);
            assert_eq!(
                correct_extended_squitter(&damaged).as_ref(),
                Some(&df18),
                "df18 bit {bit}"
            );
        }
    }

    #[test]
    fn address_parity_is_not_corrected() {
        let mut df4 = [0u8; 7];
        df4[0] = 4 << 3;
        df4[1] = 0x12;
        df4[2] = 0x34;
        df4[3] = 0x56;
        assert!(correct_extended_squitter(&df4).is_none());
        let mut long = EVEN;
        long[0] = 4 << 3;
        assert_eq!(long[0] >> 3, 4);
        assert!(correct_extended_squitter(&long).is_none());
    }
}

//! DF17 Mode S / ADS-B frames.

pub const EVEN: [u8; 14] = [
    0x8D, 0x40, 0x05, 0x8B, 0x58, 0xC9, 0x01, 0x37, 0x51, 0x47, 0xEF, 0xD0, 0x93, 0x57,
];
pub const ODD: [u8; 14] = [
    0x8D, 0x40, 0x05, 0x8B, 0x58, 0xC9, 0x04, 0xA8, 0x7F, 0x40, 0x2D, 0x3B, 0x8C, 0x59,
];
pub const IDENT: [u8; 14] = [
    0x8D, 0x40, 0x05, 0x8B, 0x20, 0x50, 0x54, 0xD4, 0xC7, 0x2C, 0xE0, 0x3F, 0x94, 0x9E,
];
pub const VELOCITY: [u8; 14] = [
    0x8D, 0x40, 0x05, 0x8B, 0x99, 0x04, 0x02, 0x7F, 0xC0, 0x00, 0x00, 0xB1, 0x41, 0x47,
];
pub const STATUS: [u8; 14] = [
    0x8D, 0x40, 0x05, 0x8B, 0xE1, 0x20, 0x00, 0x00, 0x00, 0x00, 0x00, 0xA0, 0x5F, 0x30,
];

const CPR_NBITS: f64 = 131_072.0;
const NZ: f64 = 15.0;

/// NL(lat) zone-count thresholds from 1090-WP-9-14 / dump1090.
const NL_BOUND: [f64; 58] = [
    10.470_471_30,
    14.828_174_37,
    18.186_263_57,
    21.029_394_93,
    23.545_044_87,
    25.829_247_07,
    27.938_987_10,
    29.911_356_86,
    31.772_097_08,
    33.539_934_36,
    35.228_995_98,
    36.850_251_08,
    38.412_418_92,
    39.922_566_84,
    41.386_518_32,
    42.809_140_12,
    44.194_549_51,
    45.546_267_23,
    46.867_332_52,
    48.160_391_28,
    49.427_764_39,
    50.671_501_66,
    51.893_424_69,
    53.095_161_53,
    54.278_174_72,
    55.443_784_44,
    56.593_187_56,
    57.727_473_54,
    58.847_637_76,
    59.954_592_77,
    61.049_177_74,
    62.132_166_59,
    63.204_274_79,
    64.266_165_23,
    65.318_453_10,
    66.361_710_08,
    67.396_467_74,
    68.423_220_22,
    69.442_426_31,
    70.454_510_75,
    71.459_864_73,
    72.458_845_45,
    73.451_774_42,
    74.438_934_16,
    75.420_562_57,
    76.396_843_91,
    77.367_894_61,
    78.333_740_83,
    79.294_282_25,
    80.249_232_13,
    81.198_013_49,
    82.139_569_81,
    83.071_994_45,
    83.991_735_63,
    84.891_661_91,
    85.755_416_21,
    86.535_369_98,
    87.000_000_00,
];

pub fn with_icao(template: &[u8; 14], icao: u32) -> [u8; 14] {
    df17(icao, template[4..11].try_into().expect("ME"))
}

pub fn df17(icao: u32, me: [u8; 7]) -> [u8; 14] {
    let mut frame = [0u8; 14];
    frame[0] = 0x8D;
    frame[1] = (icao >> 16) as u8;
    frame[2] = (icao >> 8) as u8;
    frame[3] = icao as u8;
    frame[4..11].copy_from_slice(&me);
    let crc = rs1090::decode::crc::modes_checksum(&frame, 112).expect("DF17");
    frame[11] = (crc >> 16) as u8;
    frame[12] = (crc >> 8) as u8;
    frame[13] = crc as u8;
    frame
}

pub fn hex14(frame: &[u8; 14]) -> String {
    hex::encode_upper(frame)
}

pub fn airborne_position(icao: u32, lat: f64, lon: f64, alt_ft: i32, odd: bool) -> [u8; 14] {
    let (yz, xz) = cpr_encode(lat, lon, odd);
    let alt = encode_ac12(alt_ft);
    let me = pack56(&[
        (5, 11),
        (2, 0),
        (1, 0),
        (12, alt),
        (1, 0),
        (1, u32::from(odd)),
        (17, yz),
        (17, xz),
    ]);
    df17(icao, me)
}

pub fn identification(icao: u32, callsign: &str) -> [u8; 14] {
    let mut chars = [32u32; 8];
    for (i, ch) in callsign.chars().take(8).enumerate() {
        chars[i] = ais_char(ch);
    }
    let fields = [
        (5, 4),
        (3, 0),
        (6, chars[0]),
        (6, chars[1]),
        (6, chars[2]),
        (6, chars[3]),
        (6, chars[4]),
        (6, chars[5]),
        (6, chars[6]),
        (6, chars[7]),
    ];
    df17(icao, pack56(&fields))
}

pub fn velocity(icao: u32, heading_deg: f64, speed_kt: f64) -> [u8; 14] {
    let h = heading_deg.to_radians();
    let vew = speed_kt * h.sin();
    let vns = speed_kt * h.cos();
    let (ew_dir, ew_vel) = vel_comp(vew);
    let (ns_dir, ns_vel) = vel_comp(vns);
    let me = pack56(&[
        (5, 19),
        (3, 1),
        (1, 0),
        (1, 0),
        (3, 0),
        (1, ew_dir),
        (10, ew_vel),
        (1, ns_dir),
        (10, ns_vel),
        (1, 0),
        (1, 0),
        (9, 0),
        (1, 0),
        (1, 0),
        (8, 0),
    ]);
    df17(icao, me)
}

pub fn status(icao: u32, squawk: u16) -> [u8; 14] {
    let emergency = match squawk {
        0x7700 => 1,
        0x7600 => 4,
        0x7500 => 5,
        _ => 0,
    };
    let me = pack56(&[
        (5, 28),
        (3, 1),
        (3, emergency),
        (13, u32::from(encode_id13(squawk))),
        (32, 0),
    ]);
    df17(icao, me)
}

fn vel_comp(v: f64) -> (u32, u32) {
    let dir = u32::from(v < 0.0);
    let mag = v.abs().round().clamp(0.0, 1022.0) as u32;
    (dir, mag + 1)
}

fn encode_ac12(alt_ft: i32) -> u32 {
    let n = ((alt_ft + 1000) / 25).clamp(0, 0x7FF) as u32;
    ((n & 0xFF0) << 1) | 0x10 | (n & 0x0F)
}

fn encode_id13(squawk: u16) -> u16 {
    let mut id = 0u16;
    if squawk & 0x0010 != 0 {
        id |= 0x1000;
    }
    if squawk & 0x1000 != 0 {
        id |= 0x0800;
    }
    if squawk & 0x0020 != 0 {
        id |= 0x0400;
    }
    if squawk & 0x2000 != 0 {
        id |= 0x0200;
    }
    if squawk & 0x0040 != 0 {
        id |= 0x0100;
    }
    if squawk & 0x4000 != 0 {
        id |= 0x0080;
    }
    if squawk & 0x0100 != 0 {
        id |= 0x0020;
    }
    if squawk & 0x0001 != 0 {
        id |= 0x0010;
    }
    if squawk & 0x0200 != 0 {
        id |= 0x0008;
    }
    if squawk & 0x0002 != 0 {
        id |= 0x0004;
    }
    if squawk & 0x0400 != 0 {
        id |= 0x0002;
    }
    if squawk & 0x0004 != 0 {
        id |= 0x0001;
    }
    id
}

fn ais_char(ch: char) -> u32 {
    match ch {
        'A'..='Z' => u32::from(ch as u8 - b'A') + 1,
        '0'..='9' => u32::from(ch as u8 - b'0') + 48,
        _ => 32,
    }
}

fn pack56(fields: &[(u8, u32)]) -> [u8; 7] {
    let mut acc = 0u64;
    let mut bits = 0u32;
    for &(width, value) in fields {
        let mask = if width == 32 {
            u32::MAX
        } else {
            (1u32 << width) - 1
        };
        acc = (acc << width) | u64::from(value & mask);
        bits += u32::from(width);
    }
    debug_assert_eq!(bits, 56);
    let bytes = acc.to_be_bytes();
    [
        bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ]
}

fn cpr_nl(lat: f64) -> u32 {
    let lat = lat.abs();
    for (i, bound) in NL_BOUND.iter().enumerate() {
        if lat < *bound {
            return 59 - i as u32;
        }
    }
    1
}

fn cpr_encode(lat: f64, lon: f64, odd: bool) -> (u32, u32) {
    let odd_i = u32::from(odd);
    let dlat = 360.0 / (4.0 * NZ - f64::from(odd_i));
    let yz = (CPR_NBITS * (lat.rem_euclid(dlat) / dlat) + 0.5).floor();
    let yz = yz as u32 & 0x1_FFFF;
    let rlat = dlat * (lat.div_euclid(dlat) + f64::from(yz) / CPR_NBITS);
    let nl = cpr_nl(rlat);
    let ni = nl.saturating_sub(odd_i).max(1);
    let dlon = 360.0 / f64::from(ni);
    let lon = lon.rem_euclid(360.0);
    let xz = (CPR_NBITS * (lon.rem_euclid(dlon) / dlon) + 0.5).floor();
    let xz = xz as u32 & 0x1_FFFF;
    (yz, xz)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EmergencyKind, Tracker};
    use rs1090::decode::crc::modes_checksum;

    fn remainder(frame: &[u8; 14]) -> u32 {
        modes_checksum(frame, 112).expect("DF17")
    }

    fn parity(frame: &[u8; 14]) -> u32 {
        let mut payload = *frame;
        payload[11..].fill(0);
        remainder(&payload)
    }

    #[test]
    fn crc_matches_known_df17() {
        assert_eq!(parity(&EVEN), 0xD0_9357);
        assert_eq!(parity(&ODD), 0x3B_8C59);
        assert_eq!(parity(&IDENT), 0x3F_949E);
        assert_eq!(parity(&VELOCITY), 0xB1_4147);
        assert_eq!(parity(&STATUS), 0xA0_5F30);
        assert_eq!(remainder(&EVEN), 0);
        assert_eq!(remainder(&IDENT), 0);
    }

    #[test]
    fn rewriting_icao_keeps_a_valid_crc() {
        let frame = with_icao(&EVEN, 0x40058B);
        assert_eq!(frame, EVEN);
        let patched = with_icao(&EVEN, 0x10_0000);
        assert_eq!(&patched[1..4], &[0x10, 0x00, 0x00]);
        assert_eq!(remainder(&patched), 0);
        assert_ne!(patched[11..], EVEN[11..]);
    }

    #[test]
    fn encoded_ident_matches_template() {
        assert_eq!(identification(0x40058B, "TEST123"), IDENT);
    }

    #[test]
    fn position_roundtrips_through_tracker() {
        let icao = 0xABCDEF;
        let lat = 50.0;
        let lon = 8.0;
        let alt = 35_000;
        let mut tracker = Tracker::new();
        let even = airborne_position(icao, lat, lon, alt, false);
        let odd = airborne_position(icao, lat, lon, alt, true);
        assert_eq!(remainder(&even), 0);
        assert!(tracker.ingest(1.0, &even).is_none());
        let fix = tracker.ingest(2.0, &odd).expect("position");
        assert_eq!(fix.aircraft.icao.to_u24(), icao);
        let got_lat = f64::from(fix.aircraft.latitude_e7) / 10_000_000.0;
        let got_lon = f64::from(fix.aircraft.longitude_e7) / 10_000_000.0;
        assert!((got_lat - lat).abs() < 0.001, "lat {got_lat}");
        assert!((got_lon - lon).abs() < 0.001, "lon {got_lon}");
        assert_eq!(fix.aircraft.altitude_ft, Some(alt));
    }

    #[test]
    fn ident_velocity_status_attach() {
        let icao = 0xABC001;
        let mut tracker = Tracker::new();
        assert!(
            tracker
                .ingest(1.0, &identification(icao, "TAM1234"))
                .is_none()
        );
        assert!(tracker.ingest(1.1, &status(icao, 0x7700)).is_none());
        assert!(tracker.ingest(1.2, &velocity(icao, 90.0, 400.0)).is_none());
        assert!(
            tracker
                .ingest(1.3, &airborne_position(icao, 50.0, 8.0, 30_000, false))
                .is_none()
        );
        let fix = tracker
            .ingest(1.4, &airborne_position(icao, 50.0, 8.0, 30_000, true))
            .expect("position");
        assert_eq!(fix.aircraft.callsign.as_bytes(), *b"TAM1234 ");
        assert_eq!(fix.aircraft.emergency, Some(EmergencyKind::General));
        assert_eq!(fix.aircraft.ground_speed_kt, Some(400));
        assert_eq!(fix.aircraft.track_deg, Some(90));
    }
}

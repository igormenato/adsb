//! One UART frame for one aircraft. Little-endian.

use crc::{Crc, CRC_16_IBM_3740};

/// CRC-16/CCITT-FALSE: poly `0x1021`, init `0xFFFF`, not reflected, xorout `0`.
/// The `crc` catalog names this `CRC_16_IBM_3740`. Check value is `0x29B1`.
const CCITT_FALSE: Crc<u16> = Crc::<u16>::new(&CRC_16_IBM_3740);

pub const TRACK_MAGIC: &[u8; 4] = b"TRCK";
pub const FRAME_LEN: usize = 30;
pub const TRACK_MAX_AIRCRAFT: usize = 64;
pub const SPEED_UNKNOWN: u16 = 0xFFFF;
pub const HEADING_UNKNOWN: u16 = 0xFFFF;
pub const ALT_UNKNOWN: i32 = i32::MIN;

/// CRC-16/CCITT-FALSE. `crc16_ccitt_false(b"123456789") == 0x29B1`.
pub fn crc16_ccitt_false(data: &[u8]) -> u16 {
    CCITT_FALSE.checksum(data)
}

/// Emergency byte on the frame. `0` means none.
/// Codes: 1 general, 2 unlawful, 3 nordo, 4 lifeguard, 5 minfuel, 6 downed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum EmergencyKind {
    General = 1,
    Unlawful = 2,
    Nordo = 3,
    Lifeguard = 4,
    MinFuel = 5,
    Downed = 6,
}

impl EmergencyKind {
    pub fn code(self) -> u8 {
        self as u8
    }

    pub fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            1 => Self::General,
            2 => Self::Unlawful,
            3 => Self::Nordo,
            4 => Self::Lifeguard,
            5 => Self::MinFuel,
            6 => Self::Downed,
            _ => return None,
        })
    }
}

/// One aircraft that already has a position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Aircraft {
    pub icao: u32,
    pub latitude_e7: i32,
    pub longitude_e7: i32,
    pub altitude_ft: i32,
    pub ground_speed_kt: u16,
    pub heading_deg: u16,
    pub emergency: Option<EmergencyKind>,
}

/// One accepted position. `unix_s` is the line's `now`, truncated to seconds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fix {
    pub unix_s: u32,
    pub aircraft: Aircraft,
}

/// The qualifying tracks from one `aircraft.json`, capped at [`TRACK_MAX_AIRCRAFT`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub unix_s: u32,
    pub aircraft: Vec<Aircraft>,
}

/// `floor(|degrees| * 1e7 + 0.5)` with the original sign.
pub fn deg_e7(degrees: f64) -> i32 {
    let scaled = (degrees.abs() * 10_000_000.0 + 0.5).floor() as i32;
    if degrees < 0.0 {
        -scaled
    } else {
        scaled
    }
}

/// Magic, unix seconds, one aircraft, then CRC-16/CCITT-FALSE over the first 28 bytes.
pub fn pack_frame(fix: &Fix) -> [u8; FRAME_LEN] {
    let mut raw = [0u8; FRAME_LEN];
    raw[0..4].copy_from_slice(TRACK_MAGIC);
    raw[4..8].copy_from_slice(&fix.unix_s.to_le_bytes());
    let icao = (fix.aircraft.icao & 0x00ff_ffff).to_le_bytes();
    raw[8..11].copy_from_slice(&icao[..3]);
    raw[11..15].copy_from_slice(&fix.aircraft.latitude_e7.to_le_bytes());
    raw[15..19].copy_from_slice(&fix.aircraft.longitude_e7.to_le_bytes());
    raw[19..23].copy_from_slice(&fix.aircraft.altitude_ft.to_le_bytes());
    raw[23..25].copy_from_slice(&fix.aircraft.ground_speed_kt.to_le_bytes());
    raw[25..27].copy_from_slice(&fix.aircraft.heading_deg.to_le_bytes());
    raw[27] = fix.aircraft.emergency.map(EmergencyKind::code).unwrap_or(0);
    let crc = crc16_ccitt_false(&raw[..28]);
    raw[28..30].copy_from_slice(&crc.to_le_bytes());
    raw
}

pub fn unpack_frame(bytes: &[u8]) -> Option<Fix> {
    if bytes.len() != FRAME_LEN || &bytes[0..4] != TRACK_MAGIC {
        return None;
    }
    let expect = crc16_ccitt_false(&bytes[..28]);
    let got = u16::from_le_bytes([bytes[28], bytes[29]]);
    if expect != got {
        return None;
    }
    let unix_s = u32::from_le_bytes(bytes[4..8].try_into().ok()?);
    let mut icao = [0u8; 4];
    icao[..3].copy_from_slice(&bytes[8..11]);
    Some(Fix {
        unix_s,
        aircraft: Aircraft {
            icao: u32::from_le_bytes(icao),
            latitude_e7: i32::from_le_bytes(bytes[11..15].try_into().ok()?),
            longitude_e7: i32::from_le_bytes(bytes[15..19].try_into().ok()?),
            altitude_ft: i32::from_le_bytes(bytes[19..23].try_into().ok()?),
            ground_speed_kt: u16::from_le_bytes(bytes[23..25].try_into().ok()?),
            heading_deg: u16::from_le_bytes(bytes[25..27].try_into().ok()?),
            emergency: match bytes[27] {
                0 => None,
                code => Some(EmergencyKind::from_code(code)?),
            },
        },
    })
}

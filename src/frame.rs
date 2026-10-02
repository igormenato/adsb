//! One UART snapshot of aircraft tracks. Little-endian.

use crc::{Crc, CRC_16_IBM_3740};

/// CRC-16/CCITT-FALSE: poly `0x1021`, init `0xFFFF`, not reflected, xorout `0`.
/// The `crc` catalog names this `CRC_16_IBM_3740`. Check value is `0x29B1`.
const CCITT_FALSE: Crc<u16> = Crc::<u16>::new(&CRC_16_IBM_3740);

pub const TRACK_MAGIC: &[u8; 4] = b"TRCK";
pub const TRACK_VERSION: u8 = 1;
pub const TRACK_HEADER_LEN: usize = 12;
pub const TRACK_RECORD_LEN: usize = 20;
pub const TRACK_MAX_AIRCRAFT: usize = 64;
pub const SPEED_UNKNOWN: u16 = 0xFFFF;
pub const HEADING_UNKNOWN: u16 = 0xFFFF;
pub const ALT_UNKNOWN: i32 = i32::MIN;

/// CRC-16/CCITT-FALSE. `crc16_ccitt_false(b"123456789") == 0x29B1`.
pub fn crc16_ccitt_false(data: &[u8]) -> u16 {
    CCITT_FALSE.checksum(data)
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
}

/// The tracks from one `aircraft.json`, capped at [`TRACK_MAX_AIRCRAFT`].
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

pub fn pack_snapshot(snapshot: &Snapshot) -> Vec<u8> {
    let count = u16::try_from(snapshot.aircraft.len()).unwrap_or(u16::MAX);
    let mut raw =
        Vec::with_capacity(TRACK_HEADER_LEN + snapshot.aircraft.len() * TRACK_RECORD_LEN + 2);
    raw.extend_from_slice(TRACK_MAGIC);
    raw.push(TRACK_VERSION);
    raw.push(0);
    raw.extend_from_slice(&count.to_le_bytes());
    raw.extend_from_slice(&snapshot.unix_s.to_le_bytes());
    for aircraft in &snapshot.aircraft {
        raw.extend_from_slice(&(aircraft.icao & 0x00ff_ffff).to_le_bytes());
        raw.extend_from_slice(&aircraft.latitude_e7.to_le_bytes());
        raw.extend_from_slice(&aircraft.longitude_e7.to_le_bytes());
        raw.extend_from_slice(&aircraft.altitude_ft.to_le_bytes());
        raw.extend_from_slice(&aircraft.ground_speed_kt.to_le_bytes());
        raw.extend_from_slice(&aircraft.heading_deg.to_le_bytes());
    }
    let crc = crc16_ccitt_false(&raw);
    raw.extend_from_slice(&crc.to_le_bytes());
    raw
}

pub fn unpack_snapshot(bytes: &[u8]) -> Option<Snapshot> {
    if bytes.len() < TRACK_HEADER_LEN + 2 || &bytes[0..4] != TRACK_MAGIC {
        return None;
    }
    if bytes[4] != TRACK_VERSION || bytes[5] != 0 {
        return None;
    }
    let count = u16::from_le_bytes([bytes[6], bytes[7]]) as usize;
    if count > TRACK_MAX_AIRCRAFT {
        return None;
    }
    let body = TRACK_HEADER_LEN + count * TRACK_RECORD_LEN;
    if bytes.len() != body + 2 {
        return None;
    }
    let expect = crc16_ccitt_false(&bytes[..body]);
    let got = u16::from_le_bytes([bytes[body], bytes[body + 1]]);
    if expect != got {
        return None;
    }
    let unix_s = u32::from_le_bytes(bytes[8..12].try_into().ok()?);
    let mut aircraft = Vec::with_capacity(count);
    for index in 0..count {
        let rec = &bytes[TRACK_HEADER_LEN + index * TRACK_RECORD_LEN..];
        aircraft.push(Aircraft {
            icao: u32::from_le_bytes(rec[0..4].try_into().ok()?) & 0x00ff_ffff,
            latitude_e7: i32::from_le_bytes(rec[4..8].try_into().ok()?),
            longitude_e7: i32::from_le_bytes(rec[8..12].try_into().ok()?),
            altitude_ft: i32::from_le_bytes(rec[12..16].try_into().ok()?),
            ground_speed_kt: u16::from_le_bytes(rec[16..18].try_into().ok()?),
            heading_deg: u16::from_le_bytes(rec[18..20].try_into().ok()?),
        });
    }
    Some(Snapshot { unix_s, aircraft })
}

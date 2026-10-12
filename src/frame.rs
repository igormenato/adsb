//! One UART snapshot of aircraft tracks. Little-endian.

use crc::{Crc, CRC_16_IBM_3740};

/// CRC-16/CCITT-FALSE: poly `0x1021`, init `0xFFFF`, not reflected, xorout `0`.
/// The `crc` catalog names this `CRC_16_IBM_3740`. Check value is `0x29B1`.
const CCITT_FALSE: Crc<u16> = Crc::<u16>::new(&CRC_16_IBM_3740);

pub const TRACK_MAGIC: &[u8; 4] = b"TRCK";
pub const TRACK_HEADER_LEN: usize = 9;
pub const TRACK_RECORD_LEN: usize = 21;
pub const TRACK_MAX_AIRCRAFT: usize = 64;
pub const SPEED_UNKNOWN: u16 = 0xFFFF;
pub const HEADING_UNKNOWN: u16 = 0xFFFF;
pub const ALT_UNKNOWN: i32 = i32::MIN;

/// CRC-16/CCITT-FALSE. `crc16_ccitt_false(b"123456789") == 0x29B1`.
pub fn crc16_ccitt_false(data: &[u8]) -> u16 {
    CCITT_FALSE.checksum(data)
}

/// Emergency on the aircraft record. `0` in the binary record, and the JSON key omitted, when none is set.
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

    pub fn as_str(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Unlawful => "unlawful",
            Self::Nordo => "nordo",
            Self::Lifeguard => "lifeguard",
            Self::MinFuel => "minfuel",
            Self::Downed => "downed",
        }
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
    /// Whole seconds of `seen_pos`, from 0 through 255.
    pub position_age_s: u8,
    pub emergency: Option<EmergencyKind>,
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
    let count = u8::try_from(snapshot.aircraft.len()).unwrap_or(u8::MAX);
    let mut raw =
        Vec::with_capacity(TRACK_HEADER_LEN + snapshot.aircraft.len() * TRACK_RECORD_LEN + 2);
    raw.extend_from_slice(TRACK_MAGIC);
    raw.push(count);
    raw.extend_from_slice(&snapshot.unix_s.to_le_bytes());
    for aircraft in &snapshot.aircraft {
        let icao = (aircraft.icao & 0x00ff_ffff).to_le_bytes();
        raw.extend_from_slice(&icao[..3]);
        raw.extend_from_slice(&aircraft.latitude_e7.to_le_bytes());
        raw.extend_from_slice(&aircraft.longitude_e7.to_le_bytes());
        raw.extend_from_slice(&aircraft.altitude_ft.to_le_bytes());
        raw.extend_from_slice(&aircraft.ground_speed_kt.to_le_bytes());
        raw.extend_from_slice(&aircraft.heading_deg.to_le_bytes());
        raw.push(aircraft.emergency.map(EmergencyKind::code).unwrap_or(0));
        raw.push(aircraft.position_age_s);
    }
    let crc = crc16_ccitt_false(&raw);
    raw.extend_from_slice(&crc.to_le_bytes());
    raw
}

/// One JSON object and a trailing newline. Keys follow [`Snapshot`] and [`Aircraft`] field order.
pub fn pack_snapshot_json(snapshot: &Snapshot) -> Vec<u8> {
    let mut raw = Vec::new();
    raw.extend_from_slice(b"{\"unix_s\":");
    push_num(&mut raw, snapshot.unix_s);
    raw.extend_from_slice(b",\"aircraft\":[");
    for (index, aircraft) in snapshot.aircraft.iter().enumerate() {
        if index > 0 {
            raw.push(b',');
        }
        raw.extend_from_slice(b"{\"icao\":");
        push_num(&mut raw, aircraft.icao);
        raw.extend_from_slice(b",\"latitude_e7\":");
        push_num(&mut raw, aircraft.latitude_e7);
        raw.extend_from_slice(b",\"longitude_e7\":");
        push_num(&mut raw, aircraft.longitude_e7);
        raw.extend_from_slice(b",\"altitude_ft\":");
        push_num(&mut raw, aircraft.altitude_ft);
        raw.extend_from_slice(b",\"ground_speed_kt\":");
        push_num(&mut raw, aircraft.ground_speed_kt);
        raw.extend_from_slice(b",\"heading_deg\":");
        push_num(&mut raw, aircraft.heading_deg);
        raw.extend_from_slice(b",\"position_age_s\":");
        push_num(&mut raw, aircraft.position_age_s);
        if let Some(emergency) = aircraft.emergency {
            raw.extend_from_slice(b",\"emergency\":\"");
            raw.extend_from_slice(emergency.as_str().as_bytes());
            raw.push(b'"');
        }
        raw.push(b'}');
    }
    raw.extend_from_slice(b"]}\n");
    raw
}

fn push_num(raw: &mut Vec<u8>, value: impl std::fmt::Display) {
    let text = value.to_string();
    raw.extend_from_slice(text.as_bytes());
}

pub fn unpack_snapshot(bytes: &[u8]) -> Option<Snapshot> {
    if bytes.len() < TRACK_HEADER_LEN + 2 || &bytes[0..4] != TRACK_MAGIC {
        return None;
    }
    let count = bytes[4] as usize;
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
    let unix_s = u32::from_le_bytes(bytes[5..9].try_into().ok()?);
    let mut aircraft = Vec::with_capacity(count);
    for index in 0..count {
        let rec = &bytes[TRACK_HEADER_LEN + index * TRACK_RECORD_LEN..];
        let mut icao = [0u8; 4];
        icao[..3].copy_from_slice(&rec[..3]);
        aircraft.push(Aircraft {
            icao: u32::from_le_bytes(icao),
            latitude_e7: i32::from_le_bytes(rec[3..7].try_into().ok()?),
            longitude_e7: i32::from_le_bytes(rec[7..11].try_into().ok()?),
            altitude_ft: i32::from_le_bytes(rec[11..15].try_into().ok()?),
            ground_speed_kt: u16::from_le_bytes(rec[15..17].try_into().ok()?),
            heading_deg: u16::from_le_bytes(rec[17..19].try_into().ok()?),
            position_age_s: rec[20],
            emergency: match rec[19] {
                0 => None,
                code => Some(EmergencyKind::from_code(code)?),
            },
        });
    }
    Some(Snapshot { unix_s, aircraft })
}

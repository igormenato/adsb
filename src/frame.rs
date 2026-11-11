//! One UART frame for one aircraft. Little-endian.

use std::fmt;

use crc::{CRC_16_IBM_3740, Crc};

/// CRC-16/CCITT-FALSE: poly `0x1021`, init `0xFFFF`, not reflected, xorout `0`.
/// The `crc` catalog names this `CRC_16_IBM_3740`. Check value is `0x29B1`.
const CCITT_FALSE: Crc<u16> = Crc::<u16>::new(&CRC_16_IBM_3740);

pub const TRACK_MAGIC: &[u8; 4] = b"TRCK";
pub const FRAME_LEN: usize = 38;
pub const CALLSIGN_LEN: usize = 8;
pub const SPEED_UNKNOWN: u16 = 0xFFFF;
pub const TRACK_UNKNOWN: u16 = 0xFFFF;
pub const ALT_UNKNOWN: i32 = i32::MIN;
const BODY_LEN: usize = FRAME_LEN - 2;

/// 24-bit Mode S address. The wire field is three bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Icao(u32);

impl Icao {
    pub const MAX: u32 = 0x00ff_ffff;

    pub const fn from_u24(n: u32) -> Option<Self> {
        if n <= Self::MAX { Some(Self(n)) } else { None }
    }

    pub const fn from_le_bytes(bytes: [u8; 3]) -> Self {
        Self(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], 0]))
    }

    pub const fn to_u24(self) -> u32 {
        self.0
    }

    pub const fn to_le_bytes(self) -> [u8; 3] {
        let bytes = self.0.to_le_bytes();
        [bytes[0], bytes[1], bytes[2]]
    }
}

impl fmt::LowerHex for Icao {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::LowerHex::fmt(&self.0, f)
    }
}

const fn icao(n: u32) -> Icao {
    match Icao::from_u24(n) {
        Some(icao) => icao,
        None => panic!("ICAO exceeds 24 bits"),
    }
}

/// CRC-16/CCITT-FALSE. `crc16_ccitt_false(b"123456789") == 0x29B1`.
pub fn crc16_ccitt_false(data: &[u8]) -> u16 {
    CCITT_FALSE.checksum(data)
}

/// Emergency byte on the frame. `0` means none.
/// Codes match the ADS-B emergency/priority status field, and each
/// discriminant is explicit: 1 general, 2 lifeguard, 3 minfuel, 4 nordo,
/// 5 unlawful, 6 downed, 7 reserved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum EmergencyKind {
    General = 1,
    Lifeguard = 2,
    MinFuel = 3,
    Nordo = 4,
    Unlawful = 5,
    Downed = 6,
    Reserved = 7,
}

impl EmergencyKind {
    pub fn code(self) -> u8 {
        self as u8
    }

    pub fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            1 => Self::General,
            2 => Self::Lifeguard,
            3 => Self::MinFuel,
            4 => Self::Nordo,
            5 => Self::Unlawful,
            6 => Self::Downed,
            7 => Self::Reserved,
            _ => return None,
        })
    }
}

/// Eight-byte ADS-B ident. The wire field is space-padded ASCII.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Callsign([u8; CALLSIGN_LEN]);

impl Callsign {
    pub fn pad(text: &str) -> Self {
        let mut bytes = [b' '; CALLSIGN_LEN];
        let src = text.as_bytes();
        let n = src.len().min(CALLSIGN_LEN);
        bytes[..n].copy_from_slice(&src[..n]);
        Self(bytes)
    }

    pub const fn from_bytes(bytes: [u8; CALLSIGN_LEN]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(self) -> [u8; CALLSIGN_LEN] {
        self.0
    }
}

impl Default for Callsign {
    fn default() -> Self {
        Self([b' '; CALLSIGN_LEN])
    }
}

/// One aircraft that already has a position.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Aircraft {
    pub icao: Icao,
    pub callsign: Callsign,
    pub latitude_e7: i32,
    pub longitude_e7: i32,
    pub altitude_ft: Option<i32>,
    pub ground_speed_kt: Option<u16>,
    pub track_deg: Option<u16>,
    pub emergency: Option<EmergencyKind>,
}

/// One accepted position. `unix_s` is the message time, truncated to seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fix {
    pub unix_s: u32,
    pub aircraft: Aircraft,
}

/// `floor(|degrees| * 1e7 + 0.5)` with the original sign.
pub fn deg_e7(degrees: f64) -> i32 {
    let scaled = (degrees.abs() * 10_000_000.0 + 0.5).floor() as i32;
    if degrees < 0.0 { -scaled } else { scaled }
}

pub fn pack_frame(fix: &Fix) -> [u8; FRAME_LEN] {
    let aircraft = &fix.aircraft;
    let mut raw = [0u8; FRAME_LEN];
    raw[0..4].copy_from_slice(TRACK_MAGIC);
    raw[4..8].copy_from_slice(&fix.unix_s.to_le_bytes());
    raw[8..11].copy_from_slice(&aircraft.icao.to_le_bytes());
    raw[11..19].copy_from_slice(&aircraft.callsign.as_bytes());
    raw[19..23].copy_from_slice(&aircraft.latitude_e7.to_le_bytes());
    raw[23..27].copy_from_slice(&aircraft.longitude_e7.to_le_bytes());
    raw[27..31].copy_from_slice(&aircraft.altitude_ft.unwrap_or(ALT_UNKNOWN).to_le_bytes());
    raw[31..33].copy_from_slice(
        &aircraft
            .ground_speed_kt
            .unwrap_or(SPEED_UNKNOWN)
            .to_le_bytes(),
    );
    raw[33..35].copy_from_slice(&aircraft.track_deg.unwrap_or(TRACK_UNKNOWN).to_le_bytes());
    raw[35] = aircraft.emergency.map(EmergencyKind::code).unwrap_or(0);
    let crc = crc16_ccitt_false(&raw[..BODY_LEN]);
    raw[BODY_LEN..FRAME_LEN].copy_from_slice(&crc.to_le_bytes());
    raw
}

pub fn check_positions() -> [Fix; 3] {
    const UNIX_S: u32 = 1_700_000_000;
    [
        Fix {
            unix_s: UNIX_S,
            aircraft: Aircraft {
                icao: icao(0x400ABC),
                callsign: Callsign::pad("SIM0001"),
                latitude_e7: deg_e7(10.5),
                longitude_e7: deg_e7(-20.25),
                altitude_ft: Some(12_000),
                ground_speed_kt: Some(280),
                track_deg: Some(90),
                emergency: None,
            },
        },
        Fix {
            unix_s: UNIX_S,
            aircraft: Aircraft {
                icao: icao(0x40621D),
                callsign: Callsign::pad("BAW123"),
                latitude_e7: deg_e7(52.2572021484375),
                longitude_e7: deg_e7(3.91937255859375),
                altitude_ft: Some(38_000),
                ground_speed_kt: Some(450),
                track_deg: Some(271),
                emergency: None,
            },
        },
        Fix {
            unix_s: UNIX_S,
            aircraft: Aircraft {
                icao: icao(0x4CA123),
                callsign: Callsign::pad("EZY456"),
                latitude_e7: deg_e7(51.47),
                longitude_e7: deg_e7(-0.45),
                altitude_ft: Some(8_000),
                ground_speed_kt: Some(220),
                track_deg: Some(180),
                emergency: Some(EmergencyKind::General),
            },
        },
    ]
}

pub fn unpack_frame(bytes: &[u8]) -> Option<Fix> {
    if bytes.len() != FRAME_LEN || &bytes[0..4] != TRACK_MAGIC {
        return None;
    }
    let expect = crc16_ccitt_false(&bytes[..BODY_LEN]);
    let got = u16::from_le_bytes(bytes[BODY_LEN..FRAME_LEN].try_into().ok()?);
    if expect != got {
        return None;
    }
    let unix_s = u32::from_le_bytes(bytes[4..8].try_into().ok()?);
    let altitude_ft = i32::from_le_bytes(bytes[27..31].try_into().ok()?);
    let ground_speed_kt = u16::from_le_bytes(bytes[31..33].try_into().ok()?);
    let track_deg = u16::from_le_bytes(bytes[33..35].try_into().ok()?);
    Some(Fix {
        unix_s,
        aircraft: Aircraft {
            icao: Icao::from_le_bytes(bytes[8..11].try_into().ok()?),
            callsign: Callsign::from_bytes(bytes[11..19].try_into().ok()?),
            latitude_e7: i32::from_le_bytes(bytes[19..23].try_into().ok()?),
            longitude_e7: i32::from_le_bytes(bytes[23..27].try_into().ok()?),
            altitude_ft: (altitude_ft != ALT_UNKNOWN).then_some(altitude_ft),
            ground_speed_kt: (ground_speed_kt != SPEED_UNKNOWN).then_some(ground_speed_kt),
            track_deg: (track_deg != TRACK_UNKNOWN).then_some(track_deg),
            emergency: match bytes[35] {
                0 => None,
                code => Some(EmergencyKind::from_code(code)?),
            },
        },
    })
}

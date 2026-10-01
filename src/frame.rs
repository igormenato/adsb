//! 32-byte little-endian record. Offsets match the layout table in the README.

use crc::{Crc, CRC_16_IBM_3740};

pub const ADSB_STRUCT_MAGIC: u16 = 0xAD5B;
pub const ADSB_STRUCT_VERSION: u8 = 1;
pub const ADSB_STRUCT_SIZE: usize = 32;
pub const ADSB_STRUCT_CRC_LEN: usize = 30;
pub const ADSB_WIRE_MAGIC: [u8; 2] = [0x5B, 0xAD];

pub const ADSB_FLAG_POSITION: u8 = 0x01;
pub const ADSB_FLAG_ALTITUDE: u8 = 0x02;
pub const ADSB_FLAG_VELOCITY: u8 = 0x04;

pub const ADSB_LATLON_INVALID: i32 = i32::MIN;
pub const ADSB_ALT_INVALID: i32 = i32::MIN;
pub const ADSB_VEL_INVALID: u16 = 0xFFFF;

/// CRC-16/CCITT-FALSE: poly `0x1021`, init `0xFFFF`, not reflected, xorout `0`.
/// The `crc` catalog names this `CRC_16_IBM_3740`. Check value is `0x29B1`.
const CCITT_FALSE: Crc<u16> = Crc::<u16>::new(&CRC_16_IBM_3740);

/// CRC-16/CCITT-FALSE. `crc16_ccitt_false(b"123456789") == 0x29B1`.
pub fn crc16_ccitt_false(data: &[u8]) -> u16 {
    CCITT_FALSE.checksum(data)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Track {
    pub icao: u32,
    pub flags: u8,
    pub latitude_e7: i32,
    pub longitude_e7: i32,
    pub altitude_ft: i32,
    pub velocity_kt: u16,
    pub timestamp_us: u64,
}

impl Track {
    pub fn empty(icao: u32, timestamp_us: u64) -> Self {
        Self {
            icao: icao & 0x00ff_ffff,
            flags: 0,
            latitude_e7: ADSB_LATLON_INVALID,
            longitude_e7: ADSB_LATLON_INVALID,
            altitude_ft: ADSB_ALT_INVALID,
            velocity_kt: ADSB_VEL_INVALID,
            timestamp_us,
        }
    }
}

pub fn pack_message(msg: &Track) -> [u8; ADSB_STRUCT_SIZE] {
    let mut raw = [0u8; ADSB_STRUCT_SIZE];
    raw[0..2].copy_from_slice(&ADSB_STRUCT_MAGIC.to_le_bytes());
    raw[2] = ADSB_STRUCT_VERSION;
    raw[3] = msg.flags;
    raw[4..8].copy_from_slice(&(msg.icao & 0x00ff_ffff).to_le_bytes());
    raw[8..12].copy_from_slice(&msg.latitude_e7.to_le_bytes());
    raw[12..16].copy_from_slice(&msg.longitude_e7.to_le_bytes());
    raw[16..20].copy_from_slice(&msg.altitude_ft.to_le_bytes());
    raw[20..22].copy_from_slice(&msg.velocity_kt.to_le_bytes());
    raw[22..30].copy_from_slice(&msg.timestamp_us.to_le_bytes());
    let crc = crc16_ccitt_false(&raw[..ADSB_STRUCT_CRC_LEN]);
    raw[30..32].copy_from_slice(&crc.to_le_bytes());
    raw
}

pub fn unpack_message(frame: &[u8]) -> Option<Track> {
    if frame.len() != ADSB_STRUCT_SIZE || frame[0..2] != ADSB_WIRE_MAGIC {
        return None;
    }
    let expect = crc16_ccitt_false(&frame[..ADSB_STRUCT_CRC_LEN]);
    let got = u16::from_le_bytes([frame[30], frame[31]]);
    if expect != got || frame[2] != ADSB_STRUCT_VERSION {
        return None;
    }
    Some(Track {
        icao: u32::from_le_bytes(frame[4..8].try_into().ok()?) & 0x00ff_ffff,
        flags: frame[3],
        latitude_e7: i32::from_le_bytes(frame[8..12].try_into().ok()?),
        longitude_e7: i32::from_le_bytes(frame[12..16].try_into().ok()?),
        altitude_ft: i32::from_le_bytes(frame[16..20].try_into().ok()?),
        velocity_kt: u16::from_le_bytes(frame[20..22].try_into().ok()?),
        timestamp_us: u64::from_le_bytes(frame[22..30].try_into().ok()?),
    })
}

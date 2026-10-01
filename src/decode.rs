//! DF17/DF18 decode through rs1090. One record per squitter, not a fused track.
//!
//! Position stays invalid until an even and an odd CPR frame for that
//! address arrive within 10 seconds. rs1090's own tracker waits 30 seconds
//! and then fills later squitters from a reference fix; this sender does not.

use std::collections::HashMap;

use rs1090::decode::adsb::ME;
use rs1090::decode::bds::bds05::{AirbornePosition, Source};
use rs1090::decode::bds::bds09::AirborneVelocitySubType;
use rs1090::decode::cpr::{airborne_position, CPRFormat};
use rs1090::decode::crc::modes_checksum;
use rs1090::decode::{Message, DF};

use crate::beast::Stream;
use crate::frame::{
    pack_message, Track, ADSB_FLAG_ALTITUDE, ADSB_FLAG_POSITION, ADSB_FLAG_VELOCITY,
};

const CPR_PAIR_WINDOW_S: f64 = 10.0;
const VEL_MAX: u16 = 65534;

/// Mode S CRC-24 remainder. A valid DF17/DF18 squitter returns 0.
pub fn modes_crc(msg: &[u8]) -> u32 {
    modes_checksum(msg, msg.len() * 8).unwrap_or(u32::MAX)
}

/// Parity bytes to append so [`modes_crc`] of the result is 0.
pub fn modes_parity(data: &[u8]) -> [u8; 3] {
    let mut msg = Vec::with_capacity(data.len() + 3);
    msg.extend_from_slice(data);
    msg.extend_from_slice(&[0, 0, 0]);
    let crc = modes_crc(&msg);
    [(crc >> 16) as u8, (crc >> 8) as u8, crc as u8]
}

fn deg_e7(degrees: f64) -> i32 {
    let scaled = (degrees.abs() * 10_000_000.0 + 0.5).floor() as i32;
    if degrees < 0.0 {
        -scaled
    } else {
        scaled
    }
}

/// 12-bit GNSS height in the ME field, in meters.
fn gnss_meters(payload: &[u8; 14]) -> u16 {
    (u16::from(payload[5]) << 4) | u16::from(payload[6] >> 4)
}

/// Same integer conversion rs1090 uses for meters: `trunc(meters * 3.28084)`.
fn gnss_feet(meters: u16) -> i32 {
    ((i64::from(meters) * 328_084) / 100_000) as i32
}

/// True when both ground-speed components are present. A raw magnitude of 0
/// means "not available"; rs1090 turns that into ±1 kt.
fn ground_speed_available(payload: &[u8; 14]) -> bool {
    let me = &payload[4..11];
    let east = (u32::from(me[1] & 0x03) << 8) | u32::from(me[2]);
    let north = (u32::from(me[3] & 0x7f) << 3) | u32::from(me[4] >> 5);
    east != 0 && north != 0
}

fn speed_kt(subtype: u8, groundspeed: f64) -> Option<u16> {
    if subtype != 1 && subtype != 2 {
        return None;
    }
    // Subtype 2 is a 4 kt step. rs1090's groundspeed field is not scaled.
    let knots = if subtype == 2 {
        groundspeed * 4.0
    } else {
        groundspeed
    };
    let rounded = knots.round().max(0.0) as u32;
    Some(u16::try_from(rounded).unwrap_or(u16::MAX).min(VEL_MAX))
}

struct CprFix {
    position: AirbornePosition,
    at: f64,
}

/// Turns a Beast byte stream into packed 32-byte records.
pub struct StructForwarder {
    stream: Stream,
    payloads: Vec<[u8; 14]>,
    even: HashMap<u32, CprFix>,
    odd: HashMap<u32, CprFix>,
    pub crc_drops: u64,
    pub sent: u64,
}

impl StructForwarder {
    pub fn new() -> Self {
        Self {
            stream: Stream::new(),
            payloads: Vec::new(),
            even: HashMap::new(),
            odd: HashMap::new(),
            crc_drops: 0,
            sent: 0,
        }
    }

    /// Decode `chunk`. `clock` is `(unix_seconds, unix_microseconds)` and is
    /// called once per CRC-valid DF17/DF18. Packed records are appended to `out`.
    pub fn feed(&mut self, chunk: &[u8], mut clock: impl FnMut() -> (f64, u64), out: &mut Vec<u8>) {
        self.stream.push_long(chunk, &mut self.payloads);
        let count = self.payloads.len();
        for index in 0..count {
            let payload = self.payloads[index];
            if let Some(record) = self.record(&payload, &mut clock) {
                out.extend_from_slice(&record);
                self.sent += 1;
            }
        }
    }

    fn record(
        &mut self,
        payload: &[u8; 14],
        clock: &mut impl FnMut() -> (f64, u64),
    ) -> Option<[u8; 32]> {
        let df = payload[0] >> 3;
        if df != 17 && df != 18 {
            return None;
        }
        if modes_crc(payload) != 0 {
            self.crc_drops += 1;
            return None;
        }
        let message = Message::try_from(payload.as_slice()).ok()?;
        let (icao, me) = match message.df {
            DF::ExtendedSquitterADSB(adsb) => (adsb.icao24.0, adsb.message),
            DF::ExtendedSquitterTisB { cf, .. } => (cf.aa.0, cf.me),
            _ => return None,
        };

        let (now_s, now_us) = clock();
        let mut track = Track::empty(icao, now_us);
        match me {
            ME::BDS05 { inner, .. } => {
                let altitude = match inner.source {
                    Source::Barometric => inner.alt,
                    Source::Gnss => Some(gnss_feet(gnss_meters(payload))),
                };
                if let Some(altitude) = altitude {
                    track.altitude_ft = altitude;
                    track.flags |= ADSB_FLAG_ALTITUDE;
                }
                if let Some((lat, lon)) = self.position(icao, &inner, now_s) {
                    track.latitude_e7 = deg_e7(lat);
                    track.longitude_e7 = deg_e7(lon);
                    track.flags |= ADSB_FLAG_POSITION;
                }
            }
            ME::BDS09(velocity) => {
                if let AirborneVelocitySubType::GroundSpeedDecoding(decoded) = velocity.velocity {
                    if ground_speed_available(payload) {
                        if let Some(speed) = speed_kt(velocity.subtype, decoded.groundspeed) {
                            track.velocity_kt = speed;
                            track.flags |= ADSB_FLAG_VELOCITY;
                        }
                    }
                }
            }
            _ => {}
        }
        Some(pack_message(&track))
    }

    fn position(&mut self, icao: u32, msg: &AirbornePosition, now_s: f64) -> Option<(f64, f64)> {
        let fix = CprFix {
            position: *msg,
            at: now_s,
        };
        match msg.parity {
            CPRFormat::Odd => {
                self.odd.insert(icao, fix);
            }
            CPRFormat::Even => {
                self.even.insert(icao, fix);
            }
        }
        let even = self.even.get(&icao)?;
        let odd = self.odd.get(&icao)?;
        if (even.at - odd.at).abs() > CPR_PAIR_WINDOW_S {
            return None;
        }
        let decoded = if even.at >= odd.at {
            airborne_position(&odd.position, &even.position)
        } else {
            airborne_position(&even.position, &odd.position)
        }?;
        Some((decoded.latitude, decoded.longitude))
    }
}

impl Default for StructForwarder {
    fn default() -> Self {
        Self::new()
    }
}

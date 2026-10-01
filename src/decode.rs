//! DF17/DF18 decode. One record per squitter, not a fused track.
//!
//! Position stays invalid until an even and an odd CPR frame for that
//! address arrive within 10 seconds. Altitude and velocity are filled
//! only when that squitter carries them.

use std::collections::HashMap;

use crate::beast::Stream;
use crate::frame::{
    pack_message, Track, ADSB_FLAG_ALTITUDE, ADSB_FLAG_POSITION, ADSB_FLAG_VELOCITY,
};

const CRC24_POLY: u32 = 0x00FF_F409;
const CRC24_TABLE: [u32; 256] = crc24_table();
const CPR_PAIR_WINDOW_S: f64 = 10.0;
const CPR_DENOM: f64 = 131_072.0;
const FT_PER_M: f64 = 3.28084;
const VEL_MAX: u16 = 65534;

/// Latitude where NL steps down. NL(lat) = 59 - index of the first boundary
/// strictly above `|lat|`. Same table as the DO-260B zone count.
const NL_BOUNDARIES: [f64; 58] = [
    10.47047129996848,
    14.828174368686794,
    18.186263570713354,
    21.029394926028463,
    23.545044865570706,
    25.829247070587755,
    27.938987101219045,
    29.911356857318083,
    31.77209707681077,
    33.53993436298484,
    35.22899597796385,
    36.85025107593526,
    38.41241892412256,
    39.922566843338615,
    41.38651832260239,
    42.80914012243555,
    44.194549514192744,
    45.546267226602346,
    46.867332524987454,
    48.160391280966216,
    49.42776439255687,
    50.67150165553835,
    51.893424691687684,
    53.09516152796003,
    54.278174722729,
    55.44378444495043,
    56.59318756205918,
    57.72747353866114,
    58.84763776148457,
    59.954592766940294,
    61.04917774246351,
    62.13216659210329,
    63.20427479381928,
    64.2661652256744,
    65.31845309682089,
    66.36171008382617,
    67.39646774084667,
    68.4232202208333,
    69.44242631144024,
    70.454510749876,
    71.45986473028982,
    72.45884544728945,
    73.45177441667865,
    74.43893415725137,
    75.42056256653356,
    76.39684390794469,
    77.36789461328188,
    78.33374082922747,
    79.29428225456925,
    80.24923213280512,
    81.19801349271948,
    82.13956980510606,
    83.07199444719814,
    83.99173562980565,
    84.89166190702085,
    85.75541620944418,
    86.535369975121,
    87.0,
];

const fn crc24_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = (i as u32) << 16;
        let mut bit = 0;
        while bit < 8 {
            if c & 0x0080_0000 != 0 {
                c = (c << 1) ^ CRC24_POLY;
            } else {
                c <<= 1;
            }
            bit += 1;
        }
        table[i] = c & 0x00ff_ffff;
        i += 1;
    }
    table
}

fn crc24_bytes(data: &[u8]) -> u32 {
    let mut crc = 0u32;
    for &byte in data {
        let index = (((crc >> 16) ^ u32::from(byte)) & 0xff) as usize;
        crc = ((crc << 8) & 0x00ff_ffff) ^ CRC24_TABLE[index];
    }
    crc
}

/// Mode S CRC-24 remainder of a message that already includes its 3 parity bytes.
/// A valid DF17/DF18 squitter returns 0.
pub fn modes_crc(msg: &[u8]) -> u32 {
    if msg.len() < 3 {
        return crc24_bytes(msg);
    }
    let (data, parity) = msg.split_at(msg.len() - 3);
    let crc = crc24_bytes(data);
    let parity = u32::from_be_bytes([0, parity[0], parity[1], parity[2]]);
    (crc ^ parity) & 0x00ff_ffff
}

/// Parity bytes to append so [`modes_crc`] of the result is 0.
pub fn modes_parity(data: &[u8]) -> [u8; 3] {
    let crc = crc24_bytes(data) & 0x00ff_ffff;
    [(crc >> 16) as u8, (crc >> 8) as u8, crc as u8]
}

fn nl(lat: f64) -> i32 {
    let abs_lat = lat.abs();
    if abs_lat > 87.0 {
        return 1;
    }
    if abs_lat == 87.0 {
        return 2;
    }
    let index = NL_BOUNDARIES.partition_point(|boundary| *boundary <= abs_lat);
    59 - index as i32
}

/// Globally unambiguous CPR. `even_*` / `odd_*` are the raw 17-bit fields.
/// `even_is_newer` selects which frame's zone the reported position uses.
fn cpr_global(
    even_lat: u32,
    even_lon: u32,
    odd_lat: u32,
    odd_lon: u32,
    even_is_newer: bool,
) -> Option<(f64, f64)> {
    let lat_even_cpr = f64::from(even_lat) / CPR_DENOM;
    let lon_even_cpr = f64::from(even_lon) / CPR_DENOM;
    let lat_odd_cpr = f64::from(odd_lat) / CPR_DENOM;
    let lon_odd_cpr = f64::from(odd_lon) / CPR_DENOM;

    let j = (59.0 * lat_even_cpr - 60.0 * lat_odd_cpr + 0.5).floor();
    let mut lat_even = (360.0 / 60.0) * (j.rem_euclid(60.0) + lat_even_cpr);
    let mut lat_odd = (360.0 / 59.0) * (j.rem_euclid(59.0) + lat_odd_cpr);
    if lat_even >= 270.0 {
        lat_even -= 360.0;
    }
    if lat_odd >= 270.0 {
        lat_odd -= 360.0;
    }
    if nl(lat_even) != nl(lat_odd) {
        return None;
    }

    let (lat, mut lon) = if even_is_newer {
        let zones = nl(lat_even);
        let ni = zones.max(1) as f64;
        let m = (lon_even_cpr * (f64::from(zones) - 1.0) - lon_odd_cpr * f64::from(zones) + 0.5)
            .floor();
        let lon = (360.0 / ni) * (m.rem_euclid(ni) + lon_even_cpr);
        (lat_even, lon)
    } else {
        let zones = nl(lat_odd);
        let ni = (zones - 1).max(1) as f64;
        let m = (lon_even_cpr * (f64::from(zones) - 1.0) - lon_odd_cpr * f64::from(zones) + 0.5)
            .floor();
        let lon = (360.0 / ni) * (m.rem_euclid(ni) + lon_odd_cpr);
        (lat_odd, lon)
    };
    if lon > 180.0 {
        lon -= 360.0;
    }
    if lat.abs() > 90.0 || lon.abs() > 180.0 {
        None
    } else {
        Some((lat, lon))
    }
}

fn deg_e7(degrees: f64) -> i32 {
    let scaled = (degrees.abs() * 10_000_000.0 + 0.5).floor() as i32;
    if degrees < 0.0 {
        -scaled
    } else {
        scaled
    }
}

/// 12-bit altitude field inside the 7-byte ME.
fn alt12(me: &[u8]) -> u16 {
    (u16::from(me[1]) << 4) | u16::from(me[2] >> 4)
}

/// Barometric altitude, 25 ft steps. Q=0 Gillham coding is left invalid.
fn baro_alt_ft(ac: u16) -> Option<i32> {
    if (ac >> 4) & 1 == 0 {
        return None;
    }
    let n = (i32::from(ac >> 5) << 4) | i32::from(ac & 0x0f);
    Some(n * 25 - 1000)
}

/// GNSS height: the 12-bit field is meters. Truncate `meters * 3.28084` toward zero.
fn gnss_alt_ft(ac: u16) -> i32 {
    (f64::from(ac) * FT_PER_M) as i32
}

/// Airborne velocity, type code 19 subtypes 1 and 2. Truncated hypot, knots.
fn ground_speed_kt(me: &[u8]) -> Option<u16> {
    let subtype = me[0] & 0x07;
    if subtype != 1 && subtype != 2 {
        return None;
    }
    let mut east = (u32::from(me[1] & 0x03) << 8) | u32::from(me[2]);
    let mut north = (u32::from(me[3] & 0x7f) << 3) | u32::from(me[4] >> 5);
    if east == 0 || north == 0 {
        return None;
    }
    east -= 1;
    north -= 1;
    if subtype == 2 {
        east *= 4;
        north *= 4;
    }
    let speed = (f64::from(east).hypot(f64::from(north))) as u32;
    Some(u16::try_from(speed).unwrap_or(u16::MAX).min(VEL_MAX))
}

fn cpr_fields(me: &[u8]) -> (bool, u32, u32) {
    let odd = (me[2] >> 2) & 1 == 1;
    let lat = (u32::from(me[2] & 0x03) << 15) | (u32::from(me[3]) << 7) | u32::from(me[4] >> 1);
    let lon = (u32::from(me[4] & 0x01) << 16) | (u32::from(me[5]) << 8) | u32::from(me[6]);
    (odd, lat, lon)
}

struct CprFix {
    lat: u32,
    lon: u32,
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
        let (now_s, now_us) = clock();
        let icao = u32::from_be_bytes([0, payload[1], payload[2], payload[3]]);
        let mut track = Track::empty(icao, now_us);
        let me = &payload[4..11];
        let tc = me[0] >> 3;

        if (9..=18).contains(&tc) || (20..=22).contains(&tc) {
            let ac = alt12(me);
            let alt = if (9..=18).contains(&tc) {
                baro_alt_ft(ac)
            } else {
                Some(gnss_alt_ft(ac))
            };
            if let Some(alt) = alt {
                track.altitude_ft = alt;
                track.flags |= ADSB_FLAG_ALTITUDE;
            }
            if let Some((lat, lon)) = self.position(icao, me, now_s) {
                track.latitude_e7 = deg_e7(lat);
                track.longitude_e7 = deg_e7(lon);
                track.flags |= ADSB_FLAG_POSITION;
            }
        } else if tc == 19 {
            if let Some(speed) = ground_speed_kt(me) {
                track.velocity_kt = speed;
                track.flags |= ADSB_FLAG_VELOCITY;
            }
        }

        Some(pack_message(&track))
    }

    fn position(&mut self, icao: u32, me: &[u8], now_s: f64) -> Option<(f64, f64)> {
        let (odd, lat, lon) = cpr_fields(me);
        let fix = CprFix {
            lat,
            lon,
            at: now_s,
        };
        if odd {
            self.odd.insert(icao, fix);
        } else {
            self.even.insert(icao, fix);
        }
        let even = self.even.get(&icao)?;
        let odd_fix = self.odd.get(&icao)?;
        if (even.at - odd_fix.at).abs() > CPR_PAIR_WINDOW_S {
            return None;
        }
        let even_is_newer = even.at >= odd_fix.at;
        cpr_global(even.lat, even.lon, odd_fix.lat, odd_fix.lon, even_is_newer)
    }
}

impl Default for StructForwarder {
    fn default() -> Self {
        Self::new()
    }
}

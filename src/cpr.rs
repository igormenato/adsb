//! Compact Position Reporting.
//!
//! Global even/odd and relative decode follow readsb `cpr.c`
//! (decodeCPRairborne, decodeCPRrelative). Pairing windows and the
//! speed check follow readsb `track.c` (`cpr_global_airborne_max_elapsed`,
//! dump1090-fa `speed_check` as the simpler of the two). NL(lat) is
//! 1090-WP-9-14.

use crate::modes::cpr_nl;

const CPR_MAX: f64 = 131_072.0;
const EARTH_M: f64 = 6_371_000.0;
/// readsb `doLocalCPR`: 100 NM from the last aircraft position.
const LOCAL_RANGE_M: f64 = 1852.0 * 100.0;
const KT_MS: f64 = 1852.0 / 3600.0;

/// dump1090 / readsb pair airborne even/odd within 10 s when speed is unknown.
pub const AIRBORNE_PAIR_S: f64 = 10.0;
/// Surface: 25 s, or 50 s when groundspeed is known and ≤ 25 kt. readsb `track.c`.
pub const SURFACE_PAIR_S: f64 = 25.0;
pub const SURFACE_SLOW_PAIR_S: f64 = 50.0;

#[derive(Clone, Copy, Debug)]
pub struct CprBits {
    pub odd: bool,
    pub surface: bool,
    pub lat: u32,
    pub lon: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct LatLon {
    pub lat: f64,
    pub lon: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GlobalFail {
    Zone,
    Bad,
}

pub fn pair_window_s(surface: bool, gs_kt: Option<u16>) -> f64 {
    if surface {
        if gs_kt.is_some_and(|gs| gs <= 25) {
            SURFACE_SLOW_PAIR_S
        } else {
            SURFACE_PAIR_S
        }
    } else {
        match gs_kt {
            None => AIRBORNE_PAIR_S,
            Some(gs) => {
                let speed = f64::from(gs.max(1));
                ((19.0 * 500.0) / speed).min(30.0)
            }
        }
    }
}

pub fn airborne_global(
    even: CprBits,
    odd: CprBits,
    latest_odd: bool,
) -> Result<LatLon, GlobalFail> {
    let lat0 = f64::from(even.lat);
    let lat1 = f64::from(odd.lat);
    let lon0 = f64::from(even.lon);
    let lon1 = f64::from(odd.lon);
    let j = ((59.0 * lat0 - 60.0 * lat1) / CPR_MAX + 0.5).floor() as i32;
    let mut rlat0 = (360.0 / 60.0) * (f64::from(j.rem_euclid(60)) + lat0 / CPR_MAX);
    let mut rlat1 = (360.0 / 59.0) * (f64::from(j.rem_euclid(59)) + lat1 / CPR_MAX);
    if rlat0 >= 270.0 {
        rlat0 -= 360.0;
    }
    if rlat1 >= 270.0 {
        rlat1 -= 360.0;
    }
    if !(-90.0..=90.0).contains(&rlat0) || !(-90.0..=90.0).contains(&rlat1) {
        return Err(GlobalFail::Bad);
    }
    if cpr_nl(rlat0) != cpr_nl(rlat1) {
        return Err(GlobalFail::Zone);
    }
    let (rlat, lon_cpr, odd_i) = if latest_odd {
        (rlat1, lon1, 1u32)
    } else {
        (rlat0, lon0, 0u32)
    };
    let nl = cpr_nl(rlat);
    let ni = nl.saturating_sub(odd_i).max(1);
    let m = ((lon0 * f64::from(nl.saturating_sub(1)) - lon1 * f64::from(nl)) / CPR_MAX + 0.5)
        .floor() as i32;
    let mut rlon =
        (360.0 / f64::from(ni)) * (f64::from(m.rem_euclid(ni as i32)) + lon_cpr / CPR_MAX);
    if rlon > 180.0 {
        rlon -= 360.0;
    }
    Ok(LatLon {
        lat: rlat,
        lon: rlon,
    })
}

/// Local unambiguous decode. readsb `decodeCPRrelative` in `cpr.c`.
pub fn relative(reference: LatLon, bits: CprBits) -> Option<LatLon> {
    let span = if bits.surface { 90.0 } else { 360.0 };
    let frac_lat = f64::from(bits.lat) / CPR_MAX;
    let frac_lon = f64::from(bits.lon) / CPR_MAX;
    let dlat = span / if bits.odd { 59.0 } else { 60.0 };
    let j = (reference.lat / dlat).floor()
        + (0.5 + rem_pos(reference.lat, dlat) / dlat - frac_lat).floor();
    let mut rlat = dlat * (j + frac_lat);
    if rlat >= 270.0 {
        rlat -= 360.0;
    }
    if !(-90.0..=90.0).contains(&rlat) || (rlat - reference.lat).abs() > dlat / 2.0 {
        return None;
    }
    let ni = cpr_nl(rlat).saturating_sub(u32::from(bits.odd)).max(1);
    let dlon = span / f64::from(ni);
    let m = (reference.lon / dlon).floor()
        + (0.5 + rem_pos(reference.lon, dlon) / dlon - frac_lon).floor();
    let mut rlon = dlon * (m + frac_lon);
    if rlon > 180.0 {
        rlon -= 360.0;
    }
    if (rlon - reference.lon).abs() > dlon / 2.0 {
        return None;
    }
    Some(LatLon {
        lat: rlat,
        lon: rlon,
    })
}

fn rem_pos(a: f64, b: f64) -> f64 {
    let r = a % b;
    if r < 0.0 { r + b } else { r }
}

pub fn greatcircle_m(a: LatLon, b: LatLon) -> f64 {
    let lat0 = a.lat.to_radians();
    let lon0 = a.lon.to_radians();
    let lat1 = b.lat.to_radians();
    let lon1 = b.lon.to_radians();
    let dlat = lat1 - lat0;
    let dlon = lon1 - lon0;
    let s = (dlat / 2.0).sin().powi(2) + lat0.cos() * lat1.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * EARTH_M * s.sqrt().asin()
}

/// dump1090-fa `track.c` `speed_check`. 500 m airborne / 100 m surface
/// plus distance at (speed × 4/3), min 200 kt airborne.
pub fn speed_ok(
    prev: LatLon,
    next: LatLon,
    elapsed_s: f64,
    surface: bool,
    gs_kt: Option<u16>,
) -> bool {
    let metres = greatcircle_m(prev, next);
    let mut speed = f64::from(gs_kt.unwrap_or(if surface { 100 } else { 600 }));
    speed *= 4.0 / 3.0;
    if surface {
        speed = speed.clamp(20.0, 150.0);
    } else if speed < 200.0 {
        speed = 200.0;
    }
    let base = if surface { 100.0 } else { 500.0 };
    let range = base + (elapsed_s + 1.0) * speed * KT_MS;
    metres <= range
}

pub fn local_in_range(reference: LatLon, next: LatLon) -> bool {
    greatcircle_m(reference, next) <= LOCAL_RANGE_M
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modes::{EVEN, ODD};
    use rs1090::decode::adsb::ME;
    use rs1090::decode::cpr::CPRFormat;
    use rs1090::decode::{DF, Message};

    fn bits(frame: &[u8; 14]) -> CprBits {
        let message = Message::try_from(frame.as_slice()).expect("df17");
        let DF::ExtendedSquitterADSB(adsb) = message.df else {
            panic!("adsb");
        };
        let ME::BDS05 { inner, .. } = adsb.message else {
            panic!("bds05");
        };
        CprBits {
            odd: inner.parity == CPRFormat::Odd,
            surface: false,
            lat: inner.lat_cpr,
            lon: inner.lon_cpr,
        }
    }

    #[test]
    fn even_odd_global_matches_known_position() {
        let even = bits(&EVEN);
        let odd = bits(&ODD);
        assert!(!even.odd && odd.odd);
        let pos = airborne_global(even, odd, true).expect("global");
        assert!((pos.lat - 49.81755).abs() < 0.001);
        assert!((pos.lon - 6.08442).abs() < 0.001);
        let even_latest = airborne_global(even, odd, false).expect("even latest");
        assert!((even_latest.lat - 49.82).abs() < 0.02);
        assert!(relative(pos, even).is_some(), "local even");
        assert!(relative(pos, odd).is_some(), "local odd");
    }
}

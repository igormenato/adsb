//! Turn Mode S frames into a [`Fix`] when a CPR position solves.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;

use rs1090::decode::adsb::ME;
use rs1090::decode::bds::bds09::AirborneVelocitySubType;
use rs1090::decode::bds::bds61::{AircraftStatusType, EmergencyState};
use rs1090::decode::cpr::CPRFormat;
use rs1090::decode::{DF, ICAO, Message};

use crate::cpr::{
    CprBits, GlobalFail, LatLon, airborne_global, local_in_range, pair_window_s, relative, speed_ok,
};
use crate::frame::{Aircraft, Callsign, EmergencyKind, Fix, Icao, deg_e7};

/// ICAOs a contest receiver keeps for CPR and ident.
/// A Pi contest station sees far fewer. Too small a value drops live pairs.
const TRACKER_CAP: usize = 256;

/// Drop ICAOs idle this long when the map is over capacity.
const CPR_IDLE_S: f64 = 180.0;

/// One ICAO's identification, velocity, and emergency between position frames.
#[derive(Clone, Debug, Default)]
struct Cached {
    callsign: Callsign,
    ground_speed_kt: Option<u16>,
    track_deg: Option<u16>,
    emergency: Option<EmergencyKind>,
}

#[derive(Clone, Copy)]
struct CprSample {
    bits: CprBits,
    t: f64,
}

#[derive(Clone, Copy)]
struct Solved {
    pos: LatLon,
    t: f64,
    valid: bool,
}

#[derive(Default)]
struct Track {
    ident: Cached,
    last_seen: f64,
    even: Option<CprSample>,
    odd: Option<CprSample>,
    pos: Option<Solved>,
}

/// Per-aircraft CPR history plus the fields a position frame does not carry.
pub struct Tracker {
    aircraft: BTreeMap<ICAO, Track>,
}

impl fmt::Debug for Tracker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tracker")
            .field("aircraft", &self.aircraft.len())
            .finish_non_exhaustive()
    }
}

impl Default for Tracker {
    fn default() -> Self {
        Self::new()
    }
}

impl Tracker {
    pub fn new() -> Self {
        Self {
            aircraft: BTreeMap::new(),
        }
    }

    /// Decode one Mode S frame. A [`Fix`] is the position that just solved.
    pub fn ingest(&mut self, timestamp: f64, frame: &[u8]) -> Option<Fix> {
        let message = Message::try_from(frame).ok()?;
        let (icao, fix, known) = match &message.df {
            DF::ExtendedSquitterADSB(adsb) => {
                let icao24 = adsb.icao24;
                let known = self.known(icao24);
                self.cache_adsb(icao24, &adsb.message);
                let fix = self.try_position(icao24, timestamp, &adsb.message);
                (icao24, fix, known)
            }
            DF::SurveillanceIdentityReply { id, ap, .. } => {
                let icao = ICAO::from(*ap);
                let known = self.known(icao);
                self.apply_squawk(icao, id.0);
                (icao, None, known)
            }
            DF::ShortAirAirSurveillance { .. }
            | DF::SurveillanceAltitudeReply { .. }
            | DF::AllCallReply { .. }
            | DF::LongAirAirSurveillance { .. }
            | DF::ExtendedSquitterTisB { .. }
            | DF::ExtendedSquitterMilitary { .. }
            | DF::CommBAltitudeReply { .. }
            | DF::CommBIdentityReply { .. }
            | DF::CommDExtended { .. } => return None,
        };
        self.aircraft.entry(icao).or_default().last_seen = timestamp;
        self.trim(timestamp, icao, known);
        fix
    }

    fn known(&self, icao: ICAO) -> bool {
        self.aircraft.contains_key(&icao)
    }

    fn ident_mut(&mut self, icao: ICAO) -> &mut Cached {
        &mut self.aircraft.entry(icao).or_default().ident
    }

    fn trim(&mut self, now: f64, keep: ICAO, known: bool) {
        if self.aircraft.len() <= TRACKER_CAP {
            return;
        }
        self.aircraft
            .retain(|icao, track| *icao == keep || now - track.last_seen <= CPR_IDLE_S);
        while self.aircraft.len() > TRACKER_CAP {
            if let Some(icao) = self.oldest_solved(keep) {
                self.aircraft.remove(&icao);
                continue;
            }
            if !known && self.known(keep) {
                self.aircraft.remove(&keep);
                continue;
            }
            let Some(icao) = self.oldest(keep) else {
                break;
            };
            self.aircraft.remove(&icao);
        }
    }

    fn recency(left_icao: &ICAO, left: &Track, right_icao: &ICAO, right: &Track) -> Ordering {
        left.last_seen
            .total_cmp(&right.last_seen)
            .then(left_icao.cmp(right_icao))
    }

    fn oldest_solved(&self, keep: ICAO) -> Option<ICAO> {
        self.aircraft
            .iter()
            .filter(|(icao, _)| **icao != keep)
            .filter(|(_, track)| track.pos.is_some_and(|p| p.valid))
            .min_by(|(left_icao, left), (right_icao, right)| {
                Self::recency(left_icao, left, right_icao, right)
            })
            .map(|(icao, _)| *icao)
    }

    fn oldest(&self, keep: ICAO) -> Option<ICAO> {
        self.aircraft
            .iter()
            .filter(|(icao, _)| **icao != keep)
            .min_by(|(left_icao, left), (right_icao, right)| {
                Self::recency(left_icao, left, right_icao, right)
            })
            .map(|(icao, _)| *icao)
    }

    fn cache_adsb(&mut self, icao24: ICAO, me: &ME) {
        match me {
            ME::BDS08 { inner, .. } => {
                if !inner.callsign.contains('#') {
                    self.ident_mut(icao24).callsign = Callsign::pad(&inner.callsign);
                }
            }
            ME::BDS09(velocity) => {
                if let AirborneVelocitySubType::GroundSpeedDecoding(speed) = &velocity.velocity {
                    let ident = self.ident_mut(icao24);
                    ident.ground_speed_kt = knots(speed.groundspeed);
                    ident.track_deg = track(speed.track);
                }
            }
            ME::BDS61(status) => {
                let ident = self.ident_mut(icao24);
                match status.subtype {
                    AircraftStatusType::EmergencyPriorityStatus => {
                        ident.emergency = emergency_kind(status.emergency_state, status.squawk.0);
                    }
                    AircraftStatusType::NoInformation
                    | AircraftStatusType::ACASRaBroadcast
                    | AircraftStatusType::Reserved => {
                        if ident.emergency.is_none() {
                            ident.emergency = kind_from_squawk(status.squawk.0);
                        }
                    }
                }
            }
            ME::BDS05 { .. }
            | ME::BDS06 { .. }
            | ME::NoPosition(_)
            | ME::Reserved0(_)
            | ME::SurfaceSystemStatus(_)
            | ME::Reserved1 { .. }
            | ME::BDS62(_)
            | ME::AircraftOperationalCoordination(_)
            | ME::BDS65(_) => {}
        }
    }

    fn try_position(&mut self, icao24: ICAO, timestamp: f64, me: &ME) -> Option<Fix> {
        let (bits, alt) = cpr_from_me(me)?;
        let pos = self.solve_cpr(icao24, timestamp, bits)?;
        if !(-90.0..=90.0).contains(&pos.lat) || !(-180.0..=180.0).contains(&pos.lon) {
            return None;
        }
        let icao = Icao::from_u24(icao24.0)?;
        let ident = self.ident_mut(icao24).clone();
        Some(Fix {
            aircraft: Aircraft {
                icao,
                callsign: ident.callsign,
                latitude_e7: deg_e7(pos.lat),
                longitude_e7: deg_e7(pos.lon),
                altitude_ft: alt,
                ground_speed_kt: ident.ground_speed_kt,
                track_deg: ident.track_deg,
                emergency: ident.emergency,
            },
        })
    }

    fn solve_cpr(&mut self, icao: ICAO, t: f64, bits: CprBits) -> Option<LatLon> {
        let sample = CprSample { bits, t };
        let track = self.aircraft.entry(icao).or_default();
        if bits.odd {
            track.odd = Some(sample);
        } else {
            track.even = Some(sample);
        }
        let gs = track.ident.ground_speed_kt;
        let window = pair_window_s(bits.surface, gs);
        let mut decoded = None;
        let mut drop_pair = false;
        if let (Some(even), Some(odd)) = (track.even, track.odd)
            && even.bits.surface == odd.bits.surface
            && even.bits.surface == bits.surface
            && (even.t - odd.t).abs() <= window
        {
            if bits.surface {
                if let Some(prev) = track.pos.filter(|p| p.valid) {
                    decoded = relative(prev.pos, bits).filter(|&pos| local_in_range(prev.pos, pos));
                }
            } else {
                match airborne_global(even.bits, odd.bits, bits.odd) {
                    Ok(pos) => decoded = Some(pos),
                    Err(GlobalFail::Zone) => decoded = None,
                    Err(GlobalFail::Bad) => drop_pair = true,
                }
            }
        }
        if !drop_pair
            && decoded.is_none()
            && let Some(prev) = track.pos.filter(|p| p.valid)
        {
            decoded = relative(prev.pos, bits).filter(|&pos| local_in_range(prev.pos, pos));
        }
        if drop_pair {
            Self::reject_pair(track);
            return None;
        }
        let mut pos = decoded?;
        if let Some(prev) = track.pos.filter(|p| p.valid)
            && !speed_ok(prev.pos, pos, (t - prev.t).abs(), bits.surface, gs)
        {
            // Global even-latest vs odd-latest of one pair can sit ~1 km
            // apart. Fall back to a local decode against the last fix.
            match relative(prev.pos, bits).filter(|&p| local_in_range(prev.pos, p)) {
                Some(local) if speed_ok(prev.pos, local, (t - prev.t).abs(), bits.surface, gs) => {
                    pos = local;
                }
                _ => return None,
            }
        }
        track.pos = Some(Solved {
            pos,
            t,
            valid: true,
        });
        Some(pos)
    }

    fn reject_pair(track: &mut Track) {
        track.even = None;
        track.odd = None;
        if let Some(pos) = &mut track.pos {
            pos.valid = false;
        }
    }

    fn apply_squawk(&mut self, icao: ICAO, squawk: u16) {
        let ident = self.ident_mut(icao);
        if ident.emergency.is_none() {
            ident.emergency = kind_from_squawk(squawk);
        } else if matches!(ident.emergency, Some(EmergencyKind::Reserved))
            && let Some(kind) = kind_from_squawk(squawk)
        {
            ident.emergency = Some(kind);
        }
    }
}

fn cpr_from_me(me: &ME) -> Option<(CprBits, Option<i32>)> {
    match me {
        ME::BDS05 { inner, .. } => Some((
            CprBits {
                odd: inner.parity == CPRFormat::Odd,
                surface: false,
                lat: inner.lat_cpr,
                lon: inner.lon_cpr,
            },
            inner.alt,
        )),
        ME::BDS06 { inner, .. } => Some((
            CprBits {
                odd: inner.parity == CPRFormat::Odd,
                surface: true,
                lat: inner.lat_cpr,
                lon: inner.lon_cpr,
            },
            None,
        )),
        _ => None,
    }
}

fn knots(value: f64) -> Option<u16> {
    if !value.is_finite() || value < 0.0 {
        return None;
    }
    let rounded = value.round();
    if rounded >= 65535.0 {
        Some(65534)
    } else {
        Some(rounded as u16)
    }
}

fn track(degrees: f64) -> Option<u16> {
    if !degrees.is_finite() {
        return None;
    }
    let rounded = degrees.round();
    if !(0.0..=360.0).contains(&rounded) {
        return None;
    }
    let degrees = rounded as u16;
    if degrees == 360 {
        Some(0)
    } else {
        Some(degrees)
    }
}

fn emergency_kind(state: EmergencyState, squawk: u16) -> Option<EmergencyKind> {
    let named = match state {
        EmergencyState::None => None,
        EmergencyState::General => Some(EmergencyKind::General),
        EmergencyState::Medical => Some(EmergencyKind::Lifeguard),
        EmergencyState::MinimumFuel => Some(EmergencyKind::MinFuel),
        EmergencyState::NoCommunication => Some(EmergencyKind::Nordo),
        EmergencyState::UnlawfulInterference => Some(EmergencyKind::Unlawful),
        EmergencyState::DownedAircraft => Some(EmergencyKind::Downed),
        EmergencyState::Reserved => Some(EmergencyKind::Reserved),
    };
    match named {
        Some(kind) if kind != EmergencyKind::Reserved => Some(kind),
        named => kind_from_squawk(squawk).or(named),
    }
}

fn kind_from_squawk(code: u16) -> Option<EmergencyKind> {
    match code {
        0x7700 => Some(EmergencyKind::General),
        0x7600 => Some(EmergencyKind::Nordo),
        0x7500 => Some(EmergencyKind::Unlawful),
        _ => None,
    }
}

/// True when rs1090 parses `frame` as a Mode S message.
pub fn is_mode_s(frame: &[u8]) -> bool {
    Message::try_from(frame).is_ok()
}

/// One Mode S frame as hex. 7-byte and 14-byte frames are accepted.
/// AVR `*hex;` and `@` plus a 12-digit hex timestamp are stripped first.
pub fn parse_hex_frame(bytes: &[u8]) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(bytes).ok()?.trim();
    if text.is_empty() || text.starts_with('#') {
        return None;
    }
    let text = text.strip_suffix(';').unwrap_or(text);
    let text = match text.as_bytes().first().copied() {
        Some(b'*') => text.get(1..)?,
        Some(b'@') => {
            let rest = text.get(1..)?;
            const STAMP: usize = 12;
            let stamp = rest.get(..STAMP)?;
            if !stamp.bytes().all(|b| b.is_ascii_hexdigit()) {
                return None;
            }
            rest.get(STAMP..)?
        }
        _ => text,
    };
    let text = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .unwrap_or(text);
    if text.len() < 14 || text.len() % 2 != 0 {
        return None;
    }
    hex::decode(text).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::modes::{EVEN, ODD, STATUS, airborne_position, hex14, with_icao};
    use crate::pack_frame;

    const IDENT: &str = "8d406b902015a678d4d220aa4bda";
    const IDENT_40058B: &str = "8d40058b205054d4c72ce03f949e";
    const VELOCITY: &str = "8D485020994409940838175B284F";
    const VELOCITY_40058B: &str = "8d40058b9904027fc00000b14147";

    fn frame(hex: &str) -> Vec<u8> {
        parse_hex_frame(hex.as_bytes()).expect("hex")
    }

    #[test]
    fn even_then_odd_emits_a_position() {
        let mut tracker = Tracker::new();
        assert!(tracker.ingest(1.0, &EVEN).is_none());
        let fix = tracker.ingest(2.0, &ODD).expect("position");
        assert_eq!(fix.aircraft.icao.to_u24(), 0x0040_058B);
        assert!((fix.aircraft.latitude_e7 as f64 / 1e7 - 49.81755).abs() < 0.001);
        assert!((fix.aircraft.longitude_e7 as f64 / 1e7 - 6.08442).abs() < 0.001);
        assert_eq!(fix.aircraft.callsign.as_bytes(), *b"        ");
        assert_eq!(fix.aircraft.ground_speed_kt, None);
        assert_eq!(fix.aircraft.emergency, None);
    }

    #[test]
    fn local_decode_follows_a_solved_pair() {
        let icao = 0x40058B;
        let lat = 49.81755;
        let lon = 6.08442;
        let mut tracker = Tracker::new();
        assert!(
            tracker
                .ingest(1.0, &airborne_position(icao, lat, lon, 39_000, false))
                .is_none()
        );
        let first = tracker
            .ingest(2.0, &airborne_position(icao, lat, lon, 39_000, true))
            .expect("global");
        let later = airborne_position(icao, lat + 0.002, lon + 0.002, 39_000, false);
        let again = tracker.ingest(13.0, &later).expect("local after 11 s");
        let dlat = (first.aircraft.latitude_e7 - again.aircraft.latitude_e7).abs();
        let dlon = (first.aircraft.longitude_e7 - again.aircraft.longitude_e7).abs();
        assert!(dlat < 50_000, "dlat={dlat}");
        assert!(dlon < 50_000, "dlon={dlon}");
    }

    #[test]
    fn bogus_jump_is_rejected() {
        let mut tracker = Tracker::new();
        assert!(tracker.ingest(1.0, &EVEN).is_none());
        assert!(tracker.ingest(2.0, &ODD).is_some());
        let even = airborne_position(0x40058B, 0.0, 0.0, 30_000, false);
        let odd = airborne_position(0x40058B, 0.0, 0.0, 30_000, true);
        assert!(tracker.ingest(3.0, &even).is_none());
        assert!(tracker.ingest(3.5, &odd).is_none());
    }

    #[test]
    fn identification_and_velocity_do_not_emit() {
        let mut tracker = Tracker::new();
        assert!(tracker.ingest(1.0, &frame(IDENT)).is_none());
        assert!(tracker.ingest(1.0, &frame(VELOCITY)).is_none());
    }

    #[test]
    fn callsign_attaches_to_the_same_icao() {
        let mut tracker = Tracker::new();
        assert!(tracker.ingest(1.0, &frame(IDENT_40058B)).is_none());
        assert!(tracker.ingest(2.0, &EVEN).is_none());
        let fix = tracker.ingest(3.0, &ODD).expect("position");
        assert_eq!(fix.aircraft.callsign.as_bytes(), *b"TEST123 ");
    }

    #[test]
    fn ident_does_not_attach_to_another_icao() {
        let mut tracker = Tracker::new();
        assert!(tracker.ingest(1.0, &frame(IDENT)).is_none());
        assert!(tracker.ingest(2.0, &EVEN).is_none());
        let fix = tracker.ingest(3.0, &ODD).expect("position");
        assert_eq!(fix.aircraft.callsign.as_bytes(), *b"        ");
    }

    #[test]
    fn emergency_is_on_the_wire() {
        let mut tracker = Tracker::new();
        assert!(tracker.ingest(1.0, &STATUS).is_none());
        assert!(tracker.ingest(2.0, &EVEN).is_none());
        let fix = tracker.ingest(3.0, &ODD).expect("position");
        assert_eq!(fix.aircraft.emergency, Some(EmergencyKind::General));
        assert_eq!(pack_frame(&fix)[31], 1);
    }

    #[test]
    fn track_360_wraps_to_0() {
        let mut tracker = Tracker::new();
        assert!(tracker.ingest(1.0, &frame(VELOCITY_40058B)).is_none());
        assert!(tracker.ingest(2.0, &EVEN).is_none());
        let fix = tracker.ingest(3.0, &ODD).expect("position");
        assert_eq!(fix.aircraft.track_deg, Some(0));
        assert_eq!(&pack_frame(&fix)[29..31], &[0, 0]);
    }

    #[test]
    fn emergency_names_and_squawks() {
        assert_eq!(
            emergency_kind(EmergencyState::General, 0x1000),
            Some(EmergencyKind::General)
        );
        assert_eq!(
            emergency_kind(EmergencyState::Medical, 0x7700),
            Some(EmergencyKind::Lifeguard)
        );
        assert_eq!(
            emergency_kind(EmergencyState::None, 0x7700),
            Some(EmergencyKind::General)
        );
        assert_eq!(
            emergency_kind(EmergencyState::Reserved, 0x7600),
            Some(EmergencyKind::Nordo)
        );
        assert_eq!(
            emergency_kind(EmergencyState::UnlawfulInterference, 0x7700),
            Some(EmergencyKind::Unlawful)
        );
        assert_eq!(emergency_kind(EmergencyState::None, 0x1000), None);
        assert_eq!(kind_from_squawk(0x7500), Some(EmergencyKind::Unlawful));
    }

    #[test]
    fn hex_lines() {
        assert_eq!(parse_hex_frame(b"8D40058B"), None);
        let even = hex14(&EVEN);
        assert_eq!(parse_hex_frame(even.as_bytes()).unwrap().len(), 14);
        assert_eq!(
            parse_hex_frame(format!("  {even}  ").as_bytes()),
            parse_hex_frame(even.as_bytes())
        );
        assert_eq!(
            parse_hex_frame(format!("*{even};").as_bytes()),
            parse_hex_frame(even.as_bytes())
        );
        assert_eq!(
            parse_hex_frame(format!("@000000000000{even};").as_bytes()),
            parse_hex_frame(even.as_bytes())
        );
        assert!(parse_hex_frame(b"*zzzz;").is_none());
        assert!(parse_hex_frame(b"@short;").is_none());
        assert!(parse_hex_frame(b"# comment").is_none());
        assert!(parse_hex_frame(b"").is_none());
    }

    #[test]
    fn one_over_cap_does_not_wipe_pending_cpr() {
        let mut tracker = Tracker::new();
        let n = TRACKER_CAP + 1;
        for i in 0..n {
            let icao = 0x10_0000 + i as u32;
            assert!(
                tracker.ingest(1.0, &with_icao(&EVEN, icao)).is_none(),
                "even {icao:06x}"
            );
        }
        let mut fixes = 0usize;
        for i in 0..n {
            let icao = 0x10_0000 + i as u32;
            if tracker.ingest(2.0, &with_icao(&ODD, icao)).is_some() {
                fixes += 1;
            }
        }
        assert_eq!(fixes, TRACKER_CAP, "fixes={fixes}");
    }

    #[test]
    fn sequential_pairs_past_cap_keep_solving() {
        let mut tracker = Tracker::new();
        let n = TRACKER_CAP + 16;
        for i in 0..n {
            let icao = 0x10_0000 + i as u32;
            let t = f64::from(i as u32);
            assert!(tracker.ingest(t, &with_icao(&EVEN, icao)).is_none());
            assert!(
                tracker.ingest(t + 0.5, &with_icao(&ODD, icao)).is_some(),
                "missing pair {icao:06x}"
            );
        }
    }
}

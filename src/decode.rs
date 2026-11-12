//! Turn Mode S frames into a [`Fix`] when an airborne position solves.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;

use rs1090::decode::adsb::ME;
use rs1090::decode::bds::bds09::AirborneVelocitySubType;
use rs1090::decode::bds::bds61::{AircraftStatusType, EmergencyState};
use rs1090::decode::cpr::{AircraftState, decode_position};
use rs1090::decode::{DF, ICAO, Message};

use crate::frame::{Aircraft, Callsign, EmergencyKind, Fix, Icao, deg_e7};

/// ICAOs a contest receiver keeps for CPR and ident.
/// A Pi contest station sees far fewer. Too small a value drops live pairs.
const TRACKER_CAP: usize = 256;

/// Local CPR in rs1090 is useful for this many seconds.
const CPR_LOCAL_MAX_AGE_S: f64 = 180.0;

/// One ICAO's identification, velocity, and emergency between position frames.
#[derive(Clone, Debug, Default)]
struct Cached {
    callsign: Callsign,
    ground_speed_kt: Option<u16>,
    track_deg: Option<u16>,
    emergency: Option<EmergencyKind>,
}

/// Recency for trim. Not ident: `decode_position` owns CPR in its own map.
#[derive(Default)]
struct Seen {
    ident: Cached,
    last_seen: f64,
}

/// Per-aircraft CPR history plus the fields a position frame does not carry.
pub struct Tracker {
    cpr: BTreeMap<ICAO, AircraftState>,
    fields: BTreeMap<ICAO, Seen>,
    reference: Option<rs1090::decode::cpr::Position>,
}

impl fmt::Debug for Tracker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Tracker")
            .field("aircraft", &self.fields.len())
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
            cpr: BTreeMap::new(),
            fields: BTreeMap::new(),
            reference: None,
        }
    }

    /// Decode one Mode S frame. A [`Fix`] is the airborne position that just solved.
    pub fn ingest(&mut self, timestamp: f64, frame: &[u8]) -> Option<Fix> {
        let mut message = Message::try_from(frame).ok()?;
        let (icao, fix, known) = match &mut message.df {
            DF::ExtendedSquitterADSB(adsb) => {
                let icao24 = adsb.icao24;
                let known = self.known(icao24);
                let no_update = None;
                decode_position(
                    &mut adsb.message,
                    timestamp,
                    &icao24,
                    &mut self.cpr,
                    &mut self.reference,
                    &no_update,
                );
                (icao24, self.apply_adsb(icao24, &adsb.message), known)
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
        self.fields.entry(icao).or_default().last_seen = timestamp;
        self.trim(timestamp, icao, known);
        fix
    }

    fn known(&self, icao: ICAO) -> bool {
        self.fields.contains_key(&icao) || self.cpr.contains_key(&icao)
    }

    fn ident_mut(&mut self, icao: ICAO) -> &mut Cached {
        &mut self.fields.entry(icao).or_default().ident
    }

    fn trim(&mut self, now: f64, keep: ICAO, known: bool) {
        if self.cpr.len() <= TRACKER_CAP && self.fields.len() <= TRACKER_CAP {
            return;
        }
        self.fields
            .retain(|icao, seen| *icao == keep || now - seen.last_seen <= CPR_LOCAL_MAX_AGE_S);
        self.cpr.retain(|icao, _| self.fields.contains_key(icao));
        while self.cpr.len() > TRACKER_CAP || self.fields.len() > TRACKER_CAP {
            if let Some(icao) = self.oldest_solved(keep) {
                self.evict(icao);
                continue;
            }
            if !known && self.known(keep) {
                self.evict(keep);
                continue;
            }
            let Some(icao) = self.oldest(keep) else {
                break;
            };
            self.evict(icao);
        }
    }

    fn evict(&mut self, icao: ICAO) {
        self.cpr.remove(&icao);
        self.fields.remove(&icao);
    }

    fn recency(left_icao: &ICAO, left: &Seen, right_icao: &ICAO, right: &Seen) -> Ordering {
        left.last_seen
            .total_cmp(&right.last_seen)
            .then(left_icao.cmp(right_icao))
    }

    fn oldest_solved(&self, keep: ICAO) -> Option<ICAO> {
        self.fields
            .iter()
            .filter(|(icao, _)| **icao != keep)
            .filter(|(icao, _)| {
                self.cpr
                    .get(*icao)
                    .and_then(AircraftState::last_position)
                    .is_some()
            })
            .min_by(|(left_icao, left), (right_icao, right)| {
                Self::recency(left_icao, left, right_icao, right)
            })
            .map(|(icao, _)| *icao)
    }

    fn oldest(&self, keep: ICAO) -> Option<ICAO> {
        self.fields
            .iter()
            .filter(|(icao, _)| **icao != keep)
            .min_by(|(left_icao, left), (right_icao, right)| {
                Self::recency(left_icao, left, right_icao, right)
            })
            .map(|(icao, _)| *icao)
    }

    fn apply_adsb(&mut self, icao24: ICAO, me: &ME) -> Option<Fix> {
        match me {
            ME::BDS08 { inner, .. } => {
                if !inner.callsign.contains('#') {
                    self.ident_mut(icao24).callsign = Callsign::pad(&inner.callsign);
                }
                None
            }
            ME::BDS09(velocity) => {
                if let AirborneVelocitySubType::GroundSpeedDecoding(speed) = &velocity.velocity {
                    let ident = self.ident_mut(icao24);
                    ident.ground_speed_kt = knots(speed.groundspeed);
                    ident.track_deg = track(speed.track);
                }
                None
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
                None
            }
            ME::BDS05 { inner, .. } => {
                let latitude = inner.latitude?;
                let longitude = inner.longitude?;
                if !(-90.0..=90.0).contains(&latitude) || !(-180.0..=180.0).contains(&longitude) {
                    return None;
                }
                let icao = Icao::from_u24(icao24.0)?;
                let ident = self.ident_mut(icao24).clone();
                Some(Fix {
                    aircraft: Aircraft {
                        icao,
                        callsign: ident.callsign,
                        latitude_e7: deg_e7(latitude),
                        longitude_e7: deg_e7(longitude),
                        altitude_ft: inner.alt,
                        ground_speed_kt: ident.ground_speed_kt,
                        track_deg: ident.track_deg,
                        emergency: ident.emergency,
                    },
                })
            }
            ME::NoPosition(_)
            | ME::BDS06 { .. }
            | ME::Reserved0(_)
            | ME::SurfaceSystemStatus(_)
            | ME::Reserved1 { .. }
            | ME::BDS62(_)
            | ME::AircraftOperationalCoordination(_)
            | ME::BDS65(_) => None,
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

    use crate::modes::{EVEN, ODD, STATUS, hex14, with_icao};
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

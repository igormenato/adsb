//! Turn Mode S frames into a [`Fix`] when an airborne position solves.

use std::collections::BTreeMap;
use std::fmt;

use rs1090::decode::adsb::ME;
use rs1090::decode::bds::bds09::AirborneVelocitySubType;
use rs1090::decode::bds::bds61::{AircraftStatusType, EmergencyState};
use rs1090::decode::cpr::{AircraftState, decode_position};
use rs1090::decode::{DF, ICAO, Message};

use crate::frame::{Aircraft, Callsign, EmergencyKind, Fix, Icao, deg_e7};

/// One ICAO's identification, velocity, and emergency between position frames.
#[derive(Clone, Debug, Default)]
struct Cached {
    callsign: Callsign,
    ground_speed_kt: Option<u16>,
    track_deg: Option<u16>,
    emergency: Option<EmergencyKind>,
}

/// Per-aircraft CPR history plus the fields a position frame does not carry.
pub struct Tracker {
    cpr: BTreeMap<ICAO, AircraftState>,
    fields: BTreeMap<u32, Cached>,
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
        match &mut message.df {
            DF::ExtendedSquitterADSB(adsb) => {
                let icao24 = adsb.icao24;
                let no_update = None;
                decode_position(
                    &mut adsb.message,
                    timestamp,
                    &icao24,
                    &mut self.cpr,
                    &mut self.reference,
                    &no_update,
                );
                self.apply_adsb(timestamp, icao24, &adsb.message)
            }
            DF::SurveillanceIdentityReply { id, ap, .. } => {
                self.apply_squawk(ap.0, id.0);
                None
            }
            DF::ShortAirAirSurveillance { .. }
            | DF::SurveillanceAltitudeReply { .. }
            | DF::AllCallReply { .. }
            | DF::LongAirAirSurveillance { .. }
            | DF::ExtendedSquitterTisB { .. }
            | DF::ExtendedSquitterMilitary { .. }
            | DF::CommBAltitudeReply { .. }
            | DF::CommBIdentityReply { .. }
            | DF::CommDExtended { .. } => None,
        }
    }

    fn apply_adsb(&mut self, timestamp: f64, icao24: ICAO, me: &ME) -> Option<Fix> {
        let addr = icao24.0;
        match me {
            ME::BDS08 { inner, .. } => {
                if !inner.callsign.contains('#') {
                    self.fields.entry(addr).or_default().callsign = Callsign::pad(&inner.callsign);
                }
                None
            }
            ME::BDS09(velocity) => {
                if let AirborneVelocitySubType::GroundSpeedDecoding(speed) = &velocity.velocity {
                    let cached = self.fields.entry(addr).or_default();
                    cached.ground_speed_kt = knots(speed.groundspeed);
                    cached.track_deg = track(speed.track);
                }
                None
            }
            ME::BDS61(status) => {
                let cached = self.fields.entry(addr).or_default();
                match status.subtype {
                    AircraftStatusType::EmergencyPriorityStatus => {
                        cached.emergency = emergency_kind(status.emergency_state, status.squawk.0);
                    }
                    AircraftStatusType::NoInformation
                    | AircraftStatusType::ACASRaBroadcast
                    | AircraftStatusType::Reserved => {
                        if cached.emergency.is_none() {
                            cached.emergency = kind_from_squawk(status.squawk.0);
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
                let unix_s = unix_seconds(timestamp)?;
                let icao = Icao::from_u24(addr)?;
                let cached = self.fields.entry(addr).or_default().clone();
                Some(Fix {
                    unix_s,
                    aircraft: Aircraft {
                        icao,
                        callsign: cached.callsign,
                        latitude_e7: deg_e7(latitude),
                        longitude_e7: deg_e7(longitude),
                        altitude_ft: inner.alt,
                        ground_speed_kt: cached.ground_speed_kt,
                        track_deg: cached.track_deg,
                        emergency: cached.emergency,
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

    fn apply_squawk(&mut self, icao: u32, squawk: u16) {
        let cached = self.fields.entry(icao).or_default();
        if cached.emergency.is_none() {
            cached.emergency = kind_from_squawk(squawk);
        } else if matches!(cached.emergency, Some(EmergencyKind::Reserved))
            && let Some(kind) = kind_from_squawk(squawk)
        {
            cached.emergency = Some(kind);
        }
    }
}

fn unix_seconds(timestamp: f64) -> Option<u32> {
    if !timestamp.is_finite() || timestamp < 0.0 || timestamp > f64::from(u32::MAX) {
        return None;
    }
    Some(timestamp.trunc() as u32)
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

/// One Mode S frame as hex. 7-byte and 14-byte frames are accepted.
pub fn parse_hex_frame(bytes: &[u8]) -> Option<Vec<u8>> {
    let text = std::str::from_utf8(bytes).ok()?.trim();
    if text.is_empty() || text.starts_with('#') {
        return None;
    }
    let text = text
        .strip_prefix("0x")
        .or_else(|| text.strip_prefix("0X"))
        .unwrap_or(text);
    if text.len() < 14 || text.len() % 2 != 0 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::pack_frame;

    const EVEN: &str = "8D40058B58C901375147EFD09357";
    const ODD: &str = "8D40058B58C904A87F402D3B8C59";
    const IDENT: &str = "8d406b902015a678d4d220aa4bda";
    const IDENT_40058B: &str = "8d40058b205054d4c72ce03f949e";
    const VELOCITY: &str = "8D485020994409940838175B284F";
    const STATUS: &str = "8d40058be1200000000000a05f30";

    fn frame(hex: &str) -> Vec<u8> {
        parse_hex_frame(hex.as_bytes()).expect("hex")
    }

    #[test]
    fn even_then_odd_emits_a_position() {
        let mut tracker = Tracker::new();
        assert!(tracker.ingest(1.0, &frame(EVEN)).is_none());
        let fix = tracker.ingest(2.0, &frame(ODD)).expect("position");
        assert_eq!(fix.unix_s, 2);
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
        assert!(tracker.ingest(2.0, &frame(EVEN)).is_none());
        let fix = tracker.ingest(3.0, &frame(ODD)).expect("position");
        assert_eq!(fix.aircraft.callsign.as_bytes(), *b"TEST123 ");
    }

    #[test]
    fn ident_does_not_attach_to_another_icao() {
        let mut tracker = Tracker::new();
        assert!(tracker.ingest(1.0, &frame(IDENT)).is_none());
        assert!(tracker.ingest(2.0, &frame(EVEN)).is_none());
        let fix = tracker.ingest(3.0, &frame(ODD)).expect("position");
        assert_eq!(fix.aircraft.callsign.as_bytes(), *b"        ");
    }

    #[test]
    fn emergency_is_on_the_wire() {
        let mut tracker = Tracker::new();
        assert!(tracker.ingest(1.0, &frame(STATUS)).is_none());
        assert!(tracker.ingest(2.0, &frame(EVEN)).is_none());
        let fix = tracker.ingest(3.0, &frame(ODD)).expect("position");
        assert_eq!(fix.aircraft.emergency, Some(EmergencyKind::General));
        assert_eq!(pack_frame(&fix)[35], 1);
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
        assert_eq!(parse_hex_frame(EVEN.as_bytes()).unwrap().len(), 14);
        assert_eq!(
            parse_hex_frame(format!("  {EVEN}  ").as_bytes()),
            parse_hex_frame(EVEN.as_bytes())
        );
        assert!(parse_hex_frame(b"# comment").is_none());
        assert!(parse_hex_frame(b"").is_none());
    }
}

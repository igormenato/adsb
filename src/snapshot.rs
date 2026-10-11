//! Turn one readsb `aircraft.json` document into a [`Snapshot`].

use serde_json::Value;

use crate::alert::EmergencyKind;
use crate::frame::{
    deg_e7, Aircraft, Snapshot, ALT_UNKNOWN, HEADING_UNKNOWN, SPEED_UNKNOWN, TRACK_MAX_AIRCRAFT,
};

/// A position older than this is left out. The snapshot time would otherwise label a stale fix as current.
pub const POSITION_MAX_AGE_S: f64 = 2.0;

#[derive(Debug, PartialEq, Eq)]
pub enum SnapshotError {
    /// The file is not a JSON object with an `aircraft` array.
    Json,
    /// `now` is missing.
    MissingNow,
    /// `now` is not a Unix second that fits in a u32.
    BadNow,
}

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SnapshotError::Json => "not an aircraft.json document",
            SnapshotError::MissingNow => "missing now",
            SnapshotError::BadNow => "now is out of range",
        })
    }
}

/// The snapshot plus the emergency state of each selected aircraft, in the same order.
pub(crate) struct SnapshotEmergency {
    pub snapshot: Snapshot,
    pub emergency: Vec<Option<EmergencyKind>>,
}

/// Aircraft with a latitude and longitude no older than [`POSITION_MAX_AGE_S`], freshest `seen_pos` first, at most 64.
pub fn snapshot_from_aircraft_json(bytes: &[u8]) -> Result<Snapshot, SnapshotError> {
    Ok(snapshot_with_emergency(bytes)?.snapshot)
}

pub(crate) fn snapshot_with_emergency(bytes: &[u8]) -> Result<SnapshotEmergency, SnapshotError> {
    let (unix_s, ranked) = ranked_aircraft(bytes)?;
    let mut aircraft = Vec::with_capacity(ranked.len());
    let mut emergency = Vec::with_capacity(ranked.len());
    for entry in ranked {
        aircraft.push(entry.aircraft);
        emergency.push(entry.emergency);
    }
    Ok(SnapshotEmergency {
        snapshot: Snapshot { unix_s, aircraft },
        emergency,
    })
}

struct Ranked {
    seen_pos: f64,
    index: usize,
    aircraft: Aircraft,
    emergency: Option<EmergencyKind>,
}

fn ranked_aircraft(bytes: &[u8]) -> Result<(u32, Vec<Ranked>), SnapshotError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| SnapshotError::Json)?;
    let now = value
        .get("now")
        .and_then(Value::as_f64)
        .ok_or(SnapshotError::MissingNow)?;
    if !now.is_finite() || now < 0.0 || now > f64::from(u32::MAX) {
        return Err(SnapshotError::BadNow);
    }
    let list = value
        .get("aircraft")
        .and_then(Value::as_array)
        .ok_or(SnapshotError::Json)?;

    let mut ranked = Vec::new();
    for (index, entry) in list.iter().enumerate() {
        if let Some((aircraft, seen_pos, emergency)) = aircraft_from_entry(entry) {
            ranked.push(Ranked {
                seen_pos,
                index,
                aircraft,
                emergency,
            });
        }
    }
    ranked.sort_by(|a, b| {
        a.seen_pos
            .total_cmp(&b.seen_pos)
            .then(a.index.cmp(&b.index))
    });
    ranked.truncate(TRACK_MAX_AIRCRAFT);
    Ok((now.trunc() as u32, ranked))
}

fn aircraft_from_entry(entry: &Value) -> Option<(Aircraft, f64, Option<EmergencyKind>)> {
    let hex = entry.get("hex")?.as_str()?;
    let icao = parse_icao(hex)?;
    let latitude = finite_number(entry.get("lat")?)?;
    let longitude = finite_number(entry.get("lon")?)?;
    if !(-90.0..=90.0).contains(&latitude) || !(-180.0..=180.0).contains(&longitude) {
        return None;
    }
    let seen_pos = finite_number(entry.get("seen_pos")?)?;
    if seen_pos > POSITION_MAX_AGE_S {
        return None;
    }
    Some((
        Aircraft {
            icao,
            latitude_e7: deg_e7(latitude),
            longitude_e7: deg_e7(longitude),
            altitude_ft: altitude(entry),
            ground_speed_kt: ground_speed(entry),
            heading_deg: heading(entry),
        },
        seen_pos,
        emergency_kind(entry),
    ))
}

/// readsb `emergency` other than `none`, otherwise squawk 7700, 7600, or 7500.
/// The emergency field wins when both are set.
fn emergency_kind(entry: &Value) -> Option<EmergencyKind> {
    if let Some(kind) = kind_from_emergency_field(entry) {
        return Some(kind);
    }
    kind_from_squawk(entry)
}

fn kind_from_emergency_field(entry: &Value) -> Option<EmergencyKind> {
    let text = entry.get("emergency")?.as_str()?.trim();
    if text.is_empty() || text.eq_ignore_ascii_case("none") {
        return None;
    }
    Some(if text.eq_ignore_ascii_case("general") {
        EmergencyKind::General
    } else if text.eq_ignore_ascii_case("lifeguard") {
        EmergencyKind::Lifeguard
    } else if text.eq_ignore_ascii_case("minfuel") {
        EmergencyKind::MinFuel
    } else if text.eq_ignore_ascii_case("nordo") {
        EmergencyKind::Nordo
    } else if text.eq_ignore_ascii_case("unlawful") {
        EmergencyKind::Unlawful
    } else if text.eq_ignore_ascii_case("downed") {
        EmergencyKind::Downed
    } else {
        EmergencyKind::Reserved
    })
}

fn kind_from_squawk(entry: &Value) -> Option<EmergencyKind> {
    let value = entry.get("squawk")?;
    if let Some(text) = value.as_str() {
        return squawk_code(text.trim());
    }
    let number = value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|number| u64::try_from(number).ok()))?;
    match number {
        7700 => Some(EmergencyKind::General),
        7600 => Some(EmergencyKind::Nordo),
        7500 => Some(EmergencyKind::Unlawful),
        _ => None,
    }
}

fn squawk_code(text: &str) -> Option<EmergencyKind> {
    match text {
        "7700" => Some(EmergencyKind::General),
        "7600" => Some(EmergencyKind::Nordo),
        "7500" => Some(EmergencyKind::Unlawful),
        _ => None,
    }
}

/// readsb writes six hex digits, optionally with one leading `~`.
fn parse_icao(hex: &str) -> Option<u32> {
    let digits = hex.strip_prefix('~').unwrap_or(hex);
    if digits.len() != 6 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(digits, 16).ok()
}

fn finite_number(value: &Value) -> Option<f64> {
    value.as_f64().filter(|number| number.is_finite())
}

fn altitude(entry: &Value) -> i32 {
    match entry.get("alt_baro") {
        Some(Value::String(text)) if text == "ground" => 0,
        Some(Value::Number(number)) => number.as_f64().map(round_i32).unwrap_or(ALT_UNKNOWN),
        _ => entry
            .get("alt_geom")
            .and_then(finite_number)
            .map(round_i32)
            .unwrap_or(ALT_UNKNOWN),
    }
}

fn round_i32(number: f64) -> i32 {
    if !number.is_finite() {
        return ALT_UNKNOWN;
    }
    let rounded = number.round();
    if rounded < f64::from(i32::MIN) || rounded > f64::from(i32::MAX) {
        ALT_UNKNOWN
    } else {
        rounded as i32
    }
}

fn ground_speed(entry: &Value) -> u16 {
    let Some(knots) = entry.get("gs").and_then(finite_number) else {
        return SPEED_UNKNOWN;
    };
    if knots < 0.0 {
        return SPEED_UNKNOWN;
    }
    let rounded = knots.round();
    if rounded >= 65535.0 {
        65534
    } else {
        rounded as u16
    }
}

fn heading(entry: &Value) -> u16 {
    let Some(degrees) = entry.get("track").and_then(finite_number) else {
        return HEADING_UNKNOWN;
    };
    let rounded = degrees.round();
    if !(0.0..=360.0).contains(&rounded) {
        return HEADING_UNKNOWN;
    }
    let degrees = rounded as u16;
    if degrees == 360 {
        0
    } else {
        degrees
    }
}

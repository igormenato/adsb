//! Turn readsb JSON into a [`Fix`] or a file [`Snapshot`].

use serde_json::Value;

use crate::frame::{
    deg_e7, Aircraft, EmergencyKind, Fix, Snapshot, ALT_UNKNOWN, HEADING_UNKNOWN, SPEED_UNKNOWN,
    TRACK_MAX_AIRCRAFT,
};

/// File parser only. A JSON-port line is one accepted position and is not aged.
pub const POSITION_MAX_AGE_S: f64 = 2.0;

/// File parser only. An emergency that still has a latitude and longitude is kept until this age.
pub const EMERGENCY_POSITION_MAX_AGE_S: f64 = 30.0;

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

/// Qualifying aircraft from one `aircraft.json`, emergencies first, then the freshest `seen_pos`, at most 64.
pub fn snapshot_from_aircraft_json(bytes: &[u8]) -> Result<Snapshot, SnapshotError> {
    let (unix_s, ranked) = ranked_aircraft(bytes)?;
    Ok(Snapshot {
        unix_s,
        aircraft: ranked.into_iter().map(|entry| entry.aircraft).collect(),
    })
}

/// One `--net-json-port` line. `None` means skip the line.
///
/// `now` has to be a Unix second that fits in a `u32`. Latitude and longitude have to be present.
/// `seen_pos` is ignored. On this port it is 0, and the line's `now` is the fix time.
pub fn fix_from_json_line(bytes: &[u8]) -> Option<Fix> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let unix_s = parse_now(&value).ok()?;
    Some(Fix {
        unix_s,
        aircraft: aircraft_from_position(&value)?,
    })
}

struct Ranked {
    seen_pos: f64,
    index: usize,
    aircraft: Aircraft,
}

fn ranked_aircraft(bytes: &[u8]) -> Result<(u32, Vec<Ranked>), SnapshotError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| SnapshotError::Json)?;
    let unix_s = parse_now(&value)?;
    let list = value
        .get("aircraft")
        .and_then(Value::as_array)
        .ok_or(SnapshotError::Json)?;

    let mut ranked = Vec::new();
    for (index, entry) in list.iter().enumerate() {
        if let Some((aircraft, seen_pos)) = aircraft_from_file_entry(entry) {
            ranked.push(Ranked {
                seen_pos,
                index,
                aircraft,
            });
        }
    }
    ranked.sort_by(|a, b| {
        b.aircraft
            .emergency
            .is_some()
            .cmp(&a.aircraft.emergency.is_some())
            .then(a.seen_pos.total_cmp(&b.seen_pos))
            .then(a.index.cmp(&b.index))
    });
    ranked.truncate(TRACK_MAX_AIRCRAFT);
    Ok((unix_s, ranked))
}

fn parse_now(value: &Value) -> Result<u32, SnapshotError> {
    let now = value
        .get("now")
        .and_then(Value::as_f64)
        .ok_or(SnapshotError::MissingNow)?;
    if !now.is_finite() || now < 0.0 || now > f64::from(u32::MAX) {
        return Err(SnapshotError::BadNow);
    }
    Ok(now.trunc() as u32)
}

fn aircraft_from_file_entry(entry: &Value) -> Option<(Aircraft, f64)> {
    let aircraft = aircraft_from_position(entry)?;
    let seen_pos = finite_number(entry.get("seen_pos")?)?;
    let max_age = if aircraft.emergency.is_some() {
        EMERGENCY_POSITION_MAX_AGE_S
    } else {
        POSITION_MAX_AGE_S
    };
    if seen_pos > max_age {
        return None;
    }
    Some((aircraft, seen_pos))
}

fn aircraft_from_position(entry: &Value) -> Option<Aircraft> {
    let hex = entry.get("hex")?.as_str()?;
    let icao = parse_icao(hex)?;
    let latitude = finite_number(entry.get("lat")?)?;
    let longitude = finite_number(entry.get("lon")?)?;
    if !(-90.0..=90.0).contains(&latitude) || !(-180.0..=180.0).contains(&longitude) {
        return None;
    }
    Some(Aircraft {
        icao,
        latitude_e7: deg_e7(latitude),
        longitude_e7: deg_e7(longitude),
        altitude_ft: altitude(entry),
        ground_speed_kt: ground_speed(entry),
        heading_deg: heading(entry),
        emergency: emergency_kind(entry),
    })
}

/// A known readsb emergency name wins. Squawk 7700, 7600, or 7500 applies when that
/// name is absent, empty, `none`, or not one of the six names, including `reserved`.
fn emergency_kind(entry: &Value) -> Option<EmergencyKind> {
    match entry.get("emergency").and_then(Value::as_str) {
        Some(text) => {
            let text = text.trim();
            if text.is_empty() || text.eq_ignore_ascii_case("none") {
                kind_from_squawk(entry)
            } else {
                kind_from_name(text).or_else(|| kind_from_squawk(entry))
            }
        }
        _ => kind_from_squawk(entry),
    }
}

fn kind_from_name(text: &str) -> Option<EmergencyKind> {
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
        return None;
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
    squawk_code(&number.to_string())
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

//! Turn one readsb `aircraft.json` document into a [`Snapshot`].

use serde_json::Value;

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

/// Aircraft with a latitude and longitude no older than [`POSITION_MAX_AGE_S`], freshest `seen_pos` first, at most 64.
pub fn snapshot_from_aircraft_json(bytes: &[u8]) -> Result<Snapshot, SnapshotError> {
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
        if let Some((aircraft, seen_pos)) = aircraft_from_entry(entry) {
            ranked.push((seen_pos, index, aircraft));
        }
    }
    ranked.sort_by(|a, b| {
        a.0.partial_cmp(&b.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.cmp(&b.1))
    });
    ranked.truncate(TRACK_MAX_AIRCRAFT);

    Ok(Snapshot {
        unix_s: now.trunc() as u32,
        aircraft: ranked
            .into_iter()
            .map(|(_, _, aircraft)| aircraft)
            .collect(),
    })
}

fn aircraft_from_entry(entry: &Value) -> Option<(Aircraft, f64)> {
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
            callsign: callsign(entry.get("flight").and_then(Value::as_str)),
            latitude_e7: deg_e7(latitude),
            longitude_e7: deg_e7(longitude),
            altitude_ft: altitude(entry),
            ground_speed_kt: ground_speed(entry),
            heading_deg: heading(entry),
        },
        seen_pos,
    ))
}

fn parse_icao(hex: &str) -> Option<u32> {
    let digits: String = hex.chars().filter(char::is_ascii_hexdigit).collect();
    if digits.is_empty() || digits.len() > 6 {
        return None;
    }
    u32::from_str_radix(&digits, 16).ok()
}

fn finite_number(value: &Value) -> Option<f64> {
    value.as_f64().filter(|number| number.is_finite())
}

fn callsign(flight: Option<&str>) -> [u8; 8] {
    let mut out = [b' '; 8];
    let Some(flight) = flight else {
        return out;
    };
    let mut index = 0;
    for byte in flight.trim().bytes() {
        if index == 8 {
            break;
        }
        if byte.is_ascii_alphanumeric() || byte == b' ' {
            out[index] = byte.to_ascii_uppercase();
            index += 1;
        }
    }
    out
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
    if rounded > f64::from(u16::MAX) {
        return 65534;
    }
    (rounded as u16).min(65534)
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

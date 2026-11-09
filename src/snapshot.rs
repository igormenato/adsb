//! Turn one readsb `--net-json` line into a [`Fix`].

use serde_json::Value;

use crate::frame::{Aircraft, Callsign, EmergencyKind, Fix, Icao, deg_e7};

/// One `--net-json-port` line. `None` means skip the line.
///
/// `now` has to be a Unix second that fits in a `u32`. Latitude and longitude have to be present.
/// `seen_pos` is ignored. On this port it is 0, and the line's `now` is the fix time.
pub fn fix_from_json_line(bytes: &[u8]) -> Option<Fix> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let unix_s = parse_now(&value)?;
    Some(Fix {
        unix_s,
        aircraft: aircraft_from_position(&value)?,
    })
}

fn parse_now(value: &Value) -> Option<u32> {
    let now = value.get("now").and_then(Value::as_f64)?;
    if !now.is_finite() || now < 0.0 || now > f64::from(u32::MAX) {
        return None;
    }
    Some(now.trunc() as u32)
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
        callsign: Callsign::pad(entry.get("flight").and_then(Value::as_str).unwrap_or("")),
        latitude_e7: deg_e7(latitude),
        longitude_e7: deg_e7(longitude),
        altitude_ft: altitude(entry),
        ground_speed_kt: ground_speed(entry),
        track_deg: track(entry),
        emergency: emergency_kind(entry),
    })
}

/// A known name wins. `reserved` still yields to squawk 7700, 7600, or 7500.
/// Those squawks apply when the name is missing, empty, `none`, or unknown.
fn emergency_kind(entry: &Value) -> Option<EmergencyKind> {
    let named = entry
        .get("emergency")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty() && !text.eq_ignore_ascii_case("none"))
        .and_then(kind_from_name);
    let squawk = kind_from_squawk(entry);
    match named {
        Some(kind) if kind != EmergencyKind::Reserved => Some(kind),
        named => squawk.or(named),
    }
}

fn kind_from_name(text: &str) -> Option<EmergencyKind> {
    const NAMES: &[(&str, EmergencyKind)] = &[
        ("general", EmergencyKind::General),
        ("lifeguard", EmergencyKind::Lifeguard),
        ("minfuel", EmergencyKind::MinFuel),
        ("nordo", EmergencyKind::Nordo),
        ("unlawful", EmergencyKind::Unlawful),
        ("downed", EmergencyKind::Downed),
        ("reserved", EmergencyKind::Reserved),
    ];
    NAMES
        .iter()
        .find(|(name, _)| text.eq_ignore_ascii_case(name))
        .map(|(_, kind)| *kind)
}

fn kind_from_squawk(entry: &Value) -> Option<EmergencyKind> {
    match entry.get("squawk")? {
        Value::String(text) => squawk_code(text.trim()),
        Value::Number(number) => squawk_code(&number.as_u64()?.to_string()),
        _ => None,
    }
}

/// readsb prints four digits. `07700` is not `7700`.
fn squawk_code(text: &str) -> Option<EmergencyKind> {
    match text {
        "7700" => Some(EmergencyKind::General),
        "7600" => Some(EmergencyKind::Nordo),
        "7500" => Some(EmergencyKind::Unlawful),
        _ => None,
    }
}

/// readsb writes six hex digits, optionally with one leading `~`.
fn parse_icao(hex: &str) -> Option<Icao> {
    let digits = hex.strip_prefix('~').unwrap_or(hex);
    if digits.len() != 6 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    u32::from_str_radix(digits, 16)
        .ok()
        .and_then(Icao::from_u24)
}

fn finite_number(value: &Value) -> Option<f64> {
    value.as_f64().filter(|number| number.is_finite())
}

fn altitude(entry: &Value) -> Option<i32> {
    match entry.get("alt_baro") {
        Some(Value::Number(number)) => match number.as_f64() {
            Some(n) => round_i32(n),
            None => geometric(entry),
        },
        _ => geometric(entry),
    }
}

fn geometric(entry: &Value) -> Option<i32> {
    entry
        .get("alt_geom")
        .and_then(finite_number)
        .and_then(round_i32)
}

fn round_i32(number: f64) -> Option<i32> {
    if !number.is_finite() {
        return None;
    }
    let rounded = number.round();
    if rounded <= f64::from(i32::MIN) || rounded > f64::from(i32::MAX) {
        None
    } else {
        Some(rounded as i32)
    }
}

fn ground_speed(entry: &Value) -> Option<u16> {
    let knots = entry.get("gs").and_then(finite_number)?;
    if knots < 0.0 {
        return None;
    }
    let rounded = knots.round();
    if rounded >= 65535.0 {
        Some(65534)
    } else {
        Some(rounded as u16)
    }
}

fn track(entry: &Value) -> Option<u16> {
    let degrees = entry.get("track").and_then(finite_number)?;
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

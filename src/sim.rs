//! Scripted Mode S scenario as `ts,hex` lines.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;
use serde::Deserialize;

use super::modes::{airborne_position, hex14, identification, status, velocity};

#[derive(Parser, Debug)]
pub struct Args {
    /// Scenario file.
    scenario: PathBuf,
    /// Write frames here instead of stdout.
    #[arg(long, value_name = "FILE")]
    out: Option<PathBuf>,
    /// Pose encoded into each position pair.
    #[arg(long, value_name = "FILE")]
    truth: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    duration: f64,
    aircraft: Vec<RawAircraft>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawAircraft {
    icao: String,
    callsign: String,
    squawk: Option<String>,
    lat: f64,
    lon: f64,
    alt_ft: i32,
    heading_deg: f64,
    speed_kt: f64,
    rate_hz: f64,
    turn_at: Option<f64>,
    heading_to_deg: Option<f64>,
    climb_at: Option<f64>,
    alt_to_ft: Option<i32>,
}

#[derive(Clone, Debug)]
struct Aircraft {
    icao: u32,
    callsign: String,
    squawk: Option<u16>,
    lat: f64,
    lon: f64,
    alt_ft: i32,
    heading_deg: f64,
    speed_kt: f64,
    rate_hz: f64,
    turn_at: Option<f64>,
    heading_to_deg: Option<f64>,
    climb_at: Option<f64>,
    alt_to_ft: Option<i32>,
}

#[derive(Clone, Copy, Debug)]
struct Pose {
    lat: f64,
    lon: f64,
    alt_ft: i32,
    heading_deg: f64,
    speed_kt: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Event {
    Ident,
    Status,
    Velocity,
    Position,
}

#[derive(Debug)]
struct Scenario {
    duration: f64,
    aircraft: Vec<Aircraft>,
}

pub fn run(opts: Args) -> ExitCode {
    let (frames, truth) = match generate(&opts.scenario) {
        Ok(out) => out,
        Err(err) => {
            eprintln!("sim: {err}");
            return ExitCode::from(1);
        }
    };
    if let Err(err) = write_out(opts.out.as_deref(), &frames) {
        eprintln!("sim: {err}");
        return ExitCode::from(1);
    }
    if let Some(path) = opts.truth.as_deref()
        && let Err(err) = fs::write(path, truth)
    {
        eprintln!("sim: {err}");
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

fn write_out(path: Option<&Path>, text: &str) -> io::Result<()> {
    match path {
        Some(path) => fs::write(path, text),
        None => {
            let mut out = io::stdout().lock();
            out.write_all(text.as_bytes())?;
            out.flush()
        }
    }
}

fn generate(path: &Path) -> Result<(String, String), String> {
    let scenario = load(path)?;
    Ok(render(&scenario))
}

fn load(path: &Path) -> Result<Scenario, String> {
    let text = fs::read_to_string(path).map_err(|err| format!("{}: {err}", path.display()))?;
    let file: File = toml::from_str(&text).map_err(|err| format!("{}: {err}", path.display()))?;
    check(file)
}

fn check(file: File) -> Result<Scenario, String> {
    if !file.duration.is_finite() || file.duration <= 0.0 {
        return Err("duration must be > 0".into());
    }
    if file.aircraft.is_empty() {
        return Err("scenario needs at least one [[aircraft]]".into());
    }
    let mut aircraft = Vec::with_capacity(file.aircraft.len());
    for (i, raw) in file.aircraft.into_iter().enumerate() {
        aircraft.push(parse_aircraft(i, raw)?);
    }
    Ok(Scenario {
        duration: file.duration,
        aircraft,
    })
}

fn parse_aircraft(i: usize, raw: RawAircraft) -> Result<Aircraft, String> {
    let at = |msg: String| format!("aircraft[{i}]: {msg}");
    let icao = parse_icao(&raw.icao).map_err(at)?;
    let callsign = parse_callsign(&raw.callsign).map_err(at)?;
    let squawk = match raw.squawk {
        Some(text) => Some(parse_squawk(&text).map_err(at)?),
        None => None,
    };
    if !raw.lat.is_finite() || !(-90.0..=90.0).contains(&raw.lat) {
        return Err(at("lat must be finite in -90..=90".into()));
    }
    if !raw.lon.is_finite() || !(-180.0..=180.0).contains(&raw.lon) {
        return Err(at("lon must be finite in -180..=180".into()));
    }
    if !(-1000..=50_000).contains(&raw.alt_ft) {
        return Err(at("alt_ft is out of range".into()));
    }
    if !raw.heading_deg.is_finite() {
        return Err(at("heading_deg must be finite".into()));
    }
    if !raw.speed_kt.is_finite() || raw.speed_kt < 0.0 {
        return Err(at("speed_kt must be >= 0".into()));
    }
    if !raw.rate_hz.is_finite() || raw.rate_hz <= 0.0 {
        return Err(at("rate_hz must be > 0".into()));
    }
    match (raw.turn_at, raw.heading_to_deg) {
        (None, None) => {}
        (Some(t), Some(h)) if t.is_finite() && t >= 0.0 && h.is_finite() => {}
        _ => {
            return Err(at(
                "turn_at and heading_to_deg must both be set to a finite heading change".into(),
            ));
        }
    }
    match (raw.climb_at, raw.alt_to_ft) {
        (None, None) => {}
        (Some(t), Some(alt)) if t.is_finite() && t >= 0.0 && (-1000..=50_000).contains(&alt) => {}
        _ => {
            return Err(at(
                "climb_at and alt_to_ft must both be set to a finite altitude change".into(),
            ));
        }
    }
    Ok(Aircraft {
        icao,
        callsign,
        squawk,
        lat: raw.lat,
        lon: raw.lon,
        alt_ft: raw.alt_ft,
        heading_deg: raw.heading_deg,
        speed_kt: raw.speed_kt,
        rate_hz: raw.rate_hz,
        turn_at: raw.turn_at,
        heading_to_deg: raw.heading_to_deg,
        climb_at: raw.climb_at,
        alt_to_ft: raw.alt_to_ft,
    })
}

fn parse_icao(text: &str) -> Result<u32, String> {
    let text = text.trim();
    if text.len() != 6 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("icao must be 6 hex digits".into());
    }
    u32::from_str_radix(text, 16).map_err(|err| err.to_string())
}

fn parse_callsign(text: &str) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() || text.len() > 8 {
        return Err("callsign must be 1..=8 characters".into());
    }
    if !text
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == ' ')
    {
        return Err("callsign must be A-Z, 0-9, or space".into());
    }
    Ok(text.to_string())
}

fn parse_squawk(text: &str) -> Result<u16, String> {
    let text = text.trim();
    if text.len() != 4 || !text.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
        return Err("squawk must be 4 octal digits".into());
    }
    let b = text.as_bytes();
    Ok(u16::from(b[0] - b'0') << 12
        | u16::from(b[1] - b'0') << 8
        | u16::from(b[2] - b'0') << 4
        | u16::from(b[3] - b'0'))
}

fn render(scenario: &Scenario) -> (String, String) {
    let mut rows = Vec::new();
    let mut truth_rows = Vec::new();
    for (index, ac) in scenario.aircraft.iter().enumerate() {
        let dt = 1.0 / ac.rate_hz;
        let mut n = 0u32;
        loop {
            let t = f64::from(n) * dt;
            if t >= scenario.duration {
                break;
            }
            let pose = ac.pose(t);
            let event = event(n, ac.squawk.is_some());
            match event {
                Event::Ident => {
                    rows.push((
                        ms(t),
                        index,
                        n,
                        hex14(&identification(ac.icao, &ac.callsign)),
                    ));
                }
                Event::Status => {
                    rows.push((
                        ms(t),
                        index,
                        n,
                        hex14(&status(ac.icao, ac.squawk.unwrap_or(0))),
                    ));
                }
                Event::Velocity => {
                    rows.push((
                        ms(t),
                        index,
                        n,
                        hex14(&velocity(ac.icao, pose.heading_deg, pose.speed_kt)),
                    ));
                }
                Event::Position => {
                    let even = airborne_position(ac.icao, pose.lat, pose.lon, pose.alt_ft, false);
                    let odd = airborne_position(ac.icao, pose.lat, pose.lon, pose.alt_ft, true);
                    rows.push((ms(t), index, n, hex14(&even)));
                    rows.push((ms(t), index, n + 1_000_000, hex14(&odd)));
                    truth_rows.push((ms(t), ac.icao, pose.lat, pose.lon, pose.alt_ft));
                }
            }
            n += 1;
        }
    }
    rows.sort_by_key(|row| (row.0, row.1, row.2));
    truth_rows.sort_by_key(|row| (row.0, row.1));
    let mut out = String::new();
    for (ms, _, _, hex) in rows {
        out.push_str(&format_ts(ms));
        out.push(',');
        out.push_str(&hex);
        out.push('\n');
    }
    let mut truth = String::from("ts,icao,lat,lon,alt_ft\n");
    for (ms, icao, lat, lon, alt_ft) in truth_rows {
        truth.push_str(&format_ts(ms));
        truth.push_str(&format!(",{icao:06x},{lat:.8},{lon:.8},{alt_ft}\n"));
    }
    (out, truth)
}

fn event(n: u32, has_squawk: bool) -> Event {
    match n % 3 {
        0 => Event::Ident,
        1 => {
            if has_squawk && n % 6 == 1 {
                Event::Status
            } else {
                Event::Velocity
            }
        }
        _ => Event::Position,
    }
}

impl Aircraft {
    fn pose(&self, t: f64) -> Pose {
        let heading = match (self.turn_at, self.heading_to_deg) {
            (Some(at), Some(next)) if t >= at => next,
            _ => self.heading_deg,
        };
        let alt_ft = match (self.climb_at, self.alt_to_ft) {
            (Some(at), Some(next)) if t >= at => next,
            _ => self.alt_ft,
        };
        let (lat, lon) = match self.turn_at {
            Some(at) if t > 0.0 && at > 0.0 => {
                let first = t.min(at);
                let (lat, lon) =
                    advance(self.lat, self.lon, self.heading_deg, self.speed_kt, first);
                if t <= at {
                    (lat, lon)
                } else {
                    advance(lat, lon, heading, self.speed_kt, t - at)
                }
            }
            _ => advance(self.lat, self.lon, heading, self.speed_kt, t),
        };
        Pose {
            lat,
            lon,
            alt_ft,
            heading_deg: heading,
            speed_kt: self.speed_kt,
        }
    }
}

fn advance(lat: f64, lon: f64, heading_deg: f64, speed_kt: f64, dt_s: f64) -> (f64, f64) {
    if dt_s <= 0.0 || speed_kt == 0.0 {
        return (lat, lon);
    }
    let nm = speed_kt * (dt_s / 3600.0);
    let h = heading_deg.to_radians();
    let dlat = nm * h.cos() / 60.0;
    let mean = (lat + dlat / 2.0).to_radians();
    let dlon = if mean.cos().abs() < 1e-12 {
        0.0
    } else {
        nm * h.sin() / (60.0 * mean.cos())
    };
    let lat = (lat + dlat).clamp(-90.0, 90.0);
    let mut lon = lon + dlon;
    if lon > 180.0 {
        lon -= 360.0;
    } else if lon < -180.0 {
        lon += 360.0;
    }
    (lat, lon)
}

fn ms(t: f64) -> i64 {
    (t * 1000.0).round() as i64
}

fn format_ts(ms: i64) -> String {
    format!("{:.3}", ms as f64 / 1000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_tmp(name: &str, body: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "trck-sim-{}-{}-{name}",
            std::process::id(),
            body.len()
        ));
        let mut f = fs::File::create(&path).expect("tmp");
        f.write_all(body.as_bytes()).expect("write");
        path
    }

    fn sample() -> String {
        r#"
duration = 4
[[aircraft]]
icao = "ABC001"
callsign = "TAM1234"
squawk = "7700"
lat = 50.0
lon = 8.0
alt_ft = 35000
heading_deg = 90
speed_kt = 400
rate_hz = 2
"#
        .into()
    }

    #[test]
    fn malformed_scenario_is_an_error() {
        let path = write_tmp("bad.toml", "duration = 1\n");
        let err = load(&path).unwrap_err();
        assert!(err.contains("aircraft"), "{err}");
        let path = write_tmp("icao.toml", &sample().replace("ABC001", "zz"));
        let err = load(&path).unwrap_err();
        assert!(err.contains("icao"), "{err}");
        let path = write_tmp("not-toml.toml", "[[[");
        let err = load(&path).unwrap_err();
        assert!(err.contains("not-toml.toml"), "{err}");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn output_is_byte_identical() {
        let path = write_tmp("same.toml", &sample());
        let a = generate(&path).expect("a").0;
        let b = generate(&path).expect("b").0;
        assert_eq!(a, b);
        assert!(a.starts_with("0.000,"));
        let _ = fs::remove_file(path);
    }
}

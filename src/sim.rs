//! Scripted Mode S scenario as `ts,hex` lines.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, Result, bail};
use clap::Parser;
use serde::Deserialize;
use serde_json::Value;

use trck::modes::{airborne_position, hex14, identification, status, velocity};
use trck::parse_hex_frame;

use super::iq::{Synth, encode_cu8};
use super::radio;

const GEOM_ALT: u32 = 8;

#[derive(Parser, Debug)]
pub struct Args {
    /// Scenario file. Omit when using --trace.
    #[arg(required_unless_present = "trace")]
    scenario: Option<PathBuf>,
    /// readsb globe_history trace JSON.
    #[arg(long, value_name = "FILE", num_args = 1.., conflicts_with = "scenario")]
    trace: Vec<PathBuf>,
    /// Keep samples at or after this Unix time (trace timestamp + dt).
    #[arg(long, value_name = "UNIX", requires = "trace")]
    from: Option<f64>,
    /// Keep samples before this Unix time.
    #[arg(long, value_name = "UNIX", requires = "trace")]
    until: Option<f64>,
    /// Write frames here instead of stdout.
    #[arg(long, value_name = "FILE")]
    out: Option<PathBuf>,
    /// Pose encoded into each position pair.
    #[arg(long, value_name = "FILE")]
    truth: Option<PathBuf>,
    /// RTL-SDR cu8 I/Q of the generated frames.
    #[arg(long, value_name = "FILE")]
    iq_out: Option<PathBuf>,
    /// Sample rate for --iq-out.
    #[arg(long, default_value_t = radio::SAMPLE_HZ)]
    iq_rate: u32,
    /// Pulse amplitude 0..=1 for --iq-out.
    #[arg(long, default_value_t = 0.8)]
    iq_amp: f64,
    /// Gaussian noise SNR in dB for --iq-out. Omit for a clean file.
    #[arg(long, requires = "iq_out")]
    iq_snr: Option<f64>,
    /// RNG seed for --iq-out noise.
    #[arg(long, default_value_t = 1)]
    iq_seed: u64,
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

#[derive(Clone, Debug)]
struct Sample {
    t: f64,
    lat: f64,
    lon: f64,
    alt_ft: i32,
    heading_deg: Option<f64>,
    speed_kt: Option<f64>,
    callsign: Option<String>,
    squawk: Option<u16>,
}

#[derive(Clone, Debug)]
struct Track {
    icao: u32,
    samples: Vec<Sample>,
}

#[derive(Debug, Deserialize)]
struct TraceFile {
    icao: String,
    timestamp: f64,
    #[serde(default)]
    trace: Vec<Vec<Value>>,
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
    match run_inner(opts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("sim: {}", super::error_chain(&err));
            ExitCode::from(1)
        }
    }
}

fn run_inner(opts: Args) -> Result<()> {
    let (frames, truth) = generate(&opts)?;
    write_out(opts.out.as_deref(), &frames)?;
    if let Some(path) = opts.truth.as_deref() {
        fs::write(path, truth)?;
    }
    if let Some(path) = opts.iq_out.as_deref() {
        write_iq(path, &frames, &opts)?;
    }
    Ok(())
}

fn write_iq(path: &Path, text: &str, opts: &Args) -> Result<()> {
    let mut frames = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let hex = line.split_once(',').map(|p| p.1).unwrap_or(line);
        let raw = parse_hex_frame(hex.as_bytes())
            .ok_or_else(|| anyhow::anyhow!("iq frame {i}: {hex}"))?;
        if raw.len() != 14 {
            bail!("iq frame {i}: expected 14 bytes");
        }
        let mut frame = [0u8; 14];
        frame.copy_from_slice(&raw);
        frames.push(frame);
    }
    let bytes = encode_cu8(
        &frames,
        Synth {
            amplitude: opts.iq_amp,
            snr_db: opts.iq_snr,
            seed: opts.iq_seed,
            sample_hz: opts.iq_rate,
            overlap_us: None,
        },
    )?;
    fs::write(path, bytes).with_context(|| path.display().to_string())
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

fn generate(opts: &Args) -> Result<(String, String)> {
    if !opts.trace.is_empty() {
        return Ok(render(&load_traces(&opts.trace, opts.from, opts.until)?));
    }
    let path = opts
        .scenario
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("scenario or --trace"))?;
    let scenario = load(path)?;
    Ok(render(&scenario_tracks(&scenario)))
}

fn load(path: &Path) -> Result<Scenario> {
    let text = fs::read_to_string(path).with_context(|| path.display().to_string())?;
    let file: File = toml::from_str(&text).with_context(|| path.display().to_string())?;
    check(file)
}

fn check(file: File) -> Result<Scenario> {
    if !file.duration.is_finite() || file.duration <= 0.0 {
        bail!("duration must be > 0");
    }
    if file.aircraft.is_empty() {
        bail!("scenario needs at least one [[aircraft]]");
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

fn parse_aircraft(i: usize, raw: RawAircraft) -> Result<Aircraft> {
    let icao = parse_icao(&raw.icao).with_context(|| format!("aircraft[{i}]"))?;
    let callsign = parse_callsign(&raw.callsign).with_context(|| format!("aircraft[{i}]"))?;
    let squawk = match raw.squawk {
        Some(text) => Some(parse_squawk(&text).with_context(|| format!("aircraft[{i}]"))?),
        None => None,
    };
    if !raw.lat.is_finite() || !(-90.0..=90.0).contains(&raw.lat) {
        bail!("aircraft[{i}]: lat must be finite in -90..=90");
    }
    if !raw.lon.is_finite() || !(-180.0..=180.0).contains(&raw.lon) {
        bail!("aircraft[{i}]: lon must be finite in -180..=180");
    }
    if !(-1000..=50_000).contains(&raw.alt_ft) {
        bail!("aircraft[{i}]: alt_ft is out of range");
    }
    if !raw.heading_deg.is_finite() {
        bail!("aircraft[{i}]: heading_deg must be finite");
    }
    if !raw.speed_kt.is_finite() || raw.speed_kt < 0.0 {
        bail!("aircraft[{i}]: speed_kt must be >= 0");
    }
    if !raw.rate_hz.is_finite() || raw.rate_hz <= 0.0 {
        bail!("aircraft[{i}]: rate_hz must be > 0");
    }
    match (raw.turn_at, raw.heading_to_deg) {
        (None, None) => {}
        (Some(t), Some(h)) if t.is_finite() && t >= 0.0 && h.is_finite() => {}
        _ => {
            bail!(
                "aircraft[{i}]: turn_at and heading_to_deg must both be set to a finite heading change"
            );
        }
    }
    match (raw.climb_at, raw.alt_to_ft) {
        (None, None) => {}
        (Some(t), Some(alt)) if t.is_finite() && t >= 0.0 && (-1000..=50_000).contains(&alt) => {}
        _ => {
            bail!(
                "aircraft[{i}]: climb_at and alt_to_ft must both be set to a finite altitude change"
            );
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

fn parse_icao(text: &str) -> Result<u32> {
    let text = text.trim();
    if text.len() != 6 || !text.bytes().all(|b| b.is_ascii_hexdigit()) {
        bail!("icao must be 6 hex digits");
    }
    Ok(u32::from_str_radix(text, 16)?)
}

fn parse_callsign(text: &str) -> Result<String> {
    let text = text.trim();
    if text.is_empty() || text.len() > 8 {
        bail!("callsign must be 1..=8 characters");
    }
    if !text
        .chars()
        .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == ' ')
    {
        bail!("callsign must be A-Z, 0-9, or space");
    }
    Ok(text.to_string())
}

fn parse_squawk(text: &str) -> Result<u16> {
    let text = text.trim();
    if text.len() != 4 || !text.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
        bail!("squawk must be 4 octal digits");
    }
    let b = text.as_bytes();
    Ok(u16::from(b[0] - b'0') << 12
        | u16::from(b[1] - b'0') << 8
        | u16::from(b[2] - b'0') << 4
        | u16::from(b[3] - b'0'))
}

fn scenario_tracks(scenario: &Scenario) -> Vec<Track> {
    scenario
        .aircraft
        .iter()
        .map(|ac| {
            let dt = 1.0 / ac.rate_hz;
            let mut samples = Vec::new();
            let mut n = 0u32;
            loop {
                let t = f64::from(n) * dt;
                if t >= scenario.duration {
                    break;
                }
                let pose = ac.pose(t);
                samples.push(Sample {
                    t,
                    lat: pose.lat,
                    lon: pose.lon,
                    alt_ft: pose.alt_ft,
                    heading_deg: Some(pose.heading_deg),
                    speed_kt: Some(pose.speed_kt),
                    callsign: Some(ac.callsign.clone()),
                    squawk: ac.squawk,
                });
                n += 1;
            }
            Track {
                icao: ac.icao,
                samples,
            }
        })
        .collect()
}

fn load_traces(paths: &[PathBuf], from: Option<f64>, until: Option<f64>) -> Result<Vec<Track>> {
    if let (Some(from), Some(until)) = (from, until)
        && from.partial_cmp(&until) != Some(std::cmp::Ordering::Less)
    {
        bail!("--from must be < --until");
    }
    let mut tracks = Vec::with_capacity(paths.len());
    for path in paths {
        tracks.push(load_trace(path, from, until)?);
    }
    if tracks.iter().all(|track| track.samples.is_empty()) {
        bail!("no airborne samples in window");
    }
    Ok(tracks)
}

fn load_trace(path: &Path, from: Option<f64>, until: Option<f64>) -> Result<Track> {
    let text = fs::read_to_string(path).with_context(|| path.display().to_string())?;
    let file: TraceFile =
        serde_json::from_str(&text).with_context(|| path.display().to_string())?;
    if !file.timestamp.is_finite() {
        bail!("{}: timestamp must be finite", path.display());
    }
    let icao = parse_icao(&file.icao).with_context(|| path.display().to_string())?;
    let mut samples = Vec::new();
    let mut callsign = None;
    let mut squawk = None;
    for row in &file.trace {
        if row.len() < 4 {
            continue;
        }
        let Some(dt) = json_f64(&row[0]) else {
            continue;
        };
        let t = file.timestamp + dt;
        if !t.is_finite() {
            continue;
        }
        if row.len() > 8 {
            let (next_cs, next_sq) = extra_ident(&row[8]);
            if next_cs.is_some() {
                callsign = next_cs;
            }
            if next_sq.is_some() {
                squawk = next_sq;
            }
        }
        if from.is_some_and(|from| t < from) || until.is_some_and(|until| t >= until) {
            continue;
        }
        let Some(lat) = json_f64(&row[1]) else {
            continue;
        };
        let Some(lon) = json_f64(&row[2]) else {
            continue;
        };
        if !(-90.0..=90.0).contains(&lat) || !(-180.0..=180.0).contains(&lon) {
            continue;
        }
        if row[3].as_str() == Some("ground") {
            continue;
        }
        let flags = row.get(6).and_then(json_u32).unwrap_or(0);
        if flags & GEOM_ALT != 0 {
            continue;
        }
        let Some(alt) = json_f64(&row[3]) else {
            continue;
        };
        if !(alt.is_finite() && (-1000.0..=50_000.0).contains(&alt)) {
            continue;
        }
        let heading_deg = row.get(5).and_then(json_f64).filter(|v| v.is_finite());
        let speed_kt = row
            .get(4)
            .and_then(json_f64)
            .filter(|v| v.is_finite() && *v >= 0.0);
        samples.push(Sample {
            t,
            lat,
            lon,
            alt_ft: alt.round() as i32,
            heading_deg,
            speed_kt,
            callsign: callsign.clone(),
            squawk,
        });
    }
    Ok(Track { icao, samples })
}

fn extra_ident(value: &Value) -> (Option<String>, Option<u16>) {
    let Some(obj) = value.as_object() else {
        return (None, None);
    };
    let callsign = obj
        .get("flight")
        .and_then(Value::as_str)
        .and_then(|s| parse_callsign(s).ok());
    let squawk = obj
        .get("squawk")
        .and_then(Value::as_str)
        .and_then(|s| parse_squawk(s).ok());
    (callsign, squawk)
}

fn json_f64(value: &Value) -> Option<f64> {
    value.as_f64().filter(|v| v.is_finite())
}

fn json_u32(value: &Value) -> Option<u32> {
    json_f64(value).map(|v| v as u32)
}

fn render(tracks: &[Track]) -> (String, String) {
    let mut rows = Vec::new();
    let mut truth_rows = Vec::new();
    for (index, track) in tracks.iter().enumerate() {
        for (n, sample) in track.samples.iter().enumerate() {
            let n = n as u32;
            match event(n, sample.squawk.is_some()) {
                Event::Ident => {
                    if let Some(callsign) = sample.callsign.as_deref() {
                        rows.push((
                            ms(sample.t),
                            index,
                            n,
                            hex14(&identification(track.icao, callsign)),
                        ));
                    }
                }
                Event::Status => {
                    rows.push((
                        ms(sample.t),
                        index,
                        n,
                        hex14(&status(track.icao, sample.squawk.unwrap_or(0))),
                    ));
                }
                Event::Velocity => {
                    if let (Some(heading_deg), Some(speed_kt)) =
                        (sample.heading_deg, sample.speed_kt)
                    {
                        rows.push((
                            ms(sample.t),
                            index,
                            n,
                            hex14(&velocity(track.icao, heading_deg, speed_kt)),
                        ));
                    }
                }
                Event::Position => {
                    let even =
                        airborne_position(track.icao, sample.lat, sample.lon, sample.alt_ft, false);
                    let odd =
                        airborne_position(track.icao, sample.lat, sample.lon, sample.alt_ft, true);
                    rows.push((ms(sample.t), index, n, hex14(&even)));
                    rows.push((ms(sample.t), index, n + 1_000_000, hex14(&odd)));
                    truth_rows.push((
                        ms(sample.t),
                        track.icao,
                        sample.lat,
                        sample.lon,
                        sample.alt_ft,
                    ));
                }
            }
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
        let err = super::super::error_chain(&load(&path).unwrap_err());
        assert!(err.contains("aircraft"), "{err}");
        let path = write_tmp("icao.toml", &sample().replace("ABC001", "zz"));
        let err = super::super::error_chain(&load(&path).unwrap_err());
        assert!(err.contains("icao"), "{err}");
        let path = write_tmp("not-toml.toml", "[[[");
        let err = super::super::error_chain(&load(&path).unwrap_err());
        assert!(err.contains("not-toml.toml"), "{err}");
        let _ = fs::remove_file(path);
    }

    fn generate_scenario(path: &Path) -> Result<(String, String)> {
        generate(&Args {
            scenario: Some(path.to_path_buf()),
            trace: Vec::new(),
            from: None,
            until: None,
            out: None,
            truth: None,
            iq_out: None,
            iq_rate: radio::SAMPLE_HZ,
            iq_amp: 0.8,
            iq_snr: None,
            iq_seed: 1,
        })
    }

    #[test]
    fn output_is_byte_identical() {
        let path = write_tmp("same.toml", &sample());
        let a = generate_scenario(&path).expect("a").0;
        let b = generate_scenario(&path).expect("b").0;
        assert_eq!(a, b);
        assert!(a.starts_with("0.000,"));
        let _ = fs::remove_file(path);
    }

    #[test]
    fn trace_skips_ground_and_missing() {
        let body = r#"{
            "icao":"abcdef",
            "timestamp":1000.0,
            "trace":[
                [0.0,-23.43,-46.47,"ground",120.0,90.0,0,null,{"flight":"TAM1234 ","squawk":"7700"}],
                [1.0,null,-46.47,35000,400.0,90.0,0,null,null],
                [2.0,-23.43,-46.47,35000,400.0,90.0,8,null,null],
                [3.0,-23.43,-46.47,35000,400.0,90.0,0,null,null],
                [4.0,-23.43,-46.47,35000,null,null,0,null,null]
            ]
        }"#;
        let path = write_tmp("trace.json", body);
        let tracks = load_traces(std::slice::from_ref(&path), None, None).expect("traces");
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].samples.len(), 2);
        assert_eq!(tracks[0].samples[0].alt_ft, 35000);
        assert_eq!(tracks[0].samples[0].callsign.as_deref(), Some("TAM1234"));
        assert_eq!(tracks[0].samples[0].squawk, Some(0x7700));
        assert_eq!(tracks[0].samples[1].speed_kt, None);
        let window =
            load_traces(std::slice::from_ref(&path), Some(1003.5), Some(1005.0)).expect("window");
        assert_eq!(window[0].samples.len(), 1);
        assert_eq!(window[0].samples[0].t, 1004.0);
        let _ = fs::remove_file(path);
    }
}

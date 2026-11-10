//! Append-only CSV of accepted positions, independent of the UART.
//!
//! One row per [`Fix`] that is enqueued for the UART. The timestamp is the Pi
//! clock when the row is written. Contest reception time is ground arrival.
//!
//! The file is created with a header if empty. Restarts append. At
//! [`MAX_TABLE_BYTES`] the current file is renamed `*.old` and a new file
//! starts, so two files are the disk cap.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::frame::{CALLSIGN_LEN, Callsign, Fix};

/// Rotate once the CSV reaches this size.
///
/// 8 MiB holds on the order of 10^5 positions. The live file plus `*.old`
/// stay under 16 MiB on the Pi SD card.
pub const MAX_TABLE_BYTES: u64 = 8 * 1024 * 1024;

const HEADER: &str = "icao,callsign,lat,lon,altitude_ft,ground_speed_kt,track_deg,timestamp\n";

/// On-disk table of accepted positions.
#[derive(Debug)]
pub struct Table {
    path: PathBuf,
    max_bytes: u64,
}

impl Table {
    /// Open `path`, creating it with a header when the file is empty.
    ///
    /// # Errors
    /// Returns an I/O error when the file cannot be created or written.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::open_capped(path, MAX_TABLE_BYTES)
    }

    /// Open `path` and rotate once the file reaches `max_bytes`.
    ///
    /// # Errors
    /// Returns an I/O error when the file cannot be created or written.
    pub fn open_capped(path: impl AsRef<Path>, max_bytes: u64) -> io::Result<Self> {
        let table = Self {
            path: path.as_ref().to_path_buf(),
            max_bytes,
        };
        table.ensure_header()?;
        Ok(table)
    }

    /// Append one row for `fix`. The timestamp is `SystemTime::now()`.
    ///
    /// # Errors
    /// Returns an I/O error when the row cannot be written or the file cannot
    /// be rotated.
    pub fn append(&self, fix: &Fix) -> io::Result<()> {
        if self.size()? >= self.max_bytes {
            self.rotate()?;
        }
        let row = format_row(fix, SystemTime::now());
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(row.as_bytes())?;
        file.flush()?;
        Ok(())
    }

    fn ensure_header(&self) -> io::Result<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        if file.metadata()?.len() == 0 {
            file.write_all(HEADER.as_bytes())?;
            file.flush()?;
        }
        Ok(())
    }

    fn size(&self) -> io::Result<u64> {
        match fs::metadata(&self.path) {
            Ok(meta) => Ok(meta.len()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(0),
            Err(err) => Err(err),
        }
    }

    fn rotate(&self) -> io::Result<()> {
        let old = rotated_path(&self.path);
        match fs::rename(&self.path, &old) {
            Ok(()) => {}
            Err(err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err),
        }
        self.ensure_header()
    }
}

fn rotated_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".old");
    PathBuf::from(name)
}

/// One CSV row, including the trailing newline.
///
/// `recorded_at` is Pi time for this record. It is not `fix.unix_s`.
pub fn format_row(fix: &Fix, recorded_at: SystemTime) -> String {
    let aircraft = &fix.aircraft;
    let icao = format!("{:06x}", aircraft.icao);
    let callsign = csv_field(&callsign_text(aircraft.callsign));
    let lat = format_e7(aircraft.latitude_e7);
    let lon = format_e7(aircraft.longitude_e7);
    let altitude = aircraft
        .altitude_ft
        .map(|feet| feet.to_string())
        .unwrap_or_default();
    let speed = aircraft
        .ground_speed_kt
        .map(|knots| knots.to_string())
        .unwrap_or_default();
    let track = aircraft
        .track_deg
        .map(|degrees| degrees.to_string())
        .unwrap_or_default();
    let timestamp = format_utc(recorded_at);
    format!("{icao},{callsign},{lat},{lon},{altitude},{speed},{track},{timestamp}\n")
}

fn callsign_text(callsign: Callsign) -> String {
    let bytes = callsign.as_bytes();
    let end = bytes
        .iter()
        .rposition(|&byte| byte != b' ')
        .map_or(0, |index| (index + 1).min(CALLSIGN_LEN));
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn csv_field(text: &str) -> String {
    if !text
        .bytes()
        .any(|byte| matches!(byte, b',' | b'"' | b'\n' | b'\r'))
    {
        return text.to_string();
    }
    let mut out = String::from("\"");
    for ch in text.chars() {
        if ch == '"' {
            out.push_str("\"\"");
        } else {
            out.push(ch);
        }
    }
    out.push('"');
    out
}

fn format_e7(e7: i32) -> String {
    let sign = if e7 < 0 { "-" } else { "" };
    let abs = u64::from(e7.unsigned_abs());
    let deg = abs / 10_000_000;
    let frac = abs % 10_000_000;
    format!("{sign}{deg}.{frac:07}")
}

fn format_utc(time: SystemTime) -> String {
    let secs = time
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let hour = rem / 3_600;
    let min = (rem % 3_600) / 60;
    let sec = rem % 60;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}:{min:02}:{sec:02}Z")
}

/// Civil date from days since 1970-01-01. Howard Hinnant `civil_from_days`.
fn civil_from_days(days: i64) -> (i32, u8, u8) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z.saturating_sub(era * 146_097) as u64;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let mut year = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    if month <= 2 {
        year += 1;
    }
    (year as i32, month as u8, day as u8)
}

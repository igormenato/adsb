//! Local CSV of each position queued for UART.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::frame::{Callsign, Fix};

/// Default file in the process working directory.
pub const DEFAULT_PATH: &str = "aircraft.csv";

const HEADER: &str = "icao,callsign,lat,lon,altitude_ft,ground_speed_kt,track_deg,timestamp\n";

/// Append-only aircraft table. One file for the whole process.
#[derive(Debug)]
pub struct Table {
    file: File,
}

impl Table {
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        Ok(Self {
            file: open_append(path.as_ref())?,
        })
    }

    /// One row for `fix`. Timestamp is the Pi clock, not contest reception.
    pub fn append(&mut self, fix: &Fix) -> io::Result<()> {
        self.append_at(fix, SystemTime::now())
    }

    fn append_at(&mut self, fix: &Fix, now: SystemTime) -> io::Result<()> {
        let row = format_row(fix, now);
        self.file.write_all(row.as_bytes())?;
        self.file.flush()?;
        Ok(())
    }
}

fn open_append(path: &Path) -> io::Result<File> {
    let mut file = OpenOptions::new().create(true).append(true).open(path)?;
    if file.metadata()?.len() == 0 {
        file.write_all(HEADER.as_bytes())?;
        file.flush()?;
    }
    Ok(file)
}

fn format_row(fix: &Fix, now: SystemTime) -> String {
    let aircraft = &fix.aircraft;
    let lat = f64::from(aircraft.latitude_e7) / 10_000_000.0;
    let lon = f64::from(aircraft.longitude_e7) / 10_000_000.0;
    let timestamp = now
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!(
        "{:06x},{},{:.7},{:.7},{},{},{},{}\n",
        aircraft.icao,
        callsign_text(aircraft.callsign),
        lat,
        lon,
        display_opt(aircraft.altitude_ft),
        display_opt(aircraft.ground_speed_kt),
        display_opt(aircraft.track_deg),
        timestamp,
    )
}

fn callsign_text(callsign: Callsign) -> String {
    let bytes = callsign.as_bytes();
    let n = bytes.iter().rposition(|&b| b != b' ').map_or(0, |i| i + 1);
    String::from_utf8_lossy(&bytes[..n]).into_owned()
}

fn display_opt<T: ToString>(value: Option<T>) -> String {
    value.map(|v| v.to_string()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    use crate::frame::{Aircraft, Icao};

    fn sample() -> Fix {
        Fix {
            aircraft: Aircraft {
                icao: Icao::from_u24(0x400ABC).expect("icao"),
                callsign: Callsign::pad("SIM0001"),
                latitude_e7: 105_000_000,
                longitude_e7: -202_500_000,
                altitude_ft: Some(12_000),
                ground_speed_kt: Some(280),
                track_deg: Some(90),
                emergency: None,
            },
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("trck-table-{}-{}", std::process::id(), name));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("dir");
        dir.join("aircraft.csv")
    }

    #[test]
    fn row_uses_pi_clock_not_message_time() {
        let fix = sample();
        let now = UNIX_EPOCH + std::time::Duration::from_secs(1_762_800_000);
        let row = format_row(&fix, now);
        assert_eq!(
            row,
            "400abc,SIM0001,10.5000000,-20.2500000,12000,280,90,1762800000\n"
        );
    }

    #[test]
    fn unknown_fields_stay_empty() {
        let mut fix = sample();
        fix.aircraft.altitude_ft = None;
        fix.aircraft.ground_speed_kt = None;
        fix.aircraft.track_deg = None;
        fix.aircraft.callsign = Callsign::default();
        let row = format_row(&fix, UNIX_EPOCH);
        assert_eq!(row, "400abc,,10.5000000,-20.2500000,,,,0\n");
    }

    #[test]
    fn append_keeps_every_row_in_one_file() {
        let path = scratch("keep");
        let mut table = Table::open(&path).expect("open");
        let fix = sample();
        table.append_at(&fix, UNIX_EPOCH).expect("first");
        table.append_at(&fix, UNIX_EPOCH).expect("second");
        table.append_at(&fix, UNIX_EPOCH).expect("third");
        let live = fs::read_to_string(&path).expect("live");
        assert!(live.starts_with(HEADER), "{live}");
        assert_eq!(live.lines().filter(|line| !line.is_empty()).count(), 4);
        assert!(!path.with_file_name("aircraft.csv.1").exists());
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }
}

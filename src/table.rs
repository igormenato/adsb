//! Local CSV of each position queued for UART.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::frame::{Callsign, Fix};

/// Default file in the process working directory.
pub const DEFAULT_PATH: &str = "aircraft.csv";

#[derive(Serialize)]
struct Row {
    icao: String,
    callsign: String,
    lat: String,
    lon: String,
    altitude_ft: Option<i32>,
    ground_speed_kt: Option<u16>,
    track_deg: Option<u16>,
    timestamp: u64,
}

/// Append-only aircraft table. One file for the whole process.
#[derive(Debug)]
pub struct Table {
    wtr: csv::Writer<File>,
}

impl Table {
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path.as_ref())?;
        let headers = file.metadata()?.len() == 0;
        Ok(Self {
            wtr: writer(file, headers),
        })
    }

    /// One row for `fix`. Timestamp is the Pi clock, not contest reception.
    pub fn append(&mut self, fix: &Fix) -> io::Result<()> {
        self.append_at(fix, SystemTime::now())
    }

    pub fn append_at(&mut self, fix: &Fix, now: SystemTime) -> io::Result<()> {
        self.wtr.serialize(row(fix, now))?;
        self.wtr.flush()?;
        Ok(())
    }
}

fn writer<W: Write>(inner: W, headers: bool) -> csv::Writer<W> {
    csv::WriterBuilder::new()
        .has_headers(headers)
        .terminator(csv::Terminator::Any(b'\n'))
        .from_writer(inner)
}

fn row(fix: &Fix, now: SystemTime) -> Row {
    let aircraft = &fix.aircraft;
    Row {
        icao: format!("{:06x}", aircraft.icao),
        callsign: callsign_text(aircraft.callsign),
        lat: format!("{:.7}", f64::from(aircraft.latitude_e7) / 10_000_000.0),
        lon: format!("{:.7}", f64::from(aircraft.longitude_e7) / 10_000_000.0),
        altitude_ft: aircraft.altitude_ft,
        ground_speed_kt: aircraft.ground_speed_kt,
        track_deg: aircraft.track_deg,
        timestamp: now
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    }
}

fn callsign_text(callsign: Callsign) -> String {
    let bytes = callsign.as_bytes();
    let n = bytes.iter().rposition(|&b| b != b' ').map_or(0, |i| i + 1);
    String::from_utf8_lossy(&bytes[..n]).into_owned()
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

    fn format_row(fix: &Fix, now: SystemTime) -> String {
        let mut wtr = writer(Vec::new(), false);
        wtr.serialize(row(fix, now)).expect("csv");
        String::from_utf8(wtr.into_inner().expect("buf")).expect("utf8")
    }

    #[test]
    fn row_uses_pi_clock_not_message_time() {
        let now = UNIX_EPOCH + std::time::Duration::from_secs(1_762_800_000);
        assert_eq!(
            format_row(&sample(), now),
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
        assert_eq!(
            format_row(&fix, UNIX_EPOCH),
            "400abc,,10.5000000,-20.2500000,,,,0\n"
        );
    }

    #[test]
    fn callsign_with_comma_round_trips() {
        let mut fix = sample();
        fix.aircraft.callsign = Callsign::pad("AB,CD");
        let row = format_row(&fix, UNIX_EPOCH);
        let mut reader = csv::ReaderBuilder::new()
            .has_headers(false)
            .from_reader(row.as_bytes());
        let record = reader.records().next().expect("row").expect("csv");
        assert_eq!(record.get(1), Some("AB,CD"));
        assert_eq!(
            row,
            "400abc,\"AB,CD\",10.5000000,-20.2500000,12000,280,90,0\n"
        );
    }

    #[test]
    fn append_keeps_every_row_in_one_file() {
        let path = scratch("keep");
        let mut table = Table::open(&path).expect("open");
        let fix = sample();
        table.append_at(&fix, UNIX_EPOCH).expect("first");
        table.append_at(&fix, UNIX_EPOCH).expect("second");
        table.append_at(&fix, UNIX_EPOCH).expect("third");
        drop(table);
        let live = fs::read_to_string(&path).expect("live");
        assert_eq!(live.lines().filter(|line| !line.is_empty()).count(), 4);
        assert!(!path.with_file_name("aircraft.csv.1").exists());
        let _ = fs::remove_dir_all(path.parent().expect("parent"));
    }
}

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, UNIX_EPOCH};

use trck::{Aircraft, Callsign, Fix, Icao, Table, check_positions, format_row};

static SEQ: AtomicU64 = AtomicU64::new(0);

fn temp_path(suffix: &str) -> PathBuf {
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("trck-table-{}-{n}{suffix}", std::process::id()))
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let path = temp_path(".csv");
        Self(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }

    fn old(&self) -> PathBuf {
        let mut name = self.0.clone().into_os_string();
        name.push(".old");
        PathBuf::from(name)
    }

    fn read(&self) -> String {
        fs::read_to_string(&self.0).expect("table")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
        let mut old = self.0.clone().into_os_string();
        old.push(".old");
        let _ = fs::remove_file(old);
    }
}

fn icao(n: u32) -> Icao {
    Icao::from_u24(n).expect("24-bit")
}

fn position(addr: u32) -> Fix {
    Fix {
        unix_s: 1_700_000_000,
        aircraft: Aircraft {
            icao: icao(addr),
            callsign: Callsign::pad(""),
            latitude_e7: 0,
            longitude_e7: 0,
            altitude_ft: None,
            ground_speed_kt: None,
            track_deg: None,
            emergency: None,
        },
    }
}

#[test]
fn a_row_matches_the_submit_columns_and_uses_pi_time() {
    let recorded = UNIX_EPOCH + Duration::from_secs(1_000_000_000);
    let row = format_row(&check_positions()[0], recorded);
    assert_eq!(
        row,
        "400abc,SIM0001,10.5000000,-20.2500000,12000,280,90,2001-09-09T01:46:40Z\n"
    );

    let unknown = format_row(&position(0xabc123), recorded);
    assert_eq!(
        unknown,
        "abc123,,0.0000000,0.0000000,,,,2001-09-09T01:46:40Z\n"
    );
}

#[test]
fn a_quoted_callsign_stays_one_csv_field() {
    let recorded = UNIX_EPOCH + Duration::from_secs(1);
    let mut fix = position(1);
    fix.aircraft.callsign = Callsign::pad("A,\"B");
    let row = format_row(&fix, recorded);
    assert_eq!(
        row,
        "000001,\"A,\"\"B\",0.0000000,0.0000000,,,,1970-01-01T00:00:01Z\n"
    );
}

#[test]
fn utc_seconds_are_civil_dates() {
    let cases = [
        (0, "1970-01-01T00:00:00Z"),
        (1, "1970-01-01T00:00:01Z"),
        (1_000_000_000, "2001-09-09T01:46:40Z"),
        (1_700_000_000, "2023-11-14T22:13:20Z"),
    ];
    for (secs, stamp) in cases {
        let row = format_row(&position(1), UNIX_EPOCH + Duration::from_secs(secs));
        assert!(row.ends_with(&format!("{stamp}\n")), "{row}");
    }
}

#[test]
fn append_survives_reopen_and_does_not_rewrite() {
    let scratch = Scratch::new();
    let table = Table::open(scratch.path()).expect("open");
    table.append(&position(1)).expect("first");
    drop(table);

    let table = Table::open(scratch.path()).expect("reopen");
    table.append(&position(2)).expect("second");

    let text = scratch.read();
    let lines: Vec<_> = text.lines().collect();
    assert_eq!(
        lines[0],
        "icao,callsign,lat,lon,altitude_ft,ground_speed_kt,track_deg,timestamp"
    );
    assert!(lines[1].starts_with("000001,"), "{text}");
    assert!(lines[2].starts_with("000002,"), "{text}");
    assert_eq!(lines.len(), 3);
    assert!(!text.contains("1700000000"), "{text}");
}

#[test]
fn a_full_file_rotates_to_old_and_stays_capped() {
    let scratch = Scratch::new();
    let table = Table::open_capped(scratch.path(), 200).expect("open");
    for addr in 1..=12 {
        table.append(&position(addr)).expect("row");
    }

    let live = scratch.read();
    let old = fs::read_to_string(scratch.old()).expect("old");
    assert!(live.len() as u64 <= 280, "live {}", live.len());
    assert!(old.len() as u64 <= 280, "old {}", old.len());
    assert!(
        live.lines().next()
            == Some("icao,callsign,lat,lon,altitude_ft,ground_speed_kt,track_deg,timestamp"),
        "{live}"
    );
    assert!(live.lines().count() > 1, "{live}");
    assert!(old.lines().count() > 1, "{old}");
}

#[test]
fn system_time_now_is_not_the_line_unix() {
    let scratch = Scratch::new();
    let table = Table::open(scratch.path()).expect("open");
    table.append(&check_positions()[1]).expect("row");
    let text = scratch.read();
    let stamp = text
        .lines()
        .nth(1)
        .expect("row")
        .rsplit(',')
        .next()
        .expect("ts");
    assert!(stamp.ends_with('Z'), "{stamp}");
    assert_ne!(stamp, "2023-11-14T22:13:20Z");
}

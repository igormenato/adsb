mod common;
use common::*;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use trck::{Aircraft, EmergencyKind, FRAME_LEN};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn run_replay(file: &Path, speed: &str, table: &Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_trck"))
        .args([
            "replay",
            file.to_str().expect("utf8"),
            "--speed",
            speed,
            "--uart",
            "-",
            "--table",
            table.to_str().expect("utf8"),
        ])
        .output()
        .expect("replay")
}

#[test]
fn replay_max_matches_golden() {
    let file = fixture("replay.csv");
    let table = table_arg();
    let out = run_replay(&file, "max", &table);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let expect_uart = fs::read(fixture("replay.uart")).expect("golden uart");
    assert_eq!(out.stdout, expect_uart);
    let got_csv = fs::read_to_string(&table).expect("table");
    let expect_csv = fs::read_to_string(fixture("replay.table.csv")).expect("golden csv");
    assert_eq!(got_csv, expect_csv);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("frames read=62"), "{stderr}");
    assert!(stderr.contains("decoded=61"), "{stderr}");
    assert!(stderr.contains("malformed=4"), "{stderr}");
    assert!(stderr.contains("fixes=14"), "{stderr}");
    assert!(stderr.contains("UART frames written=14"), "{stderr}");
    let mut by_icao: HashMap<u32, Vec<Aircraft>> = HashMap::new();
    for frame in out.stdout.as_chunks::<FRAME_LEN>().0 {
        let aircraft = frame_aircraft(frame);
        by_icao
            .entry(aircraft.icao.to_u24())
            .or_default()
            .push(aircraft);
    }
    assert_eq!(by_icao.len(), 3, "{:?}", by_icao.keys());
    for aircrafts in by_icao.values() {
        assert!(aircrafts.len() >= 2, "{aircrafts:?}");
    }
    let easy = &by_icao[&0x406B90];
    assert_eq!(easy[0].callsign.as_bytes(), *b"EZY85MH ");
    assert!(
        easy.windows(2)
            .any(|w| w[0].latitude_e7 != w[1].latitude_e7
                || w[0].longitude_e7 != w[1].longitude_e7),
        "{easy:?}"
    );
    assert!(
        by_icao[&0x40058B]
            .iter()
            .any(|a| a.emergency == Some(EmergencyKind::General)
                && a.callsign.as_bytes() == *b"TEST123 "),
        "{:?}",
        by_icao[&0x40058B]
    );
    let _ = fs::remove_file(&table);
}

#[test]
fn replay_speed_does_not_change_bytes() {
    let file = fixture("replay-speed.csv");
    let mut uart = Vec::new();
    let mut csv = String::new();
    for speed in ["1", "10", "max", "0"] {
        let table = table_arg();
        let out = run_replay(&file, speed, &table);
        assert!(
            out.status.success(),
            "{speed}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let got_csv = fs::read_to_string(&table).expect("table");
        let _ = fs::remove_file(&table);
        if uart.is_empty() {
            uart = out.stdout;
            csv = got_csv;
            continue;
        }
        assert_eq!(out.stdout, uart, "uart speed {speed}");
        assert_eq!(got_csv, csv, "csv speed {speed}");
    }
}

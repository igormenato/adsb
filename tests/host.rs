//! Host test: aircraft.json becomes one TRCK snapshot. No radio and no UART.

use trck::{
    crc16_ccitt_false, pack_alert, pack_alert_json, pack_snapshot, pack_snapshot_json,
    snapshot_from_aircraft_json, unpack_alert, unpack_snapshot, Alert, AlertTracker, EmergencyKind,
    SnapshotError, ALERT_LEN, ALERT_MAGIC, ALT_UNKNOWN, HEADING_UNKNOWN, POSITION_MAX_AGE_S,
    SPEED_UNKNOWN, TRACK_HEADER_LEN, TRACK_MAGIC, TRACK_MAX_AIRCRAFT, TRACK_RECORD_LEN,
};

fn json(body: &str) -> Vec<u8> {
    format!(r#"{{"now": 1700000000.9, "aircraft": [{body}]}}"#).into_bytes()
}

fn one(body: &str) -> trck::Snapshot {
    snapshot_from_aircraft_json(&json(body)).expect("fixture")
}

#[test]
fn published_track_and_crc() {
    assert_eq!(crc16_ccitt_false(b"123456789"), 0x29B1);
    assert_eq!(TRACK_MAGIC, b"TRCK");

    let snapshot = one(
        r#"{"hex":"40621d","flight":"ryr123","lat":52.2572021484375,"lon":3.91937255859375,"alt_baro":38000,"gs":450.2,"track":271.4,"seen_pos":1.2}"#,
    );
    assert_eq!(snapshot.unix_s, 1_700_000_000);
    assert_eq!(snapshot.aircraft.len(), 1);
    let aircraft = &snapshot.aircraft[0];
    assert_eq!(aircraft.icao, 0x40621D);
    assert_eq!(aircraft.latitude_e7, 522_572_021);
    assert_eq!(aircraft.longitude_e7, 39_193_726);
    assert_eq!(aircraft.altitude_ft, 38000);
    assert_eq!(aircraft.ground_speed_kt, 450);
    assert_eq!(aircraft.heading_deg, 271);

    let packet = pack_snapshot(&snapshot);
    assert_eq!(&packet[0..4], b"TRCK");
    assert_eq!(packet[4], 1);
    assert_eq!(packet.len(), TRACK_HEADER_LEN + TRACK_RECORD_LEN + 2);
    assert_eq!(unpack_snapshot(&packet).as_ref(), Some(&snapshot));

    let mut flipped = packet.clone();
    let last = flipped.len() - 1;
    flipped[last] ^= 0x5A;
    assert!(unpack_snapshot(&flipped).is_none());
}

#[test]
fn skips_aircraft_without_a_position() {
    let snapshot = one(r#"{"hex":"abc123","flight":"NOFIX"},
           {"hex":"~40621d","lat":10.5,"lon":-20.25,"alt_baro":"ground","seen_pos":1.5},
           {"hex":"000001","lat":1,"lon":2,"alt_geom":3280.4,"seen_pos":1}"#);
    assert_eq!(snapshot.aircraft.len(), 2);
    assert_eq!(snapshot.aircraft[0].icao, 1);
    assert_eq!(snapshot.aircraft[0].altitude_ft, 3280);
    assert_eq!(snapshot.aircraft[0].ground_speed_kt, SPEED_UNKNOWN);
    assert_eq!(snapshot.aircraft[0].heading_deg, HEADING_UNKNOWN);
    assert_eq!(snapshot.aircraft[1].icao, 0x40621D);
    assert_eq!(snapshot.aircraft[1].altitude_ft, 0);
    assert_eq!(snapshot.aircraft[1].latitude_e7, 105_000_000);
    assert_eq!(snapshot.aircraft[1].longitude_e7, -202_500_000);
}

#[test]
fn json_line_uses_the_struct_fields() {
    let snapshot = one(
        r#"{"hex":"40621d","flight":"ryr123","lat":52.2572021484375,"lon":3.91937255859375,"alt_baro":38000,"gs":450.2,"track":271.4,"seen_pos":1.2}"#,
    );
    assert_eq!(
        pack_snapshot_json(&snapshot),
        b"{\"unix_s\":1700000000,\"aircraft\":[{\"icao\":4219421,\"latitude_e7\":522572021,\"longitude_e7\":39193726,\"altitude_ft\":38000,\"ground_speed_kt\":450,\"heading_deg\":271}]}\n"
    );
    let empty = snapshot_from_aircraft_json(br#"{"now": 10, "aircraft": []}"#).unwrap();
    assert_eq!(
        pack_snapshot_json(&empty),
        b"{\"unix_s\":10,\"aircraft\":[]}\n"
    );

    let unknown = one(r#"{"hex":"abc123","lat":0,"lon":0,"seen_pos":0}"#);
    let line = pack_snapshot_json(&unknown);
    let value: serde_json::Value = serde_json::from_slice(&line).unwrap();
    assert_eq!(value["aircraft"][0]["altitude_ft"], i32::MIN);
    assert_eq!(value["aircraft"][0]["ground_speed_kt"], SPEED_UNKNOWN);
    assert_eq!(value["aircraft"][0]["heading_deg"], HEADING_UNKNOWN);
}

#[test]
fn empty_sky_is_a_valid_snapshot() {
    let snapshot = snapshot_from_aircraft_json(br#"{"now": 10, "aircraft": []}"#).unwrap();
    assert_eq!(snapshot.unix_s, 10);
    assert!(snapshot.aircraft.is_empty());
    let packet = pack_snapshot(&snapshot);
    assert_eq!(packet.len(), TRACK_HEADER_LEN + 2);
    assert_eq!(unpack_snapshot(&packet).unwrap().aircraft.len(), 0);
}

#[test]
fn keeps_the_sixty_four_freshest() {
    let mut entries = Vec::new();
    for index in 0..70 {
        entries.push(format!(
            r#"{{"hex":"{index:06x}","lat":1,"lon":2,"seen_pos":{}}}"#,
            index as f64 / 100.0
        ));
    }
    let snapshot = one(&entries.join(","));
    assert_eq!(snapshot.aircraft.len(), TRACK_MAX_AIRCRAFT);
    assert_eq!(snapshot.aircraft[0].icao, 0);
    assert_eq!(snapshot.aircraft[63].icao, 63);
}

#[test]
fn unknown_speed_heading_and_altitude() {
    let snapshot = one(
        r#"{"hex":"abc123","lat":0,"lon":0,"gs":-1,"track":-5,"seen_pos":0},
           {"hex":"abc124","lat":0,"lon":1,"gs":65534.6,"track":360,"seen_pos":0.1},
           {"hex":"abc125","lat":0,"lon":2,"alt_baro":"ground","seen_pos":0.2}"#,
    );
    assert_eq!(snapshot.aircraft[0].ground_speed_kt, SPEED_UNKNOWN);
    assert_eq!(snapshot.aircraft[0].heading_deg, HEADING_UNKNOWN);
    assert_eq!(snapshot.aircraft[0].altitude_ft, ALT_UNKNOWN);
    assert_eq!(snapshot.aircraft[1].ground_speed_kt, 65534);
    assert_eq!(snapshot.aircraft[1].heading_deg, 0);
    assert_eq!(snapshot.aircraft[2].altitude_ft, 0);
}

#[test]
fn drops_a_bad_hex() {
    let snapshot = one(r#"{"hex":"~40621d","lat":1,"lon":2,"seen_pos":0.1},
           {"hex":"40G621","lat":1,"lon":2,"seen_pos":0.2},
           {"hex":"abcdefg","lat":1,"lon":2,"seen_pos":0.3},
           {"hex":" abc","lat":1,"lon":2,"seen_pos":0.4},
           {"hex":"~~40621d","lat":1,"lon":2,"seen_pos":0.5}"#);
    assert_eq!(snapshot.aircraft.len(), 1);
    assert_eq!(snapshot.aircraft[0].icao, 0x40621D);
}

#[test]
fn drops_a_stale_position() {
    assert_eq!(POSITION_MAX_AGE_S, 2.0);
    let snapshot = one(r#"{"hex":"000001","lat":1,"lon":2,"seen_pos":2},
           {"hex":"000002","lat":1,"lon":2,"seen_pos":2.01},
           {"hex":"000003","lat":1,"lon":2},
           {"hex":"000004","lat":1,"lon":2,"seen_pos":0.4}"#);
    assert_eq!(snapshot.aircraft.len(), 2);
    assert_eq!(snapshot.aircraft[0].icao, 4);
    assert_eq!(snapshot.aircraft[1].icao, 1);
}

#[test]
fn rejects_a_bad_document() {
    assert_eq!(
        snapshot_from_aircraft_json(b"not json").unwrap_err(),
        SnapshotError::Json
    );
    assert_eq!(
        snapshot_from_aircraft_json(br#"{"aircraft":[]}"#).unwrap_err(),
        SnapshotError::MissingNow
    );
    assert_eq!(
        snapshot_from_aircraft_json(br#"{"now": -1, "aircraft":[]}"#).unwrap_err(),
        SnapshotError::BadNow
    );
    assert_eq!(
        snapshot_from_aircraft_json(br#"{"now": 1}"#).unwrap_err(),
        SnapshotError::Json
    );
}

fn scratch_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "trck-{name}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock")
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    dir
}

struct StopSender(std::process::Child);

impl Drop for StopSender {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn sender_bin() -> std::path::PathBuf {
    // The test executable is target/<profile>/deps/<test>. The sender sits beside deps.
    let mut path = std::env::current_exe().expect("test executable");
    path.pop();
    path.pop();
    path.push("trck");
    path
}

fn spawn_sender(json: &std::path::Path) -> StopSender {
    spawn_sender_with(json, &[])
}

fn spawn_sender_with(json: &std::path::Path, extra: &[&str]) -> StopSender {
    let mut args = vec![
        "--json",
        json.to_str().expect("utf-8 path"),
        "--uart",
        "-",
        "--interval",
        "0.2",
    ];
    args.extend_from_slice(extra);
    let child = std::process::Command::new(sender_bin())
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn sender");
    StopSender(child)
}

#[test]
fn sender_writes_one_snapshot_from_the_file() {
    let dir = scratch_dir("file");
    let path = dir.join("aircraft.json");
    std::fs::write(
        &path,
        r#"{"now":1700000000.9,"aircraft":[{"hex":"40621d","flight":"ryr123","lat":52.2572021484375,"lon":3.91937255859375,"alt_baro":38000,"gs":450.2,"track":271.4,"seen_pos":1.2},{"hex":"abc","flight":"NOFIX"}]}"#,
    )
    .unwrap();

    let mut sender = spawn_sender_with(&path, &["--format", "struct"]);
    let mut stdout = sender.0.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = Vec::new();
        let mut tmp = [0u8; 128];
        while buf.len() < TRACK_HEADER_LEN + TRACK_RECORD_LEN + 2 {
            match stdout.read(&mut tmp) {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.extend_from_slice(&tmp[..n]),
            }
        }
        let _ = tx.send(buf);
    });
    let buf = rx
        .recv_timeout(std::time::Duration::from_secs(3))
        .expect("sender stdout");
    let packet_len = TRACK_HEADER_LEN + TRACK_RECORD_LEN + 2;
    assert!(buf.len() >= packet_len, "short packet: {}", buf.len());
    let snapshot = unpack_snapshot(&buf[..packet_len]).expect("TRCK packet");
    assert_eq!(snapshot.unix_s, 1_700_000_000);
    assert_eq!(snapshot.aircraft.len(), 1);
    assert_eq!(snapshot.aircraft[0].icao, 0x40621D);
    assert_eq!(snapshot.aircraft[0].latitude_e7, 522_572_021);
    assert_eq!(snapshot.aircraft[0].longitude_e7, 39_193_726);
    assert_eq!(snapshot.aircraft[0].altitude_ft, 38000);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn sender_writes_one_json_line() {
    let dir = scratch_dir("json");
    let path = dir.join("aircraft.json");
    std::fs::write(
        &path,
        r#"{"now":1700000000.9,"aircraft":[{"hex":"40621d","flight":"ryr123","lat":52.2572021484375,"lon":3.91937255859375,"alt_baro":38000,"gs":450.2,"track":271.4,"seen_pos":1.2},{"hex":"abc","flight":"NOFIX"}]}"#,
    )
    .unwrap();

    let mut sender = spawn_sender(&path);
    let mut stdout = sender.0.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = Vec::new();
        let mut tmp = [0u8; 128];
        loop {
            match stdout.read(&mut tmp) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    buf.extend_from_slice(&tmp[..n]);
                    if buf.contains(&b'\n') {
                        break;
                    }
                }
            }
        }
        let _ = tx.send(buf);
    });
    let buf = rx
        .recv_timeout(std::time::Duration::from_secs(3))
        .expect("sender stdout");
    let line = buf.split(|byte| *byte == b'\n').next().unwrap();
    assert!(
        !line.windows(4).any(|window| window == b"TRCK"),
        "json mode wrote a binary packet"
    );
    let value: serde_json::Value = serde_json::from_slice(line).expect("json line");
    assert_eq!(value["unix_s"], 1_700_000_000);
    assert_eq!(value["aircraft"].as_array().unwrap().len(), 1);
    assert_eq!(value["aircraft"][0]["icao"], 0x40621D);
    assert_eq!(value["aircraft"][0]["latitude_e7"], 522_572_021);
    assert_eq!(value["aircraft"][0]["longitude_e7"], 39_193_726);
    assert_eq!(value["aircraft"][0]["altitude_ft"], 38000);
    assert_eq!(value["aircraft"][0]["ground_speed_kt"], 450);
    assert_eq!(value["aircraft"][0]["heading_deg"], 271);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn sample_flag_sends_two_aircraft_without_a_file() {
    let sample_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("sample/aircraft.json");
    let expect = snapshot_from_aircraft_json(&std::fs::read(&sample_path).unwrap()).unwrap();
    assert_eq!(expect.aircraft.len(), 2);
    assert_eq!(expect.unix_s, 1_700_000_000);
    assert_eq!(expect.aircraft[0].icao, 0x400ABC);
    assert_eq!(expect.aircraft[0].latitude_e7, 105_000_000);
    assert_eq!(expect.aircraft[0].longitude_e7, -202_500_000);
    assert_eq!(expect.aircraft[0].altitude_ft, 12000);
    assert_eq!(expect.aircraft[0].ground_speed_kt, 280);
    assert_eq!(expect.aircraft[0].heading_deg, 90);
    assert_eq!(expect.aircraft[1].icao, 0x40621D);

    let path = scratch_dir("sample").join("missing.json");
    let mut sender = spawn_sender_with(&path, &["--sample", "--format", "struct"]);
    let mut stdout = sender.0.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let packet_len = TRACK_HEADER_LEN + 2 * TRACK_RECORD_LEN + 2;
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = Vec::new();
        let mut tmp = [0u8; 128];
        while buf.len() < packet_len {
            match stdout.read(&mut tmp) {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.extend_from_slice(&tmp[..n]),
            }
        }
        let _ = tx.send(buf);
    });
    let buf = rx
        .recv_timeout(std::time::Duration::from_secs(3))
        .expect("sender stdout");
    assert!(buf.len() >= packet_len, "short packet: {}", buf.len());
    let snapshot = unpack_snapshot(&buf[..packet_len]).expect("TRCK packet");
    assert_eq!(snapshot, expect);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
}

#[test]
fn empty_sky_sends_no_packet() {
    let dir = scratch_dir("empty");
    let path = dir.join("aircraft.json");
    std::fs::write(&path, r#"{"now":10,"aircraft":[]}"#).unwrap();
    let mut sender = spawn_sender(&path);
    let mut stdout = sender.0.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut tmp = [0u8; 16];
        let n = stdout.read(&mut tmp).unwrap_or(0);
        let _ = tx.send(n);
    });
    match rx.recv_timeout(std::time::Duration::from_millis(700)) {
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            panic!("reader disconnected");
        }
        Ok(0) => panic!("sender exited before writing a packet"),
        Ok(n) => panic!("empty sky wrote {n} bytes"),
    }
    assert!(sender.0.try_wait().unwrap().is_none());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn missing_file_sends_no_packet() {
    let path = scratch_dir("missing").join("aircraft.json");
    let mut sender = spawn_sender(&path);
    let mut stdout = sender.0.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut tmp = [0u8; 16];
        let n = stdout.read(&mut tmp).unwrap_or(0);
        let _ = tx.send(n);
    });
    match rx.recv_timeout(std::time::Duration::from_millis(700)) {
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            panic!("reader disconnected");
        }
        Ok(0) => panic!("sender exited before writing a packet"),
        Ok(n) => panic!("missing aircraft.json wrote {n} bytes"),
    }
    assert!(sender.0.try_wait().unwrap().is_none());
}

fn doc(now: u32, body: &str) -> Vec<u8> {
    format!(r#"{{"now": {now}.0, "aircraft": [{body}]}}"#).into_bytes()
}

const NORMAL: &str = r#"{"hex":"400abc","lat":10.5,"lon":-20.25,"alt_baro":12000,"gs":280,"track":90,"seen_pos":0.4}"#;

fn emergency_body(extra: &str) -> String {
    format!(
        r#"{{"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,"alt_baro":38000,"gs":450.2,"track":271.4,"seen_pos":1.2,{extra}}}"#
    )
}

fn icao_in(snapshot: &trck::Snapshot, icao: u32) -> bool {
    snapshot
        .aircraft
        .iter()
        .any(|aircraft| aircraft.icao == icao)
}

#[test]
fn alert_opens_on_the_second_cycle_repeats_and_clears() {
    let mut tracker = AlertTracker::new();
    let emergency = emergency_body(r#""squawk":"7700""#);
    let both = format!("{NORMAL},{emergency}");

    let first = tracker.cycle(&doc(1_700_000_000, &both)).unwrap();
    assert!(first.alerts.is_empty());
    assert_eq!(
        first.snapshot,
        snapshot_from_aircraft_json(&doc(1_700_000_000, &both)).unwrap()
    );
    assert!(icao_in(&first.snapshot, 0x400ABC));
    assert!(icao_in(&first.snapshot, 0x40621D));
    let normal_only = snapshot_from_aircraft_json(&doc(1_700_000_000, NORMAL)).unwrap();
    let normal = first
        .snapshot
        .aircraft
        .iter()
        .find(|aircraft| aircraft.icao == 0x400ABC)
        .unwrap();
    assert_eq!(normal, &normal_only.aircraft[0]);

    // A bad read is not a cycle, so the next emergency still confirms.
    assert!(tracker.cycle(b"not json").is_err());

    let second = tracker.cycle(&doc(1_700_000_001, &both)).unwrap();
    assert_eq!(second.alerts.len(), 1);
    assert!(!second.alerts[0].cleared);
    assert_eq!(second.alerts[0].kind, EmergencyKind::General);
    assert_eq!(second.alerts[0].icao, 0x40621D);
    assert_eq!(second.alerts[0].unix_s, 1_700_000_001);
    assert_eq!(second.alerts[0].latitude_e7, 522_572_021);
    assert_eq!(second.alerts[0].longitude_e7, 39_193_726);
    assert_eq!(second.alerts[0].altitude_ft, 38000);
    assert!(icao_in(&second.snapshot, 0x400ABC));
    assert!(icao_in(&second.snapshot, 0x40621D));
    assert_eq!(
        pack_snapshot(&second.snapshot),
        pack_snapshot(&snapshot_from_aircraft_json(&doc(1_700_000_001, &both)).unwrap())
    );
    assert_eq!(&pack_snapshot(&second.snapshot)[..4], TRACK_MAGIC);
    assert!(pack_alert(&second.alerts[0]).starts_with(ALERT_MAGIC));

    let moved = format!(
        r#"{NORMAL},{{"hex":"40621d","lat":52.26,"lon":3.92,"alt_baro":38100,"gs":450,"track":271,"seen_pos":0.2,"squawk":"7700"}}"#
    );
    let third = tracker.cycle(&doc(1_700_000_002, &moved)).unwrap();
    assert_eq!(third.alerts.len(), 1);
    assert!(!third.alerts[0].cleared);
    assert_eq!(third.alerts[0].kind, EmergencyKind::General);
    assert_eq!(third.alerts[0].unix_s, 1_700_000_002);
    let moved_aircraft = third
        .snapshot
        .aircraft
        .iter()
        .find(|aircraft| aircraft.icao == 0x40621D)
        .unwrap();
    assert_eq!(third.alerts[0].latitude_e7, moved_aircraft.latitude_e7);
    assert_eq!(third.alerts[0].longitude_e7, moved_aircraft.longitude_e7);
    assert_eq!(third.alerts[0].altitude_ft, moved_aircraft.altitude_ft);
    assert_ne!(third.alerts[0].latitude_e7, 522_572_021);
    assert!(icao_in(&third.snapshot, 0x400ABC));

    let calm = format!(
        r#"{NORMAL},{{"hex":"40621d","lat":52.27,"lon":3.93,"alt_baro":38200,"gs":450,"track":270,"seen_pos":0.2}}"#
    );
    let fourth = tracker.cycle(&doc(1_700_000_003, &calm)).unwrap();
    assert_eq!(fourth.alerts.len(), 1);
    assert!(fourth.alerts[0].cleared);
    assert_eq!(fourth.alerts[0].kind, EmergencyKind::General);
    assert_eq!(fourth.alerts[0].icao, 0x40621D);
    assert_eq!(fourth.alerts[0].unix_s, 1_700_000_003);
    let calm_aircraft = fourth
        .snapshot
        .aircraft
        .iter()
        .find(|aircraft| aircraft.icao == 0x40621D)
        .unwrap();
    assert_eq!(fourth.alerts[0].latitude_e7, calm_aircraft.latitude_e7);
    assert_eq!(fourth.alerts[0].altitude_ft, calm_aircraft.altitude_ft);
    assert_ne!(fourth.alerts[0].latitude_e7, third.alerts[0].latitude_e7);
    assert!(icao_in(&fourth.snapshot, 0x400ABC));
    assert!(icao_in(&fourth.snapshot, 0x40621D));

    let fifth = tracker.cycle(&doc(1_700_000_004, &calm)).unwrap();
    assert!(fifth.alerts.is_empty());
    assert!(icao_in(&fifth.snapshot, 0x400ABC));
    assert!(icao_in(&fifth.snapshot, 0x40621D));

    assert!(tracker
        .cycle(&doc(1_700_000_005, &both))
        .unwrap()
        .alerts
        .is_empty());
    let reopened = tracker.cycle(&doc(1_700_000_006, &both)).unwrap();
    assert_eq!(reopened.alerts.len(), 1);
    assert!(!reopened.alerts[0].cleared);
}

#[test]
fn a_gap_does_not_confirm_an_emergency() {
    let mut tracker = AlertTracker::new();
    let both = format!("{},{}", NORMAL, emergency_body(r#""squawk":"7700""#));
    assert!(tracker.cycle(&doc(10, &both)).unwrap().alerts.is_empty());
    let quiet = tracker.cycle(&doc(11, NORMAL)).unwrap();
    assert!(quiet.alerts.is_empty());
    assert_eq!(quiet.snapshot.aircraft.len(), 1);
    assert_eq!(quiet.snapshot.aircraft[0].icao, 0x400ABC);
    assert!(tracker.cycle(&doc(12, &both)).unwrap().alerts.is_empty());
    let opened = tracker.cycle(&doc(13, &both)).unwrap();
    assert_eq!(opened.alerts.len(), 1);
    assert!(!opened.alerts[0].cleared);
    assert!(icao_in(&opened.snapshot, 0x400ABC));
}

#[test]
fn leaving_the_snapshot_clears_the_alert() {
    let mut tracker = AlertTracker::new();
    let both = format!("{},{}", NORMAL, emergency_body(r#""squawk":"7700""#));
    assert!(tracker.cycle(&doc(20, &both)).unwrap().alerts.is_empty());
    let open = tracker.cycle(&doc(21, &both)).unwrap();
    assert!(!open.alerts[0].cleared);
    let stale = format!(
        r#"{NORMAL},{{"hex":"40621d","lat":53.0,"lon":4.0,"alt_baro":1000,"seen_pos":5,"squawk":"7700"}}"#
    );
    let ended = tracker.cycle(&doc(22, &stale)).unwrap();
    assert_eq!(ended.alerts.len(), 1);
    assert!(ended.alerts[0].cleared);
    assert_eq!(ended.alerts[0].icao, 0x40621D);
    assert_eq!(ended.alerts[0].kind, EmergencyKind::General);
    assert_eq!(ended.alerts[0].latitude_e7, open.alerts[0].latitude_e7);
    assert_eq!(ended.alerts[0].longitude_e7, open.alerts[0].longitude_e7);
    assert_eq!(ended.alerts[0].altitude_ft, open.alerts[0].altitude_ft);
    assert_ne!(ended.alerts[0].altitude_ft, 1000);
    assert_eq!(ended.alerts[0].unix_s, 22);
    assert_eq!(ended.snapshot.aircraft.len(), 1);
    assert_eq!(ended.snapshot.aircraft[0].icao, 0x400ABC);
    assert!(tracker.cycle(&doc(23, &stale)).unwrap().alerts.is_empty());
}

#[test]
fn an_emergency_without_a_position_is_not_an_alert() {
    let body = r#"{"hex":"40621d","squawk":"7700","emergency":"general"},{"hex":"400abc","lat":10.5,"lon":-20.25,"alt_baro":12000,"seen_pos":0.4,"alert":1,"squawk":"1000","emergency":"none"}"#;
    let mut tracker = AlertTracker::new();
    for now in [30, 31, 32] {
        let cycle = tracker.cycle(&doc(now, body)).unwrap();
        assert!(cycle.alerts.is_empty());
        assert_eq!(cycle.snapshot.aircraft.len(), 1);
        assert_eq!(cycle.snapshot.aircraft[0].icao, 0x400ABC);
    }
}

#[test]
fn emergency_names_and_squawks() {
    let cases = [
        (r#""squawk":"7700""#, EmergencyKind::General),
        (r#""squawk":"7600""#, EmergencyKind::Nordo),
        (r#""squawk":"7500""#, EmergencyKind::Unlawful),
        (r#""squawk":7700"#, EmergencyKind::General),
        (r#""emergency":"general""#, EmergencyKind::General),
        (r#""emergency":"lifeguard""#, EmergencyKind::Lifeguard),
        (r#""emergency":"minfuel""#, EmergencyKind::MinFuel),
        (r#""emergency":"nordo""#, EmergencyKind::Nordo),
        (r#""emergency":"unlawful""#, EmergencyKind::Unlawful),
        (r#""emergency":"downed""#, EmergencyKind::Downed),
        (r#""emergency":"reserved""#, EmergencyKind::Reserved),
        (r#""emergency":"medical""#, EmergencyKind::Reserved),
        (
            r#""emergency":"unlawful","squawk":"7700""#,
            EmergencyKind::Unlawful,
        ),
        (
            r#""emergency":"none","squawk":"7700""#,
            EmergencyKind::General,
        ),
    ];
    for (extra, kind) in cases {
        let mut tracker = AlertTracker::new();
        let body = emergency_body(extra);
        assert!(
            tracker.cycle(&doc(1, &body)).unwrap().alerts.is_empty(),
            "{extra}"
        );
        let cycle = tracker.cycle(&doc(2, &body)).unwrap();
        assert_eq!(cycle.alerts.len(), 1, "{extra}");
        assert_eq!(cycle.alerts[0].kind, kind, "{extra}");
        assert!(!cycle.alerts[0].cleared, "{extra}");
        let line = pack_alert_json(&cycle.alerts[0]);
        let value: serde_json::Value = serde_json::from_slice(&line).unwrap();
        assert_eq!(value["alert"]["emergency"], kind.as_str(), "{extra}");
        assert_eq!(cycle.snapshot.aircraft.len(), 1, "{extra}");
        assert_eq!(cycle.snapshot.aircraft[0].icao, 0x40621D, "{extra}");
    }
}

#[test]
fn alert_line_and_packet_bytes() {
    let active = Alert {
        icao: 0x40621D,
        kind: EmergencyKind::General,
        latitude_e7: 522_572_021,
        longitude_e7: 39_193_726,
        altitude_ft: 38000,
        unix_s: 1_700_000_000,
        cleared: false,
    };
    assert_eq!(
        pack_alert_json(&active),
        b"{\"alert\":{\"icao\":4219421,\"emergency\":\"general\",\"latitude_e7\":522572021,\"longitude_e7\":39193726,\"altitude_ft\":38000,\"unix_s\":1700000000,\"cleared\":false}}\n"
    );
    let packet = [
        0x41, 0x4c, 0x52, 0x54, 0x01, 0x00, 0x01, 0x00, 0x1d, 0x62, 0x40, 0x00, 0xf5, 0xd0, 0x25,
        0x1f, 0x7e, 0x0c, 0x56, 0x02, 0x70, 0x94, 0x00, 0x00, 0x00, 0xf1, 0x53, 0x65, 0xc9, 0xdd,
    ];
    assert_eq!(packet.len(), ALERT_LEN);
    assert_eq!(&packet[..4], ALERT_MAGIC);
    assert_eq!(pack_alert(&active), packet);
    assert_eq!(unpack_alert(&packet).as_ref(), Some(&active));
    let mut flipped = packet;
    flipped[29] ^= 0x5A;
    assert!(unpack_alert(&flipped).is_none());

    let cleared = Alert {
        unix_s: 1_700_000_002,
        cleared: true,
        ..active
    };
    assert_eq!(
        pack_alert_json(&cleared),
        b"{\"alert\":{\"icao\":4219421,\"emergency\":\"general\",\"latitude_e7\":522572021,\"longitude_e7\":39193726,\"altitude_ft\":38000,\"unix_s\":1700000002,\"cleared\":true}}\n"
    );
    let cleared_packet = [
        0x41, 0x4c, 0x52, 0x54, 0x01, 0x01, 0x01, 0x00, 0x1d, 0x62, 0x40, 0x00, 0xf5, 0xd0, 0x25,
        0x1f, 0x7e, 0x0c, 0x56, 0x02, 0x70, 0x94, 0x00, 0x00, 0x02, 0xf1, 0x53, 0x65, 0xaa, 0x55,
    ];
    assert_eq!(pack_alert(&cleared), cleared_packet);
    assert_eq!(unpack_alert(&cleared_packet).as_ref(), Some(&cleared));
}

fn read_stdout(
    stdout: impl std::io::Read + Send + 'static,
    done: impl Fn(&[u8]) -> bool + Send + 'static,
) -> Vec<u8> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut stdout = stdout;
        let mut buf = Vec::new();
        let mut tmp = [0u8; 256];
        while !done(&buf) {
            match stdout.read(&mut tmp) {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.extend_from_slice(&tmp[..n]),
            }
        }
        let _ = tx.send(buf);
    });
    rx.recv_timeout(std::time::Duration::from_secs(3))
        .expect("sender stdout")
}

#[test]
fn sender_writes_the_alert_before_the_snapshot() {
    let dir = scratch_dir("alert-json");
    let path = dir.join("aircraft.json");
    std::fs::write(
        &path,
        r#"{"now":1700000000.9,"aircraft":[{"hex":"400abc","lat":10.5,"lon":-20.25,"alt_baro":12000,"gs":280,"track":90,"seen_pos":0.4},{"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,"alt_baro":38000,"gs":450.2,"track":271.4,"seen_pos":1.2,"squawk":"7700"}]}"#,
    )
    .unwrap();
    let mut sender = spawn_sender(&path);
    let stdout = sender.0.stdout.take().unwrap();
    let buf = read_stdout(stdout, |buf| {
        buf.iter().filter(|byte| **byte == b'\n').count() >= 3
    });
    let lines: Vec<_> = buf
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .collect();
    assert!(lines.len() >= 3, "short output: {}", buf.len());
    assert!(lines[0].starts_with(b"{\"unix_s\":"));
    assert!(lines[1].starts_with(b"{\"alert\":"));
    assert!(lines[2].starts_with(b"{\"unix_s\":"));
    assert_eq!(
        lines[1],
        b"{\"alert\":{\"icao\":4219421,\"emergency\":\"general\",\"latitude_e7\":522572021,\"longitude_e7\":39193726,\"altitude_ft\":38000,\"unix_s\":1700000000,\"cleared\":false}}"
    );
    for line in [lines[0], lines[2]] {
        let value: serde_json::Value = serde_json::from_slice(line).unwrap();
        let aircraft = value["aircraft"].as_array().unwrap();
        assert_eq!(aircraft.len(), 2);
        assert!(aircraft.iter().any(|entry| entry["icao"] == 0x400ABC));
        assert!(aircraft.iter().any(|entry| entry["icao"] == 0x40621D));
        assert!(value.get("alert").is_none());
    }
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn sender_writes_the_struct_alert_before_the_snapshot() {
    let dir = scratch_dir("alert-struct");
    let path = dir.join("aircraft.json");
    std::fs::write(
        &path,
        r#"{"now":1700000000.9,"aircraft":[{"hex":"400abc","lat":10.5,"lon":-20.25,"alt_baro":12000,"gs":280,"track":90,"seen_pos":0.4},{"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,"alt_baro":38000,"gs":450.2,"track":271.4,"seen_pos":1.2,"squawk":"7700"}]}"#,
    )
    .unwrap();
    let mut sender = spawn_sender_with(&path, &["--format", "struct"]);
    let stdout = sender.0.stdout.take().unwrap();
    let snapshot_len = TRACK_HEADER_LEN + 2 * TRACK_RECORD_LEN + 2;
    let total = snapshot_len + ALERT_LEN + snapshot_len;
    let buf = read_stdout(stdout, move |buf| buf.len() >= total);
    assert!(buf.len() >= total, "short output: {}", buf.len());
    assert_eq!(&buf[..4], TRACK_MAGIC);
    let first = unpack_snapshot(&buf[..snapshot_len]).expect("first snapshot");
    assert_eq!(first.aircraft.len(), 2);
    assert!(first
        .aircraft
        .iter()
        .any(|aircraft| aircraft.icao == 0x400ABC));
    assert!(first
        .aircraft
        .iter()
        .any(|aircraft| aircraft.icao == 0x40621D));
    let alert = unpack_alert(&buf[snapshot_len..snapshot_len + ALERT_LEN]).expect("alert");
    assert_eq!(alert.icao, 0x40621D);
    assert_eq!(alert.kind, EmergencyKind::General);
    assert!(!alert.cleared);
    assert_eq!(alert.unix_s, 1_700_000_000);
    let second =
        unpack_snapshot(&buf[snapshot_len + ALERT_LEN..snapshot_len + ALERT_LEN + snapshot_len])
            .expect("second snapshot");
    assert_eq!(second.aircraft.len(), 2);
    assert!(second
        .aircraft
        .iter()
        .any(|aircraft| aircraft.icao == 0x400ABC));
    let _ = std::fs::remove_dir_all(dir);
}

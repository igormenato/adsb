//! Host test: aircraft.json becomes one TRCK snapshot. No radio and no UART.

use adsb_uart_sender::{
    crc16_ccitt_false, pack_snapshot, snapshot_from_aircraft_json, unpack_snapshot, SnapshotError,
    ALT_UNKNOWN, HEADING_UNKNOWN, POSITION_MAX_AGE_S, SPEED_UNKNOWN, TRACK_HEADER_LEN, TRACK_MAGIC,
    TRACK_MAX_AIRCRAFT, TRACK_RECORD_LEN,
};

fn json(body: &str) -> Vec<u8> {
    format!(r#"{{"now": 1700000000.9, "aircraft": [{body}]}}"#).into_bytes()
}

fn one(body: &str) -> adsb_uart_sender::Snapshot {
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
    assert_eq!(&aircraft.callsign, b"RYR123  ");
    assert_eq!(aircraft.latitude_e7, 522_572_021);
    assert_eq!(aircraft.longitude_e7, 39_193_726);
    assert_eq!(aircraft.altitude_ft, 38000);
    assert_eq!(aircraft.ground_speed_kt, 450);
    assert_eq!(aircraft.heading_deg, 271);

    let packet = pack_snapshot(&snapshot);
    assert_eq!(&packet[0..4], b"TRCK");
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
    assert_eq!(&snapshot.aircraft[0].callsign, b"        ");
    assert_eq!(snapshot.aircraft[0].ground_speed_kt, SPEED_UNKNOWN);
    assert_eq!(snapshot.aircraft[0].heading_deg, HEADING_UNKNOWN);
    assert_eq!(snapshot.aircraft[1].icao, 0x40621D);
    assert_eq!(snapshot.aircraft[1].altitude_ft, 0);
    assert_eq!(snapshot.aircraft[1].latitude_e7, 105_000_000);
    assert_eq!(snapshot.aircraft[1].longitude_e7, -202_500_000);
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
        "adsb-uart-sender-{name}-{}-{}",
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
    path.push("adsb-uart-sender");
    path
}

fn spawn_sender(json: &std::path::Path) -> StopSender {
    let child = std::process::Command::new(sender_bin())
        .args([
            "--json",
            json.to_str().expect("utf-8 path"),
            "--uart",
            "-",
            "--interval",
            "0.2",
        ])
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

    let mut sender = spawn_sender(&path);
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

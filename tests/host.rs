//! Host test: aircraft.json becomes one TRCK snapshot. No radio and no UART.

use trck::{
    crc16_ccitt_false, pack_snapshot, pack_snapshot_json, snapshot_from_aircraft_json,
    unpack_snapshot, EmergencyKind, SnapshotError, ALT_UNKNOWN, HEADING_UNKNOWN,
    POSITION_MAX_AGE_S, SPEED_UNKNOWN, TRACK_HEADER_LEN, TRACK_MAGIC, TRACK_MAX_AIRCRAFT,
    TRACK_RECORD_LEN, TRACK_VERSION,
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
    assert_eq!(aircraft.emergency, None);

    let packet = pack_snapshot(&snapshot);
    assert_eq!(&packet[0..4], b"TRCK");
    assert_eq!(packet[4], TRACK_VERSION);
    assert_eq!(TRACK_VERSION, 2);
    assert_eq!(packet[TRACK_HEADER_LEN + 20], 0);
    assert_eq!(packet[TRACK_HEADER_LEN + 21], 0);
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
    assert_eq!(
        packet,
        [
            0x54, 0x52, 0x43, 0x4b, 0x02, 0x00, 0x00, 0x00, 0x0a, 0x00, 0x00, 0x00, 0xdc, 0xad,
        ]
    );
    assert_eq!(unpack_snapshot(&packet).as_ref(), Some(&snapshot));
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
    assert!(value["aircraft"][0].get("emergency").is_none());
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
fn empty_sky_sends_the_snapshot() {
    let dir = scratch_dir("empty");
    let path = dir.join("aircraft.json");
    std::fs::write(&path, r#"{"now":10,"aircraft":[]}"#).unwrap();
    let mut sender = spawn_sender_with(&path, &["--format", "struct"]);
    let stdout = sender.0.stdout.take().unwrap();
    let packet = [
        0x54, 0x52, 0x43, 0x4b, 0x02, 0x00, 0x00, 0x00, 0x0a, 0x00, 0x00, 0x00, 0xdc, 0xad,
    ];
    let buf = read_stdout(stdout, move |buf| buf.len() >= packet.len());
    assert!(
        buf.starts_with(&packet),
        "empty sky wrote {:02x?}",
        &buf[..buf.len().min(packet.len())]
    );
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

fn find<'a>(snapshot: &'a trck::Snapshot, icao: u32) -> &'a trck::Aircraft {
    snapshot
        .aircraft
        .iter()
        .find(|aircraft| aircraft.icao == icao)
        .expect("icao")
}

fn signed_packet(mut body: Vec<u8>) -> Vec<u8> {
    let crc = crc16_ccitt_false(&body);
    body.extend_from_slice(&crc.to_le_bytes());
    body
}

#[test]
fn emergency_is_on_the_aircraft_in_the_first_snapshot() {
    let both = format!("{},{}", NORMAL, emergency_body(r#""squawk":"7700""#));
    let snapshot = snapshot_from_aircraft_json(&doc(1_700_000_000, &both)).unwrap();
    let emergency = find(&snapshot, 0x40621D);
    assert_eq!(emergency.emergency, Some(EmergencyKind::General));
    assert_eq!(emergency.latitude_e7, 522_572_021);
    assert_eq!(emergency.longitude_e7, 39_193_726);
    assert_eq!(emergency.altitude_ft, 38000);
    assert_eq!(find(&snapshot, 0x400ABC).emergency, None);

    let line = pack_snapshot_json(&snapshot);
    assert!(!line.windows(8).any(|window| window == b"\"alert\":"));
    assert!(!line.windows(9).any(|window| window == b"\"cleared\""));
    let value: serde_json::Value = serde_json::from_slice(&line).unwrap();
    let aircraft = value["aircraft"].as_array().unwrap();
    assert_eq!(aircraft.len(), 2);
    assert!(aircraft[0].get("emergency").is_none());
    assert_eq!(aircraft[0]["icao"], 0x400ABC);
    assert_eq!(aircraft[1]["emergency"], "general");
    assert_eq!(aircraft[1]["icao"], 0x40621D);

    let packet = pack_snapshot(&snapshot);
    assert_eq!(
        packet,
        [
            0x54, 0x52, 0x43, 0x4b, 0x02, 0x00, 0x02, 0x00, 0x00, 0xf1, 0x53, 0x65, 0xbc, 0x0a,
            0x40, 0x00, 0x40, 0x2c, 0x42, 0x06, 0x60, 0x18, 0xee, 0xf3, 0xe0, 0x2e, 0x00, 0x00,
            0x18, 0x01, 0x5a, 0x00, 0x00, 0x00, 0x1d, 0x62, 0x40, 0x00, 0xf5, 0xd0, 0x25, 0x1f,
            0x7e, 0x0c, 0x56, 0x02, 0x70, 0x94, 0x00, 0x00, 0xc2, 0x01, 0x0f, 0x01, 0x01, 0x00,
            0x5d, 0xfd,
        ]
    );
    assert_eq!(&packet[..4], TRACK_MAGIC);
    assert_eq!(packet[4], TRACK_VERSION);
    assert_eq!(packet.len(), TRACK_HEADER_LEN + 2 * TRACK_RECORD_LEN + 2);
    assert_eq!(packet[TRACK_HEADER_LEN + 20], 0);
    assert_eq!(packet[TRACK_HEADER_LEN + 21], 0);
    let emergency_at = TRACK_HEADER_LEN + TRACK_RECORD_LEN;
    assert_eq!(packet[emergency_at + 20], EmergencyKind::General.code());
    assert_eq!(packet[emergency_at + 21], 0);
    assert_eq!(unpack_snapshot(&packet).as_ref(), Some(&snapshot));

    let mut flipped = packet.clone();
    flipped[emergency_at + 20] ^= 0x01;
    assert!(unpack_snapshot(&flipped).is_none());
}

#[test]
fn a_later_snapshot_omits_the_emergency() {
    let emergency = emergency_body(r#""squawk":"7700""#);
    let both = format!("{NORMAL},{emergency}");
    let first = snapshot_from_aircraft_json(&doc(1_700_000_000, &both)).unwrap();
    assert_eq!(
        find(&first, 0x40621D).emergency,
        Some(EmergencyKind::General)
    );

    let calm = format!(
        r#"{NORMAL},{{"hex":"40621d","lat":52.27,"lon":3.93,"alt_baro":38200,"gs":450,"track":270,"seen_pos":0.2}}"#
    );
    let second = snapshot_from_aircraft_json(&doc(1_700_000_001, &calm)).unwrap();
    assert_eq!(find(&second, 0x40621D).emergency, None);
    assert_eq!(find(&second, 0x400ABC).emergency, None);
    let line = pack_snapshot_json(&second);
    assert!(!line.windows(12).any(|window| window == b"\"emergency\""));
    assert!(!line.windows(9).any(|window| window == b"\"cleared\""));
    assert_eq!(pack_snapshot(&second)[4], TRACK_VERSION);
}

#[test]
fn emergency_names_and_squawks() {
    let cases = [
        (r#""squawk":"7700""#, Some(EmergencyKind::General)),
        (r#""squawk":"7600""#, Some(EmergencyKind::Nordo)),
        (r#""squawk":"7500""#, Some(EmergencyKind::Unlawful)),
        (r#""squawk":7700"#, Some(EmergencyKind::General)),
        (r#""squawk":7600"#, Some(EmergencyKind::Nordo)),
        (r#""squawk":7500"#, Some(EmergencyKind::Unlawful)),
        (r#""squawk":" 7700 ""#, Some(EmergencyKind::General)),
        (r#""emergency":"general""#, Some(EmergencyKind::General)),
        (r#""emergency":"lifeguard""#, Some(EmergencyKind::Lifeguard)),
        (r#""emergency":"minfuel""#, Some(EmergencyKind::MinFuel)),
        (r#""emergency":"nordo""#, Some(EmergencyKind::Nordo)),
        (r#""emergency":"unlawful""#, Some(EmergencyKind::Unlawful)),
        (r#""emergency":"downed""#, Some(EmergencyKind::Downed)),
        (r#""emergency":"GENERAL""#, Some(EmergencyKind::General)),
        (
            r#""emergency":"unlawful","squawk":"7700""#,
            Some(EmergencyKind::Unlawful),
        ),
        (
            r#""emergency":"none","squawk":"7700""#,
            Some(EmergencyKind::General),
        ),
        (
            r#""emergency":"","squawk":"7600""#,
            Some(EmergencyKind::Nordo),
        ),
        (r#""emergency":"none""#, None),
        (r#""squawk":"1000""#, None),
        (r#""emergency":"medical""#, None),
        (r#""emergency":"medical","squawk":"7700""#, None),
        (r#""emergency":"reserved""#, None),
    ];
    assert_eq!(
        [
            EmergencyKind::General.code(),
            EmergencyKind::Unlawful.code(),
            EmergencyKind::Nordo.code(),
            EmergencyKind::Lifeguard.code(),
            EmergencyKind::MinFuel.code(),
            EmergencyKind::Downed.code(),
        ],
        [1, 2, 3, 4, 5, 6]
    );
    for (extra, kind) in cases {
        let snapshot =
            snapshot_from_aircraft_json(&doc(1_700_000_000, &emergency_body(extra))).unwrap();
        assert_eq!(snapshot.aircraft.len(), 1, "{extra}");
        assert_eq!(snapshot.aircraft[0].icao, 0x40621D, "{extra}");
        assert_eq!(snapshot.aircraft[0].emergency, kind, "{extra}");
        let line = pack_snapshot_json(&snapshot);
        let value: serde_json::Value = serde_json::from_slice(&line).unwrap();
        match kind {
            Some(kind) => {
                assert_eq!(value["aircraft"][0]["emergency"], kind.as_str(), "{extra}");
                assert_eq!(
                    pack_snapshot(&snapshot)[TRACK_HEADER_LEN + 20],
                    kind.code(),
                    "{extra}"
                );
            }
            None => {
                assert!(value["aircraft"][0].get("emergency").is_none(), "{extra}");
                assert_eq!(
                    pack_snapshot(&snapshot)[TRACK_HEADER_LEN + 20],
                    0,
                    "{extra}"
                );
            }
        }
        assert!(
            !line.windows(8).any(|window| window == b"\"alert\":"),
            "{extra}"
        );
    }
}

#[test]
fn emergency_json_and_record_bytes() {
    let snapshot = one(
        r#"{"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,"alt_baro":38000,"gs":450.2,"track":271.4,"seen_pos":1.2,"emergency":"general"}"#,
    );
    assert_eq!(
        pack_snapshot_json(&snapshot),
        b"{\"unix_s\":1700000000,\"aircraft\":[{\"icao\":4219421,\"latitude_e7\":522572021,\"longitude_e7\":39193726,\"altitude_ft\":38000,\"ground_speed_kt\":450,\"heading_deg\":271,\"emergency\":\"general\"}]}\n"
    );
    let packet = [
        0x54, 0x52, 0x43, 0x4b, 0x02, 0x00, 0x01, 0x00, 0x00, 0xf1, 0x53, 0x65, 0x1d, 0x62, 0x40,
        0x00, 0xf5, 0xd0, 0x25, 0x1f, 0x7e, 0x0c, 0x56, 0x02, 0x70, 0x94, 0x00, 0x00, 0xc2, 0x01,
        0x0f, 0x01, 0x01, 0x00, 0x1e, 0x74,
    ];
    assert_eq!(packet.len(), TRACK_HEADER_LEN + TRACK_RECORD_LEN + 2);
    assert_eq!(pack_snapshot(&snapshot), packet);
    assert_eq!(unpack_snapshot(&packet).as_ref(), Some(&snapshot));

    let mut version_one = packet[..packet.len() - 2].to_vec();
    version_one[4] = 1;
    assert!(unpack_snapshot(&signed_packet(version_one)).is_none());

    let mut reserved = packet[..packet.len() - 2].to_vec();
    reserved[TRACK_HEADER_LEN + 21] = 1;
    assert!(unpack_snapshot(&signed_packet(reserved)).is_none());

    let mut unknown = packet[..packet.len() - 2].to_vec();
    unknown[TRACK_HEADER_LEN + 20] = 7;
    assert!(unpack_snapshot(&signed_packet(unknown)).is_none());
}

#[test]
fn an_emergency_without_a_fresh_position_is_left_out() {
    let body = r#"{"hex":"40621d","squawk":"7700","emergency":"general"},
        {"hex":"40621e","lat":53,"lon":4,"seen_pos":2.01,"squawk":"7600"},
        {"hex":"40621f","lat":53,"lon":4,"squawk":"7500"},
        {"hex":"400abc","lat":10.5,"lon":-20.25,"alt_baro":12000,"seen_pos":0.4,"alert":1,"squawk":"1000","emergency":"none"}"#;
    let snapshot = snapshot_from_aircraft_json(&doc(30, body)).unwrap();
    assert_eq!(snapshot.aircraft.len(), 1);
    assert_eq!(snapshot.aircraft[0].icao, 0x400ABC);
    assert_eq!(snapshot.aircraft[0].emergency, None);
    let line = pack_snapshot_json(&snapshot);
    assert!(
        serde_json::from_slice::<serde_json::Value>(&line).unwrap()["aircraft"][0]
            .get("emergency")
            .is_none()
    );

    let kept = snapshot_from_aircraft_json(&doc(
        31,
        r#"{"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,"alt_baro":38000,"seen_pos":2,"squawk":"7700"}"#,
    ))
    .unwrap();
    assert_eq!(kept.aircraft[0].emergency, Some(EmergencyKind::General));
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
fn sender_json_includes_the_emergency_on_the_first_line() {
    let dir = scratch_dir("emergency-json");
    let path = dir.join("aircraft.json");
    std::fs::write(
        &path,
        r#"{"now":1700000000.9,"aircraft":[{"hex":"400abc","lat":10.5,"lon":-20.25,"alt_baro":12000,"gs":280,"track":90,"seen_pos":0.4},{"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,"alt_baro":38000,"gs":450.2,"track":271.4,"seen_pos":1.2,"squawk":"7700"}]}"#,
    )
    .unwrap();
    let mut sender = spawn_sender(&path);
    let stdout = sender.0.stdout.take().unwrap();
    let buf = read_stdout(stdout, |buf| buf.contains(&b'\n'));
    let line = buf.split(|byte| *byte == b'\n').next().unwrap();
    assert!(!line.windows(8).any(|window| window == b"\"alert\":"));
    assert!(!line.windows(9).any(|window| window == b"\"cleared\""));
    let value: serde_json::Value = serde_json::from_slice(line).unwrap();
    let aircraft = value["aircraft"].as_array().unwrap();
    assert_eq!(aircraft.len(), 2);
    assert!(aircraft
        .iter()
        .any(|entry| entry["icao"] == 0x400ABC && entry.get("emergency").is_none()));
    assert!(aircraft
        .iter()
        .any(|entry| entry["icao"] == 0x40621D && entry["emergency"] == "general"));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn sender_struct_includes_the_emergency_on_the_first_packet() {
    let dir = scratch_dir("emergency-struct");
    let path = dir.join("aircraft.json");
    std::fs::write(
        &path,
        r#"{"now":1700000000.9,"aircraft":[{"hex":"400abc","lat":10.5,"lon":-20.25,"alt_baro":12000,"gs":280,"track":90,"seen_pos":0.4},{"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,"alt_baro":38000,"gs":450.2,"track":271.4,"seen_pos":1.2,"squawk":"7700"}]}"#,
    )
    .unwrap();
    let mut sender = spawn_sender_with(&path, &["--format", "struct"]);
    let stdout = sender.0.stdout.take().unwrap();
    let packet_len = TRACK_HEADER_LEN + 2 * TRACK_RECORD_LEN + 2;
    let buf = read_stdout(stdout, move |buf| buf.len() >= packet_len);
    assert!(buf.len() >= packet_len, "short output: {}", buf.len());
    assert!(!buf.windows(4).any(|window| window == b"ALRT"));
    assert_eq!(&buf[..4], TRACK_MAGIC);
    assert_eq!(buf[4], TRACK_VERSION);
    let snapshot = unpack_snapshot(&buf[..packet_len]).expect("TRCK packet");
    assert_eq!(snapshot.aircraft.len(), 2);
    assert_eq!(find(&snapshot, 0x400ABC).emergency, None);
    assert_eq!(
        find(&snapshot, 0x40621D).emergency,
        Some(EmergencyKind::General)
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn positionless_emergency_sends_an_empty_snapshot() {
    let dir = scratch_dir("positionless");
    let path = dir.join("aircraft.json");
    std::fs::write(
        &path,
        r#"{"now":10,"aircraft":[{"hex":"40621d","squawk":"7700","emergency":"general"},{"hex":"40621e","lat":53,"lon":4,"seen_pos":5,"squawk":"7600"}]}"#,
    )
    .unwrap();
    let mut sender = spawn_sender(&path);
    let stdout = sender.0.stdout.take().unwrap();
    let buf = read_stdout(stdout, |buf| buf.contains(&b'\n'));
    let line_end = buf
        .iter()
        .position(|byte| *byte == b'\n')
        .expect("json line");
    assert_eq!(&buf[..=line_end], b"{\"unix_s\":10,\"aircraft\":[]}\n");
    assert!(sender.0.try_wait().unwrap().is_none());
    let _ = std::fs::remove_dir_all(dir);
}

//! Host tests: aircraft.json for the file parser, JSON lines for the live port.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use trck::{
    crc16_ccitt_false, fix_from_json_line, pack_frame, snapshot_from_aircraft_json, unpack_frame,
    EmergencyKind, SnapshotError, ALT_UNKNOWN, FRAME_LEN, HEADING_UNKNOWN, SPEED_UNKNOWN,
    TRACK_MAGIC, TRACK_MAX_AIRCRAFT,
};

const PUBLISHED_LINE: &str = r#"{"now":1700000000.9,"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,"alt_baro":38000,"gs":450.2,"track":271.4}"#;

const SECOND_LINE: &str = r#"{"now":1700000000.9,"hex":"400abc","lat":10.5,"lon":-20.25,"alt_baro":12000,"gs":280,"track":90}"#;

const PUBLISHED: [u8; FRAME_LEN] = [
    0x54, 0x52, 0x43, 0x4b, 0x00, 0xf1, 0x53, 0x65, 0x1d, 0x62, 0x40, 0xf5, 0xd0, 0x25, 0x1f, 0x7e,
    0x0c, 0x56, 0x02, 0x70, 0x94, 0x00, 0x00, 0xc2, 0x01, 0x0f, 0x01, 0x00, 0x73, 0x81,
];

const EMERGENCY: [u8; FRAME_LEN] = [
    0x54, 0x52, 0x43, 0x4b, 0x00, 0xf1, 0x53, 0x65, 0x1d, 0x62, 0x40, 0xf5, 0xd0, 0x25, 0x1f, 0x7e,
    0x0c, 0x56, 0x02, 0x70, 0x94, 0x00, 0x00, 0xc2, 0x01, 0x0f, 0x01, 0x01, 0x52, 0x91,
];

const SECOND: [u8; FRAME_LEN] = [
    0x54, 0x52, 0x43, 0x4b, 0x00, 0xf1, 0x53, 0x65, 0xbc, 0x0a, 0x40, 0x40, 0x2c, 0x42, 0x06, 0x60,
    0x18, 0xee, 0xf3, 0xe0, 0x2e, 0x00, 0x00, 0x18, 0x01, 0x5a, 0x00, 0x00, 0xd7, 0x48,
];

const UNKNOWN: [u8; FRAME_LEN] = [
    0x54, 0x52, 0x43, 0x4b, 0x0a, 0x00, 0x00, 0x00, 0x23, 0xc1, 0xab, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0xff, 0xff, 0xff, 0xff, 0x00, 0x2a, 0x6a,
];

const SATURATED: [u8; FRAME_LEN] = [
    0x54, 0x52, 0x43, 0x4b, 0x0a, 0x00, 0x00, 0x00, 0x24, 0xc1, 0xab, 0x00, 0x00, 0x00, 0x00, 0x80,
    0x96, 0x98, 0x00, 0x00, 0x00, 0x00, 0x00, 0xfe, 0xff, 0x00, 0x00, 0x00, 0xa8, 0x0d,
];

fn json(body: &str) -> Vec<u8> {
    format!(r#"{{"now": 1700000000.9, "aircraft": [{body}]}}"#).into_bytes()
}

fn one(body: &str) -> trck::Snapshot {
    snapshot_from_aircraft_json(&json(body)).expect("fixture")
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

fn find(snapshot: &trck::Snapshot, icao: u32) -> &trck::Aircraft {
    snapshot
        .aircraft
        .iter()
        .find(|aircraft| aircraft.icao == icao)
        .expect("icao")
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

    let frame = pack_frame(&trck::Fix {
        unix_s: snapshot.unix_s,
        aircraft: aircraft.clone(),
    });
    assert_eq!(frame, PUBLISHED);
    assert_eq!(&frame[0..4], b"TRCK");
    assert_eq!(frame[27], 0);
    assert_eq!(
        unpack_frame(&frame).as_ref().map(|fix| &fix.aircraft),
        Some(aircraft)
    );
    assert_eq!(u16::from_le_bytes([frame[28], frame[29]]), 0x8173);

    let mut flipped = frame;
    flipped[29] ^= 0x5A;
    assert!(unpack_frame(&flipped).is_none());
}

#[test]
fn a_json_line_is_one_frame_and_a_document_is_not() {
    let fix = fix_from_json_line(PUBLISHED_LINE.as_bytes()).expect("line");
    assert_eq!(pack_frame(&fix), PUBLISHED);
    assert_eq!(
        snapshot_from_aircraft_json(PUBLISHED_LINE.as_bytes()).unwrap_err(),
        SnapshotError::Json
    );
    let document = json(
        r#"{"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,"alt_baro":38000,"gs":450.2,"track":271.4,"seen_pos":1.2}"#,
    );
    assert!(fix_from_json_line(&document).is_none());
}

#[test]
fn a_line_uses_now_and_ignores_seen_pos() {
    let fresh = fix_from_json_line(
        br#"{"now":1700000000.9,"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,"seen_pos":0}"#,
    )
    .expect("fresh");
    assert_eq!(fresh.unix_s, 1_700_000_000);
    assert_eq!(fresh.aircraft.icao, 0x40621D);
    assert_eq!(pack_frame(&fresh)[27], 0);
    assert_eq!(pack_frame(&fresh).len(), FRAME_LEN);

    let old = fix_from_json_line(
        br#"{"now":1700000000.9,"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,"seen_pos":100}"#,
    )
    .expect("old position still a line");
    assert_eq!(old.aircraft.icao, 0x40621D);
    assert_eq!(old.unix_s, 1_700_000_000);

    assert!(fix_from_json_line(br#"{"hex":"40621d","lat":1,"lon":2}"#).is_none());
    assert!(fix_from_json_line(br#"{"now":-1,"hex":"40621d","lat":1,"lon":2}"#).is_none());
    assert!(fix_from_json_line(br#"{"now":1700000000,"hex":"40621d","lon":2}"#).is_none());
    assert!(fix_from_json_line(br#"{"now":1700000000,"hex":"40621d","lat":1}"#).is_none());
    assert!(
        fix_from_json_line(br#"{"now":"1700000000","hex":"40621d","lat":1,"lon":2}"#).is_none()
    );
    assert!(fix_from_json_line(br#"{"now":1e20,"hex":"40621d","lat":1,"lon":2}"#).is_none());
    assert!(fix_from_json_line(b"not json").is_none());
    assert!(fix_from_json_line(br#"{"now":1,"aircraft":[]}"#).is_none());
}

#[test]
fn line_sentinels_and_heading_wrap() {
    let unknown =
        fix_from_json_line(br#"{"now":10,"hex":"abc123","lat":0,"lon":0,"gs":-1,"track":-5}"#)
            .expect("unknown");
    assert_eq!(unknown.aircraft.altitude_ft, ALT_UNKNOWN);
    assert_eq!(unknown.aircraft.ground_speed_kt, SPEED_UNKNOWN);
    assert_eq!(unknown.aircraft.heading_deg, HEADING_UNKNOWN);
    assert_eq!(pack_frame(&unknown), UNKNOWN);

    let saturated = fix_from_json_line(
        br#"{"now":10,"hex":"abc124","lat":0,"lon":1,"alt_baro":"ground","gs":65534.6,"track":360}"#,
    )
    .expect("saturated");
    assert_eq!(saturated.aircraft.altitude_ft, 0);
    assert_eq!(saturated.aircraft.ground_speed_kt, 65534);
    assert_eq!(saturated.aircraft.heading_deg, 0);
    assert_eq!(pack_frame(&saturated), SATURATED);
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
fn empty_sky_is_a_valid_snapshot() {
    let snapshot = snapshot_from_aircraft_json(br#"{"now": 10, "aircraft": []}"#).unwrap();
    assert_eq!(snapshot.unix_s, 10);
    assert!(snapshot.aircraft.is_empty());
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
fn unknown_speed_heading_and_altitude_in_the_file() {
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
    assert!(fix_from_json_line(br#"{"now":1,"hex":"40G621","lat":1,"lon":2}"#).is_none());
    assert!(fix_from_json_line(br#"{"now":1,"hex":"40621d","lat":91,"lon":2}"#).is_none());
}

#[test]
fn drops_a_stale_position_in_the_file() {
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

#[test]
fn emergency_is_on_the_aircraft() {
    let both = format!("{},{}", NORMAL, emergency_body(r#""squawk":"7700""#));
    let snapshot = snapshot_from_aircraft_json(&doc(1_700_000_000, &both)).unwrap();
    let emergency = find(&snapshot, 0x40621D);
    assert_eq!(emergency.emergency, Some(EmergencyKind::General));
    assert_eq!(emergency.latitude_e7, 522_572_021);
    assert_eq!(find(&snapshot, 0x400ABC).emergency, None);
    assert_eq!(
        pack_frame(&trck::Fix {
            unix_s: snapshot.unix_s,
            aircraft: emergency.clone(),
        }),
        EMERGENCY
    );
    assert_eq!(
        pack_frame(&trck::Fix {
            unix_s: snapshot.unix_s,
            aircraft: find(&snapshot, 0x400ABC).clone(),
        }),
        SECOND
    );
    assert_eq!(EMERGENCY[27], EmergencyKind::General.code());
    assert_eq!(SECOND[27], 0);

    let calm = format!(
        r#"{NORMAL},{{"hex":"40621d","lat":52.27,"lon":3.93,"alt_baro":38200,"gs":450,"track":270,"seen_pos":0.2}}"#
    );
    let second = snapshot_from_aircraft_json(&doc(1_700_000_001, &calm)).unwrap();
    assert_eq!(find(&second, 0x40621D).emergency, None);
    assert_eq!(
        pack_frame(&trck::Fix {
            unix_s: second.unix_s,
            aircraft: find(&second, 0x40621D).clone(),
        })[27],
        0
    );
}

#[test]
fn emergency_names_and_squawks_on_a_line() {
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
        let line = format!(
            r#"{{"now":1700000000.9,"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,{extra}}}"#
        );
        let fix = fix_from_json_line(line.as_bytes()).expect(extra);
        assert_eq!(fix.aircraft.icao, 0x40621D, "{extra}");
        assert_eq!(fix.aircraft.emergency, kind, "{extra}");
        let frame = pack_frame(&fix);
        assert_eq!(frame.len(), FRAME_LEN, "{extra}");
        match kind {
            Some(kind) => assert_eq!(frame[27], kind.code(), "{extra}"),
            None => assert_eq!(frame[27], 0, "{extra}"),
        }
        assert_eq!(
            unpack_frame(&frame).unwrap().aircraft.emergency,
            kind,
            "{extra}"
        );
    }

    let named = fix_from_json_line(
        br#"{"now":1700000000.9,"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,"alt_baro":38000,"gs":450.2,"track":271.4,"emergency":"general"}"#,
    )
    .expect("named");
    assert_eq!(pack_frame(&named), EMERGENCY);

    let squawk = fix_from_json_line(
        br#"{"now":1700000000.9,"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,"alt_baro":38000,"gs":450.2,"track":271.4,"squawk":"7700"}"#,
    )
    .expect("squawk");
    assert_eq!(pack_frame(&squawk), EMERGENCY);

    let mut unknown = EMERGENCY;
    unknown[27] = 7;
    let crc = crc16_ccitt_false(&unknown[..28]);
    unknown[28..30].copy_from_slice(&crc.to_le_bytes());
    assert!(unpack_frame(&unknown).is_none());
}

#[test]
fn an_emergency_without_a_position_is_left_out_of_the_file() {
    let body = r#"{"hex":"40621d","squawk":"7700","emergency":"general"},
        {"hex":"40621e","lat":53,"lon":4,"seen_pos":30.01,"squawk":"7600"},
        {"hex":"40621f","lat":53,"lon":4,"squawk":"7500"},
        {"hex":"000001","lat":1,"lon":2,"seen_pos":2.01},
        {"hex":"400abc","lat":10.5,"lon":-20.25,"alt_baro":12000,"seen_pos":0.4,"alert":1,"squawk":"1000","emergency":"none"}"#;
    let snapshot = snapshot_from_aircraft_json(&doc(30, body)).unwrap();
    assert_eq!(snapshot.aircraft.len(), 1);
    assert_eq!(snapshot.aircraft[0].icao, 0x400ABC);
    assert_eq!(snapshot.aircraft[0].emergency, None);
    assert!(fix_from_json_line(
        br#"{"now":30,"hex":"40621d","squawk":"7700","emergency":"general"}"#
    )
    .is_none());
}

#[test]
fn an_emergency_position_is_kept_for_thirty_seconds_in_the_file() {
    let kept = one(
        r#"{"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,"alt_baro":38000,"gs":450.2,"track":271.4,"seen_pos":30,"emergency":"general"}"#,
    );
    assert_eq!(kept.aircraft.len(), 1);
    assert_eq!(kept.aircraft[0].icao, 0x40621D);
    assert_eq!(kept.aircraft[0].emergency, Some(EmergencyKind::General));
    assert_eq!(
        pack_frame(&trck::Fix {
            unix_s: kept.unix_s,
            aircraft: kept.aircraft[0].clone(),
        }),
        EMERGENCY
    );

    let dropped = one(
        r#"{"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,"seen_pos":30.01,"emergency":"general"}"#,
    );
    assert!(dropped.aircraft.is_empty());

    let boundary = one(r#"{"hex":"000002","lat":1,"lon":2,"seen_pos":2,"squawk":"7700"}"#);
    assert_eq!(boundary.aircraft[0].emergency, Some(EmergencyKind::General));
}

#[test]
fn keeps_an_older_emergency_when_sixty_four_normals_are_fresh() {
    let mut entries = Vec::new();
    for index in 0..64 {
        entries.push(format!(
            r#"{{"hex":"{index:06x}","lat":1,"lon":2,"seen_pos":{}}}"#,
            index as f64 / 100.0
        ));
    }
    entries.push(r#"{"hex":"000100","lat":3,"lon":4,"seen_pos":10,"squawk":"7700"}"#.to_string());
    let snapshot = one(&entries.join(","));
    assert_eq!(snapshot.aircraft.len(), TRACK_MAX_AIRCRAFT);
    assert_eq!(snapshot.aircraft[0].icao, 0x100);
    assert_eq!(snapshot.aircraft[0].emergency, Some(EmergencyKind::General));
    assert!(snapshot.aircraft.iter().any(|aircraft| aircraft.icao == 0));
    assert!(snapshot.aircraft.iter().all(|aircraft| aircraft.icao != 63));
    assert_eq!(
        pack_frame(&trck::Fix {
            unix_s: snapshot.unix_s,
            aircraft: snapshot.aircraft[0].clone(),
        })[27],
        EmergencyKind::General.code()
    );
}

struct StopSender(std::process::Child);

impl Drop for StopSender {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_net(port: u16) -> StopSender {
    let addr = format!("127.0.0.1:{port}");
    let child = Command::new(env!("CARGO_BIN_EXE_trck"))
        .args(["--net-json", &addr, "--uart", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn sender");
    StopSender(child)
}

fn spawn_sample() -> StopSender {
    let child = Command::new(env!("CARGO_BIN_EXE_trck"))
        .args(["--sample", "--uart", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn sample");
    StopSender(child)
}

fn collect_stdout(stdout: impl Read + Send + 'static) -> Arc<Mutex<Vec<u8>>> {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let out = Arc::clone(&buf);
    std::thread::spawn(move || {
        let mut stdout = stdout;
        let mut tmp = [0u8; 256];
        loop {
            match stdout.read(&mut tmp) {
                Ok(0) | Err(_) => break,
                Ok(n) => out.lock().expect("lock").extend_from_slice(&tmp[..n]),
            }
        }
    });
    buf
}

fn wait_len(buf: &Mutex<Vec<u8>>, n: usize, timeout: Duration) -> Vec<u8> {
    let start = std::time::Instant::now();
    loop {
        let got = buf.lock().expect("lock").clone();
        if got.len() >= n || start.elapsed() > timeout {
            return got;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn listen() -> (TcpListener, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    (listener, port)
}

#[test]
fn nothing_is_written_until_a_line_arrives() {
    let (listener, port) = listen();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("accept");
        ready_tx.send(()).expect("ready");
        go_rx.recv().expect("go");
        writeln!(sock, "{PUBLISHED_LINE}").expect("line");
        std::thread::sleep(Duration::from_secs(2));
    });
    let mut sender = spawn_net(port);
    let buf = collect_stdout(sender.0.stdout.take().unwrap());
    ready_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("connected");
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        buf.lock().expect("lock").is_empty(),
        "bytes before a line: {:02x?}",
        buf.lock().expect("lock")
    );
    go_tx.send(()).expect("release");
    let got = wait_len(&buf, FRAME_LEN, Duration::from_secs(2));
    assert_eq!(got, PUBLISHED);
    assert!(sender.0.try_wait().unwrap().is_none());
}

#[test]
fn one_frame_per_valid_line_and_none_for_a_bad_line() {
    let (listener, port) = listen();
    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("accept");
        write!(
            sock,
            "not json\n{{\"now\":1}}\n{{\"now\":-1,\"hex\":\"abc123\",\"lat\":1,\"lon\":2}}\n{{\"hex\":\"abc123\",\"lat\":1,\"lon\":2}}\n{{\"now\":1.5,\"hex\":\"abc123\",\"lon\":2}}\n{{\"now\":\"1700000000\",\"hex\":\"abc123\",\"lat\":1,\"lon\":2}}\n{PUBLISHED_LINE}\n{{\"now\":1700000000,\"aircraft\":[]}}\n{SECOND_LINE}\n"
        )
        .expect("lines");
        std::thread::sleep(Duration::from_secs(2));
    });
    let mut sender = spawn_net(port);
    let buf = collect_stdout(sender.0.stdout.take().unwrap());
    let got = wait_len(&buf, FRAME_LEN * 2, Duration::from_secs(2));
    assert_eq!(got.len(), FRAME_LEN * 2, "got {}", got.len());
    assert_eq!(&got[..FRAME_LEN], &PUBLISHED);
    assert_eq!(&got[FRAME_LEN..], &SECOND);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(buf.lock().expect("lock").len(), FRAME_LEN * 2);
    assert!(sender.0.try_wait().unwrap().is_none());
}

#[test]
fn the_same_icao_is_sent_twice() {
    let (listener, port) = listen();
    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("accept");
        writeln!(sock, "{PUBLISHED_LINE}").expect("first");
        writeln!(sock, "{PUBLISHED_LINE}").expect("second");
        std::thread::sleep(Duration::from_secs(2));
    });
    let mut sender = spawn_net(port);
    let buf = collect_stdout(sender.0.stdout.take().unwrap());
    let got = wait_len(&buf, FRAME_LEN * 2, Duration::from_secs(2));
    assert_eq!(got, [PUBLISHED, PUBLISHED].concat());
}

#[test]
fn a_split_line_is_one_frame() {
    let (listener, port) = listen();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let line = PUBLISHED_LINE.as_bytes();
    let (head, tail) = line.split_at(line.len() / 2);
    let head = head.to_vec();
    let mut tail = tail.to_vec();
    tail.push(b'\n');
    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("accept");
        sock.write_all(&head).expect("head");
        ready_tx.send(()).expect("ready");
        go_rx.recv().expect("go");
        sock.write_all(&tail).expect("tail");
        std::thread::sleep(Duration::from_secs(2));
    });
    let mut sender = spawn_net(port);
    let buf = collect_stdout(sender.0.stdout.take().unwrap());
    ready_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("connected");
    std::thread::sleep(Duration::from_millis(150));
    assert!(
        buf.lock().expect("lock").is_empty(),
        "partial line wrote a frame"
    );
    go_tx.send(()).expect("release");
    let got = wait_len(&buf, FRAME_LEN, Duration::from_secs(2));
    assert_eq!(got, PUBLISHED);
}

#[test]
fn emergency_and_squawk_are_on_the_wire() {
    let (listener, port) = listen();
    let unlawful = r#"{"now":1700000000.9,"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,"alt_baro":38000,"gs":450.2,"track":271.4,"emergency":"unlawful","squawk":"7700"}"#;
    std::thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("accept");
        writeln!(
            sock,
            r#"{{"now":1700000000.9,"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,"alt_baro":38000,"gs":450.2,"track":271.4,"squawk":"7700"}}"#
        )
        .expect("squawk");
        writeln!(sock, "{unlawful}").expect("name");
        std::thread::sleep(Duration::from_secs(2));
    });
    let mut sender = spawn_net(port);
    let buf = collect_stdout(sender.0.stdout.take().unwrap());
    let got = wait_len(&buf, FRAME_LEN * 2, Duration::from_secs(2));
    assert_eq!(&got[..FRAME_LEN], &EMERGENCY);
    let second = unpack_frame(&got[FRAME_LEN..]).expect("crc");
    assert_eq!(second.aircraft.emergency, Some(EmergencyKind::Unlawful));
    assert_eq!(got[FRAME_LEN + 27], EmergencyKind::Unlawful.code());
    assert_ne!(&got[FRAME_LEN..], &EMERGENCY);
}

#[test]
fn a_dead_socket_writes_nothing_until_the_next_line() {
    let (listener, port) = listen();
    std::thread::spawn(move || {
        let (sock, _) = listener.accept().expect("first");
        drop(sock);
        let (mut sock, _) = listener.accept().expect("second");
        writeln!(sock, "{PUBLISHED_LINE}").expect("line");
        std::thread::sleep(Duration::from_secs(2));
    });
    let mut sender = spawn_net(port);
    let buf = collect_stdout(sender.0.stdout.take().unwrap());
    std::thread::sleep(Duration::from_millis(400));
    assert!(
        buf.lock().expect("lock").is_empty(),
        "dead socket wrote {:02x?}",
        buf.lock().expect("lock")
    );
    let got = wait_len(&buf, FRAME_LEN, Duration::from_secs(4));
    assert_eq!(got, PUBLISHED);
    assert!(sender.0.try_wait().unwrap().is_none());
}

#[test]
fn a_refused_port_writes_nothing() {
    let (listener, port) = listen();
    drop(listener);
    let mut sender = spawn_net(port);
    let buf = collect_stdout(sender.0.stdout.take().unwrap());
    std::thread::sleep(Duration::from_millis(700));
    assert!(buf.lock().expect("lock").is_empty());
    assert!(sender.0.try_wait().unwrap().is_none());
}

#[test]
fn sample_sends_one_frame_per_aircraft() {
    let sample_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("sample/aircraft.json");
    let expect = snapshot_from_aircraft_json(&std::fs::read(&sample_path).unwrap()).unwrap();
    assert_eq!(expect.aircraft.len(), 2);
    assert_eq!(expect.unix_s, 1_700_000_000);
    let frames = [
        pack_frame(&trck::Fix {
            unix_s: expect.unix_s,
            aircraft: expect.aircraft[0].clone(),
        }),
        pack_frame(&trck::Fix {
            unix_s: expect.unix_s,
            aircraft: expect.aircraft[1].clone(),
        }),
    ];
    assert_eq!(frames[0], SECOND);
    assert_eq!(frames[1], PUBLISHED);

    let mut sender = spawn_sample();
    let buf = collect_stdout(sender.0.stdout.take().unwrap());
    let got = wait_len(&buf, FRAME_LEN * 2, Duration::from_secs(2));
    assert!(got.len() >= FRAME_LEN * 2, "short output: {}", got.len());
    assert_eq!(&got[..FRAME_LEN * 2], &[frames[0], frames[1]].concat()[..]);
    assert!(sender.0.try_wait().unwrap().is_none());
}

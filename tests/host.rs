//! Host tests: JSON lines become UART bytes, and `--check` writes the published frames.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use trck::{ALT_UNKNOWN, FRAME_LEN, SPEED_UNKNOWN, TRACK_UNKNOWN, fix_from_json_line, pack_frame};

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

const CHECK_EMERGENCY: [u8; FRAME_LEN] = [
    0x54, 0x52, 0x43, 0x4b, 0x00, 0xf1, 0x53, 0x65, 0x23, 0xa1, 0x4c, 0xe0, 0xb2, 0xad, 0x1e, 0xe0,
    0x55, 0xbb, 0xff, 0x40, 0x1f, 0x00, 0x00, 0xdc, 0x00, 0xb4, 0x00, 0x01, 0x7f, 0xb6,
];

const UNKNOWN: [u8; FRAME_LEN] = [
    0x54, 0x52, 0x43, 0x4b, 0x0a, 0x00, 0x00, 0x00, 0x23, 0xc1, 0xab, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x80, 0xff, 0xff, 0xff, 0xff, 0x00, 0x2a, 0x6a,
];

const SATURATED: [u8; FRAME_LEN] = [
    0x54, 0x52, 0x43, 0x4b, 0x0a, 0x00, 0x00, 0x00, 0x24, 0xc1, 0xab, 0x00, 0x00, 0x00, 0x00, 0x80,
    0x96, 0x98, 0x00, 0x00, 0x00, 0x00, 0x00, 0xfe, 0xff, 0x00, 0x00, 0x00, 0xa8, 0x0d,
];

#[test]
fn published_json_is_the_documented_frame() {
    let fix = fix_from_json_line(PUBLISHED_LINE.as_bytes()).expect("line");
    assert_eq!(fix.unix_s, 1_700_000_000);
    assert_eq!(fix.aircraft.icao, 0x40621D);
    assert_eq!(fix.aircraft.latitude_e7, 522_572_021);
    assert_eq!(fix.aircraft.longitude_e7, 39_193_726);
    assert_eq!(fix.aircraft.altitude_ft, 38000);
    assert_eq!(fix.aircraft.ground_speed_kt, 450);
    assert_eq!(fix.aircraft.track_deg, 271);
    assert_eq!(pack_frame(&fix), PUBLISHED);
    assert_eq!(&PUBLISHED[..4], b"TRCK");
    assert_eq!(PUBLISHED[27], 0);
}

#[test]
fn a_json_line_is_one_frame_and_a_document_is_not() {
    let fix = fix_from_json_line(PUBLISHED_LINE.as_bytes()).expect("line");
    assert_eq!(pack_frame(&fix), PUBLISHED);
    assert!(
        fix_from_json_line(
            br#"{"now":1700000000.9,"aircraft":[{"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375}]}"#
        )
        .is_none()
    );
}

#[test]
fn a_line_uses_now_and_ignores_seen_pos() {
    let fresh = fix_from_json_line(
        br#"{"now":1700000000.9,"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,"seen_pos":0}"#,
    )
    .expect("fresh");
    assert_eq!(fresh.unix_s, 1_700_000_000);
    assert_eq!(pack_frame(&fresh)[27], 0);
    assert_eq!(pack_frame(&fresh).len(), FRAME_LEN);

    let old = fix_from_json_line(
        br#"{"now":1700000000.9,"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,"seen_pos":100}"#,
    )
    .expect("old position still a line");
    assert_eq!(old.unix_s, 1_700_000_000);
    assert_eq!(old.aircraft.icao, 0x40621D);

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
    assert!(
        fix_from_json_line(br#"{"now":30,"hex":"40621d","squawk":"7700","emergency":"general"}"#)
            .is_none()
    );
}

#[test]
fn line_sentinels_and_track_wrap() {
    let unknown =
        fix_from_json_line(br#"{"now":10,"hex":"abc123","lat":0,"lon":0,"gs":-1,"track":-5}"#)
            .expect("unknown");
    assert_eq!(unknown.aircraft.altitude_ft, ALT_UNKNOWN);
    assert_eq!(unknown.aircraft.ground_speed_kt, SPEED_UNKNOWN);
    assert_eq!(unknown.aircraft.track_deg, TRACK_UNKNOWN);
    assert_eq!(pack_frame(&unknown), UNKNOWN);

    let saturated = fix_from_json_line(
        br#"{"now":10,"hex":"abc124","lat":0,"lon":1,"alt_baro":"ground","gs":65534.6,"track":360}"#,
    )
    .expect("saturated");
    assert_eq!(saturated.aircraft.altitude_ft, 0);
    assert_eq!(saturated.aircraft.ground_speed_kt, 65534);
    assert_eq!(saturated.aircraft.track_deg, 0);
    assert_eq!(pack_frame(&saturated), SATURATED);

    let geometric =
        fix_from_json_line(br#"{"now":10,"hex":"000001","lat":1,"lon":2,"alt_geom":3280.4}"#)
            .expect("geom");
    assert_eq!(geometric.aircraft.altitude_ft, 3280);
    assert_eq!(geometric.aircraft.ground_speed_kt, SPEED_UNKNOWN);
    assert_eq!(geometric.aircraft.track_deg, TRACK_UNKNOWN);
    let frame = pack_frame(&geometric);
    assert_eq!(i32::from_le_bytes(frame[19..23].try_into().unwrap()), 3280);
}

#[test]
fn drops_a_bad_hex() {
    let tilde = fix_from_json_line(br#"{"now":1,"hex":"~40621d","lat":1,"lon":2}"#).expect("tilde");
    assert_eq!(tilde.aircraft.icao, 0x40621D);
    assert!(fix_from_json_line(br#"{"now":1,"hex":"40G621","lat":1,"lon":2}"#).is_none());
    assert!(fix_from_json_line(br#"{"now":1,"hex":"abcdefg","lat":1,"lon":2}"#).is_none());
    assert!(fix_from_json_line(br#"{"now":1,"hex":" abc","lat":1,"lon":2}"#).is_none());
    assert!(fix_from_json_line(br#"{"now":1,"hex":"~~40621d","lat":1,"lon":2}"#).is_none());
    assert!(fix_from_json_line(br#"{"now":1,"hex":"40621d","lat":91,"lon":2}"#).is_none());
}

#[test]
fn emergency_names_and_squawks_on_a_line() {
    let cases = [
        (r#""squawk":"7700""#, 1u8),
        (r#""squawk":"7600""#, 4),
        (r#""squawk":"7500""#, 5),
        (r#""squawk":7700"#, 1),
        (r#""squawk":7600"#, 4),
        (r#""squawk":7500"#, 5),
        (r#""squawk":" 7700 ""#, 1),
        (r#""squawk":"07700""#, 0),
        (r#""squawk":"77000""#, 0),
        (r#""squawk":"0770""#, 0),
        (r#""emergency":"general""#, 1),
        (r#""emergency":"lifeguard""#, 2),
        (r#""emergency":"minfuel""#, 3),
        (r#""emergency":"nordo""#, 4),
        (r#""emergency":"unlawful""#, 5),
        (r#""emergency":"downed""#, 6),
        (r#""emergency":"GENERAL""#, 1),
        (r#""emergency":"unlawful","squawk":"7700""#, 5),
        (r#""emergency":"none","squawk":"7700""#, 1),
        (r#""emergency":"","squawk":"7600""#, 4),
        (r#""emergency":"none""#, 0),
        (r#""squawk":"1000""#, 0),
        (r#""emergency":"medical""#, 0),
        (r#""emergency":"medical","squawk":"7700""#, 1),
        (r#""emergency":"reserved""#, 7),
        (r#""emergency":"RESERVED""#, 7),
        (r#""emergency":"reserved","squawk":"1000""#, 7),
        (r#""emergency":"reserved","squawk":"7700""#, 1),
        (r#""emergency":"reserved","squawk":"7600""#, 4),
        (r#""emergency":"reserved","squawk":"7500""#, 5),
    ];
    for (extra, code) in cases {
        let line = format!(
            r#"{{"now":1700000000.9,"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,{extra}}}"#
        );
        let fix = fix_from_json_line(line.as_bytes()).expect(extra);
        let frame = pack_frame(&fix);
        assert_eq!(frame.len(), FRAME_LEN, "{extra}");
        assert_eq!(frame[27], code, "{extra}");
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

fn spawn_check() -> StopSender {
    let child = Command::new(env!("CARGO_BIN_EXE_trck"))
        .args(["--check", "--uart", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn check");
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

enum Session {
    Next,
    Hold,
}

fn serve(
    listener: TcpListener,
    mut script: impl FnMut(&mut std::net::TcpStream) -> Session + Send + 'static,
) {
    std::thread::spawn(move || {
        loop {
            let (mut sock, _) = listener.accept().expect("accept");
            if matches!(script(&mut sock), Session::Hold) {
                std::thread::sleep(Duration::from_secs(2));
                break;
            }
        }
    });
}

#[test]
fn nothing_is_written_until_a_line_arrives() {
    let (listener, port) = listen();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    serve(listener, move |sock| {
        ready_tx.send(()).expect("ready");
        go_rx.recv().expect("go");
        writeln!(sock, "{PUBLISHED_LINE}").expect("line");
        Session::Hold
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
    serve(listener, |sock| {
        write!(
            sock,
            "not json\n{{\"now\":1}}\n{{\"now\":-1,\"hex\":\"abc123\",\"lat\":1,\"lon\":2}}\n{{\"hex\":\"abc123\",\"lat\":1,\"lon\":2}}\n{{\"now\":1.5,\"hex\":\"abc123\",\"lon\":2}}\n{{\"now\":\"1700000000\",\"hex\":\"abc123\",\"lat\":1,\"lon\":2}}\n{PUBLISHED_LINE}\n{{\"now\":1700000000,\"aircraft\":[]}}\n{SECOND_LINE}\n"
        )
        .expect("lines");
        Session::Hold
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
    serve(listener, |sock| {
        writeln!(sock, "{PUBLISHED_LINE}").expect("first");
        writeln!(sock, "{PUBLISHED_LINE}").expect("second");
        Session::Hold
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
    serve(listener, move |sock| {
        sock.write_all(&head).expect("head");
        ready_tx.send(()).expect("ready");
        go_rx.recv().expect("go");
        sock.write_all(&tail).expect("tail");
        Session::Hold
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
    serve(listener, move |sock| {
        writeln!(
            sock,
            r#"{{"now":1700000000.9,"hex":"40621d","lat":52.2572021484375,"lon":3.91937255859375,"alt_baro":38000,"gs":450.2,"track":271.4,"squawk":"7700"}}"#
        )
        .expect("squawk");
        writeln!(sock, "{unlawful}").expect("name");
        Session::Hold
    });
    let mut sender = spawn_net(port);
    let buf = collect_stdout(sender.0.stdout.take().unwrap());
    let got = wait_len(&buf, FRAME_LEN * 2, Duration::from_secs(2));
    assert_eq!(&got[..FRAME_LEN], &EMERGENCY);
    assert_eq!(&got[FRAME_LEN..FRAME_LEN + 4], b"TRCK");
    assert_eq!(got[FRAME_LEN + 27], 5);
    assert_ne!(&got[FRAME_LEN..], &EMERGENCY);
}

#[test]
fn a_dead_socket_writes_nothing_until_the_next_line() {
    let (listener, port) = listen();
    let mut first = true;
    serve(listener, move |sock| {
        if first {
            first = false;
            return Session::Next;
        }
        writeln!(sock, "{PUBLISHED_LINE}").expect("line");
        Session::Hold
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
fn check_and_net_json_together_are_an_error() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_trck"))
        .args(["--check", "--net-json", "127.0.0.1:9", "--uart", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    let buf = collect_stdout(child.stdout.take().unwrap());
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break status;
        }
        if started.elapsed() > Duration::from_secs(2) {
            let _ = child.kill();
            panic!("--check with --net-json kept running");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(!status.success(), "{status}");
    assert!(buf.lock().expect("lock").is_empty());
}

#[test]
fn check_writes_the_published_frames() {
    let expect = [SECOND, PUBLISHED, CHECK_EMERGENCY].concat();
    let mut sender = spawn_check();
    let buf = collect_stdout(sender.0.stdout.take().unwrap());
    let got = wait_len(&buf, expect.len(), Duration::from_secs(2));
    assert!(got.len() >= expect.len(), "short output: {}", got.len());
    assert_eq!(&got[..expect.len()], expect.as_slice());
    assert_eq!(&got[..4], b"TRCK");
    assert_eq!(got[27], 0);
    assert_eq!(got[FRAME_LEN + 27], 0);
    assert_eq!(got[FRAME_LEN * 2 + 27], 1);
    assert!(sender.0.try_wait().unwrap().is_none());
}

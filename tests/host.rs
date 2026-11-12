use std::fs;
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serialport::{SerialPort, TTYPort};

use trck::{
    Aircraft, DEFAULT_PATH, EmergencyKind, FRAME_LEN, Fix, Tracker, check_positions,
    crc16_ccitt_false, pack_frame, parse_hex_frame, unpack_frame,
};

const EVEN: &str = "8D40058B58C901375147EFD09357";
const ODD: &str = "8D40058B58C904A87F402D3B8C59";
const STATUS: &str = "8d40058be1200000000000a05f30";
const ORDINARY_A: &str = "8d4d224f58bf07c2d41a9a353d70";
const ORDINARY_B: &str = "8d4d224f58bf003b221b34aa5b8d";
const ICAO_40058B: u32 = 0x40058B;
const ICAO_4D224F: u32 = 0x4D224F;
const QUEUE: usize = 64;

const PUBLISHED: [u8; FRAME_LEN] = [
    0x54, 0x52, 0x43, 0x4b, 0x1d, 0x62, 0x40, 0x42, 0x41, 0x57, 0x31, 0x32, 0x33, 0x20, 0x20, 0xf5,
    0xd0, 0x25, 0x1f, 0x7e, 0x0c, 0x56, 0x02, 0x70, 0x94, 0x00, 0x00, 0xc2, 0x01, 0x0f, 0x01, 0x00,
    0x60, 0xef,
];

const SECOND: [u8; FRAME_LEN] = [
    0x54, 0x52, 0x43, 0x4b, 0xbc, 0x0a, 0x40, 0x53, 0x49, 0x4d, 0x30, 0x30, 0x30, 0x31, 0x20, 0x40,
    0x2c, 0x42, 0x06, 0x60, 0x18, 0xee, 0xf3, 0xe0, 0x2e, 0x00, 0x00, 0x18, 0x01, 0x5a, 0x00, 0x00,
    0x3e, 0x4c,
];

const CHECK_EMERGENCY: [u8; FRAME_LEN] = [
    0x54, 0x52, 0x43, 0x4b, 0x23, 0xa1, 0x4c, 0x45, 0x5a, 0x59, 0x34, 0x35, 0x36, 0x20, 0x20, 0xe0,
    0xb2, 0xad, 0x1e, 0xe0, 0x55, 0xbb, 0xff, 0x40, 0x1f, 0x00, 0x00, 0xdc, 0x00, 0xb4, 0x00, 0x01,
    0xaa, 0x98,
];

const PUBLISHED_GENERAL: [u8; FRAME_LEN] = [
    0x54, 0x52, 0x43, 0x4b, 0x1d, 0x62, 0x40, 0x42, 0x41, 0x57, 0x31, 0x32, 0x33, 0x20, 0x20, 0xf5,
    0xd0, 0x25, 0x1f, 0x7e, 0x0c, 0x56, 0x02, 0x70, 0x94, 0x00, 0x00, 0xc2, 0x01, 0x0f, 0x01, 0x01,
    0x41, 0xff,
];

fn decoded_aircraft() -> Aircraft {
    let mut tracker = Tracker::new();
    let even = parse_hex_frame(EVEN.as_bytes()).expect("even");
    let odd = parse_hex_frame(ODD.as_bytes()).expect("odd");
    assert!(tracker.ingest(1.0, &even).is_none());
    tracker.ingest(2.0, &odd).expect("position").aircraft
}

fn ordinary_aircraft() -> Aircraft {
    let mut tracker = Tracker::new();
    let first = parse_hex_frame(ORDINARY_A.as_bytes()).expect("ordinary a");
    let second = parse_hex_frame(ORDINARY_B.as_bytes()).expect("ordinary b");
    assert!(tracker.ingest(1.0, &first).is_none());
    tracker
        .ingest(2.0, &second)
        .expect("ordinary position")
        .aircraft
}

fn frame_aircraft(frame: &[u8; FRAME_LEN]) -> Aircraft {
    unpack_frame(frame).expect("frame").aircraft
}

#[test]
fn pack_and_unpack_are_inverses() {
    assert_eq!(crc16_ccitt_false(b"123456789"), 0x29B1);
    for position in check_positions() {
        let frame = pack_frame(&position);
        assert_eq!(unpack_frame(&frame), Some(position));
    }
    assert!(unpack_frame(&PUBLISHED[..FRAME_LEN - 1]).is_none());
    let mut bad_crc = PUBLISHED;
    bad_crc[FRAME_LEN - 2] ^= 0xFF;
    assert!(unpack_frame(&bad_crc).is_none());
    let mut bad_magic = PUBLISHED;
    bad_magic[0] = b'X';
    assert!(unpack_frame(&bad_magic).is_none());
}

#[test]
fn check_positions_match_published_frames() {
    let got: Vec<_> = check_positions()
        .into_iter()
        .map(|fix| pack_frame(&fix))
        .collect();
    assert_eq!(got[0], SECOND);
    assert_eq!(got[1], PUBLISHED);
    assert_eq!(got[2], CHECK_EMERGENCY);
    let mut general = check_positions()[1];
    general.aircraft.emergency = Some(EmergencyKind::General);
    assert_eq!(pack_frame(&general), PUBLISHED_GENERAL);
    assert_eq!(unpack_frame(&PUBLISHED_GENERAL), Some(general));
}

struct StopSender(std::process::Child);

impl Drop for StopSender {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn unique_path(name: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "trck-{}-{}-{name}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ))
}

fn table_arg() -> PathBuf {
    unique_path("aircraft.csv")
}

fn spawn_trck(args: &[&str]) -> StopSender {
    let child = Command::new(env!("CARGO_BIN_EXE_trck"))
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn sender");
    StopSender(child)
}

fn spawn_hex(port: u16) -> StopSender {
    spawn_hex_table(port, &table_arg())
}

fn spawn_hex_table(port: u16, table: &Path) -> StopSender {
    let addr = format!("127.0.0.1:{port}");
    spawn_trck(&[
        "--hex",
        &addr,
        "--uart",
        "-",
        "--table",
        table.to_str().expect("utf8"),
    ])
}

fn spawn_check() -> StopSender {
    spawn_check_table(&table_arg())
}

fn spawn_check_table(table: &Path) -> StopSender {
    spawn_trck(&[
        "--check",
        "--uart",
        "-",
        "--table",
        table.to_str().expect("utf8"),
    ])
}

fn spawn_uart(port: u16, uart: &str) -> StopSender {
    spawn_uart_table(port, uart, &table_arg())
}

fn spawn_uart_table(port: u16, uart: &str, table: &Path) -> StopSender {
    let addr = format!("127.0.0.1:{port}");
    let child = Command::new(env!("CARGO_BIN_EXE_trck"))
        .args([
            "--hex",
            &addr,
            "--uart",
            uart,
            "--table",
            table.to_str().expect("utf8"),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn sender");
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
    CloseAndAccept,
    KeepOpen,
}

fn serve(
    listener: TcpListener,
    mut script: impl FnMut(&mut std::net::TcpStream) -> Session + Send + 'static,
) {
    std::thread::spawn(move || {
        loop {
            let (mut sock, _) = listener.accept().expect("accept");
            if matches!(script(&mut sock), Session::KeepOpen) {
                std::thread::sleep(Duration::from_secs(2));
                break;
            }
        }
    });
}

fn write_pair(sock: &mut std::net::TcpStream) {
    writeln!(sock, "{EVEN}").expect("even");
    writeln!(sock, "{ODD}").expect("odd");
}

#[test]
fn nothing_is_written_until_a_position_solves() {
    let (listener, port) = listen();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    serve(listener, move |sock| {
        ready_tx.send(()).expect("ready");
        go_rx.recv().expect("go");
        write_pair(sock);
        Session::KeepOpen
    });
    let mut sender = spawn_hex(port);
    let buf = collect_stdout(sender.0.stdout.take().unwrap());
    ready_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("connected");
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        buf.lock().expect("lock").is_empty(),
        "bytes before a pair: {:02x?}",
        buf.lock().expect("lock")
    );
    go_tx.send(()).expect("release");
    let got = wait_len(&buf, FRAME_LEN, Duration::from_secs(2));
    assert_eq!(got.len(), FRAME_LEN);
    assert_eq!(
        frame_aircraft(&got.as_chunks::<FRAME_LEN>().0[0]),
        decoded_aircraft()
    );
    assert!(sender.0.try_wait().unwrap().is_none());
}

#[test]
fn one_frame_per_solved_position() {
    let (listener, port) = listen();
    serve(listener, |sock| {
        writeln!(sock, "not hex").expect("junk");
        writeln!(sock, "{EVEN}").expect("even");
        writeln!(sock, "{ODD}").expect("odd");
        Session::KeepOpen
    });
    let mut sender = spawn_hex(port);
    let buf = collect_stdout(sender.0.stdout.take().unwrap());
    let expect = decoded_aircraft();
    let got = wait_len(&buf, FRAME_LEN, Duration::from_secs(2));
    assert_eq!(got.len(), FRAME_LEN);
    assert_eq!(frame_aircraft(&got.as_chunks::<FRAME_LEN>().0[0]), expect);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(buf.lock().expect("lock").len(), FRAME_LEN);
    assert!(sender.0.try_wait().unwrap().is_none());
}

#[test]
fn the_same_icao_is_sent_twice() {
    let (listener, port) = listen();
    serve(listener, |sock| {
        write_pair(sock);
        write_pair(sock);
        Session::KeepOpen
    });
    let mut sender = spawn_hex(port);
    let buf = collect_stdout(sender.0.stdout.take().unwrap());
    let expect = decoded_aircraft();
    let got = wait_len(&buf, FRAME_LEN * 2, Duration::from_secs(2));
    assert!(
        got.len() >= FRAME_LEN * 2,
        "short output: {} bytes",
        got.len()
    );
    let frames = got.as_chunks::<FRAME_LEN>().0;
    assert!(frames.len() >= 2, "frames {}", frames.len());
    assert_eq!(frame_aircraft(&frames[0]), expect);
    for frame in frames {
        assert_eq!(frame_aircraft(frame).icao, expect.icao);
    }
    assert!(sender.0.try_wait().unwrap().is_none());
}

#[test]
fn a_split_hex_line_is_one_frame() {
    let (listener, port) = listen();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let even = EVEN.as_bytes();
    let (head, tail) = even.split_at(even.len() / 2);
    let head = head.to_vec();
    let mut tail = tail.to_vec();
    tail.push(b'\n');
    serve(listener, move |sock| {
        sock.write_all(&head).expect("head");
        ready_tx.send(()).expect("ready");
        go_rx.recv().expect("go");
        sock.write_all(&tail).expect("tail");
        writeln!(sock, "{ODD}").expect("odd");
        Session::KeepOpen
    });
    let mut sender = spawn_hex(port);
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
    assert_eq!(got.len(), FRAME_LEN);
    assert_eq!(
        frame_aircraft(&got.as_chunks::<FRAME_LEN>().0[0]),
        decoded_aircraft()
    );
}

#[test]
fn a_dead_socket_writes_nothing_until_the_next_position() {
    let (listener, port) = listen();
    let mut first = true;
    serve(listener, move |sock| {
        if first {
            first = false;
            return Session::CloseAndAccept;
        }
        write_pair(sock);
        Session::KeepOpen
    });
    let mut sender = spawn_hex(port);
    let buf = collect_stdout(sender.0.stdout.take().unwrap());
    std::thread::sleep(Duration::from_millis(400));
    assert!(
        buf.lock().expect("lock").is_empty(),
        "dead socket wrote {:02x?}",
        buf.lock().expect("lock")
    );
    let got = wait_len(&buf, FRAME_LEN, Duration::from_secs(4));
    assert_eq!(got.len(), FRAME_LEN);
    assert_eq!(
        frame_aircraft(&got.as_chunks::<FRAME_LEN>().0[0]),
        decoded_aircraft()
    );
    assert!(sender.0.try_wait().unwrap().is_none());
}

#[test]
fn a_refused_port_writes_nothing() {
    let (listener, port) = listen();
    drop(listener);
    let mut sender = spawn_hex(port);
    let buf = collect_stdout(sender.0.stdout.take().unwrap());
    std::thread::sleep(Duration::from_millis(700));
    assert!(buf.lock().expect("lock").is_empty());
    assert!(sender.0.try_wait().unwrap().is_none());
}

#[test]
fn check_and_hex_together_are_an_error() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_trck"))
        .args(["--check", "--hex", "127.0.0.1:9", "--uart", "-"])
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
            panic!("--check with --hex kept running");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(!status.success(), "{status}");
    assert!(buf.lock().expect("lock").is_empty());
}

#[test]
fn doctor_and_check_together_are_an_error() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_trck"))
        .args(["doctor", "--check", "--uart", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    let buf = collect_stdout(child.stdout.take().unwrap());
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break status;
        }
        if started.elapsed() > Duration::from_secs(2) {
            let _ = child.kill();
            panic!("doctor with --check kept running");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(!status.success(), "{status}");
    assert!(buf.lock().expect("lock").is_empty());
}

fn doctor_output(dir: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_trck"))
        .arg("doctor")
        .current_dir(dir)
        .output()
        .expect("doctor")
}

#[test]
fn doctor_prints_one_line_per_check_and_does_not_send_frames() {
    let dir = std::env::temp_dir().join(format!("trck-doctor-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let out = doctor_output(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().filter(|line| !line.is_empty()).collect();
    assert!(
        lines.len() >= 6,
        "expected one line per check, got {lines:?}"
    );
    for line in &lines {
        let status = line.split_whitespace().next();
        assert!(matches!(status, Some("ok" | "fail" | "warn")), "{line}");
    }
    let names: Vec<&str> = lines
        .iter()
        .filter_map(|line| line.split_whitespace().nth(1))
        .collect();
    for name in [
        "rtlsdr:",
        "uart:",
        "uart-console:",
        "csv:",
        "usb:",
        "clock:",
    ] {
        assert!(names.contains(&name), "{stdout}");
    }
    assert!(
        lines.iter().any(|line| line.starts_with("ok csv:")),
        "{stdout}"
    );
    assert!(!out.stdout.windows(4).any(|window| window == b"TRCK"));
    assert_ne!(out.status.code(), Some(0), "{stdout}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn check_writes_the_published_frames() {
    let expect = [SECOND, PUBLISHED, CHECK_EMERGENCY].concat();
    let mut sender = spawn_check();
    let buf = collect_stdout(sender.0.stdout.take().unwrap());
    let got = wait_len(&buf, expect.len(), Duration::from_secs(2));
    assert!(got.len() >= expect.len(), "short output: {}", got.len());
    assert_eq!(&got[..expect.len()], expect.as_slice());
    assert!(sender.0.try_wait().unwrap().is_none());
}

struct Pty {
    master: TTYPort,
    slave: TTYPort,
}

impl Pty {
    fn open() -> Self {
        let (master, slave) = TTYPort::pair().expect("pty");
        let slave_fd = slave.as_raw_fd();
        // SAFETY: `slave_fd` is the open pty slave this `Pty` owns.
        unsafe {
            let _ = libc::ioctl(slave_fd, libc::TIOCNXCL);
            let _ = libc::flock(slave_fd, libc::LOCK_UN);
            let flags = libc::fcntl(slave_fd, libc::F_GETFL);
            assert!(flags >= 0, "{}", io::Error::last_os_error());
            let rc = libc::fcntl(slave_fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
            assert_eq!(rc, 0, "{}", io::Error::last_os_error());
        }
        Self { master, slave }
    }

    fn path(&self) -> String {
        self.slave.name().expect("pty name")
    }
}

fn frame_icao(frame: &[u8]) -> u32 {
    u32::from_le_bytes([frame[4], frame[5], frame[6], 0])
}

fn frame_is_general(frame: &[u8]) -> bool {
    unpack_frame(frame).is_some_and(|fix| fix.aircraft.emergency == Some(EmergencyKind::General))
}

fn fill_until_blocked(fd: i32) -> usize {
    let buf = [0xA5u8; 1024];
    let mut wrote = 0usize;
    let mut chunk = buf.len();
    loop {
        // SAFETY: `fd` is the open pty slave. `buf` is a live slice `write` does not keep.
        let rc = unsafe { libc::write(fd, buf.as_ptr().cast(), chunk) };
        if rc < 0 {
            let err = io::Error::last_os_error();
            assert_eq!(err.kind(), io::ErrorKind::WouldBlock, "{err}");
            if chunk == 1 {
                break;
            }
            chunk = 1;
            continue;
        }
        assert!(rc > 0);
        wrote += rc as usize;
        assert!(
            wrote < 1024 * 1024,
            "pty accepted a megabyte without blocking"
        );
    }
    assert!(wrote > 0, "pty buffer was already full");
    wrote
}

fn complete_frames(raw: &[u8]) -> Vec<[u8; FRAME_LEN]> {
    let Some(pos) = raw.windows(4).position(|window| window == b"TRCK") else {
        return Vec::new();
    };
    let (frames, _) = raw[pos..].as_chunks::<FRAME_LEN>();
    frames.to_vec()
}

fn wait_frames_until(
    master: &mut TTYPort,
    timeout: Duration,
    ready: impl Fn(&[[u8; FRAME_LEN]]) -> bool,
) -> Vec<[u8; FRAME_LEN]> {
    let fd = master.as_raw_fd();
    // SAFETY: `fd` is the open pty master this test owns.
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        assert!(flags >= 0, "{}", io::Error::last_os_error());
        let rc = libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
        assert_eq!(rc, 0, "{}", io::Error::last_os_error());
    }
    let mut raw = Vec::new();
    let started = Instant::now();
    loop {
        let mut tmp = [0u8; 4096];
        // SAFETY: `fd` is the open pty master. `tmp` is a live buffer `read` does not keep.
        let rc = unsafe { libc::read(fd, tmp.as_mut_ptr().cast(), tmp.len()) };
        if rc > 0 {
            raw.extend_from_slice(&tmp[..rc as usize]);
            let frames = complete_frames(&raw);
            if ready(&frames) {
                return frames;
            }
            continue;
        }
        if rc < 0 {
            let err = io::Error::last_os_error();
            assert!(
                err.kind() == io::ErrorKind::WouldBlock || err.kind() == io::ErrorKind::Interrupted,
                "{err}"
            );
        }
        if started.elapsed() > timeout {
            panic!("got {} bytes", raw.len());
        }
        let left = timeout.saturating_sub(started.elapsed());
        let mut poll_fd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: `poll_fd` is one live `pollfd` for the pty master this test owns.
        let prc = unsafe { libc::poll(&mut poll_fd, 1, left.as_millis() as libc::c_int) };
        if prc < 0 {
            let err = io::Error::last_os_error();
            assert_eq!(err.kind(), io::ErrorKind::Interrupted, "{err}");
        }
    }
}

fn wait_frames(master: &mut TTYPort, n: usize, timeout: Duration) -> Vec<[u8; FRAME_LEN]> {
    let mut frames = wait_frames_until(master, timeout, |frames| frames.len() >= n);
    frames.truncate(n);
    frames
}

fn spawn_hex_stdin(uart: &str) -> StopSender {
    let table = table_arg();
    let child = Command::new(env!("CARGO_BIN_EXE_trck"))
        .args([
            "--hex",
            "-",
            "--uart",
            uart,
            "--table",
            table.to_str().expect("utf8"),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn sender");
    StopSender(child)
}

fn decoded_7700_aircraft() -> Aircraft {
    let mut tracker = Tracker::new();
    let status = parse_hex_frame(STATUS.as_bytes()).expect("status");
    let even = parse_hex_frame(EVEN.as_bytes()).expect("even");
    let odd = parse_hex_frame(ODD.as_bytes()).expect("odd");
    assert!(tracker.ingest(1.0, &status).is_none());
    assert!(tracker.ingest(2.0, &even).is_none());
    tracker.ingest(3.0, &odd).expect("position").aircraft
}

#[test]
fn avr_hex_writes_a_pty_frame() {
    let mut pty = Pty::open();
    let mut sender = spawn_hex_stdin(&pty.path());
    let mut stdin = sender.0.stdin.take().expect("stdin");
    writeln!(stdin, "*{STATUS};").expect("status");
    writeln!(stdin, "*{EVEN};").expect("even");
    writeln!(stdin, "@000000000000{ODD};").expect("odd");
    stdin.flush().expect("flush");
    let frames = wait_frames(&mut pty.master, 1, Duration::from_secs(2));
    let expect = decoded_7700_aircraft();
    assert_eq!(frame_aircraft(&frames[0]), expect);
    assert_eq!(expect.icao.to_u24(), ICAO_40058B);
    assert_eq!(expect.emergency, Some(EmergencyKind::General));
    assert!(sender.0.try_wait().unwrap().is_none());
}

#[test]
fn a_tty_writes_a_decoded_frame() {
    let (listener, port) = listen();
    let mut pty = Pty::open();
    let mut sender = spawn_uart(port, &pty.path());
    let (mut sock, _) = listener.accept().expect("accept");
    write_pair(&mut sock);
    let frames = wait_frames(&mut pty.master, 1, Duration::from_secs(2));
    assert_eq!(frame_aircraft(&frames[0]), decoded_aircraft());
    assert!(sender.0.try_wait().unwrap().is_none());
}

#[test]
fn queued_tty_frames_survive_reconnect() {
    let (listener, port) = listen();
    let mut pty = Pty::open();
    let mut sender = spawn_uart(port, &pty.path());
    {
        let (mut sock, _) = listener.accept().expect("accept");
        write_pair(&mut sock);
    }
    let (mut sock, _) = listener.accept().expect("accept");
    write_pair(&mut sock);
    let frames = wait_frames(&mut pty.master, 2, Duration::from_secs(5));
    let expect = decoded_aircraft();
    assert_eq!(frame_aircraft(&frames[0]), expect);
    assert_eq!(frame_aircraft(&frames[1]), expect);
    assert!(sender.0.try_wait().unwrap().is_none());
}

#[test]
fn a_full_tty_drops_ordinary_hex_and_keeps_7700() {
    assert_eq!(ordinary_aircraft().icao.to_u24(), ICAO_4D224F);
    let (listener, port) = listen();
    let mut pty = Pty::open();
    let mut sender = spawn_uart(port, &pty.path());
    let (mut sock, _) = listener.accept().expect("accept");
    fill_until_blocked(pty.slave.as_raw_fd());
    sock.set_nodelay(true).expect("nodelay");
    let mut body = String::new();
    for _ in 0..=QUEUE {
        body.push_str(ORDINARY_A);
        body.push('\n');
        body.push_str(ORDINARY_B);
        body.push('\n');
    }
    body.push_str(STATUS);
    body.push('\n');
    body.push_str(EVEN);
    body.push('\n');
    body.push_str(ODD);
    body.push('\n');
    sock.write_all(body.as_bytes()).expect("lines");
    let frames = wait_frames_until(&mut pty.master, Duration::from_secs(5), |frames| {
        frames.iter().any(|frame| frame_is_general(frame))
    });
    let icaos: Vec<u32> = frames.iter().map(|frame| frame_icao(frame)).collect();
    let ordinary = icaos
        .iter()
        .copied()
        .filter(|icao| *icao == ICAO_4D224F)
        .count();
    assert!(ordinary > 0, "{icaos:?}");
    assert!(ordinary < QUEUE + 1, "{icaos:?}");
    assert!(
        frames
            .iter()
            .any(|frame| frame_icao(frame) == ICAO_40058B && frame_is_general(frame)),
        "{icaos:?}"
    );
    assert!(sender.0.try_wait().unwrap().is_none());
}

#[test]
fn a_full_tty_of_emergencies_drops_ordinary() {
    assert_eq!(ordinary_aircraft().icao.to_u24(), ICAO_4D224F);
    let (listener, port) = listen();
    let mut pty = Pty::open();
    let mut sender = spawn_uart(port, &pty.path());
    let (mut sock, _) = listener.accept().expect("accept");
    fill_until_blocked(pty.slave.as_raw_fd());
    sock.set_nodelay(true).expect("nodelay");
    let mut body = String::new();
    body.push_str(STATUS);
    body.push('\n');
    for _ in 0..=QUEUE {
        body.push_str(EVEN);
        body.push('\n');
        body.push_str(ODD);
        body.push('\n');
    }
    body.push_str(ORDINARY_A);
    body.push('\n');
    body.push_str(ORDINARY_B);
    body.push('\n');
    sock.write_all(body.as_bytes()).expect("lines");
    let frames = wait_frames_until(&mut pty.master, Duration::from_secs(5), |frames| {
        frames.len() >= QUEUE
            && frames
                .iter()
                .any(|frame| frame_icao(frame) == ICAO_40058B && frame_is_general(frame))
    });
    let icaos: Vec<u32> = frames.iter().map(|frame| frame_icao(frame)).collect();
    assert!(!icaos.contains(&ICAO_4D224F), "{icaos:?}");
    assert!(
        frames
            .iter()
            .any(|frame| frame_icao(frame) == ICAO_40058B && frame_is_general(frame)),
        "{icaos:?}"
    );
    assert!(sender.0.try_wait().unwrap().is_none());
}

fn wait_csv_rows(path: &Path, n: usize, timeout: Duration) -> Vec<String> {
    let started = Instant::now();
    loop {
        if let Ok(text) = fs::read_to_string(path) {
            let mut lines: Vec<String> = text
                .lines()
                .filter(|line| !line.is_empty())
                .map(str::to_string)
                .collect();
            if lines.len() > n {
                assert_eq!(
                    lines[0],
                    "icao,callsign,lat,lon,altitude_ft,ground_speed_kt,track_deg,timestamp"
                );
                lines.remove(0);
                return lines;
            }
        }
        if started.elapsed() > timeout {
            let got = fs::read_to_string(path).unwrap_or_default();
            panic!(
                "table {} missing {n} rows after {:?}: {got:?}",
                path.display(),
                timeout
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn assert_csv_row(row: &str, fix: &Fix, before: u64, after: u64) {
    let cols: Vec<&str> = row.split(',').collect();
    assert_eq!(cols.len(), 8, "{row}");
    let aircraft = &fix.aircraft;
    assert_eq!(cols[0], format!("{:06x}", aircraft.icao));
    let callsign_bytes = aircraft.callsign.as_bytes();
    let callsign = String::from_utf8_lossy(&callsign_bytes);
    assert_eq!(cols[1], callsign.trim_end());
    let lat: f64 = cols[2].parse().expect("lat");
    let lon: f64 = cols[3].parse().expect("lon");
    assert!((lat - f64::from(aircraft.latitude_e7) / 10_000_000.0).abs() < 1e-7);
    assert!((lon - f64::from(aircraft.longitude_e7) / 10_000_000.0).abs() < 1e-7);
    match aircraft.altitude_ft {
        Some(v) => assert_eq!(cols[4], v.to_string()),
        None => assert_eq!(cols[4], ""),
    }
    match aircraft.ground_speed_kt {
        Some(v) => assert_eq!(cols[5], v.to_string()),
        None => assert_eq!(cols[5], ""),
    }
    match aircraft.track_deg {
        Some(v) => assert_eq!(cols[6], v.to_string()),
        None => assert_eq!(cols[6], ""),
    }
    let timestamp: u64 = cols[7].parse().expect("timestamp");
    assert!(
        timestamp >= before && timestamp <= after,
        "timestamp {timestamp} not in {before}..={after}"
    );
}

#[test]
fn check_writes_csv_rows() {
    let table = table_arg();
    let before = now_secs();
    let mut sender = spawn_check_table(&table);
    let buf = collect_stdout(sender.0.stdout.take().unwrap());
    let rows = wait_csv_rows(&table, 3, Duration::from_secs(2));
    let after = now_secs() + 1;
    let expect = check_positions();
    for (row, fix) in rows.iter().zip(expect.iter()) {
        assert_csv_row(row, fix, before, after);
    }
    let got = wait_len(&buf, FRAME_LEN * 3, Duration::from_secs(2));
    assert!(got.len() >= FRAME_LEN * 3);
    assert!(sender.0.try_wait().unwrap().is_none());
}

#[test]
fn hex_writes_csv_when_a_position_solves() {
    let table = table_arg();
    let (listener, port) = listen();
    serve(listener, |sock| {
        write_pair(sock);
        Session::KeepOpen
    });
    let before = now_secs();
    let mut sender = spawn_hex_table(port, &table);
    let buf = collect_stdout(sender.0.stdout.take().unwrap());
    let rows = wait_csv_rows(&table, 1, Duration::from_secs(2));
    let after = now_secs() + 1;
    let expect = Fix {
        aircraft: decoded_aircraft(),
    };
    assert_csv_row(&rows[0], &expect, before, after);
    let got = wait_len(&buf, FRAME_LEN, Duration::from_secs(2));
    assert_eq!(got.len(), FRAME_LEN);
    assert!(sender.0.try_wait().unwrap().is_none());
}

#[test]
fn csv_is_written_while_the_tty_is_full() {
    let table = table_arg();
    let (listener, port) = listen();
    let pty = Pty::open();
    let mut sender = spawn_uart_table(port, &pty.path(), &table);
    let (mut sock, _) = listener.accept().expect("accept");
    fill_until_blocked(pty.slave.as_raw_fd());
    let before = now_secs();
    write_pair(&mut sock);
    let rows = wait_csv_rows(&table, 1, Duration::from_secs(2));
    let after = now_secs() + 1;
    let expect = Fix {
        aircraft: decoded_aircraft(),
    };
    assert_csv_row(&rows[0], &expect, before, after);
    assert!(sender.0.try_wait().unwrap().is_none());
}

#[test]
fn default_table_is_aircraft_csv_in_the_working_directory() {
    let dir = unique_path("cwd");
    fs::create_dir_all(&dir).expect("cwd");
    let mut sender = StopSender(
        Command::new(env!("CARGO_BIN_EXE_trck"))
            .current_dir(&dir)
            .args(["--check", "--uart", "-"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn check"),
    );
    let path = dir.join(DEFAULT_PATH);
    let rows = wait_csv_rows(&path, 3, Duration::from_secs(2));
    assert_eq!(rows.len(), 3);
    assert!(sender.0.try_wait().unwrap().is_none());
    drop(sender);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn table_flag_overrides_the_path() {
    let dir = unique_path("cwd");
    fs::create_dir_all(&dir).expect("cwd");
    let table = unique_path("override.csv");
    let mut sender = StopSender(
        Command::new(env!("CARGO_BIN_EXE_trck"))
            .current_dir(&dir)
            .args([
                "--check",
                "--uart",
                "-",
                "--table",
                table.to_str().expect("utf8"),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn check"),
    );
    let rows = wait_csv_rows(&table, 3, Duration::from_secs(2));
    assert_eq!(rows.len(), 3);
    assert!(!dir.join(DEFAULT_PATH).exists());
    assert!(sender.0.try_wait().unwrap().is_none());
    drop(sender);
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_file(&table);
}

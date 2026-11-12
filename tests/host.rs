use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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

fn write_until_would_block(fd: i32) {
    let buf = [0xA5u8; 1024];
    let mut chunk = buf.len();
    loop {
        // SAFETY: `fd` is the open pty slave. `buf` is a live slice `write` does not keep.
        let rc = unsafe { libc::write(fd, buf.as_ptr().cast(), chunk) };
        if rc < 0 {
            let err = io::Error::last_os_error();
            assert_eq!(err.kind(), io::ErrorKind::WouldBlock, "{err}");
            if chunk == 1 {
                return;
            }
            chunk = 1;
            continue;
        }
        if rc == 0 {
            return;
        }
        chunk = buf.len();
    }
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

#[test]
fn stress_one_over_tracker_cap_still_emits() {
    let table = table_arg();
    let (status, stdout, stderr) = run_stress(&[
        "--uart",
        "-",
        "--table",
        table.to_str().expect("utf8"),
        "--messages",
        "514",
        "--aircraft",
        "257",
        "--rate",
        "0",
        "--ident",
        "0",
        "--velocity",
        "0",
        "--junk",
        "0",
        "--emergency",
        "0",
    ]);
    assert!(status.success(), "{stderr}");
    let frames = stdout.len() / FRAME_LEN;
    assert_eq!(frames, 256, "{stderr} bytes={}", stdout.len());
    assert_eq!(summary_count(&stderr, "fixes"), 256, "{stderr}");
    let _ = fs::remove_file(&table);
}

fn wait_exit(child: &mut std::process::Child, timeout: Duration) -> std::process::ExitStatus {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("wait") {
            return status;
        }
        if started.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            panic!("stress did not exit after {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn run_stress(args: &[&str]) -> (std::process::ExitStatus, Vec<u8>, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_trck"))
        .arg("stress")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn stress");
    let stdout = collect_stdout(child.stdout.take().expect("stdout"));
    let stderr = collect_stdout(child.stderr.take().expect("stderr"));
    let status = wait_exit(&mut child, Duration::from_secs(8));
    std::thread::sleep(Duration::from_millis(50));
    let out = stdout.lock().expect("stdout").clone();
    let err = String::from_utf8_lossy(&stderr.lock().expect("stderr")).into_owned();
    (status, out, err)
}

#[test]
fn stress_help_lists_the_knobs() {
    let out = Command::new(env!("CARGO_BIN_EXE_trck"))
        .args(["stress", "--help"])
        .output()
        .expect("help");
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    for flag in [
        "--aircraft",
        "--rate",
        "--seconds",
        "--messages",
        "--emergency",
        "--ident",
        "--velocity",
        "--junk",
        "--uart",
        "--table",
    ] {
        assert!(text.contains(flag), "{text}");
    }
}

#[test]
fn stress_rejects_a_fraction_outside_0_1() {
    let out = Command::new(env!("CARGO_BIN_EXE_trck"))
        .args(["stress", "--emergency", "2", "--uart", "-"])
        .output()
        .expect("bad fraction");
    assert!(!out.status.success(), "{out:?}");
}

#[test]
fn stress_writes_frames_csv_and_a_summary() {
    let table = table_arg();
    let (status, stdout, stderr) = run_stress(&[
        "--uart",
        "-",
        "--table",
        table.to_str().expect("utf8"),
        "--messages",
        "40",
        "--aircraft",
        "4",
        "--rate",
        "0",
        "--ident",
        "0.1",
        "--velocity",
        "0.1",
        "--junk",
        "0.1",
        "--emergency",
        "0.25",
    ]);
    assert!(status.success(), "{stderr}");
    assert!(
        stdout.windows(4).any(|window| window == b"TRCK"),
        "no UART frames: {} bytes",
        stdout.len()
    );
    assert_eq!(stdout.len() % FRAME_LEN, 0);
    for frame in stdout.as_chunks::<FRAME_LEN>().0 {
        assert!(unpack_frame(frame).is_some(), "{frame:02x?}");
    }
    for token in ["messages=", "fixes=", "dropped=", "csv=", "elapsed="] {
        assert!(stderr.contains(token), "{stderr}");
    }
    let rows = fs::read_to_string(&table).expect("csv");
    assert!(rows.lines().count() > 1, "{rows}");
    let _ = fs::remove_file(&table);
}

#[test]
fn stress_7700_is_on_the_wire() {
    let table = table_arg();
    let (status, stdout, stderr) = run_stress(&[
        "--uart",
        "-",
        "--table",
        table.to_str().expect("utf8"),
        "--messages",
        "8",
        "--aircraft",
        "1",
        "--rate",
        "0",
        "--ident",
        "0",
        "--velocity",
        "0",
        "--junk",
        "0",
        "--emergency",
        "1",
    ]);
    assert!(status.success(), "{stderr}");
    let frames = stdout.as_chunks::<FRAME_LEN>().0;
    assert!(!frames.is_empty(), "no frames");
    assert!(
        frames.iter().any(|frame| frame_is_general(frame)),
        "no 7700: {} frames",
        frames.len()
    );
    let _ = fs::remove_file(&table);
}

const CROSSING: &str = "examples/scenarios/crossing.toml";
const CLIMB: &str = "examples/scenarios/climb.toml";
const GRU_TRACES: &[&str] = &[
    "examples/traces/trace_full_e4917f.json",
    "examples/traces/trace_full_e49450.json",
    "examples/traces/trace_full_e49fb4.json",
];
const POS_TOL_M: f64 = 10.0;

fn run_sim(args: &[&str]) -> (std::process::ExitStatus, String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_trck"))
        .arg("sim")
        .args(args)
        .output()
        .expect("sim");
    (
        out.status,
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn haversine_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let r = 6_371_000.0;
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlon / 2.0).sin().powi(2);
    2.0 * r * a.sqrt().asin()
}

fn parse_truth(text: &str) -> BTreeMap<(i64, u32), (f64, f64, i32)> {
    let mut rows = BTreeMap::new();
    for line in text.lines().skip(1) {
        let cols: Vec<&str> = line.split(',').collect();
        assert_eq!(cols.len(), 5, "{line}");
        let ts_ms = (cols[0].parse::<f64>().expect("ts") * 1000.0).round() as i64;
        let icao = u32::from_str_radix(cols[1], 16).expect("icao");
        let lat: f64 = cols[2].parse().expect("lat");
        let lon: f64 = cols[3].parse().expect("lon");
        let alt: i32 = cols[4].parse().expect("alt");
        rows.insert((ts_ms, icao), (lat, lon, alt));
    }
    rows
}

fn ingest_against_truth(
    label: &str,
    frames: &str,
    truth_text: &str,
) -> (BTreeMap<u32, f64>, BTreeMap<u32, usize>, bool, bool) {
    let truth = parse_truth(truth_text);
    assert!(!truth.is_empty(), "{label} truth empty");
    let mut tracker = Tracker::new();
    let mut max_err = BTreeMap::<u32, f64>::new();
    let mut fixes = BTreeMap::<u32, usize>::new();
    let mut saw_callsign = false;
    let mut saw_7700 = false;
    for line in frames.lines() {
        let (ts, hex) = line.split_once(',').expect(line);
        let t: f64 = ts.parse().expect(ts);
        let frame = parse_hex_frame(hex.as_bytes()).expect(hex);
        let Some(fix) = tracker.ingest(t, &frame) else {
            continue;
        };
        let icao = fix.aircraft.icao.to_u24();
        let ts_ms = (t * 1000.0).round() as i64;
        let &(lat, lon, alt) = truth
            .get(&(ts_ms, icao))
            .unwrap_or_else(|| panic!("no truth for {icao:06x} t={t}"));
        let got_lat = f64::from(fix.aircraft.latitude_e7) / 10_000_000.0;
        let got_lon = f64::from(fix.aircraft.longitude_e7) / 10_000_000.0;
        let err = haversine_m(lat, lon, got_lat, got_lon);
        let slot = max_err.entry(icao).or_insert(0.0);
        if err > *slot {
            *slot = err;
        }
        assert!(
            err < POS_TOL_M,
            "{label} icao={icao:06x} t={t} err={err} m truth={lat},{lon} got={got_lat},{got_lon}"
        );
        assert_eq!(fix.aircraft.altitude_ft, Some(alt));
        let callsign_bytes = fix.aircraft.callsign.as_bytes();
        let callsign = String::from_utf8_lossy(&callsign_bytes);
        if !callsign.trim().is_empty() {
            saw_callsign = true;
        }
        if fix.aircraft.emergency == Some(EmergencyKind::General) {
            saw_7700 = true;
        }
        *fixes.entry(icao).or_insert(0) += 1;
    }
    for icao in truth.keys().map(|(_, icao)| *icao) {
        assert!(
            fixes.get(&icao).copied().unwrap_or(0) > 0,
            "no fixes for {icao:06x}"
        );
    }
    let err_txt: Vec<String> = max_err
        .iter()
        .map(|(icao, err)| format!("{icao:06x}={err:.2}"))
        .collect();
    let fix_txt: Vec<String> = fixes
        .iter()
        .map(|(icao, n)| format!("{icao:06x}={n}"))
        .collect();
    eprintln!(
        "sim {label} max position error m {} fixes={}",
        err_txt.join(" "),
        fix_txt.join(" ")
    );
    (max_err, fixes, saw_callsign, saw_7700)
}

fn assert_sim_matches_truth(scenario: &str, expect_7700: bool) -> BTreeMap<u32, f64> {
    let frames_path = unique_path("frames.csv");
    let truth_path = unique_path("truth.csv");
    let (status, _, stderr) = run_sim(&[
        scenario,
        "--out",
        frames_path.to_str().expect("utf8"),
        "--truth",
        truth_path.to_str().expect("utf8"),
    ]);
    assert!(status.success(), "{stderr}");
    let frames = fs::read_to_string(&frames_path).expect("frames");
    let truth = fs::read_to_string(&truth_path).expect("truth");
    let (max_err, _, saw_callsign, saw_7700) = ingest_against_truth(scenario, &frames, &truth);
    assert!(saw_callsign, "{scenario} no callsign");
    if expect_7700 {
        assert!(saw_7700, "{scenario} missing 7700");
    }
    let _ = fs::remove_file(&frames_path);
    let _ = fs::remove_file(&truth_path);
    max_err
}

#[test]
fn sim_help_lists_the_knobs() {
    let out = Command::new(env!("CARGO_BIN_EXE_trck"))
        .args(["sim", "--help"])
        .output()
        .expect("help");
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("--out"), "{text}");
    assert!(text.contains("--truth"), "{text}");
    assert!(text.contains("--trace"), "{text}");
    assert!(text.contains("--iq-out"), "{text}");
    assert!(
        text.contains("SCENARIO") || text.contains("scenario"),
        "{text}"
    );
}

#[test]
fn sim_rejects_a_malformed_scenario() {
    let path = unique_path("bad.toml");
    fs::write(&path, "duration = 1\n").expect("write");
    let (status, stdout, stderr) = run_sim(&[path.to_str().expect("utf8")]);
    assert!(!status.success(), "{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.contains("sim:"), "{stderr}");
    assert!(stderr.contains("aircraft"), "{stderr}");
    let _ = fs::remove_file(&path);
}

#[test]
fn sim_output_is_byte_identical() {
    let (a_status, a, a_err) = run_sim(&[CROSSING]);
    let (b_status, b, b_err) = run_sim(&[CROSSING]);
    assert!(a_status.success(), "{a_err}");
    assert!(b_status.success(), "{b_err}");
    assert_eq!(a, b);
    assert!(a.starts_with("0.000,"), "{a}");
}

#[test]
fn sim_fixes_match_scenario_ground_truth() {
    let crossing = assert_sim_matches_truth(CROSSING, true);
    let climb = assert_sim_matches_truth(CLIMB, false);
    assert_eq!(crossing.len(), 2, "{crossing:?}");
    assert_eq!(climb.len(), 1, "{climb:?}");
}

#[test]
fn sim_trace_fixes_match_truth() {
    let frames_path = unique_path("frames.csv");
    let truth_path = unique_path("truth.csv");
    let mut args: Vec<&str> = vec!["--trace"];
    args.extend_from_slice(GRU_TRACES);
    args.extend_from_slice(&[
        "--out",
        frames_path.to_str().expect("utf8"),
        "--truth",
        truth_path.to_str().expect("utf8"),
    ]);
    let (status, _, stderr) = run_sim(&args);
    assert!(status.success(), "{stderr}");
    let frames = fs::read_to_string(&frames_path).expect("frames");
    let truth = fs::read_to_string(&truth_path).expect("truth");
    let (max_err, fixes, saw_callsign, _) = ingest_against_truth("gru-traces", &frames, &truth);
    assert!(saw_callsign, "no callsign");
    assert_eq!(max_err.len(), 3, "{max_err:?}");
    assert_eq!(fixes.len(), 3, "{fixes:?}");
    let _ = fs::remove_file(&frames_path);
    let _ = fs::remove_file(&truth_path);
}

fn run_iq_cmd(args: &[&str]) -> (std::process::ExitStatus, Vec<u8>, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_trck"))
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn iq");
    let stdout = collect_stdout(child.stdout.take().expect("stdout"));
    let stderr = collect_stdout(child.stderr.take().expect("stderr"));
    let status = wait_exit(&mut child, Duration::from_secs(60));
    std::thread::sleep(Duration::from_millis(50));
    let out = stdout.lock().expect("stdout").clone();
    let err = String::from_utf8_lossy(&stderr.lock().expect("stderr")).into_owned();
    (status, out, err)
}

fn run_iq_file(
    path: &Path,
    rate: u32,
    table: &Path,
) -> (std::process::ExitStatus, Vec<u8>, String) {
    let rate = rate.to_string();
    run_iq_cmd(&[
        "--iq",
        path.to_str().expect("utf8"),
        "--iq-rate",
        &rate,
        "--uart",
        "-",
        "--table",
        table.to_str().expect("utf8"),
    ])
}

fn iq_count(stderr: &str, name: &str) -> u64 {
    let line = stderr
        .lines()
        .rev()
        .find(|line| line.starts_with("iq messages="))
        .unwrap_or(stderr);
    let prefix = format!("{name}=");
    for part in line.split_whitespace() {
        if let Some(value) = part.strip_prefix(&prefix) {
            return value
                .parse()
                .unwrap_or_else(|_| panic!("{name} in {stderr}"));
        }
    }
    panic!("missing {name}= in {stderr}");
}

fn assert_uart_matches_truth(uart: &[u8], truth_text: &str) {
    let truth = parse_truth(truth_text);
    let mut poses: BTreeMap<u32, Vec<(f64, f64, i32)>> = BTreeMap::new();
    for ((_, icao), pose) in &truth {
        poses.entry(*icao).or_default().push(*pose);
    }
    let frames = uart.as_chunks::<FRAME_LEN>().0;
    assert!(!frames.is_empty(), "no UART frames");
    let mut seen: BTreeMap<u32, usize> = BTreeMap::new();
    for frame in frames {
        let fix = unpack_frame(frame).expect("uart");
        let icao = fix.aircraft.icao.to_u24();
        let lat = f64::from(fix.aircraft.latitude_e7) / 10_000_000.0;
        let lon = f64::from(fix.aircraft.longitude_e7) / 10_000_000.0;
        let Some(list) = poses.get(&icao) else {
            panic!("unexpected icao {icao:06x}");
        };
        let err = list
            .iter()
            .map(|&(tlat, tlon, _)| haversine_m(tlat, tlon, lat, lon))
            .fold(f64::INFINITY, f64::min);
        assert!(
            err < POS_TOL_M,
            "icao={icao:06x} err={err} m lat={lat} lon={lon}"
        );
        *seen.entry(icao).or_insert(0) += 1;
    }
    for icao in poses.keys() {
        assert!(
            seen.get(icao).copied().unwrap_or(0) > 0,
            "no IQ fixes for {icao:06x}"
        );
    }
}

#[test]
fn iq_help_lists_the_knobs() {
    let out = Command::new(env!("CARGO_BIN_EXE_trck"))
        .arg("--help")
        .output()
        .expect("help");
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("--iq"), "{text}");
    assert!(text.contains("--iq-rate"), "{text}");
}

#[test]
fn iq_crossing_matches_truth() {
    let iq = unique_path("cross.cu8");
    let frames_path = unique_path("frames.csv");
    let truth_path = unique_path("truth.csv");
    let table = table_arg();
    let (status, _, stderr) = run_sim(&[
        CROSSING,
        "--out",
        frames_path.to_str().expect("utf8"),
        "--truth",
        truth_path.to_str().expect("utf8"),
        "--iq-out",
        iq.to_str().expect("utf8"),
    ]);
    assert!(status.success(), "{stderr}");
    let (status, stdout, stderr) = run_iq_file(&iq, 2_400_000, &table);
    assert!(status.success(), "{stderr}");
    let truth = fs::read_to_string(&truth_path).expect("truth");
    assert_uart_matches_truth(&stdout, &truth);
    let messages = iq_count(&stderr, "messages");
    let generated = fs::read_to_string(&frames_path)
        .expect("frames")
        .lines()
        .filter(|l| !l.is_empty())
        .count() as u64;
    assert!(
        messages >= generated,
        "messages={messages} generated={generated} {stderr}"
    );
    let _ = fs::remove_file(&iq);
    let _ = fs::remove_file(&frames_path);
    let _ = fs::remove_file(&truth_path);
    let _ = fs::remove_file(&table);
}

#[test]
fn iq_decode_rate_vs_snr() {
    let frames_path = unique_path("snr-frames.csv");
    let (status, _, stderr) = run_sim(&[CROSSING, "--out", frames_path.to_str().expect("utf8")]);
    assert!(status.success(), "{stderr}");
    let generated = fs::read_to_string(&frames_path)
        .expect("frames")
        .lines()
        .filter(|l| !l.is_empty())
        .count() as u64;
    assert!(generated > 0);
    // SNR = 10 log10(A² / (2σ²)): on-chip pulse power over complex AWGN (σ on each of I and Q).
    eprintln!(
        "SNR = 10 log10(A^2 / (2 sigma^2)): on-chip pulse power over complex AWGN (sigma on I and on Q)"
    );
    let mut rows = Vec::new();
    for snr in [20.0, 15.0, 12.0, 10.0, 8.0, 6.0] {
        let iq = unique_path(&format!("snr-{snr}.cu8"));
        let table = table_arg();
        let snr_s = format!("{snr}");
        let (status, _, stderr) = run_sim(&[
            CROSSING,
            "--out",
            frames_path.to_str().expect("utf8"),
            "--iq-out",
            iq.to_str().expect("utf8"),
            "--iq-snr",
            &snr_s,
            "--iq-seed",
            "1",
        ]);
        assert!(status.success(), "{stderr}");
        let (status, _, stderr) = run_iq_file(&iq, 2_400_000, &table);
        assert!(status.success(), "{stderr}");
        let messages = iq_count(&stderr, "messages");
        let rate = messages as f64 / generated as f64;
        eprintln!("iq snr={snr} dB messages={messages}/{generated} rate={rate:.3}");
        rows.push((snr, messages, rate));
        let _ = fs::remove_file(&iq);
        let _ = fs::remove_file(&table);
    }
    let rate_at = |db: f64| {
        rows.iter()
            .find(|(snr, _, _)| (*snr - db).abs() < 0.1)
            .map(|(_, _, r)| *r)
            .unwrap_or(0.0)
    };
    assert!(
        rate_at(20.0) >= 0.9,
        "20 dB decode rate {} is below 0.9: {rows:?}",
        rate_at(20.0)
    );
    // rs1090's dump1090-2400 needs a perfect 112-bit CRC (no 1-bit correction).
    // That cliff is ~12 dB for this generator; 10 dB still finds the preamble.
    assert!(
        rate_at(12.0) >= 0.75,
        "12 dB decode rate {} is below 0.75: {rows:?}",
        rate_at(12.0)
    );
    assert!(
        rate_at(10.0) >= 0.2,
        "10 dB decode rate {} collapsed (want this demod's ~0.3 floor, not 0): {rows:?}",
        rate_at(10.0)
    );
    let _ = fs::remove_file(&frames_path);
}

#[test]
fn iq_2msps_still_decodes() {
    let iq = unique_path("2msps.cu8");
    let frames_path = unique_path("2msps-frames.csv");
    let table = table_arg();
    let (status, _, stderr) = run_sim(&[
        CROSSING,
        "--iq-out",
        iq.to_str().expect("utf8"),
        "--iq-rate",
        "2000000",
        "--out",
        frames_path.to_str().expect("utf8"),
    ]);
    assert!(status.success(), "{stderr}");
    let (status, stdout, stderr) = run_iq_file(&iq, 2_000_000, &table);
    assert!(status.success(), "{stderr}");
    assert!(iq_count(&stderr, "messages") > 0, "{stderr}");
    assert!(iq_count(&stderr, "fixes") > 0, "{stderr}");
    assert!(stdout.windows(4).any(|w| w == b"TRCK"), "{stderr}");
    let _ = fs::remove_file(&iq);
    let _ = fs::remove_file(&frames_path);
    let _ = fs::remove_file(&table);
}

fn iq_data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

fn skip_missing(path: &Path) -> bool {
    if path.is_file() {
        return false;
    }
    eprintln!("skip {}: run scripts/fetch-iq.sh", path.display());
    true
}

fn find_under(dir: &Path, name: &str) -> Option<PathBuf> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(cur) = stack.pop() {
        let Ok(entries) = fs::read_dir(&cur) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n == "__MACOSX") {
                    continue;
                }
                stack.push(path);
            } else if path.file_name().is_some_and(|n| n == name) {
                return Some(path);
            }
        }
    }
    None
}

fn iq_msg_hex(stderr: &str) -> Vec<String> {
    iq_msgs(stderr).into_iter().map(|(hex, _)| hex).collect()
}

fn iq_msgs(stderr: &str) -> Vec<(String, f64)> {
    stderr
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("msg ")?;
            let (hex, t) = rest.split_once(" t=")?;
            Some((hex.trim().to_ascii_uppercase(), t.parse().ok()?))
        })
        .collect()
}

fn csv_col(header: &[&str], name: &str) -> Option<usize> {
    header.iter().position(|h| h.eq_ignore_ascii_case(name))
}

fn parse_csv_table(text: &str) -> (Vec<String>, Vec<Vec<String>>) {
    let mut lines = text.lines().filter(|l| !l.is_empty());
    let header: Vec<String> = lines
        .next()
        .unwrap_or("")
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();
    let rows = lines
        .map(|line| {
            line.split(',')
                .map(|s| s.trim().to_string())
                .collect::<Vec<_>>()
        })
        .collect();
    (header, rows)
}

fn hex_norm(text: &str) -> String {
    text.trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X")
        .trim_end_matches(';')
        .to_ascii_uppercase()
}

#[test]
#[ignore]
fn iq_modes1_recording() {
    let path = iq_data("modes1.bin");
    if skip_missing(&path) {
        return;
    }
    assert_eq!(fs::metadata(&path).expect("meta").len(), 713_736);
    let table = table_arg();
    let (status, _, stderr) = run_iq_file(&path, 2_000_000, &table);
    assert!(status.success(), "{stderr}");
    let messages = iq_count(&stderr, "messages");
    eprintln!("iq modes1.bin messages={messages}");
    assert!(
        messages >= 20,
        "modes1.bin decoded {messages} frames. {stderr}"
    );
    let _ = fs::remove_file(&table);
}

#[test]
#[ignore]
fn iq_awec2026_recording() {
    let dir = iq_data("awec2026");
    if !dir.is_dir() {
        eprintln!("skip {}: run scripts/fetch-iq.sh", dir.display());
        return;
    }
    let recording = find_under(&dir, "recording.bin").expect("recording.bin");
    let msg_csv = find_under(&dir, "messages.csv").expect("messages.csv");
    let pos_csv = find_under(&dir, "positions.csv").expect("positions.csv");
    let table = table_arg();
    let (status, stdout, stderr) = run_iq_file(&recording, 2_000_000, &table);
    assert!(status.success(), "{stderr}");
    let got_hex: std::collections::HashSet<String> = iq_msg_hex(&stderr).into_iter().collect();
    let msg_text = fs::read_to_string(&msg_csv).expect("messages csv");
    let (header, rows) = parse_csv_table(&msg_text);
    let heads: Vec<&str> = header.iter().map(String::as_str).collect();
    let hex_i = csv_col(&heads, "message_hex")
        .or_else(|| csv_col(&heads, "hex"))
        .unwrap_or_else(|| panic!("hex column in {header:?}"));
    let expect_hex: Vec<String> = rows
        .iter()
        .filter_map(|row| row.get(hex_i).map(|s| hex_norm(s)))
        .filter(|s| s.len() >= 14)
        .collect();
    let matched = expect_hex
        .iter()
        .filter(|h| {
            got_hex
                .iter()
                .any(|g| g.starts_with(h.as_str()) || h.starts_with(g.as_str()))
        })
        .count();
    eprintln!(
        "iq awec2026 messages matched {matched}/{} (we decoded {})",
        expect_hex.len(),
        got_hex.len()
    );
    assert_eq!(expect_hex.len(), 122, "expected 122 decoded message rows");
    assert!(
        matched >= 40,
        "recovered {matched}/122 messages; got {}",
        got_hex.len()
    );

    let pos_text = fs::read_to_string(&pos_csv).expect("positions csv");
    let (pheader, prows) = parse_csv_table(&pos_text);
    let pheads: Vec<&str> = pheader.iter().map(String::as_str).collect();
    let lat_i = csv_col(&pheads, "latitude_deg")
        .or_else(|| csv_col(&pheads, "lat"))
        .expect("lat");
    let lon_i = csv_col(&pheads, "longitude_deg")
        .or_else(|| csv_col(&pheads, "lon"))
        .expect("lon");
    let hex_p = csv_col(&pheads, "message_hex").expect("pos hex");
    let t_i = csv_col(&pheads, "time_s").expect("time_s");
    let iv_i = csv_col(&pheads, "cpr_pair_interval_s");
    assert_eq!(prows.len(), 10, "expected 10 position points");

    let have_hex = |hex: &str| {
        got_hex
            .iter()
            .any(|g| g.starts_with(hex) || hex.starts_with(g.as_str()))
    };
    let mut decoded_pos = 0usize;
    for row in &prows {
        let hex = hex_norm(&row[hex_p]);
        let t: f64 = row[t_i].parse().expect("t");
        let have = have_hex(&hex);
        if have {
            decoded_pos += 1;
        }
        let iv = iv_i
            .and_then(|i| row.get(i))
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);
        eprintln!("iq awec2026 pos t={t:.3} interval={iv:.2}s decoded={have} {hex}");
    }
    eprintln!("iq awec2026 position frames decoded {decoded_pos}/10");

    // CSV lists a lat/lon on every airborne CPR frame, pairing with the other
    // parity even if it is still in the future (intervals 5–8 s on the first
    // four evens). rs1090 only solves when both parities have already arrived,
    // so those four rows are not decoder misses.
    let mut csv_tracker = Tracker::new();
    let mut csv_solves = 0usize;
    let mut csv_vs_row = Vec::new();
    for row in &prows {
        let hex = hex_norm(&row[hex_p]);
        let t: f64 = row[t_i].parse().expect("t");
        let frame = parse_hex_frame(hex.as_bytes()).expect("pos frame");
        if let Some(fix) = csv_tracker.ingest(t, &frame) {
            csv_solves += 1;
            let lat = f64::from(fix.aircraft.latitude_e7) / 10_000_000.0;
            let lon = f64::from(fix.aircraft.longitude_e7) / 10_000_000.0;
            let csv_lat: f64 = row[lat_i].parse().expect("lat");
            let csv_lon: f64 = row[lon_i].parse().expect("lon");
            let vs = haversine_m(csv_lat, csv_lon, lat, lon);
            csv_vs_row.push(vs);
            eprintln!("iq awec2026 CSV row at t={t:.3} vs rs1090(all 10) {vs:.1} m");
        }
    }
    eprintln!(
        "iq awec2026 rs1090 solves {csv_solves}/10 CSV frames (prefix evens stay unpaired); vs-row m {csv_vs_row:?}"
    );

    let mut decoded_msgs = iq_msgs(&stderr);
    decoded_msgs.sort_by(|a, b| a.1.total_cmp(&b.1));
    let mut live = Tracker::new();
    let mut ref_fixes = Vec::new();
    for (hex, t) in &decoded_msgs {
        let Some(frame) = parse_hex_frame(hex.as_bytes()) else {
            continue;
        };
        if let Some(fix) = live.ingest(*t, &frame) {
            let lat = f64::from(fix.aircraft.latitude_e7) / 10_000_000.0;
            let lon = f64::from(fix.aircraft.longitude_e7) / 10_000_000.0;
            ref_fixes.push((lat, lon));
        }
    }
    let frames = stdout.as_chunks::<FRAME_LEN>().0;
    assert_eq!(
        frames.len(),
        ref_fixes.len(),
        "UART fixes {} vs rs1090 pairs {} from decoded IQ messages",
        frames.len(),
        ref_fixes.len()
    );
    let mut max_err = 0.0f64;
    for (frame, &(rlat, rlon)) in frames.iter().zip(ref_fixes.iter()) {
        let fix = unpack_frame(frame).expect("uart");
        let lat = f64::from(fix.aircraft.latitude_e7) / 10_000_000.0;
        let lon = f64::from(fix.aircraft.longitude_e7) / 10_000_000.0;
        let err = haversine_m(rlat, rlon, lat, lon);
        if err > max_err {
            max_err = err;
        }
    }
    eprintln!(
        "iq awec2026 UART {} fixes match the live CPR path; max {max_err:.2} m",
        frames.len()
    );
    assert!(
        max_err < POS_TOL_M,
        "IQ positions drifted from the same CPR pairs: {max_err} m"
    );
    let _ = fs::remove_file(&table);
}

fn summary_count(stderr: &str, name: &str) -> u64 {
    let line = stderr
        .lines()
        .rev()
        .find(|line| line.starts_with("stress messages="))
        .unwrap_or(stderr);
    let prefix = format!("{name}=");
    for part in line.split_whitespace() {
        if let Some(value) = part.strip_prefix(&prefix) {
            return value
                .parse()
                .unwrap_or_else(|_| panic!("{name} in {stderr}"));
        }
    }
    panic!("missing {name}= in {stderr}");
}

#[test]
fn stress_drops_on_a_full_tty_and_still_writes_csv() {
    let table = table_arg();
    let pty = Pty::open();
    let mut child = Command::new(env!("CARGO_BIN_EXE_trck"))
        .args([
            "stress",
            "--uart",
            &pty.path(),
            "--table",
            table.to_str().expect("utf8"),
            "--messages",
            "200",
            "--aircraft",
            "4",
            "--rate",
            "40",
            "--emergency",
            "0.5",
            "--ident",
            "0",
            "--velocity",
            "0",
            "--junk",
            "0",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn stress");
    let stderr_buf = collect_stdout(child.stderr.take().expect("stderr"));
    std::thread::sleep(Duration::from_millis(200));
    let fd = pty.slave.as_raw_fd();
    let filling = Arc::new(AtomicBool::new(true));
    let fill_flag = Arc::clone(&filling);
    let filler = std::thread::spawn(move || {
        while fill_flag.load(Ordering::Relaxed) {
            write_until_would_block(fd);
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    let status = wait_exit(&mut child, Duration::from_secs(15));
    filling.store(false, Ordering::Relaxed);
    let _ = filler.join();
    std::thread::sleep(Duration::from_millis(50));
    let stderr = String::from_utf8_lossy(&stderr_buf.lock().expect("stderr")).into_owned();
    assert!(status.success(), "{stderr}");
    let fixes = summary_count(&stderr, "fixes");
    let dropped = summary_count(&stderr, "dropped");
    let csv = summary_count(&stderr, "csv");
    assert!(fixes > QUEUE as u64, "fixes={fixes} {stderr}");
    assert!(dropped > 0, "no drops {stderr}");
    let rows = wait_csv_rows(&table, 1, Duration::from_secs(1));
    assert_eq!(csv, fixes);
    assert_eq!(rows.len(), fixes as usize);
    let _ = fs::remove_file(&table);
}

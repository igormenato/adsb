#![allow(dead_code)]

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::os::fd::{AsRawFd, BorrowedFd, RawFd};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use nix::errno::Errno;
use nix::fcntl::{FcntlArg, FlockArg, OFlag, fcntl};
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::unistd::{self, read};
use rs1090::decode::cpr::{Position, distance_nm};
use serialport::{SerialPort, TTYPort};
use trck::modes::{EVEN, ODD, STATUS, hex14};
use trck::{Aircraft, EmergencyKind, FRAME_LEN, Fix, Tracker, parse_hex_frame, unpack_frame};

nix::ioctl_none!(tiocnxcl, b'T', 0x0D);
pub const ORDINARY_A: &str = "8d4d224f58bf07c2d41a9a353d70";
pub const ORDINARY_B: &str = "8d4d224f58bf003b221b34aa5b8d";
pub const ICAO_40058B: u32 = 0x40058B;
pub const ICAO_4D224F: u32 = 0x4D224F;
pub const QUEUE: usize = 64;

pub const PUBLISHED: [u8; FRAME_LEN] = [
    0x54, 0x52, 0x43, 0x4b, 0x1d, 0x62, 0x40, 0x42, 0x41, 0x57, 0x31, 0x32, 0x33, 0x20, 0x20, 0xf5,
    0xd0, 0x25, 0x1f, 0x7e, 0x0c, 0x56, 0x02, 0x70, 0x94, 0x00, 0x00, 0xc2, 0x01, 0x0f, 0x01, 0x00,
    0x60, 0xef,
];

pub const SECOND: [u8; FRAME_LEN] = [
    0x54, 0x52, 0x43, 0x4b, 0xbc, 0x0a, 0x40, 0x53, 0x49, 0x4d, 0x30, 0x30, 0x30, 0x31, 0x20, 0x40,
    0x2c, 0x42, 0x06, 0x60, 0x18, 0xee, 0xf3, 0xe0, 0x2e, 0x00, 0x00, 0x18, 0x01, 0x5a, 0x00, 0x00,
    0x3e, 0x4c,
];

pub const CHECK_EMERGENCY: [u8; FRAME_LEN] = [
    0x54, 0x52, 0x43, 0x4b, 0x23, 0xa1, 0x4c, 0x45, 0x5a, 0x59, 0x34, 0x35, 0x36, 0x20, 0x20, 0xe0,
    0xb2, 0xad, 0x1e, 0xe0, 0x55, 0xbb, 0xff, 0x40, 0x1f, 0x00, 0x00, 0xdc, 0x00, 0xb4, 0x00, 0x01,
    0xaa, 0x98,
];

pub const PUBLISHED_GENERAL: [u8; FRAME_LEN] = [
    0x54, 0x52, 0x43, 0x4b, 0x1d, 0x62, 0x40, 0x42, 0x41, 0x57, 0x31, 0x32, 0x33, 0x20, 0x20, 0xf5,
    0xd0, 0x25, 0x1f, 0x7e, 0x0c, 0x56, 0x02, 0x70, 0x94, 0x00, 0x00, 0xc2, 0x01, 0x0f, 0x01, 0x01,
    0x41, 0xff,
];

pub fn decoded_aircraft() -> Aircraft {
    let mut tracker = Tracker::new();
    assert!(tracker.ingest(1.0, &EVEN).is_none());
    tracker.ingest(2.0, &ODD).expect("position").aircraft
}

pub fn ordinary_aircraft() -> Aircraft {
    let mut tracker = Tracker::new();
    let first = parse_hex_frame(ORDINARY_A.as_bytes()).expect("ordinary a");
    let second = parse_hex_frame(ORDINARY_B.as_bytes()).expect("ordinary b");
    assert!(tracker.ingest(1.0, &first).is_none());
    tracker
        .ingest(2.0, &second)
        .expect("ordinary position")
        .aircraft
}

pub fn frame_aircraft(frame: &[u8; FRAME_LEN]) -> Aircraft {
    unpack_frame(frame).expect("frame").aircraft
}

pub struct StopSender(pub std::process::Child);

impl Drop for StopSender {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

pub fn unique_path(name: &str) -> PathBuf {
    static N: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "trck-{}-{}-{name}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ))
}

pub fn table_arg() -> PathBuf {
    unique_path("aircraft.csv")
}

pub fn spawn_trck(args: &[&str]) -> StopSender {
    let child = Command::new(env!("CARGO_BIN_EXE_trck"))
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn sender");
    StopSender(child)
}

pub fn spawn_hex(port: u16) -> StopSender {
    spawn_hex_table(port, &table_arg())
}

pub fn spawn_hex_table(port: u16, table: &Path) -> StopSender {
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

pub fn spawn_check() -> StopSender {
    spawn_check_table(&table_arg())
}

pub fn spawn_check_table(table: &Path) -> StopSender {
    spawn_trck(&[
        "--check",
        "--uart",
        "-",
        "--table",
        table.to_str().expect("utf8"),
    ])
}

pub fn spawn_uart(port: u16, uart: &str) -> StopSender {
    spawn_uart_table(port, uart, &table_arg())
}

pub fn spawn_uart_table(port: u16, uart: &str, table: &Path) -> StopSender {
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

pub fn collect_stdout(stdout: impl Read + Send + 'static) -> Arc<Mutex<Vec<u8>>> {
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

pub fn wait_len(buf: &Mutex<Vec<u8>>, n: usize, timeout: Duration) -> Vec<u8> {
    let start = std::time::Instant::now();
    loop {
        let got = buf.lock().expect("lock").clone();
        if got.len() >= n || start.elapsed() > timeout {
            return got;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

pub fn listen() -> (TcpListener, u16) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    (listener, port)
}

pub enum Session {
    CloseAndAccept,
    KeepOpen,
}

pub fn serve(
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

pub fn write_pair(sock: &mut std::net::TcpStream) {
    writeln!(sock, "{}", hex14(&EVEN)).expect("even");
    writeln!(sock, "{}", hex14(&ODD)).expect("odd");
}
pub struct Pty {
    pub master: TTYPort,
    pub slave: TTYPort,
}

impl Pty {
    pub fn open() -> Self {
        let (master, slave) = TTYPort::pair().expect("pty");
        let slave_fd = slave.as_raw_fd();
        // SAFETY: `slave_fd` is the open pty slave this `Pty` owns. TIOCNXCL takes no pointer.
        let _ = unsafe { tiocnxcl(slave_fd) };
        #[allow(deprecated)]
        let _ = nix::fcntl::flock(slave_fd, FlockArg::Unlock);
        set_nonblocking(&slave);
        Self { master, slave }
    }

    pub fn path(&self) -> String {
        self.slave.name().expect("pty name")
    }
}

pub fn frame_icao(frame: &[u8]) -> u32 {
    u32::from_le_bytes([frame[4], frame[5], frame[6], 0])
}

pub fn frame_is_general(frame: &[u8]) -> bool {
    unpack_frame(frame).is_some_and(|fix| fix.aircraft.emergency == Some(EmergencyKind::General))
}

pub fn as_fd(fd: &impl AsRawFd) -> BorrowedFd<'_> {
    // SAFETY: `fd` keeps the descriptor open for this borrow.
    unsafe { BorrowedFd::borrow_raw(fd.as_raw_fd()) }
}

pub fn set_nonblocking(fd: &impl AsRawFd) {
    let fd = as_fd(fd);
    let flags = OFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFL).expect("F_GETFL"));
    fcntl(fd, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK)).expect("F_SETFL");
}

pub fn write_until_would_block(fd: RawFd) {
    // SAFETY: the worker holds this fd until `pty` is dropped after join.
    let fd = unsafe { BorrowedFd::borrow_raw(fd) };
    let buf = [0xA5u8; 1024];
    let mut chunk = buf.len();
    loop {
        match unistd::write(fd, &buf[..chunk]) {
            Ok(0) => return,
            Ok(_) => chunk = buf.len(),
            Err(err) => {
                let err = io::Error::from(err);
                assert_eq!(err.kind(), io::ErrorKind::WouldBlock, "{err}");
                if chunk == 1 {
                    return;
                }
                chunk = 1;
            }
        }
    }
}

pub fn fill_until_blocked(fd: &impl AsRawFd) -> usize {
    let fd = as_fd(fd);
    let buf = [0xA5u8; 1024];
    let mut wrote = 0usize;
    let mut chunk = buf.len();
    loop {
        match unistd::write(fd, &buf[..chunk]) {
            Ok(n) => {
                assert!(n > 0);
                wrote += n;
                assert!(
                    wrote < 1024 * 1024,
                    "pty accepted a megabyte without blocking"
                );
            }
            Err(err) => {
                let err = io::Error::from(err);
                assert_eq!(err.kind(), io::ErrorKind::WouldBlock, "{err}");
                if chunk == 1 {
                    break;
                }
                chunk = 1;
            }
        }
    }
    assert!(wrote > 0, "pty buffer was already full");
    wrote
}

pub fn complete_frames(raw: &[u8]) -> Vec<[u8; FRAME_LEN]> {
    let Some(pos) = raw.windows(4).position(|window| window == b"TRCK") else {
        return Vec::new();
    };
    let (frames, _) = raw[pos..].as_chunks::<FRAME_LEN>();
    frames.to_vec()
}

pub fn wait_frames_until(
    master: &mut TTYPort,
    timeout: Duration,
    ready: impl Fn(&[[u8; FRAME_LEN]]) -> bool,
) -> Vec<[u8; FRAME_LEN]> {
    set_nonblocking(master);
    let mut raw = Vec::new();
    let started = Instant::now();
    loop {
        let mut tmp = [0u8; 4096];
        match read(as_fd(master), &mut tmp) {
            Ok(0) => {}
            Ok(n) => {
                raw.extend_from_slice(&tmp[..n]);
                let frames = complete_frames(&raw);
                if ready(&frames) {
                    return frames;
                }
                continue;
            }
            Err(err) => {
                let err = io::Error::from(err);
                assert!(
                    err.kind() == io::ErrorKind::WouldBlock
                        || err.kind() == io::ErrorKind::Interrupted,
                    "{err}"
                );
            }
        }
        if started.elapsed() > timeout {
            panic!("got {} bytes", raw.len());
        }
        let left = timeout.saturating_sub(started.elapsed());
        let mut poll_fd = [PollFd::new(as_fd(master), PollFlags::POLLIN)];
        if let Err(err) = poll(
            &mut poll_fd,
            PollTimeout::try_from(left).unwrap_or(PollTimeout::MAX),
        ) {
            assert_eq!(err, Errno::EINTR, "{err}");
        }
    }
}

pub fn wait_frames(master: &mut TTYPort, n: usize, timeout: Duration) -> Vec<[u8; FRAME_LEN]> {
    let mut frames = wait_frames_until(master, timeout, |frames| frames.len() >= n);
    frames.truncate(n);
    frames
}

pub fn spawn_hex_stdin(uart: &str) -> StopSender {
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

pub fn decoded_7700_aircraft() -> Aircraft {
    let mut tracker = Tracker::new();
    assert!(tracker.ingest(1.0, &STATUS).is_none());
    assert!(tracker.ingest(2.0, &EVEN).is_none());
    tracker.ingest(3.0, &ODD).expect("position").aircraft
}
pub fn wait_csv_rows(path: &Path, n: usize, timeout: Duration) -> Vec<String> {
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

pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn assert_csv_row(row: &str, fix: &Fix, before: u64, after: u64) {
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
pub fn wait_exit(child: &mut std::process::Child, timeout: Duration) -> std::process::ExitStatus {
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

pub const CROSSING: &str = "examples/scenarios/crossing.toml";
pub const CLIMB: &str = "examples/scenarios/climb.toml";
pub const GRU_TRACES: &[&str] = &[
    "examples/traces/trace_full_e4917f.json",
    "examples/traces/trace_full_e49450.json",
    "examples/traces/trace_full_e49fb4.json",
];
pub const POS_TOL_M: f64 = 10.0;

pub fn run_sim(args: &[&str]) -> (std::process::ExitStatus, String, String) {
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

pub fn distance_m(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    distance_nm(
        &Position {
            latitude: lat1,
            longitude: lon1,
        },
        &Position {
            latitude: lat2,
            longitude: lon2,
        },
    ) * 1852.0
}

pub fn parse_truth(text: &str) -> BTreeMap<(i64, u32), (f64, f64, i32)> {
    let mut rows = BTreeMap::new();
    let mut reader = csv::Reader::from_reader(text.as_bytes());
    for result in reader.records() {
        let cols = result.expect("truth csv");
        assert_eq!(cols.len(), 5, "{cols:?}");
        let ts_ms = (cols[0].parse::<f64>().expect("ts") * 1000.0).round() as i64;
        let icao = u32::from_str_radix(&cols[1], 16).expect("icao");
        let lat: f64 = cols[2].parse().expect("lat");
        let lon: f64 = cols[3].parse().expect("lon");
        let alt: i32 = cols[4].parse().expect("alt");
        rows.insert((ts_ms, icao), (lat, lon, alt));
    }
    rows
}

pub fn ingest_against_truth(
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
        let err = distance_m(lat, lon, got_lat, got_lon);
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

pub fn assert_sim_matches_truth(scenario: &str, expect_7700: bool) -> BTreeMap<u32, f64> {
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

use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::os::fd::AsRawFd;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serialport::{SerialPort, TTYPort};

use trck::{
    Aircraft, FRAME_LEN, Tracker, check_positions, crc16_ccitt_false, pack_frame, parse_hex_frame,
    unpack_frame,
};

const EVEN: &str = "8D40058B58C901375147EFD09357";
const ODD: &str = "8D40058B58C904A87F402D3B8C59";

const PUBLISHED: [u8; FRAME_LEN] = [
    0x54, 0x52, 0x43, 0x4b, 0x00, 0xf1, 0x53, 0x65, 0x1d, 0x62, 0x40, 0x42, 0x41, 0x57, 0x31, 0x32,
    0x33, 0x20, 0x20, 0xf5, 0xd0, 0x25, 0x1f, 0x7e, 0x0c, 0x56, 0x02, 0x70, 0x94, 0x00, 0x00, 0xc2,
    0x01, 0x0f, 0x01, 0x00, 0x06, 0xbe,
];

const SECOND: [u8; FRAME_LEN] = [
    0x54, 0x52, 0x43, 0x4b, 0x00, 0xf1, 0x53, 0x65, 0xbc, 0x0a, 0x40, 0x53, 0x49, 0x4d, 0x30, 0x30,
    0x30, 0x31, 0x20, 0x40, 0x2c, 0x42, 0x06, 0x60, 0x18, 0xee, 0xf3, 0xe0, 0x2e, 0x00, 0x00, 0x18,
    0x01, 0x5a, 0x00, 0x00, 0x58, 0x1d,
];

const CHECK_EMERGENCY: [u8; FRAME_LEN] = [
    0x54, 0x52, 0x43, 0x4b, 0x00, 0xf1, 0x53, 0x65, 0x23, 0xa1, 0x4c, 0x45, 0x5a, 0x59, 0x34, 0x35,
    0x36, 0x20, 0x20, 0xe0, 0xb2, 0xad, 0x1e, 0xe0, 0x55, 0xbb, 0xff, 0x40, 0x1f, 0x00, 0x00, 0xdc,
    0x00, 0xb4, 0x00, 0x01, 0xcc, 0xc9,
];

fn decoded_aircraft() -> Aircraft {
    let mut tracker = Tracker::new();
    let even = parse_hex_frame(EVEN.as_bytes()).expect("even");
    let odd = parse_hex_frame(ODD.as_bytes()).expect("odd");
    assert!(tracker.ingest(1.0, &even).is_none());
    tracker.ingest(2.0, &odd).expect("position").aircraft
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
    bad_crc[36] ^= 0xFF;
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
}

struct StopSender(std::process::Child);

impl Drop for StopSender {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_hex(port: u16) -> StopSender {
    let addr = format!("127.0.0.1:{port}");
    let child = Command::new(env!("CARGO_BIN_EXE_trck"))
        .args(["--hex", &addr, "--uart", "-"])
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

fn spawn_uart(port: u16, uart: &str) -> StopSender {
    let addr = format!("127.0.0.1:{port}");
    let child = Command::new(env!("CARGO_BIN_EXE_trck"))
        .args(["--hex", &addr, "--uart", uart])
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

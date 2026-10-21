//! Read readsb `--net-json-port` and write one UART frame per position.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::os::fd::{AsRawFd, FromRawFd};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use clap::Parser;
use serialport::{ClearBuffer, DataBits, FlowControl, Parity, SerialPort, StopBits, TTYPort};

use trck::{check_frames, check_positions, fix_from_json_line, pack_frame};

const DEFAULT_UART: &str = "/dev/serial0";
const DEFAULT_BAUD: u32 = 115_200;
const DEFAULT_INTERVAL_S: f64 = 1.0;
const POLL_SLICE: Duration = Duration::from_millis(200);
const CONNECT_SLICE: Duration = Duration::from_millis(50);
/// Finished frames waiting on the UART. The frame being written sits beside this.
const QUEUE_CAP: usize = 64;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const RETRY_MIN: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(30);
const KEEPALIVE_IDLE_S: u32 = 30;
const KEEPALIVE_INTERVAL_S: u32 = 10;
const KEEPALIVE_PROBES: u32 = 3;
/// readsb builds one JSON line in an 8192-byte buffer and drops what does not fit.
const MAX_LINE: usize = 8192;

#[derive(Parser, Debug)]
#[command(about = "Send each readsb position to a UART as it arrives.")]
struct Args {
    /// Host and port of readsb `--net-json-port`. Required unless `--check`.
    #[arg(
        long,
        required_unless_present = "check",
        conflicts_with = "check",
        value_name = "HOST:PORT"
    )]
    net_json: Option<String>,
    /// UART device, or - for stdout.
    #[arg(long, default_value = DEFAULT_UART)]
    uart: String,
    #[arg(long, default_value_t = DEFAULT_BAUD)]
    baud: u32,
    /// Seconds between passes of `--check`. The JSON port does not wait.
    #[arg(long, default_value_t = DEFAULT_INTERVAL_S)]
    interval: f64,
    /// Send the known frames instead of reading readsb, to check the UART.
    #[arg(long, conflicts_with = "net_json")]
    check: bool,
}

#[derive(Debug)]
enum ChunkError {
    Stopped,
    Failed(io::Error),
}

#[derive(Debug)]
enum Pump {
    Stopped,
    Disconnected,
    Uart(io::Error),
}

enum Sink {
    Uart(TTYPort),
    Stdout,
}

struct Writer {
    sink: Sink,
    stop: Arc<AtomicBool>,
    queue: VecDeque<[u8; trck::FRAME_LEN]>,
    /// Remainder of a short write. `sent` bytes of `frame` are already in the kernel.
    current: Option<([u8; trck::FRAME_LEN], usize)>,
}

impl Writer {
    fn has_pending(&self) -> bool {
        self.current.is_some() || !self.queue.is_empty()
    }

    fn uart_fd(&self) -> Option<i32> {
        match &self.sink {
            Sink::Uart(port) => Some(port.as_raw_fd()),
            Sink::Stdout => None,
        }
    }

    fn enqueue(&mut self, frame: [u8; trck::FRAME_LEN]) {
        if self.queue.len() < QUEUE_CAP {
            self.queue.push_back(frame);
            return;
        }
        if let Some(index) = self.queue.iter().position(|queued| queued[27] == 0) {
            let dropped = self.queue.remove(index).expect("ordinary frame");
            log_drop(&dropped);
            self.queue.push_back(frame);
            return;
        }
        if frame[27] == 0 {
            log_drop(&frame);
            return;
        }
        let dropped = self.queue.pop_front().expect("full queue");
        log_drop(&dropped);
        self.queue.push_back(frame);
    }

    /// Write until the port would block or the queue is empty. A short write stays in `current`.
    fn write_some(&mut self) -> Result<(), ChunkError> {
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return Err(ChunkError::Stopped);
            }
            if self.current.is_none() {
                let Some(frame) = self.queue.pop_front() else {
                    return Ok(());
                };
                self.current = Some((frame, 0));
            }
            let (frame, sent) = self.current.expect("current frame");
            let rest = &frame[sent..];
            if rest.is_empty() {
                self.current = None;
                continue;
            }
            match write_sink(&mut self.sink, rest) {
                Ok(0) => {
                    return Err(ChunkError::Failed(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "uart write wrote nothing",
                    )));
                }
                Ok(n) => {
                    let sent = sent + n;
                    if sent >= frame.len() {
                        self.current = None;
                    } else {
                        self.current = Some((frame, sent));
                    }
                }
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(ChunkError::Failed(err)),
            }
        }
    }
}

fn log_drop(frame: &[u8; trck::FRAME_LEN]) {
    let icao = u32::from_le_bytes([frame[8], frame[9], frame[10], 0]);
    eprintln!("dropped frame icao={icao:06x} emergency={}", frame[27]);
}

fn write_sink(sink: &mut Sink, bytes: &[u8]) -> io::Result<usize> {
    match sink {
        Sink::Uart(port) => write_fd(port.as_raw_fd(), bytes),
        Sink::Stdout => {
            let mut out = io::stdout().lock();
            out.write_all(bytes)?;
            out.flush()?;
            Ok(bytes.len())
        }
    }
}

fn write_fd(fd: i32, bytes: &[u8]) -> io::Result<usize> {
    let rc = unsafe { libc::write(fd, bytes.as_ptr() as *const libc::c_void, bytes.len()) };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(rc as usize)
    }
}

enum Pending {
    Collecting,
    DiscardUntilNewline,
}

struct JsonLines {
    reader: TcpStream,
    pending: Vec<u8>,
    state: Pending,
}

enum Pull {
    Bytes,
    Eof,
}

impl JsonLines {
    /// Read what the kernel has. The socket is non-blocking, so this returns on `WouldBlock`.
    fn pull(&mut self) -> Result<Pull, io::Error> {
        let mut tmp = [0u8; 4096];
        loop {
            match self.reader.read(&mut tmp) {
                Ok(0) => return Ok(Pull::Eof),
                Ok(n) => self.pending.extend_from_slice(&tmp[..n]),
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(Pull::Bytes),
                Err(err) => return Err(err),
            }
        }
    }

    fn pop_line(&mut self) -> Option<Vec<u8>> {
        loop {
            let split = self.pending.iter().position(|byte| *byte == b'\n')?;
            let mut line: Vec<u8> = self.pending.drain(..=split).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            match self.state {
                Pending::DiscardUntilNewline => {
                    self.state = Pending::Collecting;
                    continue;
                }
                Pending::Collecting => return Some(line),
            }
        }
    }

    fn note_overflow(&mut self) {
        if self.pending.len() > MAX_LINE && !self.pending.contains(&b'\n') {
            self.pending.clear();
            self.state = Pending::DiscardUntilNewline;
        }
    }
}

fn set_nonblocking(fd: i32) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let rc = unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn clear_nonblocking(fd: i32) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let rc = unsafe { libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn open_failed(path: &str, err: impl std::fmt::Display) -> ExitCode {
    eprintln!("cannot open {path}: {err}");
    ExitCode::from(1)
}

fn open_uart(path: &str, baud: u32, stop: Arc<AtomicBool>) -> Result<Writer, ExitCode> {
    let port = serialport::new(path, baud)
        .data_bits(DataBits::Eight)
        .parity(Parity::None)
        .stop_bits(StopBits::One)
        .flow_control(FlowControl::None)
        .timeout(Duration::ZERO)
        .open_native()
        .map_err(|err| open_failed(path, err))?;
    port.clear(ClearBuffer::All)
        .map_err(|err| open_failed(path, err))?;
    set_nonblocking(port.as_raw_fd()).map_err(|err| open_failed(path, err))?;
    Ok(Writer {
        sink: Sink::Uart(port),
        stop,
        queue: VecDeque::new(),
        current: None,
    })
}

fn open_output(path: &str, baud: u32, stop: Arc<AtomicBool>) -> Result<Writer, ExitCode> {
    if path == "-" {
        return Ok(Writer {
            sink: Sink::Stdout,
            stop,
            queue: VecDeque::new(),
            current: None,
        });
    }
    open_uart(path, baud, stop)
}

fn resolve_net_json(text: &str) -> Option<Vec<SocketAddr>> {
    let addrs: Vec<_> = text.to_socket_addrs().ok()?.collect();
    if addrs.is_empty() { None } else { Some(addrs) }
}

fn grow(delay: Duration) -> Duration {
    delay.saturating_mul(2).min(RETRY_MAX)
}

fn main() -> ExitCode {
    let args = Args::parse();
    let stop = Arc::new(AtomicBool::new(false));
    let mut writer = match open_output(&args.uart, args.baud, Arc::clone(&stop)) {
        Ok(writer) => writer,
        Err(code) => return code,
    };
    {
        let stop = Arc::clone(&stop);
        if ctrlc::set_handler(move || stop.store(true, Ordering::Relaxed)).is_err() {
            eprintln!("could not install the interrupt handler");
        }
    }

    if args.check {
        eprintln!("check uart={} baud={}", args.uart, args.baud);
        let interval =
            Duration::try_from_secs_f64(args.interval.max(0.2)).unwrap_or(Duration::from_secs(1));
        return run_check(interval, &mut writer, &stop);
    }

    let Some(text) = args.net_json.as_deref() else {
        eprintln!("pass --net-json HOST:PORT, or --check");
        return ExitCode::from(1);
    };
    let Some(addrs) = resolve_net_json(text) else {
        eprintln!("net-json {text}: expected host:port");
        return ExitCode::from(1);
    };
    eprintln!("net-json={text} uart={} baud={}", args.uart, args.baud);
    run_port(&addrs, text, &mut writer, &stop)
}

fn run_check(interval: Duration, writer: &mut Writer, stop: &AtomicBool) -> ExitCode {
    while !stop.load(Ordering::Relaxed) {
        for (position, frame) in check_positions().into_iter().zip(check_frames()) {
            eprintln!(
                "fix t={} icao={:06x}",
                position.unix_s, position.aircraft.icao
            );
            writer.enqueue(frame);
        }
        let deadline = Instant::now() + interval;
        loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            if let Err(err) = writer.write_some() {
                return write_exit(err);
            }
            let now = Instant::now();
            if now >= deadline || !writer.has_pending() {
                sleep_until_stop(stop, deadline.saturating_duration_since(now));
                break;
            }
            let Some(fd) = writer.uart_fd() else {
                break;
            };
            let slice = deadline.saturating_duration_since(now).min(POLL_SLICE);
            let mut poll_fd = libc::pollfd {
                fd,
                events: libc::POLLOUT,
                revents: 0,
            };
            if let Err(err) = poll_wait(std::slice::from_mut(&mut poll_fd), slice) {
                eprintln!("uart poll: {err}");
                return ExitCode::from(1);
            }
        }
    }
    eprintln!("stopped");
    ExitCode::SUCCESS
}

fn run_port(addrs: &[SocketAddr], text: &str, writer: &mut Writer, stop: &AtomicBool) -> ExitCode {
    let mut delay = RETRY_MIN;
    while !stop.load(Ordering::Relaxed) {
        match dial_first(addrs, stop, text) {
            Ok(stream) => {
                eprintln!("net-json {text} connected");
                match pump(stream, writer, stop, text) {
                    Pump::Stopped => return write_exit(ChunkError::Stopped),
                    Pump::Uart(err) => return write_exit(ChunkError::Failed(err)),
                    Pump::Disconnected => delay = RETRY_MIN,
                }
            }
            Err(DialError::Stopped) => break,
            Err(DialError::Failed(_)) => {}
        }
        if stop.load(Ordering::Relaxed) {
            break;
        }
        sleep_until_stop(stop, delay);
        delay = grow(delay);
    }
    eprintln!("stopped");
    ExitCode::SUCCESS
}

#[derive(Debug)]
enum DialError {
    Stopped,
    Failed(io::Error),
}

/// Try each resolved address. A refused `::1` must not hide a later `127.0.0.1`.
fn dial_first(addrs: &[SocketAddr], stop: &AtomicBool, text: &str) -> Result<TcpStream, DialError> {
    if addrs.is_empty() {
        eprintln!("net-json {text}: expected host:port");
        return Err(DialError::Failed(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected host:port",
        )));
    }
    let mut last = None;
    for addr in addrs {
        if stop.load(Ordering::Relaxed) {
            return Err(DialError::Stopped);
        }
        match dial(addr, stop, CONNECT_TIMEOUT) {
            Ok(stream) => return Ok(stream),
            Err(DialError::Stopped) => return Err(DialError::Stopped),
            Err(DialError::Failed(err)) => {
                eprintln!("net-json {text}: {err}");
                last = Some(err);
            }
        }
    }
    Err(DialError::Failed(last.unwrap_or_else(|| {
        io::Error::new(io::ErrorKind::NotConnected, "connection failed")
    })))
}

struct SocketFd(i32);

impl Drop for SocketFd {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}

impl SocketFd {
    fn new(domain: libc::c_int) -> io::Result<Self> {
        let fd = unsafe {
            libc::socket(
                domain,
                libc::SOCK_STREAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
                0,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(fd))
    }

    fn into_stream(self) -> TcpStream {
        let fd = self.0;
        std::mem::forget(self);
        unsafe { TcpStream::from_raw_fd(fd) }
    }
}

fn dial(addr: &SocketAddr, stop: &AtomicBool, timeout: Duration) -> Result<TcpStream, DialError> {
    if stop.load(Ordering::Relaxed) {
        return Err(DialError::Stopped);
    }
    let domain = if addr.is_ipv4() {
        libc::AF_INET
    } else {
        libc::AF_INET6
    };
    let sock = SocketFd::new(domain).map_err(DialError::Failed)?;
    match connect_fd(sock.0, addr) {
        Ok(()) => {}
        Err(err) if pending_connect(&err) => wait_connected(sock.0, stop, timeout)?,
        Err(err) => return Err(DialError::Failed(err)),
    }
    clear_nonblocking(sock.0).map_err(DialError::Failed)?;
    Ok(sock.into_stream())
}

fn pending_connect(err: &io::Error) -> bool {
    matches!(
        err.raw_os_error(),
        Some(code)
            if code == libc::EINPROGRESS
                || code == libc::EAGAIN
                || code == libc::EWOULDBLOCK
                || code == libc::EINTR
    )
}

fn connect_fd(fd: i32, addr: &SocketAddr) -> io::Result<()> {
    let rc = match *addr {
        SocketAddr::V4(addr) => {
            let raw = libc::sockaddr_in {
                sin_family: libc::AF_INET as libc::sa_family_t,
                sin_port: addr.port().to_be(),
                sin_addr: libc::in_addr {
                    s_addr: u32::from(*addr.ip()).to_be(),
                },
                sin_zero: [0; 8],
            };
            unsafe {
                libc::connect(
                    fd,
                    &raw as *const libc::sockaddr_in as *const libc::sockaddr,
                    std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
                )
            }
        }
        SocketAddr::V6(addr) => {
            let raw = libc::sockaddr_in6 {
                sin6_family: libc::AF_INET6 as libc::sa_family_t,
                sin6_port: addr.port().to_be(),
                sin6_flowinfo: addr.flowinfo(),
                sin6_addr: libc::in6_addr {
                    s6_addr: addr.ip().octets(),
                },
                sin6_scope_id: addr.scope_id(),
            };
            unsafe {
                libc::connect(
                    fd,
                    &raw as *const libc::sockaddr_in6 as *const libc::sockaddr,
                    std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t,
                )
            }
        }
    };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Poll until the connect finishes so Ctrl-C is noticed during the wait.
fn wait_connected(fd: i32, stop: &AtomicBool, timeout: Duration) -> Result<(), DialError> {
    let deadline = Instant::now() + timeout;
    loop {
        if stop.load(Ordering::Relaxed) {
            return Err(DialError::Stopped);
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(DialError::Failed(io::Error::new(
                io::ErrorKind::TimedOut,
                "connection timed out",
            )));
        }
        let slice = deadline.saturating_duration_since(now).min(CONNECT_SLICE);
        let mut poll_fd = libc::pollfd {
            fd,
            events: libc::POLLOUT,
            revents: 0,
        };
        let rc = unsafe { libc::poll(&mut poll_fd, 1, slice.as_millis() as libc::c_int) };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(DialError::Failed(err));
        }
        if rc == 0 {
            continue;
        }
        if poll_fd.revents & (libc::POLLOUT | libc::POLLERR | libc::POLLHUP) == 0 {
            continue;
        }
        let mut so_error: libc::c_int = 0;
        let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        let grc = unsafe {
            libc::getsockopt(
                fd,
                libc::SOL_SOCKET,
                libc::SO_ERROR,
                &mut so_error as *mut libc::c_int as *mut libc::c_void,
                &mut len,
            )
        };
        if grc < 0 {
            return Err(DialError::Failed(io::Error::last_os_error()));
        }
        if so_error != 0 {
            return Err(DialError::Failed(io::Error::from_raw_os_error(so_error)));
        }
        return Ok(());
    }
}

fn set_socket_int(fd: i32, level: libc::c_int, name: libc::c_int, value: u32) -> io::Result<()> {
    let value = value as libc::c_int;
    let rc = unsafe {
        libc::setsockopt(
            fd,
            level,
            name,
            &value as *const libc::c_int as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if rc < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// These probes turn a peer that dies without a FIN into a read error, so the caller reconnects.
fn enable_keepalive(stream: &TcpStream) -> io::Result<()> {
    let fd = stream.as_raw_fd();
    set_socket_int(fd, libc::SOL_SOCKET, libc::SO_KEEPALIVE, 1)?;
    set_socket_int(fd, libc::IPPROTO_TCP, libc::TCP_KEEPIDLE, KEEPALIVE_IDLE_S)?;
    set_socket_int(
        fd,
        libc::IPPROTO_TCP,
        libc::TCP_KEEPINTVL,
        KEEPALIVE_INTERVAL_S,
    )?;
    set_socket_int(fd, libc::IPPROTO_TCP, libc::TCP_KEEPCNT, KEEPALIVE_PROBES)
}

fn poll_wait(fds: &mut [libc::pollfd], timeout: Duration) -> io::Result<i32> {
    let millis = timeout.as_millis().min(i32::MAX as u128) as libc::c_int;
    loop {
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, millis) };
        if rc < 0 {
            let err = io::Error::last_os_error();
            if err.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(err);
        }
        return Ok(rc);
    }
}

fn pump(stream: TcpStream, writer: &mut Writer, stop: &AtomicBool, text: &str) -> Pump {
    if let Err(err) = set_nonblocking(stream.as_raw_fd()) {
        eprintln!("net-json {text}: {err}");
        return Pump::Disconnected;
    }
    if let Err(err) = enable_keepalive(&stream) {
        eprintln!("net-json {text} keepalive: {err}");
        return Pump::Disconnected;
    }
    let mut lines = JsonLines {
        reader: stream,
        pending: Vec::new(),
        state: Pending::Collecting,
    };
    loop {
        if stop.load(Ordering::Relaxed) {
            return Pump::Stopped;
        }
        let mut fds = [
            libc::pollfd {
                fd: lines.reader.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: -1,
                events: 0,
                revents: 0,
            },
        ];
        let mut nfds = 1usize;
        if writer.has_pending() {
            if let Some(fd) = writer.uart_fd() {
                fds[1] = libc::pollfd {
                    fd,
                    events: libc::POLLOUT,
                    revents: 0,
                };
                nfds = 2;
            }
        }
        if let Err(err) = poll_wait(&mut fds[..nfds], POLL_SLICE) {
            eprintln!("net-json {text}: {err}");
            return end_socket(writer);
        }
        if stop.load(Ordering::Relaxed) {
            return Pump::Stopped;
        }
        let socket_revents = fds[0].revents;
        let mut fresh = false;
        if socket_revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
            match lines.pull() {
                Ok(Pull::Eof) => {
                    enqueue_ready(&mut lines, writer);
                    eprintln!("net-json {text} disconnected");
                    return end_socket(writer);
                }
                Ok(Pull::Bytes) => {
                    enqueue_ready(&mut lines, writer);
                    fresh = true;
                    if socket_revents & (libc::POLLHUP | libc::POLLERR) != 0 {
                        eprintln!("net-json {text} disconnected");
                        return end_socket(writer);
                    }
                }
                Err(err) => {
                    eprintln!("net-json {text}: {err}");
                    enqueue_ready(&mut lines, writer);
                    return end_socket(writer);
                }
            }
        }
        let uart_ready =
            nfds == 2 && fds[1].revents & (libc::POLLOUT | libc::POLLERR | libc::POLLHUP) != 0;
        if writer.has_pending() && (fresh || uart_ready || writer.uart_fd().is_none()) {
            if let Err(err) = writer.write_some() {
                return match err {
                    ChunkError::Stopped => Pump::Stopped,
                    ChunkError::Failed(err) => Pump::Uart(err),
                };
            }
        }
    }
}

fn enqueue_ready(lines: &mut JsonLines, writer: &mut Writer) {
    while let Some(line) = lines.pop_line() {
        let Some(fix) = fix_from_json_line(&line) else {
            continue;
        };
        writer.enqueue(pack_frame(fix.unix_s, &fix.aircraft));
        eprintln!("fix t={} icao={:06x}", fix.unix_s, fix.aircraft.icao);
    }
    lines.note_overflow();
}

fn end_socket(writer: &mut Writer) -> Pump {
    match writer.write_some() {
        Ok(()) => Pump::Disconnected,
        Err(ChunkError::Stopped) => Pump::Stopped,
        Err(ChunkError::Failed(err)) => Pump::Uart(err),
    }
}

fn write_exit(err: ChunkError) -> ExitCode {
    match err {
        ChunkError::Stopped => {
            eprintln!("stopped");
            ExitCode::SUCCESS
        }
        ChunkError::Failed(err) => {
            eprintln!("uart write failed: {err}");
            ExitCode::from(1)
        }
    }
}

fn sleep_until_stop(stop: &AtomicBool, mut left: Duration) {
    let step = Duration::from_millis(50);
    while left > Duration::ZERO {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let slice = left.min(step);
        thread::sleep(slice);
        left = left.saturating_sub(slice);
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{Ipv4Addr, Ipv6Addr, TcpListener};
    use std::os::fd::AsRawFd;

    use super::*;

    fn socket_int(fd: i32, level: libc::c_int, name: libc::c_int) -> libc::c_int {
        let mut value: libc::c_int = -1;
        let mut len = std::mem::size_of::<libc::c_int>() as libc::socklen_t;
        let rc = unsafe {
            libc::getsockopt(
                fd,
                level,
                name,
                &mut value as *mut libc::c_int as *mut libc::c_void,
                &mut len,
            )
        };
        assert_eq!(rc, 0, "{}", std::io::Error::last_os_error());
        value
    }

    #[test]
    fn the_readsb_socket_probes_a_silent_peer() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let stream = TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
        enable_keepalive(&stream).expect("keepalive");
        let fd = stream.as_raw_fd();
        assert_eq!(socket_int(fd, libc::SOL_SOCKET, libc::SO_KEEPALIVE), 1);
        assert_eq!(
            socket_int(fd, libc::IPPROTO_TCP, libc::TCP_KEEPIDLE),
            KEEPALIVE_IDLE_S as libc::c_int
        );
        assert_eq!(
            socket_int(fd, libc::IPPROTO_TCP, libc::TCP_KEEPINTVL),
            KEEPALIVE_INTERVAL_S as libc::c_int
        );
        assert_eq!(
            socket_int(fd, libc::IPPROTO_TCP, libc::TCP_KEEPCNT),
            KEEPALIVE_PROBES as libc::c_int
        );
    }

    struct Pty {
        master: TTYPort,
        path: String,
    }

    impl Pty {
        fn open() -> Self {
            let (master, slave) = TTYPort::pair().expect("pty");
            let path = slave.name().expect("pty name");
            drop(slave);
            Self { master, path }
        }
    }

    fn frame_icao(frame: &[u8]) -> u32 {
        u32::from_le_bytes([frame[8], frame[9], frame[10], 0])
    }

    fn frame_for(icao: u32, emergency: bool) -> [u8; trck::FRAME_LEN] {
        let squawk = if emergency { r#","squawk":"7700""# } else { "" };
        let line = format!(r#"{{"now":1700000000,"hex":"{icao:06x}","lat":1,"lon":2{squawk}}}"#);
        let fix = fix_from_json_line(line.as_bytes()).expect("line");
        assert_eq!(fix.aircraft.emergency.is_some(), emergency, "{line}");
        pack_frame(fix.unix_s, &fix.aircraft)
    }

    fn fill_until_blocked(fd: i32) -> usize {
        let buf = [0xA5u8; 1024];
        let mut wrote = 0usize;
        loop {
            let rc = unsafe { libc::write(fd, buf.as_ptr() as *const libc::c_void, buf.len()) };
            if rc < 0 {
                let err = std::io::Error::last_os_error();
                assert_eq!(err.kind(), std::io::ErrorKind::WouldBlock, "{err}");
                break;
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

    #[test]
    fn ctrl_c_stops_while_the_uart_is_not_draining() {
        let pty = Pty::open();
        let stop = Arc::new(AtomicBool::new(false));
        let mut writer = open_uart(&pty.path, DEFAULT_BAUD, Arc::clone(&stop)).expect("pty");
        fill_until_blocked(writer.uart_fd().expect("fd"));
        writer.enqueue(frame_for(1, false));
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
        thread::spawn(move || {
            let _sock = listener.accept().expect("accept").0;
            let _ = held_rx.recv();
        });
        let stream = TcpStream::connect(addr).expect("connect");
        let flag = Arc::clone(&stop);
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            flag.store(true, Ordering::Relaxed);
        });
        let started = Instant::now();
        let end = pump(stream, &mut writer, &stop, "test");
        drop(held_tx);
        assert!(matches!(end, Pump::Stopped), "{end:?}");
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(writer.has_pending(), "the frame should still be queued");
    }

    #[test]
    fn the_pty_master_returns_what_was_written() {
        let mut pty = Pty::open();
        let stop = Arc::new(AtomicBool::new(false));
        let mut writer = open_uart(&pty.path, 115_200, stop).expect("pty");
        let frame = frame_for(0x40621D, false);
        writer.enqueue(frame);
        writer.write_some().expect("write");
        assert!(!writer.has_pending());
        pty.master
            .set_timeout(Duration::from_secs(1))
            .expect("timeout");
        let mut buf = [0u8; 64];
        let n = pty.master.read(&mut buf).expect("read");
        assert_eq!(&buf[..n], &frame);
    }

    #[test]
    fn a_silent_pty_accepts_dozens_of_lines_without_stalling() {
        const LINES: u32 = 48;
        let pty = Pty::open();
        let stop = Arc::new(AtomicBool::new(false));
        let mut writer = open_uart(&pty.path, DEFAULT_BAUD, Arc::clone(&stop)).expect("pty");
        let _filled = fill_until_blocked(writer.uart_fd().expect("fd"));
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        thread::spawn(move || {
            let (mut sock, _) = listener.accept().expect("accept");
            for icao in 0..LINES {
                let line = format!(r#"{{"now":1700000000,"hex":"{icao:06x}","lat":1,"lon":2}}"#);
                writeln!(sock, "{line}").expect("line");
            }
        });
        let stream = TcpStream::connect(addr).expect("connect");
        let started = Instant::now();
        let end = pump(stream, &mut writer, &stop, "test");
        let elapsed = started.elapsed();
        assert!(matches!(end, Pump::Disconnected), "{end:?}");
        let sent = writer.current.map(|(_, sent)| sent).unwrap_or(0);
        assert!(
            elapsed < Duration::from_millis(50),
            "48 lines took {elapsed:?}"
        );
        assert!(
            writer.queue.len() + usize::from(writer.current.is_some()) > LINES as usize / 2,
            "queue {} sent {sent} elapsed {elapsed:?}",
            writer.queue.len()
        );
    }

    #[test]
    fn a_7700_survives_a_queue_full_of_ordinary_frames() {
        let pty = Pty::open();
        let stop = Arc::new(AtomicBool::new(false));
        let mut writer = open_uart(&pty.path, DEFAULT_BAUD, stop).expect("pty");
        for icao in 0..QUEUE_CAP as u32 {
            writer.enqueue(frame_for(icao, false));
        }
        writer.enqueue(frame_for(0x7700, true));
        assert_eq!(writer.queue.len(), QUEUE_CAP);
        let kept = writer.queue.back().expect("emergency");
        assert_eq!(kept[27], trck::EmergencyKind::General.code());
        assert_eq!(frame_icao(kept), 0x7700);
        assert_eq!(
            u32::from_le_bytes(kept[4..8].try_into().unwrap()),
            1_700_000_000
        );
        assert!(!writer.queue.iter().any(|frame| frame_icao(frame) == 0));
        assert!(writer.queue.iter().any(|frame| frame_icao(frame) == 1));

        writer.queue.clear();
        for icao in 1..=QUEUE_CAP as u32 {
            writer.enqueue(frame_for(icao, true));
        }
        writer.enqueue(frame_for(0x100, false));
        assert_eq!(writer.queue.len(), QUEUE_CAP);
        assert!(writer.queue.iter().all(|frame| frame[27] != 0));
        assert!(!writer.queue.iter().any(|frame| frame_icao(frame) == 0x100));
    }

    #[test]
    fn localhost_resolves_every_address() {
        let addrs = resolve_net_json("localhost:9").expect("localhost");
        assert!(addrs.iter().any(SocketAddr::is_ipv4), "{addrs:?}");
        assert!(addrs.iter().any(SocketAddr::is_ipv6), "{addrs:?}");
    }

    #[test]
    fn dials_the_later_address_when_the_first_refuses() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind");
        let port = listener.local_addr().expect("addr").port();
        thread::spawn(move || {
            let _ = listener.accept();
        });
        let addrs = [
            SocketAddr::from((Ipv6Addr::LOCALHOST, port)),
            SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        ];
        let stop = AtomicBool::new(false);
        let stream = dial_first(&addrs, &stop, "localhost").expect("dial");
        assert_eq!(
            stream.peer_addr().expect("peer").ip(),
            std::net::IpAddr::V4(Ipv4Addr::LOCALHOST)
        );
    }

    #[test]
    fn ctrl_c_interrupts_a_connect() {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            flag.store(true, Ordering::Relaxed);
        });
        let addr = "192.0.2.1:81".parse().expect("addr");
        let started = Instant::now();
        let err = dial(&addr, &stop, CONNECT_TIMEOUT).expect_err("blackhole");
        assert!(matches!(err, DialError::Stopped), "{err:?}");
        assert!(started.elapsed() < Duration::from_secs(1));
    }
}

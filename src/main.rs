//! Read readsb `--net-json-port` and write one UART frame per position.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::os::fd::AsRawFd;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use clap::Parser;
use serialport::{ClearBuffer, DataBits, FlowControl, Parity, SerialPort, StopBits, TTYPort};

use trck::{EmergencyKind, Fix, check_positions, fix_from_json_line, pack_frame};

mod sys;

const DEFAULT_UART: &str = "/dev/serial0";
const DEFAULT_BAUD: u32 = 115_200;
const CHECK_PAUSE: Duration = Duration::from_secs(1);
/// Quiet-link upper bound so Ctrl-C is noticed without a UART or socket event.
const POLL_SLICE: Duration = Duration::from_millis(200);
/// Connect wait slice so Ctrl-C is noticed during `CONNECT_TIMEOUT`.
const CONNECT_SLICE: Duration = Duration::from_millis(50);
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
    queue: VecDeque<Fix>,
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

    fn enqueue(&mut self, fix: Fix) {
        if self.queue.len() < QUEUE_CAP {
            self.queue.push_back(fix);
            return;
        }
        if let Some(index) = self
            .queue
            .iter()
            .position(|queued| queued.aircraft.emergency.is_none())
        {
            let dropped = self.queue.remove(index).expect("ordinary position");
            log_drop(&dropped);
            self.queue.push_back(fix);
            return;
        }
        if fix.aircraft.emergency.is_none() {
            log_drop(&fix);
            return;
        }
        let dropped = self.queue.pop_front().expect("full queue");
        log_drop(&dropped);
        self.queue.push_back(fix);
    }

    /// Write until the port would block or the queue is empty. A short write stays in `current`.
    fn write_some(&mut self) -> Result<(), ChunkError> {
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return Err(ChunkError::Stopped);
            }
            if self.current.is_none() {
                let Some(fix) = self.queue.pop_front() else {
                    return Ok(());
                };
                self.current = Some((pack_frame(&fix), 0));
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

fn log_drop(fix: &Fix) {
    let emergency = fix.aircraft.emergency.map(EmergencyKind::code).unwrap_or(0);
    eprintln!(
        "dropped frame icao={:06x} emergency={emergency}",
        fix.aircraft.icao
    );
}

fn write_sink(sink: &mut Sink, bytes: &[u8]) -> io::Result<usize> {
    match sink {
        Sink::Uart(port) => sys::write(port.as_raw_fd(), bytes),
        Sink::Stdout => {
            let mut out = io::stdout().lock();
            out.write_all(bytes)?;
            out.flush()?;
            Ok(bytes.len())
        }
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
    sys::set_nonblocking(port.as_raw_fd()).map_err(|err| open_failed(path, err))?;
    Ok(Writer {
        sink: Sink::Uart(port),
        stop,
        queue: VecDeque::with_capacity(QUEUE_CAP),
        current: None,
    })
}

fn open_output(path: &str, baud: u32, stop: Arc<AtomicBool>) -> Result<Writer, ExitCode> {
    if path == "-" {
        return Ok(Writer {
            sink: Sink::Stdout,
            stop,
            queue: VecDeque::with_capacity(QUEUE_CAP),
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
        return run_check(&mut writer, &stop);
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

fn run_check(writer: &mut Writer, stop: &AtomicBool) -> ExitCode {
    while !stop.load(Ordering::Relaxed) {
        for position in check_positions() {
            eprintln!(
                "fix t={} icao={:06x}",
                position.unix_s, position.aircraft.icao
            );
            writer.enqueue(position);
        }
        let deadline = Instant::now() + CHECK_PAUSE;
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
            let mut poll_fd = sys::pollfd(fd, libc::POLLOUT);
            if let Err(err) = sys::poll(std::slice::from_mut(&mut poll_fd), slice) {
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

fn dial(addr: &SocketAddr, stop: &AtomicBool, timeout: Duration) -> Result<TcpStream, DialError> {
    if stop.load(Ordering::Relaxed) {
        return Err(DialError::Stopped);
    }
    let domain = if addr.is_ipv4() {
        libc::AF_INET
    } else {
        libc::AF_INET6
    };
    let sock = sys::SocketFd::new(domain).map_err(DialError::Failed)?;
    match sys::connect(sock.as_raw_fd(), addr) {
        Ok(()) => {}
        Err(err) if pending_connect(&err) => wait_connected(sock.as_raw_fd(), stop, timeout)?,
        Err(err) => return Err(DialError::Failed(err)),
    }
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
        let mut poll_fd = sys::pollfd(fd, libc::POLLOUT);
        let rc = match sys::poll(std::slice::from_mut(&mut poll_fd), slice) {
            Ok(rc) => rc,
            Err(err) => return Err(DialError::Failed(err)),
        };
        if rc == 0 {
            continue;
        }
        if poll_fd.revents & (libc::POLLOUT | libc::POLLERR | libc::POLLHUP) == 0 {
            continue;
        }
        let so_error = sys::so_error(fd).map_err(DialError::Failed)?;
        if so_error != 0 {
            return Err(DialError::Failed(io::Error::from_raw_os_error(so_error)));
        }
        return Ok(());
    }
}

/// These probes turn a peer that dies without a FIN into a read error, so the caller reconnects.
fn enable_keepalive(stream: &TcpStream) -> io::Result<()> {
    let fd = stream.as_raw_fd();
    sys::setsockopt_int(fd, libc::SOL_SOCKET, libc::SO_KEEPALIVE, 1)?;
    sys::setsockopt_int(fd, libc::IPPROTO_TCP, libc::TCP_KEEPIDLE, KEEPALIVE_IDLE_S)?;
    sys::setsockopt_int(
        fd,
        libc::IPPROTO_TCP,
        libc::TCP_KEEPINTVL,
        KEEPALIVE_INTERVAL_S,
    )?;
    sys::setsockopt_int(fd, libc::IPPROTO_TCP, libc::TCP_KEEPCNT, KEEPALIVE_PROBES)
}

fn pump(stream: TcpStream, writer: &mut Writer, stop: &AtomicBool, text: &str) -> Pump {
    if let Err(err) = enable_keepalive(&stream) {
        eprintln!("net-json {text} keepalive: {err}");
        return Pump::Disconnected;
    }
    let mut lines = JsonLines {
        reader: stream,
        pending: Vec::with_capacity(MAX_LINE),
        state: Pending::Collecting,
    };
    loop {
        if stop.load(Ordering::Relaxed) {
            return Pump::Stopped;
        }
        let mut fds = [
            sys::pollfd(lines.reader.as_raw_fd(), libc::POLLIN),
            sys::pollfd(-1, 0),
        ];
        let mut nfds = 1usize;
        if writer.has_pending()
            && let Some(fd) = writer.uart_fd()
        {
            fds[1] = sys::pollfd(fd, libc::POLLOUT);
            nfds = 2;
        }
        if let Err(err) = sys::poll(&mut fds[..nfds], POLL_SLICE) {
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
        if writer.has_pending()
            && (fresh || uart_ready || writer.uart_fd().is_none())
            && let Err(err) = writer.write_some()
        {
            return match err {
                ChunkError::Stopped => Pump::Stopped,
                ChunkError::Failed(err) => Pump::Uart(err),
            };
        }
    }
}

fn enqueue_ready(lines: &mut JsonLines, writer: &mut Writer) {
    while let Some(line) = lines.pop_line() {
        let Some(fix) = fix_from_json_line(&line) else {
            continue;
        };
        writer.enqueue(fix);
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

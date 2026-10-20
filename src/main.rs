//! Read readsb `--net-json-port` and write one UART frame per position.

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

use trck::{Aircraft, fix_from_json_line, pack_frame, snapshot_from_aircraft_json};

const DEFAULT_UART: &str = "/dev/serial0";
const DEFAULT_BAUD: u32 = 115_200;
const DEFAULT_INTERVAL_S: f64 = 1.0;
const SAMPLE_AIRCRAFT_JSON: &str = include_str!("../sample/aircraft.json");
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const WRITE_SLICE: Duration = Duration::from_millis(50);
const READ_SLICE: Duration = Duration::from_millis(200);
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
    /// Host and port of readsb `--net-json-port`. Required unless `--sample`.
    #[arg(long, required_unless_present = "sample", value_name = "HOST:PORT")]
    net_json: Option<String>,
    /// UART device, or - for stdout.
    #[arg(long, default_value = DEFAULT_UART)]
    uart: String,
    #[arg(long, default_value_t = DEFAULT_BAUD)]
    baud: u32,
    /// Seconds between passes of `--sample`. The JSON port does not wait.
    #[arg(long, default_value_t = DEFAULT_INTERVAL_S)]
    interval: f64,
    /// Send the built-in aircraft.json sample instead of the JSON port.
    #[arg(long)]
    sample: bool,
}

#[derive(Debug)]
enum ChunkError {
    Stopped,
    Failed(io::Error),
}

enum Pump {
    Stopped,
    Disconnected,
    Uart(io::Error),
}

trait Output {
    fn write_chunk(&mut self, data: &[u8]) -> Result<(), ChunkError>;
}

struct StdoutOut;

impl Output for StdoutOut {
    fn write_chunk(&mut self, data: &[u8]) -> Result<(), ChunkError> {
        let mut out = io::stdout().lock();
        out.write_all(data).map_err(ChunkError::Failed)?;
        out.flush().map_err(ChunkError::Failed)
    }
}

struct UartOut {
    port: TTYPort,
    stop: Arc<AtomicBool>,
}

impl UartOut {
    /// Queue `data` in the kernel and return. The JSON reader shares this thread, so a frame
    /// whose bytes are already accepted must not wait out `TIOCOUTQ`.
    fn write_within(&mut self, data: &[u8], budget: Duration) -> Result<(), ChunkError> {
        self.write_and_wait(data, budget, bytes_queued, thread::sleep)
    }

    fn write_and_wait<Q, S>(
        &mut self,
        mut data: &[u8],
        budget: Duration,
        mut queued: Q,
        mut sleep: S,
    ) -> Result<(), ChunkError>
    where
        Q: FnMut(i32) -> io::Result<i32>,
        S: FnMut(Duration),
    {
        let deadline = Instant::now() + budget;
        let fd = self.port.as_raw_fd();
        while !data.is_empty() {
            self.tick(deadline)?;
            match self.port.write(data) {
                Ok(0) => {
                    return Err(ChunkError::Failed(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "uart write wrote nothing",
                    )));
                }
                Ok(n) => data = &data[n..],
                Err(err) if retry(&err) => {
                    let n = queued(fd).map_err(ChunkError::Failed)?;
                    let pause = uart_wait(n);
                    // An empty ioctl count can still be `WouldBlock`. Yield, but not for a slice.
                    sleep(if pause.is_zero() {
                        Duration::from_millis(1)
                    } else {
                        pause
                    });
                }
                Err(err) => return Err(ChunkError::Failed(err)),
            }
        }
        Ok(())
    }

    fn tick(&self, deadline: Instant) -> Result<(), ChunkError> {
        if self.stop.load(Ordering::Relaxed) {
            return Err(ChunkError::Stopped);
        }
        if Instant::now() >= deadline {
            return Err(ChunkError::Failed(io::Error::new(
                io::ErrorKind::TimedOut,
                "uart write timed out",
            )));
        }
        Ok(())
    }
}

impl Output for UartOut {
    fn write_chunk(&mut self, data: &[u8]) -> Result<(), ChunkError> {
        self.write_within(data, WRITE_TIMEOUT)
    }
}

/// How long the reader may pause while `queued` bytes still sit in the UART.
/// A full [`WRITE_SLICE`] is long enough for readsb to drop a 7700.
fn uart_wait(queued: i32) -> Duration {
    if queued <= 0 {
        return Duration::ZERO;
    }
    // 8N1 bit time at the default baud, capped well under the old drain nap.
    let nanos = u64::from(queued as u32).saturating_mul(10_000_000_000) / u64::from(DEFAULT_BAUD);
    Duration::from_nanos(nanos).min(Duration::from_millis(2))
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

enum Incoming {
    Line(Vec<u8>),
    Eof,
    Stopped,
    Failed(io::Error),
}

impl JsonLines {
    fn next_line(&mut self, stop: &AtomicBool) -> Incoming {
        loop {
            if stop.load(Ordering::Relaxed) {
                return Incoming::Stopped;
            }
            if let Some(split) = self.pending.iter().position(|byte| *byte == b'\n') {
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
                    Pending::Collecting => return Incoming::Line(line),
                }
            }
            if self.pending.len() > MAX_LINE {
                self.pending.clear();
                self.state = Pending::DiscardUntilNewline;
            }
            let mut tmp = [0u8; 4096];
            match self.reader.read(&mut tmp) {
                Ok(0) => return Incoming::Eof,
                Ok(n) => self.pending.extend_from_slice(&tmp[..n]),
                Err(err) if retry(&err) => {}
                Err(err) => return Incoming::Failed(err),
            }
        }
    }
}

fn retry(err: &io::Error) -> bool {
    matches!(
        err.kind(),
        io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
    )
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

fn bytes_queued(fd: i32) -> io::Result<i32> {
    let mut queued = 0i32;
    let rc = unsafe { libc::ioctl(fd, libc::TIOCOUTQ, &mut queued) };
    if rc < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(queued)
}

fn open_failed(path: &str, err: impl std::fmt::Display) -> ExitCode {
    eprintln!("cannot open {path}: {err}");
    ExitCode::from(1)
}

fn open_uart(path: &str, baud: u32, stop: Arc<AtomicBool>) -> Result<UartOut, ExitCode> {
    let port = serialport::new(path, baud)
        .data_bits(DataBits::Eight)
        .parity(Parity::None)
        .stop_bits(StopBits::One)
        .flow_control(FlowControl::None)
        .timeout(WRITE_SLICE)
        .open_native()
        .map_err(|err| open_failed(path, err))?;
    port.clear(ClearBuffer::All)
        .map_err(|err| open_failed(path, err))?;
    set_nonblocking(port.as_raw_fd()).map_err(|err| open_failed(path, err))?;
    Ok(UartOut { port, stop })
}

fn open_output(path: &str, baud: u32, stop: Arc<AtomicBool>) -> Result<Box<dyn Output>, ExitCode> {
    if path == "-" {
        return Ok(Box::new(StdoutOut));
    }
    Ok(Box::new(open_uart(path, baud, stop)?))
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
    let mut output = match open_output(&args.uart, args.baud, Arc::clone(&stop)) {
        Ok(output) => output,
        Err(code) => return code,
    };
    {
        let stop = Arc::clone(&stop);
        if ctrlc::set_handler(move || stop.store(true, Ordering::Relaxed)).is_err() {
            eprintln!("could not install the interrupt handler");
        }
    }

    if args.sample {
        eprintln!("sample uart={} baud={}", args.uart, args.baud);
        let interval =
            Duration::try_from_secs_f64(args.interval.max(0.2)).unwrap_or(Duration::from_secs(1));
        return run_sample(interval, &mut *output, &stop);
    }

    let Some(text) = args.net_json.as_deref() else {
        eprintln!("pass --net-json HOST:PORT, or --sample");
        return ExitCode::from(1);
    };
    let Some(addrs) = resolve_net_json(text) else {
        eprintln!("net-json {text}: expected host:port");
        return ExitCode::from(1);
    };
    eprintln!("net-json={text} uart={} baud={}", args.uart, args.baud);
    run_port(&addrs, text, &mut *output, &stop)
}

fn run_sample(interval: Duration, output: &mut dyn Output, stop: &AtomicBool) -> ExitCode {
    let snapshot = match snapshot_from_aircraft_json(SAMPLE_AIRCRAFT_JSON.as_bytes()) {
        Ok(snapshot) => snapshot,
        Err(err) => {
            eprintln!("sample: {err}");
            return ExitCode::from(1);
        }
    };
    while !stop.load(Ordering::Relaxed) {
        for aircraft in &snapshot.aircraft {
            if let Err(err) = write_frame(output, snapshot.unix_s, aircraft) {
                return write_exit(err);
            }
            eprintln!("fix t={} icao={:06x}", snapshot.unix_s, aircraft.icao);
        }
        sleep_until_stop(stop, interval);
    }
    eprintln!("stopped");
    ExitCode::SUCCESS
}

fn run_port(
    addrs: &[SocketAddr],
    text: &str,
    output: &mut dyn Output,
    stop: &AtomicBool,
) -> ExitCode {
    let mut delay = RETRY_MIN;
    while !stop.load(Ordering::Relaxed) {
        match dial_first(addrs, stop, text) {
            Ok(stream) => {
                eprintln!("net-json {text} connected");
                match pump(stream, output, stop, text) {
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

/// `connect` on a black-holed route used to block for [`CONNECT_TIMEOUT`]. Poll in
/// short slices so Ctrl-C is noticed the way the retry sleep already is.
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
        let slice = deadline.saturating_duration_since(now).min(WRITE_SLICE);
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

/// A timed-out read is an empty sky. These probes turn a peer that dies without
/// a FIN into a read error, so the caller reconnects.
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

fn pump(stream: TcpStream, output: &mut dyn Output, stop: &AtomicBool, text: &str) -> Pump {
    if let Err(err) = stream.set_read_timeout(Some(READ_SLICE)) {
        eprintln!("net-json {text} read timeout: {err}");
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
        let line = match lines.next_line(stop) {
            Incoming::Line(line) => line,
            Incoming::Eof => {
                eprintln!("net-json {text} disconnected");
                return Pump::Disconnected;
            }
            Incoming::Stopped => return Pump::Stopped,
            Incoming::Failed(err) => {
                eprintln!("net-json {text}: {err}");
                return Pump::Disconnected;
            }
        };
        let Some(fix) = fix_from_json_line(&line) else {
            continue;
        };
        if let Err(err) = write_frame(output, fix.unix_s, &fix.aircraft) {
            return match err {
                ChunkError::Stopped => Pump::Stopped,
                ChunkError::Failed(err) => Pump::Uart(err),
            };
        }
        eprintln!("fix t={} icao={:06x}", fix.unix_s, fix.aircraft.icao);
    }
}

fn write_frame(
    output: &mut dyn Output,
    unix_s: u32,
    aircraft: &Aircraft,
) -> Result<(), ChunkError> {
    output.write_chunk(&pack_frame(unix_s, aircraft))
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
    use std::io::Read;
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

    #[test]
    fn ctrl_c_during_a_stuck_write_stops() {
        let pty = Pty::open();
        let stop = Arc::new(AtomicBool::new(false));
        let mut uart = open_uart(&pty.path, 115_200, Arc::clone(&stop)).expect("pty");
        let flag = Arc::clone(&stop);
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(100));
            flag.store(true, Ordering::Relaxed);
        });
        let started = Instant::now();
        let err = uart
            .write_within(&vec![0xA5; 1024 * 1024], Duration::from_secs(5))
            .expect_err("stop");
        assert!(matches!(err, ChunkError::Stopped), "{err:?}");
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn a_stuck_port_fails_within_the_write_budget() {
        let pty = Pty::open();
        let stop = Arc::new(AtomicBool::new(false));
        let mut uart = open_uart(&pty.path, 115_200, stop).expect("pty");
        let budget = Duration::from_millis(300);
        let started = Instant::now();
        let err = uart
            .write_within(&vec![0xA5; 1024 * 1024], budget)
            .expect_err("drain");
        let elapsed = started.elapsed();
        assert!(
            matches!(err, ChunkError::Failed(ref io) if io.kind() == io::ErrorKind::TimedOut),
            "{err:?}"
        );
        assert!(elapsed >= budget);
        assert!(elapsed < Duration::from_secs(2));
    }

    #[test]
    fn the_pty_master_returns_what_was_written() {
        let mut pty = Pty::open();
        let stop = Arc::new(AtomicBool::new(false));
        let mut uart = open_uart(&pty.path, 115_200, stop).expect("pty");
        let packet = [
            0x54, 0x52, 0x43, 0x4b, 0x00, 0x0a, 0x00, 0x00, 0x00, 0x0f, 0x08,
        ];
        uart.write_within(&packet, Duration::from_secs(2))
            .expect("chunk");
        pty.master
            .set_timeout(Duration::from_secs(1))
            .expect("timeout");
        let mut buf = [0u8; 64];
        let n = pty.master.read(&mut buf).expect("read");
        assert_eq!(&buf[..n], &packet);
    }

    #[test]
    fn a_nonempty_queue_does_not_stall_the_reader_for_a_full_slice() {
        assert_eq!(uart_wait(0), Duration::ZERO);
        assert!(uart_wait(30) > Duration::ZERO);
        assert!(uart_wait(30) < WRITE_SLICE);
        assert!(uart_wait(4096) < WRITE_SLICE);

        let pty = Pty::open();
        let stop = Arc::new(AtomicBool::new(false));
        let mut uart = open_uart(&pty.path, DEFAULT_BAUD, stop).expect("pty");
        let mut stalled = Duration::ZERO;
        let started = Instant::now();
        let result = uart.write_and_wait(
            &[0xA5; 30],
            Duration::from_millis(200),
            |_| Ok(30),
            |pause| stalled += pause,
        );
        assert!(result.is_ok(), "{result:?}");
        assert!(
            stalled < WRITE_SLICE,
            "non-empty queue stalled the reader for {stalled:?}"
        );
        assert!(started.elapsed() < WRITE_SLICE);
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

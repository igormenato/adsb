//! Decode 1090 MHz ADS-B and write one UART frame per airborne position.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use serialport::{ClearBuffer, DataBits, FlowControl, Parity, SerialPort, StopBits, TTYPort};

use trck::{
    DEFAULT_PATH, EmergencyKind, FRAME_LEN, Fix, Lines, Queue, Table, Tracker, check_positions,
    parse_hex_frame,
};

mod doctor;
mod radio;
mod stress;
mod sys;

const DEFAULT_UART: &str = "/dev/serial0";
const DEFAULT_BAUD: u32 = 115_200;
const CHECK_PAUSE: Duration = Duration::from_secs(1);
const POLL_SLICE: Duration = Duration::from_millis(200);
const CONNECT_SLICE: Duration = Duration::from_millis(50);
const QUEUE_CAP: usize = 64;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const RETRY_MIN: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(30);
const KEEPALIVE_IDLE_S: u32 = 30;
const KEEPALIVE_INTERVAL_S: u32 = 10;
const KEEPALIVE_PROBES: u32 = 3;
const MAX_LINE: usize = 8192;

#[derive(Parser, Debug)]
#[command(
    about = "Send each decoded ADS-B position to a UART as it arrives.",
    args_conflicts_with_subcommands = true
)]
struct Args {
    #[command(subcommand)]
    command: Option<Cmd>,
    /// RTL-SDR device index. Used when --check and --hex are off.
    #[arg(long, default_value_t = 0)]
    rtlsdr: usize,
    /// Newline hex Mode S frames. For tests and a Beast-less feed.
    #[arg(long, conflicts_with = "check", value_name = "HOST:PORT")]
    hex: Option<String>,
    /// UART device, or - for stdout.
    #[arg(long, default_value = DEFAULT_UART)]
    uart: String,
    #[arg(long, default_value_t = DEFAULT_BAUD)]
    baud: u32,
    /// Send the known frames instead of reading the radio, to check the UART.
    #[arg(long)]
    check: bool,
    /// Local CSV of each position queued for UART. Default: aircraft.csv in the working directory.
    #[arg(long, default_value = DEFAULT_PATH, value_name = "PATH")]
    table: PathBuf,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Probe this host for RTL-SDR, UART, and CSV setup.
    Doctor,
    /// Generate Mode S in-process and run the live decode path.
    Stress(stress::Args),
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

struct Shared {
    queue: Mutex<Queue>,
    ready: Condvar,
    table: Mutex<Option<Table>>,
}

impl Shared {
    fn new(table: Option<Table>) -> Arc<Self> {
        Arc::new(Self {
            queue: Mutex::new(Queue::new(QUEUE_CAP)),
            ready: Condvar::new(),
            table: Mutex::new(table),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn push(&self, fix: Fix) -> Option<Fix> {
        let dropped = self.lock().push(fix);
        self.ready.notify_one();
        self.append_row(&fix);
        dropped
    }

    fn append_row(&self, fix: &Fix) {
        let mut table = self.table.lock().unwrap_or_else(PoisonError::into_inner);
        let Some(table) = table.as_mut() else {
            return;
        };
        if let Err(err) = table.append(fix) {
            eprintln!("table: {err}");
        }
    }
}

struct Writer {
    sink: Sink,
    stop: Arc<AtomicBool>,
    shared: Arc<Shared>,
}

impl Writer {
    fn has_pending(&self) -> bool {
        self.shared.lock().has_pending()
    }

    fn uart_fd(&self) -> Option<i32> {
        match &self.sink {
            Sink::Uart(port) => Some(port.as_raw_fd()),
            Sink::Stdout => None,
        }
    }

    fn enqueue(&mut self, fix: Fix) {
        if let Some(dropped) = self.shared.push(fix) {
            log_drop(&dropped);
        }
    }

    fn write_some(&mut self) -> Result<(), ChunkError> {
        loop {
            if self.stop.load(Ordering::Relaxed) {
                return Err(ChunkError::Stopped);
            }
            let mut buf = [0u8; FRAME_LEN];
            let n = {
                let mut q = self.shared.lock();
                if !q.begin() {
                    return Ok(());
                }
                let rest = q.unsent();
                buf[..rest.len()].copy_from_slice(rest);
                rest.len()
            };
            let wrote = match write_sink(&mut self.sink, &buf[..n]) {
                Ok(0) => {
                    return Err(ChunkError::Failed(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "uart write wrote nothing",
                    )));
                }
                Ok(n) => n,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) => return Err(ChunkError::Failed(err)),
            };
            self.shared.lock().advance(wrote);
        }
    }

    fn wait_idle(&self, timeout: Duration) {
        let q = self.shared.lock();
        if q.has_pending() {
            return;
        }
        match self.shared.ready.wait_timeout(q, timeout) {
            Ok(_) => {}
            Err(poison) => {
                let _ = poison.into_inner();
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

fn log_fix(fix: &Fix) {
    eprintln!("fix icao={:06x}", fix.aircraft.icao);
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

struct HexLines {
    reader: TcpStream,
    buf: Lines,
    tracker: Tracker,
}

enum Pull {
    Bytes,
    Eof,
}

impl HexLines {
    fn pull(&mut self) -> Result<Pull, io::Error> {
        let mut tmp = [0u8; 4096];
        loop {
            match self.reader.read(&mut tmp) {
                Ok(0) => return Ok(Pull::Eof),
                Ok(n) => self.buf.append(&tmp[..n]),
                Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Ok(Pull::Bytes),
                Err(err) => return Err(err),
            }
        }
    }
}

fn open_failed(path: &str, err: impl std::fmt::Display) -> ExitCode {
    eprintln!("cannot open {path}: {err}");
    ExitCode::from(1)
}

fn open_table(path: &Path) -> Option<Table> {
    match Table::open(path) {
        Ok(table) => Some(table),
        Err(err) => {
            eprintln!("cannot open {}: {err}", path.display());
            None
        }
    }
}

fn open_uart(
    path: &str,
    baud: u32,
    stop: Arc<AtomicBool>,
    shared: Arc<Shared>,
) -> Result<Writer, ExitCode> {
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
        shared,
    })
}

fn open_output(
    path: &str,
    baud: u32,
    stop: Arc<AtomicBool>,
    table: Option<Table>,
) -> Result<Writer, ExitCode> {
    let shared = Shared::new(table);
    if path == "-" {
        return Ok(Writer {
            sink: Sink::Stdout,
            stop,
            shared,
        });
    }
    open_uart(path, baud, stop, shared)
}

fn resolve_addr(text: &str) -> Option<Vec<SocketAddr>> {
    let addrs: Vec<_> = text.to_socket_addrs().ok()?.collect();
    if addrs.is_empty() { None } else { Some(addrs) }
}

fn grow(delay: Duration) -> Duration {
    delay.saturating_mul(2).min(RETRY_MAX)
}

fn now_unix() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn main() -> ExitCode {
    let args = Args::parse();
    match args.command {
        Some(Cmd::Doctor) => return doctor::run(),
        Some(Cmd::Stress(opts)) => return stress::run(opts),
        None => {}
    }
    let stop = Arc::new(AtomicBool::new(false));
    let table = open_table(&args.table);
    let mut writer = match open_output(&args.uart, args.baud, Arc::clone(&stop), table) {
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
        eprintln!(
            "check uart={} baud={} table={}",
            args.uart,
            args.baud,
            args.table.display()
        );
        return run_check(&mut writer, &stop);
    }

    if let Some(text) = args.hex.as_deref() {
        let Some(addrs) = resolve_addr(text) else {
            eprintln!("hex {text}: expected host:port");
            return ExitCode::from(1);
        };
        eprintln!(
            "hex={text} uart={} baud={} table={}",
            args.uart,
            args.baud,
            args.table.display()
        );
        return run_hex(&addrs, text, &mut writer, &stop);
    }

    eprintln!(
        "rtlsdr={} uart={} baud={} table={}",
        args.rtlsdr,
        args.uart,
        args.baud,
        args.table.display()
    );
    run_radio(args.rtlsdr, writer, stop)
}

fn run_radio(device: usize, writer: Writer, stop: Arc<AtomicBool>) -> ExitCode {
    let shared = Arc::clone(&writer.shared);
    let radio_stop = Arc::clone(&stop);
    let radio = thread::spawn(move || {
        let enqueue_shared = Arc::clone(&shared);
        match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(rt) => rt.block_on(radio::run_rtlsdr(
                device,
                move |fix| {
                    log_fix(&fix);
                    if let Some(dropped) = enqueue_shared.push(fix) {
                        log_drop(&dropped);
                    }
                },
                &radio_stop,
            )),
            Err(err) => eprintln!("rtlsdr runtime: {err}"),
        }
        radio_stop.store(true, Ordering::Relaxed);
        shared.ready.notify_one();
    });
    let code = run_writer(writer, &stop, None);
    stop.store(true, Ordering::Relaxed);
    let _ = radio.join();
    code
}

fn run_writer(mut writer: Writer, stop: &AtomicBool, done: Option<&AtomicBool>) -> ExitCode {
    let mut drain_from = None;
    loop {
        if stop.load(Ordering::Relaxed) {
            eprintln!("stopped");
            return ExitCode::SUCCESS;
        }
        if let Err(err) = writer.write_some() {
            return write_exit(err);
        }
        let finished = done.is_some_and(|flag| flag.load(Ordering::Relaxed));
        if finished && !writer.has_pending() {
            return ExitCode::SUCCESS;
        }
        if finished {
            let started = drain_from.get_or_insert_with(Instant::now);
            if started.elapsed() >= Duration::from_secs(2) {
                return ExitCode::SUCCESS;
            }
        }
        if writer.has_pending() {
            if let Some(fd) = writer.uart_fd() {
                let mut poll_fd = sys::pollfd(fd, libc::POLLOUT);
                if let Err(err) = sys::poll(std::slice::from_mut(&mut poll_fd), POLL_SLICE) {
                    eprintln!("uart poll: {err}");
                    return ExitCode::from(1);
                }
            }
            continue;
        }
        writer.wait_idle(POLL_SLICE);
    }
}

fn run_check(writer: &mut Writer, stop: &AtomicBool) -> ExitCode {
    while !stop.load(Ordering::Relaxed) {
        for position in check_positions() {
            log_fix(&position);
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

fn run_hex(addrs: &[SocketAddr], text: &str, writer: &mut Writer, stop: &AtomicBool) -> ExitCode {
    let mut delay = RETRY_MIN;
    while !stop.load(Ordering::Relaxed) {
        match dial_first(addrs, stop, text) {
            Ok(stream) => {
                eprintln!("hex {text} connected");
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

fn dial_first(addrs: &[SocketAddr], stop: &AtomicBool, text: &str) -> Result<TcpStream, DialError> {
    if addrs.is_empty() {
        eprintln!("hex {text}: expected host:port");
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
                eprintln!("hex {text}: {err}");
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
        eprintln!("hex {text} keepalive: {err}");
        return Pump::Disconnected;
    }
    let mut lines = HexLines {
        reader: stream,
        buf: Lines::new(MAX_LINE),
        tracker: Tracker::new(),
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
            eprintln!("hex {text}: {err}");
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
                    enqueue_hex(&mut lines, writer);
                    eprintln!("hex {text} disconnected");
                    return end_socket(writer);
                }
                Ok(Pull::Bytes) => {
                    enqueue_hex(&mut lines, writer);
                    fresh = true;
                    if socket_revents & (libc::POLLHUP | libc::POLLERR) != 0 {
                        eprintln!("hex {text} disconnected");
                        return end_socket(writer);
                    }
                }
                Err(err) => {
                    eprintln!("hex {text}: {err}");
                    enqueue_hex(&mut lines, writer);
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

fn enqueue_hex(lines: &mut HexLines, writer: &mut Writer) {
    let timestamp = now_unix();
    lines.buf.drain(|line| {
        let Some(frame) = parse_hex_frame(line) else {
            return;
        };
        let Some(fix) = lines.tracker.ingest(timestamp, &frame) else {
            return;
        };
        log_fix(&fix);
        writer.enqueue(fix);
    });
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

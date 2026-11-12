//! Hex Mode S input from a socket or stdin.

use std::io::{self, Read};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use nix::errno::Errno;
use nix::poll::{PollFd, PollFlags};
use nix::sys::socket::{setsockopt, sockopt};
use nix::unistd::isatty;

use trck::{Lines, Tracker, parse_hex_frame};

use super::sys;
use super::writer::{ChunkError, Writer, log_fix, write_exit};
use super::{POLL_SLICE, Stop, now_unix, sleep_until_stop};

const CONNECT_SLICE: Duration = Duration::from_millis(50);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const RETRY_MIN: Duration = Duration::from_secs(1);
const RETRY_MAX: Duration = Duration::from_secs(30);
const KEEPALIVE_IDLE_S: u32 = 30;
const KEEPALIVE_INTERVAL_S: u32 = 10;
const KEEPALIVE_PROBES: u32 = 3;
const MAX_LINE: usize = 8192;

struct HexLines<R> {
    reader: R,
    buf: Lines,
    tracker: Tracker,
}

enum Pull {
    Bytes,
    Eof,
}

impl<R: Read> HexLines<R> {
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
#[derive(Debug)]
enum Pump {
    Stopped,
    Disconnected,
    Uart(io::Error),
}

fn grow(delay: Duration) -> Duration {
    delay.saturating_mul(2).min(RETRY_MAX)
}

pub(crate) fn resolve_addr(text: &str) -> Option<Vec<SocketAddr>> {
    let addrs: Vec<_> = text.to_socket_addrs().ok()?.collect();
    if addrs.is_empty() { None } else { Some(addrs) }
}

pub(crate) fn run_hex_stdin(writer: &mut Writer, stop: &Stop) -> ExitCode {
    let stdin = io::stdin();
    if !isatty(&stdin).unwrap_or(false)
        && let Err(err) = sys::set_nonblocking(&stdin)
    {
        eprintln!("hex -: {err}");
        return ExitCode::from(1);
    }
    let mut lines = HexLines {
        reader: stdin,
        buf: Lines::new(MAX_LINE),
        tracker: Tracker::new(),
    };
    match pump_hex(&mut lines, writer, stop, "-") {
        Pump::Stopped => write_exit(ChunkError::Stopped),
        Pump::Uart(err) => write_exit(ChunkError::Failed(err)),
        Pump::Disconnected => {
            eprintln!("stopped");
            ExitCode::SUCCESS
        }
    }
}

pub(crate) fn run_hex(
    addrs: &[SocketAddr],
    text: &str,
    writer: &mut Writer,
    stop: &Stop,
) -> ExitCode {
    let mut delay = RETRY_MIN;
    while !stop.is_set() {
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
        if stop.is_set() {
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

fn dial_first(addrs: &[SocketAddr], stop: &Stop, text: &str) -> Result<TcpStream, DialError> {
    if addrs.is_empty() {
        eprintln!("hex {text}: expected host:port");
        return Err(DialError::Failed(io::Error::new(
            io::ErrorKind::InvalidInput,
            "expected host:port",
        )));
    }
    let mut last = None;
    for addr in addrs {
        if stop.is_set() {
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

fn dial(addr: &SocketAddr, stop: &Stop, timeout: Duration) -> Result<TcpStream, DialError> {
    if stop.is_set() {
        return Err(DialError::Stopped);
    }
    let sock = sys::tcp_socket(addr).map_err(DialError::Failed)?;
    match sys::connect(&sock, addr) {
        Ok(()) => {}
        Err(err) if pending_connect(&err) => wait_connected(sock.as_fd(), stop, timeout)?,
        Err(err) => return Err(DialError::Failed(err)),
    }
    Ok(TcpStream::from(sock))
}

fn pending_connect(err: &io::Error) -> bool {
    err.raw_os_error().is_some_and(|code| {
        let err = Errno::from_raw(code);
        err == Errno::EINPROGRESS
            || err == Errno::EAGAIN
            || err == Errno::EWOULDBLOCK
            || err == Errno::EINTR
    })
}

fn wait_connected(fd: BorrowedFd<'_>, stop: &Stop, timeout: Duration) -> Result<(), DialError> {
    let deadline = Instant::now() + timeout;
    loop {
        if stop.is_set() {
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
        let mut poll_fd = PollFd::new(fd, PollFlags::POLLOUT);
        let rc = match sys::poll(std::slice::from_mut(&mut poll_fd), slice) {
            Ok(rc) => rc,
            Err(err) => return Err(DialError::Failed(err)),
        };
        if rc == 0 {
            continue;
        }
        let revents = poll_fd.revents().unwrap_or_else(PollFlags::empty);
        if !revents.intersects(PollFlags::POLLOUT | PollFlags::POLLERR | PollFlags::POLLHUP) {
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
    setsockopt(stream, sockopt::KeepAlive, &true)?;
    setsockopt(stream, sockopt::TcpKeepIdle, &KEEPALIVE_IDLE_S)?;
    setsockopt(stream, sockopt::TcpKeepInterval, &KEEPALIVE_INTERVAL_S)?;
    setsockopt(stream, sockopt::TcpKeepCount, &KEEPALIVE_PROBES)?;
    Ok(())
}

fn pump(stream: TcpStream, writer: &mut Writer, stop: &Stop, text: &str) -> Pump {
    if let Err(err) = enable_keepalive(&stream) {
        eprintln!("hex {text} keepalive: {err}");
        return Pump::Disconnected;
    }
    let mut lines = HexLines {
        reader: stream,
        buf: Lines::new(MAX_LINE),
        tracker: Tracker::new(),
    };
    pump_hex(&mut lines, writer, stop, text)
}

fn pump_hex<R: Read + AsRawFd>(
    lines: &mut HexLines<R>,
    writer: &mut Writer,
    stop: &Stop,
    text: &str,
) -> Pump {
    loop {
        if stop.is_set() {
            return Pump::Stopped;
        }
        let (socket_revents, uart_ready) = {
            let reader = sys::as_fd(&lines.reader);
            let uart = writer.has_pending().then(|| writer.uart_fd()).flatten();
            let mut fds_two;
            let mut fds_one;
            let fds: &mut [PollFd<'_>] = if let Some(uart) = uart {
                fds_two = [
                    PollFd::new(reader, PollFlags::POLLIN),
                    PollFd::new(uart, PollFlags::POLLOUT),
                ];
                &mut fds_two
            } else {
                fds_one = [PollFd::new(reader, PollFlags::POLLIN)];
                &mut fds_one
            };
            if let Err(err) = sys::poll(fds, POLL_SLICE) {
                eprintln!("hex {text}: {err}");
                return end_socket(writer);
            }
            let socket_revents = fds[0].revents().unwrap_or_else(PollFlags::empty);
            let uart_ready = fds.get(1).is_some_and(|fd| {
                fd.revents()
                    .unwrap_or_else(PollFlags::empty)
                    .intersects(PollFlags::POLLOUT | PollFlags::POLLERR | PollFlags::POLLHUP)
            });
            (socket_revents, uart_ready)
        };
        if stop.is_set() {
            return Pump::Stopped;
        }
        let mut fresh = false;
        if socket_revents.intersects(PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR) {
            match lines.pull() {
                Ok(Pull::Eof) => {
                    enqueue_hex(lines, writer);
                    eprintln!("hex {text} disconnected");
                    return end_socket(writer);
                }
                Ok(Pull::Bytes) => {
                    enqueue_hex(lines, writer);
                    fresh = true;
                    if socket_revents.intersects(PollFlags::POLLHUP | PollFlags::POLLERR) {
                        eprintln!("hex {text} disconnected");
                        return end_socket(writer);
                    }
                }
                Err(err) => {
                    eprintln!("hex {text}: {err}");
                    enqueue_hex(lines, writer);
                    return end_socket(writer);
                }
            }
        }
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

fn enqueue_hex<R>(lines: &mut HexLines<R>, writer: &mut Writer) {
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

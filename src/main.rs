//! Read readsb `--net-json-port` and write one UART frame per position.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::os::fd::AsRawFd;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use clap::Parser;
use serialport::{ClearBuffer, DataBits, FlowControl, Parity, SerialPort, StopBits, TTYPort};

use trck::{fix_from_json_line, pack_frame, snapshot_from_aircraft_json, Fix};

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
    /// `write` then wait until the kernel output queue is empty. Neither step blocks in the kernel:
    /// the fd is non-blocking, and the drain polls `TIOCOUTQ` so Ctrl-C is noticed.
    fn write_within(&mut self, mut data: &[u8], budget: Duration) -> Result<(), ChunkError> {
        let deadline = Instant::now() + budget;
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
                Err(err) if retry(&err) => {}
                Err(err) => return Err(ChunkError::Failed(err)),
            }
        }
        let fd = self.port.as_raw_fd();
        loop {
            self.tick(deadline)?;
            match bytes_queued(fd) {
                Ok(0) => return Ok(()),
                Ok(_) => thread::sleep(WRITE_SLICE),
                Err(err) => return Err(ChunkError::Failed(err)),
            }
        }
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

struct JsonLines {
    reader: TcpStream,
    pending: Vec<u8>,
    skipping: bool,
}

impl JsonLines {
    fn next_line(&mut self, stop: &AtomicBool) -> Result<Option<Vec<u8>>, Pump> {
        loop {
            if stop.load(Ordering::Relaxed) {
                return Err(Pump::Stopped);
            }
            if let Some(split) = self.pending.iter().position(|byte| *byte == b'\n') {
                let mut line: Vec<u8> = self.pending.drain(..=split).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if self.skipping {
                    self.skipping = false;
                    continue;
                }
                return Ok(Some(line));
            }
            if self.pending.len() > MAX_LINE {
                self.pending.clear();
                self.skipping = true;
            }
            let mut tmp = [0u8; 4096];
            match self.reader.read(&mut tmp) {
                Ok(0) => return Ok(None),
                Ok(n) => self.pending.extend_from_slice(&tmp[..n]),
                Err(err) if retry(&err) => {}
                Err(err) => {
                    eprintln!("net-json read: {err}");
                    return Err(Pump::Disconnected);
                }
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

fn resolve_net_json(text: &str) -> Option<SocketAddr> {
    text.to_socket_addrs().ok()?.next()
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
    let Some(addr) = resolve_net_json(text) else {
        eprintln!("net-json {text}: expected host:port");
        return ExitCode::from(1);
    };
    eprintln!("net-json={text} uart={} baud={}", args.uart, args.baud);
    run_port(addr, text, &mut *output, &stop)
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
            let frame = pack_frame(&Fix {
                unix_s: snapshot.unix_s,
                aircraft: aircraft.clone(),
            });
            if let Err(err) = output.write_chunk(&frame) {
                return write_failed(err);
            }
            eprintln!("fix t={} icao={:06x}", snapshot.unix_s, aircraft.icao);
        }
        sleep_until_stop(stop, interval);
    }
    eprintln!("stopped");
    ExitCode::SUCCESS
}

fn run_port(addr: SocketAddr, text: &str, output: &mut dyn Output, stop: &AtomicBool) -> ExitCode {
    let mut delay = RETRY_MIN;
    while !stop.load(Ordering::Relaxed) {
        match TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT) {
            Ok(stream) => {
                eprintln!("net-json {text} connected");
                match pump(stream, output, stop) {
                    Pump::Stopped => {
                        eprintln!("stopped");
                        return ExitCode::SUCCESS;
                    }
                    Pump::Uart(err) => {
                        eprintln!("uart write failed: {err}");
                        return ExitCode::from(1);
                    }
                    Pump::Disconnected => {
                        eprintln!("net-json {text} disconnected");
                        delay = RETRY_MIN;
                    }
                }
            }
            Err(err) => eprintln!("net-json {text}: {err}"),
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

fn pump(stream: TcpStream, output: &mut dyn Output, stop: &AtomicBool) -> Pump {
    if let Err(err) = stream.set_read_timeout(Some(READ_SLICE)) {
        eprintln!("net-json read timeout: {err}");
        return Pump::Disconnected;
    }
    let mut lines = JsonLines {
        reader: stream,
        pending: Vec::new(),
        skipping: false,
    };
    loop {
        let line = match lines.next_line(stop) {
            Ok(Some(line)) => line,
            Ok(None) => return Pump::Disconnected,
            Err(pump) => return pump,
        };
        let Some(fix) = fix_from_json_line(&line) else {
            continue;
        };
        let frame = pack_frame(&fix);
        if let Err(err) = output.write_chunk(&frame) {
            return match err {
                ChunkError::Stopped => Pump::Stopped,
                ChunkError::Failed(err) => Pump::Uart(err),
            };
        }
        eprintln!("fix t={} icao={:06x}", fix.unix_s, fix.aircraft.icao);
    }
}

fn write_failed(err: ChunkError) -> ExitCode {
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

    use super::*;

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
}

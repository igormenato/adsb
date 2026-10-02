//! Read readsb `aircraft.json` and write one track snapshot to a UART each second.

use std::fs;
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use clap::Parser;
use serialport::{ClearBuffer, DataBits, FlowControl, Parity, SerialPort, StopBits, TTYPort};

use adsb_uart_sender::{pack_snapshot, pack_snapshot_json, snapshot_from_aircraft_json};

const DEFAULT_UART: &str = "/dev/serial0";
const DEFAULT_BAUD: u32 = 115_200;
const DEFAULT_JSON: &str = "/run/readsb/aircraft.json";
const DEFAULT_INTERVAL_S: f64 = 1.0;
const SAMPLE_AIRCRAFT_JSON: &str = include_str!("../sample/aircraft.json");
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const WRITE_SLICE: Duration = Duration::from_millis(50);

#[derive(Parser, Debug)]
#[command(about = "Send readsb aircraft tracks to a UART once a second.")]
struct Args {
    /// Path to readsb aircraft.json.
    #[arg(long, default_value = DEFAULT_JSON)]
    json: String,
    /// UART device, or - for stdout.
    #[arg(long, default_value = DEFAULT_UART)]
    uart: String,
    #[arg(long, default_value_t = DEFAULT_BAUD)]
    baud: u32,
    /// Seconds between snapshots.
    #[arg(long, default_value_t = DEFAULT_INTERVAL_S)]
    interval: f64,
    /// Encoding written to the UART each interval.
    #[arg(long, value_enum, default_value = "json")]
    format: Format,
    /// Send the built-in sample instead of reading aircraft.json.
    #[arg(long)]
    sample: bool,
}

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum Format {
    /// Binary TRCK packet.
    Struct,
    /// One JSON object per line.
    Json,
}

#[derive(Debug)]
enum ChunkError {
    Stopped,
    Failed(io::Error),
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

fn main() -> ExitCode {
    let args = Args::parse();
    let stop = Arc::new(AtomicBool::new(false));
    let mut output = match open_output(&args.uart, args.baud, Arc::clone(&stop)) {
        Ok(output) => output,
        Err(code) => return code,
    };
    let interval =
        Duration::try_from_secs_f64(args.interval.max(0.2)).unwrap_or(Duration::from_secs(1));
    {
        let stop = Arc::clone(&stop);
        if ctrlc::set_handler(move || stop.store(true, Ordering::Relaxed)).is_err() {
            eprintln!("could not install the interrupt handler");
        }
    }

    eprintln!(
        "json={} uart={} baud={} interval={}s format={}",
        if args.sample {
            "sample"
        } else {
            args.json.as_str()
        },
        args.uart,
        args.baud,
        interval.as_secs_f64(),
        match args.format {
            Format::Struct => "struct",
            Format::Json => "json",
        }
    );

    while !stop.load(Ordering::Relaxed) {
        let loaded = if args.sample {
            Ok(SAMPLE_AIRCRAFT_JSON.as_bytes().to_vec())
        } else {
            fs::read(&args.json)
        };
        match loaded {
            Ok(bytes) => match snapshot_from_aircraft_json(&bytes) {
                Ok(snapshot) => {
                    let packet = match args.format {
                        Format::Struct => pack_snapshot(&snapshot),
                        Format::Json => pack_snapshot_json(&snapshot),
                    };
                    if let Err(err) = output.write_chunk(&packet) {
                        return match err {
                            ChunkError::Stopped => {
                                eprintln!("stopped");
                                ExitCode::SUCCESS
                            }
                            ChunkError::Failed(err) => {
                                eprintln!("uart write failed: {err}");
                                ExitCode::from(1)
                            }
                        };
                    }
                    eprintln!(
                        "snapshot t={} aircraft={}",
                        snapshot.unix_s,
                        snapshot.aircraft.len()
                    );
                }
                Err(err) => eprintln!("aircraft.json: {err}"),
            },
            Err(err) => eprintln!("waiting for aircraft.json at {}: {err}", args.json),
        }
        sleep_until_stop(&stop, interval);
    }

    eprintln!("stopped");
    ExitCode::SUCCESS
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
    use std::ffi::CStr;

    use super::*;

    struct Pty {
        master: i32,
        path: String,
    }

    impl Pty {
        fn open() -> Self {
            let master = unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY) };
            assert!(master >= 0, "{}", io::Error::last_os_error());
            assert_eq!(unsafe { libc::grantpt(master) }, 0);
            assert_eq!(unsafe { libc::unlockpt(master) }, 0);
            let mut name = [0u8; 64];
            let rc = unsafe {
                libc::ptsname_r(master, name.as_mut_ptr().cast::<libc::c_char>(), name.len())
            };
            assert_eq!(rc, 0, "{}", io::Error::last_os_error());
            let path = CStr::from_bytes_until_nul(&name)
                .expect("pty name")
                .to_string_lossy()
                .into_owned();
            Self { master, path }
        }
    }

    impl Drop for Pty {
        fn drop(&mut self) {
            unsafe {
                libc::close(self.master);
            }
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
    fn a_drained_port_accepts_the_chunk() {
        let pty = Pty::open();
        let master = pty.master;
        let stop_reader = Arc::new(AtomicBool::new(false));
        let reader_stop = Arc::clone(&stop_reader);
        let reader = thread::spawn(move || {
            let mut buf = [0u8; 64];
            while !reader_stop.load(Ordering::Relaxed) {
                let mut pollfd = libc::pollfd {
                    fd: master,
                    events: libc::POLLIN,
                    revents: 0,
                };
                let ready = unsafe { libc::poll(&mut pollfd, 1, 50) };
                if ready > 0 {
                    unsafe {
                        libc::read(master, buf.as_mut_ptr().cast::<libc::c_void>(), buf.len());
                    }
                }
            }
        });
        let stop = Arc::new(AtomicBool::new(false));
        let mut uart = open_uart(&pty.path, 115_200, stop).expect("pty");
        uart.write_within(b"TRCK", Duration::from_secs(2))
            .expect("chunk");
        stop_reader.store(true, Ordering::Relaxed);
        reader.join().expect("reader");
    }
}

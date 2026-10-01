//! Read Beast from readsb and write it to a UART.

use std::io::{self, Read, Write};
use std::net::{SocketAddr, TcpStream, ToSocketAddrs};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::{Parser, ValueEnum};
use serialport::{ClearBuffer, DataBits, FlowControl, Parity, SerialPort, StopBits};

use adsb_uart_sender::StructForwarder;

const DEFAULT_UART: &str = "/dev/serial0";
const DEFAULT_BAUD: u32 = 115_200;
const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_PORT: u16 = 30_005;

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Mode {
    Raw,
    Struct,
}

#[derive(Parser, Debug)]
#[command(about = "Forward readsb Beast output to a UART as raw Beast or packed structs.")]
struct Args {
    #[arg(long, value_enum)]
    mode: Mode,
    /// UART device, or - for stdout.
    #[arg(long, default_value = DEFAULT_UART)]
    uart: String,
    #[arg(long, default_value_t = DEFAULT_BAUD)]
    baud: u32,
    #[arg(long, default_value = DEFAULT_HOST)]
    beast_host: String,
    #[arg(long, default_value_t = DEFAULT_PORT)]
    beast_port: u16,
}

trait Output {
    fn write_chunk(&mut self, data: &[u8]) -> io::Result<()>;
}

struct StdoutOut;

impl Output for StdoutOut {
    fn write_chunk(&mut self, data: &[u8]) -> io::Result<()> {
        let mut out = io::stdout().lock();
        out.write_all(data)?;
        out.flush()
    }
}

struct UartOut {
    port: Box<dyn SerialPort>,
}

impl Output for UartOut {
    fn write_chunk(&mut self, mut data: &[u8]) -> io::Result<()> {
        while !data.is_empty() {
            let wrote = self.port.write(data)?;
            if wrote == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "UART write failed",
                ));
            }
            data = &data[wrote..];
        }
        // flush is tcdrain: this chunk leaves the port before the next write.
        self.port.flush()
    }
}

fn open_output(path: &str, baud: u32) -> Result<Box<dyn Output>, ExitCode> {
    if path == "-" {
        return Ok(Box::new(StdoutOut));
    }
    let port = match serialport::new(path, baud)
        .data_bits(DataBits::Eight)
        .parity(Parity::None)
        .stop_bits(StopBits::One)
        .flow_control(FlowControl::None)
        // Writes block until the kernel accepts them. tcdrain then waits
        // until those bytes have left the adapter.
        .timeout(Duration::from_secs(30))
        .open()
    {
        Ok(port) => port,
        Err(err) => {
            eprintln!("cannot open {path}: {err}");
            return Err(ExitCode::from(1));
        }
    };
    if let Err(err) = port.clear(ClearBuffer::All) {
        eprintln!("cannot open {path}: {err}");
        return Err(ExitCode::from(1));
    }
    Ok(Box::new(UartOut { port }))
}

fn wall_clock() -> (f64, u64) {
    let since = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let micros = since.as_micros() as u64;
    (micros as f64 / 1_000_000.0, micros)
}

fn connect(host: &str, port: u16, stop: &AtomicBool) -> Option<TcpStream> {
    loop {
        if stop.load(Ordering::Relaxed) {
            return None;
        }
        let addr = match resolve(host, port) {
            Some(addr) => addr,
            None => {
                eprintln!("waiting for readsb at {host}:{port}: address lookup failed");
                thread::sleep(Duration::from_secs(1));
                continue;
            }
        };
        match TcpStream::connect_timeout(&addr, Duration::from_secs(5)) {
            Ok(sock) => {
                if let Err(err) = sock.set_read_timeout(Some(Duration::from_secs(1))) {
                    eprintln!("beast connection lost: {err}");
                    thread::sleep(Duration::from_secs(1));
                    continue;
                }
                eprintln!("connected to readsb at {host}:{port}");
                return Some(sock);
            }
            Err(err) => {
                eprintln!("waiting for readsb at {host}:{port}: {err}");
                thread::sleep(Duration::from_secs(1));
            }
        }
    }
}

fn resolve(host: &str, port: u16) -> Option<SocketAddr> {
    (host, port).to_socket_addrs().ok()?.next()
}

fn main() -> ExitCode {
    let args = Args::parse();
    let mut output = match open_output(&args.uart, args.baud) {
        Ok(output) => output,
        Err(code) => return code,
    };

    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = Arc::clone(&stop);
        if ctrlc::set_handler(move || stop.store(true, Ordering::Relaxed)).is_err() {
            eprintln!("could not install the interrupt handler");
        }
    }

    let mut forwarder = StructForwarder::new();
    let mut total_raw = 0usize;
    let mut last_log = Instant::now();
    let mut buf = [0u8; 4096];
    let mut records = Vec::with_capacity(4096);
    eprintln!(
        "mode={} uart={} baud={} beast={}:{}",
        match args.mode {
            Mode::Raw => "raw",
            Mode::Struct => "struct",
        },
        args.uart,
        args.baud,
        args.beast_host,
        args.beast_port
    );

    while !stop.load(Ordering::Relaxed) {
        let Some(mut sock) = connect(&args.beast_host, args.beast_port, &stop) else {
            break;
        };
        loop {
            if stop.load(Ordering::Relaxed) {
                break;
            }
            match sock.read(&mut buf) {
                Ok(0) => {
                    eprintln!("readsb closed the connection");
                    break;
                }
                Ok(n) => {
                    let write = if args.mode == Mode::Raw {
                        let result = output.write_chunk(&buf[..n]);
                        if result.is_ok() {
                            total_raw += n;
                        }
                        result
                    } else {
                        records.clear();
                        forwarder.feed(&buf[..n], wall_clock, &mut records);
                        if records.is_empty() {
                            Ok(())
                        } else {
                            output.write_chunk(&records)
                        }
                    };
                    if let Err(err) = write {
                        eprintln!("beast connection lost: {err}");
                        break;
                    }
                }
                Err(err)
                    if err.kind() == io::ErrorKind::WouldBlock
                        || err.kind() == io::ErrorKind::TimedOut => {}
                Err(err) => {
                    eprintln!("beast connection lost: {err}");
                    break;
                }
            }
            if last_log.elapsed() >= Duration::from_secs(1) {
                if args.mode == Mode::Raw {
                    eprintln!("raw forwarded {total_raw} bytes");
                } else {
                    eprintln!(
                        "struct sent {} crc_dropped {}",
                        forwarder.sent, forwarder.crc_drops
                    );
                }
                last_log = Instant::now();
            }
        }
    }

    eprintln!("stopped");
    ExitCode::SUCCESS
}

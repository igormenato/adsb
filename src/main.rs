//! Read readsb `aircraft.json` and write one track snapshot to a UART each second.

use std::fs;
use std::io::{self, Write};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use clap::Parser;
use serialport::{ClearBuffer, DataBits, FlowControl, Parity, SerialPort, StopBits};

use adsb_uart_sender::{pack_snapshot, snapshot_from_aircraft_json};

const DEFAULT_UART: &str = "/dev/serial0";
const DEFAULT_BAUD: u32 = 115_200;
const DEFAULT_JSON: &str = "/run/readsb/aircraft.json";
const DEFAULT_INTERVAL_S: f64 = 1.0;

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
    fn write_chunk(&mut self, data: &[u8]) -> io::Result<()> {
        self.port.write_all(data)?;
        self.port.flush()
    }
}

fn open_failed(path: &str, err: impl std::fmt::Display) -> ExitCode {
    eprintln!("cannot open {path}: {err}");
    ExitCode::from(1)
}

fn open_output(path: &str, baud: u32) -> Result<Box<dyn Output>, ExitCode> {
    if path == "-" {
        return Ok(Box::new(StdoutOut));
    }
    let port = serialport::new(path, baud)
        .data_bits(DataBits::Eight)
        .parity(Parity::None)
        .stop_bits(StopBits::One)
        .flow_control(FlowControl::None)
        // Writes block until the kernel accepts them. flush() then waits
        // until those bytes have left the adapter.
        .timeout(Duration::from_secs(30))
        .open()
        .map_err(|err| open_failed(path, err))?;
    port.clear(ClearBuffer::All)
        .map_err(|err| open_failed(path, err))?;
    Ok(Box::new(UartOut { port }))
}

fn main() -> ExitCode {
    let args = Args::parse();
    let mut output = match open_output(&args.uart, args.baud) {
        Ok(output) => output,
        Err(code) => return code,
    };
    let interval =
        Duration::try_from_secs_f64(args.interval.max(0.2)).unwrap_or(Duration::from_secs(1));

    let stop = Arc::new(AtomicBool::new(false));
    {
        let stop = Arc::clone(&stop);
        if ctrlc::set_handler(move || stop.store(true, Ordering::Relaxed)).is_err() {
            eprintln!("could not install the interrupt handler");
        }
    }

    eprintln!(
        "json={} uart={} baud={} interval={}s",
        args.json,
        args.uart,
        args.baud,
        interval.as_secs_f64()
    );

    while !stop.load(Ordering::Relaxed) {
        match fs::read(&args.json) {
            Ok(bytes) => match snapshot_from_aircraft_json(&bytes) {
                Ok(snapshot) => {
                    let packet = pack_snapshot(&snapshot);
                    if let Err(err) = output.write_chunk(&packet) {
                        eprintln!("uart write failed: {err}");
                        return ExitCode::from(1);
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

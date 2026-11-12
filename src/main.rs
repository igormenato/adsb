//! Decode 1090 MHz ADS-B and write one UART frame per airborne position.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use clap::{Parser, Subcommand};

use trck::{DEFAULT_PATH, Replay, ReplayLine, Tracker, is_mode_s};

use writer::Writer;

mod doctor;
mod hex;
mod iq;
mod radio;
mod sim;
mod stress;
mod sys;
mod writer;

const DEFAULT_UART: &str = "/dev/serial0";
const DEFAULT_BAUD: u32 = 115_200;
const CHECK_PAUSE: Duration = Duration::from_secs(1);
const POLL_SLICE: Duration = Duration::from_millis(200);

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
    /// Newline hex or AVR Mode S frames. HOST:PORT, or - for stdin.
    #[arg(long, conflicts_with = "check", value_name = "HOST:PORT|-")]
    hex: Option<String>,
    /// RTL-SDR cu8 I/Q file. Same demodulate2400 path as the dongle.
    #[arg(long, conflicts_with_all = ["check", "hex"], value_name = "FILE")]
    iq: Option<PathBuf>,
    /// Sample rate of --iq. Other rates are resampled to 2.4 MS/s.
    #[arg(long, value_name = "HZ")]
    iq_rate: Option<u32>,
    #[command(flatten)]
    output: OutputArgs,
    /// Send the known frames instead of reading the radio, to check the UART.
    #[arg(long)]
    check: bool,
}

#[derive(clap::Args, Debug)]
struct OutputArgs {
    /// UART device, or - for stdout.
    #[arg(long, default_value = DEFAULT_UART)]
    uart: String,
    #[arg(long, default_value_t = DEFAULT_BAUD)]
    baud: u32,
    /// Local CSV of each position queued for UART.
    #[arg(long, default_value = DEFAULT_PATH, value_name = "PATH")]
    table: PathBuf,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Probe this host for RTL-SDR, UART, and CSV setup.
    Doctor,
    /// Generate Mode S in-process and run the live decode path.
    Stress(stress::Args),
    /// Write a scripted Mode S scenario as ts,hex lines.
    Sim(sim::Args),
    /// Replay recorded `ts,hex` frames through the live pipeline.
    Replay {
        /// CSV of `ts,hex` lines.
        file: PathBuf,
        #[command(flatten)]
        output: OutputArgs,
        /// Playback rate. `0` or `max` runs as fast as possible.
        #[arg(long, default_value = "1", value_parser = parse_pace)]
        speed: Pace,
    },
}

#[derive(Debug)]
struct Stop {
    flag: Mutex<bool>,
    wake: Condvar,
    notify: tokio::sync::Notify,
}

impl Stop {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            flag: Mutex::new(false),
            wake: Condvar::new(),
            notify: tokio::sync::Notify::new(),
        })
    }

    fn request(&self) {
        *self.flag.lock().unwrap_or_else(PoisonError::into_inner) = true;
        self.wake.notify_all();
        self.notify.notify_waiters();
    }

    fn is_set(&self) -> bool {
        *self.flag.lock().unwrap_or_else(PoisonError::into_inner)
    }

    async fn wait(&self) {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_set() {
                return;
            }
            notified.await;
        }
    }
}

fn install_ctrlc(stop: Arc<Stop>) {
    if ctrlc::set_handler(move || stop.request()).is_err() {
        eprintln!("could not install the interrupt handler");
    }
}

#[derive(Clone, Copy, Debug)]
enum Pace {
    Max,
    Rate(f64),
}

fn parse_pace(text: &str) -> Result<Pace, String> {
    if text.eq_ignore_ascii_case("max") {
        return Ok(Pace::Max);
    }
    let n: f64 = text
        .parse()
        .map_err(|_| format!("invalid --speed {text}"))?;
    if n == 0.0 {
        return Ok(Pace::Max);
    }
    if !n.is_finite() || n < 0.0 {
        return Err(format!("invalid --speed {text}"));
    }
    Ok(Pace::Rate(n))
}

impl std::fmt::Display for Pace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Pace::Max => f.write_str("max"),
            Pace::Rate(n) => write!(f, "{n}"),
        }
    }
}

fn fail(err: impl std::fmt::Display) -> ExitCode {
    eprintln!("{err}");
    ExitCode::from(1)
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
        Some(Cmd::Sim(opts)) => return sim::run(opts),
        Some(Cmd::Replay {
            file,
            output,
            speed,
        }) => return run_replay(file, &output.uart, output.baud, &output.table, speed),
        None => {}
    }
    let stop = Stop::new();
    let table = writer::open_table(&args.output.table);
    let mut writer = match writer::open_output(
        &args.output.uart,
        args.output.baud,
        Arc::clone(&stop),
        table,
    ) {
        Ok(writer) => writer,
        Err(err) => return fail(format!("{err:#}")),
    };
    install_ctrlc(Arc::clone(&stop));

    if args.iq_rate.is_some() && args.iq.is_none() {
        return fail("--iq-rate requires --iq");
    }

    if args.check {
        eprintln!(
            "check uart={} baud={} table={}",
            args.output.uart,
            args.output.baud,
            args.output.table.display()
        );
        return writer::run_check(&mut writer, &stop);
    }

    if let Some(path) = args.iq.as_deref() {
        let rate = args.iq_rate.unwrap_or(radio::SAMPLE_HZ);
        eprintln!(
            "iq={} rate={rate} uart={} baud={} table={}",
            path.display(),
            args.output.uart,
            args.output.baud,
            args.output.table.display()
        );
        return run_iq(path, rate, writer, stop);
    }

    if let Some(text) = args.hex.as_deref() {
        eprintln!(
            "hex={text} uart={} baud={} table={}",
            args.output.uart,
            args.output.baud,
            args.output.table.display()
        );
        if text == "-" {
            return hex::run_hex_stdin(&mut writer, &stop);
        }
        let Some(addrs) = hex::resolve_addr(text) else {
            return fail(format!("hex {text}: expected host:port"));
        };
        return hex::run_hex(&addrs, text, &mut writer, &stop);
    }

    eprintln!(
        "rtlsdr={} uart={} baud={} table={}",
        args.rtlsdr,
        args.output.uart,
        args.output.baud,
        args.output.table.display()
    );
    run_radio(args.rtlsdr, writer, stop)
}

fn run_iq(path: &Path, rate: u32, writer: Writer, stop: Arc<Stop>) -> ExitCode {
    let shared = Arc::clone(&writer.shared);
    let csv_on = shared
        .table
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .is_some();
    let radio_stop = Arc::clone(&stop);
    let done = Arc::new(AtomicBool::new(false));
    let n_drop = Arc::new(AtomicU64::new(0));
    let n_csv = Arc::new(AtomicU64::new(0));
    let stats = Arc::new(Mutex::new(radio::IqStats::default()));
    let started = Instant::now();
    let path = path.to_path_buf();
    let reader = {
        let done = Arc::clone(&done);
        let n_drop = Arc::clone(&n_drop);
        let n_csv = Arc::clone(&n_csv);
        let stats = Arc::clone(&stats);
        let shared = Arc::clone(&shared);
        thread::spawn(move || {
            let enqueue_shared = Arc::clone(&shared);
            match radio::run_iq(
                &path,
                rate,
                move |fix| {
                    writer::log_fix(&fix);
                    if csv_on {
                        n_csv.fetch_add(1, Ordering::Relaxed);
                    }
                    if let Some(dropped) = enqueue_shared.push(fix) {
                        n_drop.fetch_add(1, Ordering::Relaxed);
                        writer::log_drop(&dropped);
                    }
                },
                &radio_stop,
            ) {
                Ok(got) => *stats.lock().unwrap_or_else(PoisonError::into_inner) = got,
                Err(err) => eprintln!("iq: {err:#}"),
            }
            done.store(true, Ordering::Relaxed);
            shared.ready.notify_one();
        })
    };
    let code = writer::run_writer(writer, &stop, Some(&done));
    stop.request();
    let _ = reader.join();
    let got = *stats.lock().unwrap_or_else(PoisonError::into_inner);
    let elapsed = started.elapsed().as_secs_f64();
    eprintln!(
        "iq messages={} fixes={} dropped={} csv={} elapsed={elapsed:.3}s",
        got.messages,
        got.fixes,
        n_drop.load(Ordering::Relaxed),
        n_csv.load(Ordering::Relaxed),
    );
    code
}

fn run_replay(file: PathBuf, uart: &str, baud: u32, table_path: &Path, speed: Pace) -> ExitCode {
    let stop = Stop::new();
    install_ctrlc(Arc::clone(&stop));
    let input =
        match fs::File::open(&file).with_context(|| format!("cannot open {}", file.display())) {
            Ok(input) => input,
            Err(err) => return fail(format!("{err:#}")),
        };
    let table = writer::open_table(table_path);
    let mut writer = match writer::open_output(uart, baud, Arc::clone(&stop), table) {
        Ok(writer) => writer,
        Err(err) => return fail(format!("{err:#}")),
    };
    eprintln!(
        "replay file={} uart={uart} baud={baud} speed={speed} table={}",
        file.display(),
        table_path.display()
    );

    let mut tracker = Tracker::new();
    let mut prev = None;
    let mut read = 0usize;
    let mut decoded = 0usize;
    let mut malformed = 0usize;
    let mut fixes = 0usize;
    let mut replay = Replay::new(input);
    loop {
        if stop.is_set() {
            break;
        }
        let line = match replay.next_record() {
            Ok(None) => break,
            Ok(Some(line)) => line,
            Err(err) => {
                eprintln!("{}: {err}", file.display());
                print_replay_summary(read, decoded, malformed, fixes, writer.frames_written);
                return ExitCode::from(1);
            }
        };
        match line {
            ReplayLine::Skip => {}
            ReplayLine::Malformed => malformed += 1,
            ReplayLine::Frame { timestamp, frame } => {
                read += 1;
                if is_mode_s(&frame) {
                    decoded += 1;
                }
                if let Some(prev_ts) = prev {
                    pace_sleep(speed, timestamp - prev_ts, &stop);
                }
                if stop.is_set() {
                    break;
                }
                prev = Some(timestamp);
                if let Some(fix) = tracker.ingest(timestamp, &frame) {
                    fixes += 1;
                    writer::log_fix(&fix);
                    writer.enqueue_at(fix, unix_time(timestamp));
                }
                if let Err(err) = writer.write_some() {
                    print_replay_summary(read, decoded, malformed, fixes, writer.frames_written);
                    return writer::write_exit(err);
                }
            }
        }
    }
    let drain = writer::drain_writer(&mut writer, &stop);
    print_replay_summary(read, decoded, malformed, fixes, writer.frames_written);
    match drain {
        Ok(()) => {
            if stop.is_set() {
                eprintln!("stopped");
            }
            ExitCode::SUCCESS
        }
        Err(err) => writer::write_exit(err),
    }
}

fn print_replay_summary(read: usize, decoded: usize, malformed: usize, fixes: usize, uart: usize) {
    eprintln!(
        "frames read={read} decoded={decoded} malformed={malformed} fixes={fixes} UART frames written={uart}"
    );
}

fn pace_sleep(pace: Pace, dt: f64, stop: &Stop) {
    match pace {
        Pace::Max => {}
        Pace::Rate(n) => {
            if !dt.is_finite() || dt <= 0.0 {
                return;
            }
            let secs = dt / n;
            if !secs.is_finite() || secs <= 0.0 {
                return;
            }
            let mut left = Duration::from_secs_f64(secs);
            let step = Duration::from_millis(50);
            while left > Duration::ZERO {
                if stop.is_set() {
                    return;
                }
                let slice = left.min(step);
                thread::sleep(slice);
                left = left.saturating_sub(slice);
            }
        }
    }
}

fn unix_time(ts: f64) -> SystemTime {
    if !ts.is_finite() || ts <= 0.0 {
        return UNIX_EPOCH;
    }
    let secs = ts.trunc();
    let nanos = ((ts - secs) * 1_000_000_000.0)
        .round()
        .clamp(0.0, 999_999_999.0) as u32;
    UNIX_EPOCH + Duration::new(secs as u64, nanos)
}

fn run_radio(device: usize, writer: Writer, stop: Arc<Stop>) -> ExitCode {
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
                    writer::log_fix(&fix);
                    if let Some(dropped) = enqueue_shared.push(fix) {
                        writer::log_drop(&dropped);
                    }
                },
                &radio_stop,
            )),
            Err(err) => eprintln!("rtlsdr runtime: {err}"),
        }
        radio_stop.request();
        shared.ready.notify_one();
    });
    let code = writer::run_writer(writer, &stop, None);
    stop.request();
    let _ = radio.join();
    code
}

fn sleep_until_stop(stop: &Stop, timeout: Duration) {
    let flag = stop.flag.lock().unwrap_or_else(PoisonError::into_inner);
    let _ = stop
        .wake
        .wait_timeout_while(flag, timeout, |set| !*set)
        .unwrap_or_else(PoisonError::into_inner);
}

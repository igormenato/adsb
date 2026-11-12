//! In-process Mode S firehose through the live decode path.

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use super::{OutputArgs, Stop, now_unix, open_output, open_table, run_writer, sleep_until_stop};
use anyhow::{Result, bail};
use clap::Parser;
use trck::modes::{EVEN, IDENT, ODD, STATUS, VELOCITY, with_icao};
use trck::{Icao, Tracker};

const ICAO_BASE: u32 = 0x10_0000;
const MIX_PARTS: u32 = 10_000;

#[derive(Parser, Debug)]
pub struct Args {
    /// Distinct ICAOs to cycle through.
    #[arg(long, default_value_t = 32)]
    aircraft: usize,
    /// Messages per second. 0 means as fast as possible.
    #[arg(long, default_value_t = 0)]
    rate: u32,
    /// Stop after this many seconds. 0 means no time limit.
    #[arg(long, default_value_t = 0)]
    seconds: u64,
    /// Stop after this many generated Mode S frames. 0 means no cap.
    #[arg(long, default_value_t = 0)]
    messages: u64,
    /// Fraction of ICAOs that advertise squawk 7700.
    #[arg(long, default_value_t = 0.1, value_parser = parse_frac)]
    emergency: f64,
    /// Fraction of messages that are BDS 0,8 identification.
    #[arg(long, default_value_t = 0.15, value_parser = parse_frac)]
    ident: f64,
    /// Fraction of messages that are BDS 0,9 velocity.
    #[arg(long, default_value_t = 0.15, value_parser = parse_frac)]
    velocity: f64,
    /// Fraction of messages that are CRC-bad or truncated junk.
    #[arg(long, default_value_t = 0.05, value_parser = parse_frac)]
    junk: f64,
    #[command(flatten)]
    output: OutputArgs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Position,
    Ident,
    Velocity,
    Status,
    BadCrc,
    Truncated,
}

#[derive(Debug)]
enum Frame {
    Long([u8; 14]),
    Short([u8; 7]),
}

#[derive(Default, Debug)]
struct Stats {
    messages: AtomicU64,
    fixes: AtomicU64,
    dropped: AtomicU64,
    csv: AtomicU64,
}

fn parse_frac(text: &str) -> Result<f64, String> {
    let value: f64 = text.parse::<f64>().map_err(|err| err.to_string())?;
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(value)
    } else {
        Err("expected a fraction 0..=1".into())
    }
}

fn frac_parts(value: f64) -> u32 {
    (value * f64::from(MIX_PARTS)).round() as u32
}

fn emergency_count(aircraft: usize, fraction: f64) -> usize {
    (aircraft as f64 * fraction).round() as usize
}

impl Args {
    fn check(&self) -> Result<()> {
        if self.aircraft == 0 {
            bail!("stress --aircraft must be at least 1");
        }
        let max = (Icao::MAX - ICAO_BASE) as usize;
        if self.aircraft > max {
            bail!("stress --aircraft must be at most {max}");
        }
        if frac_parts(self.ident) + frac_parts(self.velocity) + frac_parts(self.junk) > MIX_PARTS {
            bail!("stress --ident + --velocity + --junk must be <= 1");
        }
        Ok(())
    }
}

pub fn run(opts: Args) -> ExitCode {
    if let Err(err) = opts.check() {
        eprintln!("{err}");
        return ExitCode::from(1);
    }
    let stop = Stop::new();
    super::install_ctrlc(Arc::clone(&stop));
    let table = open_table(&opts.output.table);
    let csv_on = table.is_some();
    let writer = match open_output(
        &opts.output.uart,
        opts.output.baud,
        Arc::clone(&stop),
        table,
    ) {
        Ok(writer) => writer,
        Err(err) => {
            eprintln!("{}", super::error_chain(&err));
            return ExitCode::from(1);
        }
    };
    let rate = if opts.rate == 0 {
        "unlimited".to_string()
    } else {
        opts.rate.to_string()
    };
    eprintln!(
        "stress aircraft={} rate={rate} seconds={} messages={} emergency={:.2} ident={:.2} velocity={:.2} junk={:.2} uart={} table={}",
        opts.aircraft,
        opts.seconds,
        opts.messages,
        opts.emergency,
        opts.ident,
        opts.velocity,
        opts.junk,
        opts.output.uart,
        opts.output.table.display()
    );
    let shared = Arc::clone(&writer.shared);
    let stats = Arc::new(Stats::default());
    let done = Arc::new(AtomicBool::new(false));
    let started = Instant::now();
    let gen_stop = Arc::clone(&stop);
    let gen_done = Arc::clone(&done);
    let gen_stats = Arc::clone(&stats);
    let gen_shared = Arc::clone(&shared);
    let handle = thread::spawn(move || {
        generate(&opts, gen_shared, &gen_stop, &gen_stats, csv_on);
        gen_done.store(true, Ordering::Relaxed);
        shared.ready.notify_one();
    });
    let code = run_writer(writer, &stop, Some(&done));
    stop.request();
    let _ = handle.join();
    let elapsed = started.elapsed().as_secs_f64();
    eprintln!(
        "stress messages={} fixes={} dropped={} csv={} elapsed={elapsed:.3}s",
        stats.messages.load(Ordering::Relaxed),
        stats.fixes.load(Ordering::Relaxed),
        stats.dropped.load(Ordering::Relaxed),
        stats.csv.load(Ordering::Relaxed),
    );
    code
}

fn generate(opts: &Args, shared: Arc<super::Shared>, stop: &Stop, stats: &Stats, csv_on: bool) {
    let mut tracker = Tracker::new();
    let mut odd = vec![false; opts.aircraft];
    let mut seen = vec![false; opts.aircraft];
    let junk = frac_parts(opts.junk);
    let ident = frac_parts(opts.ident);
    let velocity = frac_parts(opts.velocity);
    let n_em = emergency_count(opts.aircraft, opts.emergency);
    let start = Instant::now();
    let deadline = (opts.seconds > 0).then(|| start + Duration::from_secs(opts.seconds));
    let mut n = 0u64;
    while !stop.is_set() {
        if opts.messages > 0 && n >= opts.messages {
            break;
        }
        if let Some(deadline) = deadline
            && Instant::now() >= deadline
        {
            break;
        }
        if opts.rate > 0 {
            let due = Duration::from_secs_f64(n as f64 / f64::from(opts.rate));
            let now = start.elapsed();
            if now < due {
                sleep_until_stop(stop, due - now);
                if stop.is_set() {
                    break;
                }
            }
        }
        let i = (n as usize) % opts.aircraft;
        let icao = ICAO_BASE + i as u32;
        let emergency = i < n_em;
        let kind = next_kind(n, !seen[i], emergency, junk, ident, velocity);
        seen[i] = true;
        let frame = build(kind, icao, odd[i]);
        if kind == Kind::Position {
            odd[i] = !odd[i];
        }
        stats.messages.fetch_add(1, Ordering::Relaxed);
        if let Some(fix) = tracker.ingest(now_unix(), frame.bytes()) {
            stats.fixes.fetch_add(1, Ordering::Relaxed);
            if csv_on {
                stats.csv.fetch_add(1, Ordering::Relaxed);
            }
            if shared.push(fix).is_some() {
                stats.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
        n += 1;
    }
}

fn next_kind(n: u64, first: bool, emergency: bool, junk: u32, ident: u32, velocity: u32) -> Kind {
    if first && emergency {
        return Kind::Status;
    }
    let slot = (n.wrapping_mul(7_919) % u64::from(MIX_PARTS)) as u32;
    if slot < junk {
        if n.is_multiple_of(2) {
            Kind::BadCrc
        } else {
            Kind::Truncated
        }
    } else if slot < junk + ident {
        if emergency { Kind::Status } else { Kind::Ident }
    } else if slot < junk + ident + velocity {
        Kind::Velocity
    } else {
        Kind::Position
    }
}

fn build(kind: Kind, icao: u32, odd: bool) -> Frame {
    match kind {
        Kind::Position => Frame::Long(with_icao(if odd { &ODD } else { &EVEN }, icao)),
        Kind::Ident => Frame::Long(with_icao(&IDENT, icao)),
        Kind::Velocity => Frame::Long(with_icao(&VELOCITY, icao)),
        Kind::Status => Frame::Long(with_icao(&STATUS, icao)),
        Kind::BadCrc => {
            let mut frame = with_icao(&EVEN, icao);
            frame[13] ^= 0x01;
            Frame::Long(frame)
        }
        Kind::Truncated => Frame::Short([0x8D, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00]),
    }
}

impl Frame {
    fn bytes(&self) -> &[u8] {
        match self {
            Frame::Long(bytes) => bytes,
            Frame::Short(bytes) => bytes,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use trck::EmergencyKind;

    #[test]
    fn even_then_odd_for_a_new_icao_solves() {
        let mut tracker = Tracker::new();
        let even = with_icao(&EVEN, ICAO_BASE);
        let odd = with_icao(&ODD, ICAO_BASE);
        assert!(tracker.ingest(1.0, &even).is_none());
        let fix = tracker.ingest(2.0, &odd).expect("position");
        assert_eq!(fix.aircraft.icao.to_u24(), ICAO_BASE);
        assert_eq!(fix.aircraft.emergency, None);
    }

    #[test]
    fn status_then_position_carries_7700() {
        let mut tracker = Tracker::new();
        let status = with_icao(&STATUS, ICAO_BASE + 1);
        let even = with_icao(&EVEN, ICAO_BASE + 1);
        let odd = with_icao(&ODD, ICAO_BASE + 1);
        assert!(tracker.ingest(1.0, &status).is_none());
        assert!(tracker.ingest(2.0, &even).is_none());
        let fix = tracker.ingest(3.0, &odd).expect("position");
        assert_eq!(fix.aircraft.emergency, Some(EmergencyKind::General));
    }

    #[test]
    fn junk_does_not_solve() {
        let mut tracker = Tracker::new();
        let mut bad = with_icao(&EVEN, ICAO_BASE);
        bad[13] ^= 0x01;
        assert!(tracker.ingest(1.0, &bad).is_none());
        assert!(tracker.ingest(1.0, &[0x8D, 0, 0, 0, 0, 0, 0]).is_none());
    }

    #[test]
    fn mix_picks_every_kind() {
        let mut seen = [false; 6];
        for n in 0..MIX_PARTS as u64 {
            match next_kind(n, false, false, 1_000, 1_500, 1_500) {
                Kind::Position => seen[0] = true,
                Kind::Ident => seen[1] = true,
                Kind::Velocity => seen[2] = true,
                Kind::Status => seen[3] = true,
                Kind::BadCrc => seen[4] = true,
                Kind::Truncated => seen[5] = true,
            }
        }
        assert_eq!(seen, [true, true, true, false, true, true]);
        assert_eq!(next_kind(0, true, true, 0, 0, 0), Kind::Status);
    }

    #[test]
    fn fractions_must_fit() {
        let mut opts = Args::parse_from(["stress"]);
        opts.aircraft = 0;
        assert!(opts.check().is_err());
        opts.aircraft = 4;
        opts.ident = 0.6;
        opts.velocity = 0.6;
        opts.junk = 0.0;
        assert!(opts.check().is_err());
        opts.ident = 0.2;
        opts.velocity = 0.2;
        opts.junk = 0.1;
        assert!(opts.check().is_ok());
    }
}

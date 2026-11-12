//! RTL-SDR at 1090 MHz, demodulated by rs1090.

use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};

use desperado::{IqAsyncSource, IqFormat, IqSource};
use futures_util::StreamExt;
use num_complex::Complex;
use rs1090::source::demod::demod2400::demodulate2400;
use rs1090::source::demod::magnitude_u16;

use trck::{Fix, Tracker};

use super::Stop;

const CENTER_HZ: u32 = 1_090_000_000;
pub const SAMPLE_HZ: u32 = 2_400_000;
/// Tenths of a dB. `496` is 49.6 dB, the jet1090 default.
const GAIN_TENTHS_DB: i32 = 496;
const OPEN_RETRY: Duration = Duration::from_secs(2);
const LOOKAHEAD: usize = 326;
const FILE_CHUNK: usize = 32_768;

#[derive(Clone, Copy, Debug, Default)]
pub struct IqStats {
    pub messages: u64,
    pub fixes: u64,
}

struct Demod {
    mag: Vec<u16>,
    offset: u64,
    tracker: Tracker,
    stats: IqStats,
}

impl Demod {
    fn new() -> Self {
        Self {
            mag: Vec::new(),
            offset: 0,
            tracker: Tracker::new(),
            stats: IqStats::default(),
        }
    }

    fn push(&mut self, iq: &[Complex<f32>], enqueue: &impl Fn(Fix), log_msg: bool) {
        if iq.is_empty() {
            return;
        }
        self.mag.extend_from_slice(&magnitude_u16(iq));
        self.decode(enqueue, log_msg);
    }

    fn finish(&mut self, enqueue: &impl Fn(Fix), log_msg: bool) {
        self.mag.resize(self.mag.len() + LOOKAHEAD, 0);
        self.decode(enqueue, log_msg);
        self.mag.clear();
    }

    fn decode(&mut self, enqueue: &impl Fn(Fix), log_msg: bool) {
        if self.mag.len() <= LOOKAHEAD {
            return;
        }
        for data in demodulate2400(&self.mag) {
            self.stats.messages += 1;
            let timestamp =
                (self.offset + data.sample_position as u64) as f64 / f64::from(SAMPLE_HZ);
            if log_msg {
                eprint!("msg ");
                for byte in data.msg {
                    eprint!("{byte:02X}");
                }
                eprintln!(" t={timestamp:.6}");
            }
            let Some(fix) = self.tracker.ingest(timestamp, &data.msg) else {
                continue;
            };
            self.stats.fixes += 1;
            enqueue(fix);
        }
        let scanned = self.mag.len() - LOOKAHEAD;
        self.mag.drain(..scanned);
        self.offset += scanned as u64;
    }
}

pub async fn run_rtlsdr(device: usize, enqueue: impl Fn(Fix), stop: &Stop) {
    let mut delay = OPEN_RETRY;
    while !stop.is_set() {
        match IqAsyncSource::from_rtlsdr(device, CENTER_HZ, SAMPLE_HZ, Some(GAIN_TENTHS_DB)).await {
            Ok(mut source) => {
                eprintln!("rtlsdr {device} open");
                delay = OPEN_RETRY;
                demodulate(&mut source, &enqueue, stop).await;
                if let Err(err) = source.stop().await {
                    eprintln!("rtlsdr {device} stop: {err}");
                }
            }
            Err(err) => {
                eprintln!("rtlsdr {device}: {err}");
            }
        }
        if stop.is_set() {
            break;
        }
        tokio::select! {
            biased;
            () = stop.wait() => break,
            () = tokio::time::sleep(delay) => {}
        }
        delay = delay.saturating_mul(2).min(Duration::from_secs(30));
    }
}

async fn demodulate(source: &mut IqAsyncSource, enqueue: &impl Fn(Fix), stop: &Stop) {
    let mut demod = Demod::new();
    while !stop.is_set() {
        let chunk = tokio::select! {
            biased;
            () = stop.wait() => return,
            chunk = source.next() => chunk,
        };
        let Some(chunk) = chunk else {
            demod.finish(enqueue, false);
            eprintln!("rtlsdr ended");
            return;
        };
        let buf = match chunk {
            Ok(buf) => buf,
            Err(err) => {
                eprintln!("rtlsdr read: {err}");
                return;
            }
        };
        demod.push(&buf, enqueue, false);
    }
}

pub fn run_iq(path: &Path, sample_hz: u32, enqueue: impl Fn(Fix), stop: &Stop) -> Result<IqStats> {
    if sample_hz == 0 {
        bail!("iq-rate must be > 0");
    }
    let mut source = IqSource::from_file(path, CENTER_HZ, sample_hz, FILE_CHUNK, IqFormat::Cu8)
        .with_context(|| path.display().to_string())?;
    let mut demod = Demod::new();
    let mut resamp = (sample_hz != SAMPLE_HZ).then(|| Resamp::new(sample_hz));
    for chunk in &mut source {
        if stop.is_set() {
            return Ok(demod.stats);
        }
        let buf = chunk.with_context(|| path.display().to_string())?;
        match resamp.as_mut() {
            Some(resamp) => demod.push(&resamp.push(&buf), &enqueue, true),
            None => demod.push(&buf, &enqueue, true),
        }
    }
    if !stop.is_set() {
        demod.finish(&enqueue, true);
    }
    Ok(demod.stats)
}

struct Resamp {
    prev: Option<Complex<f32>>,
    t: f64,
    seen: i64,
    step: f64,
}

impl Resamp {
    fn new(in_hz: u32) -> Self {
        Self {
            prev: None,
            t: 0.0,
            seen: 0,
            step: f64::from(in_hz) / f64::from(SAMPLE_HZ),
        }
    }

    fn at(&self, chunk: &[Complex<f32>], idx: i64) -> Option<Complex<f32>> {
        let local = idx - self.seen;
        if local == -1 {
            self.prev
        } else if local >= 0 {
            chunk.get(local as usize).copied()
        } else {
            None
        }
    }

    fn push(&mut self, chunk: &[Complex<f32>]) -> Vec<Complex<f32>> {
        let mut out = Vec::new();
        loop {
            let i0 = self.t.floor() as i64;
            let frac = (self.t - i0 as f64) as f32;
            let Some(a) = self.at(chunk, i0) else {
                break;
            };
            let Some(b) = self.at(chunk, i0 + 1) else {
                break;
            };
            out.push(Complex {
                re: a.re + (b.re - a.re) * frac,
                im: a.im + (b.im - a.im) * frac,
            });
            self.t += self.step;
        }
        if let Some(last) = chunk.last() {
            self.prev = Some(*last);
        }
        self.seen += chunk.len() as i64;
        out
    }
}

//! RTL-SDR at 1090 MHz, demodulated by rs1090.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use desperado::IqAsyncSource;
use futures_util::StreamExt;
use rs1090::source::demod::demod2400::demodulate2400;
use rs1090::source::demod::magnitude_u16;

use trck::{Fix, Tracker};

const CENTER_HZ: u32 = 1_090_000_000;
const SAMPLE_HZ: u32 = 2_400_000;
const SAMPLE_RATE: f64 = 2.4e6;
/// Tenths of a dB. `496` is 49.6 dB, the jet1090 default.
const GAIN_TENTHS_DB: i32 = 496;
const OPEN_RETRY: Duration = Duration::from_secs(2);

pub async fn run_rtlsdr(device: usize, enqueue: impl Fn(Fix), stop: &AtomicBool) {
    let mut delay = OPEN_RETRY;
    while !stop.load(Ordering::Relaxed) {
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
        if stop.load(Ordering::Relaxed) {
            break;
        }
        tokio::time::sleep(delay).await;
        delay = delay.saturating_mul(2).min(Duration::from_secs(30));
    }
}

async fn demodulate(source: &mut IqAsyncSource, enqueue: &impl Fn(Fix), stop: &AtomicBool) {
    let mut tracker = Tracker::new();
    while !stop.load(Ordering::Relaxed) {
        let chunk = tokio::select! {
            biased;
            () = wait_stop(stop) => return,
            chunk = source.next() => chunk,
        };
        let Some(chunk) = chunk else {
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
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        for data in demodulate2400(&magnitude_u16(&buf)) {
            let timestamp = now - data.sample_position as f64 / SAMPLE_RATE;
            let Some(fix) = tracker.ingest(timestamp, &data.msg) else {
                continue;
            };
            enqueue(fix);
        }
    }
}

async fn wait_stop(stop: &AtomicBool) {
    while !stop.load(Ordering::Relaxed) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

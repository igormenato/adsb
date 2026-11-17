//! RTL-SDR at 1090 MHz. readsb 2.4 MS/s demod with 1-bit PI CRC correction.

use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use rs_rtl::{DeviceId, RtlSdr};

use trck::demod::{demodulate2400, msg_octets};
use trck::mag::{cu8_to_f32, mag_cu8_into, mag_iq};
use trck::{Fix, Tracker};

use super::{POLL_SLICE, Stop, sleep_until_stop};

const CENTER_HZ: u32 = 1_090_000_000;
pub const SAMPLE_HZ: u32 = 2_400_000;
const OPEN_RETRY: Duration = Duration::from_secs(2);
const LOOKAHEAD: usize = 326;
const FILE_CHUNK: usize = 32_768;

#[derive(Clone, Copy, Debug, Default)]
pub struct IqStats {
    pub messages: u64,
    pub fixes: u64,
    pub fixed: u64,
}

struct Demod {
    mag: Vec<u16>,
    offset: u64,
    tracker: Tracker,
    stats: IqStats,
    fix: bool,
}

impl Demod {
    fn new(fix: bool) -> Self {
        Self {
            mag: Vec::new(),
            offset: 0,
            tracker: Tracker::new(),
            stats: IqStats::default(),
            fix,
        }
    }

    fn push_cu8(&mut self, cu8: &[u8], enqueue: &impl Fn(Fix), log_msg: bool) {
        let n = cu8.len() / 2;
        if n == 0 {
            return;
        }
        let start = self.mag.len();
        self.mag.resize(start + n, 0);
        mag_cu8_into(&cu8[..n * 2], &mut self.mag[start..]);
        self.decode(enqueue, log_msg);
    }

    fn push_mag(&mut self, mag: &[u16], enqueue: &impl Fn(Fix), log_msg: bool) {
        if mag.is_empty() {
            return;
        }
        self.mag.extend_from_slice(mag);
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
        for data in demodulate2400(&self.mag, self.fix) {
            self.stats.messages += 1;
            if data.corrected_bits != 0 {
                self.stats.fixed += 1;
            }
            let timestamp =
                (self.offset + data.sample_position as u64) as f64 / f64::from(SAMPLE_HZ);
            if log_msg {
                eprint!("msg ");
                for byte in msg_octets(&data.msg) {
                    eprint!("{byte:02X}");
                }
                eprintln!(" t={timestamp:.6}");
            }
            let Some(fix) = self.tracker.ingest(timestamp, msg_octets(&data.msg)) else {
                continue;
            };
            self.stats.fixes += 1;
            enqueue(fix);
        }
        let scanned = self.mag.len() - LOOKAHEAD;
        self.mag.copy_within(scanned.., 0);
        self.mag.truncate(self.mag.len() - scanned);
        self.offset += scanned as u64;
    }
}

pub fn run_rtlsdr(device: usize, gain: Option<i32>, enqueue: impl Fn(Fix), stop: &Stop, fix: bool) {
    let mut delay = OPEN_RETRY;
    while !stop.is_set() {
        match open_rtlsdr(device, gain) {
            Ok(mut sdr) => {
                eprintln!("rtlsdr {device} open");
                delay = OPEN_RETRY;
                match sdr.start_streaming() {
                    Ok(reader) => drain_rtlsdr(&reader, &enqueue, stop, fix),
                    Err(err) => eprintln!("rtlsdr {device} stream: {err}"),
                }
            }
            Err(err) => eprintln!("rtlsdr {device}: {err}"),
        }
        if stop.is_set() {
            break;
        }
        sleep_until_stop(stop, delay);
        delay = delay.saturating_mul(2).min(Duration::from_secs(30));
    }
}

fn open_rtlsdr(device: usize, gain: Option<i32>) -> Result<RtlSdr, rs_rtl::Error> {
    let mut sdr = RtlSdr::open(DeviceId::Index(device))?;
    sdr.set_center_freq(CENTER_HZ)?;
    sdr.set_sample_rate(SAMPLE_HZ)?;
    match gain {
        Some(g) => sdr.set_gain_manual(g)?,
        None => sdr.set_gain_auto()?,
    }
    Ok(sdr)
}

fn drain_rtlsdr(reader: &rs_rtl::AsyncReadHandle, enqueue: &impl Fn(Fix), stop: &Stop, fix: bool) {
    let mut demod = Demod::new(fix);
    let mut last_drop = 0u64;
    while !stop.is_set() {
        match reader.recv_timeout(POLL_SLICE) {
            Ok(rs_rtl::TryRecv::Item(buf)) => demod.push_cu8(&buf, enqueue, false),
            Ok(rs_rtl::TryRecv::Empty) => {}
            Ok(rs_rtl::TryRecv::End) => {
                demod.finish(enqueue, false);
                eprintln!("rtlsdr ended");
                break;
            }
            Err(err) => {
                eprintln!("rtlsdr read: {err}");
                break;
            }
        }
        let dropped = reader.dropped_chunks();
        if dropped != last_drop {
            // TACK is 21 bytes with no spare field. USB drops stay on stderr.
            eprintln!("rtlsdr usb_dropped={dropped}");
            last_drop = dropped;
        }
    }
    reader.stop();
}

pub fn run_iq(
    path: &Path,
    sample_hz: u32,
    enqueue: impl Fn(Fix),
    stop: &Stop,
    fix: bool,
) -> Result<IqStats> {
    if sample_hz == 0 {
        bail!("iq-rate must be > 0");
    }
    let mut file = File::open(path).with_context(|| path.display().to_string())?;
    let mut buf = vec![0u8; FILE_CHUNK * 2];
    let mut demod = Demod::new(fix);
    let mut resamp = (sample_hz != SAMPLE_HZ).then(|| Resamp::new(sample_hz));
    loop {
        if stop.is_set() {
            return Ok(demod.stats);
        }
        let n = file
            .read(&mut buf)
            .with_context(|| path.display().to_string())?;
        if n == 0 {
            break;
        }
        let n = n & !1;
        if n == 0 {
            continue;
        }
        match resamp.as_mut() {
            Some(resamp) => demod.push_mag(resamp.push(&buf[..n]), &enqueue, true),
            None => demod.push_cu8(&buf[..n], &enqueue, true),
        }
    }
    if !stop.is_set() {
        demod.finish(&enqueue, true);
    }
    Ok(demod.stats)
}

struct Resamp {
    prev: Option<(f32, f32)>,
    t: f64,
    seen: i64,
    step: f64,
    mag: Vec<u16>,
}

impl Resamp {
    fn new(in_hz: u32) -> Self {
        Self {
            prev: None,
            t: 0.0,
            seen: 0,
            step: f64::from(in_hz) / f64::from(SAMPLE_HZ),
            mag: Vec::new(),
        }
    }

    fn at(&self, chunk: &[u8], idx: i64) -> Option<(f32, f32)> {
        let local = idx - self.seen;
        if local == -1 {
            self.prev
        } else if local >= 0 {
            let i = local as usize;
            let pair = chunk.get(2 * i..2 * i + 2)?;
            Some((cu8_to_f32(pair[0]), cu8_to_f32(pair[1])))
        } else {
            None
        }
    }

    fn push(&mut self, chunk: &[u8]) -> &[u16] {
        self.mag.clear();
        let samples = (chunk.len() / 2) as i64;
        loop {
            let i0 = self.t.floor() as i64;
            let frac = (self.t - i0 as f64) as f32;
            let Some(a) = self.at(chunk, i0) else {
                break;
            };
            let Some(b) = self.at(chunk, i0 + 1) else {
                break;
            };
            let i = a.0 + (b.0 - a.0) * frac;
            let q = a.1 + (b.1 - a.1) * frac;
            self.mag.push(mag_iq(i, q));
            self.t += self.step;
        }
        if samples > 0 {
            let last = (samples as usize - 1) * 2;
            self.prev = Some((cu8_to_f32(chunk[last]), cu8_to_f32(chunk[last + 1])));
        }
        self.seen += samples;
        &self.mag
    }
}

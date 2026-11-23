//! Software autogain over the tuner manual gain table.
//!
//! Ported from wiedehopf/readsb `gainStatistics` / `--gain auto` (GPL).
//! Stats are windowed magnitude sums; gain steps are applied between IQ
//! buffers, not in the demod inner loop.

/// dump1090-2400 / trck live sample rate.
const SAMPLE_HZ: u32 = 2_400_000;
/// Half a short Mode S message at 2 MS/s (`MODES_SHORT_MSG_SAMPLES / 2` in readsb).
const WINDOW: usize = 56;
/// 0.5 s at dump1090-2400.
const INTERVAL_SAMPLES: u64 = SAMPLE_HZ as u64 / 2;
const RISE_INTERVALS: u32 = 30;
const REBOUND_INTERVALS: u32 = 3;
/// readsb `--gain auto` start, tenths of a dB.
const START_TENTH_DB: i32 = 300;
/// readsb default `minGain`; skips FC0013's negative LNA steps.
const MIN_TENTH_DB: i32 = 0;
const STARTUP: u32 = 8;
const LOUD_LEVEL: u32 = 243;
const NOISE_LOW_LEVEL: u32 = 34;
const NOISE_HIGH_LEVEL: u32 = 36;
const LOUD_TH: u32 = LOUD_LEVEL * LOUD_LEVEL * WINDOW as u32;
const NOISE_LOW_TH: u32 = NOISE_LOW_LEVEL * NOISE_LOW_LEVEL * WINDOW as u32;
const NOISE_HIGH_TH: u32 = NOISE_HIGH_LEVEL * NOISE_HIGH_LEVEL * WINDOW as u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GainChange {
    pub old_tenth_db: i32,
    pub new_tenth_db: i32,
    pub reason: GainReason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GainReason {
    VeryLoud,
    Loud,
    NoiseHigh,
    NoiseLow,
}

impl GainReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::VeryLoud => "decreasing, many strong signals",
            Self::Loud => "decreasing, strong signal",
            Self::NoiseHigh => "decreasing, noise too high",
            Self::NoiseLow => "increasing, noise too low",
        }
    }
}

#[derive(Debug)]
pub struct AutoGain {
    table: Vec<i32>,
    idx: usize,
    min_idx: usize,
    startup: u32,
    slow_rise: u32,
    loud_rebound: f32,
    loud_events: u64,
    noise_low: u64,
    noise_high: u64,
    total: u64,
    leftover: [u16; WINDOW],
    leftover_len: usize,
}

impl AutoGain {
    pub fn new(table: &[i32]) -> Option<Self> {
        if table.is_empty() {
            return None;
        }
        let min_idx = table.iter().position(|&g| g >= MIN_TENTH_DB).unwrap_or(0);
        let mut idx = nearest_index(table, START_TENTH_DB);
        idx = idx.clamp(min_idx, table.len() - 1);
        Some(Self {
            table: table.to_vec(),
            idx,
            min_idx,
            startup: STARTUP,
            slow_rise: 0,
            loud_rebound: 0.0,
            loud_events: 0,
            noise_low: 0,
            noise_high: 0,
            total: 0,
            leftover: [0; WINDOW],
            leftover_len: 0,
        })
    }

    pub fn current(&self) -> i32 {
        self.table[self.idx]
    }

    pub fn observe(&mut self, mag: &[u16]) {
        self.total += mag.len() as u64;
        let mut i = 0;
        if self.leftover_len > 0 {
            let need = WINDOW - self.leftover_len;
            if mag.len() < need {
                self.leftover[self.leftover_len..self.leftover_len + mag.len()]
                    .copy_from_slice(mag);
                self.leftover_len += mag.len();
                return;
            }
            let mut sum = 0u32;
            for &x in &self.leftover[..self.leftover_len] {
                sum += u32::from(x);
            }
            for &x in &mag[..need] {
                sum += u32::from(x);
            }
            self.add_window(sum);
            self.leftover_len = 0;
            i = need;
        }
        while i + WINDOW <= mag.len() {
            let mut sum = 0u32;
            for &x in &mag[i..i + WINDOW] {
                sum += u32::from(x);
            }
            self.add_window(sum);
            i += WINDOW;
        }
        let rest = mag.len() - i;
        if rest > 0 {
            self.leftover[..rest].copy_from_slice(&mag[i..]);
            self.leftover_len = rest;
        }
    }

    pub fn evaluate(&mut self) -> Option<GainChange> {
        if self.total < INTERVAL_SAMPLES {
            return None;
        }
        let change = self.decide();
        self.startup /= 2;
        self.loud_events = 0;
        self.noise_low = 0;
        self.noise_high = 0;
        self.total = 0;
        change
    }

    fn add_window(&mut self, sum: u32) {
        if sum > LOUD_TH {
            self.loud_events += 1;
        }
        if sum < NOISE_LOW_TH {
            self.noise_low += WINDOW as u64;
        }
        if sum < NOISE_HIGH_TH {
            self.noise_high += WINDOW as u64;
        }
    }

    fn decide(&mut self) -> Option<GainChange> {
        let noise_low = self.noise_low.saturating_mul(100) > self.total.saturating_mul(5);
        let noise_high = self.noise_high.saturating_mul(100) < self.total;
        let loud = self.loud_events > 0;
        let very_loud = self.loud_events > 5;
        let mut lower = 0i32;
        let mut increase = 0i32;
        let mut reason = GainReason::NoiseLow;
        if loud || noise_high {
            lower = 1;
            if very_loud && self.startup == 0 {
                lower = 2;
            }
            if loud {
                self.loud_rebound += lower as f32;
            }
            reason = if very_loud {
                GainReason::VeryLoud
            } else if loud {
                GainReason::Loud
            } else {
                GainReason::NoiseHigh
            };
        } else if noise_low {
            let ready = self.startup > 0
                || self.slow_rise >= RISE_INTERVALS
                || (self.loud_rebound > 1.0 && self.slow_rise >= REBOUND_INTERVALS);
            if ready {
                self.slow_rise = 0;
                increase = 1;
                if self.loud_rebound > 0.0 {
                    self.loud_rebound *= 0.95;
                    self.loud_rebound -= increase as f32;
                }
                reason = GainReason::NoiseLow;
            } else {
                self.slow_rise += 1;
            }
        }
        if self.startup > 0 {
            lower *= self.startup as i32;
            increase *= self.startup as i32;
        }
        if lower == 0 && increase == 0 {
            return None;
        }
        let old = self.current();
        let delta = increase - lower;
        let max_idx = self.table.len() - 1;
        let new_idx = (self.idx as i32 + delta).clamp(self.min_idx as i32, max_idx as i32) as usize;
        self.idx = new_idx;
        let new = self.current();
        if new == old {
            return None;
        }
        Some(GainChange {
            old_tenth_db: old,
            new_tenth_db: new,
            reason,
        })
    }
}

fn nearest_index(gains: &[i32], want: i32) -> usize {
    gains
        .iter()
        .enumerate()
        .min_by_key(|(_, g)| (i64::from(**g) - i64::from(want)).unsigned_abs())
        .map(|(i, _)| i)
        .unwrap_or(0)
}

pub fn log_change(change: GainChange) {
    eprintln!(
        "gain {:.1} -> {:.1} dB ({})",
        f64::from(change.old_tenth_db) / 10.0,
        f64::from(change.new_tenth_db) / 10.0,
        change.reason.as_str()
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mag::mag_cu8;

    const TABLE: &[i32] = &[0, 10, 20, 30, 40, 90, 140, 200, 300, 400, 496];
    const FC: &[i32] = &[-99, -73, -54, 58, 71, 179, 197];

    fn interval_mags(window_mag: u16) -> Vec<u16> {
        vec![window_mag; INTERVAL_SAMPLES as usize]
    }

    fn mix_mags(quiet_frac: f64, quiet: u16, rest: u16) -> Vec<u16> {
        let n = INTERVAL_SAMPLES as usize;
        let n_quiet = ((n / WINDOW) as f64 * quiet_frac) as usize * WINDOW;
        let mut mag = vec![rest; n];
        mag[..n_quiet].fill(quiet);
        mag
    }

    fn run_until_hold(auto: &mut AutoGain, mag: &[u16], cap: usize) -> usize {
        let mut steps = 0;
        for _ in 0..cap {
            auto.observe(mag);
            if auto.evaluate().is_some() {
                steps += 1;
            } else {
                break;
            }
        }
        steps
    }

    #[test]
    fn starts_near_30_db_and_skips_negative_fc0013() {
        let auto = AutoGain::new(TABLE).unwrap();
        assert_eq!(auto.current(), 300);
        let fc = AutoGain::new(FC).unwrap();
        assert_eq!(fc.current(), 197);
        assert!(fc.table[fc.min_idx] >= 0);
        assert!(AutoGain::new(&[]).is_none());
    }

    #[test]
    fn stays_put_on_normal_input() {
        let mut auto = AutoGain::new(TABLE).unwrap();
        auto.startup = 0;
        let mag = mix_mags(0.03, 500, 8_000);
        auto.observe(&mag);
        assert_eq!(auto.evaluate(), None);
        assert_eq!(auto.current(), 300);
        auto.observe(&mag);
        assert_eq!(auto.evaluate(), None);
    }

    #[test]
    fn steps_down_on_clipping() {
        let mut auto = AutoGain::new(TABLE).unwrap();
        let mag = interval_mags(u16::MAX);
        auto.observe(&mag);
        let change = auto.evaluate().expect("lower");
        assert!(change.new_tenth_db < change.old_tenth_db);
        assert_eq!(change.reason, GainReason::VeryLoud);
    }

    #[test]
    fn steps_down_on_loud_not_clipping() {
        let mut auto = AutoGain::new(TABLE).unwrap();
        auto.startup = 0;
        let mut mag = mix_mags(0.03, 500, 8_000);
        mag[..WINDOW].fill(60_000);
        auto.observe(&mag);
        let change = auto.evaluate().expect("lower");
        assert_eq!(change.reason, GainReason::Loud);
        assert!(change.new_tenth_db < change.old_tenth_db);
    }

    #[test]
    fn steps_up_on_low_noise_floor() {
        let mut auto = AutoGain::new(TABLE).unwrap();
        auto.idx = nearest_index(TABLE, 90);
        let mag = interval_mags(200);
        auto.observe(&mag);
        let change = auto.evaluate().expect("raise");
        assert!(change.new_tenth_db > change.old_tenth_db);
        assert_eq!(change.reason, GainReason::NoiseLow);
    }

    #[test]
    fn respects_table_limits() {
        let mut auto = AutoGain::new(TABLE).unwrap();
        auto.idx = 0;
        auto.startup = 0;
        auto.min_idx = 0;
        auto.observe(&interval_mags(u16::MAX));
        assert_eq!(auto.evaluate(), None);
        assert_eq!(auto.current(), 0);

        auto.idx = TABLE.len() - 1;
        auto.startup = STARTUP;
        auto.observe(&interval_mags(0));
        assert_eq!(auto.evaluate(), None);
        assert_eq!(auto.current(), 496);
    }

    #[test]
    fn fc0013_will_not_step_into_negative_lna() {
        let mut auto = AutoGain::new(FC).unwrap();
        auto.idx = auto.min_idx;
        auto.startup = 0;
        auto.observe(&interval_mags(u16::MAX));
        assert_eq!(auto.evaluate(), None);
        assert_eq!(auto.current(), 58);
    }

    #[test]
    fn hysteresis_blocks_immediate_raise_after_loud() {
        let mut auto = AutoGain::new(TABLE).unwrap();
        auto.startup = 0;
        auto.idx = nearest_index(TABLE, 200);
        auto.observe(&interval_mags(60_000));
        assert!(auto.evaluate().is_some());
        let quiet = interval_mags(200);
        for _ in 0..REBOUND_INTERVALS {
            auto.observe(&quiet);
            assert_eq!(auto.evaluate(), None, "raise during rebound");
        }
        auto.observe(&quiet);
        let change = auto.evaluate().expect("raise after rebound");
        assert_eq!(change.reason, GainReason::NoiseLow);
    }

    #[test]
    fn hysteresis_slow_rise_without_loud() {
        let mut auto = AutoGain::new(TABLE).unwrap();
        auto.startup = 0;
        auto.idx = nearest_index(TABLE, 90);
        let quiet = interval_mags(200);
        for _ in 0..RISE_INTERVALS {
            auto.observe(&quiet);
            assert_eq!(auto.evaluate(), None);
        }
        auto.observe(&quiet);
        assert_eq!(auto.evaluate().expect("raise").reason, GainReason::NoiseLow);
    }

    #[test]
    fn no_oscillation_on_constant_hold_or_limits() {
        let mut auto = AutoGain::new(TABLE).unwrap();
        auto.startup = 0;
        let hold = mix_mags(0.03, 500, 8_000);
        for _ in 0..40 {
            auto.observe(&hold);
            assert_eq!(auto.evaluate(), None);
        }
        assert_eq!(auto.current(), 300);

        let mut auto = AutoGain::new(TABLE).unwrap();
        run_until_hold(&mut auto, &interval_mags(u16::MAX), 20);
        assert_eq!(auto.current(), 0);
        auto.startup = 0;
        for _ in 0..10 {
            auto.observe(&interval_mags(u16::MAX));
            assert_eq!(auto.evaluate(), None);
        }

        let mut auto = AutoGain::new(TABLE).unwrap();
        auto.idx = 0;
        run_until_hold(&mut auto, &interval_mags(0), 20);
        assert_eq!(auto.current(), 496);
        auto.startup = 0;
        auto.slow_rise = 0;
        for _ in 0..10 {
            auto.observe(&interval_mags(0));
            assert_eq!(auto.evaluate(), None);
        }
    }

    fn to_cu8(x: f32) -> u8 {
        (x.mul_add(128.0, 127.5)).clamp(0.0, 255.0) as u8
    }

    fn apply_gain(antenna: &[f32], tenth_db: i32) -> Vec<u8> {
        let g = 10f32.powf(tenth_db as f32 / 200.0);
        let mut cu8 = Vec::with_capacity(antenna.len());
        for &a in antenna {
            cu8.push(to_cu8(a * g));
        }
        cu8
    }

    fn gaussian(seed: &mut u64) -> f32 {
        *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let u = ((*seed >> 33) as u32 as f32 / u32::MAX as f32).clamp(1e-6, 1.0);
        *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let v = (*seed >> 33) as u32 as f32 / u32::MAX as f32;
        (-2.0 * u.ln()).sqrt() * (2.0 * std::f32::consts::PI * v).cos()
    }

    fn antenna_noise(sigma: f32, n_iq: usize, seed: u64) -> Vec<f32> {
        let mut seed = seed | 1;
        let mut out = Vec::with_capacity(n_iq * 2);
        for _ in 0..n_iq * 2 {
            out.push(sigma * gaussian(&mut seed));
        }
        out
    }

    fn feed_interval(auto: &mut AutoGain, cu8: &[u8]) {
        let mag = mag_cu8(cu8);
        assert!(!mag.is_empty());
        while auto.total < INTERVAL_SAMPLES {
            auto.observe(&mag);
        }
    }

    fn converge(sigma: f32) -> (i32, Vec<i32>) {
        let antenna = antenna_noise(sigma, WINDOW * 32, 1);
        let mut auto = AutoGain::new(TABLE).unwrap();
        let mut path = vec![auto.current()];
        for _ in 0..24 {
            let cu8 = apply_gain(&antenna, auto.current());
            feed_interval(&mut auto, &cu8);
            match auto.evaluate() {
                Some(c) => path.push(c.new_tenth_db),
                None => path.push(auto.current()),
            }
        }
        (auto.current(), path)
    }

    #[test]
    fn simulated_cu8_converges_by_input_level() {
        let (low, low_path) = converge(0.0002);
        let (mid, mid_path) = converge(0.002);
        let (high, high_path) = converge(0.05);
        eprintln!("autogain sim low σ=0.0002 path={low_path:?} settle={low}");
        eprintln!("autogain sim mid σ=0.002 path={mid_path:?} settle={mid}");
        eprintln!("autogain sim high σ=0.05 path={high_path:?} settle={high}");
        assert!(
            low >= 300,
            "weak input should sit at high gain: {low_path:?}"
        );
        assert!(
            high <= 90,
            "hot input should sit at low gain: {high_path:?}"
        );
        assert!(
            mid <= low && high <= mid,
            "mid should land between weak and hot: low={low} mid={mid} high={high} {mid_path:?}"
        );
    }
}

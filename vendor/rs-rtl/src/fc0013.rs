//! Fitipower FC0013 tuner.
//!
//! Register programming is the public silicon map (I2C 0xC6, chip id 0xA3 at
//! 0x00, PLL, LNA gain). This is not a translation of librtlsdr's GPL driver.

use tracing::{debug, trace};

use crate::device::I2cBus;
use crate::error::{Error, Result};

/// 8-bit I2C address used on the RTL2832U bridge (7-bit 0x63).
pub const I2C_ADDR: u8 = 0xc6;
/// Chip-id register 0x00 value that identifies an FC0013.
pub const CHIP_ID: u8 = 0xa3;

const INIT: [(u8, u8); 21] = [
    (0x01, 0x09),
    (0x02, 0x16),
    (0x03, 0x00),
    (0x04, 0x00),
    (0x05, 0x17),
    (0x06, 0x02),
    (0x07, 0x0a),
    (0x08, 0xff),
    (0x09, 0x6e),
    (0x0a, 0xb8),
    (0x0b, 0x82),
    (0x0c, 0xfc),
    (0x0d, 0x01),
    (0x0e, 0x00),
    (0x0f, 0x00),
    (0x10, 0x00),
    (0x11, 0x00),
    (0x12, 0x00),
    (0x13, 0x00),
    (0x14, 0x50),
    (0x15, 0x01),
];

const LNA_GAINS: &[(i32, u8)] = &[
    (-99, 0x02),
    (-73, 0x03),
    (-65, 0x05),
    (-63, 0x04),
    (-63, 0x00),
    (-60, 0x07),
    (-58, 0x01),
    (-54, 0x06),
    (58, 0x0f),
    (61, 0x0e),
    (63, 0x0d),
    (65, 0x0c),
    (67, 0x0b),
    (68, 0x0a),
    (70, 0x09),
    (71, 0x08),
    (179, 0x17),
    (181, 0x16),
    (182, 0x15),
    (184, 0x14),
    (186, 0x13),
    (188, 0x12),
    (191, 0x11),
    (197, 0x10),
];

const VHF_TRACK: &[(u32, u8)] = &[
    (177_500_000, 0x1c),
    (184_500_000, 0x18),
    (191_500_000, 0x14),
    (198_500_000, 0x10),
    (205_500_000, 0x0c),
    (219_500_000, 0x08),
    (299_999_999, 0x04),
];

const MULT: &[(u32, u8, u8, u8)] = &[
    (37_084_000, 96, 0x82, 0x00),
    (55_625_000, 64, 0x02, 0x02),
    (74_167_000, 48, 0x42, 0x00),
    (111_250_000, 32, 0x82, 0x02),
    (148_334_000, 24, 0x22, 0x00),
    (222_500_000, 16, 0x42, 0x02),
    (296_667_000, 12, 0x12, 0x00),
    (445_000_000, 8, 0x22, 0x02),
    (593_334_000, 6, 0x0a, 0x00),
    (950_000_000, 4, 0x12, 0x02),
];

/// Fitipower FC0013 state.
#[derive(Clone, Debug)]
pub struct Fc0013 {
    xtal_hz: u32,
    bandwidth_hz: u32,
    freq_hz: u32,
}

impl Fc0013 {
    /// Create a driver using the RTL2832U crystal as the tuner clock.
    pub fn new(xtal_hz: u32) -> Self {
        Self {
            xtal_hz,
            bandwidth_hz: 0,
            freq_hz: 0,
        }
    }

    /// Zero-IF tuner: the RTL2832U IF is 0 Hz.
    pub fn if_freq(&self) -> u32 {
        0
    }

    /// Supported LNA gains in tenths of a dB.
    pub fn gains(&self) -> &'static [i32] {
        GAIN_VALUES
    }

    /// Write the power-on register map.
    pub fn init(&mut self, bus: &impl I2cBus) -> Result<()> {
        for &(reg, mut val) in &INIT {
            if reg == 0x07 && (self.xtal_hz == 28_800_000 || self.xtal_hz == 27_000_000) {
                val |= 0x20;
            }
            if reg == 0x0c {
                val |= 0x02;
            }
            wr(bus, reg, val)?;
        }
        debug!("FC0013 init xtal={}Hz", self.xtal_hz);
        Ok(())
    }

    /// Tune RF and apply the current LPF bandwidth setting.
    pub fn set_freq(&mut self, bus: &impl I2cBus, freq_hz: u32) -> Result<()> {
        self.freq_hz = freq_hz;
        self.program(bus)
    }

    /// Enable LNA AGC. IF VGA is fixed at 0x0a, matching the usual RTL-SDR path.
    pub fn set_gain_auto(&mut self, bus: &impl I2cBus) -> Result<()> {
        let d = rd(bus, 0x0d)?;
        wr(bus, 0x0d, d & !0x08)?;
        wr(bus, 0x13, 0x0a)?;
        Ok(())
    }

    /// Manual LNA gain: closest table step to `gain_tenth_db`.
    ///
    /// Mode bits match librtlsdr `fc0013_set_gain_mode(dev, 1)`: set 0x0d bit 3
    /// and fix IF VGA at 0x13 = 0x0a, then write the LNA code into 0x14.
    pub fn set_gain_manual(&mut self, bus: &impl I2cBus, gain_tenth_db: i32) -> Result<()> {
        let d = rd(bus, 0x0d)?;
        wr(bus, 0x0d, d | 0x08)?;
        wr(bus, 0x13, 0x0a)?;
        let code = nearest_lna(gain_tenth_db);
        let cur = rd(bus, 0x14)?;
        wr(bus, 0x14, (cur & 0xe0) | code)?;
        Ok(())
    }

    /// FC0013 has no readable combined gain; report 0.
    pub fn read_gain(&self, _bus: &impl I2cBus) -> Result<i32> {
        Ok(0)
    }

    /// Store LPF bandwidth and retune if a frequency is already set.
    pub fn set_bandwidth(&mut self, bus: &impl I2cBus, bw: u32, _rate: u32) -> Result<u32> {
        self.bandwidth_hz = bw;
        if self.freq_hz != 0 {
            self.program(bus)?;
        }
        Ok(0)
    }

    /// Clear manual LNA and drop to the lowest table step.
    ///
    /// librtlsdr's FC0013 standby is a no-op, so the last `set_gain_manual`
    /// (often 19.7 dB) stays in the tuner until USB reset. Re-enable LNA AGC
    /// and write LNA code 0x02 (-9.9 dB) so close/teardown does not leave the
    /// dongle saturating.
    pub fn standby(&self, bus: &impl I2cBus) -> Result<()> {
        let d = rd(bus, 0x0d)?;
        wr(bus, 0x0d, d & !0x08)?;
        wr(bus, 0x13, 0x0a)?;
        let cur = rd(bus, 0x14)?;
        wr(bus, 0x14, (cur & 0xe0) | 0x02)
    }

    fn program(&self, bus: &impl I2cBus) -> Result<()> {
        let freq = self.freq_hz;
        set_vhf_track(bus, freq)?;
        set_band_filters(bus, freq)?;

        let (multi, mut r5, mut r6) = multiplier(freq);
        let f_vco = u64::from(freq) * u64::from(multi);
        let vco_high = f_vco >= 3_060_000_000;
        if vco_high {
            r6 |= 0x08;
        }

        let xtal_div2 = self.xtal_hz / 2;
        let (am, pm) = fa_fp(f_vco, xtal_div2)?;
        if am > 15 || pm < 0x0b {
            return Err(Error::InvalidParam(format!(
                "FC0013 PLL out of range at {freq} Hz"
            )));
        }

        r6 |= 0x20;
        let xin = xin_frac(f_vco, xtal_div2);
        r6 = (r6 & 0x3f) | lpf_bits(self.bandwidth_hz);
        r5 |= 0x07;

        wr(bus, 0x01, am)?;
        wr(bus, 0x02, pm)?;
        wr(bus, 0x03, (xin >> 8) as u8)?;
        wr(bus, 0x04, xin as u8)?;
        wr(bus, 0x05, r5)?;
        wr(bus, 0x06, r6)?;

        let r11 = rd(bus, 0x11)?;
        wr(bus, 0x11, if multi == 64 { r11 | 0x04 } else { r11 & 0xfb })?;

        wr(bus, 0x0e, 0x80)?;
        wr(bus, 0x0e, 0x00)?;
        wr(bus, 0x0e, 0x00)?;
        let cal = rd(bus, 0x0e)? & 0x3f;
        if vco_high && cal > 0x3c {
            r6 &= !0x08;
            wr(bus, 0x06, r6)?;
            wr(bus, 0x0e, 0x80)?;
            wr(bus, 0x0e, 0x00)?;
        } else if !vco_high && cal < 0x02 {
            r6 |= 0x08;
            wr(bus, 0x06, r6)?;
            wr(bus, 0x0e, 0x80)?;
            wr(bus, 0x0e, 0x00)?;
        }

        trace!(
            "FC0013 tune {freq} Hz multi={multi} am={am} pm={pm} xin=0x{xin:04x} bw={}",
            self.bandwidth_hz
        );
        Ok(())
    }
}

/// Gain steps in tenths of a dB, same order as [`LNA_GAINS`].
pub static GAIN_VALUES: &[i32] = &[
    -99, -73, -65, -63, -63, -60, -58, -54, 58, 61, 63, 65, 67, 68, 70, 71, 179, 181, 182, 184,
    186, 188, 191, 197,
];

pub(crate) fn nearest_lna(want: i32) -> u8 {
    LNA_GAINS
        .iter()
        .min_by_key(|(db, _)| (i64::from(*db) - i64::from(want)).unsigned_abs())
        .map(|(_, code)| *code)
        .unwrap_or(0x10)
}

fn multiplier(freq_hz: u32) -> (u8, u8, u8) {
    for &(max, n, r5, r6) in MULT {
        if freq_hz < max {
            return (n, r5, r6);
        }
    }
    (2, 0x0a, 0x02)
}

fn fa_fp(f_vco: u64, xtal_div2: u32) -> Result<(u8, u8)> {
    let div = u64::from(xtal_div2);
    let mut xdiv = f_vco / div;
    if f_vco - xdiv * div >= div / 2 {
        xdiv += 1;
    }
    let mut pm = u8::try_from(xdiv / 8).map_err(|_| Error::InvalidParam("FC0013 PM".into()))?;
    let mut am = u8::try_from(xdiv - u64::from(pm) * 8)
        .map_err(|_| Error::InvalidParam("FC0013 AM".into()))?;
    if am < 2 {
        am += 8;
        pm = pm.saturating_sub(1);
    }
    if pm > 31 {
        am = am.saturating_add(8 * (pm - 31));
        pm = 31;
    }
    Ok((am, pm))
}

fn xin_frac(f_vco: u64, xtal_div2: u32) -> u16 {
    let rem_khz = (f_vco - (f_vco / u64::from(xtal_div2)) * u64::from(xtal_div2)) / 1000;
    let mut xin = (rem_khz << 15) / u64::from(xtal_div2 / 1000);
    if xin >= 16384 {
        xin += 32768;
    }
    xin as u16
}

fn lpf_bits(bw_hz: u32) -> u8 {
    if bw_hz == 0 {
        0
    } else if bw_hz <= 6_000_000 {
        0x80
    } else if bw_hz <= 7_000_000 {
        0x40
    } else {
        0
    }
}

fn set_vhf_track(bus: &impl I2cBus, freq_hz: u32) -> Result<()> {
    let cur = rd(bus, 0x1d)? & 0xe3;
    let bits = VHF_TRACK
        .iter()
        .find(|(max, _)| freq_hz <= *max)
        .map(|(_, b)| *b)
        .unwrap_or(0x1c);
    wr(bus, 0x1d, cur | bits)
}

fn set_band_filters(bus: &impl I2cBus, freq_hz: u32) -> Result<()> {
    let r07 = rd(bus, 0x07)?;
    let r14 = rd(bus, 0x14)?;
    if freq_hz < 300_000_000 {
        wr(bus, 0x07, r07 | 0x10)?;
        wr(bus, 0x14, r14 & 0x1f)?;
    } else {
        wr(bus, 0x07, r07 & 0xef)?;
        wr(bus, 0x14, (r14 & 0x1f) | 0x40)?;
    }
    Ok(())
}

fn wr(bus: &impl I2cBus, reg: u8, val: u8) -> Result<()> {
    bus.i2c_write(I2C_ADDR, &[reg, val])
}

fn rd(bus: &impl I2cBus, reg: u8) -> Result<u8> {
    bus.i2c_read_reg(I2C_ADDR, reg)
}

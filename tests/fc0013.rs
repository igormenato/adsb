use std::cell::RefCell;
use std::collections::HashMap;

use rs_rtl::Error;
use rs_rtl::device::I2cBus;
use rs_rtl::error::Result;
use rs_rtl::fc0013::{self, Fc0013};
use rs_rtl::tuner::{self, TunerType, probe};

const ADSB_HZ: u32 = 1_090_000_000;
const SAMPLE_HZ: u32 = 2_400_000;
const XTAL_HZ: u32 = 28_800_000;
const DEFAULT_GAIN: i32 = 496;

#[derive(Default)]
struct FakeI2c {
    chips: RefCell<HashMap<u8, Chip>>,
}

struct Chip {
    regs: [u8; 256],
    ptr: u8,
}

impl Chip {
    fn new() -> Self {
        Self {
            regs: [0; 256],
            ptr: 0,
        }
    }
}

impl FakeI2c {
    fn with_chip(addr: u8, chip_id: u8) -> Self {
        let bus = Self::default();
        bus.set(addr, 0x00, chip_id);
        bus
    }

    fn set(&self, addr: u8, reg: u8, val: u8) {
        let mut chips = self.chips.borrow_mut();
        let chip = chips.entry(addr).or_insert_with(Chip::new);
        chip.regs[reg as usize] = val;
    }

    fn get(&self, addr: u8, reg: u8) -> u8 {
        self.chips
            .borrow()
            .get(&addr)
            .map(|c| c.regs[reg as usize])
            .unwrap_or(0)
    }
}

impl I2cBus for FakeI2c {
    fn i2c_write(&self, addr: u8, data: &[u8]) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }
        let mut chips = self.chips.borrow_mut();
        let chip = chips.entry(addr).or_insert_with(Chip::new);
        chip.ptr = data[0];
        for (i, &b) in data.iter().skip(1).enumerate() {
            let idx = chip.ptr as usize + i;
            if idx < 256 {
                chip.regs[idx] = b;
            }
        }
        if data.len() > 1 {
            chip.ptr = data[0].wrapping_add((data.len() - 1) as u8);
        }
        if data == [0x0e, 0x00] {
            chip.regs[0x0e] = 0x10;
        }
        Ok(())
    }

    fn i2c_read(&self, addr: u8, len: u16) -> Result<Vec<u8>> {
        let mut chips = self.chips.borrow_mut();
        let chip = chips.entry(addr).or_insert_with(Chip::new);
        let mut out = Vec::with_capacity(len as usize);
        for i in 0..len as usize {
            let idx = chip.ptr as usize + i;
            out.push(if idx < 256 { chip.regs[idx] } else { 0 });
        }
        Ok(out)
    }
}

fn fc0013_bus() -> FakeI2c {
    let bus = FakeI2c::with_chip(fc0013::I2C_ADDR, fc0013::CHIP_ID);
    bus.set(fc0013::I2C_ADDR, 0x0e, 0x10);
    bus
}

#[test]
fn probe_picks_fc0013_on_chip_id_a3() {
    let bus = fc0013_bus();
    assert_eq!(probe(&bus).unwrap(), (TunerType::Fc0013, 0xc6));
}

#[test]
fn probe_still_picks_r820t_on_chip_id_69() {
    let bus = FakeI2c::with_chip(tuner::R820T_I2C_ADDR, 0x69);
    assert_eq!(probe(&bus).unwrap(), (TunerType::R820T, 0x34));
}

#[test]
fn missing_tuner_error_lists_fc0013() {
    let err = probe(&FakeI2c::default()).unwrap_err();
    assert!(matches!(err, Error::TunerNotFound));
    assert_eq!(
        err.to_string(),
        "no supported tuner found (checked R820T at 0x34, R828D at 0x74, FC0013 at 0xc6)"
    );
}

#[test]
fn tune_1090_mhz_programs_pll_and_uhf_path() {
    let bus = fc0013_bus();
    let mut tuner = Fc0013::new(XTAL_HZ);
    tuner.init(&bus).unwrap();
    tuner.set_bandwidth(&bus, SAMPLE_HZ, SAMPLE_HZ).unwrap();
    tuner.set_freq(&bus, ADSB_HZ).unwrap();

    assert_eq!(bus.get(fc0013::I2C_ADDR, 0x01), 0x07);
    assert_eq!(bus.get(fc0013::I2C_ADDR, 0x02), 0x12);
    assert_eq!(bus.get(fc0013::I2C_ADDR, 0x03), 0x31);
    assert_eq!(bus.get(fc0013::I2C_ADDR, 0x04), 0xc7);
    assert_eq!(bus.get(fc0013::I2C_ADDR, 0x05), 0x0f);
    assert_eq!(bus.get(fc0013::I2C_ADDR, 0x06), 0xa2);
    assert_eq!(bus.get(fc0013::I2C_ADDR, 0x07) & 0x10, 0);
    assert_eq!(bus.get(fc0013::I2C_ADDR, 0x14) & 0xe0, 0x40);
    assert_eq!(bus.get(fc0013::I2C_ADDR, 0x1d) & 0x1c, 0x1c);
    assert_eq!(tuner.if_freq(), 0);
}

#[test]
fn default_gain_maps_to_highest_fc0013_step() {
    let bus = fc0013_bus();
    let mut tuner = Fc0013::new(XTAL_HZ);
    tuner.init(&bus).unwrap();
    tuner.set_gain_manual(&bus, DEFAULT_GAIN).unwrap();
    assert_eq!(bus.get(fc0013::I2C_ADDR, 0x14) & 0x1f, 0x10);
}

#[test]
fn manual_gain_enables_mode_and_fixed_if() {
    let bus = fc0013_bus();
    let mut tuner = Fc0013::new(XTAL_HZ);
    tuner.init(&bus).unwrap();
    tuner.set_gain_manual(&bus, DEFAULT_GAIN).unwrap();
    assert_eq!(bus.get(fc0013::I2C_ADDR, 0x0d) & 0x08, 0x08);
    assert_eq!(bus.get(fc0013::I2C_ADDR, 0x13), 0x0a);
    assert_eq!(bus.get(fc0013::I2C_ADDR, 0x14) & 0x1f, 0x10);
    tuner.set_gain_auto(&bus).unwrap();
    assert_eq!(bus.get(fc0013::I2C_ADDR, 0x0d) & 0x08, 0);
}

#[test]
fn gain_auto_clears_manual_lna_bit() {
    let bus = fc0013_bus();
    let mut tuner = Fc0013::new(XTAL_HZ);
    tuner.init(&bus).unwrap();
    tuner.set_gain_auto(&bus).unwrap();
    assert_eq!(bus.get(fc0013::I2C_ADDR, 0x0d) & 0x08, 0);
    assert_eq!(bus.get(fc0013::I2C_ADDR, 0x13), 0x0a);
}

#[test]
fn fc0013_demod_is_zero_if() {
    let path = TunerType::Fc0013.demod_path();
    assert!(path.zero_if);
    assert!(!path.i_adc_only);
    assert!(!path.invert_spectrum);
    assert_eq!(path.if_hz, 0);
}

#[test]
fn r820t_demod_path_is_unchanged() {
    let path = TunerType::R820T.demod_path();
    assert!(!path.zero_if);
    assert!(path.i_adc_only);
    assert!(path.invert_spectrum);
    assert_eq!(path.if_hz, 3_570_000);
}

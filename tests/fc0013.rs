use rs_rtl::device::FakeI2c;
use rs_rtl::fc0013::{self, Fc0013};
use rs_rtl::tuner::{self, TunerType};
use rs_rtl::{Error, probe};

const ADSB_HZ: u32 = 1_090_000_000;
const SAMPLE_HZ: u32 = 2_400_000;
const XTAL_HZ: u32 = 28_800_000;
const DEFAULT_GAIN: i32 = 496;

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
    let err = probe(&FakeI2c::new()).unwrap_err();
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
    assert!(path.invert_spectrum);
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

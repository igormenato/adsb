//! ADSB-55 experiment: SNR sweep of three demod ideas vs current `--fix` baseline.
//!
//! ```text
//! cargo run --release --locked --example adsb55_bench
//! ```

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use trck::demod::{DemodOpts, ModeSMessage, demodulate2400_opts, msg_octets};
use trck::iq::{Synth, encode_cu8, encode_noise_cu8};
use trck::mag::{cu8_to_f32, mag_cu8, mag_iq};
use trck::modes::{EVEN, df5_identity, df11, with_icao};

const FRAMES: usize = 2000;
const SNR_DB: [f64; 13] = [
    4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0, 13.0, 14.0, 15.0, 16.0,
];
const SEED: u64 = 0xA05B_0055;
const SAMPLE_HZ: u32 = 2_400_000;
const NOISE_S: f64 = 60.0;
const NOISE_SNR_DB: f64 = 10.0;
const CONF_N: u8 = 10;

struct Variant {
    name: &'static str,
    opts: DemodOpts,
}

struct Point {
    sent: usize,
    hit: usize,
    false_decodes: usize,
    decoded: usize,
    cpu_s: f64,
    iq_s: f64,
}

fn main() {
    let variants = [
        Variant {
            name: "baseline",
            opts: DemodOpts::baseline(true),
        },
        Variant {
            name: "soft-conf",
            opts: DemodOpts::soft_conf(true),
        },
        Variant {
            name: "2-bit",
            opts: DemodOpts::two_bit(true),
        },
        Variant {
            name: "adaptive",
            opts: DemodOpts::adaptive(true),
        },
    ];

    let _ = demodulate2400_opts(
        &mag_cu8(&encode_cu8(&[EVEN], Synth::default()).expect("warm")),
        DemodOpts::two_bit(true),
    );

    println!("ADSB-55 demod bench");
    println!("frames={FRAMES} conf_n={CONF_N} seed={SEED:#x}");
    println!("SNR is 10 log10(A^2 / (2 sigma^2)); jitter 5 us; mix DF17/DF11/DF5");
    println!();

    let frames = mix_frames(FRAMES, SEED);
    let sent_set = sent_octets(&frames);

    println!("snr_db\tvariant\tdecode_p\tse\thits\tsent\tfalse\tdecoded\tcpu_per_iq");
    for snr in SNR_DB {
        let synth = Synth {
            snr_db: Some(snr),
            seed: SEED.wrapping_add((snr as u64) << 8),
            start_jitter_us: 5.0,
            leading_us: 3.0,
            ..Synth::default()
        };
        let cu8 = encode_cu8(&frames, synth).expect("iq");
        let mag = mag_cu8(&cu8);
        let iq_s = mag.len() as f64 / f64::from(SAMPLE_HZ);
        for v in &variants {
            let (cpu_s, msgs) = timed_demod(&mag, v.opts);
            let point = score(&sent_set, FRAMES, &msgs, cpu_s, iq_s);
            let p = point.hit as f64 / point.sent as f64;
            let se = (p * (1.0 - p) / point.sent as f64).sqrt();
            println!(
                "{snr:.0}\t{}\t{p:.4}\t{se:.4}\t{}\t{}\t{}\t{}\t{:.3}",
                v.name,
                point.hit,
                point.sent,
                point.false_decodes,
                point.decoded,
                point.cpu_s / point.iq_s
            );
        }
    }

    println!();
    println!("60 s AWGN at {NOISE_SNR_DB} dB sigma (no frames)");
    println!("variant\tfalse_total\tfalse_per_s\tcpu_per_iq");
    let mut noise_false = [0u64; 4];
    let mut noise_cpu = [0.0; 4];
    let mut noise_iq = 0.0;
    for sec in 0..NOISE_S as u32 {
        let cu8 = encode_noise_cu8(
            1.0,
            Synth {
                snr_db: Some(NOISE_SNR_DB),
                seed: SEED.wrapping_add(0x1000_0000).wrapping_add(u64::from(sec)),
                ..Synth::default()
            },
        )
        .expect("noise");
        let mag = mag_cu8(&cu8);
        noise_iq += mag.len() as f64 / f64::from(SAMPLE_HZ);
        for (i, v) in variants.iter().enumerate() {
            let (cpu_s, msgs) = timed_demod(&mag, v.opts);
            noise_cpu[i] += cpu_s;
            noise_false[i] += msgs.len() as u64;
        }
    }
    for (i, v) in variants.iter().enumerate() {
        println!(
            "{}\t{}\t{:.3}\t{:.3}",
            v.name,
            noise_false[i],
            noise_false[i] as f64 / NOISE_S,
            noise_cpu[i] / noise_iq
        );
    }

    real_files(&variants);
}

fn mix_frames(n: usize, seed: u64) -> Vec<[u8; 14]> {
    let mut rng = SplitMix(seed | 1);
    let mut used = HashSet::new();
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        let icao = (rng.next() & 0x00ff_ffff) as u32;
        if icao == 0 || !used.insert(icao) {
            continue;
        }
        match out.len() % 3 {
            0 => out.push(with_icao(&EVEN, icao)),
            1 => out.push(df11(icao)),
            _ => {
                let mut frame = [0u8; 14];
                let squawk = (rng.next() % 0o7777) as u16;
                frame[..7].copy_from_slice(&df5_identity(icao, squawk));
                out.push(frame);
            }
        }
    }
    out
}

fn sent_octets(frames: &[[u8; 14]]) -> HashSet<Vec<u8>> {
    frames.iter().map(|f| msg_octets(f).to_vec()).collect()
}

fn score(
    sent: &HashSet<Vec<u8>>,
    n_sent: usize,
    msgs: &[ModeSMessage],
    cpu_s: f64,
    iq_s: f64,
) -> Point {
    let mut seen = HashSet::new();
    let mut false_decodes = 0usize;
    for m in msgs {
        let oct = msg_octets(&m.msg).to_vec();
        if sent.contains(&oct) {
            seen.insert(oct);
        } else {
            false_decodes += 1;
        }
    }
    Point {
        sent: n_sent,
        hit: seen.len(),
        false_decodes,
        decoded: msgs.len(),
        cpu_s,
        iq_s,
    }
}

fn timed_demod(mag: &[u16], opts: DemodOpts) -> (f64, Vec<ModeSMessage>) {
    let t0 = Instant::now();
    let msgs = demodulate2400_opts(mag, opts);
    (t0.elapsed().as_secs_f64(), msgs)
}

fn real_files(variants: &[Variant]) {
    println!();
    if std::process::Command::new("scripts/fetch-iq.sh")
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        println!("fetch-iq.sh ok");
    } else {
        println!("fetch-iq.sh failed or missing; skip recordings");
        return;
    }
    let modes1 = PathBuf::from("tests/data/modes1.bin");
    if modes1.is_file() {
        println!("file\tvariant\tunique_crc\tmessages\tcpu_per_iq");
        report_file("modes1.bin", &modes1, 2_000_000, variants);
    }
    let awec = find_under(Path::new("tests/data/awec2026"), "recording.bin");
    if let Some(path) = awec {
        report_file("awec2026", &path, 2_000_000, variants);
    }
}

fn report_file(name: &str, path: &Path, in_hz: u32, variants: &[Variant]) {
    let cu8 = fs::read(path).expect("read iq");
    let mag = mag_resample_24(&cu8, in_hz);
    let iq_s = mag.len() as f64 / f64::from(SAMPLE_HZ);
    for v in variants {
        let (cpu_s, msgs) = timed_demod(&mag, v.opts);
        let unique: HashSet<Vec<u8>> = msgs.iter().map(|m| msg_octets(&m.msg).to_vec()).collect();
        println!(
            "{name}\t{}\t{}\t{}\t{:.3}",
            v.name,
            unique.len(),
            msgs.len(),
            cpu_s / iq_s
        );
    }
}

fn mag_resample_24(cu8: &[u8], in_hz: u32) -> Vec<u16> {
    if in_hz == SAMPLE_HZ {
        return mag_cu8(cu8);
    }
    let n_in = cu8.len() / 2;
    if n_in < 2 {
        return mag_cu8(cu8);
    }
    let step = f64::from(in_hz) / f64::from(SAMPLE_HZ);
    let mut out = Vec::new();
    let mut t: f64 = 0.0;
    loop {
        let i0 = t.floor() as usize;
        if i0 + 1 >= n_in {
            break;
        }
        let frac = (t - i0 as f64) as f32;
        let a0 = cu8_to_f32(cu8[2 * i0]);
        let a1 = cu8_to_f32(cu8[2 * i0 + 1]);
        let b0 = cu8_to_f32(cu8[2 * i0 + 2]);
        let b1 = cu8_to_f32(cu8[2 * i0 + 3]);
        out.push(mag_iq(a0 + (b0 - a0) * frac, a1 + (b1 - a1) * frac));
        t += step;
    }
    out
}

fn find_under(dir: &Path, name: &str) -> Option<PathBuf> {
    fn walk(dir: &Path, name: &str) -> Option<PathBuf> {
        for ent in fs::read_dir(dir).ok()? {
            let ent = ent.ok()?;
            let path = ent.path();
            if path.is_dir() {
                if let Some(hit) = walk(&path, name) {
                    return Some(hit);
                }
            } else if ent.file_name() == name {
                return Some(path);
            }
        }
        None
    }
    walk(dir, name)
}

struct SplitMix(u64);

impl SplitMix {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
}

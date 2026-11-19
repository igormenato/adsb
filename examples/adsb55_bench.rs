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
/// Set ADSB55_SNR=16 to run one point (used to capture the 16 dB per-DF diagnosis).
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
    sent_df: [usize; 3],
    hit_df: [usize; 3],
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
    println!("ICAO 1..0xBFFFFF (rs1090 is_plausible_icao); DF5 reuses prior DF17 ICAO");
    println!();

    // rs1090's ICAO filter is process-global: measure noise + recordings before
    // the synth sweep fills it with thousands of addresses.
    let (noise_false, noise_cpu, noise_iq) = measure_noise(&variants);
    let file_report = measure_files(&variants);

    let frames = mix_frames(FRAMES, SEED);
    let sent = index_sent(&frames);

    println!(
        "snr_db\tvariant\tdecode_p\tse\thits\tsent\tfalse\tdecoded\tcpu_per_iq\tdf17_p\tdf11_p\tdf5_p"
    );
    let snrs: Vec<f64> = match std::env::var("ADSB55_SNR") {
        Ok(s) => vec![s.parse().expect("ADSB55_SNR")],
        Err(_) => SNR_DB.to_vec(),
    };
    for snr in snrs {
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
            let point = score(&sent, FRAMES, &msgs, cpu_s, iq_s);
            let p = point.hit as f64 / point.sent as f64;
            let se = (p * (1.0 - p) / point.sent as f64).sqrt();
            let dfp = |i: usize| {
                if point.sent_df[i] == 0 {
                    0.0
                } else {
                    point.hit_df[i] as f64 / point.sent_df[i] as f64
                }
            };
            println!(
                "{snr:.0}\t{}\t{p:.4}\t{se:.4}\t{}\t{}\t{}\t{}\t{:.3}\t{:.4}\t{:.4}\t{:.4}",
                v.name,
                point.hit,
                point.sent,
                point.false_decodes,
                point.decoded,
                point.cpu_s / point.iq_s,
                dfp(0),
                dfp(1),
                dfp(2),
            );
        }
    }

    println!();
    println!("60 s AWGN at {NOISE_SNR_DB} dB sigma (no frames)");
    println!("variant\tfalse_total\tfalse_per_s\tcpu_per_iq");
    for (i, v) in variants.iter().enumerate() {
        println!(
            "{}\t{}\t{:.3}\t{:.3}",
            v.name,
            noise_false[i],
            noise_false[i] as f64 / NOISE_S,
            noise_cpu[i] / noise_iq
        );
    }

    print!("{file_report}");
}

fn measure_noise(variants: &[Variant]) -> ([u64; 4], [f64; 4], f64) {
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
    (noise_false, noise_cpu, noise_iq)
}

/// ICAOs in 1..0x00BFFFFF so `is_plausible_icao` (rs1090) does not reject them.
/// DF5 AP replies only score >0 after that ICAO is in the filter, so each DF5
/// reuses the preceding DF17's address (same capture, earlier in time).
fn mix_frames(n: usize, seed: u64) -> Vec<[u8; 14]> {
    let mut rng = SplitMix(seed | 1);
    let mut used = HashSet::new();
    let mut last_df17 = 0u32;
    let mut out = Vec::with_capacity(n);
    while out.len() < n {
        match out.len() % 3 {
            0 => {
                let icao = plausible_icao(&mut rng, &mut used);
                last_df17 = icao;
                out.push(with_icao(&EVEN, icao));
            }
            1 => out.push(df11(plausible_icao(&mut rng, &mut used))),
            _ => {
                let mut frame = [0u8; 14];
                let squawk = (rng.next() % 0o7777) as u16;
                frame[..7].copy_from_slice(&df5_identity(last_df17, squawk));
                out.push(frame);
            }
        }
    }
    out
}

fn plausible_icao(rng: &mut SplitMix, used: &mut HashSet<u32>) -> u32 {
    loop {
        let icao = (rng.next() % 0x00C0_0000) as u32;
        if icao != 0 && used.insert(icao) {
            return icao;
        }
    }
}

struct Sent {
    octets: HashSet<Vec<u8>>,
    by_df: [HashSet<Vec<u8>>; 3],
}

fn df_bucket(msg: &[u8; 14]) -> Option<usize> {
    match msg[0] >> 3 {
        17 | 18 => Some(0),
        11 => Some(1),
        0 | 4 | 5 => Some(2),
        _ => None,
    }
}

fn index_sent(frames: &[[u8; 14]]) -> Sent {
    let mut octets = HashSet::new();
    let mut by_df = [HashSet::new(), HashSet::new(), HashSet::new()];
    for f in frames {
        let oct = msg_octets(f).to_vec();
        octets.insert(oct.clone());
        if let Some(i) = df_bucket(f) {
            by_df[i].insert(oct);
        }
    }
    Sent { octets, by_df }
}

fn score(sent: &Sent, n_sent: usize, msgs: &[ModeSMessage], cpu_s: f64, iq_s: f64) -> Point {
    let mut seen = HashSet::new();
    let mut hit_df = [0usize; 3];
    let mut false_decodes = 0usize;
    for m in msgs {
        let oct = msg_octets(&m.msg).to_vec();
        if sent.octets.contains(&oct) {
            if seen.insert(oct.clone())
                && let Some(i) = df_bucket(&m.msg)
            {
                hit_df[i] += 1;
            }
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
        sent_df: [
            sent.by_df[0].len(),
            sent.by_df[1].len(),
            sent.by_df[2].len(),
        ],
        hit_df,
    }
}

fn timed_demod(mag: &[u16], opts: DemodOpts) -> (f64, Vec<ModeSMessage>) {
    let t0 = Instant::now();
    let msgs = demodulate2400_opts(mag, opts);
    (t0.elapsed().as_secs_f64(), msgs)
}

fn measure_files(variants: &[Variant]) -> String {
    let mut out = String::from("\n");
    let modes1 = PathBuf::from("tests/data/modes1.bin");
    if !modes1.is_file()
        && !std::process::Command::new("scripts/fetch-iq.sh")
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    {
        out.push_str("fetch-iq.sh failed or missing; skip recordings\n");
        return out;
    }
    out.push_str("fetch-iq.sh ok\n");
    if modes1.is_file() {
        out.push_str("file\tvariant\tunique_crc\tmessages\tcpu_per_iq\n");
        report_file(&mut out, "modes1.bin", &modes1, 2_000_000, variants);
    }
    if let Some(path) = find_under(Path::new("tests/data/awec2026"), "recording.bin") {
        report_file(&mut out, "awec2026", &path, 2_000_000, variants);
    }
    out
}

fn report_file(out: &mut String, name: &str, path: &Path, in_hz: u32, variants: &[Variant]) {
    let cu8 = fs::read(path).expect("read iq");
    let mag = mag_resample_24(&cu8, in_hz);
    let iq_s = mag.len() as f64 / f64::from(SAMPLE_HZ);
    for v in variants {
        let (cpu_s, msgs) = timed_demod(&mag, v.opts);
        let unique: HashSet<Vec<u8>> = msgs.iter().map(|m| msg_octets(&m.msg).to_vec()).collect();
        out.push_str(&format!(
            "{name}\t{}\t{}\t{}\t{:.3}\n",
            v.name,
            unique.len(),
            msgs.len(),
            cpu_s / iq_s
        ));
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

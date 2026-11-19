//! Live-path CPU and message-stream identity. Ignored benches need `--release`.

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

use sha2::{Digest, Sha256};
use trck::demod::{demodulate2400, msg_octets};
use trck::mag::mag_cu8;

fn hash_msgs(samples: &[u16], fix: bool) -> (usize, String) {
    let msgs = demodulate2400(samples, fix);
    let mut h = Sha256::new();
    for m in &msgs {
        h.update(m.sample_position.to_le_bytes());
        h.update(m.score.to_le_bytes());
        h.update([m.corrected_bits]);
        h.update(m.signal_level.to_bits().to_le_bytes());
        h.update(msg_octets(&m.msg));
    }
    (msgs.len(), hex::encode(h.finalize()))
}

fn cpu_s(samples: &[u16], fix: bool, repeats: u32) -> f64 {
    let t0 = Instant::now();
    for _ in 0..repeats {
        std::hint::black_box(demodulate2400(samples, fix));
    }
    t0.elapsed().as_secs_f64() / f64::from(repeats)
}

fn mag_cpu_s(cu8: &[u8], repeats: u32) -> f64 {
    let n = cu8.len() / 2;
    let mut out = vec![0u16; n];
    let t0 = Instant::now();
    for _ in 0..repeats {
        trck::mag::mag_cu8_into(cu8, &mut out);
        std::hint::black_box(&out);
    }
    t0.elapsed().as_secs_f64() / f64::from(repeats)
}

fn capture_s(iq_bytes: usize, hz: u32) -> f64 {
    (iq_bytes / 2) as f64 / f64::from(hz)
}

#[test]
fn short_buffer_hashes_empty() {
    let (n, hash) = hash_msgs(&[0; 10], true);
    assert_eq!(n, 0);
    assert_eq!(
        hash,
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

fn synth_quiet(seconds: f64, hz: u32) -> Vec<u8> {
    let n = (seconds * f64::from(hz)) as usize;
    let mut cu8 = vec![127u8; n * 2];
    for (i, b) in cu8.iter_mut().enumerate() {
        if i % 64 == 0 {
            *b = 140;
        }
    }
    cu8
}

#[test]
#[ignore = "release bench: cargo test --release --test live_path -- --ignored --nocapture"]
fn bench_and_hash_modes1_awec_synth() {
    let modes1 = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/modes1.bin");
    let awec = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/awec2026");
    let synth = synth_quiet(10.0, 2_400_000);
    eprintln!(
        "host={} synth10s_bytes={}",
        std::env::consts::ARCH,
        synth.len()
    );

    let t0 = Instant::now();
    let mag_s = mag_cpu_s(&synth, 3);
    let mag = mag_cu8(&synth);
    let dem_s = cpu_s(&mag, true, 2);
    let (n, hash) = hash_msgs(&mag, true);
    let cap = capture_s(synth.len(), 2_400_000);
    eprintln!(
        "synth10s mag={mag_s:.4}s demod={dem_s:.4}s total={:.4}s cpu_s_per_1s_iq={:.4} msgs={n} hash={hash} (capture={cap}s, warmup={:.3}s)",
        mag_s + dem_s,
        (mag_s + dem_s) / cap,
        t0.elapsed().as_secs_f64()
    );

    if modes1.is_file() {
        let cu8 = fs::read(&modes1).expect("modes1");
        let mag_t = mag_cpu_s(&cu8, 20);
        let mag = mag_cu8(&cu8);
        let dem_t = cpu_s(&mag, true, 20);
        let (n, hash) = hash_msgs(&mag, true);
        let cap = capture_s(cu8.len(), 2_000_000);
        eprintln!(
            "modes1.bin mag={mag_t:.4}s demod={dem_t:.4}s total={:.4}s cpu_s_per_1s_iq={:.4} msgs={n} hash={hash} (capture={cap:.4}s, file_rate=2e6)",
            mag_t + dem_t,
            (mag_t + dem_t) / cap
        );
    } else {
        eprintln!("skip modes1.bin");
    }

    let rec = find_recording(&awec);
    if let Some(path) = rec {
        let cu8 = fs::read(&path).expect("awec");
        let mag_t = mag_cpu_s(&cu8, 1);
        let mag = mag_cu8(&cu8);
        let dem_t = cpu_s(&mag, true, 1);
        let (n, hash) = hash_msgs(&mag, true);
        let cap = capture_s(cu8.len(), 2_000_000);
        eprintln!(
            "awec recording.bin mag={mag_t:.4}s demod={dem_t:.4}s total={:.4}s cpu_s_per_1s_iq={:.4} msgs={n} hash={hash} (capture={cap:.3}s, file_rate=2e6, path={})",
            mag_t + dem_t,
            (mag_t + dem_t) / cap,
            path.display()
        );
    } else {
        eprintln!("skip awec");
    }

    if modes1.is_file() {
        match hash_iq_cli(&modes1, 2_000_000) {
            Some((n, hash)) => eprintln!("modes1.bin cli msgs={n} hash={hash}"),
            None => eprintln!("modes1.bin cli skipped (could not exec trck)"),
        }
    }
    if let Some(path) = find_recording(&awec) {
        match hash_iq_cli(&path, 2_000_000) {
            Some((n, hash)) => eprintln!("awec cli msgs={n} hash={hash}"),
            None => eprintln!("awec cli skipped (could not exec trck)"),
        }
    }
}

fn hash_iq_cli(path: &Path, rate: u32) -> Option<(usize, String)> {
    let table = std::env::temp_dir().join(format!("trck-live-path-{}.csv", std::process::id()));
    let out = Command::new(env!("CARGO_BIN_EXE_trck"))
        .args([
            "--iq",
            path.to_str().expect("utf8"),
            "--iq-rate",
            &rate.to_string(),
            "--uart",
            "-",
            "--table",
            table.to_str().expect("utf8"),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .ok()?;
    let _ = fs::remove_file(&table);
    if !out.status.success() {
        eprintln!(
            "trck --iq {} status={:?} stderr={}",
            path.display(),
            out.status.code(),
            String::from_utf8_lossy(&out.stderr)
        );
        return None;
    }
    let mut h = Sha256::new();
    let mut n = 0usize;
    for line in String::from_utf8_lossy(&out.stderr).lines() {
        let Some(rest) = line.strip_prefix("msg ") else {
            continue;
        };
        n += 1;
        h.update(rest.as_bytes());
        h.update(b"\n");
    }
    Some((n, hex::encode(h.finalize())))
}

fn find_recording(dir: &Path) -> Option<std::path::PathBuf> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(cur) = stack.pop() {
        let Ok(entries) = fs::read_dir(&cur) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n == "__MACOSX") {
                    continue;
                }
                stack.push(path);
            } else if path.file_name().is_some_and(|n| n == "recording.bin") {
                return Some(path);
            }
        }
    }
    None
}

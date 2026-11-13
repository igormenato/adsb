mod common;
use common::*;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use trck::{FRAME_LEN, Tracker, parse_hex_frame, unpack_frame};

fn run_iq_cmd(args: &[&str]) -> (std::process::ExitStatus, Vec<u8>, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_trck"))
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn iq");
    let stdout = collect_stdout(child.stdout.take().expect("stdout"));
    let stderr = collect_stdout(child.stderr.take().expect("stderr"));
    let status = wait_exit(&mut child, Duration::from_secs(60));
    std::thread::sleep(Duration::from_millis(50));
    let out = stdout.lock().expect("stdout").clone();
    let err = String::from_utf8_lossy(&stderr.lock().expect("stderr")).into_owned();
    (status, out, err)
}

fn run_iq_file(
    path: &Path,
    rate: u32,
    table: &Path,
) -> (std::process::ExitStatus, Vec<u8>, String) {
    let rate = rate.to_string();
    run_iq_cmd(&[
        "--iq",
        path.to_str().expect("utf8"),
        "--iq-rate",
        &rate,
        "--uart",
        "-",
        "--table",
        table.to_str().expect("utf8"),
    ])
}

fn iq_count(stderr: &str, name: &str) -> u64 {
    let line = stderr
        .lines()
        .rev()
        .find(|line| line.starts_with("iq messages="))
        .unwrap_or(stderr);
    let prefix = format!("{name}=");
    for part in line.split_whitespace() {
        if let Some(value) = part.strip_prefix(&prefix) {
            return value
                .parse()
                .unwrap_or_else(|_| panic!("{name} in {stderr}"));
        }
    }
    panic!("missing {name}= in {stderr}");
}

fn assert_uart_matches_truth(uart: &[u8], truth_text: &str) {
    let truth = parse_truth(truth_text);
    let mut poses: BTreeMap<u32, Vec<(f64, f64, i32)>> = BTreeMap::new();
    for ((_, icao), pose) in &truth {
        poses.entry(*icao).or_default().push(*pose);
    }
    let frames = uart.as_chunks::<FRAME_LEN>().0;
    assert!(!frames.is_empty(), "no UART frames");
    let mut seen: BTreeMap<u32, usize> = BTreeMap::new();
    for frame in frames {
        let fix = unpack_frame(frame).expect("uart");
        let icao = fix.aircraft.icao.to_u24();
        let lat = f64::from(fix.aircraft.latitude_e7) / 10_000_000.0;
        let lon = f64::from(fix.aircraft.longitude_e7) / 10_000_000.0;
        let Some(list) = poses.get(&icao) else {
            panic!("unexpected icao {icao:06x}");
        };
        let err = list
            .iter()
            .map(|&(tlat, tlon, _)| distance_m(tlat, tlon, lat, lon))
            .fold(f64::INFINITY, f64::min);
        assert!(
            err < POS_TOL_M,
            "icao={icao:06x} err={err} m lat={lat} lon={lon}"
        );
        *seen.entry(icao).or_insert(0) += 1;
    }
    for icao in poses.keys() {
        assert!(
            seen.get(icao).copied().unwrap_or(0) > 0,
            "no IQ fixes for {icao:06x}"
        );
    }
}

fn iq_data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

fn skip_missing(path: &Path) -> bool {
    if path.is_file() {
        return false;
    }
    eprintln!("skip {}: run scripts/fetch-iq.sh", path.display());
    true
}

fn find_under(dir: &Path, name: &str) -> Option<PathBuf> {
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
            } else if path.file_name().is_some_and(|n| n == name) {
                return Some(path);
            }
        }
    }
    None
}

fn iq_msg_hex(stderr: &str) -> Vec<String> {
    iq_msgs(stderr).into_iter().map(|(hex, _)| hex).collect()
}

fn iq_msgs(stderr: &str) -> Vec<(String, f64)> {
    stderr
        .lines()
        .filter_map(|line| {
            let rest = line.strip_prefix("msg ")?;
            let (hex, t) = rest.split_once(" t=")?;
            Some((hex.trim().to_ascii_uppercase(), t.parse().ok()?))
        })
        .collect()
}

fn csv_col(header: &[&str], name: &str) -> Option<usize> {
    header.iter().position(|h| h.eq_ignore_ascii_case(name))
}

fn parse_csv_table(text: &str) -> (Vec<String>, Vec<Vec<String>>) {
    let mut lines = text.lines().filter(|l| !l.is_empty());
    let header: Vec<String> = lines
        .next()
        .unwrap_or("")
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();
    let rows = lines
        .map(|line| {
            line.split(',')
                .map(|s| s.trim().to_string())
                .collect::<Vec<_>>()
        })
        .collect();
    (header, rows)
}

fn hex_norm(text: &str) -> String {
    text.trim()
        .trim_start_matches("0x")
        .trim_start_matches("0X")
        .trim_end_matches(';')
        .to_ascii_uppercase()
}

#[test]
fn iq_help_lists_the_knobs() {
    let out = Command::new(env!("CARGO_BIN_EXE_trck"))
        .arg("--help")
        .output()
        .expect("help");
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("--iq"), "{text}");
    assert!(text.contains("--iq-rate"), "{text}");
}

#[test]
fn iq_crossing_matches_truth() {
    let iq = unique_path("cross.cu8");
    let frames_path = unique_path("frames.csv");
    let truth_path = unique_path("truth.csv");
    let table = table_arg();
    let (status, _, stderr) = run_sim(&[
        CROSSING,
        "--out",
        frames_path.to_str().expect("utf8"),
        "--truth",
        truth_path.to_str().expect("utf8"),
        "--iq-out",
        iq.to_str().expect("utf8"),
    ]);
    assert!(status.success(), "{stderr}");
    let (status, stdout, stderr) = run_iq_file(&iq, 2_400_000, &table);
    assert!(status.success(), "{stderr}");
    let truth = fs::read_to_string(&truth_path).expect("truth");
    assert_uart_matches_truth(&stdout, &truth);
    let messages = iq_count(&stderr, "messages");
    let generated = fs::read_to_string(&frames_path)
        .expect("frames")
        .lines()
        .filter(|l| !l.is_empty())
        .count() as u64;
    assert!(
        messages >= generated,
        "messages={messages} generated={generated} {stderr}"
    );
    let _ = fs::remove_file(&iq);
    let _ = fs::remove_file(&frames_path);
    let _ = fs::remove_file(&truth_path);
    let _ = fs::remove_file(&table);
}

#[test]
fn iq_decode_rate_vs_snr() {
    let frames_path = unique_path("snr-frames.csv");
    let (status, _, stderr) = run_sim(&[CROSSING, "--out", frames_path.to_str().expect("utf8")]);
    assert!(status.success(), "{stderr}");
    let generated = fs::read_to_string(&frames_path)
        .expect("frames")
        .lines()
        .filter(|l| !l.is_empty())
        .count() as u64;
    assert!(generated > 0);
    // SNR = 10 log10(A² / (2σ²)): on-chip pulse power over complex AWGN (σ on each of I and Q).
    eprintln!(
        "SNR = 10 log10(A^2 / (2 sigma^2)): on-chip pulse power over complex AWGN (sigma on I and on Q)"
    );
    let mut rows = Vec::new();
    for snr in [20.0, 15.0, 12.0, 10.0, 8.0, 6.0] {
        let iq = unique_path(&format!("snr-{snr}.cu8"));
        let table = table_arg();
        let snr_s = format!("{snr}");
        let (status, _, stderr) = run_sim(&[
            CROSSING,
            "--out",
            frames_path.to_str().expect("utf8"),
            "--iq-out",
            iq.to_str().expect("utf8"),
            "--iq-snr",
            &snr_s,
            "--iq-seed",
            "1",
        ]);
        assert!(status.success(), "{stderr}");
        let (status, _, stderr) = run_iq_file(&iq, 2_400_000, &table);
        assert!(status.success(), "{stderr}");
        let messages = iq_count(&stderr, "messages");
        let rate = messages as f64 / generated as f64;
        eprintln!("iq snr={snr} dB messages={messages}/{generated} rate={rate:.3}");
        rows.push((snr, messages, rate));
        let _ = fs::remove_file(&iq);
        let _ = fs::remove_file(&table);
    }
    let rate_at = |db: f64| {
        rows.iter()
            .find(|(snr, _, _)| (*snr - db).abs() < 0.1)
            .map(|(_, _, r)| *r)
            .unwrap_or(0.0)
    };
    assert!(
        rate_at(20.0) >= 0.9,
        "20 dB decode rate {} is below 0.9: {rows:?}",
        rate_at(20.0)
    );
    // rs1090's dump1090-2400 needs a perfect 112-bit CRC (no 1-bit correction).
    // That cliff is ~12 dB for this generator; 10 dB still finds the preamble.
    assert!(
        rate_at(12.0) >= 0.75,
        "12 dB decode rate {} is below 0.75: {rows:?}",
        rate_at(12.0)
    );
    assert!(
        rate_at(10.0) >= 0.2,
        "10 dB decode rate {} collapsed (want this demod's ~0.3 floor, not 0): {rows:?}",
        rate_at(10.0)
    );
    let _ = fs::remove_file(&frames_path);
}

#[test]
fn iq_2msps_still_decodes() {
    let iq = unique_path("2msps.cu8");
    let frames_path = unique_path("2msps-frames.csv");
    let table = table_arg();
    let (status, _, stderr) = run_sim(&[
        CROSSING,
        "--iq-out",
        iq.to_str().expect("utf8"),
        "--iq-rate",
        "2000000",
        "--out",
        frames_path.to_str().expect("utf8"),
    ]);
    assert!(status.success(), "{stderr}");
    let (status, stdout, stderr) = run_iq_file(&iq, 2_000_000, &table);
    assert!(status.success(), "{stderr}");
    assert!(iq_count(&stderr, "messages") > 0, "{stderr}");
    assert!(iq_count(&stderr, "fixes") > 0, "{stderr}");
    assert!(stdout.windows(4).any(|w| w == b"TRCK"), "{stderr}");
    let _ = fs::remove_file(&iq);
    let _ = fs::remove_file(&frames_path);
    let _ = fs::remove_file(&table);
}

#[test]
#[ignore = "needs scripts/fetch-iq.sh"]
fn iq_modes1_recording() {
    let path = iq_data("modes1.bin");
    if skip_missing(&path) {
        return;
    }
    assert_eq!(fs::metadata(&path).expect("meta").len(), 713_736);
    let table = table_arg();
    let (status, _, stderr) = run_iq_file(&path, 2_000_000, &table);
    assert!(status.success(), "{stderr}");
    let messages = iq_count(&stderr, "messages");
    eprintln!("iq modes1.bin messages={messages}");
    assert!(
        messages >= 20,
        "modes1.bin decoded {messages} frames. {stderr}"
    );
    let _ = fs::remove_file(&table);
}

#[test]
#[ignore = "needs scripts/fetch-iq.sh"]
fn iq_awec2026_recording() {
    let dir = iq_data("awec2026");
    if !dir.is_dir() {
        eprintln!("skip {}: run scripts/fetch-iq.sh", dir.display());
        return;
    }
    let recording = find_under(&dir, "recording.bin").expect("recording.bin");
    let msg_csv = find_under(&dir, "messages.csv").expect("messages.csv");
    let pos_csv = find_under(&dir, "positions.csv").expect("positions.csv");
    let table = table_arg();
    let (status, stdout, stderr) = run_iq_file(&recording, 2_000_000, &table);
    assert!(status.success(), "{stderr}");
    let got_hex: std::collections::HashSet<String> = iq_msg_hex(&stderr).into_iter().collect();
    let msg_text = fs::read_to_string(&msg_csv).expect("messages csv");
    let (header, rows) = parse_csv_table(&msg_text);
    let heads: Vec<&str> = header.iter().map(String::as_str).collect();
    let hex_i = csv_col(&heads, "message_hex")
        .or_else(|| csv_col(&heads, "hex"))
        .unwrap_or_else(|| panic!("hex column in {header:?}"));
    let expect_hex: Vec<String> = rows
        .iter()
        .filter_map(|row| row.get(hex_i).map(|s| hex_norm(s)))
        .filter(|s| s.len() >= 14)
        .collect();
    let matched = expect_hex
        .iter()
        .filter(|h| {
            got_hex
                .iter()
                .any(|g| g.starts_with(h.as_str()) || h.starts_with(g.as_str()))
        })
        .count();
    eprintln!(
        "iq awec2026 messages matched {matched}/{} (we decoded {})",
        expect_hex.len(),
        got_hex.len()
    );
    assert_eq!(expect_hex.len(), 122, "expected 122 decoded message rows");
    assert!(
        matched >= 40,
        "recovered {matched}/122 messages; got {}",
        got_hex.len()
    );

    let pos_text = fs::read_to_string(&pos_csv).expect("positions csv");
    let (pheader, prows) = parse_csv_table(&pos_text);
    let pheads: Vec<&str> = pheader.iter().map(String::as_str).collect();
    let lat_i = csv_col(&pheads, "latitude_deg")
        .or_else(|| csv_col(&pheads, "lat"))
        .expect("lat");
    let lon_i = csv_col(&pheads, "longitude_deg")
        .or_else(|| csv_col(&pheads, "lon"))
        .expect("lon");
    let hex_p = csv_col(&pheads, "message_hex").expect("pos hex");
    let t_i = csv_col(&pheads, "time_s").expect("time_s");
    let iv_i = csv_col(&pheads, "cpr_pair_interval_s");
    assert_eq!(prows.len(), 10, "expected 10 position points");

    let have_hex = |hex: &str| {
        got_hex
            .iter()
            .any(|g| g.starts_with(hex) || hex.starts_with(g.as_str()))
    };
    let mut decoded_pos = 0usize;
    for row in &prows {
        let hex = hex_norm(&row[hex_p]);
        let t: f64 = row[t_i].parse().expect("t");
        let have = have_hex(&hex);
        if have {
            decoded_pos += 1;
        }
        let iv = iv_i
            .and_then(|i| row.get(i))
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(0.0);
        eprintln!("iq awec2026 pos t={t:.3} interval={iv:.2}s decoded={have} {hex}");
    }
    eprintln!("iq awec2026 position frames decoded {decoded_pos}/10");

    // CSV lists a lat/lon on every airborne CPR frame, pairing with the other
    // parity even if it is still in the future (intervals 5–8 s on the first
    // four evens). rs1090 only solves when both parities have already arrived,
    // so those four rows are not decoder misses.
    let mut csv_tracker = Tracker::new();
    let mut csv_solves = 0usize;
    let mut csv_vs_row = Vec::new();
    for row in &prows {
        let hex = hex_norm(&row[hex_p]);
        let t: f64 = row[t_i].parse().expect("t");
        let frame = parse_hex_frame(hex.as_bytes()).expect("pos frame");
        if let Some(fix) = csv_tracker.ingest(t, &frame) {
            csv_solves += 1;
            let lat = f64::from(fix.aircraft.latitude_e7) / 10_000_000.0;
            let lon = f64::from(fix.aircraft.longitude_e7) / 10_000_000.0;
            let csv_lat: f64 = row[lat_i].parse().expect("lat");
            let csv_lon: f64 = row[lon_i].parse().expect("lon");
            let vs = distance_m(csv_lat, csv_lon, lat, lon);
            csv_vs_row.push(vs);
            eprintln!("iq awec2026 CSV row at t={t:.3} vs rs1090(all 10) {vs:.1} m");
        }
    }
    eprintln!(
        "iq awec2026 rs1090 solves {csv_solves}/10 CSV frames (prefix evens stay unpaired); vs-row m {csv_vs_row:?}"
    );

    let mut decoded_msgs = iq_msgs(&stderr);
    decoded_msgs.sort_by(|a, b| a.1.total_cmp(&b.1));
    let mut live = Tracker::new();
    let mut ref_fixes = Vec::new();
    for (hex, t) in &decoded_msgs {
        let Some(frame) = parse_hex_frame(hex.as_bytes()) else {
            continue;
        };
        if let Some(fix) = live.ingest(*t, &frame) {
            let lat = f64::from(fix.aircraft.latitude_e7) / 10_000_000.0;
            let lon = f64::from(fix.aircraft.longitude_e7) / 10_000_000.0;
            ref_fixes.push((lat, lon));
        }
    }
    let frames = stdout.as_chunks::<FRAME_LEN>().0;
    assert_eq!(
        frames.len(),
        ref_fixes.len(),
        "UART fixes {} vs rs1090 pairs {} from decoded IQ messages",
        frames.len(),
        ref_fixes.len()
    );
    let mut max_err = 0.0f64;
    for (frame, &(rlat, rlon)) in frames.iter().zip(ref_fixes.iter()) {
        let fix = unpack_frame(frame).expect("uart");
        let lat = f64::from(fix.aircraft.latitude_e7) / 10_000_000.0;
        let lon = f64::from(fix.aircraft.longitude_e7) / 10_000_000.0;
        let err = distance_m(rlat, rlon, lat, lon);
        if err > max_err {
            max_err = err;
        }
    }
    eprintln!(
        "iq awec2026 UART {} fixes match the live CPR path; max {max_err:.2} m",
        frames.len()
    );
    assert!(
        max_err < POS_TOL_M,
        "IQ positions drifted from the same CPR pairs: {max_err} m"
    );
    let _ = fs::remove_file(&table);
}

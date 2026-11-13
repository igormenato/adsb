mod common;
use common::*;

use std::fs;
use std::os::fd::AsRawFd;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use trck::{FRAME_LEN, unpack_frame};

fn run_stress(args: &[&str]) -> (std::process::ExitStatus, Vec<u8>, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_trck"))
        .arg("stress")
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn stress");
    let stdout = collect_stdout(child.stdout.take().expect("stdout"));
    let stderr = collect_stdout(child.stderr.take().expect("stderr"));
    let status = wait_exit(&mut child, Duration::from_secs(8));
    std::thread::sleep(Duration::from_millis(50));
    let out = stdout.lock().expect("stdout").clone();
    let err = String::from_utf8_lossy(&stderr.lock().expect("stderr")).into_owned();
    (status, out, err)
}
fn summary_count(stderr: &str, name: &str) -> u64 {
    let line = stderr
        .lines()
        .rev()
        .find(|line| line.starts_with("stress messages="))
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

#[test]
fn stress_one_over_tracker_cap_still_emits() {
    let table = table_arg();
    let (status, stdout, stderr) = run_stress(&[
        "--uart",
        "-",
        "--table",
        table.to_str().expect("utf8"),
        "--messages",
        "514",
        "--aircraft",
        "257",
        "--rate",
        "0",
        "--ident",
        "0",
        "--velocity",
        "0",
        "--junk",
        "0",
        "--emergency",
        "0",
    ]);
    assert!(status.success(), "{stderr}");
    let frames = stdout.len() / FRAME_LEN;
    assert_eq!(frames, 256, "{stderr} bytes={}", stdout.len());
    assert_eq!(summary_count(&stderr, "fixes"), 256, "{stderr}");
    let _ = fs::remove_file(&table);
}
#[test]
fn stress_help_lists_the_knobs() {
    let out = Command::new(env!("CARGO_BIN_EXE_trck"))
        .args(["stress", "--help"])
        .output()
        .expect("help");
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    for flag in [
        "--aircraft",
        "--rate",
        "--seconds",
        "--messages",
        "--emergency",
        "--ident",
        "--velocity",
        "--junk",
        "--uart",
        "--table",
    ] {
        assert!(text.contains(flag), "{text}");
    }
}

#[test]
fn stress_rejects_a_fraction_outside_0_1() {
    let out = Command::new(env!("CARGO_BIN_EXE_trck"))
        .args(["stress", "--emergency", "2", "--uart", "-"])
        .output()
        .expect("bad fraction");
    assert!(!out.status.success(), "{out:?}");
}

#[test]
fn stress_writes_frames_csv_and_a_summary() {
    let table = table_arg();
    let (status, stdout, stderr) = run_stress(&[
        "--uart",
        "-",
        "--table",
        table.to_str().expect("utf8"),
        "--messages",
        "40",
        "--aircraft",
        "4",
        "--rate",
        "0",
        "--ident",
        "0.1",
        "--velocity",
        "0.1",
        "--junk",
        "0.1",
        "--emergency",
        "0.25",
    ]);
    assert!(status.success(), "{stderr}");
    assert!(
        stdout.windows(4).any(|window| window == b"TRCK"),
        "no UART frames: {} bytes",
        stdout.len()
    );
    assert_eq!(stdout.len() % FRAME_LEN, 0);
    for frame in stdout.as_chunks::<FRAME_LEN>().0 {
        assert!(unpack_frame(frame).is_some(), "{frame:02x?}");
    }
    for token in ["messages=", "fixes=", "dropped=", "csv=", "elapsed="] {
        assert!(stderr.contains(token), "{stderr}");
    }
    let rows = fs::read_to_string(&table).expect("csv");
    assert!(rows.lines().count() > 1, "{rows}");
    let _ = fs::remove_file(&table);
}

#[test]
fn stress_7700_is_on_the_wire() {
    let table = table_arg();
    let (status, stdout, stderr) = run_stress(&[
        "--uart",
        "-",
        "--table",
        table.to_str().expect("utf8"),
        "--messages",
        "8",
        "--aircraft",
        "1",
        "--rate",
        "0",
        "--ident",
        "0",
        "--velocity",
        "0",
        "--junk",
        "0",
        "--emergency",
        "1",
    ]);
    assert!(status.success(), "{stderr}");
    let frames = stdout.as_chunks::<FRAME_LEN>().0;
    assert!(!frames.is_empty(), "no frames");
    assert!(
        frames.iter().any(|frame| frame_is_general(frame)),
        "no 7700: {} frames",
        frames.len()
    );
    let _ = fs::remove_file(&table);
}
#[test]
fn stress_drops_on_a_full_tty_and_still_writes_csv() {
    let table = table_arg();
    let pty = Pty::open();
    let mut child = Command::new(env!("CARGO_BIN_EXE_trck"))
        .args([
            "stress",
            "--uart",
            &pty.path(),
            "--table",
            table.to_str().expect("utf8"),
            "--messages",
            "200",
            "--aircraft",
            "4",
            "--rate",
            "40",
            "--emergency",
            "0.5",
            "--ident",
            "0",
            "--velocity",
            "0",
            "--junk",
            "0",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn stress");
    let stderr_buf = collect_stdout(child.stderr.take().expect("stderr"));
    std::thread::sleep(Duration::from_millis(200));
    let fd = pty.slave.as_raw_fd();
    let filling = Arc::new(AtomicBool::new(true));
    let fill_flag = Arc::clone(&filling);
    let filler = std::thread::spawn(move || {
        while fill_flag.load(Ordering::Relaxed) {
            write_until_would_block(fd);
            std::thread::sleep(Duration::from_millis(10));
        }
    });
    let status = wait_exit(&mut child, Duration::from_secs(15));
    filling.store(false, Ordering::Relaxed);
    let _ = filler.join();
    std::thread::sleep(Duration::from_millis(50));
    let stderr = String::from_utf8_lossy(&stderr_buf.lock().expect("stderr")).into_owned();
    assert!(status.success(), "{stderr}");
    let fixes = summary_count(&stderr, "fixes");
    let dropped = summary_count(&stderr, "dropped");
    let csv = summary_count(&stderr, "csv");
    assert!(fixes > QUEUE as u64, "fixes={fixes} {stderr}");
    assert!(dropped > 0, "no drops {stderr}");
    let rows = wait_csv_rows(&table, 1, Duration::from_secs(1));
    assert_eq!(csv, fixes);
    assert_eq!(rows.len(), fixes as usize);
    let _ = fs::remove_file(&table);
}

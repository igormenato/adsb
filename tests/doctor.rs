mod common;
use common::*;

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn doctor_output(dir: &std::path::Path) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_trck"))
        .arg("doctor")
        .current_dir(dir)
        .output()
        .expect("doctor")
}

#[test]
fn doctor_and_check_together_are_an_error() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_trck"))
        .args(["doctor", "--check", "--uart", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    let buf = collect_stdout(child.stdout.take().unwrap());
    let started = Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break status;
        }
        if started.elapsed() > Duration::from_secs(2) {
            let _ = child.kill();
            panic!("doctor with --check kept running");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(!status.success(), "{status}");
    assert!(buf.lock().expect("lock").is_empty());
}
#[test]
fn doctor_prints_one_line_per_check_and_does_not_send_frames() {
    let dir = std::env::temp_dir().join(format!("trck-doctor-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let out = doctor_output(&dir);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = stdout.lines().filter(|line| !line.is_empty()).collect();
    assert!(
        lines.len() >= 6,
        "expected one line per check, got {lines:?}"
    );
    for line in &lines {
        let status = line.split_whitespace().next();
        assert!(matches!(status, Some("ok" | "fail" | "warn")), "{line}");
    }
    let names: Vec<&str> = lines
        .iter()
        .filter_map(|line| line.split_whitespace().nth(1))
        .collect();
    for name in [
        "rtlsdr:",
        "uart:",
        "uart-console:",
        "csv:",
        "usb:",
        "clock:",
    ] {
        assert!(names.contains(&name), "{stdout}");
    }
    assert!(
        lines.iter().any(|line| line.starts_with("ok csv:")),
        "{stdout}"
    );
    assert!(!out.stdout.windows(4).any(|window| window == b"TRCK"));
    assert_ne!(out.status.code(), Some(0), "{stdout}");
    let _ = std::fs::remove_dir_all(&dir);
}

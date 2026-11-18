mod common;
use common::*;

use std::fs;
use std::process::Command;
#[test]
fn help_lists_install_service() {
    let out = Command::new(env!("CARGO_BIN_EXE_trck"))
        .arg("install-service")
        .arg("--help")
        .output()
        .expect("help");
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("systemd"), "{text}");
    assert!(text.contains("ARGS"), "{text}");
}

#[test]
fn install_service_needs_root() {
    let out = Command::new(env!("CARGO_BIN_EXE_trck"))
        .arg("install-service")
        .output()
        .expect("install-service");
    assert!(!out.status.success(), "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("root"), "{err}");

    let out = Command::new(env!("CARGO_BIN_EXE_trck"))
        .args(["install-service", "--", "--ppm", "-8", "--gain", "19.7"])
        .output()
        .expect("install-service args");
    assert!(!out.status.success(), "{out:?}");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("root"), "{err}");
    assert!(!err.contains("unexpected"), "{err}");
}

#[test]
fn systemd_analyze_verify_rewritten_unit() {
    let dir = unique_path("unit");
    fs::create_dir_all(&dir).expect("dir");
    let exe = env!("CARGO_BIN_EXE_trck");
    let src = include_str!("../packaging/trck.service");
    let unit = src
        .replace("/usr/local/bin/trck", exe)
        .replace("/var/lib/trck", dir.to_str().expect("utf8"));
    assert!(unit.contains("StartLimitIntervalSec=0"), "{unit}");
    assert!(unit.contains("--allow-power-commands"), "{unit}");
    let path = dir.join("trck-adsb.service");
    fs::write(&path, &unit).expect("unit");
    let out = Command::new("systemd-analyze")
        .args(["verify", path.to_str().expect("utf8")])
        .output()
        .expect("systemd-analyze");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    eprintln!(
        "systemd-analyze verify status={} stdout={stdout} stderr={stderr}",
        out.status
    );
    assert!(out.status.success(), "{out:?}");
    let _ = fs::remove_dir_all(&dir);
}

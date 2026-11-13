mod common;
use common::*;

use std::fs;
use std::process::Command;

#[test]
fn sim_help_lists_the_knobs() {
    let out = Command::new(env!("CARGO_BIN_EXE_trck"))
        .args(["sim", "--help"])
        .output()
        .expect("help");
    assert!(out.status.success(), "{out:?}");
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("--out"), "{text}");
    assert!(text.contains("--truth"), "{text}");
    assert!(text.contains("--trace"), "{text}");
    assert!(text.contains("--iq-out"), "{text}");
    assert!(
        text.contains("SCENARIO") || text.contains("scenario"),
        "{text}"
    );
}

#[test]
fn sim_rejects_a_malformed_scenario() {
    let path = unique_path("bad.toml");
    fs::write(&path, "duration = 1\n").expect("write");
    let (status, stdout, stderr) = run_sim(&[path.to_str().expect("utf8")]);
    assert!(!status.success(), "{stderr}");
    assert!(stdout.is_empty(), "{stdout}");
    assert!(stderr.contains("sim:"), "{stderr}");
    assert!(stderr.contains("aircraft"), "{stderr}");
    let _ = fs::remove_file(&path);
}

#[test]
fn sim_output_is_byte_identical() {
    let (a_status, a, a_err) = run_sim(&[CROSSING]);
    let (b_status, b, b_err) = run_sim(&[CROSSING]);
    assert!(a_status.success(), "{a_err}");
    assert!(b_status.success(), "{b_err}");
    assert_eq!(a, b);
    assert!(a.starts_with("0.000,"), "{a}");
}

#[test]
fn sim_fixes_match_scenario_ground_truth() {
    let crossing = assert_sim_matches_truth(CROSSING, true);
    let climb = assert_sim_matches_truth(CLIMB, false);
    assert_eq!(crossing.len(), 2, "{crossing:?}");
    assert_eq!(climb.len(), 1, "{climb:?}");
}

#[test]
fn sim_trace_fixes_match_truth() {
    let frames_path = unique_path("frames.csv");
    let truth_path = unique_path("truth.csv");
    let mut args: Vec<&str> = vec!["--trace"];
    args.extend_from_slice(GRU_TRACES);
    args.extend_from_slice(&[
        "--out",
        frames_path.to_str().expect("utf8"),
        "--truth",
        truth_path.to_str().expect("utf8"),
    ]);
    let (status, _, stderr) = run_sim(&args);
    assert!(status.success(), "{stderr}");
    let frames = fs::read_to_string(&frames_path).expect("frames");
    let truth = fs::read_to_string(&truth_path).expect("truth");
    let (max_err, fixes, saw_callsign, _) = ingest_against_truth("gru-traces", &frames, &truth);
    assert!(saw_callsign, "no callsign");
    assert_eq!(max_err.len(), 3, "{max_err:?}");
    assert_eq!(fixes.len(), 3, "{fixes:?}");
    let _ = fs::remove_file(&frames_path);
    let _ = fs::remove_file(&truth_path);
}

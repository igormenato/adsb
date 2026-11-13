mod common;
use common::*;

use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};
use std::time::Duration;

use trck::modes::{EVEN, ODD, hex14};
use trck::{
    DEFAULT_PATH, EmergencyKind, FRAME_LEN, Fix, check_positions, crc16_ccitt_false, pack_frame,
    unpack_frame,
};

#[test]
fn pack_and_unpack_are_inverses() {
    assert_eq!(crc16_ccitt_false(b"123456789"), 0x29B1);
    for position in check_positions() {
        let frame = pack_frame(&position);
        assert_eq!(unpack_frame(&frame), Some(position));
    }
    assert!(unpack_frame(&PUBLISHED[..FRAME_LEN - 1]).is_none());
    let mut bad_crc = PUBLISHED;
    bad_crc[FRAME_LEN - 2] ^= 0xFF;
    assert!(unpack_frame(&bad_crc).is_none());
    let mut bad_magic = PUBLISHED;
    bad_magic[0] = b'X';
    assert!(unpack_frame(&bad_magic).is_none());
}

#[test]
fn check_positions_match_published_frames() {
    let got: Vec<_> = check_positions()
        .into_iter()
        .map(|fix| pack_frame(&fix))
        .collect();
    assert_eq!(got[0], SECOND);
    assert_eq!(got[1], PUBLISHED);
    assert_eq!(got[2], CHECK_EMERGENCY);
    let mut general = check_positions()[1];
    general.aircraft.emergency = Some(EmergencyKind::General);
    assert_eq!(pack_frame(&general), PUBLISHED_GENERAL);
    assert_eq!(unpack_frame(&PUBLISHED_GENERAL), Some(general));
}
#[test]
fn nothing_is_written_until_a_position_solves() {
    let (listener, port) = listen();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    serve(listener, move |sock| {
        ready_tx.send(()).expect("ready");
        go_rx.recv().expect("go");
        write_pair(sock);
        Session::KeepOpen
    });
    let mut sender = spawn_hex(port);
    let buf = collect_stdout(sender.0.stdout.take().unwrap());
    ready_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("connected");
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        buf.lock().expect("lock").is_empty(),
        "bytes before a pair: {:02x?}",
        buf.lock().expect("lock")
    );
    go_tx.send(()).expect("release");
    let got = wait_len(&buf, FRAME_LEN, Duration::from_secs(2));
    assert_eq!(got.len(), FRAME_LEN);
    assert_eq!(
        frame_aircraft(&got.as_chunks::<FRAME_LEN>().0[0]),
        decoded_aircraft()
    );
    assert!(sender.0.try_wait().unwrap().is_none());
}

#[test]
fn one_frame_per_solved_position() {
    let (listener, port) = listen();
    serve(listener, |sock| {
        writeln!(sock, "not hex").expect("junk");
        writeln!(sock, "{}", hex14(&EVEN)).expect("even");
        writeln!(sock, "{}", hex14(&ODD)).expect("odd");
        Session::KeepOpen
    });
    let mut sender = spawn_hex(port);
    let buf = collect_stdout(sender.0.stdout.take().unwrap());
    let expect = decoded_aircraft();
    let got = wait_len(&buf, FRAME_LEN, Duration::from_secs(2));
    assert_eq!(got.len(), FRAME_LEN);
    assert_eq!(frame_aircraft(&got.as_chunks::<FRAME_LEN>().0[0]), expect);
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(buf.lock().expect("lock").len(), FRAME_LEN);
    assert!(sender.0.try_wait().unwrap().is_none());
}

#[test]
fn the_same_icao_is_sent_twice() {
    let (listener, port) = listen();
    serve(listener, |sock| {
        write_pair(sock);
        write_pair(sock);
        Session::KeepOpen
    });
    let mut sender = spawn_hex(port);
    let buf = collect_stdout(sender.0.stdout.take().unwrap());
    let expect = decoded_aircraft();
    let got = wait_len(&buf, FRAME_LEN * 2, Duration::from_secs(2));
    assert!(
        got.len() >= FRAME_LEN * 2,
        "short output: {} bytes",
        got.len()
    );
    let frames = got.as_chunks::<FRAME_LEN>().0;
    assert!(frames.len() >= 2, "frames {}", frames.len());
    assert_eq!(frame_aircraft(&frames[0]), expect);
    for frame in frames {
        assert_eq!(frame_aircraft(frame).icao, expect.icao);
    }
    assert!(sender.0.try_wait().unwrap().is_none());
}

#[test]
fn a_split_hex_line_is_one_frame() {
    let (listener, port) = listen();
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let (go_tx, go_rx) = std::sync::mpsc::channel::<()>();
    let even = hex14(&EVEN);
    let even = even.as_bytes();
    let (head, tail) = even.split_at(even.len() / 2);
    let head = head.to_vec();
    let mut tail = tail.to_vec();
    tail.push(b'\n');
    serve(listener, move |sock| {
        sock.write_all(&head).expect("head");
        ready_tx.send(()).expect("ready");
        go_rx.recv().expect("go");
        sock.write_all(&tail).expect("tail");
        writeln!(sock, "{}", hex14(&ODD)).expect("odd");
        Session::KeepOpen
    });
    let mut sender = spawn_hex(port);
    let buf = collect_stdout(sender.0.stdout.take().unwrap());
    ready_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("connected");
    std::thread::sleep(Duration::from_millis(150));
    assert!(
        buf.lock().expect("lock").is_empty(),
        "partial line wrote a frame"
    );
    go_tx.send(()).expect("release");
    let got = wait_len(&buf, FRAME_LEN, Duration::from_secs(2));
    assert_eq!(got.len(), FRAME_LEN);
    assert_eq!(
        frame_aircraft(&got.as_chunks::<FRAME_LEN>().0[0]),
        decoded_aircraft()
    );
}

#[test]
fn a_dead_socket_writes_nothing_until_the_next_position() {
    let (listener, port) = listen();
    let mut first = true;
    serve(listener, move |sock| {
        if first {
            first = false;
            return Session::CloseAndAccept;
        }
        write_pair(sock);
        Session::KeepOpen
    });
    let mut sender = spawn_hex(port);
    let buf = collect_stdout(sender.0.stdout.take().unwrap());
    std::thread::sleep(Duration::from_millis(400));
    assert!(
        buf.lock().expect("lock").is_empty(),
        "dead socket wrote {:02x?}",
        buf.lock().expect("lock")
    );
    let got = wait_len(&buf, FRAME_LEN, Duration::from_secs(4));
    assert_eq!(got.len(), FRAME_LEN);
    assert_eq!(
        frame_aircraft(&got.as_chunks::<FRAME_LEN>().0[0]),
        decoded_aircraft()
    );
    assert!(sender.0.try_wait().unwrap().is_none());
}

#[test]
fn a_refused_port_writes_nothing() {
    let (listener, port) = listen();
    drop(listener);
    let mut sender = spawn_hex(port);
    let buf = collect_stdout(sender.0.stdout.take().unwrap());
    std::thread::sleep(Duration::from_millis(700));
    assert!(buf.lock().expect("lock").is_empty());
    assert!(sender.0.try_wait().unwrap().is_none());
}

#[test]
fn check_and_hex_together_are_an_error() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_trck"))
        .args(["--check", "--hex", "127.0.0.1:9", "--uart", "-"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn");
    let buf = collect_stdout(child.stdout.take().unwrap());
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().expect("wait") {
            break status;
        }
        if started.elapsed() > Duration::from_secs(2) {
            let _ = child.kill();
            panic!("--check with --hex kept running");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(!status.success(), "{status}");
    assert!(buf.lock().expect("lock").is_empty());
}
#[test]
fn check_writes_the_published_frames() {
    let expect = [SECOND, PUBLISHED, CHECK_EMERGENCY].concat();
    let mut sender = spawn_check();
    let buf = collect_stdout(sender.0.stdout.take().unwrap());
    let got = wait_len(&buf, expect.len(), Duration::from_secs(2));
    assert!(got.len() >= expect.len(), "short output: {}", got.len());
    assert_eq!(&got[..expect.len()], expect.as_slice());
    assert!(sender.0.try_wait().unwrap().is_none());
}
#[test]
fn check_writes_csv_rows() {
    let table = table_arg();
    let before = now_secs();
    let mut sender = spawn_check_table(&table);
    let buf = collect_stdout(sender.0.stdout.take().unwrap());
    let rows = wait_csv_rows(&table, 3, Duration::from_secs(2));
    let after = now_secs() + 1;
    let expect = check_positions();
    for (row, fix) in rows.iter().zip(expect.iter()) {
        assert_csv_row(row, fix, before, after);
    }
    let got = wait_len(&buf, FRAME_LEN * 3, Duration::from_secs(2));
    assert!(got.len() >= FRAME_LEN * 3);
    assert!(sender.0.try_wait().unwrap().is_none());
}

#[test]
fn hex_writes_csv_when_a_position_solves() {
    let table = table_arg();
    let (listener, port) = listen();
    serve(listener, |sock| {
        write_pair(sock);
        Session::KeepOpen
    });
    let before = now_secs();
    let mut sender = spawn_hex_table(port, &table);
    let buf = collect_stdout(sender.0.stdout.take().unwrap());
    let rows = wait_csv_rows(&table, 1, Duration::from_secs(2));
    let after = now_secs() + 1;
    let expect = Fix {
        aircraft: decoded_aircraft(),
    };
    assert_csv_row(&rows[0], &expect, before, after);
    let got = wait_len(&buf, FRAME_LEN, Duration::from_secs(2));
    assert_eq!(got.len(), FRAME_LEN);
    assert!(sender.0.try_wait().unwrap().is_none());
}
#[test]
fn default_table_is_aircraft_csv_in_the_working_directory() {
    let dir = unique_path("cwd");
    fs::create_dir_all(&dir).expect("cwd");
    let mut sender = StopSender(
        Command::new(env!("CARGO_BIN_EXE_trck"))
            .current_dir(&dir)
            .args(["--check", "--uart", "-"])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn check"),
    );
    let path = dir.join(DEFAULT_PATH);
    let rows = wait_csv_rows(&path, 3, Duration::from_secs(2));
    assert_eq!(rows.len(), 3);
    assert!(sender.0.try_wait().unwrap().is_none());
    drop(sender);
    let _ = fs::remove_dir_all(&dir);
}
#[test]
fn table_flag_overrides_the_path() {
    let dir = unique_path("cwd");
    fs::create_dir_all(&dir).expect("cwd");
    let table = unique_path("override.csv");
    let mut sender = StopSender(
        Command::new(env!("CARGO_BIN_EXE_trck"))
            .current_dir(&dir)
            .args([
                "--check",
                "--uart",
                "-",
                "--table",
                table.to_str().expect("utf8"),
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn check"),
    );
    let rows = wait_csv_rows(&table, 3, Duration::from_secs(2));
    assert_eq!(rows.len(), 3);
    assert!(!dir.join(DEFAULT_PATH).exists());
    assert!(sender.0.try_wait().unwrap().is_none());
    drop(sender);
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_file(&table);
}

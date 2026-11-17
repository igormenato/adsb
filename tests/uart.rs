mod common;
use common::*;

use std::io::{self, Write};
use std::time::{Duration, Instant};

use nix::unistd::read;
use trck::modes::{EVEN, ODD, STATUS, hex14};
use trck::{ACK_LEN, Cmd, EmergencyKind, Fix, pack_cmd};

#[test]
fn avr_hex_writes_a_pty_frame() {
    let mut pty = Pty::open();
    let mut sender = spawn_hex_stdin(&pty.path());
    let mut stdin = sender.0.stdin.take().expect("stdin");
    writeln!(stdin, "*{};", hex14(&STATUS)).expect("status");
    writeln!(stdin, "*{};", hex14(&EVEN)).expect("even");
    writeln!(stdin, "@000000000000{};", hex14(&ODD)).expect("odd");
    stdin.flush().expect("flush");
    let frames = wait_frames(&mut pty.master, 1, Duration::from_secs(2));
    let expect = decoded_7700_aircraft();
    assert_eq!(frame_aircraft(&frames[0]), expect);
    assert_eq!(expect.icao.to_u24(), ICAO_40058B);
    assert_eq!(expect.emergency, Some(EmergencyKind::General));
    assert!(sender.0.try_wait().unwrap().is_none());
}

#[test]
fn a_tty_writes_a_decoded_frame() {
    let (listener, port) = listen();
    let mut pty = Pty::open();
    let mut sender = spawn_uart(port, &pty.path());
    let (mut sock, _) = listener.accept().expect("accept");
    write_pair(&mut sock);
    let frames = wait_frames(&mut pty.master, 1, Duration::from_secs(2));
    assert_eq!(frame_aircraft(&frames[0]), decoded_aircraft());
    assert!(sender.0.try_wait().unwrap().is_none());
}

#[test]
fn queued_tty_frames_survive_reconnect() {
    let (listener, port) = listen();
    let mut pty = Pty::open();
    let mut sender = spawn_uart(port, &pty.path());
    {
        let (mut sock, _) = listener.accept().expect("accept");
        write_pair(&mut sock);
    }
    let (mut sock, _) = listener.accept().expect("accept");
    write_pair(&mut sock);
    let frames = wait_frames(&mut pty.master, 2, Duration::from_secs(5));
    let expect = decoded_aircraft();
    assert_eq!(frame_aircraft(&frames[0]), expect);
    assert_eq!(frame_aircraft(&frames[1]), expect);
    assert!(sender.0.try_wait().unwrap().is_none());
}

#[test]
fn a_full_tty_drops_ordinary_hex_and_keeps_7700() {
    assert_eq!(ordinary_aircraft().icao.to_u24(), ICAO_4D224F);
    let (listener, port) = listen();
    let mut pty = Pty::open();
    let mut sender = spawn_uart(port, &pty.path());
    let (mut sock, _) = listener.accept().expect("accept");
    fill_until_blocked(&pty.slave);
    sock.set_nodelay(true).expect("nodelay");
    let mut body = String::new();
    for _ in 0..=QUEUE {
        body.push_str(ORDINARY_A);
        body.push('\n');
        body.push_str(ORDINARY_B);
        body.push('\n');
    }
    body.push_str(&hex14(&STATUS));
    body.push('\n');
    body.push_str(&hex14(&EVEN));
    body.push('\n');
    body.push_str(&hex14(&ODD));
    body.push('\n');
    sock.write_all(body.as_bytes()).expect("lines");
    let frames = wait_frames_until(&mut pty.master, Duration::from_secs(5), |frames| {
        frames.iter().any(|frame| frame_is_general(frame))
    });
    let icaos: Vec<u32> = frames.iter().map(|frame| frame_icao(frame)).collect();
    let ordinary = icaos
        .iter()
        .copied()
        .filter(|icao| *icao == ICAO_4D224F)
        .count();
    assert!(ordinary > 0, "{icaos:?}");
    assert!(ordinary < QUEUE + 1, "{icaos:?}");
    assert!(
        frames
            .iter()
            .any(|frame| frame_icao(frame) == ICAO_40058B && frame_is_general(frame)),
        "{icaos:?}"
    );
    assert!(sender.0.try_wait().unwrap().is_none());
}

#[test]
fn a_full_tty_of_emergencies_drops_ordinary() {
    assert_eq!(ordinary_aircraft().icao.to_u24(), ICAO_4D224F);
    let (listener, port) = listen();
    let mut pty = Pty::open();
    let mut sender = spawn_uart(port, &pty.path());
    let (mut sock, _) = listener.accept().expect("accept");
    fill_until_blocked(&pty.slave);
    sock.set_nodelay(true).expect("nodelay");
    let even = hex14(&EVEN);
    let odd = hex14(&ODD);
    let mut body = String::new();
    body.push_str(&hex14(&STATUS));
    body.push('\n');
    for _ in 0..=QUEUE {
        body.push_str(&even);
        body.push('\n');
        body.push_str(&odd);
        body.push('\n');
    }
    body.push_str(ORDINARY_A);
    body.push('\n');
    body.push_str(ORDINARY_B);
    body.push('\n');
    sock.write_all(body.as_bytes()).expect("lines");
    let frames = wait_frames_until(&mut pty.master, Duration::from_secs(5), |frames| {
        frames.len() >= QUEUE
            && frames
                .iter()
                .any(|frame| frame_icao(frame) == ICAO_40058B && frame_is_general(frame))
    });
    let icaos: Vec<u32> = frames.iter().map(|frame| frame_icao(frame)).collect();
    assert!(!icaos.contains(&ICAO_4D224F), "{icaos:?}");
    assert!(
        frames
            .iter()
            .any(|frame| frame_icao(frame) == ICAO_40058B && frame_is_general(frame)),
        "{icaos:?}"
    );
    assert!(sender.0.try_wait().unwrap().is_none());
}
#[test]
fn csv_is_written_while_the_tty_is_full() {
    let table = table_arg();
    let (listener, port) = listen();
    let pty = Pty::open();
    let mut sender = spawn_uart_table(port, &pty.path(), &table);
    let (mut sock, _) = listener.accept().expect("accept");
    fill_until_blocked(&pty.slave);
    let before = now_secs();
    write_pair(&mut sock);
    let rows = wait_csv_rows(&table, 1, Duration::from_secs(2));
    let after = now_secs() + 1;
    let expect = Fix {
        aircraft: decoded_aircraft(),
    };
    assert_csv_row(&rows[0], &expect, before, after);
    assert!(sender.0.try_wait().unwrap().is_none());
}

fn wait_tacks(master: &mut serialport::TTYPort, n: usize, timeout: Duration) -> Vec<[u8; ACK_LEN]> {
    set_nonblocking(master);
    let mut raw = Vec::new();
    let started = Instant::now();
    loop {
        let mut tmp = [0u8; 4096];
        match read(as_fd(master), &mut tmp) {
            Ok(0) => {}
            Ok(count) => {
                raw.extend_from_slice(&tmp[..count]);
                let tacks: Vec<[u8; ACK_LEN]> = raw
                    .windows(ACK_LEN)
                    .filter(|w| w.starts_with(b"TACK"))
                    .filter_map(|w| w.try_into().ok())
                    .collect();
                if tacks.len() >= n {
                    return tacks;
                }
            }
            Err(err) => {
                let err = io::Error::from(err);
                assert!(
                    err.kind() == io::ErrorKind::WouldBlock
                        || err.kind() == io::ErrorKind::Interrupted,
                    "{err}"
                );
            }
        }
        if started.elapsed() > timeout {
            panic!("got {} bytes waiting for {n} TACK", raw.len());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn pause_then_status_both_ack() {
    let (listener, port) = listen();
    let mut pty = Pty::open();
    let mut sender = spawn_uart(port, &pty.path());
    let (_sock, _) = listener.accept().expect("accept");
    pty.master.write_all(&pack_cmd(Cmd::Pause)).expect("pause");
    pty.master
        .write_all(&pack_cmd(Cmd::Status))
        .expect("status");
    pty.master.flush().expect("flush");
    let tacks = wait_tacks(&mut pty.master, 2, Duration::from_secs(3));
    assert_eq!(tacks[0][4], Cmd::Pause as u8);
    assert_eq!(tacks[0][5], 0);
    assert_eq!(tacks[0][18], 1);
    assert_eq!(tacks[1][4], Cmd::Status as u8);
    assert_eq!(tacks[1][5], 0);
    assert_eq!(tacks[1][18], 1);
    assert!(sender.0.try_wait().unwrap().is_none());
}

#[test]
fn uart_hangup_reopens_and_acks() {
    let (listener, port) = listen();
    let link = unique_path("uart");
    let first = Pty::open();
    std::os::unix::fs::symlink(first.path(), &link).expect("link");
    let mut sender = spawn_uart(port, link.to_str().expect("utf8"));
    let (_sock, _) = listener.accept().expect("accept");
    drop(first);
    std::thread::sleep(Duration::from_millis(80));
    let mut second = Pty::open();
    std::fs::remove_file(&link).expect("unlink");
    std::os::unix::fs::symlink(second.path(), &link).expect("relink");
    std::thread::sleep(Duration::from_millis(200));
    second
        .master
        .write_all(&pack_cmd(Cmd::Pause))
        .expect("pause");
    second.master.flush().expect("flush");
    let tacks = wait_tacks(&mut second.master, 1, Duration::from_secs(3));
    assert_eq!(tacks[0][4], Cmd::Pause as u8);
    assert_eq!(tacks[0][5], 0);
    assert!(sender.0.try_wait().unwrap().is_none());
    let _ = std::fs::remove_file(&link);
}

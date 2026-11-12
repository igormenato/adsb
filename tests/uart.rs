mod common;
use common::*;

use std::io::Write;
use std::time::Duration;

use trck::modes::{EVEN, ODD, STATUS, hex14};
use trck::{EmergencyKind, Fix};

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

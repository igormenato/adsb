//! Host test: raw Beast is a byte copy, and struct records match the README layout.
//! No radio and no UART.

use adsb_uart_sender::{
    crc16_ccitt_false, forward_raw, modes_crc, modes_parity, pack_message, unpack_message,
    StructForwarder, Track, ADSB_ALT_INVALID, ADSB_FLAG_ALTITUDE, ADSB_FLAG_POSITION,
    ADSB_FLAG_VELOCITY, ADSB_LATLON_INVALID, ADSB_STRUCT_CRC_LEN, ADSB_STRUCT_MAGIC,
    ADSB_STRUCT_SIZE, ADSB_STRUCT_VERSION, ADSB_VEL_INVALID, ADSB_WIRE_MAGIC,
};

const EVEN: [u8; 14] = [
    0x8D, 0x40, 0x62, 0x1D, 0x58, 0xC3, 0x82, 0xD6, 0x90, 0xC8, 0xAC, 0x28, 0x63, 0xA7,
];
const ODD: [u8; 14] = [
    0x8D, 0x40, 0x62, 0x1D, 0x58, 0xC3, 0x86, 0x43, 0x5C, 0xC4, 0x12, 0x69, 0x2A, 0xD6,
];
const BEAST_EXAMPLE: &[u8] = &[
    0x1a, 0x32, 0x08, 0x3e, 0x27, 0xb6, 0xcb, 0x6a, 0x1a, 0x1a, 0x00, 0xa1, 0x84, 0x1a, 0x1a, 0xc3,
    0xb3, 0x1d,
];

fn beast_encode(msg_type: u8, mlat: &[u8], signal: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0x1a];
    for byte in [msg_type]
        .into_iter()
        .chain(mlat.iter().copied())
        .chain([signal])
        .chain(payload.iter().copied())
    {
        out.push(byte);
        if byte == 0x1A {
            out.push(0x1A);
        }
    }
    out
}

fn with_parity(data: &[u8]) -> Vec<u8> {
    let mut msg = data.to_vec();
    msg.extend_from_slice(&modes_parity(data));
    msg
}

fn assert_packed(frame: &[u8], msg: &Track) {
    assert_eq!(
        frame.len(),
        32,
        "struct record is {} bytes, want 32",
        frame.len()
    );
    assert_eq!(&frame[0..2], &ADSB_WIRE_MAGIC);
    assert_eq!(frame[2], 1);
    assert_eq!(frame[3], msg.flags);
    assert_eq!(
        u32::from_le_bytes(frame[4..8].try_into().unwrap()),
        msg.icao & 0x00ff_ffff
    );
    assert_eq!(
        i32::from_le_bytes(frame[8..12].try_into().unwrap()),
        msg.latitude_e7
    );
    assert_eq!(
        i32::from_le_bytes(frame[12..16].try_into().unwrap()),
        msg.longitude_e7
    );
    assert_eq!(
        i32::from_le_bytes(frame[16..20].try_into().unwrap()),
        msg.altitude_ft
    );
    assert_eq!(
        u16::from_le_bytes(frame[20..22].try_into().unwrap()),
        msg.velocity_kt
    );
    assert_eq!(
        u64::from_le_bytes(frame[22..30].try_into().unwrap()),
        msg.timestamp_us
    );
    let expect = crc16_ccitt_false(&frame[..30]);
    assert_eq!(
        u16::from_le_bytes(frame[30..32].try_into().unwrap()),
        expect
    );
    assert_eq!(unpack_message(frame).as_ref(), Some(msg));
}

fn feed(payloads: &[&[u8]], times: &[(f64, u64)]) -> Vec<Track> {
    let mut wire = Vec::new();
    for payload in payloads {
        wire.extend(beast_encode(0x33, &[0; 6], 0, payload));
    }
    let mut times = times.iter().copied();
    let mut forwarder = StructForwarder::new();
    let mut frames = Vec::new();
    forwarder.feed(
        &wire,
        || times.next().expect("clock exhausted"),
        &mut frames,
    );
    assert_eq!(frames.len() / 32, payloads.len());
    frames
        .chunks(32)
        .map(|frame| {
            let decoded = unpack_message(frame).expect("sender struct did not unpack");
            assert_packed(frame, &decoded);
            decoded
        })
        .collect()
}

fn velocity_squitter() -> Vec<u8> {
    // TC 19 subtype 1, 300 kt east and 400 kt north. Speed is 500 kt.
    let me = [0x99u8, 0x01, 0x2D, 0x32, 0x20, 0x00, 0x00];
    with_parity(&[
        0x8D, 0xAB, 0xC1, 0x23, me[0], me[1], me[2], me[3], me[4], me[5], me[6],
    ])
}

fn gnss_squitter() -> Vec<u8> {
    // TC 20, GNSS height 1000 m. Feet are trunc(meters * 3.28084) = 3280.
    let me = [0xA0u8, 0x3E, 0x80, 0x00, 0x00, 0x00, 0x00];
    with_parity(&[
        0x8D, 0x00, 0x00, 0x01, me[0], me[1], me[2], me[3], me[4], me[5], me[6],
    ])
}

#[test]
fn host_bench() {
    assert_eq!(ADSB_STRUCT_SIZE, 32);
    assert_eq!(ADSB_STRUCT_CRC_LEN, 30);
    assert_eq!(ADSB_STRUCT_MAGIC, 0xAD5B);
    assert_eq!(ADSB_WIRE_MAGIC, [0x5B, 0xAD]);
    assert_eq!(ADSB_STRUCT_VERSION, 1);

    assert_eq!(modes_crc(&EVEN), 0);
    assert_eq!(modes_crc(&ODD), 0);
    assert_eq!(crc16_ccitt_false(b"123456789"), 0x29B1);
    assert_eq!(with_parity(&EVEN[..11]), EVEN);

    assert!(BEAST_EXAMPLE.windows(2).any(|pair| pair == [0x1a, 0x1a]));
    assert_eq!(forward_raw(BEAST_EXAMPLE), BEAST_EXAMPLE);
    let again = beast_encode(0x32, &hex("083e27b6cb6a"), 0x1A, &hex("00a1841ac3b31d"));
    assert_eq!(again, BEAST_EXAMPLE);

    let samples = [
        beast_encode(0x31, &hex("010203040506"), 0x00, &hex("00ab")),
        beast_encode(0x32, &hex("1a1a1a1a1a1a"), 0x1A, &hex("0011223344551a")),
        beast_encode(0x33, &[0; 6], 0xFF, &EVEN),
        beast_encode(0x33, &[0xff; 6], 0x10, &ODD),
    ];
    let mut stream = vec![0x00, 0xff];
    for sample in &samples {
        stream.extend_from_slice(sample);
    }
    assert_eq!(forward_raw(&stream), stream.as_slice());
    let escaped = beast_encode(0x33, &[0x00, 0x1a, 0x00, 0x00, 0x00, 0x00], 0x00, &EVEN);
    assert!(escaped.iter().filter(|byte| **byte == 0x1a).count() >= 2);
    assert_eq!(forward_raw(&escaped), escaped.as_slice());

    assert_eq!(modes_crc(&velocity_squitter()), 0);
    assert_eq!(modes_crc(&gnss_squitter()), 0);

    let odd_wire = beast_encode(0x33, &hex("112233445566"), 0x1A, &ODD);
    let even_wire = beast_encode(0x33, &[0; 6], 0x00, &EVEN);
    assert!(odd_wire.windows(2).any(|pair| pair == [0x1a, 0x1a]));
    let mut times = [(1000.0, 1_000_000_000u64), (1005.0, 1_005_000_000)].into_iter();
    let mut forwarder = StructForwarder::new();
    let head = {
        let mut head = vec![0x99, 0x1A, 0x00];
        head.extend_from_slice(&odd_wire[..5]);
        head
    };
    let mut early = Vec::new();
    forwarder.feed(&head, || times.next().expect("clock"), &mut early);
    assert!(early.is_empty(), "an incomplete Beast frame was emitted");
    let mut tail = odd_wire[5..].to_vec();
    tail.extend_from_slice(&even_wire);
    let mut frames = Vec::new();
    forwarder.feed(&tail, || times.next().expect("clock"), &mut frames);
    assert_eq!(frames.len(), 64);
    assert_eq!(
        forward_raw(&[&odd_wire[..], &even_wire[..]].concat()),
        [&odd_wire[..], &even_wire[..]].concat()
    );

    let first = unpack_message(&frames[..32]).expect("sender struct did not unpack");
    let second = unpack_message(&frames[32..]).expect("sender struct did not unpack");
    assert_packed(&frames[..32], &first);
    assert_packed(&frames[32..], &second);
    assert_eq!(first.flags, ADSB_FLAG_ALTITUDE);
    assert_eq!(first.altitude_ft, 38000);
    assert_ne!(second.flags & ADSB_FLAG_POSITION, 0);
    let lat = f64::from(second.latitude_e7) / 1e7;
    let lon = f64::from(second.longitude_e7) / 1e7;
    assert!((lat - 52.2572021484375).abs() <= 1e-6, "CPR latitude {lat}");
    assert!(
        (lon - 3.91937255859375).abs() <= 1e-6,
        "CPR longitude {lon}"
    );
    assert_eq!(second.latitude_e7, 522_572_021);
    assert_eq!(second.longitude_e7, 39_193_726);
    assert_eq!(second.altitude_ft, 38000);
    assert_eq!(second.icao, 0x40621D);
    assert_eq!(second.timestamp_us, 1_005_000_000);

    let late = &feed(&[&ODD, &EVEN], &[(1000.0, 1), (1011.0, 2)])[1];
    assert_eq!(late.flags & ADSB_FLAG_POSITION, 0);
    assert_eq!(late.altitude_ft, 38000);

    let on_time = &feed(&[&ODD, &EVEN], &[(1000.0, 1), (1010.0, 2)])[1];
    assert_ne!(on_time.flags & ADSB_FLAG_POSITION, 0);
    assert_eq!(on_time.latitude_e7, 522_572_021);

    let speed = &feed(&[&velocity_squitter()], &[(1.0, 50)])[0];
    assert_eq!(speed.velocity_kt, 500);
    assert_eq!(speed.flags, ADSB_FLAG_VELOCITY);
    assert_eq!(speed.icao, 0xABC123);

    let height = &feed(&[&gnss_squitter()], &[(1.0, 60)])[0];
    assert_eq!(height.altitude_ft, 3280);
    assert_eq!(height.flags, ADSB_FLAG_ALTITUDE);

    let mut bad = EVEN;
    bad[13] ^= 0xFF;
    let mut dropped = StructForwarder::new();
    let mut emitted = Vec::new();
    dropped.feed(
        &beast_encode(0x33, &[0; 6], 0, &bad),
        || (0.0, 0),
        &mut emitted,
    );
    assert!(emitted.is_empty());
    assert_eq!(dropped.crc_drops, 1);

    let samples = [
        Track {
            icao: 0x40621D,
            flags: 0x07,
            latitude_e7: 522_572_021,
            longitude_e7: 39_193_726,
            altitude_ft: 38000,
            velocity_kt: 450,
            timestamp_us: 1_005_000_000,
        },
        Track::empty(0x1A1A1A, 0x1A),
        Track {
            icao: 0xABCDEF,
            flags: 0x04,
            latitude_e7: ADSB_LATLON_INVALID,
            longitude_e7: ADSB_LATLON_INVALID,
            altitude_ft: ADSB_ALT_INVALID,
            velocity_kt: 500,
            timestamp_us: (1u64 << 40) + 26,
        },
        Track {
            icao: 1,
            flags: 0x02,
            latitude_e7: -338_687_000,
            longitude_e7: -706_690_000,
            altitude_ft: -1000,
            velocity_kt: ADSB_VEL_INVALID,
            timestamp_us: 0,
        },
    ];
    for msg in &samples {
        assert_packed(&pack_message(msg), msg);
    }
    let good = pack_message(&samples[0]);
    let mut flipped = good;
    flipped[31] ^= 0x5A;
    assert!(unpack_message(&flipped).is_none());
    assert_eq!(unpack_message(&good).unwrap().icao, 0x40621D);
}

/// The Beast sample fed through readsb: Mode A/C, a short squitter, the
/// published CPR pair, a 500 kt velocity, a 1000 m GNSS height, and one
/// even frame with a broken CRC. Eight cycles, all inside the 10 second window.
#[test]
fn readsb_sample_burst() {
    let df11 = with_parity(&[0x5D, 0x40, 0x62, 0x1D]);
    let mut bad = EVEN;
    bad[13] ^= 0xFF;
    let velocity = velocity_squitter();
    let gnss = gnss_squitter();
    let mut blob = Vec::new();
    for _ in 0..8 {
        blob.extend(beast_encode(0x31, &hex("010203040506"), 0x10, &hex("00AB")));
        blob.extend(beast_encode(0x32, &hex("00000000001a"), 0x1A, &df11));
        blob.extend(beast_encode(0x33, &hex("112233445566"), 0x1A, &ODD));
        blob.extend(beast_encode(0x33, &[0; 6], 0x20, &EVEN));
        blob.extend(beast_encode(0x33, &hex("0000000000aa"), 0x30, &velocity));
        blob.extend(beast_encode(0x33, &hex("0000000000bb"), 0x40, &gnss));
        blob.extend(beast_encode(0x33, &[0xff; 6], 0x00, &bad));
    }
    assert_eq!(forward_raw(&blob), blob.as_slice());

    let mut tick = 0u64;
    let mut forwarder = StructForwarder::new();
    let mut frames = Vec::new();
    forwarder.feed(
        &blob,
        || {
            let us = 1_000_000_000 + tick * 10_000;
            tick += 1;
            (us as f64 / 1_000_000.0, us)
        },
        &mut frames,
    );
    assert_eq!(forwarder.sent, 32);
    assert_eq!(forwarder.crc_drops, 8);
    assert_eq!(frames.len(), 32 * 32);

    let records: Vec<Track> = frames
        .chunks(32)
        .map(|frame| {
            let decoded = unpack_message(frame).expect("sender struct did not unpack");
            assert_packed(frame, &decoded);
            decoded
        })
        .collect();

    for (index, record) in records.iter().enumerate() {
        let cycle = index / 4;
        match index % 4 {
            0 => {
                assert_eq!(record.icao, 0x40621D);
                assert_eq!(record.altitude_ft, 38000);
                if cycle == 0 {
                    assert_eq!(record.flags, ADSB_FLAG_ALTITUDE);
                    assert_eq!(record.latitude_e7, ADSB_LATLON_INVALID);
                } else {
                    // Odd frame is newer, so CPR uses the odd grid of the same pair.
                    assert_eq!(record.flags, ADSB_FLAG_POSITION | ADSB_FLAG_ALTITUDE);
                    assert_eq!(record.latitude_e7, 522_657_802);
                    assert_eq!(record.longitude_e7, 39_389_125);
                }
            }
            1 => {
                assert_eq!(record.icao, 0x40621D);
                assert_eq!(record.flags, ADSB_FLAG_POSITION | ADSB_FLAG_ALTITUDE);
                assert_eq!(record.latitude_e7, 522_572_021);
                assert_eq!(record.longitude_e7, 39_193_726);
                assert_eq!(record.altitude_ft, 38000);
            }
            2 => {
                assert_eq!(record.icao, 0xABC123);
                assert_eq!(record.flags, ADSB_FLAG_VELOCITY);
                assert_eq!(record.velocity_kt, 500);
            }
            3 => {
                assert_eq!(record.icao, 1);
                assert_eq!(record.flags, ADSB_FLAG_ALTITUDE);
                assert_eq!(record.altitude_ft, 3280);
            }
            _ => unreachable!(),
        }
        assert_eq!(record.timestamp_us, 1_000_000_000 + index as u64 * 10_000);
    }
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

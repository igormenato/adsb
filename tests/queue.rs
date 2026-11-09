use trck::{Aircraft, Callsign, EmergencyKind, FRAME_LEN, Fix, Icao, Queue, pack_frame};

fn position(addr: u32, emergency: Option<EmergencyKind>) -> Fix {
    Fix {
        unix_s: 1_700_000_000,
        aircraft: Aircraft {
            icao: Icao::from_u24(addr).expect("icao"),
            callsign: Callsign::pad(""),
            latitude_e7: 0,
            longitude_e7: 0,
            altitude_ft: None,
            ground_speed_kt: None,
            track_deg: None,
            emergency,
        },
    }
}

fn icao_of(fix: &Fix) -> u32 {
    fix.aircraft.icao.to_u24()
}

fn emergency(addr: u32) -> Fix {
    position(addr, Some(EmergencyKind::General))
}

fn write_all(queue: &mut Queue, chunk: usize) -> Vec<u8> {
    let mut got = Vec::new();
    while queue.begin() {
        let n = {
            let rest = queue.unsent();
            let n = rest.len().min(chunk);
            got.extend_from_slice(&rest[..n]);
            n
        };
        queue.advance(n);
    }
    got
}

#[test]
fn a_short_queue_keeps_arrival_order() {
    let mut queue = Queue::new(3);
    for addr in [1, 2, 3] {
        assert!(queue.push(position(addr, None)).is_none());
    }
    let got = write_all(&mut queue, FRAME_LEN);
    let expect = [1, 2, 3]
        .into_iter()
        .flat_map(|addr| pack_frame(&position(addr, None)))
        .collect::<Vec<_>>();
    assert_eq!(got, expect);
}

#[test]
fn a_full_queue_drops_the_oldest_ordinary_position() {
    let mut queue = Queue::new(3);
    assert!(queue.push(position(1, None)).is_none());
    assert!(queue.push(emergency(2)).is_none());
    assert!(queue.push(position(3, None)).is_none());
    let dropped = queue.push(position(4, None)).expect("dropped");
    assert_eq!(icao_of(&dropped), 1);

    let got = write_all(&mut queue, FRAME_LEN);
    let expect = [emergency(2), position(3, None), position(4, None)]
        .into_iter()
        .flat_map(|fix| pack_frame(&fix))
        .collect::<Vec<_>>();
    assert_eq!(got, expect);
}

#[test]
fn an_ordinary_arrival_is_dropped_when_every_queued_position_is_an_emergency() {
    let mut queue = Queue::new(2);
    assert!(queue.push(emergency(1)).is_none());
    assert!(queue.push(emergency(2)).is_none());
    let dropped = queue.push(position(0xaaaa, None)).expect("ordinary");
    assert_eq!(icao_of(&dropped), 0xaaaa);
    let dropped = queue.push(emergency(3)).expect("oldest emergency");
    assert_eq!(icao_of(&dropped), 1);

    let got = write_all(&mut queue, FRAME_LEN);
    let expect = [emergency(2), emergency(3)]
        .into_iter()
        .flat_map(|fix| pack_frame(&fix))
        .collect::<Vec<_>>();
    assert_eq!(got, expect);
}

#[test]
fn the_in_flight_frame_is_outside_the_cap() {
    let mut queue = Queue::new(2);
    let first = position(1, None);
    assert!(queue.push(first).is_none());
    assert!(queue.begin());
    let frame = queue.unsent().to_vec();
    assert_eq!(frame, pack_frame(&first));
    assert!(queue.push(position(2, None)).is_none());
    assert!(queue.push(position(3, None)).is_none());
    let dropped = queue.push(position(4, None)).expect("dropped");
    assert_eq!(icao_of(&dropped), 2);
    assert_eq!(queue.unsent(), frame.as_slice());
    queue.advance(frame.len());
    assert!(queue.begin());
    assert_eq!(queue.unsent(), pack_frame(&position(3, None)).as_slice());
}

#[test]
fn a_short_write_resumes_the_same_frame() {
    let mut queue = Queue::new(2);
    let first = position(1, None);
    let second = position(2, None);
    assert!(queue.push(first).is_none());
    assert!(queue.push(second).is_none());
    let got = write_all(&mut queue, 5);
    let mut expect = pack_frame(&first).to_vec();
    expect.extend_from_slice(&pack_frame(&second));
    assert_eq!(got, expect);
    assert!(!queue.has_pending());
}

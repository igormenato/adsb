//! Pi sender: one readsb position at a time onto the UART.

mod frame;
mod queue;
mod snapshot;

#[doc(inline)]
pub use frame::{
    ALT_UNKNOWN, Aircraft, EmergencyKind, FRAME_LEN, Fix, SPEED_UNKNOWN, TRACK_MAGIC,
    TRACK_UNKNOWN, check_positions, crc16_ccitt_false, deg_e7, pack_frame, unpack_frame,
};
#[doc(inline)]
pub use queue::enqueue_fix;
#[doc(inline)]
pub use snapshot::fix_from_json_line;

#[cfg(kani)]
mod kani_proofs {
    use std::collections::VecDeque;

    use super::*;

    fn plane(emergency: bool) -> Fix {
        Fix {
            unix_s: 0,
            aircraft: Aircraft {
                icao: 0,
                latitude_e7: 0,
                longitude_e7: 0,
                altitude_ft: 0,
                ground_speed_kt: 0,
                track_deg: 0,
                emergency: emergency.then_some(EmergencyKind::General),
            },
        }
    }

    fn emergency_count(queue: &VecDeque<Fix>) -> usize {
        let mut n = 0usize;
        let mut i = 0usize;
        while i < queue.len() {
            if queue[i].aircraft.emergency.is_some() {
                n += 1;
            }
            i += 1;
        }
        n
    }

    #[kani::proof]
    #[kani::unwind(8)]
    fn ordinary_never_evicts_emergency() {
        const CAP: usize = 2;
        let mut queue = VecDeque::with_capacity(CAP);
        enqueue_fix(&mut queue, CAP, plane(kani::any()));
        enqueue_fix(&mut queue, CAP, plane(kani::any()));
        kani::assume(queue.len() == CAP);
        let before = emergency_count(&queue);
        enqueue_fix(&mut queue, CAP, plane(false));
        assert!(emergency_count(&queue) == before);
        assert!(queue.len() <= CAP);
    }
}

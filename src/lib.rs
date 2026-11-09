//! Pi sender: one readsb position at a time onto the UART.

mod frame;
mod snapshot;

#[doc(inline)]
pub use frame::{
    ALT_UNKNOWN, Aircraft, CALLSIGN_LEN, EmergencyKind, FRAME_LEN, Fix, SPEED_UNKNOWN, TRACK_MAGIC,
    TRACK_UNKNOWN, check_positions, crc16_ccitt_false, deg_e7, pack_frame, unpack_frame,
};
#[doc(inline)]
pub use snapshot::fix_from_json_line;

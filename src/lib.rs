//! Pi sender: one readsb position at a time onto the UART.

mod frame;
mod snapshot;

pub use frame::{
    ALT_UNKNOWN, Aircraft, EmergencyKind, FRAME_LEN, Fix, SPEED_UNKNOWN, TRACK_MAGIC,
    TRACK_UNKNOWN, crc16_ccitt_false, deg_e7, pack_frame, sample_frames, unpack_frame,
};
pub use snapshot::fix_from_json_line;

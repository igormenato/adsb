//! Pi sender: one readsb position at a time onto the UART.

mod frame;
mod snapshot;

pub use frame::{
    ALT_UNKNOWN, Aircraft, EmergencyKind, FRAME_LEN, Fix, SPEED_UNKNOWN, TRACK_MAGIC,
    TRACK_UNKNOWN, crc16_ccitt_false, deg_e7, pack_frame, unpack_frame,
};
pub use snapshot::{
    EMERGENCY_POSITION_MAX_AGE_S, POSITION_MAX_AGE_S, Snapshot, SnapshotError, TRACK_MAX_AIRCRAFT,
    fix_from_json_line, snapshot_from_aircraft_json,
};

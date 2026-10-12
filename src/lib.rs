//! Pi sender: one readsb position at a time onto the UART.

mod frame;
mod snapshot;

pub use frame::{
    crc16_ccitt_false, deg_e7, pack_frame, unpack_frame, Aircraft, EmergencyKind, Fix, Snapshot,
    ALT_UNKNOWN, FRAME_LEN, HEADING_UNKNOWN, SPEED_UNKNOWN, TRACK_MAGIC, TRACK_MAX_AIRCRAFT,
};
pub use snapshot::{
    fix_from_json_line, snapshot_from_aircraft_json, SnapshotError, EMERGENCY_POSITION_MAX_AGE_S,
    POSITION_MAX_AGE_S,
};

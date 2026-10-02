//! Pi sender: one track snapshot from readsb `aircraft.json` onto the UART.

mod frame;
mod snapshot;

pub use frame::{
    crc16_ccitt_false, deg_e7, pack_snapshot, unpack_snapshot, Aircraft, Snapshot, ALT_UNKNOWN,
    HEADING_UNKNOWN, SPEED_UNKNOWN, TRACK_HEADER_LEN, TRACK_MAGIC, TRACK_MAX_AIRCRAFT,
    TRACK_RECORD_LEN, TRACK_VERSION,
};
pub use snapshot::{snapshot_from_aircraft_json, SnapshotError};

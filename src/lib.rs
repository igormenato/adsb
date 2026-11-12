//! Pi sender: one decoded ADS-B position at a time onto the UART.

mod correct;
mod decode;
mod frame;
mod lines;
mod queue;
mod table;

#[doc(inline)]
pub use decode::{Tracker, parse_hex_frame};
#[doc(inline)]
pub use frame::{
    ALT_UNKNOWN, Aircraft, CALLSIGN_LEN, Callsign, EmergencyKind, FRAME_LEN, Fix, Icao,
    SPEED_UNKNOWN, TRACK_MAGIC, TRACK_UNKNOWN, check_positions, crc16_ccitt_false, deg_e7,
    pack_frame, unpack_frame,
};
#[doc(inline)]
pub use lines::Lines;
#[doc(inline)]
pub use queue::Queue;
#[doc(inline)]
pub use table::{DEFAULT_PATH, Table};

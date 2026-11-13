//! Pi sender: one decoded ADS-B position at a time onto the UART.

mod cmd;
mod decode;
mod frame;
mod lines;
pub mod modes;
mod queue;
mod replay;
mod table;

#[doc(inline)]
pub use cmd::{
    ACK_LEN, CmdFeed, CmdScan, CmdStats, CmdStatus, LAST_FIX_NONE, PowerOp, decide, pack_reply,
};
#[doc(inline)]
pub use decode::{Tracker, is_mode_s, parse_hex_frame};
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
pub use replay::{Replay, ReplayLine};
#[doc(inline)]
pub use table::{DEFAULT_PATH, Table};

//! Pi sender: one decoded ADS-B position at a time onto the UART.

pub mod autogain;
mod cmd;
mod cpr;
mod crc_fix;
mod decode;
pub mod demod;
mod frame;
mod lines;
pub mod mag;
pub mod modes;
mod queue;
mod replay;
mod table;

#[doc(inline)]
pub use cmd::{
    ACK_LEN, AfterReply, Cmd, CmdFeed, CmdScan, CmdStatus, Health, PowerOp, decide, pack_cmd,
    pack_reply,
};
#[doc(inline)]
pub use decode::{Tracker, is_mode_s, parse_hex_frame};
#[doc(inline)]
pub use frame::{
    Aircraft, Callsign, EmergencyKind, FRAME_LEN, Fix, Icao, check_positions, crc16_ccitt_false,
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

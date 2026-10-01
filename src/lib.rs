//! Pi sender: readsb Beast on TCP, raw bytes or one 32-byte record per squitter.

mod beast;
mod decode;
mod frame;

pub use decode::{modes_crc, modes_parity, StructForwarder};
pub use frame::{
    crc16_ccitt_false, pack_message, unpack_message, Track, ADSB_ALT_INVALID, ADSB_FLAG_ALTITUDE,
    ADSB_FLAG_POSITION, ADSB_FLAG_VELOCITY, ADSB_LATLON_INVALID, ADSB_STRUCT_CRC_LEN,
    ADSB_STRUCT_MAGIC, ADSB_STRUCT_SIZE, ADSB_STRUCT_VERSION, ADSB_VEL_INVALID, ADSB_WIRE_MAGIC,
};

/// Raw mode is the readsb bytes, unchanged, including `0x1A` escaping.
pub fn forward_raw(chunk: &[u8]) -> &[u8] {
    chunk
}

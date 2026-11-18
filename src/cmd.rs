//! Host commands on the same UART as position frames. Little-endian.

use crate::frame::crc16_ccitt_false;

pub const CMD_MAGIC: &[u8; 4] = b"TCMD";
pub const ACK_LEN: usize = 21;
pub const CMD_LEN: usize = 7;
pub const LAST_FIX_NONE: u32 = u32::MAX;

const CMD_BODY_LEN: usize = CMD_LEN - 2;
const ACK_BODY_LEN: usize = ACK_LEN - 2;

/// Byte on the command and reply frames.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Cmd {
    Ping = 0,
    Status = 1,
    Reboot = 2,
    Shutdown = 3,
    Pause = 4,
    Resume = 5,
    Restart = 6,
}

impl Cmd {
    pub fn from_u8(n: u8) -> Option<Self> {
        Some(match n {
            0 => Self::Ping,
            1 => Self::Status,
            2 => Self::Reboot,
            3 => Self::Shutdown,
            4 => Self::Pause,
            5 => Self::Resume,
            6 => Self::Restart,
            _ => return None,
        })
    }
}

/// Result byte on a reply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CmdStatus {
    Ok = 0,
    Disabled = 1,
    Unknown = 2,
    Failed = 3,
}

/// Fields filled on every reply. Cheap to copy onto PING too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CmdStats {
    pub uptime_s: u32,
    pub frames_sent: u32,
    pub last_fix_age_s: u32,
}

/// One reply the Pi writes after a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CmdReply {
    pub cmd: u8,
    pub status: CmdStatus,
    pub paused: bool,
    pub stats: CmdStats,
}

/// Host action that must not run inside tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PowerOp {
    Reboot,
    Shutdown,
}

/// Work after the reply is on the wire. Pause and resume take effect immediately.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AfterReply {
    None,
    Power(PowerOp),
    Restart,
}

pub fn unpack_cmd(bytes: &[u8]) -> Option<u8> {
    if bytes.len() != CMD_LEN || &bytes[0..4] != CMD_MAGIC {
        return None;
    }
    let expect = crc16_ccitt_false(&bytes[..CMD_BODY_LEN]);
    let got = u16::from_le_bytes(bytes[CMD_BODY_LEN..CMD_LEN].try_into().ok()?);
    (expect == got).then_some(bytes[4])
}

pub fn pack_reply(reply: &CmdReply) -> [u8; ACK_LEN] {
    let mut raw = [0u8; ACK_LEN];
    raw[0..4].copy_from_slice(b"TACK");
    raw[4] = reply.cmd;
    raw[5] = reply.status as u8;
    raw[6..10].copy_from_slice(&reply.stats.uptime_s.to_le_bytes());
    raw[10..14].copy_from_slice(&reply.stats.frames_sent.to_le_bytes());
    raw[14..18].copy_from_slice(&reply.stats.last_fix_age_s.to_le_bytes());
    raw[18] = u8::from(reply.paused);
    let crc = crc16_ccitt_false(&raw[..ACK_BODY_LEN]);
    raw[ACK_BODY_LEN..ACK_LEN].copy_from_slice(&crc.to_le_bytes());
    raw
}

/// What to write, the run state after this command, and any later host action.
pub fn decide(cmd: u8, allow_power: bool, paused: bool, stats: CmdStats) -> (CmdReply, AfterReply) {
    let (status, paused, after) = match Cmd::from_u8(cmd) {
        Some(Cmd::Ping | Cmd::Status) => (CmdStatus::Ok, paused, AfterReply::None),
        Some(Cmd::Pause) => (CmdStatus::Ok, true, AfterReply::None),
        Some(Cmd::Resume) => (CmdStatus::Ok, false, AfterReply::None),
        Some(Cmd::Restart) => (CmdStatus::Ok, false, AfterReply::Restart),
        Some(Cmd::Reboot) if allow_power => {
            (CmdStatus::Ok, paused, AfterReply::Power(PowerOp::Reboot))
        }
        Some(Cmd::Shutdown) if allow_power => {
            (CmdStatus::Ok, paused, AfterReply::Power(PowerOp::Shutdown))
        }
        Some(Cmd::Reboot | Cmd::Shutdown) => (CmdStatus::Disabled, paused, AfterReply::None),
        None => (CmdStatus::Unknown, paused, AfterReply::None),
    };
    (
        CmdReply {
            cmd,
            status,
            paused,
            stats,
        },
        after,
    )
}

const LINE_MAX: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Line {
    Cmd(u8),
    Quiet,
    Noise,
}

fn parse_line(bytes: &[u8]) -> Line {
    let bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
    match bytes {
        b"PING" => Line::Cmd(Cmd::Ping as u8),
        b"STATUS" => Line::Cmd(Cmd::Status as u8),
        b"REBOOT" => Line::Cmd(Cmd::Reboot as u8),
        b"SHUTDOWN" => Line::Cmd(Cmd::Shutdown as u8),
        b"PAUSE" => Line::Cmd(Cmd::Pause as u8),
        b"RESUME" => Line::Cmd(Cmd::Resume as u8),
        b"RESTART" => Line::Cmd(Cmd::Restart as u8),
        b"" | b"STATUS_ADSB" => Line::Quiet,
        _ => Line::Noise,
    }
}

fn line_feed(line: Line) -> CmdFeed {
    match line {
        Line::Cmd(cmd) => CmdFeed::Cmd(cmd),
        Line::Quiet => CmdFeed::None,
        Line::Noise => CmdFeed::Noise,
    }
}

#[derive(Debug, Default)]
pub struct CmdScan {
    buf: [u8; CMD_LEN],
    n: usize,
    line: [u8; LINE_MAX],
    line_n: usize,
    overflow: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CmdFeed {
    None,
    Cmd(u8),
    BadCrc,
    Noise,
}

impl CmdScan {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn feed(&mut self, byte: u8) -> CmdFeed {
        if self.overflow {
            return self.feed_line(byte);
        }
        if self.n >= CMD_LEN {
            self.drop_to_line();
        }
        self.buf[self.n] = byte;
        self.n += 1;
        let mut bad = false;
        let mut from_line = CmdFeed::None;
        loop {
            while self.n > 0 && !magic_prefix(&self.buf, self.n) {
                let feed = self.drop_to_line();
                if feed != CmdFeed::None {
                    from_line = feed;
                }
            }
            if self.n < CMD_LEN {
                return if bad { CmdFeed::BadCrc } else { from_line };
            }
            match unpack_cmd(&self.buf) {
                Some(cmd) => {
                    self.n = 0;
                    self.clear_line();
                    return CmdFeed::Cmd(cmd);
                }
                None => {
                    bad = true;
                    let feed = self.drop_to_line();
                    if feed != CmdFeed::None {
                        from_line = feed;
                    }
                }
            }
        }
    }

    fn drop_to_line(&mut self) -> CmdFeed {
        if self.n == 0 {
            return CmdFeed::None;
        }
        let byte = self.buf[0];
        self.buf.copy_within(1..self.n, 0);
        self.n -= 1;
        self.feed_line(byte)
    }

    fn feed_line(&mut self, byte: u8) -> CmdFeed {
        if self.overflow {
            if byte == b'\n' {
                self.clear_line();
                return CmdFeed::Noise;
            }
            return CmdFeed::None;
        }
        if byte == b'\n' {
            let feed = line_feed(parse_line(&self.line[..self.line_n]));
            self.clear_line();
            return feed;
        }
        if self.line_n >= LINE_MAX {
            self.overflow = true;
            return CmdFeed::None;
        }
        self.line[self.line_n] = byte;
        self.line_n += 1;
        CmdFeed::None
    }

    fn clear_line(&mut self) {
        self.line_n = 0;
        self.overflow = false;
    }
}

fn magic_prefix(buf: &[u8], n: usize) -> bool {
    let m = n.min(4);
    buf[..m] == CMD_MAGIC[..m]
}

pub fn pack_cmd(cmd: Cmd) -> [u8; CMD_LEN] {
    let mut raw = [0u8; CMD_LEN];
    raw[0..4].copy_from_slice(CMD_MAGIC);
    raw[4] = cmd as u8;
    let crc = crc16_ccitt_false(&raw[..CMD_BODY_LEN]);
    raw[CMD_BODY_LEN..CMD_LEN].copy_from_slice(&crc.to_le_bytes());
    raw
}

#[cfg(test)]
fn unpack_reply(bytes: &[u8]) -> Option<CmdReply> {
    if bytes.len() != ACK_LEN || &bytes[0..4] != b"TACK" {
        return None;
    }
    let expect = crc16_ccitt_false(&bytes[..ACK_BODY_LEN]);
    let got = u16::from_le_bytes(bytes[ACK_BODY_LEN..ACK_LEN].try_into().ok()?);
    if expect != got {
        return None;
    }
    let status = match bytes[5] {
        0 => CmdStatus::Ok,
        1 => CmdStatus::Disabled,
        2 => CmdStatus::Unknown,
        3 => CmdStatus::Failed,
        _ => return None,
    };
    Some(CmdReply {
        cmd: bytes[4],
        status,
        paused: match bytes[18] {
            0 => false,
            1 => true,
            _ => return None,
        },
        stats: CmdStats {
            uptime_s: u32::from_le_bytes(bytes[6..10].try_into().ok()?),
            frames_sent: u32::from_le_bytes(bytes[10..14].try_into().ok()?),
            last_fix_age_s: u32::from_le_bytes(bytes[14..18].try_into().ok()?),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const STATS: CmdStats = CmdStats {
        uptime_s: 12,
        frames_sent: 3,
        last_fix_age_s: 1,
    };

    #[test]
    fn cmd_round_trip() {
        for cmd in [
            Cmd::Ping,
            Cmd::Status,
            Cmd::Reboot,
            Cmd::Shutdown,
            Cmd::Pause,
            Cmd::Resume,
            Cmd::Restart,
        ] {
            let raw = pack_cmd(cmd);
            assert_eq!(unpack_cmd(&raw), Some(cmd as u8));
        }
        let reply = CmdReply {
            cmd: Cmd::Status as u8,
            status: CmdStatus::Ok,
            paused: true,
            stats: STATS,
        };
        let raw = pack_reply(&reply);
        assert_eq!(unpack_reply(&raw), Some(reply));
        let fail = CmdReply {
            status: CmdStatus::Failed,
            paused: false,
            ..reply
        };
        let raw = pack_reply(&fail);
        assert_eq!(unpack_reply(&raw), Some(fail));
    }

    #[test]
    fn bad_crc_is_dropped() {
        let mut raw = pack_cmd(Cmd::Ping);
        raw[CMD_LEN - 1] ^= 0xFF;
        assert_eq!(unpack_cmd(&raw), None);
        let mut scan = CmdScan::new();
        let mut saw_bad = false;
        for byte in raw {
            if scan.feed(byte) == CmdFeed::BadCrc {
                saw_bad = true;
            }
        }
        assert!(saw_bad);
    }

    #[test]
    fn scan_resyncs_after_garbage() {
        let ping = pack_cmd(Cmd::Ping);
        let mut scan = CmdScan::new();
        let mut got = None;
        for byte in [0x00, 0xFF].iter().copied().chain(ping) {
            if let CmdFeed::Cmd(cmd) = scan.feed(byte) {
                got = Some(cmd);
            }
        }
        assert_eq!(got, Some(Cmd::Ping as u8));
    }

    #[test]
    fn power_refused_without_flag() {
        let (reply, after) = decide(Cmd::Reboot as u8, false, false, STATS);
        assert_eq!(reply.status, CmdStatus::Disabled);
        assert_eq!(after, AfterReply::None);
        let (reply, after) = decide(Cmd::Shutdown as u8, false, true, STATS);
        assert_eq!(reply.status, CmdStatus::Disabled);
        assert!(reply.paused);
        assert_eq!(after, AfterReply::None);
    }

    #[test]
    fn power_flag_sets_op() {
        let (reply, after) = decide(Cmd::Reboot as u8, true, false, STATS);
        assert_eq!(reply.status, CmdStatus::Ok);
        assert_eq!(after, AfterReply::Power(PowerOp::Reboot));
        let (reply, after) = decide(Cmd::Shutdown as u8, true, false, STATS);
        assert_eq!(reply.status, CmdStatus::Ok);
        assert_eq!(after, AfterReply::Power(PowerOp::Shutdown));
    }

    #[test]
    fn pause_resume_restart() {
        let (reply, after) = decide(Cmd::Pause as u8, false, false, STATS);
        assert_eq!(reply.status, CmdStatus::Ok);
        assert!(reply.paused);
        assert_eq!(after, AfterReply::None);

        let (reply, after) = decide(Cmd::Pause as u8, false, true, STATS);
        assert!(reply.paused);
        assert_eq!(after, AfterReply::None);

        let (reply, after) = decide(Cmd::Resume as u8, false, true, STATS);
        assert_eq!(reply.status, CmdStatus::Ok);
        assert!(!reply.paused);
        assert_eq!(after, AfterReply::None);

        let (reply, after) = decide(Cmd::Status as u8, false, true, STATS);
        assert!(reply.paused);
        assert_eq!(after, AfterReply::None);

        let (reply, after) = decide(Cmd::Restart as u8, false, true, STATS);
        assert_eq!(reply.status, CmdStatus::Ok);
        assert!(!reply.paused);
        assert_eq!(after, AfterReply::Restart);
        let (reply, after) = decide(Cmd::Restart as u8, false, false, STATS);
        assert!(!reply.paused);
        assert_eq!(after, AfterReply::Restart);
    }

    fn feeds(scan: &mut CmdScan, bytes: &[u8]) -> Vec<CmdFeed> {
        bytes
            .iter()
            .copied()
            .map(|byte| scan.feed(byte))
            .filter(|feed| *feed != CmdFeed::None)
            .collect()
    }

    #[test]
    fn parse_line_maps_ascii_commands() {
        assert_eq!(parse_line(b"PING"), Line::Cmd(Cmd::Ping as u8));
        assert_eq!(parse_line(b"STATUS"), Line::Cmd(Cmd::Status as u8));
        assert_eq!(parse_line(b"PAUSE\r"), Line::Cmd(Cmd::Pause as u8));
        assert_eq!(parse_line(b"RESUME"), Line::Cmd(Cmd::Resume as u8));
        assert_eq!(parse_line(b"RESTART"), Line::Cmd(Cmd::Restart as u8));
        assert_eq!(parse_line(b"REBOOT"), Line::Cmd(Cmd::Reboot as u8));
        assert_eq!(parse_line(b"SHUTDOWN"), Line::Cmd(Cmd::Shutdown as u8));
        assert_eq!(parse_line(b"STATUS_ADSB"), Line::Quiet);
        assert_eq!(parse_line(b""), Line::Quiet);
        assert_eq!(parse_line(b"ping"), Line::Noise);
        assert_eq!(parse_line(b"NOPE"), Line::Noise);
    }

    #[test]
    fn ascii_lines_and_crlf() {
        let mut scan = CmdScan::new();
        assert_eq!(
            feeds(&mut scan, b"PAUSE\r\n"),
            [CmdFeed::Cmd(Cmd::Pause as u8)]
        );
        assert_eq!(feeds(&mut scan, b"STATUS_ADSB\r\n"), []);
        assert_eq!(
            feeds(&mut scan, b"RESUME\n"),
            [CmdFeed::Cmd(Cmd::Resume as u8)]
        );
    }

    #[test]
    fn ascii_split_across_feeds() {
        let mut scan = CmdScan::new();
        assert_eq!(feeds(&mut scan, b"PI"), []);
        assert_eq!(feeds(&mut scan, b"NG\n"), [CmdFeed::Cmd(Cmd::Ping as u8)]);
        assert_eq!(feeds(&mut scan, b"RESTAR"), []);
        assert_eq!(feeds(&mut scan, b"T\n"), [CmdFeed::Cmd(Cmd::Restart as u8)]);
    }

    #[test]
    fn ascii_words_with_t_byte_at_a_time() {
        const WORDS: &[(&[u8], CmdFeed)] = &[
            (b"PING", CmdFeed::Cmd(Cmd::Ping as u8)),
            (b"STATUS", CmdFeed::Cmd(Cmd::Status as u8)),
            (b"REBOOT", CmdFeed::Cmd(Cmd::Reboot as u8)),
            (b"SHUTDOWN", CmdFeed::Cmd(Cmd::Shutdown as u8)),
            (b"PAUSE", CmdFeed::Cmd(Cmd::Pause as u8)),
            (b"RESUME", CmdFeed::Cmd(Cmd::Resume as u8)),
            (b"RESTART", CmdFeed::Cmd(Cmd::Restart as u8)),
        ];
        for &(word, expect) in WORDS {
            for ending in [b"\r\n".as_slice(), b"\n".as_slice()] {
                let mut scan = CmdScan::new();
                let mut bytes = word.to_vec();
                bytes.extend_from_slice(ending);
                assert_eq!(feeds(&mut scan, &bytes), [expect], "{word:?} {ending:?}");
            }
        }
        let mut scan = CmdScan::new();
        assert_eq!(
            feeds(&mut scan, b"STATUS_ADSB\nSTATUS\n"),
            [CmdFeed::Cmd(Cmd::Status as u8)]
        );
    }

    #[test]
    fn mixed_tcmd_and_ascii() {
        let mut scan = CmdScan::new();
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&pack_cmd(Cmd::Ping));
        bytes.extend_from_slice(b"PAUSE\r\n");
        bytes.extend_from_slice(&pack_cmd(Cmd::Status));
        bytes.extend_from_slice(b"STATUS_ADSB\n");
        bytes.extend_from_slice(b"RESUME\n");
        bytes.extend_from_slice(&pack_cmd(Cmd::Pause));
        assert_eq!(
            feeds(&mut scan, &bytes),
            [
                CmdFeed::Cmd(Cmd::Ping as u8),
                CmdFeed::Cmd(Cmd::Pause as u8),
                CmdFeed::Cmd(Cmd::Status as u8),
                CmdFeed::Cmd(Cmd::Resume as u8),
                CmdFeed::Cmd(Cmd::Pause as u8),
            ]
        );
    }

    #[test]
    fn garbage_between_frames_and_lines() {
        let mut scan = CmdScan::new();
        let mut bytes = vec![0x00, 0xFF];
        bytes.extend_from_slice(&pack_cmd(Cmd::Ping));
        bytes.extend_from_slice(b"??\nPING\n");
        assert_eq!(
            feeds(&mut scan, &bytes),
            [
                CmdFeed::Cmd(Cmd::Ping as u8),
                CmdFeed::Noise,
                CmdFeed::Cmd(Cmd::Ping as u8),
            ]
        );
    }

    #[test]
    fn overlong_line_is_noise_then_recovers() {
        let mut scan = CmdScan::new();
        let mut bytes = vec![b'X'; LINE_MAX + 1];
        bytes.extend_from_slice(b"\nPING\n");
        assert_eq!(
            feeds(&mut scan, &bytes),
            [CmdFeed::Noise, CmdFeed::Cmd(Cmd::Ping as u8)]
        );
    }
}

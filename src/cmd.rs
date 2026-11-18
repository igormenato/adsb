//! Host commands on the same UART as position frames. Little-endian.

use crate::frame::crc16_ccitt_false;

pub const CMD_MAGIC: &[u8; 4] = b"TCMD";
pub const ACK_LEN: usize = 9;
pub const CMD_LEN: usize = 7;

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
    Simulation = 7,
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
            7 => Self::Simulation,
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum Health {
    Ok = 0,
    Paused = 1,
    Sim = 2,
}

impl Health {
    pub fn from_u8(n: u8) -> Option<Self> {
        Some(match n {
            0 => Self::Ok,
            1 => Self::Paused,
            2 => Self::Sim,
            _ => return None,
        })
    }
}

/// One reply the Pi writes after a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CmdReply {
    pub cmd: u8,
    pub status: CmdStatus,
    pub health: Health,
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
    raw[6] = reply.health as u8;
    let crc = crc16_ccitt_false(&raw[..ACK_BODY_LEN]);
    raw[ACK_BODY_LEN..ACK_LEN].copy_from_slice(&crc.to_le_bytes());
    raw
}

fn health(paused: bool, simulating: bool) -> Health {
    if paused {
        Health::Paused
    } else if simulating {
        Health::Sim
    } else {
        Health::Ok
    }
}

/// What to write, the run state after this command, and any later host action.
pub fn decide(
    cmd: u8,
    allow_power: bool,
    paused: bool,
    simulating: bool,
) -> (CmdReply, AfterReply) {
    let (status, paused, simulating, after) = match Cmd::from_u8(cmd) {
        Some(Cmd::Ping | Cmd::Status) => (CmdStatus::Ok, paused, simulating, AfterReply::None),
        Some(Cmd::Pause) => (CmdStatus::Ok, true, simulating, AfterReply::None),
        Some(Cmd::Resume) => (CmdStatus::Ok, false, false, AfterReply::None),
        Some(Cmd::Restart) => (CmdStatus::Ok, false, false, AfterReply::Restart),
        Some(Cmd::Simulation) => (CmdStatus::Ok, false, true, AfterReply::None),
        Some(Cmd::Reboot) if allow_power => (
            CmdStatus::Ok,
            paused,
            simulating,
            AfterReply::Power(PowerOp::Reboot),
        ),
        Some(Cmd::Shutdown) if allow_power => (
            CmdStatus::Ok,
            paused,
            simulating,
            AfterReply::Power(PowerOp::Shutdown),
        ),
        Some(Cmd::Reboot | Cmd::Shutdown) => {
            (CmdStatus::Disabled, paused, simulating, AfterReply::None)
        }
        None => (CmdStatus::Unknown, paused, simulating, AfterReply::None),
    };
    (
        CmdReply {
            cmd,
            status,
            health: health(paused, simulating),
        },
        after,
    )
}

const LINE_MAX: usize = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Line {
    Cmd(u8),
    Noise,
}

fn parse_line(bytes: &[u8]) -> Option<Line> {
    let bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
    if bytes.is_empty() {
        return None;
    }
    Some(match bytes {
        b"PING" => Line::Cmd(Cmd::Ping as u8),
        b"STATUS" | b"STATUS_ADSB" => Line::Cmd(Cmd::Status as u8),
        b"REBOOT" => Line::Cmd(Cmd::Reboot as u8),
        b"SHUTDOWN" => Line::Cmd(Cmd::Shutdown as u8),
        b"PAUSE" => Line::Cmd(Cmd::Pause as u8),
        b"RESUME" => Line::Cmd(Cmd::Resume as u8),
        b"RESTART" => Line::Cmd(Cmd::Restart as u8),
        b"SIMULATION" => Line::Cmd(Cmd::Simulation as u8),
        _ => Line::Noise,
    })
}

fn line_feed(line: Option<Line>) -> CmdFeed {
    match line {
        Some(Line::Cmd(cmd)) => CmdFeed::Cmd(cmd),
        Some(Line::Noise) => CmdFeed::Noise,
        None => CmdFeed::None,
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
    let health = Health::from_u8(bytes[6])?;
    Some(CmdReply {
        cmd: bytes[4],
        status,
        health,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

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
            Cmd::Simulation,
        ] {
            let raw = pack_cmd(cmd);
            assert_eq!(unpack_cmd(&raw), Some(cmd as u8));
        }
        let reply = CmdReply {
            cmd: Cmd::Status as u8,
            status: CmdStatus::Ok,
            health: Health::Paused,
        };
        let raw = pack_reply(&reply);
        assert_eq!(unpack_reply(&raw), Some(reply));
        let sim = CmdReply {
            health: Health::Sim,
            ..reply
        };
        let raw = pack_reply(&sim);
        assert_eq!(unpack_reply(&raw), Some(sim));
        let fail = CmdReply {
            status: CmdStatus::Failed,
            health: Health::Ok,
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
        let (reply, after) = decide(Cmd::Reboot as u8, false, false, false);
        assert_eq!(reply.status, CmdStatus::Disabled);
        assert_eq!(after, AfterReply::None);
        let (reply, after) = decide(Cmd::Shutdown as u8, false, true, false);
        assert_eq!(reply.status, CmdStatus::Disabled);
        assert_eq!(reply.health, Health::Paused);
        assert_eq!(after, AfterReply::None);
        let (reply, after) = decide(Cmd::Shutdown as u8, false, true, true);
        assert_eq!(reply.health, Health::Paused);
        assert_eq!(after, AfterReply::None);
    }

    #[test]
    fn power_flag_sets_op() {
        let (reply, after) = decide(Cmd::Reboot as u8, true, false, false);
        assert_eq!(reply.status, CmdStatus::Ok);
        assert_eq!(after, AfterReply::Power(PowerOp::Reboot));
        let (reply, after) = decide(Cmd::Shutdown as u8, true, false, true);
        assert_eq!(reply.status, CmdStatus::Ok);
        assert_eq!(reply.health, Health::Sim);
        assert_eq!(after, AfterReply::Power(PowerOp::Shutdown));
    }

    #[test]
    fn pause_resume_restart() {
        let (reply, after) = decide(Cmd::Pause as u8, false, false, false);
        assert_eq!(reply.status, CmdStatus::Ok);
        assert_eq!(reply.health, Health::Paused);
        assert_eq!(after, AfterReply::None);

        let (reply, after) = decide(Cmd::Pause as u8, false, true, false);
        assert_eq!(reply.health, Health::Paused);
        assert_eq!(after, AfterReply::None);

        let (reply, after) = decide(Cmd::Resume as u8, false, true, false);
        assert_eq!(reply.status, CmdStatus::Ok);
        assert_eq!(reply.health, Health::Ok);
        assert_eq!(after, AfterReply::None);

        let (reply, after) = decide(Cmd::Status as u8, false, true, false);
        assert_eq!(reply.health, Health::Paused);
        assert_eq!(after, AfterReply::None);

        let (reply, after) = decide(Cmd::Restart as u8, false, true, false);
        assert_eq!(reply.status, CmdStatus::Ok);
        assert_eq!(reply.health, Health::Ok);
        assert_eq!(after, AfterReply::Restart);
        let (reply, after) = decide(Cmd::Restart as u8, false, false, false);
        assert_eq!(reply.health, Health::Ok);
        assert_eq!(after, AfterReply::Restart);
    }

    #[test]
    fn simulation_health_and_resume_to_live() {
        let (reply, after) = decide(Cmd::Simulation as u8, false, false, false);
        assert_eq!(reply.status, CmdStatus::Ok);
        assert_eq!(reply.health, Health::Sim);
        assert_eq!(after, AfterReply::None);

        let (reply, after) = decide(Cmd::Simulation as u8, false, false, true);
        assert_eq!(reply.health, Health::Sim);
        assert_eq!(after, AfterReply::None);

        let (reply, after) = decide(Cmd::Simulation as u8, false, true, false);
        assert_eq!(reply.health, Health::Sim);
        assert_eq!(after, AfterReply::None);

        let (reply, after) = decide(Cmd::Pause as u8, false, false, true);
        assert_eq!(reply.health, Health::Paused);
        assert_eq!(after, AfterReply::None);

        let (reply, _) = decide(Cmd::Status as u8, false, true, true);
        assert_eq!(reply.health, Health::Paused);

        let (reply, after) = decide(Cmd::Resume as u8, false, true, true);
        assert_eq!(reply.status, CmdStatus::Ok);
        assert_eq!(reply.health, Health::Ok);
        assert_eq!(after, AfterReply::None);

        let (reply, _) = decide(Cmd::Resume as u8, false, false, true);
        assert_eq!(reply.health, Health::Ok);

        let (reply, after) = decide(Cmd::Restart as u8, false, false, true);
        assert_eq!(reply.health, Health::Ok);
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
        assert_eq!(parse_line(b"PING"), Some(Line::Cmd(Cmd::Ping as u8)));
        assert_eq!(parse_line(b"STATUS"), Some(Line::Cmd(Cmd::Status as u8)));
        assert_eq!(parse_line(b"PAUSE\r"), Some(Line::Cmd(Cmd::Pause as u8)));
        assert_eq!(parse_line(b"RESUME"), Some(Line::Cmd(Cmd::Resume as u8)));
        assert_eq!(parse_line(b"RESTART"), Some(Line::Cmd(Cmd::Restart as u8)));
        assert_eq!(
            parse_line(b"SIMULATION"),
            Some(Line::Cmd(Cmd::Simulation as u8))
        );
        assert_eq!(parse_line(b"REBOOT"), Some(Line::Cmd(Cmd::Reboot as u8)));
        assert_eq!(
            parse_line(b"SHUTDOWN"),
            Some(Line::Cmd(Cmd::Shutdown as u8))
        );
        assert_eq!(
            parse_line(b"STATUS_ADSB"),
            Some(Line::Cmd(Cmd::Status as u8))
        );
        assert_eq!(parse_line(b""), None);
        assert_eq!(parse_line(b"ping"), Some(Line::Noise));
        assert_eq!(parse_line(b"NOPE"), Some(Line::Noise));
    }

    #[test]
    fn ascii_lines_and_crlf() {
        let mut scan = CmdScan::new();
        assert_eq!(
            feeds(&mut scan, b"PAUSE\r\n"),
            [CmdFeed::Cmd(Cmd::Pause as u8)]
        );
        assert_eq!(
            feeds(&mut scan, b"STATUS_ADSB\r\n"),
            [CmdFeed::Cmd(Cmd::Status as u8)]
        );
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
            (b"SIMULATION", CmdFeed::Cmd(Cmd::Simulation as u8)),
            (b"STATUS_ADSB", CmdFeed::Cmd(Cmd::Status as u8)),
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
            [
                CmdFeed::Cmd(Cmd::Status as u8),
                CmdFeed::Cmd(Cmd::Status as u8)
            ]
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

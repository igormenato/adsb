//! Mode S Beast binary splitter.
//!
//! A frame is `0x1A`, a type byte, then a body. `0x1A` inside the body is
//! escaped as `0x1A 0x1A`. This only finds frame boundaries so struct mode
//! can hand each Mode S payload to rs1090. Raw mode never calls it: raw mode
//! copies the TCP bytes, and rs1090's Beast client would unescape them.

const REMAINDER_CAP: usize = 64 * 1024;
const BODY_SHORT: usize = 14;
const BODY_LONG: usize = 21;
const BODY_MODE_AC: usize = 9;

enum Walk {
    NeedMore,
    /// Unescaped `0x1A` inside the body. `at` is that marker.
    Interrupted {
        at: usize,
    },
    Done {
        body: [u8; BODY_LONG],
        next: usize,
    },
}

fn walk_body(buf: &[u8], start: usize, body_len: usize) -> Walk {
    let mut body = [0u8; BODY_LONG];
    let mut filled = 0;
    let mut j = start;
    while j < buf.len() && filled < body_len {
        let byte = buf[j];
        if byte == 0x1A {
            if j + 1 >= buf.len() {
                return Walk::NeedMore;
            }
            if buf[j + 1] == 0x1A {
                body[filled] = 0x1A;
                filled += 1;
                j += 2;
                continue;
            }
            return Walk::Interrupted { at: j };
        }
        body[filled] = byte;
        filled += 1;
        j += 1;
    }
    if filled < body_len {
        Walk::NeedMore
    } else {
        Walk::Done { body, next: j }
    }
}

/// Parse `buf`. `on_payload` sees unescaped Mode S payloads (7 or 14 bytes).
/// Mode A/C and status frames are skipped. Returns the index where the
/// unparsed tail starts; `0` means the whole buffer is still incomplete.
pub fn parse(buf: &[u8], mut on_payload: impl FnMut(&[u8])) -> usize {
    let mut i = 0;
    let mut last_consumed = 0;
    let mut frames = 0usize;

    while i < buf.len() {
        if buf[i] != 0x1A {
            i += 1;
            last_consumed = i;
            continue;
        }
        if i + 1 >= buf.len() {
            break;
        }
        let msg_type = buf[i + 1];

        if msg_type == 0x31 {
            match walk_body(buf, i + 2, BODY_MODE_AC) {
                Walk::NeedMore => break,
                Walk::Interrupted { at } => {
                    i = at;
                    last_consumed = at;
                }
                Walk::Done { next, .. } => {
                    i = next;
                    last_consumed = next;
                }
            }
            continue;
        }

        if msg_type == 0x34 {
            let mut j = i + 2;
            while j < buf.len() {
                if buf[j] == 0x1A {
                    if j + 1 < buf.len() && buf[j + 1] == 0x1A {
                        j += 2;
                        continue;
                    }
                    break;
                }
                j += 1;
            }
            if j >= buf.len() {
                break;
            }
            i = j;
            last_consumed = j;
            continue;
        }

        let body_len = match msg_type {
            0x32 => BODY_SHORT,
            0x33 => BODY_LONG,
            _ => {
                i += if msg_type == 0x1A { 1 } else { 2 };
                last_consumed = i;
                continue;
            }
        };

        match walk_body(buf, i + 2, body_len) {
            Walk::NeedMore => break,
            Walk::Interrupted { at } => {
                i = at;
                last_consumed = at;
            }
            Walk::Done { body, next } => {
                on_payload(&body[7..body_len]);
                frames += 1;
                i = next;
                last_consumed = next;
            }
        }
    }

    if frames == 0 && last_consumed == 0 {
        0
    } else {
        last_consumed
    }
}

/// Byte buffer for a Beast stream that arrives in TCP chunks.
pub struct Stream {
    pending: Vec<u8>,
}

impl Stream {
    pub fn new() -> Self {
        Self {
            pending: Vec::with_capacity(256),
        }
    }

    /// Append `chunk` and push each complete 14-byte Mode S payload into `out`.
    pub fn push_long(&mut self, chunk: &[u8], out: &mut Vec<[u8; 14]>) {
        out.clear();
        self.pending.extend_from_slice(chunk);
        let consumed = parse(&self.pending, |payload| {
            if payload.len() == 14 {
                let mut frame = [0u8; 14];
                frame.copy_from_slice(payload);
                out.push(frame);
            }
        });
        if consumed > 0 {
            self.pending.drain(..consumed);
        }
        if self.pending.len() > REMAINDER_CAP {
            let drop_n = self.pending.len() - REMAINDER_CAP;
            self.pending.drain(..drop_n);
        }
    }
}

impl Default for Stream {
    fn default() -> Self {
        Self::new()
    }
}

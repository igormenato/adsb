//! Reassembles one readsb socket into JSON lines.
//!
//! Append every read from one socket burst, then drain once.
//! A finished line is returned even when it is longer than the cap.
//! An unfinished line longer than the cap is dropped through the next newline.

/// Newline splitter with a cap on an unfinished line.
#[derive(Debug)]
pub struct Lines {
    pending: Vec<u8>,
    state: Phase,
    max: usize,
}

#[derive(Debug)]
enum Phase {
    Collect,
    Discard,
}

impl Lines {
    /// `max` is the longest unfinished line that stays buffered.
    pub fn new(max: usize) -> Self {
        Self {
            pending: Vec::with_capacity(max),
            state: Phase::Collect,
            max,
        }
    }

    pub fn append(&mut self, bytes: &[u8]) {
        self.pending.extend_from_slice(bytes);
    }

    /// Yield each finished line, then apply the unfinished-line cap.
    pub fn drain(&mut self, mut emit: impl FnMut(&[u8])) {
        while let Some(line) = self.pop_line() {
            emit(&line);
        }
        self.note_overflow();
    }

    fn pop_line(&mut self) -> Option<Vec<u8>> {
        loop {
            let split = self.pending.iter().position(|byte| *byte == b'\n')?;
            let mut line: Vec<u8> = self.pending.drain(..=split).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            match self.state {
                Phase::Discard => {
                    self.state = Phase::Collect;
                }
                Phase::Collect => return Some(line),
            }
        }
    }

    fn note_overflow(&mut self) {
        if self.pending.len() > self.max && !self.pending.contains(&b'\n') {
            self.pending.clear();
            self.state = Phase::Discard;
        }
    }
}

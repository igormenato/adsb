//! Holds positions until the UART accepts their frames.
//!
//! The cap counts positions that are not yet on the wire.
//! A full queue drops the oldest position that has no emergency.
//! An ordinary arrival is dropped when every queued position is an emergency.
//! An emergency arrival then drops the oldest emergency.
//! The frame already started on the wire stays outside the cap.

use std::collections::VecDeque;

use crate::frame::{FRAME_LEN, Fix, pack_frame};

/// Positions waiting on one UART, plus the frame write still in progress.
#[derive(Debug)]
pub struct Queue {
    cap: usize,
    waiting: VecDeque<Fix>,
    current: Option<Partial>,
}

#[derive(Debug)]
struct Partial {
    frame: [u8; FRAME_LEN],
    sent: usize,
}

impl Queue {
    /// `cap` is how many positions may wait behind the in-flight frame.
    pub fn new(cap: usize) -> Self {
        Self {
            cap,
            waiting: VecDeque::with_capacity(cap),
            current: None,
        }
    }

    pub fn has_pending(&self) -> bool {
        self.current.is_some() || !self.waiting.is_empty()
    }

    /// Queue `fix`. Returns the position dropped to make room.
    #[must_use]
    pub fn push(&mut self, fix: Fix) -> Option<Fix> {
        if self.waiting.len() < self.cap {
            self.waiting.push_back(fix);
            return None;
        }
        if let Some(index) = self
            .waiting
            .iter()
            .position(|queued| queued.aircraft.emergency.is_none())
        {
            let dropped = self.waiting.remove(index).expect("ordinary position");
            self.waiting.push_back(fix);
            return Some(dropped);
        }
        if fix.aircraft.emergency.is_none() {
            return Some(fix);
        }
        let dropped = self.waiting.pop_front().expect("full queue");
        self.waiting.push_back(fix);
        Some(dropped)
    }

    /// Pack the next waiting position when no frame is in flight.
    ///
    /// Returns false when nothing is waiting and nothing is in flight.
    pub fn begin(&mut self) -> bool {
        if self.current.is_some() {
            return true;
        }
        let Some(fix) = self.waiting.pop_front() else {
            return false;
        };
        self.current = Some(Partial {
            frame: pack_frame(&fix),
            sent: 0,
        });
        true
    }

    /// Bytes of the in-flight frame that the UART has not accepted.
    ///
    /// Panics when no frame is in flight.
    pub fn unsent(&self) -> &[u8] {
        let partial = self.current.as_ref().expect("current frame");
        &partial.frame[partial.sent..]
    }

    /// Record that `n` bytes from [`Self::rest`] were accepted by the UART.
    pub fn advance(&mut self, n: usize) {
        let partial = self.current.as_mut().expect("current frame");
        partial.sent += n;
        if partial.sent >= partial.frame.len() {
            self.current = None;
        }
    }
}

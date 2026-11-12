//! Recorded `ts,hex` lines for [`crate::Tracker::ingest`].

use std::io::{self, Read};

use crate::decode::parse_hex_frame;

/// One CSV line from a recording.
#[derive(Debug, PartialEq)]
pub enum ReplayLine {
    /// Blank or `#` comment.
    Skip,
    /// A Mode S frame with the recording timestamp.
    Frame { timestamp: f64, frame: Vec<u8> },
    /// Missing time or hex, bad time, or hex that is not a Mode S frame.
    Malformed,
}

/// Stream of [`ReplayLine`] values from a `ts,hex` recording.
#[derive(Debug)]
pub struct Replay<R> {
    inner: csv::Reader<R>,
    record: csv::StringRecord,
}

impl<R: Read> Replay<R> {
    pub fn new(input: R) -> Self {
        Self {
            inner: csv::ReaderBuilder::new()
                .has_headers(false)
                .flexible(true)
                .trim(csv::Trim::All)
                .comment(Some(b'#'))
                .from_reader(input),
            record: csv::StringRecord::new(),
        }
    }

    /// Next record, or `Ok(None)` at end of input. I/O errors are not frames.
    pub fn next_record(&mut self) -> io::Result<Option<ReplayLine>> {
        match self.inner.read_record(&mut self.record) {
            Ok(false) => Ok(None),
            Ok(true) => Ok(Some(from_record(&self.record))),
            Err(err) => match err.into_kind() {
                csv::ErrorKind::Io(err) => Err(err),
                _ => Ok(Some(ReplayLine::Malformed)),
            },
        }
    }
}

fn from_record(record: &csv::StringRecord) -> ReplayLine {
    let time = record.get(0).unwrap_or("");
    if record.len() < 2 {
        return if time.is_empty() || time.starts_with('#') {
            ReplayLine::Skip
        } else {
            ReplayLine::Malformed
        };
    }
    let Ok(timestamp) = time.parse::<f64>() else {
        return ReplayLine::Malformed;
    };
    if !timestamp.is_finite() {
        return ReplayLine::Malformed;
    }
    match parse_hex_frame(record.get(1).unwrap_or_default().as_bytes()) {
        Some(frame) => ReplayLine::Frame { timestamp, frame },
        None => ReplayLine::Malformed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_one(text: &str) -> ReplayLine {
        Replay::new(text.as_bytes())
            .next_record()
            .expect("replay")
            .expect("record")
    }

    #[test]
    fn skip_blank_and_comment() {
        assert!(Replay::new(b"" as &[u8]).next_record().unwrap().is_none());
        assert_eq!(parse_one("  \t\n"), ReplayLine::Skip);
        assert!(
            Replay::new("# note\n".as_bytes())
                .next_record()
                .unwrap()
                .is_none()
        );
        assert_eq!(parse_one("  # still a comment\n"), ReplayLine::Skip);
    }

    #[test]
    fn frame_splits_time_and_hex() {
        match parse_one("1.5, 8D40058B58C901375147EFD09357") {
            ReplayLine::Frame { timestamp, frame } => {
                assert_eq!(timestamp, 1.5);
                assert_eq!(frame.len(), 14);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn pymodes_quoted_hex_and_extra_columns() {
        match parse_one(r#"1457996400,"8D406B909945DE10000405999BE4","406B90",19"#) {
            ReplayLine::Frame { timestamp, frame } => {
                assert_eq!(timestamp, 1_457_996_400.0);
                assert_eq!(frame.len(), 14);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn malformed_without_aborting() {
        assert_eq!(parse_one("no-comma"), ReplayLine::Malformed);
        assert_eq!(
            parse_one("x,8D40058B58C901375147EFD09357"),
            ReplayLine::Malformed
        );
        assert_eq!(parse_one("1,zz"), ReplayLine::Malformed);
        assert_eq!(parse_one("1,8D40058B"), ReplayLine::Malformed);
        assert_eq!(
            parse_one("inf,8D40058B58C901375147EFD09357"),
            ReplayLine::Malformed
        );
    }

    #[test]
    fn all_ff_hex_is_not_mode_s() {
        match parse_one("1,FFFFFFFFFFFFFFFFFFFFFFFFFFFF") {
            ReplayLine::Frame { frame, .. } => {
                assert_eq!(frame.len(), 14);
                assert!(!crate::is_mode_s(&frame));
            }
            other => panic!("{other:?}"),
        }
    }
}

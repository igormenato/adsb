//! Recorded `ts,hex` lines for [`crate::Tracker::ingest`].

use crate::decode::parse_hex_frame;

/// One CSV line from a recording.
#[derive(Debug, PartialEq)]
pub enum ReplayLine {
    /// Blank or `#` comment.
    Skip,
    /// A Mode S frame with the recording timestamp.
    Frame { timestamp: f64, frame: Vec<u8> },
    /// Missing comma, bad time, or hex that is not a Mode S frame.
    Malformed,
}

/// Parse one `ts,hex` line. Blank and comment lines are [`ReplayLine::Skip`].
pub fn parse_replay_line(line: &str) -> ReplayLine {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return ReplayLine::Skip;
    }
    let Some((time, hex)) = line.split_once(',') else {
        return ReplayLine::Malformed;
    };
    let Ok(timestamp) = time.trim().parse::<f64>() else {
        return ReplayLine::Malformed;
    };
    if !timestamp.is_finite() {
        return ReplayLine::Malformed;
    }
    let Some(frame) = parse_hex_frame(hex.trim().as_bytes()) else {
        return ReplayLine::Malformed;
    };
    ReplayLine::Frame { timestamp, frame }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_blank_and_comment() {
        assert_eq!(parse_replay_line(""), ReplayLine::Skip);
        assert_eq!(parse_replay_line("  \t"), ReplayLine::Skip);
        assert_eq!(parse_replay_line("# note"), ReplayLine::Skip);
        assert_eq!(parse_replay_line("  # still a comment"), ReplayLine::Skip);
    }

    #[test]
    fn frame_splits_time_and_hex() {
        let line = "1.5, 8D40058B58C901375147EFD09357";
        match parse_replay_line(line) {
            ReplayLine::Frame { timestamp, frame } => {
                assert_eq!(timestamp, 1.5);
                assert_eq!(frame.len(), 14);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn malformed_without_aborting() {
        assert_eq!(parse_replay_line("no-comma"), ReplayLine::Malformed);
        assert_eq!(
            parse_replay_line("x,8D40058B58C901375147EFD09357"),
            ReplayLine::Malformed
        );
        assert_eq!(parse_replay_line("1,zz"), ReplayLine::Malformed);
        assert_eq!(parse_replay_line("1,8D40058B"), ReplayLine::Malformed);
        assert_eq!(
            parse_replay_line("inf,8D40058B58C901375147EFD09357"),
            ReplayLine::Malformed
        );
    }

    #[test]
    fn all_ff_hex_is_not_mode_s() {
        match parse_replay_line("1,FFFFFFFFFFFFFFFFFFFFFFFFFFFF") {
            ReplayLine::Frame { frame, .. } => {
                assert_eq!(frame.len(), 14);
                assert!(!crate::is_mode_s(&frame));
            }
            other => panic!("{other:?}"),
        }
    }
}

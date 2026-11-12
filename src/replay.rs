use crate::decode::parse_hex_frame;

#[derive(Debug, PartialEq)]
pub enum ReplayLine {
    Skip,
    Frame { timestamp: f64, frame: Vec<u8> },
    Malformed,
}

/// Parse one `ts,hex` line. Blank and comment lines are [`ReplayLine::Skip`].
/// Extra columns after hex are ignored. Quotes around a field are stripped.
pub fn parse_replay_line(line: &str) -> ReplayLine {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return ReplayLine::Skip;
    }
    let mut fields = line.split(',');
    let Some(time) = fields.next() else {
        return ReplayLine::Malformed;
    };
    let Some(hex) = fields.next() else {
        return ReplayLine::Malformed;
    };
    let Ok(timestamp) = csv_field(time).parse::<f64>() else {
        return ReplayLine::Malformed;
    };
    if !timestamp.is_finite() {
        return ReplayLine::Malformed;
    }
    let Some(frame) = parse_hex_frame(csv_field(hex).as_bytes()) else {
        return ReplayLine::Malformed;
    };
    ReplayLine::Frame { timestamp, frame }
}

fn csv_field(text: &str) -> &str {
    let text = text.trim();
    text.strip_prefix('"')
        .and_then(|text| text.strip_suffix('"'))
        .unwrap_or(text)
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
    fn pymodes_quoted_hex_and_extra_columns() {
        let line = r#"1457996400,"8D406B909945DE10000405999BE4","406B90",19"#;
        match parse_replay_line(line) {
            ReplayLine::Frame { timestamp, frame } => {
                assert_eq!(timestamp, 1_457_996_400.0);
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

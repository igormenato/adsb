use trck::Lines;

fn take(buf: &mut Lines, bytes: &[u8]) -> Vec<Vec<u8>> {
    buf.append(bytes);
    let mut lines = Vec::new();
    buf.drain(|line| lines.push(line.to_vec()));
    lines
}

#[test]
fn a_split_line_and_a_crlf_become_one_line() {
    let mut buf = Lines::new(64);
    assert!(take(&mut buf, b"he").is_empty());
    assert_eq!(take(&mut buf, b"llo\r\n"), vec![b"hello".to_vec()]);
}

#[test]
fn one_read_can_hold_two_lines() {
    let mut buf = Lines::new(64);
    assert_eq!(
        take(&mut buf, b"a\nb\n"),
        vec![b"a".to_vec(), b"b".to_vec()]
    );
}

#[test]
fn reads_in_one_burst_join_before_the_cap() {
    let mut buf = Lines::new(4);
    buf.append(b"abc");
    buf.append(b"d\nnext\n");
    let mut lines = Vec::new();
    buf.drain(|line| lines.push(line.to_vec()));
    assert_eq!(lines, vec![b"abcd".to_vec(), b"next".to_vec()]);
}

#[test]
fn a_finished_line_may_exceed_the_cap() {
    let mut buf = Lines::new(4);
    assert_eq!(take(&mut buf, b"abcdef\n"), vec![b"abcdef".to_vec()]);
}

#[test]
fn an_unfinished_line_at_the_cap_is_kept() {
    let mut buf = Lines::new(4);
    assert!(take(&mut buf, b"abcd").is_empty());
    assert_eq!(take(&mut buf, b"\n"), vec![b"abcd".to_vec()]);
}

#[test]
fn an_unfinished_line_past_the_cap_is_dropped_through_the_newline() {
    let mut buf = Lines::new(4);
    assert!(take(&mut buf, b"abcde").is_empty());
    assert_eq!(take(&mut buf, b"zz\nok\n"), vec![b"ok".to_vec()]);
}

#[test]
fn two_appends_past_the_cap_drop_before_drain() {
    let mut buf = Lines::new(4);
    buf.append(b"abc");
    buf.append(b"de");
    let mut lines = Vec::new();
    buf.drain(|line| lines.push(line.to_vec()));
    assert!(lines.is_empty());
    assert_eq!(take(&mut buf, b"nope\ngood\n"), vec![b"good".to_vec()]);
}

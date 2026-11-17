use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;

use rs_rtl::deliver_samples;

#[test]
fn a_full_channel_increments_dropped() {
    let (tx, rx) = mpsc::sync_channel(1);
    let dropped = AtomicU64::new(0);
    assert!(deliver_samples(&tx, &dropped, vec![1]));
    assert!(deliver_samples(&tx, &dropped, vec![2]));
    assert_eq!(dropped.load(Ordering::Relaxed), 1);
    assert_eq!(rx.recv().unwrap().unwrap(), vec![1]);
    drop(rx);
    assert!(!deliver_samples(&tx, &dropped, vec![3]));
    assert_eq!(dropped.load(Ordering::Relaxed), 1);
}

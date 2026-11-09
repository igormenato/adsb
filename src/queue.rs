//! Send-queue drop policy. `Writer::enqueue` in `src/main.rs` calls this.

use std::collections::VecDeque;

use crate::frame::Fix;

/// Put `fix` on the UART send queue. `cap` is `QUEUE_CAP` in `src/main.rs` (64).
/// Returns the `Fix` that was dropped, if any.
pub fn enqueue_fix(queue: &mut VecDeque<Fix>, cap: usize, fix: Fix) -> Option<Fix> {
    if queue.len() < cap {
        queue.push_back(fix);
        return None;
    }
    if let Some(index) = queue
        .iter()
        .position(|queued| queued.aircraft.emergency.is_none())
    {
        let dropped = queue.remove(index).expect("ordinary position");
        queue.push_back(fix);
        return Some(dropped);
    }
    if fix.aircraft.emergency.is_none() {
        return Some(fix);
    }
    let dropped = queue.pop_front().expect("full queue");
    queue.push_back(fix);
    Some(dropped)
}

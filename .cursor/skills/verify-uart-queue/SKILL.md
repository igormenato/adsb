---
name: verify-uart-queue
description: >-
  Model-check Writer::enqueue on the UART send queue. An ordinary plane
  must never kick an emergency out of the queue, and enqueue must not
  touch the in-flight frame in Writer.current. Use when changing
  src/main.rs Writer::enqueue, Writer::write_some, QUEUE_CAP, or UART
  backpressure and drop behavior.
---

# Verify the UART send queue

A 7700 that is already queued must not be thrown away to make room for a normal plane. A frame that `write_some` has already popped into `Writer.current` is on the UART and is not a drop candidate.

If that breaks, the satellite never sees the alert.

## Does this change need the check?

Run it when the diff touches any of:

- `src/main.rs` `Writer::enqueue`
- `src/main.rs` `Writer::write_some` (the pop into `current`, or clearing `current`)
- `src/main.rs` `QUEUE_CAP`
- `src/main.rs` `Writer.queue` or `Writer.current`
- `verify/uart-queue/WriterQueue.tla` or `WriterQueue.cfg`

Skip it for JSON parse, `pack_frame`, CRC, CLI flags, and `sys.rs`.

If the covered functions changed after the sync commit below, do not trust a prior pass. Follow `/maintain-formal-verification-skill` or update and rerun here.

## Run

From the repo root:

```sh
verify/uart-queue/check.sh
```

Exit 0 means TLC found no invariant break in the model, under the bounds below. Any other exit is a fail. `check.sh` fetches `tla2tools.jar` 1.7.4 (sha1 `bee4a54f3ee3d4afc347c3240ec2d9e93b075104`) into `verify/uart-queue/.tools/` on first run. Java 21 is enough.

Read every TLC result with `/statechart`. It turns an invariant break into the shortest path in plain words. A pass gets a chart of how the queue moves. Do not copy `/statechart` commands from memory. Open that skill.

## Property

`OrdinaryNeverEvictsEmergency`. `Writer::enqueue` of an ordinary `Fix` never lowers the number of queued emergencies.

`EnqueueLeavesCurrent`. `Writer::enqueue` does not change `Writer.current`.

`QueueFits`. The queue length is at most `QueueCap`.

`TypeOK`. Queue entries are ordinary or emergency. `current` is one of those or `"none"`.

## Bounds and assumptions

- `QueueCap` is 3 in `WriterQueue.cfg`. `QUEUE_CAP` in `src/main.rs` is 64. The check is the drop policy, not the number 64. Every order of enqueue ordinary, enqueue emergency, start write, and finish write is explored at cap 3 (45 distinct states on the last pass).
- Each queued `Fix` is only its emergency bit. ICAO, callsign, and kinematics are out.
- `Writer.current`'s `sent` offset is out. Occupied vs `"none"` is in. A WouldBlock is `FinishWrite` not firing yet.
- One thread. `Enqueue` and `write_some` interleave the way `pump` can enqueue lines then write, or write then enqueue.
- `pack_frame` is assumed to succeed. The model pops the `Fix` and stores its kind.

A pass is a pass of this model at cap 3. It is not a proof of the binary, of cap 64, or of reconnect.

## What the model maps to

| Model | Code |
| --- | --- |
| `queue` | `Writer.queue` (`VecDeque<Fix>`) |
| `"ordinary"` / `"emergency"` | `aircraft.emergency.is_none()` / `is_some()` |
| `current` | `Writer.current` empty vs occupied |
| `Enqueue` LET `q` | `Writer::enqueue` body |
| `Len(queue) < QueueCap` then `Append` | `queue.len() < QUEUE_CAP` then `push_back` |
| `HasOrdinary` then `RemoveFirstOrdinary` then `Append` | `position(emergency.is_none())` then `remove` then `push_back` |
| `kind = "ordinary"` then `q = queue` | `fix.aircraft.emergency.is_none()` then `return` |
| `Append(Tail(queue), kind)` | `pop_front` then `push_back` (all queued are emergencies, new is emergency) |
| `StartWrite` | `write_some` pops `queue` into `current` when `current` is `None` |
| `FinishWrite` | `write_some` sets `current = None` when `sent >= FRAME_LEN` |

The LET `q` is the policy. The primed `evictedEmergencyForOrdinary` and `currentMovedByEnqueue` assignments are the properties. Do not fold the properties into the IF. If you change `a' = b \/ c`, keep the parentheses. Without them TLC parses `(a' = b) \/ c` and a real break becomes "variable not assigned".

## Bug vs modeling error

Replay the TLC path against `Writer::enqueue` and `write_some`.

- Each `EnqueueOrdinary` / `EnqueueEmergency` must match one of the four branches above.
- Each `StartWrite` needs `current.is_none()` and a non-empty queue.
- Each `FinishWrite` needs `current.is_some()`.

If a step cannot happen in that code, the spec is wrong. Fix `WriterQueue.tla`. Do not weaken an invariant to hide it.

If every step can happen, it is a bug in `enqueue` or `write_some`. Report the path in those names. Do not change product code unless asked.

A path we have seen fail on purpose, when `enqueue` always `pop_front`s on a full queue:

1. Enqueue a 7700.
2. Enqueue two ordinary planes. The queue is full.
3. Enqueue a third ordinary plane. Drop-oldest removes the 7700.

The real `enqueue` would have removed an ordinary plane instead.

## Intended change

If the drop policy is meant to change, rewrite LET `q` to match the new `enqueue`. Change `OrdinaryNeverEvictsEmergency` or `EnqueueLeavesCurrent` only with evidence in the tests or the commit message. Keep `QueueCap` at 3 unless the new policy depends on the numeric cap.

Move **sync commit** forward only after `check.sh` exits 0 and you have confirmed LET `q` still matches `Writer::enqueue`.

## Coverage

- File: `src/main.rs`
- Functions: `Writer::enqueue`, `Writer::write_some`
- Constant: `QUEUE_CAP`
- Spec: `verify/uart-queue/WriterQueue.tla`, `WriterQueue.cfg`
- Runner: `verify/uart-queue/check.sh`
- Sync commit: see the last line of this file. Update that line, not a copy elsewhere.

## Out of scope

JSON lines, `pack_frame`, CRC, `sys.rs`, Ctrl-C, dial, keepalive. `end_socket` / reconnect keeping the same `Writer` is not an action here. If reconnect starts clearing the queue, this check will not see it.

## Sync commit

`SYNC_COMMIT` not yet recorded. Set it to the commit that added this skill after `check.sh` passes on that commit.

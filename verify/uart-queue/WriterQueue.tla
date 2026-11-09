---- MODULE WriterQueue ----
\* Model of src/main.rs Writer::enqueue and the current-frame
\* handoff in Writer::write_some.
\*
\* queue is Writer.queue: VecDeque<Fix>, each entry "ordinary" or
\* "emergency" from aircraft.emergency.is_none() / is_some().
\* current is Writer.current: "none" when None, otherwise the kind
\* of the Fix that write_some just popped and packed. The sent
\* offset is omitted. Drop never reads it.
\* QUEUE_CAP in src/main.rs is 64. This model uses QueueCap from
\* the config (3). The number is not the property. The drop
\* policy is.

EXTENDS Naturals, Sequences

CONSTANT QueueCap

VARIABLES queue, current, evictedEmergencyForOrdinary, currentMovedByEnqueue

vars == <<queue, current, evictedEmergencyForOrdinary, currentMovedByEnqueue>>

Kinds == {"ordinary", "emergency"}

EmergCount(q) == Len(SelectSeq(q, LAMBDA x : x = "emergency"))

HasOrdinary(q) == \E i \in 1..Len(q) : q[i] = "ordinary"

RemoveFirstOrdinary(q) ==
  LET i == CHOOSE i \in 1..Len(q) :
              /\ q[i] = "ordinary"
              /\ \A j \in 1..(i - 1) : q[j] # "ordinary"
  IN SubSeq(q, 1, i - 1) \o SubSeq(q, i + 1, Len(q))

TypeOK ==
  /\ queue \in Seq(Kinds)
  /\ current \in (Kinds \union {"none"})
  /\ evictedEmergencyForOrdinary \in BOOLEAN
  /\ currentMovedByEnqueue \in BOOLEAN

QueueFits == Len(queue) <= QueueCap

\* Harm detector: an ordinary Enqueue removed an emergency from
\* the queue. Writer::enqueue must not do that.
OrdinaryNeverEvictsEmergency == evictedEmergencyForOrdinary = FALSE

\* Harm detector: Enqueue changed Writer.current. The in-flight
\* frame is outside the drop policy.
EnqueueLeavesCurrent == currentMovedByEnqueue = FALSE

Init ==
  /\ queue = << >>
  /\ current = "none"
  /\ evictedEmergencyForOrdinary = FALSE
  /\ currentMovedByEnqueue = FALSE

\* Writer::enqueue. kind is the new Fix's emergency.
\* q is the VecDeque after this call. The two primed ghosts are
\* the properties, not the policy. Keep them outside the LET.
Enqueue(kind) ==
  LET q ==
        IF Len(queue) < QueueCap THEN
          Append(queue, kind)
        ELSE IF HasOrdinary(queue) THEN
          Append(RemoveFirstOrdinary(queue), kind)
        ELSE IF kind = "ordinary" THEN
          queue
        ELSE
          Append(Tail(queue), kind)
  IN
    /\ queue' = q
    /\ current' = current
    /\ currentMovedByEnqueue' = (currentMovedByEnqueue \/ (current' # current))
    /\ evictedEmergencyForOrdinary' =
          (evictedEmergencyForOrdinary
           \/ (kind = "ordinary" /\ EmergCount(q) < EmergCount(queue)))

EnqueueOrdinary == Enqueue("ordinary")
EnqueueEmergency == Enqueue("emergency")

\* Writer::write_some, the pop into current when current is None.
StartWrite ==
  /\ current = "none"
  /\ Len(queue) > 0
  /\ current' = Head(queue)
  /\ queue' = Tail(queue)
  /\ UNCHANGED <<evictedEmergencyForOrdinary, currentMovedByEnqueue>>

\* Writer::write_some, a completed write (sent reaches FRAME_LEN).
\* A WouldBlock that leaves current Some is just this step not
\* firing yet. Enqueue can run while current stays set.
FinishWrite ==
  /\ current # "none"
  /\ current' = "none"
  /\ UNCHANGED <<queue, evictedEmergencyForOrdinary, currentMovedByEnqueue>>

Next ==
  \/ EnqueueOrdinary
  \/ EnqueueEmergency
  \/ StartWrite
  \/ FinishWrite

Spec == Init /\ [][Next]_vars

====

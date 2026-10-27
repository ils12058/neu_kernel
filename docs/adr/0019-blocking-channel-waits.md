# ADR 0019: Blocking Channel waits, suspension and wakeup

- **Status:** Accepted, scoped to the single-CPU, IF=0 kernel-transaction model and
  experimental ABI 0.4.
- **Date:** 2026-10-27.
- **Related:** NEU-005, NEU-006, NEU-007, NEU-008, NEU-009, NEU-010, NEU-011,
  NEU-013, NEU-014, NEU-021, NEU-023, NEU-024; [ADR 0017](0017-bounded-scalar-channel-endpoints.md);
  [ADR 0018](0018-channel-conservation-and-retirement.md).
- **Architecture:** [v0.2](../Neu_Kernel_Architecture_v0.2.md) remains unchanged.
  This record implements a scoped part of the K6 communication direction; it does
  not ratify unrelated register proposals.

## Context

ADR 0017/0018 established two typed endpoint capabilities, two independent
two-slot FIFOs, immediate Full/Empty results and conservation/retirement
invariants. A user program that must wait for a peer therefore has to poll.
Polling is not scheduler blocking: it consumes runnable time, obscures FIFO
pressure, and cannot prove that a Thread was actually suspended without losing a
notification.

A real wait introduces several hazards absent from the nonblocking design:

- the active execution lease borrows the capability registry that owns Channel
  state;
- a wait must outlive the syscall frame without retaining a Rust borrow or a
  controller-stack pointer;
- a send or receive that observes Empty/Full must register before another
  transaction can change that condition;
- a woken Thread is not guaranteed to run immediately, so it must recheck;
- endpoint close, handle close and Node termination must find and release every
  pending operation exactly once;
- an all-blocked topology has no user code that can create a producer.

The design must preserve ABI 0.1/0.2/0.3 exactly and must not add public
timeouts, capability transfer, derivation, revocation, priorities or SMP.

## Decision

### Thread state and scheduler

`ThreadState` gains `Blocked`. A blocked Thread is not in the runnable FIFO:
`Queue::next` can never return it, and `Queue::wake` is the only path that marks
it `Ready` and appends it exactly once. `Queue::block` and `Queue::reblock`
require a `SavedContext` with `continuation == true`, a live ActiveContext,
validated stack and an Active Node; they clear the active selection only after
the Thread record and saved frame are committed.

The Node remains `Active` while any member is blocked. A terminal no-runnable
condition marks the Node `Quiesced`, distinct from normal exit and from a fault.
`ExitReason::Quiesced` is never fabricated as a successful syscall result.

### Owned wait registry

`kernel/src/wait.rs` owns a fixed-capacity registry of three waits, equal to the
supported three-Thread topology. Each Thread owns at most one registration and
may not register while another registration exists. A `Waiter` is a plain
copyable record containing:

- the exact `ActiveContext` (Node/Thread/Space generation and root);
- endpoint, Channel and object generations;
- endpoint side, owning Node, operation kind and pending payload;
- a monotonically allocated `sequence` for FIFO order;
- a monotonic non-wrapping `token` for individual removal/settle;
- state `Waiting`, `Ready` or `Queued`, an optional terminal error, and whether
  its operation pin is still held.

No pointer, borrow, `Guard<'_>` or `&mut Objects` is stored. The registry lives
inside `capability::Objects`, so condition checks, registration, wake decisions
and terminalization share one exclusive IF=0 transaction with the Channel state.

### Atomic check, registration and critical boundary

The blocking syscall performs, while the active execution lease and the
exclusive registry transaction are held:

1. validate Node/table/handle identity, object type/rights and endpoint/Channel
   generations through the existing lookup;
2. attempt the real nonblocking send/receive;
3. on success, return the committed result and wake the appropriate peer waiter;
4. on Empty/Full, preflight wait capacity and duplicate registration, take one
   extra object pin, register the `Waiter`, and return `Outcome::Blocked`;
5. end the borrows and return the lease before the controller schedules.

The Empty/Full observation and registration are one transaction, so no enqueue,
dequeue, close or wake can interleave between them. Registration failure rolls
back the pin and never suspends.

### Linearization points

The design has explicit commit boundaries:

- **Immediate Channel operation:** the queue mutation and counter update inside
  `Channels::send`/`Channels::receive`, inside the registry transaction.
- **Wake notification:** `Waits::wake` marking the oldest eligible `Waiting`
  registration `Ready`, in the same transaction as the successful queue mutation.
- **Blocking registration:** publication of the `Waiter` in `Waits::register`,
  after the failed Empty/Full attempt and pin, in the same transaction.
- **Scheduler admission:** `take_ready` moving `Ready` to `Queued`, followed by
  `Queue::wake` changing the Thread from `Blocked` to `Ready` and enqueueing it.
- **Completion:** `settle_wait` removing the queued registration and performing
  the actual Channel operation, in the same transaction as the recheck. A
  Full/Empty retry restores the same token, sequence and pin before that
  transaction ends.
- **Endpoint terminalization:** `release_id` observing zero references, then
  `terminate_endpoint` marking same-endpoint waiters `PeerClosed`, releasing
  their pins and marking other Channel waiters `Ready`.
- **Node cancellation:** `cancel_node_waits` removing all waits of the faulted
  Node, before controller completion, handle drain and root reclamation.

### Lost-wakeup prevention and wake policy

Normal message availability uses **wake-one**: a successful send marks the
oldest `Waiting` receive registration for that inbound side; a successful
receive marks the oldest `Waiting` send registration for the newly available
capacity. FIFO order is the registration `sequence`, not Thread id or stack.

Endpoint retirement uses **wake-all for affected waiters**: same-endpoint
registrations become terminal `PeerClosed`; other registrations on the same
Channel are marked `Ready` so they can drain committed messages or observe
closure. No wake reserves a message or capacity.

A wake grants only scheduler eligibility. Between admission and execution,
another Thread can consume the condition. The pre-entry `settle_wait` therefore
rechecks under the registry transaction:

- if the operation can commit, it commits once, releases the pin and returns;
- if it is still Empty/Full, `restore`/`retry` returns the same registration to
  `Waiting` without a second pin or a lost notification;
- if the endpoint became terminal, it returns the stored `PeerClosed` and the
  pin was already released at terminalization.

A registration in `Ready`/`Queued` is skipped by ordinary wakes. Because restore
happens inside the same exclusive transaction as the failed recheck, a later
producer transaction either sees `Waiting` and wakes it, or sees the waiter still
queued and the settle path observes the new condition. The single-CPU IF=0
transaction is the proof boundary; this is not an SMP claim.

Duplicate admission is prevented by Thread state (`wake` requires `Blocked`),
the scheduler's `contains` check, registry state transitions, and the
monotonic token. An Exited/Reclaimed Thread cannot be woken; stale Thread,
endpoint, Channel and object generations are rejected by the existing typed
identities before any queue access.

### Blocking syscall continuation and resume

ABI 0.4 adds `0x15 channel_send_wait` and `0x16 channel_receive_wait`. The native
entry saves the actual syscall-return frame. On suspension the controller stores
`SavedContext::continuation`, which:

- requires `RCX == RIP` and `R11 == flags` from the SYSCALL instruction;
- requires valid CS/SS/RSP/canonical range and a preemptible user flag set;
- forbids `fresh` or hardware-`interrupted` combinations;
- permits arbitrary RAX/RDX because they are the operation result, unlike a
  yield frame that must have zero results.

The kernel never fabricates a timer-interrupted context. On wake, the pre-entry
settle writes the ABI status into RAX and scalar into RDX, then the normal
trusted IRETQ path restores the user RSP, preserved GPRs and post-SYSCALL RIP.
RCX/R11 remain SYSCALL clobbers. A terminal wait resumes once with a negative
status; a Node-cancelled Thread never resumes.

### Pending-operation lifetime, pins and close

A successfully registered wait owns one extra endpoint/object pin in addition to
the usual lookup guard pin, which ends with the syscall. The pin keeps the
object generation live even if a sibling closes the original handle. Settlement,
terminalization or cancellation releases that pin exactly once; the wait record
retains `pinned=false` if the terminal path already did so.

Closing one handle is not endpoint retirement. Final reference closure invokes
terminalization even if a wait pin remains; the Channel endpoint itself retires
only when both references and pins reach zero. Thus no waiter points at a
recycled endpoint, and no retired endpoint is delayed forever by a forgotten
pin. Authorization to complete an in-flight operation comes from the already
validated registration, not from re-looking up a possibly closed handle.

### Node fault and no-runnable behavior

On a user fault, the controller first returns the active execution lease, then
cancels every wait owned by that Node (releasing pins and removing registry
entries), then completes/cancels the Node's Threads, drains handles and only then
reclaims stacks, contexts, tables and the address-space root. Healthy peer Nodes
and their Channel authority are untouched. The saved context of a cancelled
Thread is cleared and never resumed.

If the runnable FIFO empties while any test member lacks an outcome, the
supported topology has no external producer: all remaining peers are either
blocked or exited. The controller performs a bounded classification instead of
spinning or waiting for the QEMU deadline: cancel waits, mark blocked Threads
and Nodes `Quiesced`, retire stacks, verify zero pins, drain handles and roots,
and print the classification. This is a deliberate deadlock/quiescence result,
not a successful message transfer and not a timeout pass. No idle-HLT protocol
or fabricated wake is introduced.

### ABI, capacity and conservation

ABI 0.4 reports features `0x7f` and `(0,4,0x7f)` = `0x000000040000007f`; bit 6
denotes the two blocking operations. Older images retain their exact versions,
masks and `-1` rejection of `0x15`/`0x16`. `0x13`/`0x14` remain immediate and
never block any image.

FIFO capacity remains two. ADR 0018 conservation is unchanged: a blocked send
counts as sent only when `settle_wait` actually enqueues; a blocked receive
counts as received only when it actually dequeues. Registration, pinning,
readiness, retry and cancellation do not touch message counters. Full/Empty
retries do not mutate queued values.

Indefinite waits have no public timeout argument and no internal deadline. The
representation is "no deadline"; a future deadline extension must add explicit
state rather than infer one from the LAPIC clockevent. No timeout is treated as
success.

## Alternatives considered

- **Userspace polling:** retains the lost-wakeup and fairness problem and does
  not provide a Blocked scheduler state; rejected by the milestone's objective.
- **Reserve a queue slot at wake:** would convert notification into ownership and
  complicate capacity/conservation; rejected in favor of rechecking at settle.
- **Store `&mut Objects`, a `Guard`, or a controller-stack pointer in the wait:**
  cannot survive the lease return or a context switch without aliasing; rejected.
- **Context-switch from the native handler with live Rust borrows:** violates the
  existing trusted handoff and lease rules; rejected.
- **Awaken every waiter on every message:** allows thundering-herd rechecks and
  breaks deterministic FIFO wake-one; reserved for terminal peer closure.
- **Treat an empty runnable queue as completion or wait for QEMU timeout:**
  misreports deadlock as success; rejected.
- **Add public timeouts/deadlines now:** would invent guarantees the LAPIC
  clockevent does not provide and expand the ABI beyond the phase; deferred.

## Consequences and future work

Blocking receive/send now suspend and resume real Ring 3 Threads, preserve
authority and pins, and remove polling from the supported path. Close and Node
fault paths have explicit cancellation and exactly-once pin release. ABI 0.3 and
all earlier images remain behaviorally unchanged. The cost is a fixed three-entry
wait registry, one extra pin per blocked operation, and a recheck on every wake.

Deferred: public timeouts, deadlines, priority donation/inheritance, wait sets,
capability transport/moves, derivation, revocation, public Channel/Node/Thread
creation, shared memory, events, SMP, kernel preemption, and POSIX compatibility.
Physical hardware and host CI remain unverified. The implementation status
document records the executed host/QEMU/QMP evidence and any remaining limits.

## History

- **2026-10-27:** Accepted for K6 / Phase 6B. Extends ADR 0017/0018 with a
  single-CPU blocking Channel wait contract; architecture v0.2 unchanged.

# ADR 0021: Capability-transfer lifecycle hardening

- **Status:** Accepted for the bounded single-CPU K7 / Phase 7B scope.
- **Date:** 2026-10-27.
- **Related:** NEU-005, NEU-006, NEU-007, NEU-008, NEU-009, NEU-010, NEU-011,
  NEU-013, NEU-014, NEU-021, NEU-023, NEU-024; ADRs 0018–0020.
- **Architecture:** v0.2 remains unchanged. Experimental ABI 0.5 and the
  existing fixed resource limits remain unchanged.

## Context

ADR 0020 defines the two commits that move one owned Flag/Counter reference
through the existing Channel FIFO. K6B adds blocking scalar operations and a
wait registry whose wake admission is separate from operation settlement. Their
interaction creates lifecycle boundaries that a successful enqueue alone cannot
prove: a sender can terminate before or after commit, the receiver can terminate
after delivery, and a woken Thread can already be on the runnable FIFO when its
wait becomes terminal.

The same two-slot directional FIFO must preserve mixed scalar/capability order.
A blocking scalar sender may hold an endpoint pin while the queue is full; a
dequeue can wake it, but wake is only permission to retry, not a reservation of
the newly available slot. Fault evidence must therefore reconcile the actual
transfer, wait token, scheduler admission, ownership counts and cleanup order,
not just expected serial output.

## Decision

### Terminal waiters and single admission

Terminalizing a wait preserves its existing `Ready` or `Queued` scheduler state.
Only a `Waiting` registration transitions to `Ready`. A waiter already admitted
to the runnable FIFO must not be downgraded and admitted again when ready waits
are drained. Terminal completion retains the original token until settlement
reports the error, and releases the operation pin exactly once.

Wake continues to grant eligibility only. Settlement rechecks peer liveness and
FIFO state under the existing exclusive transaction; it commits once, restores
the same wait for another block, or returns the terminal error. No capability
operation becomes blocking as part of this decision.

### Transfer and fault boundaries

The send enqueue remains the transfer commit point. A controlled sender fault
before that commit cannot create queued authority. A fault after commit cannot
reclaim or undo the queued token: the peer may still receive it after sender
cleanup. After delivery, the receiver owns the new Node-local handle and ordinary
Node cleanup accounts for it. Repeated transfers remain individual FIFO moves
with fresh recipient handle generations and separately conserved object
references.

The guest/controller fixture publishes sender-reclamation gates only after
controller cleanup. Its controlled Thread fault actions and scheduler captures
observe the before-commit, after-commit and after-delivery boundaries without
issuing direct fault instructions from the transfer routine.

### Mixed FIFO and blocking scalar wait

Scalars and capability tokens continue to share the original FIFO slots, head,
tail, length and capacity. The integration fixture fills that FIFO, registers a
blocking scalar send, then observes wake, scheduler queueing and settlement after
a dequeue. Settlement rechecks capacity and commits the pending scalar exactly
once; later capability movement and receipt are reconciled against the same FIFO
order. Endpoint pins, wait-witness publications, transfer records, queue state
and final object references/pins are checked independently by the host model and
stopped-CPU QMP observations.

### Scope and evidence boundary

No ABI operation, object type, queue, metadata pool, supported topology or
architecture contract is added. The movement subset remains eligible Flag and
Counter references over ABI 0.5, within the existing single-CPU IF=0 transaction
model. Architecture v0.2 and all older ABI profiles retain their behavior.

The `test-cap-moves-terminal-order` Cargo alias and mode selection are **not
validated**: the test runner has no matching case, and no build/QMP execution or
independent terminal-order evidence was completed. This ADR makes no claim about
that scenario. It must remain explicitly unverified until it is routed,
executed and checked against actual scheduler/ownership state.

## Alternatives considered

- **Downgrade every terminal waiter to `Ready`:** can admit a `Queued` Thread a
  second time; rejected in favor of preserving already-published scheduler state.
- **Treat wake or successful send as a reserved dequeue/slot:** would turn
  notification into ownership and bypass the existing settle-time recheck;
  rejected.
- **Undo a committed transfer when the sender faults:** would revoke authority
  already owned by the FIFO and make fault timing change the commit contract;
  rejected.
- **Add a capability queue or blocking capability operation:** would change FIFO
  ordering, resource bounds or ABI surface; deferred.
- **Use serial claims without independent state reconciliation:** cannot detect
  duplicate admission, lost pins, stale generations or incorrect reference
  ownership; rejected as validation evidence.

## Consequences and deferred work

The wait registry, scheduler FIFO and capability FIFO now have explicit checked
boundaries for the tested terminal and mixed-message cases. Ownership stays
conserved across pre-commit failure, post-commit sender teardown, receiver
teardown and repeated moves, without changing the public experimental ABI or
fixed limits. The cost is test-only witness state and stricter QMP/model checks;
those witnesses are not an ABI surface.

Terminal-order behavior remains unverified as stated above. Broader topologies,
other transferable object types, blocking capability calls, deadlines,
derivation, revocation, SMP, hardware and hosted CI remain outside this decision.
See [implementation status](../IMPLEMENTATION_STATUS.md) for actual validation
results and limits.

## History

- **2026-10-27:** Accepted for K7 / Phase 7B lifecycle hardening. Extends ADR
  0020 and the K6B wait contract without changing architecture v0.2 or ABI 0.5.

# ADR 0018: Channel conservation and retirement invariants

- **Status:** Accepted, scoped to the existing single-CPU nonblocking Channel.
- **Related:** NEU-005, NEU-006, NEU-007, NEU-008, NEU-009, NEU-010, NEU-011, NEU-021, NEU-023, NEU-024; [ADR 0017](0017-bounded-scalar-channel-endpoints.md).

## Context

A bounded Channel can reject sends under backpressure and lose messages when a
receiver terminates. These events must not make committed messages disappear
from accounting, revive old authority, or turn a partially completed teardown
into a recoverable error. ADR 0017 chose a pair of endpoint objects, two
independent two-slot inbound FIFOs and nonblocking ABI 0.3 operations. K6 needs
stronger lifecycle evidence before wait/wake design.

## Decision

For each FIFO, `head, tail < 2`, `len <= 2`, and
`tail = (head + len) mod 2`. FIFO entries are ordered from `head` for `len`
slots. Each sender direction has a separate lifetime equation:

```
sent[sender] = received[peer] + queued[peer] + discarded[peer]
```

`sent` is indexed by sending endpoint; `received`, `discarded`, and `queued`
are indexed by receiving endpoint. A successful send or receive changes its
queue and counter in one exclusive transaction; Full, Empty, PeerClosed, bad
capabilities and rights failures do not commit a message. Full and Empty return
to userspace immediately. Ordering is successful commit order; there is no
promised fairness or cross-direction total order.

Retiring a receiving endpoint discards its own inbound queue, after checked
accumulation, and preserves messages addressed to its surviving peer. That peer
can drain them before observing PeerClosed; sending to the dead receiver fails.
Closing one handle is not endpoint retirement if another reference or operation
pin exists. Last reference plus last pin retires the endpoint. Execution leases
are returned and canceled Threads cannot enter before Node table drainage;
Channel ownership is separate from page-table lifetime. No IRQ accesses these
mutable data. This is one CPU with IF=0, not an SMP or kernel-preemption proof.

Counters are `u64` per Channel generation. Checked send/receive counter overflow
refuses before queue mutation. Retirement validates its own FIFO and checks the
discard sum before mutation. Failure there is an internal invariant failure:
object collection already consumes the endpoint reference and must fail-stop,
not return a retryable successful or partially completed close. Queue metadata
corruption is rejected as terminal before indexing or mutation. Diagnostic storage
is bounded; its post-commit failure is also fail-stop. Handle, object, endpoint
and Channel generations retire on exhaustion rather than wrap. Successful trusted
reconstruction resets queue and counters only after complete retirement and gives
new Channel and endpoint generations; historical per-generation accounting must
be observed before the next creation. A new table is not a way to recreate the
same live Node identity: tables belong to exact Node generations.

Pair creation preflights both live Node tables, manifest, handles, objects and
Channel/endpoint generations before publication, under exclusive access. A failed
preflight consumes no generation, publishes no endpoint, and preserves unrelated
references and queued messages. Fixed slots are counted, not a general resource
quota or public factory. See implementation status for evidence and remaining
coverage; a host reconstruction is not user-driven creation.

## Alternatives considered

- **Unchecked counters or wrapping generations:** can hide loss or revive stale
  identity and cannot be safely interpreted by callers.
- **Return a normal close error after partial retirement:** invites retry after
  state or reference ownership has already changed.
- **Flush both directions at peer close:** destroys messages already committed
  for a still-live receiver.
- **Add waits or scheduler states now:** would require a separate atomic
  sleep/wake and lifetime contract that this phase has not established.

## Consequences and future work

A later blocking design needs owned sender/receiver wait queues, a real blocked
Thread state, atomic check/enqueue/sleep to avoid lost wakeups, a wake-one or
wake-all policy, close and peer-close wakeups, timeout and cancellation rules,
Node-wide fault cancellation, handle-close versus pinned-wait lifetime, scheduler
integration and priority-inversion analysis. None is implemented by userspace
retries. Capability moves, derivation and revocation remain separate decisions.
The architecture remains v0.2 and the ABI remains experimental 0.3.

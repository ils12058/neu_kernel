# Experimental Neu x86-64 ABI 0.4 — blocking scalar Channels

Status: experimental, implemented by the `channel-wait-test` image family. This
is the K6 / Phase 6B blocking extension of [ABI 0.3](experimental-v0.3.md). It is
not a frozen production IPC ABI, a public timeout interface, or a claim that
unrelated K6/K7 mechanisms exist. [ADR 0019](../adr/0019-blocking-channel-waits.md)
records the decision; [implementation status](../IMPLEMENTATION_STATUS.md)
records executed validation and limits.

## Compatibility and calling convention

Retain the [ABI 0.2](experimental-v0.2.md) register/preservation rules: RAX selects
the operation and returns signed status; arguments use RDI/RSI/RDX/R10/R8/R9; RDX
is the optional scalar result. RCX/R11 are SYSCALL clobbers; other GPRs and user
RSP are preserved on returning calls. Return uses the checked IRETQ boundary.
Unused arguments carry no authority. No userspace pointers are accepted.

`abi_query = 1` returns `(major<<48)|(minor<<32)|features`. Version 0.4 uses
features `0x7f` and returns `0x000000040000007f`: the ABI 0.3 bits
query/exit/bootstrap/close/inspect/Channel (1/2/4/8/16/32) plus bit 6
`CHANNEL_WAIT = 64`. Earlier images retain exactly ABI 0.1/0.2/0.3 and masks
`3`/`0x1f`/`0x3f`; ABI 0.1/0.2/0.3 reject `0x15`/`0x16` with `-1` and publish no
wait state. Query before using an extension. Published numbers are never
reassigned.

| Number | Name | Arguments | Success scalar |
| --- | --- | --- | --- |
| 1 | abi_query | none | 0x40000007f |
| 2 | thread_exit | RDI: status 0–255 | does not return |
| 0x10 | bootstrap_cap_get | RDI: index | opaque existing Node-local handle |
| 0x11 | cap_close | RDI: handle | 0 |
| 0x12 | address_space_inspect | RDI: handle, RSI: page VA | current mapping attributes |
| 0x13 | channel_send | RDI: endpoint handle, RSI: u64 payload | 0 |
| 0x14 | channel_receive | RDI: endpoint handle | received u64 |
| 0x15 | channel_send_wait | RDI: endpoint handle, RSI: u64 payload | 0 |
| 0x16 | channel_receive_wait | RDI: endpoint handle | received u64 |

Operations `0x10`–`0x14` retain their original contracts. In particular `0x13`
and `0x14` remain nonblocking: Full and Empty still return immediately. No
channel_create, public timeout/deadline, buffer copy, shared-memory, capability
argument, derive/move, revocation or cancellation operation is defined. Internal
ChannelId, EndpointId, object IDs and pointers are not handles and are never
returned through these operations.

## Blocking send and receive

`channel_send_wait` requires ChannelEndpoint SEND (internal right bit 32).
`channel_receive_wait` requires ChannelEndpoint RECEIVE (bit 64). Combined
authority is 96. The kernel validates Node/table/handle identity, object
generation/type/rights, exact endpoint owner, and Channel/peer generations
before touching queue or wait state. Authority comes from the trusted active
Node/Thread/SpaceId/root and native/scheduler/stack bindings, not unused
registers.

Both operations first attempt the real nonblocking operation:

- send with space available enqueues immediately and returns RAX=0, RDX=0;
- receive with a message available dequeues exactly one scalar and returns it in
  RDX with RAX=0;
- an immediate success also wakes the appropriate peer waiter.

On Empty (receive) or Full (send), with a live peer, the kernel registers one
owned pending operation for the calling Thread, takes one endpoint/object pin,
prepares a suspension continuation, and marks the Thread `Blocked`. It leaves the
runnable FIFO and another Thread is dispatched. The user frame is not resumed
with an invented result.

The pending scalar of a blocked send is preserved in the registration. It is
enqueued only if the later recheck finds capacity and the peer still live; a
retry never enqueues twice. A blocked receive consumes exactly one scalar if the
recheck finds one; it never consumes twice. A wake grants scheduling eligibility,
not message ownership or reserved capacity.

If a woken Thread finds the condition no longer satisfied, the same registration
returns to `Waiting` with the same token, FIFO sequence and pin, and the Thread
returns to `Blocked`. A later producer wake can select it again. There is no
userspace polling in this path.

### Indefinite waits

This profile supports indefinite waits only. There is no public timeout argument
and no hidden deadline: the pending record has "no deadline". A future deadline
extension must add explicit state and must not infer nanosecond guarantees from
the LAPIC clockevent. A QEMU deadline is a test failure boundary, never a
successful wait completion.

## Wake policy and ordering

A committed send wakes the oldest (lowest registration sequence) `Waiting`
receiver for that inbound queue. A committed receive wakes the oldest `Waiting`
sender for the capacity it created. This is wake-one, FIFO by registration, with
no priority ordering.

Terminal endpoint retirement wakes all affected waiters:

- a waiter whose own waited endpoint retires becomes terminal `PeerClosed`;
- other waiters on the same Channel become runnable to drain committed messages
  or observe closure.

A successful receive on a peer whose endpoint has retired drains previously
committed messages first; only an empty inbound queue then returns PeerClosed.
Send to a retired peer returns PeerClosed without delivering the pending scalar.
No waiter retains a recycled endpoint or Channel generation.

## Continuation and register results

The suspension stores the actual post-SYSCALL return frame. RCX and R11 still
describe the SYSCALL return site (`RCX == RIP`, `R11 == flags`); RAX/RDX hold the
operation arguments until settlement and are replaced with the final status and
scalar. CS/SS, user RSP, canonical range, stack validation and the preemptible
user flag are validated. The kernel never fabricates a hardware timer-interrupted
frame for a blocking syscall.

When woken and settled, RAX receives the signed status and RDX the scalar, then
the trusted IRETQ path restores the preserved GPRs, user RSP and post-SYSCALL RIP.
RCX/R11 remain clobbered. A terminal wait resumes once with its negative status.
A Node-wide fault or quiescence cancels the Thread without returning to user mode.

## Close, fault, cancellation and lifetime

A registered wait owns one extra endpoint/object pin. Closing the original handle
is not endpoint retirement while another reference or pin exists; the in-flight
operation may still complete from its already-authorized registration. Final
reference closure marks same-endpoint waiters terminal `PeerClosed`, releases
their wait pins exactly once, and wakes other Channel waiters. The Channel
endpoint retires only when references and pins both reach zero, so no waiter can
outlive its endpoint generation.

A Node user fault returns the active execution lease, removes every wait owned by
that Node, releases its pins, and only then completes/cancels Threads, drains
handles, and reclaims stacks, saved contexts, tables and the address-space root.
Healthy peers survive. Cancelled Threads are never resumed.

If the runnable FIFO empties while test members are still live, the supported
topology has no external producer. The controller classifies bounded quiescence:
it cancels waits, marks blocked Threads and Nodes `Quiesced`, retires resources,
and verifies cleanup. This is an explicit deadlock classification, not a
successful transfer, endless spin, reset, triple fault or timeout pass.

## Statuses

| Signed RAX | Meaning |
| --- | --- |
| 0 | Success; RDX is 0 for send or the received scalar for receive |
| -1 | Unsupported operation/configuration (legacy 0.1/0.2/0.3 images for 0x15/0x16) |
| -2 | Invalid argument/register contract |
| -16 | Invalid handle |
| -17 | Stale handle/generation |
| -18 | Insufficient rights |
| -19 | Wrong object type |
| -20 | Missing/inconsistent object |
| -21 | Wrong Node/table owner or execution-lease mismatch |
| -22 | Capacity/resource exhaustion; registration did not suspend |
| -23 | Registry in use or duplicate registration |
| -24 | AddressSpace inspection mapping error (unchanged 0.2 operation) |
| -32 | Immediate empty receive (nonblocking 0x14 only) |
| -33 | Immediate full send (nonblocking 0x13 only) |
| -34 | Peer closed; for receive, own inbound was already drained |
| -35 | Internal terminal/inconsistent Channel identity |

`0x15`/`0x16` never return -32/-33 after a suspension decision; those are
immediate-only results of `0x13`/`0x14`. A failed registration returns its negative
status without blocking or changing message counters. Node cancellation and
quiescence do not manufacture a syscall status in user mode.

## Conservation and limits

ADR 0018's per-direction invariant is unchanged:

```text
sent[sender] = received[peer] + queued[peer] + discarded[peer]
```

A blocked send counts as sent only at the actual enqueue commit; a blocked
receive counts as received only at the actual dequeue commit. Registration,
pins, readiness, retry, cancellation and terminalization do not change message
counters. FIFO capacity remains exactly two values per direction.

The wait registry is fixed at three entries, matching the supported three-Thread
topology; each Thread owns at most one pending operation. Each successful
registration adds one object pin. Diagnostic/witness storage is bounded
supervisor-only instrumentation and is not an ABI surface. The blocking image
retains the existing 96-record base diagnostic budget, its bounded Channel
records, and a 48-record wait witness ring; exceeding a diagnostic, dispatch or
test deadline is a failure, not a pass.

Operations run at IF=0 on one CPU. Condition check, registration, wake, settle and
terminalization occur inside one exclusive registry transaction; no IRQ/NMI
handler reads or mutates this state. This does not claim SMP, general kernel
preemption, physical-hardware or hosted-CI support.

Deferred: public timeouts/deadlines, priority donation/inheritance, wait sets,
capability transport/moves, derivation, revocation, public Channel/Node/Thread
creation, shared memory, events, variable-length/zero-copy payloads, ELF runtime,
SMP and Linux/POSIX compatibility.

# ADR 0020: Atomic move-only capability transfer

- **Status:** Accepted for the bounded single-CPU Phase 7A subset.
- **Date:** 2026-10-27.
- **Related:** NEU-005, NEU-006, NEU-007, NEU-008, NEU-009, NEU-010, NEU-011,
  NEU-013, NEU-014, NEU-021, NEU-023, NEU-024; ADRs 0015–0019.
- **Architecture:** v0.2 remains unchanged. This accepts the initial movement
  contract, not the architecture's future derivation or revocation proposals.

## Context

Scalar Channels communicate values but cannot hand authority to another Node.
Copying a sender's numeric handle is meaningless in the recipient's local table.
Closing it before a fallible enqueue could lose authority; granting another
reference before closing it could duplicate authority on a failed send. A move
must instead relocate exactly one existing reference through explicit owners.

K6B already owns Channels and waits in one exclusive Objects registry. The
verified active Node table is moved into a non-copyable execution lease; no
suspended mutable Nodes or Paging alias is needed. The two-slot directional
FIFOs, eight-slot tables and single-CPU IF=0 transactions remain the bounds.

## Decision

### Authority and object subset

Add endpoint SEND_CAPS (128), separate from scalar SEND (32), RECEIVE (64), and
object READ/WRITE. A source entry also has a private boolean transfer-eligibility
property. Trusted bootstrap grants eligibility only to supported Flag or Counter
entries; receiving preserves it. Knowing a handle or holding ordinary READ/WRITE
does not imply permission to move it. The endpoint must belong to the caller's
exact Node generation. No operation selects a foreign table by NodeId.

The 0.5 fixture preserves bootstrap indices 0–7 and their objects. Both Nodes'
index 3 is an eligible Flag.READ grant; only index 4's own endpoint gains
SEND_CAPS in addition to SEND|RECEIVE (224). Index 5 remains SEND|RECEIVE without
SEND_CAPS. No table or object-pool expansion is used. Other historical profiles
retain their original grants. Legacy Rights::from_bits rejects bit 128; there
is no public rights-construction API in this slice.

Only Flag and Counter references may move. AddressSpace and ChannelEndpoint are
owner-associated and deliberately unsupported; moving them would require new
ownership, root-lifetime, endpoint and wait contracts. Node/Thread/device and
MemoryObject transfers are also outside this decision. Moving one handle does
not invalidate independent handles to the same object.

### Tagged slots and reference ownership

Each existing FIFO slot contains its scalar storage and an optional non-copyable
OwnedTransfer. None means scalar; Some means capability and scalar storage is
zero. Head, tail and length govern both kinds together, with capacity two and
strict FIFO order. There is no separate capability queue. Validation rejects a
transfer tag outside the occupied interval or a nonzero scalar in a capability
slot. The optional token owns one ObjectRef and exact object rights; it contains
no user handle, borrow, Guard or stack pointer.

The reference moves through three owners:

```
source table entry -> one occupied FIFO slot -> recipient table entry
```

The global object reference count is unchanged at these transitions. Independent
creator, handle and pin ownership stays separate. Tokens cannot be cloned; their
explicit delivery or discard protocol is required. The object can be collected
only after all references and pins end.

### Two atomic commits

Send validates the live caller table, endpoint object/type/owner/SEND_CAPS,
source slot/generation/object/type/rights/eligibility, peer generation/liveness,
FIFO integrity/capacity and sent-counter capacity before mutation. It then
detaches exactly the source entry and moves its reference into the preflighted
FIFO, advances that FIFO once and increments sent once. This is one exclusive
IF=0 transaction. No recoverable allocation or user access occurs after
detachment. The enqueue/length/sent publication is the logical send commit;
other kernel operations cannot observe the earlier detached intermediate state.
The source manifest entry is left stale and cannot recreate it.

Receive validates its own endpoint with RECEIVE and checks the FIFO head kind,
live object generation, receive-counter capacity and a reusable recipient slot.
It consumes the owned token and installs a new entry using that recipient slot's
next non-wrapping generation, preserving rights and eligibility. This separate
transaction commits delivery. It does not copy the sender's handle. Numeric
handle equality across Nodes never establishes shared authority.
The recipient slot publication is the logical receive commit; no fallible work
follows dequeue before that publication. Exclusive table/registry borrows and
the still-owned handle references protect both endpoint and object generations
through each transaction, so these immediate moves need no persistent wait pin.

Full, closed-peer, bad-source, unsupported-type, missing-right and ineligible
source failures precede source detachment. A full recipient table leaves the
capability at the head with its reference, counters and slot generations intact.
The receiver may close an expendable handle and retry. Successful sending means
queued ownership; eventual delivery is not guaranteed.

### Mixed messages and K6B waits

Wrong-kind receive returns MessageKind (-36), removes nothing and never skips
the head. This applies to immediate scalar receive, blocking scalar receive and
capability receive. A scalar wait awakened by a capability enqueue rechecks and
returns MessageKind, releasing its operation pin; it cannot interpret authority
as data or sleep past an incompatible head. A successful capability dequeue
wakes a full-queue scalar sender, and also notifies an eligible local receiver
when another message is exposed. Wake grants eligibility; settlement still
rechecks under the existing transaction. No capability operation blocks or
registers a wait. Host tests exercise these mixed wait/cancel paths; the guest
fixture also checks immediate blocking-scalar kind rejection. The unchanged K6B
guest scenarios exercise actual scalar suspension and resumption.

### Close, faults and accounting

Closing the source after successful movement is stale. Closing one endpoint
handle does not retire its independent references. Final endpoint retirement
detaches all capability tokens addressed to its dead receiver and releases each
reference once through Objects. Surviving-peer inbound messages are preserved.
Sender termination cannot undo an already queued move. Receiver termination
discards its inbound transfer. Neither case introduces an ownership cycle
because transferable Flag/Counter objects own no further capabilities.

Node cleanup first retires execution leases and cancels waits, then drains
handles, accounts endpoint retirement and queued-reference releases, reaps root
references and finally destroys paging/Node metadata. The registry-reference
decrease equals closed handles plus discarded queued capability references;
equating it with handle count alone would reject legitimate cleanup.

For each sender direction, sent = peer received + peer queued + peer discarded
counts both kinds. Failed receive due to capacity or kind is not delivery.
Checked counter overflow refuses before mutation. An impossible corruption after
ownership detachment is fail-stop; expected operational failures are returned
before commit. Diagnostic failure after commit is likewise fail-stop. Exhausted
handle/object/endpoint/Channel generations never wrap.

### ABI and execution boundary

Experimental ABI 0.5 adds 0x17 channel_send_cap and 0x18 channel_receive_cap,
feature CAP_MOVE=0x80 and query 0x00000005000000ff. Existing operations remain
assigned. No pointer-copy interface or public object factory is added.
The Flag READ used by the guest is a separately gated private fixture operation,
not a promised public object ABI; see the [0.5 specification](../abi/experimental-v0.5.md).

Native entry validates Node/Thread/Space/root/TSS/stack publication and the lease
owner before invoking these methods. Table and Objects borrows end before any
return to Ring 3 or scheduler handoff. IRQ/NMI never access mutable capability,
Channel or transfer state. This is not an SMP or kernel-preemption contract.
The new QMP witness is supervisor-only, bounded and sampled while execution is
stopped at the native probe; its diagnostic addresses are never queued authority
or userspace outputs.

## Alternatives considered

- **Copy then close:** creates an unwanted second reference and complicates
  rollback; directly moving the existing token makes ownership conservation clear.
- **Close then enqueue without preflight:** loses authority on Full or peer close.
- **Separate capability FIFO:** bypasses shared ordering and backpressure.
- **Reserve recipient slots on send:** requires access to another Node's table
  and delivery reservations; receive-time capacity is sufficient and auditable.
- **Transfer every object or encode eligibility in READ:** bypasses existing owner
  contracts and conflates object use with authority movement.
- **Skip incompatible messages or leave scalar waits asleep at a cap head:**
  breaks FIFO order or hides an incompatible condition; explicit kind errors allow
  the caller to choose the appropriate operation.

## Consequences and deferred work

The bounded mechanism can hand off simple object authority without copying,
amplifying rights, leaking a queued reference or mutating a foreign table.
Remaining K7 work should expand topology and transfer/fault interleavings and
review resource governance before considering additional transferable object
types. Blocking capability operations require another decision. Derivation,
attenuation APIs and revocation belong to later work, including K10. Hardware,
SMP and hosted CI require their own validation; actual results are recorded in
implementation status.

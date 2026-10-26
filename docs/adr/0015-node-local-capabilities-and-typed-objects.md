# ADR 0015: Authorize typed objects through Node-local generation-checked handles

- **Date:** 2026-10-26
- **Status:** Accepted — K5 / Phase 5A, bounded internal authorization and test-only scalar requests, one CPU.
- **Extends:** [0014](0014-multiple-threads-in-one-node.md), [0010](0010-independent-bootstrap-nodes-and-fault-containment.md), [0009](0009-experimental-native-syscall-and-trusted-return.md).
- **Related NEU decisions:** NEU-005/006 (Node authority shared by its Threads), NEU-007/009 (explicit typed capability subset), NEU-008 (ABI 0.1 unchanged), NEU-011 (one CPU), NEU-021/023/024 (bounded security, cleanup and tests). Transfer, delegation and revocation proposals remain deferred.
- **Architecture:** [Section 5](../Neu_Kernel_Architecture_v0.2.md#5-node-model), [Section 6](../Neu_Kernel_Architecture_v0.2.md#6-capability-security-model).

## Context

Shared address spaces do not authorize protected object operations. Guessable
object numbers, Thread-indexed tables or unchecked user Node selectors would
violate the Node authority boundary. A copied table could duplicate references
without accounting, and a syscall handler borrowing the controller's suspended
mutable model would violate exclusive access. Close must also preserve independent
references and an already authorized operation's object lifetime.

## Decision

Create one non-copyable eight-slot table with every Node. Its typed identity is
bound to the complete NodeId generation. A1/A2 use the same table; B uses another.
Handles encode nonzero slot+1 and a nonzero 32-bit generation in an opaque scalar.
Validate all fields, refuse exhausted generations rather than wrap, and resolve
only in the caller's table. Equal numbers across Nodes are expected and grant no
cross-Node authority. Neither secrecy nor an unchecked NodeId selects authority.

Use a separate four-slot kernel object registry containing concrete TestCounter
and TestFlag storage. Typed object identities include slot, type and generation;
no Rust pointer, frame identity or privileged VA becomes a user handle. Counter
READ/WRITE rights are independent; Flag permits READ. Reject zero/unknown rights
and type mismatches before access. No universal ADMIN or userspace factory exists.
Explicit trusted bootstrap grants retain object references. A deferred test grant
has a fixed owner/value/rights plan and requires recorded checked transactions;
userspace phase words provide ordering only. Failed grant validates
capacity, identity and rights before either reference or handle publication.

Move the actual Node table into a non-copyable execution lease for each selected
slice, after validating the full active Thread/Node/SpaceId/root association.
Return the lease before suspension or termination can mutate the model. Native
requests validate the trusted session against this lease, current CR3 and entry
stack. No handler borrows the controller's mutable Nodes or paging manager.
One CPU, IF=0 and a non-reentrant exclusive storage guard serialize requests;
IRQs never access capability storage. This is not SMP-safe table mutation.

A successful lookup creates a bounded borrowed object guard and pins its exact
generation. Its methods enforce the authorized type and rights. It cannot outlive
registry storage or expose arbitrary mutable internals. Close removes one table
entry, releases one reference and prevents later lookup of that handle. Other
grants remain usable. The last reference reclaims the object only after all
operation pins end; an already authorized operation may finish after close.
Closing is not revocation and implies no mapping or descendant invalidation.

Retain the table after one Thread exits. After the final exit or Node-wide fault,
first finish native/IRQ ownership and cancel saved execution. Mark the table
terminal to reject new grants/lookups, then close remaining
Node handles and release their references before Node reclamation. Refuse metadata
reclamation while a table is leased or contains entries. Paging teardown remains
separate from authority cleanup. Slot reuse advances generations independently
for objects and handles. Diagnostic IDs never authorize userspace requests.

Use only feature-gated private scalar read/write/exchange/close/flag/read-close
operations outside the ABI 0.1 number range. Read-close is a lifetime test of an
already authorized guard, not a revocation primitive. Query=1, exit=2, version and
feature results stay unchanged; earlier images reject these private numbers.
No public object factory, derivation, transfer, attenuation or revocation is added.

## Alternatives considered

- **Thread-local or global handle lookup.** Breaks shared Node authority or exposes
  another Node's authority through numbers alone.
- **Pointers or payload-bearing handles.** Bypass typed kernel object storage and
  its lifetime validation.
- **Copy tables or retain a controller mutable reference in native storage.**
  Duplicates ownership or overlaps suspended exclusive borrows. Moving a lease
  gives one owner at every boundary.
- **Keep zero-reference objects forever or free them immediately despite pins.**
  Leaks bounded capacity or invalidates authorized operations; defer only until
  the last operation guard drops.
- **Expose factories/delegation now.** Adds authority-construction and transfer
  policies before the lookup/close foundation is tested.

## Consequences

Node-local authority and actual typed object operations become testable through
Ring 3 requests. Explicit ownership movement and fixed pools favor auditability
over throughput. This accepts only the initial authorization foundation. Privileged
corruption, hostile nesting, SMP, public creation, transfer, derivation, revocation,
MemoryObjects, IPC and a general userspace runtime remain separately scoped work.

## History

- **2026-10-26:** Chosen for Phase 5A before implementation. Architecture v0.2 and
  earlier ADR rationale remain unchanged. Actual evidence and remaining limits
  belong in [implementation status](../IMPLEMENTATION_STATUS.md).

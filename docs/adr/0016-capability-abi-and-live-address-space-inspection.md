# ADR 0016: Version capability discovery and inspect live AddressSpace mappings

- **Date:** 2026-10-26
- **Status:** Accepted — K5 / Phase 5B, experimental ABI 0.2, scalar read-only inspection, one CPU.
- **Extends:** [0009](0009-experimental-native-syscall-and-trusted-return.md), [0010](0010-independent-bootstrap-nodes-and-fault-containment.md), [0014](0014-multiple-threads-in-one-node.md), [0015](0015-node-local-capabilities-and-typed-objects.md).
- **Related NEU decisions:** NEU-005/006/007/008/009 (Node authority and scoped native object ABI), NEU-018 (live address-space ownership), NEU-021/023/024 (safe access, teardown and evidence). Mapping authority, factories, transfer and revocation remain deferred.
- **Architecture:** [Section 6](../Neu_Kernel_Architecture_v0.2.md#6-capability-security-model), [Section 10](../Neu_Kernel_Architecture_v0.2.md#10-native-syscalls-and-abi).

## Context

A real AddressSpace operation must read current tables, not a copied bootstrap
permission record. The controller owns mutable Paging and FrameAllocator state
across native execution. Its normal table reader deliberately requires common
CR3, while native requests run under a selected user root. Passing an unchecked
Paging pointer to a handler would violate that ownership boundary even with IF=0.
A handle alone also cannot extend root lifetime safely or deliver bootstrap
authority without a discovery/compatibility contract.

## Decision

Keep ABI query=1 and self-exit=2. Configurations with the capability ABI advertise
experimental **0.2**, with query/exit/bootstrap/close/AddressSpace-inspect feature
bits 1/2/4/8/16. Assign **0x10 bootstrap_cap_get**, **0x11 cap_close**, and
**0x12 address_space_inspect**. Retain scalar registers, preserved-state rules and
zero scalar on errors. Earlier configurations retain ABI 0.1 and reject the new
numbers. Private Phase 5A fd00..fd05 operations remain separate and unadvertised.
Never recycle published numbers; future compatible additions require documented
feature discovery. This is not a frozen production ABI.

Store a sealed, bounded bootstrap manifest of original Node-local handles in the
actual table. Trusted installation is explicit and atomic per entry. Discovery
validates the original entry every time; close/reuse never recreates a grant or
silently substitutes a new generation. The profile grants each Node its own
AddressSpace INSPECT handle and independent reference. Inert AddressSpace and
Flag grants support negative tests. Userspace discovers handles instead of
assuming their numeric encoding or supplying a Node/object/root selector.

Represent AddressSpace as a typed object owning a non-copyable **SpaceRef** to a
live SpaceId/root plus its owning NodeId. INSPECT is an object-specific right
(bit 16); Counter READ/WRITE do not grant inspection. Inert AddressSpace references
have no operation authority. The kernel's existing UserSpace remains the root
owner. Object lifetime references are counted separately from the two execution
pins, so root destruction is refused while any AddressSpace object retains one.
This does not prevent controlled mapping changes between operations.

Object-reference/pin lifetime follows ADR 0015. When the last AddressSpace handle
and operation pin end, retire the object but retain its SpaceRef until the common
boundary reaps it. This avoids mutating Registry through a shared native inspection
borrow. Release retired SpaceRefs before root destruction. A SpaceRef does not
own a capability table, so terminal cleanup has no reference cycle. Close is not
unmap, root destruction or revocation.

For one slice, create a checked immutable paging view after normal verified root
activation. Its Rust lifetime borrows the paging registry/table-ownership metadata
and prevents mutable Paging access until the slice returns. Its table reader uses
only the selected Node's already verified supervisor workspace aliases, accepts
only that hierarchy's owned levels/frames, and validates the complete active
Node/Thread/SpaceId/root binding and live SpaceRef. Page frames are accessed as
volatile hardware table storage, not through aliased Rust mutable references.
Only hardware Accessed/Dirty updates may occur during this immutable interval.

Register that immutable view through a scoped callback whose lifetime encloses
user execution. The private erased pointer addresses the view, never mutable
Paging; its callback may execute only while the registration, shared borrow,
selected root and lifetime pins are live. Clear registration on the common stack
before any paging/model mutation. IF=0 and one CPU are additional preconditions,
not the lifetime or ownership justification. IRQs never use the reader.

Walk current four-level entries for page-aligned addresses in the existing bounded
user arena. Return only PRESENT/USER/WRITE/EXECUTE bits, with effective ancestor
permissions and zero for a valid unmapped page. Reject kernel/noncanonical/out-of-
range addresses, unsupported flags/huge leaves and unowned or malformed paths.
No physical address, CR3, SpaceId or object identity is returned. Every request
requires typed INSPECT authority before the walk. The initial profile supports
only each Node's own AddressSpace; foreign views and transfers are not added.

After terminal execution, return capability/inspection leases, disable new
requests, cancel continuations, close handles, reap AddressSpace references and
then reclaim roots and Node metadata. Failed grants and object creation must
return retained SpaceRefs without partial publication or leaks. Preserve the
existing integer-only, user-preemptible, non-preemptible-kernel contract.

## Alternatives considered

- **Read through a raw pointer to suspended Paging.** IF=0 cannot justify aliasing
  its mutable controller borrow or extending a table's lifetime.
- **Cache bootstrap mapping flags in the object.** Does not inspect the live
  address space or observe later stack removal.
- **Temporarily switch to common CR3 inside a syscall.** Requires moving off the
  private stack and a larger continuation/borrow protocol; existing verified
  supervisor aliases suffice for bounded read-only inspection.
- **Count object references as execution pins.** Exhausts A's two-pin bound and
  conflates root existence with queued context ownership.
- **Hardcode startup handles or remint them after close.** Breaks opaque discovery
  and can recreate authority that the Node deliberately discarded.
- **Expose private counter operations as public ABI.** Test mechanisms are not
  the implemented AddressSpace mechanism or a production object contract.

## Consequences

The kernel exposes a small versioned authority path to an existing object, with
actual page-table reads and independent CPU evidence. Scoped pointer erasure is
confined to the audited read-only callback; borrowing, owned metadata and root
references supply its safety argument. Fixed pools, sealed test payloads, own-Node
views, 4 KiB mappings, one CPU and the bounded user arena remain limits. Public
MAP/UNMAP, MemoryObjects, factories, delegation, transfers, revocation, SMP and a
general userspace runtime require separate decisions.

## History

- **2026-10-26:** Selected for Phase 5B before implementation. Architecture v0.2
  and prior ADR rationale remain intact. Actual evidence and limitations belong
  in [implementation status](../IMPLEMENTATION_STATUS.md) and the versioned ABI specification.

- **2026-10-26 validation:** 129 host tests, 97 QEMU scenarios, nine interleaving
  repeats and four independently clean-built QMP boots pass. The full suite was
  followed by rebuilt ABI 0.2 fixtures proving healthy B inspection after A's
  close/fault cleanup. See implementation status for exact provenance and limits.

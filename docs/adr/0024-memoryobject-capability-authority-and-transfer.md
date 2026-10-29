# ADR 0024: MemoryObject capability authority and transfer

- Date: 2026-10-29
- Status: Accepted for the bounded Phase 8B scope (2026-10-29)
- Related decisions: NEU-005, NEU-017, NEU-018, NEU-021, NEU-023, NEU-024
- Architecture: [memory and object lifetimes](../Neu_Kernel_Architecture_v0.2.md#15-memory-architecture)
- Predecessors: [ADR 0020](0020-atomic-capability-moves.md),
  [ADR 0021](0021-capability-transfer-lifecycle-hardening.md),
  [ADR 0022](0022-memoryobject-backing-and-lifetime.md),
  [ADR 0023](0023-live-userspace-memoryobject-sharing.md)

## Context

ADR 0022 established generation-checked MemoryObject backing with exact
collection; ADR 0023 proved real Ring 3 sharing and read-only fault containment.
Both attach mappings through **trusted creator authority**: the fixture keeps the
creator reference and calls a trusted mapping helper. ADR 0020/0021 moved only
Flag and Counter capabilities through the K7 Channel FIFO.

K8 Phase 8B asks whether a MemoryObject can become a real capability-governed
resource without widening the ABI or the security boundary:

- a genuine, generation-checked capability type in the existing Node-local
  typed registry, with stale/retired/wrong-kind/wrong-table/forged handles
  rejected and no raw object identifier usable as authority;
- a bounded rights model (`READ`, `WRITE`, `MAP`, `TRANSFER` only) with no
  amplification, no executable mappings and no writable mapping from a
  read-only capability;
- one exact paging lifetime across the capability registry, Node-local handles,
  queued Channel transfers and mappings, with exactly-once collection;
- transfer over the existing `CHANNEL_SEND_CAP` (0x17) / `CHANNEL_RECEIVE_CAP`
  (0x18) operations, preserving ABI 0.5 and failed-operation semantics;
- a capability-authorized mapping and real Ring 3 evidence inspected by the
  independent stopped-CPU oracle, not a serial success scalar.

Trusted creator authority and capability authority must remain distinguishable:
the creator token is kernel setup authority, while a capability is Node-held
authority that can cross a Channel.

## Decision

### Representation and handle domain

`ObjectType::MemoryObject = 5` occupies the existing bounded typed registry
(`Objects`). A live slot owns exactly one non-copyable
`paging::MemoryObjectRef` value; no other representation of a MemoryObject
capability carries authority. Handles are Node-local `(slot, generation)` words;
`ObjectId` embeds kind, generation and slot. Every lookup validates live state,
generation, kind, table ownership and the requested rights before use. Raw
`ObjectId`/`MemoryObjectId` values are metadata, never authority: there is no
public constructor and no userspace operation accepts one.

### Bounded rights

`Rights` keeps the existing bit layout. MemoryObject capabilities are restricted
to `READ` (1), `WRITE` (2), `MAP` (4) and `TRANSFER` (8), combined as
`MEMORY_OBJECT` (15); the per-kind mask rejects every other bit. `EXECUTE`,
derivation (`DERIVE`) and revocation (`REVOKE`) stay deferred to K10. `TRANSFER`
is the capability's own movement right and is unrelated to the endpoint's
`SEND_CAPS`: an endpoint with `SEND_CAPS` cannot move a capability that lacks
`TRANSFER`.

Mapping authority derives from the **actual** entry rights, not the lookup
request: a writable mapping requires `MAP | READ_WRITE`, a read-only mapping
requires `MAP | READ`. Leaves are NX, `W^X` is enforced, and a read-only
capability can never authorize a writable leaf.

### Transfer over the existing Channel operations

`CHANNEL_SEND_CAP` accepts a MemoryObject source only when the endpoint carries
`SEND_CAPS` and the source entry carries valid MemoryObject rights, `TRANSFER`
and the internal transferable marker set by trusted bootstrap. The source handle
is detached and its single `Objects` reference becomes an `OwnedTransfer` in the
FIFO in one exclusive transaction. Ordinary failures (wrong kind, missing
rights, non-transferable, closed peer, full queue, exhausted counters) occur
before detachment and preserve the source handle, queue and generations.

`CHANNEL_RECEIVE_CAP` validates the queued kind and rights, installs the moved
reference into a fresh slot generation with an exact rights copy and marks the
new handle transferable. A full receiver table, exhausted receiver generation or
invalid queued description leaves the head queued and the sender's transferred
reference intact. Scalar/capability FIFO order and mixed-queue behavior are
unchanged from ADR 0020/0021. No new syscall number, feature bit or ABI revision
is introduced; ABI 0.5 remains current.

### Cross-registry lifetime

Trusted bootstrap retains exactly one paging capability reference
(`Registry::retain_memory_object`) while the creator is still live, publishes the
capability object, then releases the creator. The paging reference is owned by
the **capability object**, not by individual handles:

- every handle closed or moved keeps the reference count unchanged;
- a queued `OwnedTransfer` keeps the `Objects` reference (and therefore the
  paging reference) alive across sender exit;
- a received handle continues to own that same reference;
- mappings add their own records and do not create capability references;
- the reference is released exactly once when the last `Objects` owner dies and
  the architecture drains the bounded pending array.

Dead MemoryObject slots cannot release paging state from inside the capability
registry, so they park their reference in a fixed `pending` array. The
architecture drains it after each reaping transaction
(`release_capability_reference`), then retries slots that could not be queued
while the batch was full (`retry_pending_memory_objects`). Collection
(`collect_memory_object`) refuses while the creator, any capability reference or
any mapping exists, and restores the exact frames once none remain. Generation
values do not wrap; exhausted slots retire permanently.

### Capability-authorized mapping

`Guard::memory_object(writable)` is the only path from a handle to mapping
authority. It checks the actual entry rights and returns the live reference;
`authorize_capability_mapping` mints an authority tagged as capability
provenance; `map_memory_object` re-validates the live capability reference, the
exact root, user range, permissions and W^X before committing. A reference
released between authorization and mapping cannot map. The trusted creator path
(`authorize_memory_mapping`) remains separate and is never accepted as proof of
capability authority.

The Phase 8B fixture maps the object at `0x600000` (user RW/NX) during trusted
setup using Node A's bootstrap MemoryObject capability, before Thread execution.
That mapping is honestly **staged**: the later transferred capability does not
create a new mapping, and no runtime mmap/re-map operation exists. The staged
mapping does prove that the mapping was authorized by capability rights rather
than by the creator token; the transfer proof is separate.

### Live Phase 8B contract

The `memory-object-cap` profile runs two Nodes through the native syscall entry,
Node, Thread and lease machinery:

1. Trusted setup creates a two-page object, retains one capability reference,
   installs Node A's manifest index 3 as the MemoryObject capability, maps the
   object through it at `0x600000` RW/NX, releases the creator, and installs
   both A1 and B1.
2. A1 calls the real `CHANNEL_SEND_CAP` path through its endpoint handle and
   exits. B1 waits at its receive gate, calls `CHANNEL_RECEIVE_CAP`, receives
   the exact rights, sends the capability back through its own endpoint and
   exits. The return transfer remains queued until the Channel retires.
3. The controller retires A, destroys B and A in that order, discards the
   queued transfer exactly once, collects the object, and verifies exact
   allocator, table and frame restoration.

The controller only writes private gate/counter words in each Node's DATA page.
Transfer counts, handle generations, queue occupancy and ownership come from
production registries.

### Independent evidence

`memory-object-cap` has ten checkpoints (0–9) and a 194-word witness descriptor
(`neu_memory_cap_witness`) containing addresses, strides, field offsets, fixed
constants and schema identities only. The QMP observer
(`tools/memory_object_qmp.py`, `observe_cap`) stops the sole CPU inside
`neu_memory_cap_wait`, rechecks RIP/CPL/CR3/IF, then reads:

- the production capability slots and handle tables, including generations,
  rights and the transferable bit;
- the Channel FIFO transfer slots and scalar/capability conservation counters;
- the paging object/frame/space/mapping records and capability reference count;
- the allocator counters and bitmap, and the table-owner inventory;
- raw physical page-table pages and effective user leaf rights;
- both Nodes' saved Ring 3 frames and private gate/counter blocks.

Required observations include: the capability exists only in A's table before
the send; A's source handle becomes stale while the queue holds the single
transferred reference; B's full table rejects delivery without changing the
queue; B's received handle has the exact object identity and rights; teardown
removes all handles and discards the queued return transfer exactly once; and
the final bitmap/table inventory equals the recorded baseline. A serial
`verified=true` is never accepted as evidence.

## Alternatives considered

- **A new MemoryObject syscall or ABI 0.6**: reuses none of the proven K7
  transfer lifecycle and would freeze public object ABI before K9/K10. Rejected.
- **Per-handle paging references**: makes handle counts and queued transfers
  drive physical lifetime, double-counts received handles and complicates
  exactly-once collection. Rejected in favor of one reference per capability
  object.
- **Keep the creator reference for the fixture lifetime**: would make mapping
  authority indistinguishable from creator authority and leak the object if
  teardown is incomplete. Rejected.
- **A separate MemoryObject registry**: duplicates generation/handle logic and
  cannot reuse the typed Node table. Rejected.
- **Executable or derived capabilities**: outside K8; K10 owns derivation and
  revocation, and NX/W^X must not weaken. Rejected.
- **Deriving mapping rights from the lookup request instead of the entry**:
  would silently amplify read-only capabilities. Rejected.
- **Copying payload through the kernel or trusting guest-printed values**:
  cannot prove real Ring 3 sharing or authority. Rejected.

## Consequences

- MemoryObjects are now first-class capability-governed resources in the
  trusted test contract: real handles, generation checks, bounded rights,
  FIFO transfer and cross-registry lifetime.
- The fixed pools and ABI are unchanged: 2 Nodes, 3 Threads, 16 data frames,
  64 tables, 64 mappings, 4 MemoryObjects, 2 pages per object, ABI 0.5.
- There is still no public `memory_create`/`mmap`, no runtime mapping by a
  received capability, no derivation, no revocation, no SMP, no demand paging
  and no COW.
- The capability registry's pending batch is bounded; a full batch defers
  collection until the architecture drains and retries. Kernel-invariant
  failures remain fail-stop.
- The QMP observer depends on `repr(C)` production layouts and the single
  stopped-CPU model. Host tests cover the generic transfer/lifetime contract;
  they do not execute x86 Ring 3 code.
- ADR 0022 and ADR 0023 remain accepted for their scopes; this record extends
  them without changing their historical rationale.

## History

- 2026-10-29: Proposed for bounded Phase 8B after host transfer/lifetime tests
  and the focused `memory-object-cap` stopped-CPU scenario passed with
  independent raw-memory validation. Prior ADRs are preserved unchanged.
- 2026-10-29: Accepted for the bounded scope after `make doctor`, `make check`,
  `make build` and `make test` passed with 236 host tests and 143 unique QEMU
  scenarios, focused K7/K8A-1/K8A-2/K8B profiles, repeated `memory-object-cap`
  runs and clean-target rebuilds. The 8B slot-schema change initially broke the
  frozen K7 slot decoder; the K7 oracle now consumes a guest-published table
  layout descriptor, and every K7 scenario passed again before acceptance.

# ADR 0025: Runtime MemoryObject mapping from a received capability

- Date: 2026-10-30
- Status: Accepted for the bounded Phase 8C scope (2026-10-30)
- Related decisions: NEU-005, NEU-017, NEU-018, NEU-021, NEU-023, NEU-024
- Architecture: [memory and object lifetimes](../Neu_Kernel_Architecture_v0.2.md#15-memory-architecture)
- Predecessors: [ADR 0022](0022-memoryobject-backing-and-lifetime.md),
  [ADR 0023](0023-live-userspace-memoryobject-sharing.md),
  [ADR 0024](0024-memoryobject-capability-authority-and-transfer.md)

## Context

ADR 0024 made a MemoryObject a generation-checked Node-local capability and moved
it over the existing Channel operations, but the mapping in that phase was
honestly **staged**: trusted setup mapped the object through Node A's capability
before any Thread executed. The transferred capability created no mapping, and no
runtime mapping operation existed. Phase 8C asks the next question directly:

> Can a **running** Node receive a MemoryObject capability through real Channel
> syscalls and then cause a **new** mapping of the same physical backing that is
> genuinely authorized by that received handle?

Answering it requires more than reusing `map_memory_object` at an arbitrary
moment. The ordinary mapping transaction demands an exclusive, inactive,
unpinned destination space, which a running Node never satisfies. Removing a live
Thread's pin to force the check would weaken the ownership model; installing a
mapping while the CPU still runs that Thread's root would race with Ring 3
execution and violate the single-CPU transaction discipline. The mapping
therefore needs a **scheduler safe point** and an authority path that re-derives
the capability from the destination Node's own table.

The existing oracle and pool limits stay fixed. ADR 0024 documents 2 Nodes,
3 Threads, 16 data frames, 64 tables, 64 mappings, 4 MemoryObjects and 2 pages
per object; this phase must fit those pools without rewriting the earlier
records.

## Decision

### Test-only mapping request convention

`TEST_MAP` (`0xffff_ffff_ffff_fc01`) is a private fixture operation, not ABI 0.6
and not a public mapping syscall. Its arguments are exactly
`(handle, base, pages, writable, offset)`:

- `handle` is a Node-local capability handle belonging to the **calling** Node;
- `base` is a page-aligned user virtual address inside the existing
  `[0x400000, 0x800000)` fixture range;
- `pages` is 1 or 2, `offset` is a page index, and `offset + pages` must stay
  inside the object's two pages;
- `writable` is 0 or 1 and selects the requested leaf permissions (`R` or `RW`).

The request carries **no physical address and no raw object id**, so it cannot
target arbitrary memory or bypass rights. The kernel validates the arguments,
derives authority from the live handle with `Rights::MAP` plus the write right if
requested, and records one bounded pending request
(`MapRequest { sequence, node, thread, table, space, root, handle, object,
memory_object, base, offset, pages, permissions, writable, state, status }`).
Nothing is mapped by the syscall itself. Invalid arguments, a full single-slot
ledger, stale/forged/wrong-kind handles and missing rights return recoverable
statuses and leave every handle, reference, mapping and allocator accounting
untouched.

The one-pending-request ledger is deliberate: a second request while one is
pending returns `InUse`; a sequence mismatch, a terminal request state or a
replayed post-completion sequence returns `Owner`/`InUse` without a second
mapping. This bounds the mechanism to the fixture and prevents a request queue.

### Scheduler safe point

The request syscall returns `Outcome::Request`; the scheduler records
`SliceEvent::Requested(Frame)` and suspends the issuing Thread through
`Queue::request` → `Nodes::request` → `Nodes::suspend_context`. The Thread becomes
`Runnable` with a scheduler-owned saved continuation and is **not** placed on the
runnable FIFO. The controller therefore owns:

- the destination root (it is not active on the CPU);
- the exact saved Ring 3 context (RIP/RSP/RFLAGS/CS/SS, registers);
- the Node capability table lease used to re-derive authority;
- the single execution pin of the issuing Thread.

No borrowed capability guard or paging lease survives a context switch: the
request records only scalar identity and arguments. The guard is re-created
inside the commit transaction and dropped before the Thread is re-enqueued.

### Suspended boundary instead of weakening the ordinary API

`paging::Registry` gains an explicit `Boundary::{Exclusive, Suspended}` and
`can_map_suspended(SpaceId)` which accepts exactly the safe-point state:
no active destination space, a live root, and exactly **one** pin. The ordinary
`map_memory_object` / `validate_memory_object_mapping` / `unmap_memory_object`
entry points remain `Boundary::Exclusive` (`pins == 0`, no active root) and still
reject a live Thread's space. Only the safe-point variants
(`map_memory_object_suspended`, `validate_memory_object_mapping_suspended`, and
the `Paging::map_memory_backing_suspended` wrapper) accept the suspended
boundary. No pin is removed, no check is skipped and no public API is widened.

### Commit: re-derived authority, pin spanning authorization and mapping

`address_space::commit_request(nodes, paging, space, active, sequence)` runs in
one IF=0 kernel transaction with interrupts disabled. Before any mutation it
verifies:

- no lease is outstanding; the sequence is the pending one and still `Pending`;
- the model's Node/Thread identity, table identity, SpaceId and exact root still
  match the recorded request;
- the Thread is `Runnable`, has a scheduler-owned saved context, is not queued;
- the destination `UserSpace` token matches the request space/root;
- `paging::current_root() == paging.root()` (the CPU is at the kernel root);
- the ledger limit is not exhausted.

It then re-derives authority from the **received handle**: `Table::lookup` with
`ObjectType::MemoryObject` and `Rights::MAP`, the write right when requested, the
exact slot/generation/capability object and the live paging object identity. The
guard's pin spans authorization and the complete mapping, so the capability
generation cannot be collected between validation and install. `TRANSFER` is not
required to map; rights are never amplified.

Mapping reuses the existing page-tree transaction through
`authorize_capability_mapping` + `map_memory_backing_suspended`, which enforces
the exact root, user range, alignment, page count, object bounds, reserved
ranges, overlap, mapping capacity, W^X and NX. New intermediate tables are
recorded in the destination `UserSpace.tables`; failures roll back leaves and
tables and are retryable. Unexpected backend corruption is fail-stop. The
mapping is committed **before** the Thread is resumed, the TLB is flushed by the
existing transaction, and the saved Ring 3 context is preserved.

Committed mappings survive the received handle closing; close is not revoke. The
object stays alive while any mapping exists and is collected exactly once after
the last mapping and capability reference disappear.

### Resume, cancellation and single-CPU ordering

`Queue::resume` → `Nodes::resume_request` replaces the saved continuation and
re-admits the Thread through the ordinary enqueue transaction. It refuses a
foreign association, a queued Thread, a non-continuation context, or a Node that
no longer owns the space. Exactly one admission can succeed.

`Queue::cancel_request` → `Nodes::cancel_request` turns the suspended Thread
`Exited`, records the outcome, drops the saved context and terminates the Node
table when it was the last live Thread. A canceled Thread has no saved context,
so `resume_request` fails with `Association` and the FIFO can never dispatch it.
`address_space::cancel_request` requires the model Thread to be `Exited` already,
moves the ledger entry to `Cancelled`, and a later commit for that sequence fails
`Owner`; the record can never commit. A replay of the sequence is rejected the
same way.

On one CPU both commit and cancellation run between dispatches with IF=0, so
cancellation cannot interleave with a synchronous commit: the commit either
completes before the cancel is observed or the request is already terminal.
The cancellation profile proves this order concretely: Node A exits first and is
destroyed; a deferred second Node-B Thread is then installed and faults while the
request is pending; `Nodes::complete` cancels the suspended B1
(`ExitReason::Cancelled { by }`); the controller proves commit refusal and resume
refusal **before** retiring B1, writes the replay/done gates, and only then tears
Node B down. No Thread is resumed after cancellation and no mapping is installed
into a retired root.

### Fixed pools and frame accounting

The cancellation profile exercises three Thread identities while ADR 0024 keeps
the 16-frame data pool. Rather than raising `DATA_FRAMES`, the fixture destroys
Node A (freeing six frames) before installing the third Thread: peak usage is 14
frames during setup, 8 after A's teardown, and 11 after the second B Thread is
installed. The Node A mapping and its object backing remain valid until B's
request path and teardown complete. No pool constant or ADR 0022–0024 statement
changes.

### Profiles and independent evidence

Seven stopped-CPU profiles share eleven checkpoints (stages 0–10):

| Case | Purpose |
| --- | --- |
| `memory-object-runtime` | Received capability maps the same backing at `0x700000`; B reads it in Ring 3; A's mapping survives; staged VAs, rights and close behavior verified |
| `memory-object-runtime-writable` | `READ|WRITE|MAP|TRANSFER` permits a writable alias at `0x701000`; Ring 3 writes reach the shared backing |
| `memory-object-runtime-fault` | A read-only runtime mapping rejects a Ring 3 store with a genuine `#PF`, error `0x7`, `CR2 = 0x700000`, real CPL 3 frame |
| `memory-object-runtime-rights` | `MAP|READ|TRANSFER` without `WRITE` rejects a writable request (`-18`) and never maps it |
| `memory-object-runtime-read-denied` | `WRITE|MAP|TRANSFER` without `READ` rejects both read-only and writable requests and installs nothing |
| `memory-object-runtime-peer-exit` | A exits first; B's runtime request commits and B reads the surviving backing before its own teardown |
| `memory-object-runtime-cancel` | A first; B2's genuine `#PF` cancels B1's pending request; commit and resume are refused before B1 is retired; exact allocator baseline restored |

The QMP oracle (`tools/memory_object_qmp.py`, `observe_runtime`) stops the sole
CPU inside the bounded wait, rechecks RIP/CPL/CR3/IF, and reads a 256-word
witness descriptor containing addresses, strides, field offsets, constants and
schema identities only. It reads production capability tables, Channel FIFO
transfers and counters, paging object/frame/space/mapping records, the request
ledger and witness words, raw physical page-table leaves, `UserSpace` table
inventories, saved Ring 3 frames, the hardware fault record and the allocator
bitmap. Required observations include:

- before receipt Node B has no mapping for the target VA and no MemoryObject
  capability; after commit the same physical object pages are reachable at B's
  VA with the requested effective user permissions and NX;
- exactly one request transitions issued→pending→committed (or
  cancelled/rejected), the ledger never holds two pending requests, and a second
  or replayed sequence is refused without a second mapping;
- negative cases (forged/stale/wrong-kind/closed handles, wrong owner, missing
  rights, unaligned/reserved/overlapping/out-of-bounds addresses, invalid page
  counts, exhausted capacity, teardown/root-busy states) leave handles,
  references, tables, mappings and the allocator model-consistent;
- committed mappings survive handle close; the capability reference is released
  exactly once; the object is collected only after all mapping and capability
  references disappear; and the final allocator bitmap, table inventory and
  frame inventory equal the stage-0 baseline.

Serial success lines are never accepted as evidence; missing or mismatched raw
state fails the case.

## Alternatives considered

- **A public `mmap` syscall or ABI 0.6**: outside K8 and would freeze a public
  mapping ABI before K9/K10. Rejected; the request operation is a private exact
  argument fixture convention.
- **Removing the live Thread's execution pin to satisfy `can_allocate`**: would
  bypass the ownership proof and permit mapping into a root the CPU is executing.
  Rejected; the suspended boundary is an explicit, narrower predicate.
- **Mapping inside the syscall before suspension**: cannot guarantee an inactive
  root, an exclusive paging transaction or preserved Ring 3 state. Rejected.
- **A separate request queue or worker**: unnecessary for a single-CPU bounded
  fixture and would add untracked lifetimes. Rejected; one pending slot.
- **Kernel payload copy to emulate sharing**: cannot prove the received
  capability authorized a real mapping or that Ring 3 touched the same physical
  pages. Rejected.
- **Requiring `TRANSFER` to map**: conflates movement rights with mapping rights
  and would reject legitimate received capabilities. Rejected.
- **Treating close as revoke**: contradicts ADR 0024's lifetime model and would
  destroy committed mappings. Rejected.
- **Raising `DATA_FRAMES` to fit three Threads**: would rewrite ADR 0024's fixed
  pool contract. Rejected in favor of the A-first/deferred-install ordering.
- **Serial-only assertions**: cannot distinguish a genuine mapping from a
  reported scalar. Rejected; every stage is read from stopped-CPU production
  state.

## Consequences

- A Node can now receive a MemoryObject capability over the existing Channel
  operations and cause a **new** mapping at runtime, with the received handle as
  the sole authority. This closes the Phase 8B staged-mapping limitation for the
  bounded fixture.
- The ordinary mapping API, all pins and all fixed pools are unchanged; the
  suspended boundary is an additional predicate used only at the scheduler safe
  point. ABI 0.5, architecture v0.2 and ADRs 0022–0024 remain in force.
- Cancellation is terminal and replay-safe; commit and resume are refused before
  a canceled Thread is retired, and a canceled Thread is never dispatched again.
- There is still no public `memory_create`/`mmap`/`munmap`, no public
  MemoryObject factory, no derivation/revocation (K10), no SMP, no multi-CPU TLB
  shootdown, no demand paging, no COW, no file-backed memory, no executable
  shared mappings, no ELF loading and no Linux compatibility.
- The safe point and cancellation proofs are single-CPU only. On multiple CPUs
  they would require an IPI-based rendezvous and TLB shootdown, which remain K9
  work and are not claimed.
- Host tests cover the suspended boundary, rights re-authorization, rollback and
  the request/resume/cancel model; the QMP oracle covers physical mappings,
  hardware faults, the capability ledger and exact allocator restoration. Host
  tests do not execute x86 Ring 3 code.

## History

- 2026-10-30: Proposed for bounded Phase 8C after the host suspended-boundary and
  request-lifecycle tests passed and all seven `memory-object-runtime-*` profiles
  passed independently with stopped-CPU evidence. Prior ADRs are preserved
  unchanged.
- 2026-10-31: Accepted after final validation. The full suite passed 240 host
  tests and 150 unique QEMU scenarios, and the seven runtime profiles, the focused
  K7/K8A/K8B/K8A-2 profiles and all legacy preempt profiles passed individually
  and in repeats. Finalization corrected two test-fixture timing races without
  changing any kernel contract or oracle assertion: the K8B controller now
  publishes stage 3 only after Node B's receive-gate spin, and the legacy preempt
  fixture stops and boundedly drains the one-shot user timer at every
  `SliceEvent::Exited` rather than only before the stale audit. No authority,
  boundary, pool or ABI decision changed.

# ADR 0022: MemoryObject backing and lifetime foundation

- Date: 2026-10-27
- Status: Proposed (partial implementation)
- Related decisions: NEU-018, NEU-005 and NEU-024
- Architecture: [memory and object lifetimes](../Neu_Kernel_Architecture_v0.2.md#15-memory-architecture)

## Context

Private data and stack frames belong to a specific generation of `SpaceId`.
An alias of a private frame in that same space is not shared backing: destroying
that space eventually frees its frames. Two address spaces need backing whose
ownership and lifetime are independent of either address space. Removing the
private owner check would allow foreign private frames to be mapped.

## Decision

The proposed foundation uses a bounded, generation-checked MemoryObject identity
and distinguishes its frame ownership from private `SpaceId` ownership. Each
object has one trusted creator reference and one or two distinct initialized
frames. Frame ownership transfers only after all construction checks pass. The paging backend allocates and zeroes frames before adoption and rolls back
earlier allocations after ordinary construction failure. Failed registry adoption
leaves ownership with its caller. This is not a public factory.

A trusted internal grant names the exact object generation, space generation and
maximum write permission. Ordinary private mapping checks stay unchanged. The
separate shared mapping path verifies the grant, the live registered root,
object backing, bounds, overlapping records, capacity, and non-executable
permissions before installing leaves. Once all leaves are installed, it
publishes a per-page mapping record with object provenance. Failed installation
rolls back already installed leaves, including detached page-table frames, using
`PageTree::unmap`. This assumes the existing backend rollback operations succeed;
backend corruption remains an error, not a recoverable transaction guarantee.
The effective transaction boundary is the existing single-CPU, inactive,
unpinned-space, non-preemptible kernel context; no IRQ accesses this state.

Mapping records are the backing references; their count differs from the creator
reference and from the number of physical frames. Unmap verifies the recorded
object, backing and actual leaf before removing the PTE, invalidating the local
TLB, and retiring its record. After release of the creator reference, existing
mappings retain the object, but grants cannot add new mappings. Collection requires
no creator or mappings and validates all backing before returning the frames to
kernel ownership. Trusted caller code must then free every returned frame once.
The object generation is retired and never wraps; ordinary frame generation
checks still apply. Operations hold exclusive Rust borrows; there is no separately
tracked asynchronous operation pin in this slice.

## Alternatives considered

- Assign shared frames to a Node or to the kernel permanently: neither records
  the real shared owner nor ensures the other mapping survives Node teardown.
- Remove private-frame ownership checks: violates isolation and permits private
  stack or code backing to be repurposed.
- Implement a public capability and mapping ABI now: expands the security and
  execution boundary before allocation, teardown and rollback are complete.

## Consequences

Host tests exercise real registry and page-tree transactions with a simulated
table backend. The existing kernel paging self-test adopts frames allocated and
zeroed by the real backend, checks object ownership and returns them to its
normal free path. The kernel `UserSpace` destructor refuses to run while any
MemoryObject mapping is present; it does not yet detach those leaves, and its
consumed input is not returned on failure, so this is not a retryable teardown. The backend collection path is not recoverable after unexpected allocator
corruption during physical frees, and actual user-space mapping teardown is not
yet integrated. QMP has not independently inspected MemoryObject state,
and no Ring 3 cross-Node shared-memory or hardware permission claim is made.
These gaps keep this record proposed and Phase 8A-1 incomplete. ABI 0.5 and
architecture v0.2 remain unchanged.

## History

- 2026-10-27: Proposed initial ownership and mapping ledger; integration and
  independent runtime evidence remain outstanding.

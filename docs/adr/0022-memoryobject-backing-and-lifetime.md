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
normal free path. At the initial partial implementation, the kernel `UserSpace` destructor refused to run while any
MemoryObject mapping was present; it does not yet detach those leaves, and its
consumed input is not returned on failure, so this is not a retryable teardown. The backend collection path is not recoverable after unexpected allocator
corruption during physical frees, and actual user-space mapping teardown is not
yet integrated. QMP has not independently inspected MemoryObject state,
and no Ring 3 cross-Node shared-memory or hardware permission claim is made.
These were the original gaps. The 2026-10-28 update below supersedes these implementation details; the decision remains proposed pending its independent QMP evidence and remaining integration. ABI 0.5 and architecture v0.2 remain unchanged.

## History

- 2026-10-27: Proposed initial ownership and mapping ledger; integration and
  independent runtime evidence remain outstanding.


## 2026-10-28: Teardown and batch-reclamation update

A borrowed, non-copyable `UserSpace` token now remains with its owner when
preflight rejects a wrong root, active or pinned space, outstanding AddressSpace
reference, or live Thread stack. Node controller call sites retain their owning
option until teardown succeeds. Once destructive teardown starts the token is
marked started; unexpected errors cause a kernel panic/halt, not a retryable
result. Successful completion marks it finished, and reuse is rejected.

Preflight enumerates only this space's object mapping records, checks each
object/frame generation, permissions and actual leaf, and verifies that every
walk table is in the space's existing table inventory. It then detaches leaves,
invalidates each VA, retires each mapping record and finally performs the
existing private-frame/table and SpaceId reclamation. Shared-leaf removal keeps
intermediate tables in the original inventory until whole-root teardown, so it
cannot silently invalidate the workspace aliases or free a table twice. This
slice does not provide a general backend mapping API that extends the inventory;
the controlled kernel fixture maps only into already owned user leaf tables.

Object collection first validates the object, absence of references, each live
frame and the allocator bitmap for the entire batch. The registry then transfers
the object frames to kernel ownership and retires the object, after which the
validated allocator batch is freed and frame identities retired. The exclusive
single-CPU borrow prevents changes between preflight and commit. Unexpected
post-commit inconsistency panics; it is not a recoverable partial release.
FrameAllocator batch validation rejects duplicates and invalid entries without
changing its bitmap. Map rollback and post-leaf unmap cleanup likewise fail-stop
if backend cleanup fails rather than returning a misleading ordinary error.

A trusted `test-nodes` guest fixture tests real zeroed two-page backing mapped
RW in one root and R in a second, both teardown orders, the surviving mapping,
creator release, pinned and other retryable teardown rejections, and final
allocator counts. The host independently tests page-tree transactions and
failure injection. The existing Node QMP checks still do **not** independently
inspect this fixture's object fields or cross-root PTEs. This remains a partial
implementation, not proof of Ring 3 access or hardware-enforced sharing.

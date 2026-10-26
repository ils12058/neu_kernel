# Experimental Neu x86-64 ABI 0.2

Status: experimental, implemented only by the `abi-02` image family. This is a
small scalar contract, not a frozen production ABI. Architecture v0.2 is the
architecture document version; it is independent of the syscall ABI version.
[ADR 0016](../adr/0016-capability-abi-and-live-address-space-inspection.md) records
why these choices exist. [Implementation status](../IMPLEMENTATION_STATUS.md)
records validation and supported profiles.

## Calling and discovery convention

Use native `SYSCALL` from a valid current Thread. RAX selects the operation and
returns signed status. RDI, RSI, RDX, R10, R8, R9 contain six scalar arguments;
RDX returns the scalar result. RCX/R11 are architectural clobbers. All other GPRs
and user RSP are preserved on returning calls. Return uses checked IRETQ.
Unused arguments are ignored in this version and carry no authority. Errors
return scalar zero. No operation accepts a userspace pointer, NodeId, SpaceId,
ObjectId, physical address or page-table root as an authority selector.

`abi_query` (1) takes no arguments and returns status 0 and:

`(major << 48) | (minor << 32) | feature_bits`

| Feature | Bit |
| --- | --- |
| Query | 1 |
| Self Thread exit | 2 |
| Bootstrap capability discovery | 4 |
| Capability close | 8 |
| AddressSpace inspection | 16 |

ABI 0.2 returns `0x000000020000001f`. Earlier configurations, including Phase 5A
private capability images, retain ABI 0.1 and `0x0000000100000003`. They return
NOT_SUPPORTED for 0x10–0x12. The old query=1 and thread_exit=2 behavior remains:
exit accepts an unsigned status 0–255 in RDI, does not return on success, and
returns INVALID_ARGUMENT for larger values. Exit is per Thread; a live sibling
retains its Node and authority.

Software must query version/features before using extensions. Future compatible
additions allocate new numbers and feature bits; published numbers are never
reassigned. Incompatible changes require a separately documented version and
compatibility decision. Unsupported numbers return NOT_SUPPORTED. The private
Phase 5A `0xfffffffffffffd00`–`fd05` counter/flag operations are absent from ABI
0.2 images, remain private in their own images, and are never advertised features.
The private scheduler yield remains test infrastructure, not a public ABI operation.

## Operations

| Number | Name | Inputs | Scalar on success |
| --- | --- | --- | --- |
| 1 | abi_query | none | version/features above |
| 2 | thread_exit | RDI: status 0–255 | does not return |
| 0x10 | bootstrap_cap_get | RDI: bootstrap index | opaque handle |
| 0x11 | cap_close | RDI: handle | 0 |
| 0x12 | address_space_inspect | RDI: handle, RSI: page VA | mapping bits below |

A handle is an opaque 64-bit scalar scoped strictly to the caller's Node. Its
current internal slot/generation encoding is not a startup or stable ABI promise.
Do not infer authority from numeric values or their equality in different Nodes.
The kernel derives the caller from the checked active Node/Thread/SpaceId/root,
selected native stack, TSS and scheduler publication. Threads do not select a
capability authority domain.

### Bootstrap capability discovery

The trusted constructor installs and seals these grants **before** admitting user
execution. Both initial Nodes receive the same indexed *kind* of grants, to their
own distinct objects:

| Index | Object | Rights | Purpose |
| --- | --- | --- | --- |
| 0 | Own live AddressSpace | INSPECT | Primary inspection handle |
| 1 | Same AddressSpace, independent handle | INSPECT | Independent reference |
| 2 | Same AddressSpace, independent handle | NONE | Explicit inert negative-test grant |
| 3 | Own TestFlag | READ | Wrong-type negative-test fixture |

This is the fixed audited test profile, not a generic startup manifest format.
A1/A2 see Node A's same table and original handles. B1 sees Node B's table.
No cross-Node grants exist. Each AddressSpace object owns one retained real
SpaceRef; the constructor releases all object creator references after grant
installation. The manifest contains original handles without extra object refs.
The four objects have eight handle references and two root references initially.

Discovery returns the original still-live handle. It does not allocate an object,
create a handle, add a reference, refresh a generation or recreate closed
authority. Invalid/uninstalled indexes fail; a closed original handle stays stale
even if its slot is later reused. User-controlled phase words only arrange the
test sequence and cannot install authority.

### Capability close

Close validates the exact calling Node/table and handle generation, invalidates
one slot and releases one object reference. New lookups through that handle fail.
Double close and stale close fail. Independent handles remain valid. An already
authorized operation guard pins its object until completion; the last close
cannot free storage while such a guard lives. Closing an AddressSpace handle
neither unmaps pages nor destroys the Node's address space. Close is not revocation.

### AddressSpace inspection

Requires an AddressSpace handle with object-specific INSPECT (internal bit 16).
Counter READ/WRITE and Flag READ never grant inspection. NONE grants no operation
rights. Only live current Node-owned SpaceIds and retained roots can be inspected
in this profile. The handle is required even for a guessed VA.

RSI must be a canonical, 4096-byte-aligned page address in `[0x400000,0x800000)`.
Noncanonical, kernel, out-of-arena and unaligned inputs fail. Four-level 4 KiB
mappings only; unsupported/reserved flags, nonzero nonpresent entries, huge
leaves, unowned hierarchy pages and unsupported effective W+X fail. Hardware
Accessed/Dirty bits are accepted where appropriate. No mapping is changed.

| Result bit | Meaning |
| --- | --- |
| 1 | Present |
| 2 | User-accessible through every ancestor |
| 4 | Writable through every ancestor |
| 8 | Executable through every ancestor (NX clear) |

A valid unmapped page returns success and zero. All bits describe **current live
PTEs**, not a cached initial description. There is no returned frame address,
CR3, identity or implementation pointer. This operation discloses only attributes
of the address space for which the caller possesses authority.

## Status codes

| Signed RAX | Meaning |
| --- | --- |
| 0 | Success |
| -1 | Unsupported operation/configuration |
| -2 | Invalid address or argument |
| -16 | Invalid/malformed handle or bootstrap index |
| -17 | Stale generation, closed original bootstrap grant, or double close |
| -18 | Insufficient or inappropriate object-specific rights |
| -19 | Wrong object type |
| -20 | Expired/stale object identity |
| -21 | Invalid owner/execution/root binding |
| -22 | Fixed metadata capacity exhausted |
| -23 | Storage/lease busy |
| -24 | Malformed or unsupported live page-table state |

The sealed test images have a 96-request diagnostic budget for capability
operations; budget exhaustion returns -22 before mutation and retains the lease.
Query/exit do not consume this budget. It is not a resource-governance design.

Some internal conditions are unreachable from ordinary valid userspace. A
corrupt native publication is fail-stop before object dispatch; a checked reader
scope mismatch is rejected before page-table access. Authorization precedes
address validation/walking. Rejections never return mapping attributes. Capacity
and installation failures are exercised through trusted bootstrap/host tests;
there is no public grant or allocation operation.

## Ownership, serialization and limits

One CPU, user-only preemption, IF=0 kernel transactions. No IRQ accesses object,
capability or reader state. A Node's actual table moves in a non-copyable lease
for each native slice and returns before scheduler/model changes. Each operation
is serialized at its validated lookup/close transaction. No SMP concurrency
claim is made.

After verified root activation, a shared Paging borrow freezes structural
mutation for the entire slice. A scoped immutable view reads volatile PTE storage
through exactly that root's verified supervisor aliases. Its private erased
pointer is valid only inside the enclosing borrow/registration closure. It never
aliases suspended mutable Paging or FrameAllocator access. The callback checks
complete execution identity, current CR3, live SpaceRef and execution pins. Only
hardware A/D updates occur while the view is installed. Registration ends on the
common stack before any Paging mutation.

AddressSpace references protect root existence independently of execution pins;
they do not freeze mappings between requests. Last handle/guard release retires
the object. Common-boundary reaping releases its SpaceRef without mutating the
shared native registry view. Terminal execution leases return, canceled contexts
are retired, handles drain and root references are reaped before root/Node
reclamation. Thread-stack retirement can remove an individual stack while its
Node/root and sibling capabilities remain alive. There is no ownership cycle.

MAP/UNMAP, cross-Node views/transfers, derivation, attenuation, revocation,
MemoryObjects, IPC, factories, device delegation, general Thread creation, SMP,
kernel preemption, pointer-copy APIs and a userspace runtime are deferred.

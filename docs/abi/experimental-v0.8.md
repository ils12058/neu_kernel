# Experimental Neu x86-64 ABI 0.8 — authorized DMA

Status: experimental; implemented by `dma-test` profiles. Acceptance is recorded
in [PCI/DMA completion](../PCI_DMA_COMPLETION.md). This extends the existing
[device ABI 0.7](experimental-v0.7.md); [ADR 0032](../adr/0032-pci-iommu-authorized-dma.md)
defines the platform and isolation contract.

## Version and scalar convention

ABI_QUERY (`RAX=1`) returns success and `RDX=0x00000008000007ff`: minor version
8, the previous feature bits, and DMA=`0x400`. Earlier images retain their own
version and return NotSupported (`-1`, RDX=0) for `0x22` through `0x28`.

RAX selects the operation and returns signed status. RDI/RSI/RDX/R10/R8/R9
carry six arguments; RDX returns the scalar result and is zero on error. All
unused arguments must be zero. Reserved arguments are checked before authority.
The existing validated native-entry, saved-register and blocked-continuation
contract is unchanged. These operations accept no userspace pointer, physical
address, requester ID, interrupt route or raw device-register address.

| RAX | Operation | Used arguments in register order | Successful RDX |
| --- | --- | --- | --- |
| `0x22` | DMA_DOMAIN | Original claimed MmioRegion handle | Original DmaDomain handle |
| `0x23` | DMA_BUFFER | Original DmaDomain handle | Original DmaBuffer handle |
| `0x24` | DMA_MAP | Domain handle, buffer handle, permissions | Device-visible base `0x100000` |
| `0x25` | DMA_UNMAP | Domain handle, buffer handle | 0 |
| `0x26` | DMA_READ | Buffer handle, byte offset | One u64 from the buffer |
| `0x27` | DMA_WRITE | Buffer handle, byte offset, u64 value | 0 |
| `0x28` | DMA_SUBMIT | Domain handle, buffer handle, byte offset, byte length, direction | Submission count in this claim |

DMA_SUBMIT starts asynchronous hardware work. Its successful return is not a
completion receipt. The existing INTERRUPT_WAIT and sequence-bound
INTERRUPT_ACK operations deliver/acknowledge actual DMA completion, status bit
`0x100`. The device must finish rearming before another submission can succeed.

## Typed, generation-bound authority

| Type | Object kind | Access rights | Initial grant including DERIVE/REVOKE |
| --- | --- | --- | --- |
| DmaDomain | 9 | MAP/UNMAP=`4`, INSPECT=`0x10`, SUBMIT=`0x20` | `0x334` |
| DmaBuffer | 10 | READ=`1`, WRITE=`2`, MAP=`4` | `0x307` |

DERIVE=`0x100` and REVOKE=`0x200` remain independent management rights.
Derivation permits only exact, object-valid subsets. These objects have no
TRANSFER authority and cannot move between Nodes. Existing capability inspection
can inspect their type and rights; this extension adds no raw domain-inspection
or PCI-configuration operation.

DMA_DOMAIN requires READ on the **original** MmioRegion returned by
DEVICE_CLAIM. DMA_BUFFER requires MAP on the **original** DmaDomain. Each lazily
installs one anchor, returning that same handle on subsequent authorized calls.
Derived handles cannot call either operation to recover broader authority.
Allocation failure publishes no provisional capability and does not consume a
usable slot. The DMA profile has eleven object slots; ABI 0.7 retains nine.

Every operation checks its Node-local handle generation, exact type/rights,
executing Node, active device generation and live lease. Domain and buffer
capabilities belong to the same exclusive device claim. Ordinary MemoryObject
authority does not authorize a DMA buffer or choose its backing.

DMA_MAP requires domain MAP and buffer MAP plus all requested direction rights.
Permission `1` permits the device to read host memory, `2` permits it to write
host memory, and `3` permits both. DMA_UNMAP requires MAP on both objects.
DMA_SUBMIT requires domain SUBMIT and buffer MAP plus READ for direction `1`
(host to device) or WRITE for direction `2` (device to host). Direction `3` is
invalid for a submission. Existing mappings cannot amplify the submission
handle's rights.

## Buffer and mapping contract

Each claim owns one logical **8192-byte** buffer backed by two reserved,
physically noncontiguous kernel pages. Their intervening canary page is never
included in the application's IOMMU mapping. CPU access is mediated through
DMA_READ/WRITE; the buffer is not mapped into the caller's CPU address space.
Word offsets must be 8-byte aligned and smaller than 8192.

DMA_MAP publishes exactly two 4 KiB translations at IOVA `0x100000` and
`0x101000`. It returns only after synchronous context/IOTLB invalidation.
Only one application mapping may exist. DMA_UNMAP clears both leaves and
returns after the corresponding invalidation completes. Map/unmap epochs must
match actual completion before a later request may use or reuse the mapping.

A submission accepts any byte offset and a length from **1 through 4096**;
checked `offset + length` must be at most 8192. Cross-page transfers therefore
work without requiring contiguous physical backing. There is one in-flight
operation. CPU buffer access, another submission and unmap are denied while
that operation remains busy. Each claim permits **16 successful submissions**;
the next otherwise valid submission returns `-53` and quarantines the claim.
The existing device watchdog and mediated-register budget also apply.

Cache visibility is part of the backend contract: this implementation requires
CLFLUSH with a 64-byte line size, flushes table updates before invalidation, and
synchronizes buffers before issue and after actual completion. It does not
assume the IOMMU advertises coherent page-table walks.

## Withdrawal, completion and reassignment

Closing any DMA capability, closing an existing claim anchor, revoking a tracked
anchor/mapping/submission descendant, or losing the owning Node quarantines the
whole claim. Mapping and in-flight submission grants are tracked separately;
revoking a SEND-only descendant can cancel work mapped through its parent.
Strict-descendant revocation still preserves the source capability itself.

Authority withdrawal denies further use immediately. A surviving blocked waiter
receives the existing terminal result exactly once. Hardware ownership and
buffer backing persist until RUN clears **and** actual completion is observed;
revocation does not pretend to undo an already issued transfer. The I/O APIC
route stays masked during retirement.

After old work retires, the backend removes application translations, waits for
invalidation, zeros host buffers, writes a private zero page into EDU SRAM and
reads all 4096 bytes back through a separate private verification mapping. Both
transfers must produce their actual masked completions. Only after full zero
readback does it remove the private mappings, clear address/count registers,
perform the existing factorial reset and retire every old typed lease handle.
Unproved quiescence/invalidation retains quarantine and reserved backing.

EDU ignores command-register writes with RUN clear. Its legitimate final command
value is therefore `6` after the last device-to-host scrub readback, with RUN=0;
source, destination and count are zero. Final cleanup disables bus mastering
and keeps VT-d enabled against empty reserved roots.

## Errors and precedence

Existing argument/capability/device errors remain in force. New DMA errors are:

| Status | Meaning |
| --- | --- |
| `-49` | Bounds/alignment/length/overflow violation |
| `-50` | Invalid or unmapped DMA direction/permission |
| `-51` | Mapping state conflict or missing completed mapping |
| `-52` | Busy buffer, device phase or incomplete invalidation |
| `-53` | Submission/epoch quota exhausted |
| `-54` | Required IOMMU isolation or hardware operation failed |
| `-55` | Invalid DMA owner/generation/lease |

The dispatcher checks reserved arguments first, then domain authority, direction
encoding and buffer authority. Submission checks armed/idle/clear-status state
before mapping permissions, transfer bounds and quota. Buffer word access checks
offset/alignment before busy state. Earlier device/capability checks can return
their own stale, rights, type or quarantine errors before a DMA-specific error.
No-IOMMU startup fails before granting a DMA domain or enabling bus mastering.

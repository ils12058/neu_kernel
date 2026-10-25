# ADR 0005: Preserve boot memory and allocate physical frame ownership

- **Date:** 2026-10-25
- **Status:** Accepted — K1 / Phase 1A only.
- **Supersedes:** [ADR 0004](0004-limine-phase-0-integration.md), the Phase 0
  integration contract. Its loader pin and rationale are retained here.
- **Related NEU decisions:** `NEU-004` (boot integration), `NEU-018 / MEM`
  (initial PMM only), `NEU-025 / BOOT` (full first-Node plan remains open).
- **Architecture:** [Section 15](../Neu_Kernel_Architecture_v0.2.md#15-detailed-physical-and-virtual-memory--neu-mem-001)
  and [Section 25](../Neu_Kernel_Architecture_v0.2.md#25-decision-register-seed-for-adrs).
- **Related ADR:** [0006: exception foundation](0006-kernel-exception-descriptors-and-fatal-handling.md).

## Context

Phase 0 preserved only a memory-map summary and retained all loader memory.
Frame allocation needs the full descriptors, explicit kernel/metadata exclusion,
and an ownership model that works with sparse physical ranges. Accessing newly
allocated physical frames is a separate mapping problem; no controlled kernel
mapping or page-table ownership transfer exists yet.

Limine's executable/module memory-map type is illustrative, not an authoritative
source of the executable's location. The actual image extent must independently
protect the descriptor copy, bitmap, code/data, stacks, and descriptor tables.

## Decision

Retain Limine **12.9.4**, base revision **6**, Rust **1.95.0**, the freestanding
x86-64 ELF, and the Phase 0 UEFI/QEMU workflow. Extend the revision-0 firmware and
memory-map requests with **Executable Address**. Combine its physical/virtual
bases with linker-defined image bounds to reserve the actual physical image.
Validate the virtual base, page alignment, and range arithmetic before use.

Copy every validated descriptor into fixed kernel storage (maximum 4096),
preserving base, length, and raw type. Audit the copy against retained loader
data, publish it immutably, and stop exposing loader descriptor pointers.

Only full 4 KiB frames in explicitly usable regions may be candidates. Exclude
physical page zero and the actual kernel reservation even if a usable descriptor
overlaps it. Every other region type and every gap stays reserved, including
bootloader-reclaimable, modules, ACPI, firmware/runtime, MMIO, and unknown types.
**Do not reclaim any bootloader-reclaimable memory in Phase 1A.** Loader page
tables and other live bootstrap structures remain reserved. Kernel-owned CPU
descriptors replace loader GDT use under ADR 0006 without authorizing reclamation.

Use a bitmap over dense **candidate ordinals**, translated through usable spans;
do not index metadata by the highest physical address. The fixed 128 KiB bitmap
supports at most 1,048,576 candidate frames (4 GiB of candidate RAM), independently
of holes or high physical addresses. Exceeding metadata capacity fails explicitly
without publishing a partial pool. Both descriptor and bitmap storage are inside
the independently reserved kernel image; they never consume unreserved frames.

Allocation/free use exclusive mutable access on one BSP with IF=0. Reject
unaligned, noncandidate, and unallocated frees; return explicit exhaustion;
maintain `allocated + free = total`. Allocation returns a physical address only.
It neither establishes mappings nor reads, writes, or zeroes frame contents.

The page-fault **test image only** additionally requests HHDM to inspect existing
loader page tables read-only. It does not extend frame-allocation authority or
install a general physical-memory access mechanism.

## Alternatives considered

- **Dense physical-address bitmap.** A large sparse address space would consume
  metadata for holes and reserved devices; candidate ordinals avoid that cost.
- **Dynamic metadata/free lists or a buddy allocator.** These add storage,
  contiguous-allocation, or mapping dependencies before a mapping foundation
  exists. Static metadata provides an explicit bound and ownership proof.
- **Trust memory-map labels alone for kernel exclusion.** This omits an
  independent reservation of the actual image and all allocator metadata.
- **Release loader-reclaimable RAM now.** Copying descriptors or replacing the
  GDT does not transfer ownership of the live loader page tables and all other
  bootstrap references. That release would be premature.

## Consequences

- No heap, mapped-frame writes, or general-purpose allocator dependency is
  needed. Sparse high addresses remain representable with a small bitmap.
- Fixed storage reserves 96 KiB for descriptors and 128 KiB for allocation bits.
  Oversized maps/pools fail closed; allocation and address translation have
  bounded linear scans rather than performance guarantees.
- Callers retain ownership discipline. Free detects currently unallocated
  frames; it cannot distinguish a stale address from a later allocation of the
  same frame. This is a trusted internal API, not a userspace authority contract.
- Returned frames are not promised mapped, writable, or zero-filled. Userspace
  exposure requires separate mapping, initialization, and lifetime work.
- Only the PMM/ownership portion of `NEU-018` is accepted. VMM, MemoryObjects,
  heap allocation, reclamation, and the full integration plan remain future work.

## History

- **2026-10-25:** Accepted under the explicit Phase 1A implementation request.
  Supersedes the Phase 0 contract in ADR 0004 with the extensions above; preserves
  its original rationale and loader choice. Validation and implementation limits
  are recorded in [implementation status](../IMPLEMENTATION_STATUS.md).

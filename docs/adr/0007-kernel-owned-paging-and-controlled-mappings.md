# ADR 0007: Own paging structures and constrain kernel mappings

- **Date:** 2026-10-25
- **Status:** Accepted — K1 / Phase 1B, single CPU with IF=0.
- **Amends:** [0005: physical ownership](0005-boot-memory-ownership-and-frame-allocation.md)
  and [0006: exception foundation](0006-kernel-exception-descriptors-and-fatal-handling.md).
  [0004: Limine integration](0004-limine-phase-0-integration.md) remains Accepted.
- **Related NEU decisions:** `NEU-004` (bootstrap access), `NEU-018 / MEM`
  (kernel paging only), `NEU-023 / DIAG` (guards), `NEU-024 / TEST` (local tests).
- **Architecture:** [Section 15](../Neu_Kernel_Architecture_v0.2.md#15-detailed-physical-and-virtual-memory--neu-mem-001)
  and [Section 25](../Neu_Kernel_Architecture_v0.2.md#25-decision-register-seed-for-adrs).

[ADR 0008](0008-isolated-ring-3-fixture-and-test-return.md) adds the scoped Phase 2A
user-root and test-return contract; the original rationale below is retained.

## Context

The Phase 1A allocator owns physical frames but provides no access mechanism.
The CPU still uses loader tables whose writable direct-map aliases are unsuitable
as an enduring permission boundary. A new root must preserve live code, constants,
data/BSS, the active stack, IST, descriptors, and copied boot/allocator metadata.
Copying a top-level table would leave the kernel borrowing its child tables.

Dynamic table construction also needs access to frames before the new hierarchy
exists. Correct rollback and frame reuse require explicit ownership, parent/child
references, temporary-access lifetimes, and local translation invalidation.

## Decision

Keep Rust 1.95.0, `x86_64-unknown-none`, Limine 12.9.4/base revision 6, and the
verified 1-vCPU UEFI/TCG profile. Request revision-0 **HHDM** for normal bootstrap,
in addition to the existing firmware/map/executable-address requests. This is a
necessary, temporary access dependency, not a permanent direct map.

Before activation, inspect the actual loader hierarchy through retained HHDM
table pages, including 1 GiB/2 MiB leaves. Check table pages against retained
loader regions, physical-address width, canonicality, and effective permissions.
Before touching a newly allocated frame, prove that its HHDM view translates to
that owned physical page, writable and supervisor-only. Trust in Limine's mapping
of its own retained table pages is the initial bootstrap prerequisite.

This bootstrap HHDM is a non-owning view, not a reference transferring ownership
of usable RAM to the loader. Temporary borrows end before rollback returns any
new frame. All parent/child references in the abandoned new hierarchy are
cleared first; the untouched active loader hierarchy continues using its own
retained table frames. No HHDM view remains after activation.

Allocate and zero every new table from the physical allocator. Construct a fresh
four-level root without copying loader parent pointers. Track each owned table,
its level, pinned state, and parent references. Verify the complete required image
mapping and table aliases before CR3 changes. PGE and PCIDE must be disabled;
four-level paging, NX, write-protect, and IF=0 remain required. Activate only a
distinct owned root, then check CR3 and mappings again before reporting success.

Map the image in 4 KiB supervisor pages: RX text; R/NX constants; RW/NX requests,
data/BSS and stacks. Linker-aligned **one-page low and high guards** surround both
the 64 KiB normal and 32 KiB #DF stacks. Their physical pages remain reserved as
part of the image, but their virtual PTEs are absent in the new root.

After activation, discard the loader HHDM from the active address space. Use a
bounded table workspace at `0xffff900000000000`: 64 RW/NX table aliases and a
single temporary access slot at index 511. A scratch borrow accepts only a newly
owned frame or a live data-frame identity, is non-executable, and is removed and
invalidated before returning. It cannot leak a Rust reference beyond the borrow.
Mutable access is rejected while that frame has an executable alias.

Use generation-checked identities for at most 16 owned data frames and explicit
records for at most 64 controlled mappings. Allocation zeroes all 4096 bytes
before returning an initialized identity. Map only initialized owned frames into
the 4 MiB test region at `0xffffa00000000000`. Support R, RW/NX, and RX; reject
W+X, incompatible physical-frame aliases, alignment/range/width errors, duplicate
locations, stale identities, and capacity failures. There is no public syscall.

Prepare mapping transactions before publishing ownership references. If any page
fails, roll back earlier leaves and newly allocated child tables. Removing a
mapping does not free its backing frame: remaining mapping references prevent
release. Detach empty, unpinned intermediate tables, flush the current root's
translation/walk caches, remove their workspace aliases and invalidate those
aliases, then return table frames to the allocator. Leaf changes use `invlpg`.
The active root and pinned image/workspace tables are retained for kernel life.

Keep **all bootloader-reclaimable RAM reserved**. The old root becomes inactive,
but no complete audit of other loader/firmware allocations is claimed. Failed
construction before activation clears all new references and returns new frames
while leaving the old CR3 intact. Post-switch failures use fatal diagnostics.

## Alternatives considered

- **Shallow-copy the loader root.** Child ownership, broad aliases, and their
  lifetimes would remain inherited. It does not establish an owned hierarchy.
- **Keep or rebuild a broad writable HHDM.** This adds aliases to executable
  backing and mapping work for unrelated physical ranges. A bounded workspace
  avoids that permission and scope burden.
- **Recursive paging.** Viable, but introduces a recursive layout and related
  invalidation/lifetime considerations beyond the selected small workspace.
- **Static table pages or a general heap/VMM.** The former would bypass the
  requested allocator-backed ownership; the latter adds unnecessary machinery.
- **Reclaim loader RAM at the switch.** Retiring CR3 alone is not a complete
  dependency audit, and increased free counts are not an acceptance criterion.

## Consequences

- Active-root W^X covers the image and supported controlled aliases; no writable
  HHDM alias of kernel text survives. This claim concerns this active hierarchy,
  not the retained inactive loader tables or arbitrary privileged modification.
- Table frames, mapping references, initialization, and physical ownership have
  distinct lifetimes. Direct physical addresses alone authorize no access.
- Metadata and pools are bounded: 64 table frames, 16 data identities, 64 mapping
  records, and one temporary view. Exhaustion is explicit. Scans and conservative
  CR3 flushes favor auditability over performance.
- New mappings use 4 KiB WB RAM pages only; boot inspection understands large
  pages. User mappings, device cache policies, SMP/shootdowns, demand paging,
  MemoryObjects, Nodes, and general recovery remain outside this contract.

## History

- **2026-10-25:** Accepted under the explicit Phase 1B request. Clarified ADR
  0005 as an amendment to 0004 while retaining the prior dated history. Host
  transaction tests, QEMU transition/permission/guard tests, exact commands and
  remaining limits are recorded in [implementation status](../IMPLEMENTATION_STATUS.md).

- **2026-10-25, Phase 2A:** ADR 0008 adds private user tables and a separate
  guarded RSP0 stack, classifies user-origin faults, and restores the kernel
  continuation before cleanup. Existing supervisor and #DF behavior remains.
  No full Node or native syscall specification is accepted by that test path.

Reference: [Intel SDM, Volume 3A](https://cdrdv2-public.intel.com/819714/253668-sdm-vol-3a.pdf),
paging structures and translation/walk-cache invalidation; the pinned Limine
protocol reference remains in `boot/limine.lock.json`.

### 2026-10-25 — Phase 3A extension

[ADR 0010](0010-independent-bootstrap-nodes-and-fault-containment.md) extends
private user-root ownership to two live domains with typed address-space IDs,
scoped mapping keys and independent cleanup. Each user workspace maps only its
own table frames; backend inspection/mutation requires the pinned kernel root.
The original pool bounds and W^X/flush-before-reuse rationale remain accepted.
This does not introduce shared MemoryObjects or a generalized userspace VMM.

# ADR 0030: Bounded capability derivation and revocation

- Date: 2026-11-14
- Status: Accepted for the bounded K10 authority model; validation acceptance is
  recorded separately in [K10 completion](../K10_COMPLETION.md).
- Related decisions: NEU-005, NEU-007, NEU-008, NEU-009, NEU-011, NEU-018,
  NEU-021, NEU-023, NEU-024.
- Architecture: [capability security](../Neu_Kernel_Architecture_v0.2.md#6-capability-security-model),
  [shared memory](../Neu_Kernel_Architecture_v0.2.md#8-memory-architecture-and-zero-copy-ipc),
  [memory lifetimes](../Neu_Kernel_Architecture_v0.2.md#15-detailed-physical-and-virtual-memory--neu-mem-001),
  [roadmap](../Neu_Kernel_Architecture_v0.2.md#22-development-roadmap-and-acceptance-tests).
- Extends [ADR 0015](0015-node-local-capabilities-and-typed-objects.md),
  [ADR 0020](0020-atomic-capability-moves.md),
  [ADR 0024](0024-memoryobject-capability-authority-and-transfer.md),
  [ADR 0025](0025-runtime-memoryobject-mapping.md) and
  [ADR 0029](0029-bounded-scheduled-smp.md).

## Context

K9 supplies synchronized Node/Thread/Channel transactions and hardware TLB
shootdowns. A handle can already close or move without destroying another
owner's object. K10 must distinguish those lifetime operations from withdrawing
delegated authority, including a mapping whose original handle has closed.
Revoking every grant to an object would also destroy independently issued
authority; revoking handles alone would leave live translations usable.

## Decision

### Explicit derivation and strict descendant revocation

`DERIVE=0x100` and `REVOKE=0x200` are independent management rights. Earlier
grants gain neither implicitly. Trusted bootstrap creates an independent
lineage root when a grant carries either right. Derivation creates one new
Node-local handle with a nonempty, object-valid subset of the source rights.
Transfer eligibility is inherited, never added, and MemoryObject movement
still requires TRANSFER. Cross-Node delegation uses the existing atomic move.

Revocation affects the source's **strict descendants**. It preserves the
source, branches outside the selected subtree, and independent roots to the
same object. A successful revoke permits fresh derivation from the surviving
source. Closing a source or moving a descendant does not implicitly revoke.

The fixed 64-slot lineage uses checked 32-bit generations. Each entry counts
handle, queued-transfer, wait and mapping owners separately. Ownerless ancestors
remain structural until their last child disappears; generation-retired slots
cannot wrap into reusable authority. A noncopyable revocation ticket retains
its source and binds a monotonic epoch. Only one revocation may be active;
derivation within its scope returns InUse. Unrelated derivation remains legal.

### One transaction domain, explicit operation cancellation

The K9 Acquire/Release lock serializes lookup/use, derive, close, move, wait
registration/settlement and the initial revoke sweep. IRQ handlers never take
this lock. No mutable registry borrow or capability lease crosses user entry,
an IPI wait or a scheduler switch.

Begin first marks descendants unusable. The controller purges affected handles,
removes only affected capability messages while retaining surviving FIFO order,
and settles affected waits with Revoked. Settlement independently rechecks
lineage even if readiness preceded revocation. Scalar messages and independent
grants retain their existing semantics. Counter/capacity rejection is preflighted
before publishing revocation. Unexpected corruption after publication fails stop.

### Mapping ownership survives until hardware invalidation completes

A tracked MemoryObject lookup cannot expose the legacy untracked reference.
`MapPermit` binds space, root, offset, range and permissions; its borrowed
authority view cannot escape the guarded callback. The paging core rejects a
different range. The trusted architecture adapter atomically installs exactly
that range and verifies it before the callback succeeds. Each mapped page then
owns one lineage reference and one capability-object pin, separately from its
live paging record and backing reference.

Revocation validates the grant, mapping record and actual owned leaf PTE, then
atomically clears that PTE. Both registries retain ownership while an epoch-bound
shootdown is pending. Every online CPU is targeted: a CPU executing the affected
root performs INVLPG; another reloads its actual CR3. This conservative mask
avoids relying on an unimplemented per-root CPU residency cache.

Only the coordinator's completed receipt, matching root/address/epoch/target
mask, permits retirement. The architecture also verifies the PTE remains
absent. Success returns after every selected mapping retires. Missing ACKs
fail stop and retain backing. Normal unmap requires an exclusive unpinned
space. Final teardown restores the allocator and page-table inventory.

The requesting Thread blocks while the BSP services mappings, without retaining
the transaction lock. The revocation is controller-owned: requester cancellation
or Node exit cannot abandon it. CPUs keep servicing IPIs until pending work
finishes, even when all user Threads have exited. A cancelled requester is never
woken or given a new handle.

### Explicit experimental ABI and finite scope

[ABI 0.6](../abi/experimental-v0.6.md) adds CAP_DERIVE/CAP_REVOKE and the authority
feature bit. Old operation numbers, versions and feature masks remain unchanged.
All unused arguments of the two new calls must be zero; they accept only local
handles and scalar rights. Mapping selection, physical addresses, lineage IDs
and remote Node identities are never supplied by userspace.

The target remains x86-64/QEMU, at most four CPUs, two Nodes, three Threads,
integer contexts, fixed metadata and non-preemptible kernel transactions.
K10's data-frame pool is 18, extending the earlier 16 by its two shared backing
pages. The legacy pool remains 16. There are 64 mapping records, eight handles
per Node, six capability objects, one Channel pair and three pending waits.
Public object factories, general mapping APIs and K11 device authority remain
separate work.

## Alternatives considered

- Implicit management rights on existing handles would silently increase
  authority. Explicit bootstrap management rights preserve earlier grants.
- Object-wide revocation would withdraw independent authority. A grant forest
  provides a concrete subtree and survives handle close/move.
- Deleting only handles would leave queued, blocked and mapped access alive.
  All four owner classes participate in revocation.
- Reclaiming at PTE removal would race stale remote translations. Both
  registries retain ownership until hardware completion.
- Global stop-the-world execution or a per-root residency optimization would
  add scheduler/cache protocols. Conservative online-CPU invalidation uses
  K9's existing verified mechanism while unrelated Threads keep running.
- General mapping/factory syscalls, arbitrary limits and fine-grained locks
  would broaden the ABI and synchronization problem beyond K10's acceptance.

## Consequences

Revocation is linear in bounded grant, handle, queue and mapping pools; a
mapping revoke may wait for several sequential page shootdowns. A global
revocation slot limits throughput but makes ordering and failure behavior
explicit. Completion cannot undo data that an authorized caller already copied.

The independent [authority QMP protocol](../testing/authority-qmp.md) checks raw
production state, actual hardware frames/PTEs and post-instruction observations.
Host tests cover concurrent transaction interleavings, exhaustion, cancellation,
rollback and failed shootdown retention. These local tests are not physical
hardware or external CI validation.

## History

- 2026-11-14: Chose the bounded K10 model and ABI 0.6 while implementing the
  one-shot K10 task. Earlier ADR deferrals remain accurate for their recorded
  milestones; this ADR extends their derivation/revocation boundaries.

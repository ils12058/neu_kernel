# ADR 0029: Bounded scheduled SMP with shared transactions and remote invalidation

- Date: 2026-11-14
- Status: Accepted for bounded Phases 9C-2, 9C-3 and 9D
- Related decisions: NEU-003, NEU-004, NEU-005, NEU-011, NEU-013, NEU-017,
  NEU-018, NEU-020, NEU-021, NEU-023, NEU-024
- Architecture:
  [scheduling and SMP](../Neu_Kernel_Architecture_v0.2.md#11-scheduling-smp-and-deferred-asymmetric-cores),
  [scheduler and timer architecture](../Neu_Kernel_Architecture_v0.2.md#17-scheduling-clocks-and-cpu-topology--neu-sched-001),
  [roadmap and acceptance](../Neu_Kernel_Architecture_v0.2.md#22-development-roadmap-and-acceptance-tests)
- Predecessors: [ADR 0013](0013-single-cpu-user-preemption.md),
  [ADR 0025](0025-runtime-memoryobject-mapping.md),
  [ADR 0026](0026-smp-bootstrap-and-percpu-foundations.md),
  [ADR 0027](0027-smp-crosscpu-work-and-coordination.md),
  [ADR 0028](0028-static-smp-user-execution.md)

## Context

Phase 9C-1 proves two statically assigned Ring 3 Threads can execute together,
but its AP cannot schedule, transact against shared authority, block, migrate
or change live translations. K9's architecture acceptance requires concurrent
scheduling and synchronization stress. The remaining work therefore needs
reusable CPU-local entry, synchronized lifecycle ownership, remote sibling
quiescence and real cross-CPU translation invalidation.

The fixed pools remain sufficient for an initial proof: at most four CPUs,
two Nodes and three Threads. General kernel preemption, balancing, arbitrary
AP allocation and public mapping/authority interfaces can be evaluated after
those invariants are established.

## Decision

### Every CPU runs the same scheduling loop

`kernel/src/smp_scheduler.rs` owns one FIFO per CPU, an explicit Thread owner
and affinity mask, and one generation-bound fault origin per Node. Admission,
dispatch, save, block, wake, migration and completion preserve one execution
owner per Thread. A queued Thread carries a complete saved context; a running
Thread carries no saved publication; a blocked Thread owns a native continuation
and cannot be queued until one wake commits.

Migration operates only on queued work with an owned saved context and an
allowed destination. Running, blocked, stopping, stale or already migrated
work is rejected. Queue removal and destination admission form one exclusive
transaction. CPU roles are absent from Node and Thread identities. The BSP
does boot setup and fixture coordination, then uses the same scheduler as APs.

The fixtures initially place A1 on CPU0 and A2 on CPU1; B1 shares CPU0 on the
two-CPU image or starts on CPU2 on the four-CPU image. A1/A2 share Node A's
root and authority table, with distinct guarded stacks. CPU3 initially idles
and receives A2's saved context in the migration profile. These placements
are test inputs rather than permanent scheduling roles.

### Short shared transactions have one lock and explicit boundaries

`kernel/src/sync.rs` supplies a bounded Acquire/Release `SpinLock<T>`. Its guard
cannot move between CPUs, and shared use requires `T: Send`. The hardware
controller places Nodes, queues, capability/Channel state and inspection
bindings in one `SpinLock<State>`. Kernel transactions require IF=0 and DF=0.
Every authority lease returns before its transaction ends; no guard or mutable
registry borrow survives user entry, IPI delivery or an interrupt wait.

This coarse lock fits the fixed pools and makes close/use, Channel settlement,
queue ownership and Node teardown share one ordering. It serializes kernel
transactions while allowing concurrent user execution. IRQ handlers never
acquire it: they copy CPU-owned frames, acknowledge their LAPIC, publish local
reschedule/invalidation state, and return to the owning controller. Per-CPU
locks and balancing can refine this ordering later without changing ownership.

The frame allocator, user-root construction, final destruction and serial
device remain BSP-owned. The permission transaction below changes existing
backing under the shared lock and allocates nothing.

### Entry and exit preserve owned complete frames

The x86-64 bridge is reusable for every scheduled Thread. Native SYSCALL and
hardware IRQ paths produce the same 176-byte normalized exception layout;
the controller copies its 160-byte ABI Frame into CPU-owned storage and restores
the kernel continuation before any shared transaction. Entry sets the Thread's
owned TSS.RSP0, private root and validated full context. Exit restores CR0,
CR3, EFER, GS/KERNEL_GS, native MSRs and segments.

SYSCALL uses a CPU-local SWAPGS stack selection and SFMASK. A CPU's hardware
APIC identity is re-derived before reading its local storage, since user GS
is zero. The timer uses vector 224, the reschedule/shootdown IPI vector 241
and spurious vector 255. Three PIT samples calibrate a 10 ms LAPIC one-shot
quantum; kernel code remains non-preemptible.

Every native/timer candidate is validated before a syscall can change authority,
Channel or wait state. Invalid user RIP/RSP, flags or native RCX/R11
binding becomes `FaultSource::RejectedReturn` through the Node-fault protocol.
Malformed user state cannot turn a failed saved-context check into a panic.
Only the ABI's result registers change when a native continuation resumes.

When no work is runnable, the CPU checks its wake generation with IF clear,
then enters `sti; hlt; cli`. A changed generation avoids sleep; a pending
IPI wakes the halt. Queue publication and notifications cannot lose a wake.

### A Node fault quiesces siblings before reclamation

A lock-held election binds one active Thread as the Node's fault origin and
removes its queued siblings from admission. Running siblings receive reschedule
requests and relinquish their CPU slots as `Cancelled { by: origin }`. A
competing fault joins the elected cancellation instead of committing another
Node fault. The origin waits without holding the lock and commits only when
no sibling remains active. Normal/unauthorized completion during a stop and
completion with an outstanding authority lease are rejected before mutation.

The unrelated Node remains runnable. Capability tables close and Node waits
cancel when that Node reaches terminal state, while AddressSpace references
and execution pins keep backing alive. All CPUs subsequently restore the kernel
root and permanent bootstrap RSP0 before inspection views expire, Thread stacks
release their pins, retired object references are reaped and roots/frames are
destroyed. Completed stop origins clear so a reused generation is admissible.

### Live permissions use a bound remote invalidation protocol

`kernel/src/shootdown.rs` binds an epoch, root, page address and target mask.
Begin rejects malformed addresses/participants, overlap and generation
exhaustion. Each target acknowledges the current request once; commit requires
every target. A terminal failure prevents subsequent commit or reclamation.

The hardware adapter prevalidates the actual alias PTE, registry owner and
coordinator request, then atomically clears its W bit. An ordered publication
releases the epoch/root/address/targets. Each CPU executes INVLPG when using
that root or reloads its actual CR3 otherwise, then publishes its ACK. The
BSP commits after every target's actual instruction has completed. Timeout
fails stop with the existing backing retained.

The proof uses an actual warmed DATA alias at `0x410000`, followed by a denied
A2 store at `0x410210`. QMP observes each target after its invalidation
instruction and before its ACK publication. This protocol establishes the
ordering needed for later mapping teardown and K10 authority invalidation;
it introduces no public mapping or revocation ABI.

## Verification and acceptance

The [K9 completion contract](../K9_COMPLETION.md) requires the host suite,
every-feature lint, default build and full legacy/new QEMU matrix to pass.
The [scheduled SMP QMP protocol](../testing/smp-qmp.md#scheduled-smp-evidence-phases-9c-2-9c-3-and-9d)
specifies twelve execution profiles and independent checks of actual CPU
registers, normalized frames, raw queues/owners, authority and wait pins,
PTEs, invalidation instructions, fault quiescence and final allocator/table
conservation. Guest serial and diagnostic summaries alone are insufficient.

Acceptance passed on 2026-11-14: 306 Rust host tests, eight Python observer
regression tests, formatting/every-feature/host-library clippy, the default
image build and all 181 registered QEMU executions with the existing
30-second deadline. Five further stress, migration, idle/wakeup, four-CPU
shootdown and malformed-return runs passed using identical image hashes.
All twelve scheduled profiles restored the exact allocator bitmap and table
inventory, with zero final roots, pins, references, authority or waiters.

The source inventory is frozen in `build/k9-validation-source-manifest.json`.
All 78 source hashes and 181 tested image hashes were verified; the default
build matches the full matrix's boot image. The complete and repeated reports
are `build/k9-full-test-results.json` and `build/k9-repeat-results.json`;
`build/k9-validation-summary.json` records the compact audit and artifact
hashes. [Implementation status](../IMPLEMENTATION_STATUS.md#execution-profiles-and-independent-acceptance)
documents the preserved first failed attempt and the strict observer-only
timer-drain correction. This closes the bounded K9 completion contract.

## Consequences and remaining scope

Architecture v0.2 and experimental ABI 0.5 remain the baselines. Phases 9A,
9B and 9C-1 retain their historical profiles and scoped ADRs. The new scheduler
adds shared kernel transactions, per-CPU FIFO/affinity, migration, native/timer
preemption, blocking/idle wakeups, sibling quiescence and remote invalidation
within the fixed four-CPU, two-Node, three-Thread integer execution model.

Work stealing, load balancing, general kernel preemption, arbitrary AP frame
allocation, public mapping/Thread creation, TLS/FPU/SIMD scheduling and physical
hardware validation remain later work. K10 owns capability derivation and
revocation, rights-subset enforcement and concurrent mapping invalidation.

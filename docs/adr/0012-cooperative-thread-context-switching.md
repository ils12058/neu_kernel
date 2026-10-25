# ADR 0012: Cooperatively switch owned user contexts through a common kernel continuation

- **Date:** 2026-10-26
- **Status:** Accepted — K4 / Phase 4B, one CPU, two bootstrap Threads, IF=0.
- **Extends:** [0009](0009-experimental-native-syscall-and-trusted-return.md),
  [0010](0010-independent-bootstrap-nodes-and-fault-containment.md) and
  [0011](0011-local-timer-interrupt-foundation.md).
- **Related NEU decisions:** NEU-005/006 (Node/Thread distinction), NEU-008
  (unchanged ABI 0.1), NEU-011 (single CPU), NEU-017 / EXEC (embedded execution),
  NEU-018 / MEM (owned stacks/pinned roots), NEU-020 / SCHED (cooperative FIFO
  subset), NEU-021/023/024 (context validation, containment and local tests).
- **Architecture:** [Section 17](../Neu_Kernel_Architecture_v0.2.md#17-scheduling-clocks-and-cpu-topology--neu-sched-001).

## Context

Phase 3A starts each initial Thread once and returns only on termination. Its
shared syscall stack and global continuation cannot safely hold two suspended
Rust call chains. Phase 4A deliberately admits interrupts only in a separate
kernel test window. Reusing that timer for scheduling now would expose unresolved
kernel preemption and entry-state ownership.

We need real suspension and resumption of user execution, without introducing
arbitrary kernel-stack switching or claiming full preemptive scheduling.

## Decision

Use a bounded architecture-neutral FIFO containing generation-checked ThreadIds.
There is at most one Running Thread. Admission makes a configured Thread Runnable;
yield saves its context and appends it to the FIFO tail. Dispatch consumes the
front context. Reject duplicate/stale/terminal admission and dispatch; an empty
queue returns no work, and one runnable Thread may yield back to itself. Nodes
remain separately identified resource/authority domains. One initial Thread per
Node stays a bootstrap restriction, not a permanent Node/Thread equivalence.

Store a complete supported user context by value in its Thread: 15 GPRs,
RIP/RSP/RFLAGS, CS/SS, NodeId/ThreadId/SpaceId/root association and guarded kernel
stack binding. Initial and yielded contexts have distinct validity rules.
Validate ownership, canonical ranges, selectors, flags and the sealed RX resume
site before actual IRETQ. Keep assembly layouts/offsets asserted and tested.
Floating-point/vector state and TLS remain unavailable; retain MP/TS and zero
FS/GS. This is an x86-64 execution record, while FIFO selection is CPU-independent.

Allocate each Thread an **8 KiB** supervisor RW/NX kernel-entry stack from its
Node's existing owned-frame scope, with absent 4 KiB guards below/above. A bounded
higher-half arena assigns distinct Thread slots; full identities bind ownership.
Map a stack only in its own private user root. The common kernel continuation
uses the permanent normal image stack, mapped in every root. Stack inspection and
initialization use owned-frame scratch views under the kernel root. No shared
kernel child tables or sibling stack aliases are introduced.

This uses four additional backing frames for two stacks. A's four user frames,
B's five and those four total thirteen data owners, within the existing sixteen.
Keep 64 table, 64 mapping, two live-space, two Node and two Thread bounds. Pin a
space for the entire queued/running/suspended context lifetime. Pins prevent
mapping removal, frame release, stack teardown and root destruction; release the
pin only after terminal context/queue ownership is cleared on the common stack.
Stack-construction rollback must return exact pre-attempt accounting while a
sibling's saved context remains pinned. Record a small bounded pin limit and
refuse overflow/underflow or stale identities.

Use the existing native entry mechanism, with a **kernel-owned selected-stack
pointer** only in switching configurations. Validate and publish it before user
entry; the early assembly still saves user RSP as a scalar and selects trusted
RSP before any push/call/user-memory access. Set and read back TSS.RSP0 to the
same Thread stack. Preserve separate #DF IST1 and native critical IST2 stacks.
The non-reentrant guard protects one active entry and is never reused for an
unfinished kernel frame.

A yield handler validates its copied user frame, prepares success outputs and
copies it into a bounded supervisor handoff record. It then abandons its kernel
call chain through the common continuation. Move RSP to the common image stack
**before** restoring kernel CR3, so the transition never uses an unmapped Thread
stack. No suspended Rust frame or reference remains on a Thread stack. Only then
restore MSRs, clear selected entry state, deactivate root metadata and update the
Thread model/FIFO. The outgoing saved context owns user register values, not a
pointer into abandoned stack storage.

The next dispatch validates its current identities, live root/mappings, stack
ownership and saved frame under the kernel root. With IF=0 publish native session,
selected stack and TSS.RSP0, activate the verified root, copy the saved 160-byte
frame onto its own stack and restore all registers through IRETQ. Each slice has
its own live common-stack call continuation, consumed before the next slice.
Never select a root, stack or Thread from an unchecked user argument.

Use **0xffffffffffffff00** as a test-only native yield operation, compiled only
for the switching tests. It returns RAX=0/RDX=0 after resumption, with the existing
register preservation/clobber convention. It is not a public operation-number
allocation. `abi_query=1`, `thread_exit=2`, ABI tag 0.1 and advertised feature bits
remain unchanged; earlier configurations still reject this private operation.
A future public yield requires a separate compatibility decision.

Normal exit terminates the current Thread and its Node only when the final live
association ends. Supported user faults terminate their own Node and discard its
saved state without invalidating a healthy queued context. Invalid saved return
state is rejected before activation/resumption and classified separately from a
hardware exception. Reclaim only after dequeue/terminal state, common-root
restoration, pin release and private stack teardown; then release the user root
and model associations. Preserve outcomes separately from resource lifetimes.

Keep **IF=0** throughout cooperative user execution, syscalls and switching.
Phase 4A timer tests remain independent. No timer callback changes FIFO or active
Thread state. No general kernel preemption, scheduler lock, wait queue, SMP,
public factory/capability, ELF or IPC mechanism is added.

## Alternatives considered

- **Restart A after running B.** Does not preserve the suspended instruction,
  user stack or register context, so it cannot establish context switching.
- **Suspend Rust calls on the shared native stack.** The next syscall can
  overwrite a live frame and the global continuation; separate user records alone
  do not repair that lifetime bug.
- **Save and switch arbitrary kernel call chains.** Requires a larger kernel
  context/locking/preemption contract. Completing the kernel boundary first keeps
  the supported switch point explicit and eliminates dormant Rust references.
- **Static reserved stacks or larger metadata pools.** Per-Thread allocator-owned
  stacks exercise real construction/reclamation. Two pages per bounded handler
  fit current pools; guards and high-water checks constrain the tested footprint.
- **Timer-driven preemption now.** Native early entry and interrupted Ring 0
  frames need a separate safe deferred-switch contract before enabling it.

## Consequences

Real yielded Threads can alternate and resume after their saved syscall sites
with private user stacks and distinct roots. Kernel stack reuse becomes local to
one Thread and no stack must retain a suspended kernel continuation. Conservative
MSR/root transitions favor auditability over performance. Fixed queues, sealed
payload sites and small stacks bound this experiment; no fairness or CPU-time
isolation is promised against a Thread that never yields.

## Phase 4C prerequisites

Before timer-driven user preemption, extend context capture to actual interrupt
frames, define IRQ acknowledgment/deferred `need_resched` ownership, and admit
IF=1 only where the native entry and return protocol supports it. Ring 0 timer
entry may request later work but must not switch a suspended Rust frame. Define
atomic active identity/root/TSS publication, nested critical-entry behavior,
preemption-disable regions and per-CPU state before widening the enabled surface.
Keep x87/SIMD policy explicit; multiple Threads per Node, SMP and public scheduling
operations require their own decisions. Phase 4B does not accept the full
preemptive scheduling proposal.

## History

- **2026-10-26:** Accepted for the explicit Phase 4B request. Previous rationale
  and architecture v0.2 remain intact. Actual commands, CPU evidence and limits
  belong in [implementation status](../IMPLEMENTATION_STATUS.md).

References: [Intel SDM](https://cdrdv2-public.intel.com/819714/253668-sdm-vol-3a.pdf)
(privilege transitions/TSS/IRET) and the [freestanding Rust target](https://doc.rust-lang.org/rustc/platform-support/x86_64-unknown-none.html)
(integer-only code generation, SysV without a red zone).

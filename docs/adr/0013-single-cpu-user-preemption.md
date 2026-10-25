# ADR 0013: Preempt user contexts through an acknowledged IRQ handoff

- **Date:** 2026-10-26
- **Status:** Accepted — K4 / Phase 4C, one CPU, two bootstrap Nodes, integer-only userspace preemption.
- **Extends:** [0011](0011-local-timer-interrupt-foundation.md), [0012](0012-cooperative-thread-context-switching.md), [0009](0009-experimental-native-syscall-and-trusted-return.md) and [0010](0010-independent-bootstrap-nodes-and-fault-containment.md).
- **Related NEU decisions:** NEU-003/011; NEU-005/006/008; NEU-018 owned memory subset; NEU-020 single-CPU round robin subset; NEU-021/023/024 scoped safety, containment and local tests.
- **Architecture:** [Section 17](../Neu_Kernel_Architecture_v0.2.md#17-scheduling-clocks-and-cpu-topology--neu-sched-001). Full kernel preemption, SMP and public object/scheduler APIs remain outside this decision.

## Context

Cooperative FIFO selection cannot make a non-yielding Thread share the CPU.
Phase 4A's IRQ entry preserves registers but validates only a bounded Ring 0
window. Phase 4B completes each kernel slice before selection, avoiding dormant
Rust frames on private stacks. Enabling IF without extending those contracts
would permit wrong-stack entry, invalid saved flags and asynchronous mutation
through suspended exclusive references.

## Decision

Retain the ThreadId FIFO and private guarded stacks/pins. Introduce an explicit
preemptible execution mode and distinguish initial, native-yield and hardware
interrupt contexts. An interrupt record preserves all fifteen GPRs exactly,
including RAX, RDX, RCX and R11, plus the real RIP/RSP/RFLAGS/CS/SS. Native-return
validation remains sealed to syscall continuation sites; interrupt resumption
accepts validated positions inside the live owned RX payload. Only the explicitly
preemptible mode permits and requires IF. IOPL, DF, TF, NT, VM, AC and reserved flags remain rejected. Preserve hardware
RF in an interrupt-origin record only; native returns retain their stricter RF
policy. QEMU supplies RF in actual external-interrupt frames, so treating such
frames as syscall frames would incorrectly reject valid interrupts. ABI 0.1 numbers, feature information and statuses stay
unchanged; earlier images retain IF=0.

IRQ assembly saves fifteen GPRs, vector/error and the five hardware words: 176
bytes, RIP offset 136. Ring 3 interrupt gates clear IF and choose TSS.RSP0. Validate
origin, current identity/root, exact private stack, TSS.RSP0, native entry guard,
selectors, flags and immutable user ranges without dereferencing user pointers.
Require a real APIC timer ISR bit; send one EOI and verify it clears before any
rescheduling handoff. Spurious vectors send no EOI; unexpected/software events
cannot become scheduling evidence.

Publish one bounded CPU-owned handoff slot with an exact active binding and
sequence. The IRQ copies values into it, never accesses the scheduler, Nodes,
paging manager or allocator, and abandons its call chain through the existing
common-stack continuation. Consume that slot once, with IF=0 on the common root,
before updating the FIFO. Duplicate, stale, inconsistent or partially published
handoffs fail closed. No ISR borrow crosses the handoff; no Thread retains a
Rust ISR or syscall frame. Kernel-owned outcomes are separate from historical
diagnostic values.

Use a calibrated **provisional 10 ms one-shot** LAPIC clockevent. Re-arm at the
final assembly-to-user boundary, after slow validation and test observations,
so kernel setup does not consume a newly selected slice. This separates hardware
event counts, scheduler selections and observed computation progress. It is not
a clocksource or accurate CPU-time accounting. With no competitor, acknowledge
and resume the same interrupt frame without a FIFO/common-stack switch.

All normal Ring 0 paths remain IF=0, including native entry through FMASK. A timer
that expires during a syscall stays pending until validated IRETQ opens the user
window; the IRQ then captures Ring 3, never an arbitrary Rust frame. No general
Ring 0 rescheduling is added. The existing bounded kernel-only drain window may
acknowledge pending events without switching; other enabled kernel origins fail
closed. Before teardown mask/stop, drain on the common stack, verify ISR/IRR empty
and stable counts, then disable APIC software delivery. Never free a root or stack
before its handoff and native session end and its lifetime pin is released.

Publish native binding, selected stack, TSS.RSP0 and verified CR3 with IF=0 before
arming and IRETQ. Restore the common stack before changing away from a private
root; re-establish kernel SS/DS/ES after the long-mode IRQ null-SS entry before
resuming ordinary kernel Rust/drain execution. Interrupt code touches only copied session values, private frames and the
controller. xAPIC requires its verified supervisor UC mapping under each private
root; these mappings own private tables, never borrow a sibling hierarchy.
NMI/#MC IST2 and #DF IST1 retain their separate fatal policy. Keep zero FS/GS and
MP/TS restrictions; do not expose unsaved x87/SIMD/TLS state.

## Alternatives considered

- **Call the scheduler inside an arbitrary IRQ.** Overlaps mutable controller,
  paging and object borrows and risks suspending an unsafe Rust kernel frame.
- **Turn interrupts into synthetic yields.** Loses real interrupted RIP and
  changes registers that a hardware interrupt must preserve.
- **Save arbitrary kernel call chains.** Requires locking, cancellation and
  kernel-preemption contracts beyond this milestone.
- **Periodic tick with slice counting.** Valid future alternative; a one-shot at
  the final user boundary avoids charging slow kernel setup to the next Thread
  and limits pending work during disabled intervals.
- **Enable IF inside native dispatch.** Expands early-entry, nesting and borrow
  hazards without being needed for user-only preemption.

## Consequences

Non-yielding Threads can make independent progress and resume actual interrupted
instructions. Conservative per-slice validation/MSR transitions and bounded
handoffs favor auditability over throughput. Calibration, QEMU pauses, handler
work and entry overhead prevent claims of exact milliseconds or hard real time.
One CPU, one initial Thread per Node, fixed pools, sealed bootstrap code and no
extended state remain explicit limitations. Wait queues, SMP, public controls and
general kernel preemption require separate decisions.

## History

- **2026-10-26:** Chosen under the Phase 4C request before implementation. Existing
  ADR rationale and architecture v0.2 are preserved. Implementation details,
  actual acceptance results and unverified cases belong in
  [implementation status](../IMPLEMENTATION_STATUS.md).

References: [Intel system programming](https://cdrdv2-public.intel.com/819714/253668-sdm-vol-3a.pdf),
[freestanding Rust target](https://doc.rust-lang.org/rustc/platform-support/x86_64-unknown-none.html).

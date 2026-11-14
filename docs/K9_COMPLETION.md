# K9 completion contract

Started 2026-11-14 from `46ef221` plus the existing, locally validated Phase
9C-1 changes. Those changes are preserved in `build/k9-starting-state/`.

The completed K9 work implements the initial SMP scheduler in architecture
v0.2 sections 11, 17 and 22. The target stays x86-64/QEMU, at most four CPUs,
two Nodes and three Threads, integer-only execution, and experimental ABI 0.5.
Kernel transactions run with local interrupts disabled. General kernel
preemption and K10 authority derivation/revocation remain separate milestones.

## Required completion evidence

1. Reusable CPU-local user entry/exit preserves complete saved contexts across
   timer preemption, native syscall return, yield, blocking and termination.
   Two and four CPU configurations execute scheduled Ring 3 Threads; two
   Threads from one Node can run simultaneously without duplicate dispatch.
2. Every CPU selects work through a synchronized scheduling policy. Runnable
   queues, wakeups, migration and idle/IPI transitions preserve one owner per
   Thread. CPU roles are not encoded in Node or Thread identities.
3. Shared lifecycle/capability/Channel transactions have explicit lock ownership
   and ordering. No mutable registry borrow or lock survives user entry or an
   interrupt wait; IRQ handlers never acquire the scheduler lock.
4. A Node fault stops running siblings before capability, stack, root or frame
   reclamation. Unrelated Nodes keep making progress. Duplicate, stale and
   unauthorized transitions are rejected without partial mutation.
5. A real shared-address-space permission change executes a generation-checked
   remote TLB invalidation protocol. Every target CPU acknowledges actual local
   invalidation or a verified root switch before backing can be reclaimed.
6. Host concurrency/state-machine tests and stopped-CPU QMP observations check
   production queues, ownership, CPU registers, frames, PTEs, IRQ/IPI records
   and final allocator/table inventories. Existing assertions and timeouts stay
   in force. The full legacy/new validation matrix must pass before K9 is
   recorded as complete.

## Completed checkpoints — accepted 2026-11-14

| Checkpoint | Status | Verified execution |
| --- | --- | --- |
| 9C-2 | Complete | Synchronized lifecycle/FIFO queues, reusable native/timer CPU bridge, preemptive two/four-CPU scheduling and concurrent shared-Node execution |
| 9C-3 | Complete | Saved-context migration, blocking/idle wakeups, capability/Channel transactions, Node-fault sibling quiescence and malformed-return containment |
| 9D | Complete | Real remote TLB invalidation, inert rejection/stress profiles, full regression and accepted-scope documentation |

These are internal checkpoints of this completion task. Together with the
retained 9A, 9B and 9C-1 profiles, they complete **K9 / Phase 9 for the bounded
SMP scope above**.

## Acceptance evidence

Validation on local branch `codex/k9-smp-completion` passed:

- 306 Rust host tests and eight Python observer regression tests.
- All 181 registered QEMU executions, including the twelve new scheduled SMP
  profiles, with the unchanged 30-second per-case deadline.
- Five additional runs of stress, migration, blocked idle/wakeup, four-CPU TLB
  invalidation and malformed-return rejection, using identical kernel images.
- Formatting, every-feature and host-library clippy, and the default image
  build. The default kernel SHA-256 matches the full regression's boot case.
- All 78 frozen source hashes and all 181 tested kernel image hashes.
  Every scheduled SMP run restored the exact allocator bitmap and table
  inventory, with zero final user roots, pins, references, authority or waiters.

The complete report is `build/k9-full-test-results.json`; repeated-run evidence
is `build/k9-repeat-results.json`. The compact audit and artifact hashes are
in `build/k9-validation-summary.json`, with source hashes in
`build/k9-validation-source-manifest.json`. Full-run and repeated SMP
captures are retained separately in `build/k9-full-test-evidence/` and
`build/k9-repeat-evidence/`.

The first full attempt and its failed preemption capture remain preserved.
[Implementation status](IMPLEMENTATION_STATUS.md#execution-profiles-and-independent-acceptance)
records the strictly bounded user/kernel timer-accounting correction and the
successful replacement run. K10 derivation/revocation is the next milestone.

## Successor — 2026-11-14

[K10 advanced authority](K10_COMPLETION.md) is now complete for its bounded
x86-64/QEMU scope, extending K9 with explicit derivation, strict descendant
revocation and tracked mapping retirement. This document retains K9's original
acceptance counts and ABI 0.5 scope. K11 device primitives are now next.

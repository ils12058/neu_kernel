# ADR 0027: Cross-CPU work, mailboxes and verified IPI coordination

- Date: 2026-11-01
- Status: Accepted for the bounded K9 / Phase 9B scope (2026-11-01)
- Related decisions: NEU-003, NEU-004, NEU-005, NEU-011, NEU-018, NEU-020,
  NEU-021, NEU-023, NEU-024
- Architecture:
  [scheduling, SMP and asymmetric cores](../Neu_Kernel_Architecture_v0.2.md#11-scheduling-smp-and-deferred-asymmetric-cores),
  [scheduling, clocks and CPU topology](../Neu_Kernel_Architecture_v0.2.md#17-scheduling-clocks-and-cpu-topology),
  [verification and CI](../Neu_Kernel_Architecture_v0.2.md#21-verification-fault-injection-and-ci),
  [roadmap and acceptance tests](../Neu_Kernel_Architecture_v0.2.md#22-development-roadmap-and-acceptance-tests)
- Predecessors: [ADR 0011](0011-local-timer-interrupt-foundation.md),
  [ADR 0013](0013-single-cpu-user-preemption.md),
  [ADR 0026](0026-smp-bootstrap-and-percpu-foundations.md)

## Context

Phase 9A stopped one step short of concurrency. Application processors entered
real kernel code, established a trustworthy GS-based identity, loaded private
descriptor tables and parked with interrupts disabled; every existing kernel
subsystem stayed BSP-owned and no AP executed work. The architecture document
describes SMP scheduling as future work, and ADR 0026 explicitly deferred
"concurrent Ring 3 scheduling, shared or per-CPU run queues, Thread migration,
load balancing, AP kernel preemption, remote TLB shootdown, concurrent frame
allocation, concurrent capability mutation, general kernel preemption, IPIs and
a per-CPU timer policy" to K9B/K9C.

Phase 9B asks a narrower question than "run the scheduler on many CPUs":

> Can the BSP hand a bounded, well-formed command to a specific AP, have that
> AP execute it exactly once with an independently reproducible result, observe
> the completion without trusting a guest scalar, and coordinate two CPUs
> through a real barrier — while every existing single-CPU registry remains
> untouched by AP code?

The phase also requires a real AP wakeup protocol. An AP that parks with
`IF=0` cannot be woken by a maskable interrupt; a normal IPI is only a valid
wakeup if the AP deliberately publishes an idle state, opens one documented
interrupt window, and resumes at the instruction the sender expects.

## Decision

### Ownership model

The protocol keeps the Phase 9A asymmetry:

- the BSP is the only publisher of work commands;
- each AP is the only executor of its own mailbox;
- the BSP never writes result, completion, execution or interrupt-accounting
  fields, and the AP never writes the BSP's command fields;
- the fixed `neu_smp_cpus` blocks remain AP-owned publications, and the BSP
  only reads them.

No existing subsystem gained an AP caller. The frame allocator, paging
registry and current root, scheduler FIFO and Thread contexts, Node, Thread
and capability registries, Channel state, MemoryObject mapping mutators,
preemption mailbox, timer policy and the COM1/panic/fault path are still
BSP-only. No global was converted to `Sync` and no broad lock was introduced.

### Mailbox representation

`neu_smp_work_mailboxes` is a fixed array of `MAX_CPUS` (4) 64-byte-aligned,
256-byte mailboxes of 32 documented `u64` fields. `neu_smp_work_layout`
publishes `[ABI, magic, MAX_CPUS, fields, stride, gate words, gate stride,
journal slots, journal fields]` so an independent observer decodes the
production arrays without trusting a guest scalar. A mailbox carries:

- identity: magic `NEU_WKE1`, logical index and APIC ID, published by the AP;
- protocol: phase, state, sequence, acknowledged sequence;
- command: kind, input, rounds, publication order;
- acceptance: accept sequence/order and accepting logical/APIC;
- completion: completion sequence/logical/APIC, start marker, marker, result,
  result auxiliary word, execution count and execution mask;
- accounting: failure code, rejection count, last rejection code, IPI receive
  count, EOI count, spurious count.

Command kinds are `Transform` (LCG/xorshift mix with an auxiliary word),
`Checksum` (FNV-1a with a length-derived auxiliary word), `Stop` (framework
validation only) and `FaultProbe` (rejected by validation). Rounds are bounded
to `1..=4096`; zero rounds or an unknown kind is rejected before publication.

### Sequence identities and ordering

Sequences start at 1 and advance by exactly one per successful publication.
`publisher.publish` requires an online, idle mailbox with no unacknowledged
command, writes every command field, then publishes `SEQUENCE` and the
`Published` state with Release stores. `executor.accept` reads them with
Acquire semantics, requires `ACCEPT_ORDER + 1 == SEQUENCE` and an incomplete
sequence, then records the accepting identity, the start marker
(`mix(sequence ^ (logical << 32) ^ apic)`) and the live RSP before publishing
`Executing`. `complete` requires `COMPLETE_ORDER + 1 == SEQUENCE`, records the
completion identity, result and marker, sets the bit in the execution mask and
publishes `Completed`. `acknowledge` is monotonic and rejects stale values.
The AP retires an acknowledged command only after the BSP observes completion.

Exactly-once is structural: an unacknowledged command cannot be overwritten
(`Busy`), a replayed accept fails on the order check, a replayed completion
fails on state/order, and the execution mask must always equal
`(1 << sequence) - 1`. A sequence of `u64::MAX` is rejected as
`SequenceOverflow` before wraparound.

Invalid commands are not silently applied. A rejected publish leaves the
previous idempotent fields untouched, counts a rejection and stores the error
code; the AP terminalizes its own mailbox on a protocol violation.

### Two-phase barrier

For the two-CPU overlap profile the BSP publishes a fixed gate of an 8-word
header and four 4-word participant entries:
`{magic, epoch, participants, phase, release, overlap, stop, failure}` plus
per-CPU `{entered, ready, done, reserved}`. Phases are `Closed → Open →
Checkpoint → Released → Complete`.

The gate is not a work queue. The AP arrives after accepting the same command
the BSP also executes, publishes `entered/ready`, and spins inside an exported
assembly loop on the Acquire `RELEASE` word. The BSP waits for both
participants, logs the deterministic checkpoint, dwells in its own exported
loop, verifies the gate is still `Checkpoint`, then publishes the epoch. Both
CPUs execute the identical transform and must produce the same result before
departing; the overlap witness is the mask of CPUs that entered concurrently,
and the final state requires every participant's `done` word.

Overlap is proven by stopped-CPU state, not by timing: at the checkpoint QMP
observes the BSP inside `neu_smp_work_bsp_dwell_begin..end`, the AP inside
`neu_smp_work_gate_wait_begin..end`, the gate still `Checkpoint` with
`RELEASE = 0` and `OVERLAP = 0b11`, and both mailboxes `Executing`.

### AP wakeup: Stage A polling

Stage A is the always-available fallback. The AP spins in `executor.accept`
with `pause`, a bounded counter and no interrupt enable. This proves the
mailbox protocol and the execution path on any configuration, including
`-smp 4`, without touching the local APIC.

### AP wakeup: Stage B xAPIC IPI

Stage B is implemented and verified. A normal maskable IPI is only used as a
wakeup because the AP publishes a documented idle window:

1. The AP records the current IPI-received counter, publishes `IpiIdle` with a
   Release store, and executes the exported `sti; hlt; neu_smp_work_idle_after:
   cli; ret` sequence. The STI shadow guarantees that an interrupt is recognized
   only after `hlt`, so the interrupted RIP is exactly
   `neu_smp_work_idle_after`.
2. The BSP waits for `IpiIdle`, then sends fixed-delivery vector `0xF0` to the
   target's APIC ID through the owned-root LAPIC window, checking the ICR
   delivery-status bit and requiring the controller to become idle.
3. The AP's private IDT has entries for `0xF0` (work) and `0xFF` (spurious),
   both Ring 0 interrupt gates using IST1. The stub saves the documented
   exception frame, calls Rust, clears IF in the saved RFLAGS and returns.
4. The handler verifies vector and in-service state, CS = 8, IF = 1, RIP =
   `neu_smp_work_idle_after`, RSP inside the AP's own stack, the saved identity
   and phase `IpiIdle`. It EOIs exactly once, verifies the in-service bit
   clears, and only then increments the IPI and EOI counters and publishes
   `IpiBusy`.
5. The spurious vector is counted and never EOI'd. An unexpected vector, an
   unexpected in-service classification or any identity mismatch is terminal
   for that CPU.

The BSP only sends after the AP publishes `IpiIdle`, never to a CPU that has
not reached the required state. No timer, MSI or x2APIC mechanism is used. The
AP re-enters `executor.accept` only after the IPI accounting proves the wakeup.

### Failure semantics

Every wait is bounded. A missed wakeup, delivery failure, in-service timeout,
mailbox protocol error, identity mismatch, unexpected vector, result mismatch
or journal inconsistency is a distinct error. On the AP, an error terminalizes
the mailbox (`Failed` phase, failure code, `Failed` bootstrap state with a
`FAILURE_WORK_BASE + code` record) and parks silently; the BSP never continues
as if that CPU were healthy. On the BSP, the driver records the failure in the
evidence words and halts through the existing boot-failure path. Inconsistent
shared state is never repaired or reused.

### Memory restrictions

AP code touches only its own `CpuBlock`, its own mailbox and its own journal
slots; it never allocates frames, mutates page tables, enters the scheduler,
touches capabilities/Channels/MemoryObjects, or writes serial. The BSP writes
only command/acknowledgement fields and the gate; it never fabricates result,
completion or interrupt-accounting fields. Each AP additionally appends every
command to an AP-written journal (four slots of ten words: sequence, kind,
input, rounds, result, auxiliary, start marker, completion marker, accept
order, complete order) so the QMP oracle can recompute every result and order
independently of the final mailbox contents.

### Evidence contract

Seven profiles are registered and executed individually:

| Case | vCPUs | Proves |
| --- | --- | --- |
| `smp-work-2` | 2 | Two different commands accepted, executed and stopped exactly once by one AP |
| `smp-work-4` | 4 | Three APs concurrently execute different commands without mailbox aliasing |
| `smp-work-overlap` | 2 | BSP and AP execute identical work across the two-phase gate; overlap frozen at the checkpoint |
| `smp-work-repeat` | 2 | Three sequential commands with independent ordering and results |
| `smp-work-invalid` | 2 | Duplicate, invalid, identity, stale-sequence, stale-ack and replayed-poll rejections |
| `smp-ipi-2` | 2 | Stage B wakeup for every command; counters, EOIs and zero spurious |
| `smp-ipi-4` | 4 | Stage B wakeup for three APs at once |

`tools/smp_qmp.py` stops the whole VM (requiring `paused`), resolves QMP's own
CPU set and per-CPU `apic-id` through QOM, and reads production memory: the
layout witness, every mailbox, every journal slot, the evidence words and the
gate. For the parallel and overlap profiles it also stops the VM at the
published deterministic checkpoint and verifies both CPUs' live RIP/RSP/CR3/GS,
that the BSP and every AP are inside their exported work range, that every
mailbox is `Executing` with the expected sequence and start marker and no
completion fields, and that the gate is still `Checkpoint`. It rejects
duplicate executions, holes or replays in the execution mask, foreign
accept/completion identities, non-reproducible results or markers, wrong
ordering, a failed or rejected mailbox, IPI accounting mismatches and any
unexpected interrupt. Final state additionally reuses the Phase 9A checks:
parked APs, bootstrap history, private stacks and descriptor tables, canaries,
live registers and masked LAPIC timers.

A printed result or a scalar `parallel=true` is never accepted as evidence.

## Alternatives considered

- **A shared lock or `Sync` conversion around the mailbox**: would hide rather
  than define ownership and risks unsynchronized legacy state. Rejected; the
  protocol uses per-word Release/Acquire publication with a single writer per
  field group.
- **Overwriting a busy mailbox to simplify the BSP**: loses exactly-once and
  makes lost work indistinguishable from success. Rejected; a busy mailbox
  fails with `Busy`.
- **A per-AP work queue or per-CPU run queue**: implies scheduler integration
  and capacity/back-pressure policy not in scope. Rejected; fixed one-slot
  mailboxes with explicit sequence identities.
- **Timing-based overlap** (`sleep`, iteration counts, wall-clock comparison):
  not reproducible evidence and easily sequentialized. Rejected; overlap is a
  two-phase barrier with a released epoch and a stopped-CPU checkpoint.
- **Claiming IPI support from an ICR write or a mailbox-only test**: does not
  prove delivery, vector entry, EOI or wakeup. Rejected; Stage B requires the
  AP-side handler accounting and QMP state.
- **Waking a halted `IF=0` AP with a normal maskable IPI**: architecturally
  impossible. Rejected; the AP publishes `IpiIdle` and opens one documented
  `sti; hlt` window with the interrupted RIP verified by the handler.
- **Using the local-APIC timer as the wakeup**: introduces periodic policy and
  a tick the phase does not own. Rejected; the timer stays masked on every CPU.
- **x2APIC or MSI wakeups**: different addressing and no Phase 9B consumer.
  Rejected; xAPIC only.
- **Letting APs run existing Node Threads or mutate registries**: would race
  with BSP-only state. Rejected; APs execute only bounded mailbox commands and
  barrier participation.
- **Fabricating completion values on the BSP for diagnostics**: would make the
  ownership proof unfalsifiable. Rejected; results come only from the AP
  mailbox and journal.
- **Reporting the phase complete without Stage B**: allowed by the prompt as a
  fallback, but Stage B was completed, proven and is not deferred.

## Consequences

- Neu now dispatches real work to real APs with exactly-once execution,
  reproducible results and independent stopped-CPU verification, on genuine
  `-smp 2` and `-smp 4` VMs. The two-CPU overlap profile proves genuine
  concurrent execution against a barrier rather than a sequential handoff.
- Work IPIs are implemented and verified for both `-smp 2` and `-smp 4`, with
  a per-AP IDT, IST1 handler, documented idle window, one EOI per IPI and no
  spurious or unexpected interrupts.
- Every existing single-CPU registry remains BSP-owned. This remains the
  phase's central limitation: no Thread runs on an AP, there is no per-CPU run
  queue, no migration, no load balancing, no AP preemption and no remote TLB
  shootdown. The gate is a test barrier, not a scheduling primitive.
- Still deferred: K9C scheduler integration, AP execution of existing Node
  Threads, concurrent capability/frame/page-table mutation, remote TLB
  shootdown, general kernel preemption and K10 derivation/revocation.
- Architecture v0.2, experimental ABI 0.5 and ADRs 0005–0026 remain in force.
  No public ABI, syscall, pool constant or legacy assertion changed.
- Host tests cover the mailbox state machine, publication/acknowledgement
  ordering, invalidity, duplicate, busy, exhaustion, identity, terminalization
  and barrier transitions. They do not execute x86 SMP code; multi-vCPU QEMU
  runs remain the only execution evidence.

## History

- 2026-11-01: Accepted for bounded Phase 9B after the full validation matrix:
  275 host tests, 163/163 QEMU scenario executions (161 unique) with the
  unchanged 30-second timeout under `/usr/sbin/taskpolicy -t 4`, individual
  repeats of all seven Phase 9B profiles plus `smp-bootstrap-4` and
  `memory-object-runtime`, and a clean-target rebuild from `cargo clean` that
  repeated fmt, every-feature clippy, the default image build and the host
  suite. Rejected attempts during bring-up: the invalid-profile identity
  negative check inverted `Option::ok_or` into a success (fixed by using the
  rejection itself as the error, not weakening the check); the xAPIC in-service
  lookup used a packed 4-byte stride instead of the architectural 16-byte
  stride, so a delivered `0xF0` was classified as unexpected (fixed in the
  production helper); the IPI collector waited for `IpiIdle` after the `Stop`
  command, whose terminal phase is `Stopped` (fixed by scoping the idle wait to
  work commands); and the gate object was linker-garbage-collected on profiles
  that never reference it (fixed with a `#[used]` publication). No timeout,
  assertion, priority or historical test was weakened.

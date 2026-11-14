# ADR 0028: Static two-CPU Ring 3 execution with verified ownership

- Date: 2026-11-03
- Status: Accepted for the bounded K9 / Phase 9C-1 scope (2026-11-03)
- Related decisions: NEU-003, NEU-004, NEU-005, NEU-011, NEU-013, NEU-017,
  NEU-018, NEU-020, NEU-021, NEU-023, NEU-024
- Architecture:
  [scheduling, SMP and asymmetric cores](../Neu_Kernel_Architecture_v0.2.md#11-scheduling-smp-and-deferred-asymmetric-cores),
  [verification and CI](../Neu_Kernel_Architecture_v0.2.md#21-verification-fault-injection-and-ci),
  [roadmap and acceptance tests](../Neu_Kernel_Architecture_v0.2.md#22-development-roadmap-and-acceptance-tests)
- Predecessors: [ADR 0013](0013-single-cpu-user-preemption.md),
  [ADR 0025](0025-runtime-memoryobject-mapping.md),
  [ADR 0026](0026-smp-bootstrap-and-percpu-foundations.md),
  [ADR 0027](0027-smp-crosscpu-work-and-coordination.md)

## Context

Phases 9A and 9B brought application processors into kernel code and gave them
bounded mailbox work with verified IPIs, but both left every scheduling and
address-space subsystem BSP-owned. No Node Thread had executed on an AP, and no
two Threads had been in Ring 3 at the same time. ADR 0027 explicitly deferred
"AP execution of existing Node Threads" and every concurrent registry
mutation to K9C.

Phase 9C-1 asks a narrower question than a preemptive multi-CPU scheduler:

> Can two distinct Node Threads, in distinct address spaces, execute real Ring 3
> code on the BSP and one AP at the same time — with each CPU's privileged
> state, root, stack and identity independently verified, an AP-side fault
> contained without disturbing its peer, every lifecycle transition recorded
> exactly once, and final cleanup returning the machine to its pre-execution
> baseline?

The phase must not be satisfied by sequential entry, by both CPUs doing kernel
work, or by AP execution that depends on unsynchronized BSP-owned mutable
state.

## Decision

### Static assignment is fixed before release

Placement is decided by the BSP before either CPU leaves Ring 0:

- logical CPU 0 runs Node A / Thread A1 in AddressSpace A;
- logical CPU 1 runs Node B / Thread B1 in AddressSpace B.

The two Nodes, Threads, spaces and roots must be distinct; a Thread may occupy
one CPU slot; a Node, space or root may not be placed in two slots; the two
kernel entry arenas must be page-aligned, non-empty and disjoint. `Session::ready`
requires both roles plus a bound barrier page, so a one-CPU configuration can
never start executing. `release` requires both placements; `enter` is
one-shot per CPU and only from `Released`; `exit`, `complete` and
`acknowledge` are one-shot and ordered; `acknowledge` requires both CPUs to have
completed; `reap` requires acknowledgement. The phases are `Prepared →
Released → Entered → Exited → Completed → Acknowledged → Reaped`.

The production model lives in `kernel/src/smp_user.rs` and contains no CPU
instructions; host tests exercise every rejection above. The x86-64 controller
uses the same transitions around the hardware entry and exit, so the model and
the machine cannot disagree about which CPU owns which terminal outcome.

### BSP-owned allocation, per-CPU consumption

The BSP prepares both private roots with the existing
`Paging::prepare_domain_shared`, builds one Node and one initial Thread per
space with the existing `Nodes` transitions, installs both `SavedContext`s,
and dispatches each Thread to its own CPU with `Nodes::start_on`. It is the
only allocator and the only mutator of Node, Thread, paging, capability and
serial state.

Each CPU consumes only its own published descriptor. The AP path never reaches
`Nodes`, `Paging`, the paging registry, the capability registry or the serial
device; it waits on its own phase word, validates its own identity, enters
Ring 3, publishes its raw exit in its own CPU-local block, and parks. No
`&mut` alias crosses a CPU boundary and no broad lock was introduced.

The BSP publishes one 96-word, 64-byte-aligned block per CPU at
`neu_smp_user_blocks + logical * neu_smp_user_layout[3]`. The layout witness is
`[magic, MAX_CPUS, fields, stride]`. A block carries the CPU and APIC identity,
role, Node/Thread/space identities with the Thread generation, kernel and user
roots, entry and saved Ring 3 context, the model stack binding, the image-
reserved kernel entry arena and RSP0, the CPU's own TSS/GDT/IDT bases, the
barrier virtual and physical addresses, the user frame virtual/physical
identities, the exact expected exit tuple (vector, error, RIP, RSP, CR2,
registers), the allocator/table baseline and the phase. The BSP alone sets
`SESSION_BARRIER`; each CPU alone sets its `ENTRY_COUNT`, `EXIT_*`, `FAULT_*`,
`RESTORED_*` and `HANDLER_RSP` words.

### Ring 3 entry, exit and CPU identity

`neu_smp_user_enter` saves the owning CPU's kernel continuation, root, CR0,
EFER and GS base in that CPU's bridge record, disables SYSCALL/SYSENTER and
FPU/SIMD exposure, loads the private user root, publishes the user selectors
and `iretq`s to CPL3 with IF clear. No SWAPGS is used: the entry path clears
`IA32_GS_BASE`, and every CPL3 exit re-derives the logical identity from CPUID
before touching any block.

The CPL3 exit path (`smp_user::boundary` in the x86-64 layer) is entered before
the fatal-fault guard. It validates, against the descriptor the BSP published
for that CPU: the kernel CS in force after the privilege transition, the user
CS/SS, the expected vector, error code, RIP, RSP and CR2, the RFLAGS policy
(bit 1 set; IF, DF, IOPL, TF, NT, AC clear), the handler RSP inside the CPU's
own arena, the root observed on exit equal to the published user root, and the
expected general registers. Only then does it record the raw exit, mark
`CHECK_EXIT` and publish `PHASE_EXITED`; otherwise it records a failure code.
It then restores the CPU's kernel root, CR0, EFER and GS base from its own
bridge and returns to the suspended Rust continuation.

A CPU whose descriptor, identity, root, arena or exit tuple does not match is
terminal for that CPU: it publishes a failure and parks. The peer's evidence is
never treated as a substitute.

### Concurrency evidence

Both roots alias one image-owned barrier page read/write at user level. The
fixture publishes a per-role bit in stage A, waits for both bits, dwells in a
bounded loop, and synchronizes again in stage B. Role 0 then publishes a
survivor word into the shared page; role 1 observes it after its own release and
records the observation in its private DATA frame.

Concurrency is proven by stopped-CPU state, not by a scalar:

- the QMP oracle stops the whole VM at the published prepared window with both
  CPUs at CPL0 and no Ring 3 entry;
- it stops the VM again while both CPUs are inside the exported Ring 3 payload,
  requiring CPL3, user CS/SS, RFLAGS policy, user CR3 (distinct roots), RIP
  inside the copied fixture span for both CPUs, and the stage-A barrier word
  equal to `0b11`;
- the final barrier word and both DATA frames must show the survivor
  publication seen from the other address space, and the overlap profile
  requires the per-CPU execution intervals to intersect.

A guest `both_running=true` or a serial line is never evidence.

### Fault containment and peer survival

The AP-fault profile makes role 1 store to the kernel text virtual address at
`neu_smp_user_fault_site`. The store faults at CPL3 with `#PF`, error code 7
(present, write, user), CR2 equal to the text base, RIP equal to the fault site
and CS/SS `0x3b`/`0x33`. The AP's handler runs on the AP's own arena, records the
raw frame, publishes `PHASE_EXITED` and resumes the AP's kernel continuation;
the AP then parks. Role 0 keeps executing, completes its own stage-B
observation and exits normally through `int 0x80`. Both blocks must publish
exactly one exit and one completion; the peer's DATA frame, barrier observation
and `ExitReason::Normal(128)` stay valid.

The fatal kernel fault record must remain untouched, proving the user fault was
contained by the user exit path rather than the fatal handler.

### Completion, acknowledgement and teardown

Each Thread's outcome is published exactly once through
`Nodes::complete_on` from its own CPU slot; duplicate or foreign completions are
rejected. The session then completes, acknowledges after both completions, and
reaps. The AP is required to have parked before any root is destroyed, and the
test-only Ring 3 doorway is disarmed and both private roots destroyed after
both CPUs are terminal. Stack bindings, spaces and Nodes are released in the
model's order, and the final allocator/table inventory plus the current root
must equal the pre-execution baseline. Stale Thread generations cannot be
dispatched after slot reuse.

### Profiles

Six profiles are registered and executed individually:

| Case | vCPUs | Proves |
| --- | --- | --- |
| `smp-user-2` | 2 | Both Threads prepared, statically assigned, concurrently at CPL3, distinct roots/stacks, real user work, exact completion and baseline teardown |
| `smp-user-overlap` | 2 | The execution intervals intersect: both CPUs inside the Ring 3 payload while the stage-A barrier already reads `0b11` |
| `smp-user-isolation` | 2 | No user frame or root is shared; each root re-verifies its own W^X/NX/guard layout |
| `smp-user-ap-fault` | 2 | AP `#PF` containment, raw fault tuple, AP park and a normally completing BSP peer |
| `smp-user-completion` | 2 | Duplicate completion/acknowledgement/dispatch attempts are rejected once both Threads are terminal |
| `smp-user-invalid` | 2 | Placement, identity, root, barrier, generation and release rejections; no Ring 3 entry, no completion |

The invalid profile performs nine distinct rejections: duplicate
Thread/Node/space and duplicate CPU slot, forged or stale `SavedContext`,
kernel address space, accepted page-aligned barrier, unaligned barrier, second
barrier binding, one-CPU release, repeated Thread on a second CPU, and a stale
Thread generation after slot reuse. It never enters Ring 3 and publishes
`Cancelled` with checks `0x1ff`; the rejected construction is aborted, not
quiesced, because it never transitioned to `Active`.

## Alternatives considered

- **A broad kernel mutex or `Sync` conversion**: hides ownership instead of
  defining it, invites unsynchronized legacy aliases and violates the phase
  constraints. Rejected; the BSP remains the sole allocator and registry owner.
- **Letting the AP call `Nodes`/`Paging` concurrently**: would create unsound
  `&mut` aliases and concurrent page-table mutation. Rejected; the AP consumes
  one validated descriptor and publishes raw evidence only.
- **Sequential entry on one CPU, or two kernel-only CPUs**: cannot prove
  concurrent CPL3 execution. Rejected; the stopped-CPU checkpoint requires both
  CPUs at CPL3 with distinct roots in the same frozen moment.
- **Timing or scalar flags as concurrency proof**: a guest `both_running` is
  not evidence and timing is not reproducible. Rejected; the two-phase barrier
  plus stopped-CPU state is the witness.
- **Migrating Threads or a per-CPU run queue**: scheduling policy explicitly
  out of scope. Rejected; static assignment with no migration.
- **Using the legacy native entry concurrently**: the single-CPU native path
  assumes exclusive BSP state. Rejected; a separate bridge enters and exits
  each CPU's own context.
- **Concurrent page-table mutation for coordination**: unsafe by construction
  and forbidden by the phase. Rejected; both roots are fully prepared before
  release and the shared barrier page is mapped read/write in both.
- **Introducing ABI 0.6 for the fixture**: no public ABI change is needed.
  Rejected; the fixture uses the existing ABI 0.5 image interface.
- **Weakening the existing user/native/SMP assertions to fit the new path**:
  would hide regressions. Rejected; every legacy selector and case is
  preserved and re-run unchanged.

## Consequences

- Neu now executes two Node Threads in Ring 3 at the same time on genuine
  BSP and AP hardware, with distinct private roots, per-CPU privileged state,
  contained faults and independently verified lifecycle and teardown.
- The BSP remains the only publisher and the only allocator. This is the
  phase's central limitation: no preemptive multi-CPU scheduler, no Thread
  migration, no load balancing, no per-CPU run queues, no AP-side registry
  mutation, no remote TLB shootdown and no concurrent capability, frame or
  page-table changes.
- The AP runs with its local timer masked; no IPI, timer or other interrupt is
  required for or admitted during the Ring 3 profile. User RFLAGS keeps IF
  clear, so the bridge is the only entry and exit path.
- Still deferred: K9C scheduler integration, AP execution of ordinary Threads
  beyond the bounded bridge, remote TLB shootdown, general kernel preemption
  and K10 derivation/revocation.
- Architecture v0.2, experimental ABI 0.5, ADRs 0005–0027 and all fixed pools
  remain in force. No legacy selector, syscall, assertion or timeout changed.
- Host tests cover the placement, identity, barrier, exit, completion,
  acknowledgement, reclamation and stale-generation state machines; they do not
  execute x86 SMP code. Multi-vCPU QEMU runs with stopped-CPU QMP evidence
  remain the only execution evidence.

## History

- 2026-11-03: Accepted for bounded Phase 9C-1 after the full validation matrix:
  286 host tests, 169/169 QEMU scenario executions (167 unique) with the
  unchanged 30-second timeout under `/usr/sbin/taskpolicy -t 4`, individual
  repeats of all six Phase 9C-1 profiles, and a clean-target rebuild from
  `cargo clean` that repeated fmt, every-feature clippy, the default image
  build and the host suite. Rejected attempts during bring-up: the AP recorded
  its entry transition on the BSP only after the AP had already published
  `EXITED` (fixed by recording the observed entry before accepting the
  terminal exit); the AP waited on the acknowledgement word while the BSP
  publishes it before `REAPED` (fixed by waiting for the terminal phase and
  then requiring the acknowledgement count); the AP-fault evidence compared
  the saved `EXIT_RIP` with the error code instead of the fault-site address
  (fixed in the controller); and the invalid profile quiesced a Node that had
  never left construction (fixed by aborting the unstarted Thread) while the
  stale-generation rejection reused the duplicate-Thread check bit (fixed by
  giving it its own bit and restoring the nine-rejection mask). No timeout,
  assertion, priority or historical test was weakened.

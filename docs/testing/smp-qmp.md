# SMP QMP evidence (K9)

The bootstrap, mailbox and static Ring 3 sections retain their original phase
contracts. The [scheduled SMP protocol](#scheduled-smp-evidence-phases-9c-2-9c-3-and-9d)
below covers the conventional scheduler, shared lifecycle transactions,
migration, blocking, fault quiescence and remote TLB invalidation.

The `smp-*` images boot real application processors into Neu kernel entry code.
The guest prints topology and park lines, but those lines are **not** evidence.
`tools/smp_qmp.py` stops the whole VM and reads the production per-CPU
publication, the loader handshake, live vCPU registers, descriptor tables,
local-APIC state and address translations. A guest `true` is never accepted.

## Profiles

| Case | vCPUs | Purpose |
| --- | --- | --- |
| `smp-bootstrap-1` | 1 | SMP foundation active on the legacy single-CPU profile; no AP |
| `smp-bootstrap-2` | 2 | One real AP enters, self-tests, switches root and parks |
| `smp-bootstrap-4` | 4 | Three real APs with distinct identities, stacks and tables |
| `smp-bootstrap-reject` | 4 | Count mismatch must fail before any block or release is published |
| `smp-cpu-identity` | 4 | Re-runs the 4-CPU path for the identity assertions |
| `smp-ap-isolation` | 4 | Re-runs the 4-CPU path for the isolation assertions |

`smp-cpu-identity` and `smp-ap-isolation` execute the same guest path as
`smp-bootstrap-4`; they are registered for the named contract checks but are
not additional unique scenarios.

Use `python3 tools/neu.py test --case <name>`. The tool selects the matching
`-smp` count automatically; running an SMP image with the wrong vCPU count
fails the guest's count check and cannot produce a passing observation. Each
case keeps the existing 30-second default timeout, failure reporting and final
QMP shutdown.

## Stopped-CPU procedure

1. Require `query-status` to be `running`, call `stop`, then require `paused`.
2. Read `query-cpus-fast`; the number of vCPUs must equal the profile and
   every `apic-id` is fetched through QOM and must be distinct and < 256.
3. Read `neu_smp_layout` and all four 48-word blocks at
   `neu_smp_cpus + logical * 384`, plus `neu_smp_released`.
4. For each positive profile, match every block against the ELF symbols and the
   documented constants; for each running logical CPU, map its published APIC
   ID to QMP's `cpu-index` and inspect that vCPU.
5. For the reject profile, verify that all blocks and the release counter are
   zero, that the BSP is halted in Neu's halt loop with IF=0 on the loader
   root, and that every other vCPU is outside kernel text with no per-CPU
   identity.

## Positive checks

Per-CPU publication:

- magic, logical index, BSP flag and exactly one entry;
- stack and IST ranges equal the ELF symbols and the documented sizes
  (BSP 64 KiB/32 KiB, AP 16 KiB/8 KiB);
- GDT/TSS/IDT bases equal the per-CPU symbols and the documented limits
  (`0x27`, `0x67`, `0xfff`);
- APIC ID < 256, unique, distinct from the BSP, equal to the CPUID APIC ID and
  present in QMP's CPU set; APs are ordered by ascending APIC ID;
- APIC base: xAPIC on, x2APIC off, global enable on; BSP bit set only on the
  BSP; every AP's base equals the BSP base with bit 8 cleared;
- state `Parked` with history bits 0–4, no failure or fault record, park count
  0 for the BSP and 1 for each AP;
- `SELF_TEST` contains all nine local checks; `ISOLATION` equals the
  documented BSP (0x37) or AP (0x3f) bitmask; scheduler-local and reserved
  words are zero;
- GS base equals the CPU's own block address; CR3 equals the owned root;
  entry, ready and park stack pointers lie inside the CPU's own stack;
- CR0/CR4/EFER/RFLAGS show protected mode, paging, long mode, write-protect,
  no x2APIC, no task switch and IF=0;
- BSP handshake words (`LOADER_CR3`, `LOADER_RSP`, `MP_INFO`, `RELEASE_RIP`,
  `WAIT_SPINS`, `PARK_RSP`) are zero; each AP records a distinct non-zero
  loader CR3, a loader RSP outside the kernel image, an aligned higher-half
  MP descriptor, its own entry RIP and a bounded completed root wait;
- stack/IST canaries in memory and the observed copies are intact.

Live vCPU state, one stopped CPU at a time:

- RIP in Neu's BSP halt loop or the AP park loop; `HLT=1`, `CPL=0`, IF=0;
- CR3 equals the observed owned root and CR0/CR4/EFER equal the publication;
- GS base, GDTR and IDTR equal the published values; `TR = 0x18` names the
  published TSS with limit `0x67`, a TSS64 descriptor and access `0x8900` or
  `0x8b00`;
- RSP is inside the published stack; `CS = 8`, `CS64`, `DPL=0`;
- local APIC is software-enabled, LVTT is masked on every CPU, and AP timer
  initial/current counts are zero (the BSP's Limine timer remains the only
  programmed timer and stays masked).

Memory and layout:

- all nine AP guard pages and the four BSP guards translate to `Unmapped`;
- every stack/IST bottom maps to a distinct physical frame;
- BSP stack/IST, AP stack/IST, GDT, TSS and IDT intervals do not overlap.

## Reject checks

`smp-bootstrap-reject` boots the 2-CPU profile image with four vCPUs. The
observer requires that no per-CPU block and no release counter were published,
that exactly one vCPU is halted in Neu's halt loop with interrupts disabled on
a non-zero loader root, and that every other vCPU is outside kernel text and
the park loop, shares that loader root and owns no per-CPU identity block. This
proves the count check fails before AP release rather than after partial
initialization.

## Limits

The observer only reads state the kernel actually publishes plus architectural
state exposed by QEMU. It cannot prove the absence of hidden side effects by
itself; the isolation claims are supported by the compile-time feature
isolation, the explicit BSP gates on shared paths, the per-AP self-tests and
the negative register/translation checks above. Host tests cover the
host-testable topology and state-machine rules but do not execute SMP code.
The Phase 9A profiles do not test or claim IPI delivery, TLB shootdown, a
per-CPU scheduler or concurrent allocation; the Phase 9B profiles below add
work-mailbox and work-IPI evidence.

# Cross-CPU work evidence (bounded Phase 9B)

## Profiles

| Case | vCPUs | Purpose |
| --- | --- | --- |
| `smp-work-2` | 2 | One AP accepts, executes and stops two different commands |
| `smp-work-4` | 4 | Three APs concurrently execute different commands |
| `smp-work-overlap` | 2 | BSP and AP run identical work across the two-phase gate |
| `smp-work-repeat` | 2 | Three sequential commands with independent ordering |
| `smp-work-invalid` | 2 | Six distinct negative rejections |
| `smp-ipi-2` | 2 | Stage B xAPIC wakeup for every command |
| `smp-ipi-4` | 4 | Stage B xAPIC wakeup for three APs |

## Final stopped-CPU procedure

After the guest reaches its idle marker, the existing Phase 9A observation runs
first, then the work oracle reads:

1. `neu_smp_work_layout`, required to equal `[1, 0x4e45_555f_574b_4531, 4, 32,
   256, 24, 192, 4, 10]`.
2. All four 32-word mailboxes at `neu_smp_work_mailboxes + logical * 256`.
   Slots for the BSP and beyond the profile must be entirely zero.
3. All sixteen journal slots at `neu_smp_work_journal` (four slots of ten words
   per CPU, stride 80 bytes). Slots beyond each AP's plan must be zero.
4. The nineteen evidence words at `neu_smp_work_evidence`.
5. The 24-word gate at `neu_smp_work_gate`.

Every running AP's mailbox must publish the matching magic, logical index and
APIC ID from its Phase 9A block; final phase `Stopped`, state `Idle`, and exact
`SEQUENCE = ACKED = EXECUTIONS = PUBLISH_ORDER = ACCEPT_SEQUENCE =
ACCEPT_ORDER = COMPLETE_SEQUENCE = COMPLETE_ORDER = plan + 1`; execution mask
`(1 << expected) - 1`; no failure or rejection; start RSP inside the AP's
private stack; a reproducible stop command in the final command fields; and,
for IPI profiles, `IPI_RECEIVED = IPI_EOIS = expected` with zero spurious. Poll
profiles require all IPI counters zero.

Every journal slot must match the profile plan exactly: sequence, kind, input,
rounds, the Python-recomputed result and auxiliary word, the recomputed start
and completion markers, and independent accept/complete order equal to the
sequence. This proves the result values came from AP execution rather than a
BSP publication.

The evidence words must publish magic and ABI, mode (poll = 1, IPI = 2), the
CPU count, total commands and per-AP commands, the stopped AP count, zero
failure, six negative checks for `smp-work-invalid`, balanced IPI
sent/received/EOI counts and zero spurious for IPI profiles, and, for the
overlap profile, the recomputed BSP result/auxiliary, epoch 1, overlap mask
`0b11` and release epoch 1.

## Deterministic checkpoint

For `smp-work-4`, `smp-ipi-4` and `smp-work-overlap` the runner stops the whole
VM when the guest logs `[NEU] Work checkpoint:`. It requires:

- the profile's exact vCPU count and distinct APIC identities;
- the BSP inside `neu_smp_work_bsp_dwell_begin..end`;
- every AP inside `neu_smp_work_ap_dwell_begin..end` (parallel) or
  `neu_smp_work_gate_wait_begin..end` (overlap);
- `CPL=0`, IF=0, CR3 equal to the owned root and a GS base that decodes to the
  expected per-CPU block, covering every logical index 1..count-1 distinctly;
- mailbox state `Executing`, sequence 1, accept order 1 and the correct start
  marker, with no completion fields;
- for overlap: gate phase `Checkpoint`, `RELEASE = 0`, `OVERLAP = 0b11`,
  `ENTERED = READY = 1`, `DONE = 0`, and the BSP result not yet executed;
- for parallel: an all-zero gate.

The VM is resumed after the checkpoint. A printed `parallel=true` or a
serial-only scalar is never accepted.

## IPI-specific checks

For `smp-ipi-*` the runner requires the `SMP work IPI armed` marker, the
per-mailbox `IPI_RECEIVED`/`IPI_EOIS` counts above and the evidence totals.
The QMP live-register checks additionally require the local APIC to be
software-enabled with every LVT masked; the handler's in-service verification
and one-EOI-per-IPI accounting are production code, and the oracle cross-checks
their published counters rather than any serial claim.

## Limits

As in Phase 9A, only published production memory and architectural state are
read. Mailbox and gate failures are distinguished by published error codes and
the evidence words. Host tests cover the mailbox and barrier state machines but
do not execute x86 SMP code; the multi-vCPU profiles are the execution
evidence. Thread migration, per-CPU run queues, AP scheduling, remote TLB
shootdown and concurrent allocator/capability mutation remain untested and
unclaimed.

# Static two-CPU Ring 3 evidence (bounded Phase 9C-1)

## Profiles

| Case | vCPUs | Purpose |
| --- | --- | --- |
| `smp-user-2` | 2 | Both statically assigned Threads execute real Ring 3 work concurrently |
| `smp-user-overlap` | 2 | The two execution intervals intersect inside the Ring 3 payload |
| `smp-user-isolation` | 2 | No user frame or root is shared; each root re-verifies its own layout |
| `smp-user-ap-fault` | 2 | AP `#PF` containment with a normally completing BSP peer |
| `smp-user-completion` | 2 | Duplicate completion, acknowledgement and dispatch are rejected |
| `smp-user-invalid` | 2 | Nine placement/identity/root/barrier/generation rejections, no entry |

## Publication and checkpoints

The guest publishes one 96-word, 64-byte-aligned block per CPU at
`neu_smp_user_blocks + logical * neu_smp_user_layout[3]`; the layout witness is
`[magic, MAX_CPUS, fields, stride]`. The oracle stops the whole VM at two
exported windows:

1. **Prepared** — both CPUs at CPL0, no Ring 3 entry. Every descriptor
   identity, Node/Thread generation, space/root, saved context, stack binding,
   entry arena, TSS/GDT/IDT base, RSP0 and baseline word is matched against the
   ELF symbols and the documented constants; both CPU blocks must be `Ready`
   and the private roots distinct.
2. **Concurrent** — both CPUs inside the Ring 3 fixture span at CPL3, on user
   CS/SS with the RFLAGS policy, each on its own user CR3, RIP inside the
   copied payload range, and the stage-A barrier word equal to `0b11`. Both
   descriptor phases must be `Entered`, and each CPU's bridge continuation RSP
   must lie inside its own published kernel arena.

The VM is resumed after each checkpoint. A guest `both_running` word or a
serial line is never accepted.

## Final checks

After the idle marker, the oracle re-reads both descriptors and requires:
exactly one entry, exit, completion and acknowledgement per CPU; a normal exit
(`int 0x80`) on the BSP and, for the fault profile, a `#PF` tuple with vector
14, error 7, the fault-site RIP and the text-base CR2 on the AP; the raw
exit RIP/RSP/RFLAGS/CS/SS and the exit CR3 equal to the published user root;
the restored kernel root, GS base, CR0 and EFER equal to the pre-entry state;
the handler RSP inside the CPU's own arena; the barrier and both DATA frames
showing the survivor publication observed from the other address space; and,
for the isolation and overlap profiles, the documented frame disjointness and
interval intersection. The private roots must still translate every user page
with its published W^X/NX/rights and translate the guard/unmapped pages as
absent.

The model identities are cross-checked against the production objects: the
Thread generation, the exact Node/Thread/space IDs, the completion outcome
(including `UserFault` for the AP-fault profile), the reclaimed Thread state
and the final allocator/table inventory equal to the pre-execution baseline.
Both CPUs must be parked, the AP timer masked, the local APIC untouched by this
profile and the fatal fault record clear.

## Invalid checks

`smp-user-invalid` publishes `Cancelled` with checks `0x1ff`, zero entry, exit,
completion and acknowledgement counts, zero baseline/final difference and no
Ring 3 bridge use. The oracle verifies that every unpublished descriptor word
is zero, that the CPU descriptor tables and gates are still the disarmed
kernel ones, and that the fatal fault record is untouched. The nine rejections
are: duplicate Thread/Node/space and CPU slot, forged `SavedContext`, kernel
address space, accepted page-aligned barrier, unaligned barrier, second barrier
binding, one-CPU release, repeated Thread on a second CPU and stale Thread
generation after slot reuse.

## Limits

The two Threads are statically assigned before release and never migrate. No
per-CPU run queue, preemptive multi-CPU scheduler, load balancing, AP-owned
registry or page-table mutation, remote TLB shootdown or concurrent allocation
is tested or claimed by the static profiles. Host tests cover the placement
and lifecycle state machines; the stopped-CPU multi-vCPU profiles are the
execution evidence.

# Scheduled SMP evidence (Phases 9C-2, 9C-3 and 9D)

## Profiles

| Case | vCPUs | Purpose |
| --- | --- | --- |
| `smp-schedule-2` | 2 | CPU-local FIFO scheduling, native returns and timer preemption of three Threads |
| `smp-schedule-4` | 4 | The same scheduling loop on all CPUs, including an idle CPU |
| `smp-schedule-shared` | 2 | Concurrent same-Node Threads perform real atomic increments of one shared counter |
| `smp-schedule-migrate` | 4 | A2 resumes its saved context on CPU3 after executing on CPU1 |
| `smp-schedule-fault` | 2 | A2's user protection fault cancels its running sibling; B1 survives |
| `smp-schedule-reject` | 2 | Unsafe user RSP/DF at a real SYSCALL is contained before any operation or resume |
| `smp-schedule-idle` | 4 | B1 blocks with a pinned Channel wait; its CPU enters real STI/HLT and wakes |
| `smp-schedule-ipc` | 2 | Scalar/Flag Channel transactions while shared-Node Threads run on two CPUs |
| `smp-schedule-stress` | 4 | Longer scheduling/migration run with native/IRQ observation pauses disabled |
| `smp-tlb-2` | 2 | Live write-permission removal, remote invalidation and denied user store |
| `smp-tlb-4` | 4 | Every CPU invalidates or flushes its root before acknowledging the change |
| `smp-tlb-invalid` | 2 | Twenty-two inert mapping/coordinator rejections followed by the real shootdown |

`python3 tools/neu.py test --case <case>` builds the isolated feature and selects
the matching CPU count. Normal profiles use 512 MiB; stress uses 1 GiB. The
default deadline remains 30 seconds. Three PIT calibration samples determine
the local APIC's 10 ms one-shot quantum; the observer recomputes the median
frequency and initial count from those samples. Work release requires 40 BSP
timer preemptions (90 for stress).

## Production schemas and observation

`tools/smp_scheduler_qmp.py` is a dedicated observer with its own expectations.
It reuses only generic QMP/physical-memory helpers from the earlier observer.
It repeatedly stops **all** QEMU CPUs, verifies the paused state and QOM APIC
identities, inspects registers and raw production storage, and resumes the VM.
A snapshot with the actual transaction lock held is skipped; an unlocked
snapshot must satisfy the scheduler, authority and waiter invariants.

The ELF exports locate storage rather than certify it:

- `neu_smp_sched_layout` identifies four 832-byte CPU records, each with 64
  atomic metadata words, the owned 160-byte ABI Frame at offset 512 and the
  128-byte bridge at offset 672.
- `neu_smp_sched_value_layout`, `neu_smp_sched_threads`,
  `neu_smp_sched_queues` and `neu_smp_sched_contexts` locate the actual
  StackBinding, SavedContext, Thread, FIFO, owner/affinity and CPU-slot fields.
- `neu_smp_sched_entries` and `neu_smp_sched_traces` retain up to 256 32-word
  records per CPU. Each record includes the full 20-word Frame, Thread/Node,
  root, entry sequence and CPU identity. Exit records also carry the vector,
  error, CR2, actual stack-frame address, KERNEL_GS and ISR/EOI evidence.
- `neu_smp_sched_authority`, `neu_smp_sched_inventory` and
  `neu_smp_sched_tlb` locate actual capability tables, objects, Channel queues,
  wait pins, Node fields, frame bitmap, page-table metadata and shootdown state.

The observer independently reads the first native and timer normalized
176-byte frames directly from each executing CPU's virtual stack. Their
registers, vector/error and owning root/stack must match the traces. Native
and IRQ entries retain the CPU's trusted KERNEL_GS bridge; the generic CPU
exception stub clears both GS MSRs before Rust. Every CPU's live GDT, TSS,
IDT, guard pages, CR0, EFER, root and GS identity are checked independently.
Timer vector 224 and IPI vector 241 require an actual ISR bit followed by one
EOI; spurious vector 255 has no EOI.

The baseline, prepared and terminal resource checkpoints use bounded masked
LAPIC observation windows. Normal profiles also expose the first native/IRQ
frames in such windows. Stress omits those native/IRQ pauses and retains full
entry/exit histories and stopped-CPU invariant checks. None of these windows
fabricates a user execution, saved frame, IRQ or acknowledgement.

## Scheduling, authority and containment

Two Nodes contain A1/B1/A2; A1 and A2 share Node A's root and authority table
but have distinct guarded stacks. At least one stopped snapshot must show
both same-Node Threads simultaneously at CPL3, with distinct CPU ownership.
The shared-counter profile additionally requires the physical counter to equal
the sum of both Threads' final progress counts. Queues, saved frames, Thread
states, active CPU slots and affinity must agree at every accepted snapshot.
The migration profile must show the actual A2 user registers on CPU3 after
its CPU1 execution. Every resumed Frame is reconstructed from the previous
exit, allowing only RAX/RDX operation results at a native return.

The Channel profiles require a raw blocked/pinned B1 waiter, exactly one
block/wake admission, scalar receipt and a transferred Flag with a stale
sender handle. The four-CPU idle profile also requires CPU2 at the actual
`sti; hlt; cli` path, IF set, no active Thread and no missed wake. Actual
authority entries and queued transfers must account for every object reference
and waiter pin; terminal tables, queues and waits must be empty.

Fault profiles stop A1 before A2 commits its Node fault. The observer checks
the pre-commit active slots, cancelled sibling and retained authority, backing
and root pins. B1 must continue to normal exit and `NEU_LAST_FAULT` stay zero.
The rejection profile independently reads A2's real SYSCALL frame with RSP
`0x40cff9`, DF set, vector 128/error 0 and the linked rejection resume site.
That frame must be the last A2 entry boundary, never a resumed context; the
CPU records exactly one rejected return and zero CPU exceptions.

## Remote translations and final conservation

The TLB profiles warm a writable DATA alias at `0x410000`. A prevalidated
transaction atomically clears only W in its actual PTE and publishes a bound
epoch/root/address/target set. For every target, QMP observes the CPU after its
actual INVLPG or CR3-reload instruction, inside the linked observation span,
with that CPU's ACK still unpublished. The coordinator commits only after
all targets acknowledge the current request. A2's subsequent store at
`0x410210` must take a genuine user `#PF` (error 7) and leave the warmed value
3 unchanged. The unrelated Node cannot map the private alias.

The invalid profile checks all 22 exact errors and reads the real PTE before
and after each rejected request. Simulated coordinator acknowledgements use a
separate model and cannot advance the actual hardware epoch.

After all CPUs stop, each TSS.RSP0 must name its permanent bootstrap stack.
Inspection views expire before Thread-stack destruction and root-pin release.
The observer checks every allocated frame, ownership record, table and reachable
translation, then requires the final allocator **bitmap** and original table
inventory to match the baseline exactly, with zero user roots, pins,
references, mappings, capability authority or waiters. Every CPU restores its
kernel controls and has empty LAPIC ISR/IRR state before the existing Neu halt
checks and host-controlled QMP exit.

This protocol covers the bounded four-CPU, two-Node, three-Thread integer
execution model with IRQ-disabled kernel transactions. Work stealing, load
balancing, general kernel preemption, arbitrary allocation from APs, a public
mapping ABI and K10 derivation/revocation remain later work.

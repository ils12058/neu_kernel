# SMP QMP evidence (bounded Phase 9A and 9B)

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

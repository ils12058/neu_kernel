# ADR 0026: SMP bootstrap and per-CPU foundations

- Date: 2026-10-31
- Status: Accepted for the bounded K9 / Phase 9A scope (2026-10-31)
- Related decisions: NEU-003, NEU-004, NEU-005, NEU-011, NEU-018, NEU-020,
  NEU-021, NEU-023, NEU-024
- Architecture:
  [scheduling, SMP and asymmetric cores](../Neu_Kernel_Architecture_v0.2.md#11-scheduling-smp-and-deferred-asymmetric-cores),
  [scheduling, clocks and CPU topology](../Neu_Kernel_Architecture_v0.2.md#17-scheduling-clocks-and-cpu-topology),
  [verification and CI](../Neu_Kernel_Architecture_v0.2.md#21-verification-fault-injection-and-ci),
  [roadmap and acceptance tests](../Neu_Kernel_Architecture_v0.2.md#22-development-roadmap-and-acceptance-tests)
- Predecessors: [ADR 0005](0005-boot-memory-ownership-and-frame-allocation.md),
  [ADR 0006](0006-kernel-exception-descriptors-and-fatal-handling.md),
  [ADR 0007](0007-kernel-owned-paging-and-controlled-mappings.md),
  [ADR 0013](0013-single-cpu-user-preemption.md)

## Context

Every accepted Neu contract through K8 is single-CPU. The architecture document
describes SMP as future work, and the implementation relies on that assumption
in ways that are not locally visible: one frame bitmap, one paging registry and
current root, one scheduler FIFO, one Node/capability object registry, one
preemption handoff mailbox, one local-APIC timer policy and an unsynchronized
COM1 writer. `-smp 4` on the QEMU command line starts the extra vCPUs, but those
CPUs previously remained where the loader left them; no Neu code observed them.

Phase 9A asks a narrower question than "run the kernel on many CPUs":

> Can Neu start genuine application processors into real kernel entry code,
> give each one a trustworthy CPU identity and private privileged state, prove
> that state independently, and park them without exposing the existing
> single-CPU kernel state to concurrent execution?

That framing matters. Moving the scheduler, capability registry or frame
allocator to multiple CPUs would require per-CPU ownership, cross-CPU
synchronization and remote TLB shootdown that do not exist yet. Doing it in the
same phase as the first AP bring-up would make it impossible to attribute a
failure to the bootstrap mechanics or to the concurrent subsystem rewrite. This
record therefore accepts only the bootstrap foundation and keeps every existing
subsystem BSP-owned.

The audit performed before editing found the following BSP-only state, all of
which APs must not reach in Phase 9A:

- the physical frame bitmap allocator and its accounting;
- the paging registry, active root and owned-root construction;
- the scheduler FIFO, Thread contexts, runnable queue and wake paths;
- Node, Thread and capability object registries and all Channel state;
- the preemption mailbox and local-APIC timer policy;
- the fatal-fault record, panic path, halt contract and COM1 serial writer.

No global was converted to `Sync` merely to compile. No AP path calls any of
these subsystems.

## Decision

### Pinned Limine SMP handoff

Neu keeps Limine **12.9.4** (base revision 6, protocol reference
`3a0526b7…`) and adds only the standard SMP request. The response must have
revision 0, no unknown flags and no x2APIC flag; the request asks for xAPIC.
The kernel accepts only the configured processor count from a feature-gated
profile:

| Feature | Expected vCPUs | Purpose |
| --- | --- | --- |
| `smp-one` | 1 | Legacy single-CPU execution with SMP foundation active |
| `smp-two` | 2 | One real application processor |
| `smp-four` | 4 | Three real application processors |

The discovered `cpu_count` must equal the profile constant. Zero, oversized,
under-counted or over-counted responses are rejected before any per-CPU state is
published. The loader-provided CPU pointer array and every `mp_info` pointer are
validated as higher-half, aligned, readable handoff memory. Each descriptor's
`lapic_id` must be inside the xAPIC range (< 256); the bootstrap processor's
descriptor identity must agree with its CPUID APIC identity; APIC IDs and
processor IDs must be unique; the BSP must be present. A descriptor whose
`goto_address` is already non-zero is treated as an already-released AP and
rejected.

APIC identity is never used as a dense index. The topology assigns logical 0 to
the BSP and logical 1.. to APs sorted by ascending APIC ID, so sparse hardware
identities map onto dense kernel indices deterministically. `MAX_CPUS` is fixed
at 4; capacity is a compile-time contract, not a runtime allocation.

### Per-CPU publication block

`neu_smp_cpus` is a fixed array of four 64-byte-aligned blocks. Each block is 48
documented `u64` fields (384 bytes) with magic `0x4e45_555f_4350_5531`
("NEU_CPU1"). `neu_smp_layout` publishes `[magic, MAX_CPUS, BLOCK_FIELDS,
BLOCK_STRIDE]` so an independent observer can decode the array without trusting
a guest scalar. The fields cover:

- identity: logical index, BSP flag, APIC ID, processor ID, CPUID APIC ID;
- bootstrap state and its complete transition history;
- stack, IST, GDT, TSS and IDT bases and limits;
- architectural state (CR0, CR3, CR4, EFER, RFLAGS, APIC base, GS base);
- entry, ready and park stack pointers and the park count;
- stack/IST canaries and the values the AP observed;
- a local self-test bitmap, a fault record, loader handshake values;
- an isolation-claims bitmap and an explicit scheduler-local placeholder.

The scheduler-local word is reserved and remains zero in Phase 9A; no per-CPU
run queue or migration mechanism is claimed. A single exported
`neu_smp_released` counter records how many APs the BSP released.

CPU identity is read through `IA32_GS_BASE`: the BSP and every AP point GS at
their own block. A reader validates that the base names a block whose magic is
published before deriving the logical index. Before an AP publishes its GS base
(including fault paths that occur earlier), identity falls back to matching the
CPUID APIC ID against published blocks. There is **no** global mutable
`current_cpu`. The GS base is CPU-local register state: it survives interrupts
on the same CPU and is never migrated because Phase 9A has no scheduler on APs.
Any future code that can migrate a Thread must re-derive or reload the identity
at the migration boundary; that is explicit K9B work.

### State machine and publication ordering

Each CPU publishes exactly one documented sequence:

`Uninitialized (0) → Prepared (1) → Entered (2) → Ready (3) → Parked (4)`

`Failed (5)` is terminal. The state word is written with a Release store and
read with Acquire semantics; a history bitmask records every state that has
been passed through. A consumer accepts only a `Parked` CPU whose history
contains bits 1–4, or a `Failed` CPU. This prevents a torn or reordered
publication from being read as success.

Before releasing any AP, the BSP publishes the complete static part of every
block (magic, logical index, APIC ID, stack/IST/table addresses, expected APIC
base, release RIP) and validates the per-CPU arenas. It then publishes its own
block (`Prepared`), writes each loader `goto_address`, updates the release
counter and advances to `Entered`. APs can therefore never observe a partially
initialized block: the release store is the only event that makes an AP run,
and the block is complete before that store.

The owned page tables are published separately through `OWNED_ROOT`. Only the
BSP writes it, only after it has translated every AP stack, IST and descriptor
table in the new root and proved that every guard page is absent. APs spin with
a bounded counter on an Acquire load; a timeout records `Failed` and parks.
This two-phase ordering is what allows the owned-root mapping to be built at
all: APs start on the loader root, wait there, and switch only to a root that
has already been proven to contain their code, stacks and tables.

### Independent AP entry and private privileged state

Each AP has its own assembly entry stub and never runs on a BSP stack. The stub
executes `cli`/`cld`, saves the loader `RSP`, loads its own 16 KiB stack, and
calls Rust. The Rust entry:

1. validates its logical index, block magic and non-BSP role;
2. records the loader RSP, MP descriptor and loader CR3;
3. verifies CPUID APIC identity, descriptor APIC identity, processor identity
   and the descriptor's `goto_address` against the published release RIP;
4. validates CR0, CR4, EFER and RFLAGS (protected mode, paging, long mode,
   write-protect, no x2APIC, no task switch, IF=0);
5. proves its RSP is inside its own published stack;
6. loads and reads back its own GDT, IDT and TSS (`TR = 0x18`), with RSP0 and
   IST1 pointing at its own arena and an I/O bitmap that denies user port
   access;
7. establishes `IA32_GS_BASE` as its block address and verifies the readback;
8. verifies the APIC base is xAPIC-enabled, globally enabled, non-x2APIC and
   has the BSP bit clear (the expected value derived from the BSP's own base);
9. advances to `Entered`, waits boundedly for `OWNED_ROOT`, switches CR3 and
   verifies the new root;
10. re-reads both stack canaries, publishes `Ready`, then `Parked`, and halts
    with IF=0.

Every failure records a distinct code in the block, marks `Failed` and parks
silently. An AP exception stores the vector, error code, faulting RIP and
handler RSP in its own block and parks; it never touches the BSP fatal record,
panic path or serial writer. Entry counts, loader CR3/RSP, wait spins and
canaries make a second entry, reuse of loader memory or a skipped root switch
observable from outside.

The AP arena layout is fixed in assembly inside the kernel image: low guard
page, 16 KiB stack, middle guard page, 8 KiB IST stack, IST guard page, 64-byte
GDT, 104-byte TSS and a 4 KiB IDT, all page-aligned and non-overlapping. The
guard pages are skipped by the owned-root image mapping and remain unmapped;
the BSP verifies that before publishing the root. The BSP keeps its existing
64 KiB stack and 32 KiB deep-fault IST stack.

### Deliberate isolation from single-CPU state

Phase 9A APs execute only the AP entry, its local self-tests, the root switch
and the park loop. They do not enter the scheduler, syscall path, paging
construction, preemption, Channel, capability, Node or frame-allocation code,
and they do not write COM1. The SMP images are compile-time isolated from every
single-CPU test subsystem so a configuration cannot silently combine the two.

The BSP keeps its existing behavior. `is_bsp()` gates serial output, panic,
fatal faults and the halt contract; on an AP these paths park instead. The
AP-local exception path is separate. Local-APIC timer policy remains BSP-only:
the BSP's Limine-programmed timer stays masked in these images, AP LVTT entries
remain masked and AP timer counters are zero. No IPI is sent and no TLB
shootdown is implemented or claimed.

### Evidence contract

Six stopped-CPU profiles are registered. `smp-bootstrap-1`, `smp-bootstrap-2`
and `smp-bootstrap-4` boot genuine `-smp 1`, `-smp 2` and `-smp 4` VMs.
`smp-bootstrap-reject` configures the 2-CPU profile and boots four vCPUs, so the
count check must fail before any release. `smp-cpu-identity` and
`smp-ap-isolation` execute the 4-CPU path to exercise the identity and
isolation assertions by name; they are not additional unique scenarios.

`tools/smp_qmp.py` stops the whole VM, requires `paused`, and reads:

- the layout witness and every 48-word block from production memory;
- QMP's own CPU set and per-CPU `apic-id` through QOM, matched against the
  published identities;
- live RIP/RSP/CR0/CR3/CR4/EFER/RFLAGS/GS/GDTR/IDTR/TR/CS for each vCPU;
- the physical translation of every stack and IST bottom, and the absence of
  every BSP and AP guard page;
- the local-APIC LVTT mask, SPIV enable bit and timer counters per CPU;
- disjointness of all per-CPU arenas and descriptor tables;
- for the reject profile, that no block or release was published, the BSP is
  halted in Neu's halt loop on the loader root, and every other vCPU is outside
  kernel text with no per-CPU identity.

A guest `true` or serial line is never accepted as evidence. Repeated runs are
not new scenarios.

## Alternatives considered

- **Starting APs after paging is active**: simpler for mapping, but APs would
  enter on an unspecified root while the BSP still constructs tables, or the
  BSP would have to delay all AP bring-up until after every legacy subsystem
  initialized. The two-phase loader-root wait was chosen because it starts real
  APs early while keeping the root switch strictly ordered.
- **Running APs on BSP or loader stacks**: cannot prove per-CPU stack
  ownership and would let an AP corrupt the BSP. Rejected.
- **Sharing one GDT/TSS/IDT across CPUs**: TSS and IST are CPU-private; shared
  TSS state is not reentrant. Rejected; each AP gets private tables.
- **A global mutable `current_cpu`**: would be a data race before any
  synchronization exists. Rejected in favor of a CPU-local GS base with a
  published block.
- **Using the APIC ID as the logical index**: sparse hardware identities would
  force sparse arrays and ambiguous bounds. Rejected; dense logical indices
  are sorted by APIC ID.
- **Converting BSP-only statics to `Sync` or adding global locks so APs can
  call existing subsystems**: would compile but expose unsynchronized
  registries and violate the isolation goal. Rejected. K9B must design
  per-CPU ownership and cross-CPU protocols first.
- **Sending an IPI or using a shared lock as the root barrier**: no IPI or
  lock protocol is validated yet. Rejected; APs spin on one Acquire atomic
  with a bounded counter.
- **x2APIC support**: adds MSR-based APIC addressing and a different ID space
  with no Phase 9A consumer. Rejected; x2APIC responses are refused.
- **Arming per-AP local-APIC timers or a kernel preemption tick**: would
  imply AP scheduling that does not exist. Rejected; AP timers stay masked and
  zero.
- **Claiming that `-smp 4` alone proves SMP**: QEMU starts vCPUs regardless of
  guest behavior. Rejected; every claim is tied to production blocks and
  stopped-vCPU state.
- **Serial-only readiness lines**: cannot distinguish a parked AP from a
  reported scalar. Rejected.

## Consequences

- Neu now starts genuine application processors into real kernel entry code on
  `-smp 2` and `-smp 4`, with unique stacks, private descriptor tables, a
  trustworthy identity, validated privileged state, an atomic readiness
  handshake and a bounded parked state. The 1-CPU profile keeps the legacy
  single-CPU path.
- The existing single-CPU subsystems remain BSP-owned and untouched. This is
  the phase's central limitation, not an implementation detail: no Thread runs
  on an AP, no work is balanced, no run queue is per-CPU and no cross-CPU
  memory operation is synchronized beyond the documented publication words.
- Still absent and explicitly deferred to K9B/K9C: concurrent Ring 3
  scheduling, shared or per-CPU run queues, Thread migration, load balancing,
  AP kernel preemption, remote TLB shootdown, concurrent frame allocation,
  concurrent capability mutation, general kernel preemption, IPIs and a
  per-CPU timer policy. K10 derivation/revocation remains separate.
- Architecture v0.2, experimental ABI 0.5 and ADRs 0005–0025 remain in force.
  No public ABI, syscall, pool constant or legacy assertion changed. There is
  no ABI 0.6.
- Host tests cover topology assignment, duplicate/range/capacity rejection,
  the transition history, failed-terminal behavior and per-CPU region
  overlap/minimums. QMP covers the architectural claims on stopped CPUs. Host
  tests do not execute x86 SMP code.
- ELF validation compares symbol **address differences** rather than object
  identity for adjacency. LLVM may assume distinct `extern static` objects have
  distinct addresses even when the linker places them at the same address;
  direct `stack_top != guard_mid` comparisons can therefore fold to true. The
  build-time ELF checker and the QMP block-versus-symbol checks still verify
  the absolute layout, so the arithmetic form does not weaken the contract.

## History

- 2026-10-31: Accepted for bounded Phase 9A after the clean-target rebuild,
  253 host tests, a 156/156 execution full suite (154 unique) and individual
  repeats of the six SMP profiles and representative legacy cases. Host load
  pushed the TCG vCPU onto efficiency cores and tripped pre-existing timing
  bounds in legacy multi-Thread tests (the `capabilities-rollback` failure
  reproduced on unmodified HEAD), so the final full suite ran under
  `/usr/sbin/taskpolicy -t 4`; no harness timeout, guest image or assertion
  changed. Rejected attempts stopped on `timer` calibration, a
  `channels-stress` QMP deadline, a `preempt` IRQ-frame observation and the
  legacy 64-dispatch cap; every affected case passed individually with the
  unchanged timeout.

# ADR 0008: Enter an isolated Ring 3 fixture with a test-only return

- **Date:** 2026-10-25
- **Status:** Accepted — K2 / Phase 2A isolated execution test only.
- **Extends:** [0007: owned paging](0007-kernel-owned-paging-and-controlled-mappings.md)
  and [0006: exception foundation](0006-kernel-exception-descriptors-and-fatal-handling.md).
- **Related NEU decisions:** `NEU-017 / EXEC` (embedded first execution only),
  `NEU-018 / MEM` (bounded user mappings), `NEU-021 / SEC` (privilege boundary),
  `NEU-023 / DIAG` (origin classification), `NEU-024 / TEST` (local evidence).
- **Architecture:** [Section 14](../Neu_Kernel_Architecture_v0.2.md#14-node-image-bootstrap-and-execution-abi--neu-exec-001),
  [Section 15](../Neu_Kernel_Architecture_v0.2.md#15-detailed-physical-and-virtual-memory--neu-mem-001),
  and [Section 18](../Neu_Kernel_Architecture_v0.2.md#18-threat-model-and-security-boundaries--neu-sec-001).

[ADR 0009](0009-experimental-native-syscall-and-trusted-return.md) adds the separate
Phase 2B native configuration. The original test gate and rationale below remain.

## Context

Phase 1B owns the active kernel root and enforces supervisor mappings. Real user
execution additionally requires U/S at every parent level, independent backing,
a trusted privilege-entry stack, and exact hardware context validation. A kernel
stack already holding a suspended Rust continuation cannot also be overwritten
by TSS.RSP0 exception delivery. Copying kernel parent pointers would introduce
shared-table lifetimes and mutation policy before they are needed.

The architecture's Node, startup descriptor and native syscall specifications
remain proposals. The supplied Phase 2A brief authorizes a narrower execution
proof before `SYSCALL`, `abi_query`, `thread_exit`, scheduling or a general loader.

## Decision

Keep the pinned Rust/target/loader and single-CPU IF=0 environment. Compile the
execution fixture and DPL=3 gate only in explicit `test-user*` configurations.
Ordinary boot does not enter Ring 3 or enable a user-callable completion gate.

Build a fresh owned PML4 and **private child tables**, including the higher-half
kernel image and table workspace. No child-table pointer is borrowed from the
kernel root. The same permanently reserved kernel image backing is mapped
supervisor-only; bounded table aliases also stay supervisor RW/NX. This costs
extra table frames but avoids shared-subtree lifetime and mutation rules.

Allow user entries only in a bounded lower-half fixture range. Set U/S at every
required level, reject incompatible existing parents instead of upgrading them,
and retain physical-width, alignment, conflict, NX and W^X checks. User mapping
records use the existing generation identities and alias/reference registry.

Copy trusted embedded assembly bytes from kernel R/NX storage into a separately
allocated, fully zeroed frame, then map it user RX. Allocate separate initialized
R/NX constants, RW/NX data, and RW/NX stack pages. Leave adjacent stack guards
absent. The fixture reads constants, computes 42, uses its stack, writes data and
then executes the selected completion or security probe. No kernel executable
page becomes user-accessible; no filesystem, ELF parser or startup descriptor is
needed for this fixture. Its local entry contract does not freeze `NeuStartInfo`.

Use long-mode user CS/SS and an explicit `IRETQ` frame. Zero GPRs, establish a
canonical aligned user stack, set safe RFLAGS with IF/IOPL/DF clear, and start with
zero FS/GS selectors/bases and no TLS. Keep x87/SIMD unavailable through CR0.MP/TS
rather than expose previous extended state. Disable native `SYSCALL` and legacy
`SYSENTER` entry for the fixture. There is no production syscall dispatch.

Provide a separate guarded **32 KiB kernel privilege-entry stack** via TSS.RSP0;
retain the existing normal stack and double-fault IST1. A dedicated DPL=3
interrupt gate at `0x80` completes only this test. CPU faults retain their actual
vector/error/context. Classify ordinary user-origin faults by saved CS, preserve
RIP/RSP/CS/SS and CR2, and validate exact expected context without dereferencing
any user pointer. Privileged faults and critical NMI/#DF/#MC paths remain fatal.

A successful or expected-fault boundary abandons the user context and restores
the suspended kernel continuation on its original stack/root. Disable the test
gate, inspect the data result through its owned frame, and destroy the inactive
user hierarchy. Never resume an unchecked user frame.

Freeze translation changes while the user root is active. Deactivation flushes
local translations with PGE/PCIDE disabled. Clear all inactive table/leaf/workspace
references before releasing any user table; invalidate its remaining kernel
workspace alias before physical reuse. Drop mapping records only after those
translations are retired, then free backing identities. Preserve original kernel
tables and exact frame accounting. Inactive construction failures follow the
same teardown discipline. All bootloader-reclaimable RAM remains reserved.

Require QMP to observe **actual CPL=3** with the selected CR3, user RIP/RSP,
selectors and safe flags during a bounded fixture loop. Separately check the
CPU-saved trap/fault frame, the kernel entry stack and result, then verify CPL=0
halt under the original kernel root. Missing the observation is a failure.

## Alternatives considered

- **Share higher-half child tables.** Viable later, but needs explicit pins,
  parent counts across roots and coordinated modification/reclamation policy.
  Private tables make this first lifetime proof smaller.
- **Run kernel text at user privilege.** That weakens supervisor separation and
  does not demonstrate an independent executable fixture.
- **Build an ELF loader or userspace runtime now.** Adds parsing, packaging and
  ABI work unrelated to the immediate privilege-transition proof.
- **Use the suspended normal stack as RSP0.** Would overwrite live Rust frames;
  a separate stack preserves both contexts and the existing #DF emergency stack.
- **Implement `SYSCALL`/`SYSRET` now.** Hardware stack switching and native ABI
  design belong to Phase 2B; an explicitly temporary interrupt gate is bounded.
- **Treat a halt or serial banner as Ring 3 proof.** Neither establishes CPL.
  Direct QMP state plus hardware boundary frames provide separate observations.

## Consequences

- The fixture proves one independent unprivileged context, not Node lifecycle,
  cross-Node isolation, scheduling, production syscalls or stable executable ABI.
- Private tables add ten frames for this layout, plus four user backing frames.
  Existing bounded pools suffice; construction exhaustion is rolled back.
- User faults terminate the isolated fixture and return to trusted kernel cleanup.
  There are no signals, user recovery handlers or general user-copy operations.
- FS/GS stay TLS-free and extended state is unavailable. Supporting arbitrary
  user programs requires a separate CPU-state and ABI decision.
- The bounded observation loop introduces a host-observation deadline. A delayed
  host can fail the test; completion without a CPL=3 sample cannot pass.
- Physical hardware, SMP, side-channel defenses and production hardening are not
  established. The broader register entries retain their existing proposal scope.

## History

- **2026-10-25:** Accepted under the Phase 2A request. Original ADR 0006/0007
  reasoning remains historical; this record adds private user tables, RSP0 entry,
  origin classification and test completion. Actual commands, results and limits
  are in [implementation status](../IMPLEMENTATION_STATUS.md). Architecture v0.2
  and the native syscall/startup proposals remain unchanged.

- **2026-10-25, Phase 2B:** ADR 0009 enables owned native MSR configuration only
  in native test images, using the existing guarded stack with explicit software
  switching. Phase 2A still disables SCE and retains its original `int 0x80`
  cases. No native operation is retroactively attributed to this Phase 2A record.

Reference: [Intel SDM, Volume 3A](https://cdrdv2-public.intel.com/819714/253668-sdm-vol-3a.pdf),
U/S paging permissions, long-mode interrupt/IRET frames, TSS.RSP0 and CR0.MP/TS.

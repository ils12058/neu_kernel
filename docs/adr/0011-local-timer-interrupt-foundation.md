# ADR 0011: Establish a bounded local timer interrupt window

- **Date:** 2026-10-25
- **Status:** Accepted — K4 / Phase 4A, one CPU, isolated kernel timer tests only.
- **Extends:** [0006](0006-kernel-exception-descriptors-and-fatal-handling.md),
  [0007](0007-kernel-owned-paging-and-controlled-mappings.md),
  [0009](0009-experimental-native-syscall-and-trusted-return.md) and
  [0010](0010-independent-bootstrap-nodes-and-fault-containment.md).
- **Related NEU decisions:** NEU-003 (x86-64), NEU-011 (one CPU first),
  NEU-018 / MEM (device mapping boundary), NEU-020 / SCHED (clockevent subset),
  NEU-021 / SEC (entry/state ownership), NEU-023 / DIAG (fail-stop),
  NEU-024 / TEST (local hardware evidence). Full scheduling remains proposed.
- **Architecture:** [Section 17](../Neu_Kernel_Architecture_v0.2.md#17-scheduling-clocks-and-cpu-topology--neu-sched-001).

## Context

All existing runtime paths require IF=0. Exception entries are terminal and
cannot serve a returning timer interrupt. Native SYSCALL initially retains a
user RSP, uses static entry storage and a shared privilege stack, and suspends a
mutable bootstrap controller. Simply executing STI would expose those assumptions
and could asynchronously access unsynchronized paging, allocator or Node state.

We need real repeatable timer delivery before choosing a safe context-switch
mechanism. The first enabled window must exclude all existing Rust mechanisms
that depend on permanent interrupt masking.

## Decision

Discover the BSP local APIC through CPUID and IA32_APIC_BASE. Require the
verified single-logical-processor profile and an already hardware-enabled BSP
APIC; unsupported base/width/capability states fail before the enabled window. Prefer x2APIC only
when advertised and its enable/MSR interface can be configured and read back.
Otherwise use xAPIC through a dedicated supervisor RW/NX 4 KiB device mapping.
Select PAT index 3 with PCD=PWT=1 and PAT=0 only after verifying that PAT entry
is UC. Do not treat MMIO as allocator RAM or create a writable-back direct alias.
Verify APIC version, ID, available LVT entries, base/mode and programmed values.
This BSP-only discovery needs no ACPI table parser or IOAPIC routing.

Mask both legacy PICs and unused local APIC LVT sources. Reserve vector 0x30
for the periodic local timer and 0xff for spurious delivery. All other external
vectors have vector-specific fatal entries in this configuration. Preserve CPU
exceptions, #DF IST1 and existing test gates in their original configurations.
A real timer event requires the timer ISR bit. Acknowledge it exactly once with
EOI and verify the ISR bit clears. Spurious delivery requires no EOI; a software
INT to the timer vector must fail rather than count as hardware evidence.

Calibrate the divided APIC countdown against polled PIT channel 2 one-shot
intervals using the QEMU PC PIT reference (1,193,182 Hz). Use multiple bounded
samples, reject stopped/invalid/inconsistent counters, and derive an approximately
1 ms periodic initial count with checked arithmetic. TSC-deadline and host CPU
frequency are unnecessary. This is a provisional clockevent calibration, not a
nanosecond clocksource or a scheduler quantum implementation.

Use a dedicated returning assembly path: normalize the 64-bit hardware frame,
save all 15 GPRs, clear DF, align the Rust call stack, and restore the exact saved
stack/registers before IRETQ. The interrupt gate clears IF; no handler executes
STI. Supported enabled execution is only a bounded assembly loop on the guarded
normal kernel stack, at CPL=0 under the original kernel root. No userspace or
syscall execution is admitted. CPU exceptions and critical conditions remain
fatal. A future CPL=3 timer entry is not a supported resumption path.

The handler uses only atomic session/counter values, a bounded exclusively written
trace and its controller interface. It allocates nothing, blocks on nothing,
logs nothing and touches no paging manager, allocator or Node model. Setup and
trace inspection require IF=0. Publish configuration before opening the window;
use atomics for asynchronous state and keep trace borrows outside it. Unexpected
vectors, reentry, overflow, wrong context or inconsistent acknowledgment fail
closed. No scheduler lock or fake scheduling state is added.

Program and read back the masked timer with IF=0. Enable its periodic LVT only
when all handlers and state are ready, then enter the isolated STI window. Use
PIT output plus an instruction budget to bound the wait even if timer delivery
fails. Close with CLI, mask/stop the timer, then permit only a bounded drain
window for an already pending timer. Verify ISR/IRR empty and ticks stable with
IF=0 before teardown. Leave the local timer stopped, all LVT sources masked,
APIC software disabled and both PICs masked; finish at the existing IF=0 halt.
Repeat tests rearm only after pending delivery has drained. Do not restore an
unknown firmware interrupt service or globally change ordinary boot policy.

## Constraints before Phase 4B context switching

The initial scheduling direction is **userspace preemption with a kernel that is
not generally preemptible**. This is a prerequisite analysis, not an implemented
context switch or acceptance of the full scheduling proposal.

- A resumable Thread needs a complete saved GPR/RIP/RFLAGS/RSP/selector context,
  including whether entry came from Ring 3 or Ring 0. A kernel interrupt may
  request deferred work but must not arbitrarily switch a suspended Rust frame.
- Allocate and guard per-Thread kernel stacks, with explicit ownership and
  teardown. The shared syscall stack cannot hold two suspended syscall frames.
  Either finish non-preemptible syscalls before switching or replace it with a
  proven per-Thread entry-stack handoff. Protect the early user-RSP window.
- Switch the selected Node root only while all live kernel entry/continuation
  mechanisms and new stack are mapped. Keep the old resources referenced until
  no saved frame/CPU translation can use them; preserve scoped mapping ownership.
- Update TSS.RSP0, active Thread/Node/root, trusted return contract and any entry
  stack selector as one interrupt-safe handoff. Avoid observing mixed identities.
- Timer delivery during a syscall must only account/request `need_resched` (or
  equivalent) until a validated safe user-return boundary. Do not clear the native
  busy guard or overwrite its static session while a suspended entry exists.
- Define interrupt nesting, preemption-disable regions and deferred-switch
  ownership before exposing IF=1 to arbitrary kernel code. Paging transactions,
  scratch aliases, frame allocation, lifecycle and serial logging remain IF=0.
- Define x87/SIMD policy and save/restore any state userspace may use. Current
  integer-only execution with TS restrictions is not an extended-state switch.
- Replace single-global current/session/continuation assumptions with explicitly
  CPU-owned state before SMP. This milestone adds no SWAPGS, IPIs, AP startup,
  migration, cross-CPU TLB protocol or scheduler queue.

## Alternatives considered

- **Reuse fatal exception entry.** It has no returning register/stack protocol.
- **Enable interrupts around ordinary Rust bootstrap code.** Existing exclusive
  mutable ownership and native entry are not safe for that wider concurrency.
- **Use PIT IRQ0 as the scheduler event.** Requires legacy routing while leaving
  the proposed local clockevent untested. PIT is only a bounded reference here.
- **Assume an APIC frequency or require TSC deadline.** The former invents timing;
  the latter needlessly requires optional hardware. Calibration gives evidence.
- **Map xAPIC as WB RAM.** Incorrect device cacheability would undermine MMIO.
- **Switch Threads in the first timer handler.** Per-Thread stacks, context
  publication and syscall/preemption ownership are unresolved dependencies.

## Consequences

The enabled surface stays small and independently testable. Controller setup,
calibration, bounded delivery, EOI, IRETQ and quiescence can be checked without
claiming fairness, CPU accounting or user preemption. PIT/platform assumptions,
virtual-machine calibration noise and any untested controller mode must remain
explicit. Physical hardware, general ACPI/IOAPIC routing, nested critical-event
recovery and SMP require later work. Existing Node and ABI behavior stays IF=0.

## History

- **2026-10-25:** Recorded before timer implementation under the Phase 4A request.
  Acceptance chooses this scope; actual verification belongs in
  [implementation status](../IMPLEMENTATION_STATUS.md). Architecture v0.2 and
  previous ADR rationale remain unchanged.

References: [Intel SDM Volume 3A](https://cdrdv2-public.intel.com/819714/253668-sdm-vol-3a.pdf)
(APIC, EOI/spurious, PAT and interrupt frames),
[QEMU 11.0.0 PIT reference](https://github.com/qemu/qemu/blob/v11.0.0/include/hw/timer/i8254.h)
and [one-shot output semantics](https://github.com/qemu/qemu/blob/v11.0.0/hw/timer/i8254_common.c).

### 2026-10-25 — local verification

QEMU 11.0.0 TCG `-cpu max` advertises x2APIC and passes actual MSR-interface
configuration/readback and periodic delivery. A separate `max,x2apic=off` profile
passes xAPIC MMIO delivery with a QMP physical page-table walk confirming UC
leaf flags. Both use the existing q35/one-CPU/512 MiB UEFI profile. PIT calibration
is reproducible within the checked sample spread; delivered tick counts vary
under TCG and are not precise elapsed-time or CPU-time accounting. Spurious
handling is tested with a controlled software interrupt; unsupported capability
is a trusted pre-write injection, not a physical APIC-less machine test.

The freestanding [Rust target](https://doc.rust-lang.org/rustc/platform-support/x86_64-unknown-none.html)
uses the System V convention without a red zone and defaults to no floating-point
or vector-register code generation. The integer-only handler retains that policy;
this is not an x87/SIMD Thread context switch.

### 2026-10-26 — cooperative switching boundary

[ADR 0012](0012-cooperative-thread-context-switching.md) implements the cooperative
subset using private entry stacks and copied user contexts. It finishes every
kernel slice before FIFO selection; it does not schedule from this timer or
enable interrupts in native/user execution. These timer tests retain their
original enable/drain/stop policy. Deferred interrupt-driven switching belongs
to Phase 4C and still requires an explicit safe-entry/rescheduling contract.

### 2026-10-26 — Phase 4C extension

[ADR 0013](0013-single-cpu-user-preemption.md) admits IF=1 only for explicitly
preemptible user contexts and captures real timer-origin frames through the
common-stack boundary. Native kernel execution and resource/scheduler mutation
remain IF=0. Earlier fixtures retain their original behavior; the accepted ABI,
protection-domain and ownership directions remain unchanged. This extension does
not add general kernel preemption, SMP or public object/scheduler interfaces.

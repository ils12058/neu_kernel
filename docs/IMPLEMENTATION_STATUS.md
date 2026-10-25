# Neu Kernel implementation status

**Verified:** 2026-10-26. **Milestone:** K4 / Phase 4B, controlled single-CPU
cooperative Thread context switching. **Result:** complete against the supplied
Phase 4B criteria within the documented QEMU profile. The suite has **64 host
tests and 56 QEMU scenarios**, preserving all previous **52 host tests and 47
QEMU scenarios**. Two live Ring 3 Threads yield and resume real saved contexts
through FIFO selection. No timer-driven or general kernel preemption exists.

The inspected tree was clean on `main` at
[`0fba57e`](https://github.com/ils12058/neu_kernel/commit/0fba57e409bf600380b6544397ecbb303483c962).
The full original 52/47 suite was rerun before implementation in
`build/phase4b-baseline.log`; historical Phase 4A status remains at that commit.
Architecture v0.2, Rust/target, Limine and firmware are unchanged. Verification is
local single-CPU QEMU; hosted CI and physical hardware remain unverified.

## Created and modified files

Created:

- `kernel/src/context.rs`: owned x86-64 register/return context, identity and
  kernel-stack binding, initial/yielded validity rules and ABI-layout assertions.
- `kernel/src/scheduler.rs`: bounded ThreadId FIFO, admission, selection, yield,
  terminal removal and nine host tests; no privileged CPU/timer operations.
- `kernel/src/arch/x86_64/cooperative.rs`: trusted construction, dispatch,
  outcome/rollback/lifetime checks and scoped fault-injection controllers.
- `kernel/src/arch/x86_64/cooperative.S`: complete user-context restore, safe
  common-stack handoff and multi-stage payload with persistent stack witnesses.
- `docs/adr/0012-cooperative-thread-context-switching.md`: rationale and Phase 4C
  requirements, recorded before implementing the switching mechanism.

Modified:

- `kernel/src/node.rs`: Runnable state, saved context/stack/FIFO ownership,
  checked suspend/dispatch and terminal resource-reference guards.
- `kernel/src/paging/mod.rs`, `kernel/src/paging/tests.rs`: bounded context pins,
  owner-scoped supervisor stack mappings and three added lifetime/rollback tests.
- `kernel/src/arch/x86_64/paging.rs`: allocator-backed private guarded stacks,
  stack-table ownership, pin/unpin, teardown, high-water and owned result views.
- `kernel/src/arch/x86_64/native.rs` and `native.S`: selected trusted stack,
  per-slice binding, private yield and copied-context terminal handoff.
- `kernel/src/arch/x86_64/userspace.rs` and `userspace.S`: expose the common
  continuation storage and a typed saved-context bridge only for switching.
- `kernel/src/arch/x86_64/exceptions.rs`: validated TSS.RSP0 selection/readback.
- `kernel/src/syscall.rs`: equality derives for by-value context comparisons;
  existing numbers, feature/version/status values and assembly layout unchanged.
- `kernel/src/main.rs`, `kernel/src/lib.rs`, `kernel/src/arch/x86_64/mod.rs`,
  `kernel/Cargo.toml`: wiring and nine mutually exclusive switching scenarios.
- `tools/neu.py`: actual per-slice CPL/root/GPR/stack/TSS observations, page walks,
  saved-frame/FIFO/lifecycle checks and emitted transition instruction checks.
- `README.md`, this status, ADR index and ADRs 0009–0011: current behavior,
  verification and dated extensions preserving earlier rationale.

Timer implementation, linker, boot protocol, dependencies and toolchain are
unchanged. Architecture v0.2 stays byte-for-byte unchanged (SHA-256
`dede67c23c5b3044454e07356d262af2d8392b5e5a15a35651c0f960296b5cbd`).

## Tested environment and compatibility

| Component | Observed version or identity |
| --- | --- |
| Host | Apple Silicon / arm64, macOS 27.0.1 |
| Rust | `rustc 1.95.0 (59807616e 2026-04-14)`, pinned in `rust-toolchain.toml` |
| Cargo | `1.95.0 (f2d3ce0bd 2026-03-21)` |
| Kernel target | `x86_64-unknown-none`; bundled Rust ELF linker, no nightly toolchain or custom target JSON |
| Limine | Official `v12.9.4` binary release; archive and EFI SHA-256 pinned in `boot/limine.lock.json` |
| Protocol reference | `limine-protocol` commit `3a0526b700e356f0eac1b71a77697b3fd1c707a3`, base revision 6 |
| QEMU | `qemu-system-x86_64` 11.0.0, TCG, q35, `-cpu max`, 1 vCPU, 512 MiB |
| Firmware | Non-Secure-Boot EDK II/OVMF pair distributed with Homebrew QEMU 11.0.0; identified below by file hashes |
| Python | 3.14.7; runner uses only the standard library |

During Phase 0 the named Rust toolchain initially lacked the freestanding target; it was
added using `rustup target add --toolchain 1.95.0 x86_64-unknown-none`. No Homebrew
packages or system-wide firmware were installed for this milestone. Installed
QEMU supplied compatible firmware. ISO utilities and mtools were absent and are
not required by the selected boot workflow. Phase 4B adds no host packages,
nightly features, or third-party Rust dependencies.

The firmware pair selected from the installed QEMU distribution was:

```text
CODE: /opt/homebrew/Cellar/qemu/11.0.0/share/qemu/edk2-x86_64-code.fd
SHA256: 33090cc07675baa5190d9f1e84bf5176b33bcbfa9bacac522961150cdb6dbb2a
VARS: /opt/homebrew/Cellar/qemu/11.0.0/share/qemu/edk2-i386-vars.fd
SHA256: db792a6da367422c8db230c82125217305d7cbe059c4b4e75cb48fdcf29b1373
```

The variables template's `i386` filename is the distribution's naming convention;
the executable firmware is x86-64. Each launch copies variables to disposable
storage. The original firmware files are never written. Other QEMU/OVMF versions
have not been validated; the runner records the firmware hashes on each test run.

## Build and run

Prerequisites are Rust/rustup, Python 3, curl, Make, `qemu-system-x86_64`, and a
matching x86-64 OVMF code/variables pair. On a new machine, install missing tools
explicitly; the scripts do not install host packages. The Rust pin requires
`rustfmt`, `clippy`, and the `x86_64-unknown-none` target. For example:

```sh
rustup toolchain install 1.95.0 --profile minimal --component rustfmt --component clippy --target x86_64-unknown-none
make doctor
make build
make run
```

`make build` downloads the pinned official Limine archive on its first invocation,
verifies its SHA-256 and the extracted `BOOTX64.EFI`, builds with Cargo `--locked`,
and validates the kernel's ELF format, LOAD permissions, entry, stack sizes,
and page-aligned guard symbols.
Subsequent builds use the checked project-local cache. The kernel has no crates.io
dependencies. A first Rust toolchain/target installation and loader download need
network access; the populated cache permits subsequent local builds.

The boot artifact is an EFI directory, not an ISO or installed physical disk:

```text
build/boot/esp/
├── EFI/BOOT/BOOTX64.EFI
├── EFI/BOOT/limine.conf
└── boot/neu-kernel
```

`build/boot/manifest.json` records the kernel checksum, toolchain identity, ELF
entry/symbols, and Limine lock. The build also preserves the external loader's
license and third-party notices alongside the artifact.

`make run` boots a private snapshot of this directory as a read-only QEMU virtual
FAT disk. It uses virtio block boot I/O, `bootindex=1`, disposable NVRAM, no network,
no graphical display, and serial stdio. This host's QEMU rejects a read-only FAT
disk attached through the default IDE hard-disk device; virtio supports this
configuration. Explicit boot priority avoids the firmware's internal shell.
Boot disk I/O belongs to the firmware and Limine, not a kernel driver.

The normal kernel prints its diagnostics and remains in the interrupt-disabled
halt loop. Stop `make run` with **Ctrl-C**. The script prints the exact QEMU
argument list before launch.

Firmware discovery checks the installed QEMU share directory and common OVMF
locations. To select another compatible pair, supply both variables:

```sh
OVMF_CODE=/path/to/OVMF_CODE.fd OVMF_VARS=/path/to/OVMF_VARS.fd make run
OVMF_CODE=/path/to/OVMF_CODE.fd OVMF_VARS=/path/to/OVMF_VARS.fd make test
```

## Cooperative Thread scheduling and saved contexts

The new configurations are explicitly cooperative. A two-entry FIFO contains
**ThreadId**, not NodeId. Selection has no CR3, TSS, MSR, assembly or timer work;
the architecture controller supplies/captures execution values and applies the
selected context. The initial Thread remains Ready until its execution record,
stack and space pin exist. Admission makes it Runnable and records FIFO ownership.
Dispatch takes its saved context, clears queued ownership and makes it Running.
Yield saves a new context and appends it to the FIFO tail. Exit/fault removes
resumption eligibility. Empty FIFO returns no work; one eligible Thread can yield
to itself. Duplicate, stale, Running and terminal admission are rejected.

Node and Thread identities remain separate generation-checked types. Node
Ready/Active reflect this bounded prototype's inactive/selected slice; normal exit
ends the Node only after the final live associated Thread. A supported fault
terminates its own Node. One initial Thread per Node is still a construction
policy. There is no public creation, priority, affinity, wait queue or capability
implementation.

`SavedContext` owns a **copy** of all 15 GPRs, user RIP/RSP/RFLAGS and CS/SS, plus
NodeId/ThreadId/SpaceId/root and kernel-stack binding. Its 160-byte `Frame` has
120 bytes of GPRs followed by the 40-byte return frame; RAX offset is 112, return
RSP offset is 24. Layout assertions, host tests and QMP reads of the actual native
stack frame check this assembly boundary. Metadata is kernel-only, not a new
userspace ABI structure or a pointer to an abandoned stack frame.

Fresh contexts are constructed with the initial entry/RSP, flags=2 and zero GPRs;
native preflight requires that exact initial record. Yielded
contexts require matching ownership, valid canonical code/stack ranges, fixed
user selectors, permitted flags, successful RAX/RDX outputs and coherent RCX/R11
return RIP/flags. The native layer additionally seals the exact RX resume site
and verifies live mappings/root/stack before entry. A rejected saved record never
activates its root or configures a native session. Integer-only execution, zero
FS/GS bases and MP/TS restrictions remain; no extended-state context is exposed.

The private test operation **`0xffffffffffffff00`** yields only in switching
configurations. It eventually resumes with RAX=0/RDX=0 and the original ABI's
preserved-register convention. It carries no Thread/Node/stack selector argument.
`abi_query=1`, `thread_exit=2`, tag `0x100000003` and advertised 0.1 features remain
unchanged. The core dispatcher still returns NOT_SUPPORTED for this private
number; a public yield number/feature requires another compatibility decision.

## Private entry stacks, pins and switching order

Each Thread owns two allocated backing pages (8 KiB) through its Node's existing
SpaceId, mapped supervisor RW/NX only in that Node's root. The fixed arena is:

| Thread slot | Low guard | Mapped stack | High guard |
| --- | --- | --- | --- |
| A / slot 0 | `0xffffc00000000000` | `0xffffc00000001000..0xffffc00000003000` | `0xffffc00000003000` |
| B / slot 1 | `0xffffc00000004000` | `0xffffc00000005000..0xffffc00000007000` | `0xffffc00000007000` |

Guards are absent 4 KiB pages; the table's range ends are exclusive. Full Thread
identity, SpaceId and root bind the stack rather than raw slot numbers. The
common kernel root maps neither private stack, and each user root maps no sibling
stack. Stack backing is zeroed before its initialized frame identity, then filled
with a high-water pattern. The lowest 512 bytes must remain untouched after every
slice, in addition to the hardware guards. This bounds the tested scalar-handler
footprint; it does not establish a universal kernel stack size for future calls.

Each stack adds three private table frames. Those new table frames have aliases
only in the **common kernel workspace**, never in a user-root workspace. Existing
user-root aliases remain its original private table subset. Backend table access
still requires the actual common kernel CR3. Removing one stack therefore cannot
leave borrowed table aliases in another root. PTE removal, local invalidation and
detachment/flush precede table and backing reuse.

A queued/running/suspended context holds a space-lifetime pin. Pins block mapping
removal, owned-frame release, stack teardown and root retirement even while that
root is inactive. There are at most two pins per scope; stale pins, underflow,
capacity overflow and active-root unpin are rejected. Thread metadata separately
refuses reclamation with queued, active, saved-context or stack associations.
The trusted controller releases pins only after terminal state on the common
stack. Historical diagnostic numbers do not authorize resumption.

The current layout uses A's four user frames, B's five and four stack frames:
**thirteen data owners**, within the unchanged limit sixteen. Ten original private
tables plus three stack tables per root fit the 64-table bound. Mapping records
remain bounded to 64, and Nodes/Threads/live user spaces remain two each. No pool
increase, heap, static per-Thread stack reservation or MemoryObject is introduced.

Every dispatch occurs under the common root with IF=0:

1. Consume the FIFO's exact live Thread context; verify Node/SpaceId/root, full
   saved record, stack ownership/permissions/guards and return contract.
2. Establish the native critical IST contract. Publish the validated kernel-only
   selected-stack pointer and read back the same value in TSS.RSP0. Configure and
   read back MSRs; bind the current Thread/Node/root before permitting user entry.
3. Activate the verified root while still on the permanent normal image stack,
   mapped in both roots. Copy the saved frame onto that Thread's stack and restore
   GPRs/return state through validated IRETQ.
4. Native entry stores user RSP as a scalar, then loads the selected supervisor
   stack pointer **before any push/call/user-stack access**. No unchecked user
   argument selects it. Query/exit retain the established validation and statuses.
5. On yield, copy the successful user frame into a bounded kernel handoff record.
   Finish logging before abandoning that entry call chain. Move RSP to the common
   image stack, release the busy guard there, then restore common CR3 through the
   original bridge. No suspended Rust call chain remains on a private stack.
6. Restore/read back original MSRs, reset TSS.RSP0, deactivate root metadata and
   clear selected-stack/saved-user-RSP/session storage. Verify stack margin, then
   publish Runnable/terminal state and perform the next FIFO selection.

The common continuation is live for only **one slice**, and is consumed before
another starts. It holds normal Rust controller/paging borrows, mapped permanently
in every root; handlers access only copied session values and their own frames.
No references into abandoned Thread stacks or a switched-out address space remain.
#DF IST1 and native NMI/#MC IST2 retain separate guarded image-reserved stacks.

On terminal exit/fault, discard the resumable record and FIFO eligibility, release
the lifetime pin, unmap/free the private entry stack, clear its model association,
destroy the inactive user root/backing, then release Node/Thread metadata. Outcome
retention survives resource release until metadata-slot reuse. Invalid saved-state
rejection is distinct from an actual CPU exception. Unexpected privileged faults
remain fatal; none of this promises arbitrary kernel-corruption recovery.

## Demonstrated user resumption

Each payload reserves 24 bytes on its private user stack and retains three
Node-specific 64-bit markers across calls/yields. Four different code regions
perform seed+7, seed+14, seed+21 and seed+28, advancing a private progress word.
A uses seed 35 and ends at **63**; B uses 77 and ends at **105**. B also checks its
private-only sentinel at `0x406000`. Identical user VAs map distinct owned frames.

Yield occurs through a CALL/SYSCALL wrapper. Saved RIP is its post-SYSCALL RET;
saved RSP `0x404fd8` points to the **later-phase return address on that Thread's
user stack**. Resumption restores this actual frame and RET continues at the
previously saved caller site. QMP reads the stack return address for middle
yields and then observes later phase-specific instruction ranges, stable markers,
progress and canaries under the same owner/root. Restarting the entry point
would fail those CPU/data/phase checks even if serial names alternated.

The positive FIFO order is A0, B0, A1, B1, A2, B2, A3, B3. Every non-initial
dispatch consumes exactly the previous saved full frame. Normal A reclamation
occurs before B's final resume; faults also reclaim only the offending Node while
the healthy saved context remains pinned and later progresses alone.

The final `switch` run recorded the following actual QMP CPU/data samples. Every
row had CPL=3, CS/SS=`0x3b/0x33`, IF=0 and user RSP=`0x404fe0`:

| Dispatch | Thread / phase | User RIP | CR3 | Private result / progress |
| --- | --- | --- | --- | --- |
| 1 | A / 0 | `0x4000f2` | `0xc000` | 42 / 1 |
| 2 | B / 0 | `0x4000f2` | `0x20000` | 84 / 1 |
| 3 | A / 1 | `0x40017c` | `0xc000` | 49 / 2 |
| 4 | B / 1 | `0x40017c` | `0x20000` | 91 / 2 |
| 5 | A / 2 | `0x400211` | `0xc000` | 56 / 3 |
| 6 | B / 2 | `0x400211` | `0x20000` | 98 / 3 |
| 7 | A / 3 | `0x400270` | `0xc000` | 63 / 4 |
| 8 | B / 3 | `0x400270` | `0x20000` | 105 / 4 |

TSS.RSP0 was `0xffffc00000003000` for A and `0xffffc00000007000` for B;
their actual native frame RSPs were `0xffffc00000002f60` and
`0xffffc00000006f60`. Saved yield RIP/RSP were `0x4002d9/0x404fd8`.
The QMP frame at A's phase-1 yield contained the next caller return address
`0x4001c9` on its user stack, followed by the later phase-2 CPU observation.
Final allocation/free/table counts returned to **7 / 118385 / 7**, equal to that
image's baseline. These are QEMU observations for this build, not fixed physical
addresses or hardware guarantees; the checker resolves symbols and compares
per-image baselines on each run.

## Controller and clockevent foundation

Timer code is compiled only by `timer-test` configurations. The normal kernel,
all supervisor negative tests and all Ring 3/Node/native cases retain IF=0.
The timer profile uses no Node or Thread and performs no context switch.

CPUID.1 APIC/x2APIC/PAT support, physical-address width and IA32_APIC_BASE select
the BSP controller. Require a hardware-enabled BSP and one logical processor.
CPUID's logical-count field is used only when HTT is set; HTT clear means one
logical processor even if EBX's count field is zero. QMP also records `-smp 1`.
An unsupported capability/base/width fails before the enabled window.

The verified QEMU 11 `max` profile advertises x2APIC. Neu transitions
IA32_APIC_BASE from `0xfee00900` to `0xfee00d00`, reads it back and uses MSRs at
`0x800 + register_offset/16`. Actual APIC ID is 0, version `0x50014` (six LVT
entries), physical width 40. The separate `timer-xapic` profile uses
`-cpu max,x2apic=off`, making the same discovery policy select MMIO. These values
are observed platform details, not a portable CPU/base promise.

For xAPIC, only the discovered 4 KiB controller page is mapped at
`0xffffb00000000000`, supervisor RW/NX. PAT index 3 must contain UC type 0;
PWT=PCD=1, PAT=0 selects it. The observed PAT MSR is `0x0000010500070406` and
initial leaf is `0x80000000fee0001b`. A QMP physical walk independently checks
all four actual entries and effective supervisor/cache flags (hardware A/D bits
may subsequently change). Reject any overlap with described non-reserved RAM,
loader/kernel backing or framebuffer. Device physical memory is neither an
allocator frame nor a data-frame identity. No HHDM/device WB alias is created.
Three temporary page-table frames are released after stopping; seven pinned
kernel tables remain. Existing 64-table/16-data-owner/64-mapping bounds stay fixed.

This is BSP-local discovery, so no MADT/RSDP parser, ACPI interpreter, IOAPIC route,
MSI or userspace IRQ interface is needed. Mask both PICs (ports 0x21/0xa1,
read back 0xff), all unused LVT entries and the timer before opening IF. Check
APIC version, ID, masks, SVR, TPR, divisor, initial count and absence of inherited
ISR/IRR events. Setup rejects an unknown pending firmware APIC service.

Reserve **vector 0x30 (48)** for the local periodic timer and **0xff (255)** for
spurious delivery. All external vectors have dedicated DPL0 interrupt-gate stubs
in timer configurations, so unexpected delivery retains its actual vector and
fails closed. Exception vectors 0–31 and #DF IST1 are unchanged. Test-only user
gates retain their original configurations and interrupt policy.

Calibrate the divided APIC countdown with **three PIT channel 2 mode-0 one-shot
samples**, each 11,932 PIT input ticks (about 10 ms). The reference is the QEMU PC
PIT's **1,193,182 Hz** oscillator, not the host Mac frequency. Speaker output is
disabled and the channel is polled rather than routed as IRQ0. Divisor 16 is
programmed explicitly; reject missing/stopped counters, inconsistent sample
spread, invalid periods and checked-arithmetic overflow. The median divided
frequency determines a rounded **1,000 microsecond** initial count. TSC deadline
is not required or used.

This establishes an approximate periodic **clockevent**, not a monotonic
nanosecond clocksource, scheduling quantum, timer manager or CPU-time accounting.
Delivered ticks count acknowledged events and remain monotonically increasing
across repeated windows. TCG delivery/coalescing and debugger pauses affect event
counts; do not derive precise elapsed nanoseconds from them. The proposed 10 ms
scheduling quantum is still unimplemented.

## Interrupt entry, ownership and shutdown

Each stub pushes zero error and its actual vector, then saves all 15 GPRs. The
64-bit CPU frame supplies RIP/CS/RFLAGS/RSP/SS. Assembly clears DF, retains the
saved frame pointer in a preserved register, aligns RSP for Rust, then restores
the original frame/GPRs and executes IRETQ. The existing freestanding Rust target
uses SysV without a red zone or floating-point/vector-register code generation;
no x87/SIMD context management is introduced.

Only `neu_timer_wait` executes with IF=1, on the guarded 64 KiB normal kernel
stack under the original kernel CR3, CPL=0, FS/GS bases zero. The loop uses PIT
output and a separate ten-million-iteration budget, so missing timer delivery
cannot cause an unbounded HLT wait. It checks all 15 GPR canaries and exact RSP
after CLI. QMP independently samples 13 stable GPRs during the loop; RAX/RDX are
temporarily used for the port read and are checked after return by assembly.
Hardware frame RIP/RSP must lie within that exact assembly window/stack span.
A timer interrupt from CPL=3 or unrelated kernel code is not a supported return.

Interrupt gates clear IF; handlers never execute STI. A guard rejects reentry.
The handler verifies its trusted stack/frame, actual root, saved privilege/IF,
current IF=0/DF=0 and running/draining phase. A timer event requires its **actual
APIC ISR bit**. It writes **exactly one EOI**, verifies ISR clears, records the
hardware frame and publishes the next tick. Software INT 0x30 without an ISR bit
is rejected before accounting. Spurious 0xff returns without EOI or timer count.
Unexpected vectors halt, leaving IF=0 and the timer stopped/masked; they never
masquerade as ticks. Exceptions receive no timer EOI.

Immediate work is bounded: classification, checks, controller acknowledgment,
atomic counters and one fixed event record. There is no ordinary logger,
allocation, blocking, scheduler lock, deferred switch or mutable paging/allocator/
Node access. The fixed trace has 256 entries in reserved kernel BSS. Overflow
fails closed, including before a u64 tick wrap. Atomic mode/root/phase state is
published before enable; ticks publish completed records with Release/Acquire.
One non-nested CPU writes raw trace slots; setup/inspection create no trace
reference during IF=1. Existing exclusive mutable kernel objects are untouched
by the handler even when their trusted caller remains suspended.

For each session, configure/read back the masked periodic timer with IF=0,
publish Running, unmask and enter the assembly window. PIT interval 59,659
bounds the enabled measurement (about 50 ms). Return with CLI, mask the LVT and
write initial count zero, then publish Draining and open a separate bounded
2,386-PIT-tick window to acknowledge any already pending timer event. No new
periodic count is armed while draining. Verify ISR/IRR empty, no busy entry,
unchanged CR3 and equal tick/EOI totals. A further IF=0 reference interval must
leave ticks unchanged. Repeat sessions rearm only after this quiescence check;
the global event count does not reset.

Final shutdown keeps all LVT sources and both PICs masked, timer initial/current
counts zero, TPR=0xff and SVR software-enable clear. It restores IF=0, removes the
xAPIC device mapping if present, checks exact allocator/table accounting and
uses the established controlled halt. APIC hardware enable/x2 mode is retained;
unknown firmware services and PIT channel programming are not restored. PIT
channel 2 ends gated off with speaker disabled. Fatal invariant paths halt rather
than promise device/memory rollback after arbitrary kernel corruption.

Earlier native/Node configurations retain their shared entry-stack and IF=0
contract. Cooperative configurations use the private stacks and common handoff
above. No configuration schedules from a hardware timer. The
[Phase 4A prerequisite analysis](adr/0011-local-timer-interrupt-foundation.md#constraints-before-phase-4b-context-switching)
remains historical rationale; [ADR 0012](adr/0012-cooperative-thread-context-switching.md#phase-4c-prerequisites)
records the remaining timer-preemption boundary.

## Internal Node and Thread model

`node::Nodes` has separate two-slot Node and Thread arrays. `NodeId` and
`ThreadId` are distinct Rust types with independent slot/generation validation;
their diagnostic integer encodings may coincide across types. A Node records its
state, address-space identity and optional outcome. A Thread records its own
state, associated NodeId, root/SpaceId, entry/RSP and outcome. There is no embedded
permanent one-Thread field inside Node and no capability-space placeholder.

The trusted controller owns each actual `UserSpace` resource handle separately
from the model's checked association. Only it constructs bootstrap Nodes; there
is no public NodeFactory or user-supplied Node selector. Node-local capability
governance remains incomplete.

| Object | Lifecycle and terminal rule |
| --- | --- |
| Node | Created → Configuring → Ready ↔ Active → Exited → Reclaimed. Construction failure can exit Created/Configuring. |
| Thread | Configuring → Ready → Runnable ↔ Running → Exited → Reclaimed; earlier run-once fixtures go directly from Ready to Running. One initial Thread per Node is current bootstrap policy. |
| Normal exit | Ends the bound Thread. Node exits only after its final live associated Thread ends; a model test checks that rule independently of bootstrap policy. |
| User fault | Ends only that Node's associations, copies actual fault metadata, returns to the trusted controller without resuming the Thread. |
| Construction failure | Separate `ConstructionFailure` outcome; owned paging resources roll back before model reclamation. |
| Reclamation | Requires Exited, no active/live/queued/saved context, released kernel stack and released address-space association. Outcomes remain until metadata-slot reuse. |

`ExitReason` separates `Normal(u8)`, `UserFault(Fault)` and
`ConstructionFailure`. `Fault` distinguishes actual CPU exceptions from rejected
return state and copies vector, error, RIP, RSP, CS, SS, RFLAGS and CR2. No
user-memory diagnostic structure is dereferenced. Generation exhaustion refuses
reuse rather than wrapping. FIFO policy is implemented separately; this remains
a small internal model without a public object API or multithreaded Nodes.

## Experimental Neu syscall contract

The central definition is `kernel/src/syscall.rs`; the native assembly fixture
receives constants directly through Rust `global_asm!` operands. Absolute ELF
symbols expose the emitted values to the host verifier. Numbers and version
encoding remain **experimental**, without POSIX/Linux compatibility or a frozen
wire layout.

| Register/field | Contract |
| --- | --- |
| RAX on entry | Unsigned 64-bit operation number: `abi_query=1`, `thread_exit=2`. |
| Arguments | Six unsigned 64-bit values in RDI, RSI, RDX, R10, R8, R9. Unused arguments are ignored. |
| RAX on return | Signed 64-bit status encoded in two's complement: success 0, `NEU_NOT_SUPPORTED=-1`, `NEU_INVALID_ARGUMENT=-2`. |
| RDX on return | Scalar output, zero on errors. Query returns `0x0000000100000003`. |
| Preserved state | RSP and all other GPRs except RAX/RDX outputs and RCX/R11 hardware clobbers. Representative 64-bit RBX/RBP/R12–R15 and argument-register canaries are tested. |
| RCX/R11 | Caller-clobbered; current return leaves hardware-saved user return RIP/flags, never kernel addresses. |
| Version/features | Bits 63..48 major 0, 47..32 minor 1, 31..0 features: query bit 0 and exit bit 1. |

`abi_query` has no pointer argument or memory output. The payload checks its
status and complete scalar tag. An unknown operation returns -1/RDX=0 and safely
resumes Ring 3. `thread_exit` checks RDI before narrowing: 0..255 terminates the
currently bound initial Thread; 256 or a negative value represented as unsigned
64 bits returns -2/RDX=0. Other arguments are ignored. The trusted active binding
supplies Node/Thread identity; userspace supplies only the scalar exit status.
The conformance cases require status 0. Normal exit ends the Node only when no
live associated Thread remains. ABI 0.1 and the original single-fixture cases
are unchanged; there is no public Node-management syscall.

The internal `Registers`/`ReturnContext`/`Frame` C layouts serve assembly only,
not user-memory ABI structures. No user copying, object syscall, capability table,
startup descriptor, ELF parser or runtime is introduced.

## CPU configuration, stack and FS/GS invariants

The native image requires CPUID SYSCALL support and initially disabled SCE. It
owns a snapshot of EFER/STAR/LSTAR/FMASK, configures and reads them back before
user entry, and checks them again during dispatch. Unrelated EFER bits, including
long mode and NXE, remain intact.

| State | Actual selected contract |
| --- | --- |
| EFER | Prior value OR SCE; observed `0xd01` during native execution, `0xd00` after cleanup. |
| STAR | `0x0028000800000000`: kernel CS 0x08, derived SS 0x10; upper base 0x28 corresponds to user SS 0x33 and long-mode CS 0x3b. |
| LSTAR | The supervisor RX `neu_native_entry` symbol, checked against the ELF and hardware MSR readback. |
| FMASK | `0x77700`: TF/IF/DF/IOPL/NT/RF/VM/AC masked on entry. Assembly also clears DF defensively before Rust. |
| Legacy SYSENTER | CS MSR 0x174 is zero; no legacy service is enabled. |
| FS/GS | Selectors/bases and KERNEL_GS base zero, no TLS or kernel GS/per-CPU pointer, no SWAPGS. User return restores DS/ES 0x33. |
| Syscall stack | Earlier fixtures use the separate guarded 32 KiB image stack. Switching fixtures use their selected private guarded 8 KiB stack. Only the active common continuation remains on the permanent 64 KiB normal stack. |
| Critical entry | Native builds give NMI/#MC a separate guarded 16 KiB IST2; #DF retains its original 32 KiB IST1. |

SYSCALL does **not** choose TSS.RSP0. The first instructions read/set a
supervisor-only RIP-relative non-reentrant guard, store user RSP as a scalar,
and select the trusted stack: LEA for earlier fixtures, or a validated published
kernel pointer for switching fixtures. There is no push, call, Rust or
user-address dereference before that switch. The noncanonical-user-RSP integration
case reaches this actual trusted stack and is then rejected, demonstrating that
entry did not rely on the user stack being usable.

After switching, assembly saves every GPR and constructs a 160-byte private
return frame. RIP and flags originate in hardware RCX/R11; RSP was saved by the
entry sequence. User CS/SS are **synthesized** from the selected contract, because
SYSCALL does not save their old values. The direct CPL=3 snapshots provide
independent privilege evidence rather than treating synthesized selectors as
hardware proof. Stack alignment is 16 bytes before calling ordinary Rust.

Native entry clears FS/GS bases before Rust. The Phase 2A interrupt path retains
its original zero-base behavior. RIP-relative entry state avoids a SWAPGS state
window for this single-CPU design; it cannot be reused as an SMP or TLS design.

## Dispatch, validated return and terminal exit

The Rust handler accepts only the exact kernel-stack frame address. It checks
actual kernel CS/SS, stack bounds, masked flags, MSRs, the non-reentrant guard,
and the still-live user root before processing copied scalar register values.
The snapshot is preserved in supervisor-only `NEU_NATIVE_LAST`.

Return validation requires:

- The expected sealed address-space root.
- CS 0x3b/SS 0x33, never privileged selectors.
- Canonical lower-half RIP inside fixture RX bytes and at an approved post-SYSCALL
  resume site. This prototype deliberately supports only the fixture's sites.
- Canonical, word-aligned RSP inside the live user stack, excluding both guards.
- Required flag bit 1 and only arithmetic flags, mask `0x8d7`; unsafe direction,
  trap, privilege and reserved flags are rejected.

Validation runs before dispatch and is rechecked before returning. The returning
path writes only the trusted kernel frame, restores DS/ES and saved registers,
then uses **IRETQ**. No SYSRETQ instruction or unchecked user frame is used.
The dispatcher changes only RAX/RDX; the ABI preservation check and user-side
canaries separately verify the return contract.

Successful `thread_exit` takes a separate nonreturning path: record the terminal
status, restore the trusted suspended kernel continuation/root, restore/read back
the original MSRs, then inspect owned result backing and reclaim the inactive
fixture. No user instruction after successful exit is executed. Unsafe return
states also terminate the isolated fixture before IRETQ; malformed user values
are never dereferenced as privileged pointers.

The native configuration leaves the Phase 2A completion gate DPL=0. Original
`test-user*` images still enable their DPL=3 `int 0x80` gate and keep SCE disabled.
Ordinary builds launch no user fixture and install no native syscall service.

## Address-space ownership and cleanup

`SpaceId` is a generation-checked identity for one of two live user scopes.
Mappings are indexed by `(SpaceId, VA)`, not a global VA. Each user frame has
exactly one owning scope; duplicate physical ownership, mapping another scope's
frame, or passing a root that does not match the scope is rejected. Mapping
references prevent premature release; active metadata prevents mutation/teardown
until the trusted return has restored the kernel root and deactivated the scope.
Existing kernel/single-tree wrappers use the explicit kernel scope.

Each Node has a fresh PML4 and private children for user and supervisor mappings;
no kernel or sibling child tables are borrowed. The permanent kernel image
backing remains supervisor-only RX text, R/NX constants and RW/NX data/stacks.
Kernel table workspace aliases cover all live owned tables for construction and
inspection. Each user root maps **only its own table aliases**, supervisor RW/NX.
Backend table inspection/mutation requires actual CR3 to be the original kernel
root. A sibling's creation/reclamation therefore never leaves borrowed workspace
aliases in the surviving root. The scratch slot is absent in both user roots.

| User VA | A | B |
| --- | --- | --- |
| `0x400000` | Private RX payload frame | Different private RX payload frame |
| `0x401000` | Private R/NX constants, seed 35 | Different R/NX constants, seed 77 |
| `0x402000` | Private RW/NX data, result 42 in run-once fixtures / 63 after four switching stages | Different RW/NX data, result 84 / 105 respectively |
| `0x403000` | Absent low stack guard | Absent low stack guard |
| `0x404000` | Private RW/NX stack | Different RW/NX stack |
| `0x405000` | Absent high stack guard | Absent high stack guard |
| `0x406000` | Absent | Private R/NX sentinel `0x5152535461626364` |

The following sizes and CALL offset describe the earlier run-once fixtures;
Phase 4B's additional private stacks and three user-stack witnesses are detailed
above. All allocated backing is zeroed before initialization/publication. Initial RSP is
`0x404ff8`; the syscall wrapper CALL gives saved RSP `0x404ff0`. The payload
checks the complete query output and register canaries, stores seed+7, and B
actually reads/checks its private-only sentinel. Fixture values are test data,
not ABI requirements. No user code shares the kernel payload's physical backing.

Both scopes are constructed and verified before either executes. The run-once
profile needs ten private table frames per root, A's four backing frames and B's
five: 29 domain frames plus seven pinned kernel table frames. Static guarded
entry/emergency stacks remain image-reserved. Switching adds two backing pages
and three private table pages per Thread (46 total allocations with both live,
including the seven common kernel tables). The existing bounds are unchanged:
64 table frames, 16 data owners, 64 controlled mappings. Newly explicit bounds
are two live user scopes, two Nodes and two Threads. A third scope is rejected
without allocation changes; host tests also cover exhausted generations/data
capacity. **All bootloader-reclaimable memory remains reserved.**

The native session binds one copied `ActiveContext` (NodeId, ThreadId, SpaceId,
root, entry, RSP). It resets counters/observation state, requires a clear busy
guard, and validates the root and existing sealed return contract at dispatch.
Only one Thread can be active. No handler accesses the controller's suspended
mutable model/paging borrow. A terminal path copies an outcome by value, restores
the original kernel continuation/root, restores/read-backs MSRs, checks the clear
guard and deactivates scope metadata. The controller then updates lifecycle and
inspects owned backing. Each later execution gets a fresh binding and contract.

Supported noncritical CPL=3 exceptions copy the real hardware frame/CR2 and
terminate the bound Node. CPL=0 faults and NMI/#DF/#MC retain the existing fatal
policy. Diagnostic values never become user pointers. Fault teardown follows the
same inactive-resource path as normal exit; a faulting Thread is not resumed.

Reclamation clears all of that scope's hierarchy/workspace references, reloads
the kernel CR3 with PGE/PCIDE disabled, removes only its mapping records, then
releases its table aliases/frames and user frame identities. Only then clear the
root binding, retire SpaceId and reclaim Node/Thread metadata. Per-owner deltas
and surviving hierarchy/reference checks replace the singleton global-baseline
assumption. The final two-Node cleanup must still return exact global accounting.
Construction rollback follows this order for partial roots, including before
root allocation, while preserving the live sibling. Unexpected kernel invariant
failures halt with ownership retained; arbitrary fatal paths are not claimed to
restore accounting or recover safely.

## Actual validation and results

Executed from the repository root:

```sh
make doctor > build/phase4b-doctor.log 2>&1
make check > build/phase4b-check.log 2>&1
make build > build/phase4b-build.log 2>&1
make test > build/phase4b-test-run.log 2>&1
cargo test --locked --lib --target aarch64-apple-darwin
python3 tools/neu.py test --case switch
python3 tools/neu.py test --case switch-a-fault
python3 tools/neu.py test --case switch-b-fault
python3 tools/neu.py test --case switch-cross
python3 tools/neu.py test --case switch-stack-user
python3 tools/neu.py test --case switch-stack-guard
python3 tools/neu.py test --case switch-invalid-context
python3 tools/neu.py test --case switch-rollback
python3 tools/neu.py test --case switch-stale
```

All nine focused switching cases passed before the complete suite. A clean source
copy excluding `.git`, `.cache`, `build`, `target` and Python caches also ran:

```sh
make build
python3 -c "from tools.neu import build; build('switch')"
python3 -c "from tools.neu import build; build('switch-rollback')"
```

All three cold builds pass with a fresh pinned-loader download/checksum check,
Cargo `--locked`, ELF64/x86-64 four LOAD segments, W^X segment checks, guarded
stacks and EFI artifacts. The harness also validates cooperative payload bounds
and actual SYSCALL (`0f 05`) and IRETQ (`48 cf`) instruction bytes. No host packages
or external Rust dependencies were installed.

| Check | Actual result |
| --- | --- |
| Doctor | Pass: unchanged Rust/target, QEMU and hashed OVMF pair. |
| Static checks | Pass: formatting and Clippy with warnings denied for 56 kernel configurations plus host library; Python syntax passes. |
| Host tests | **64 pass**: all original 52 plus 9 FIFO/context/lifecycle tests and 3 pin/stack-mapping/rollback tests. |
| Original QEMU cases | **All 47 pass** with their original intent: supervisor/#DF, Ring 3/ABI negatives, two-Node isolation and all 7 timer cases. |
| New switching cases | **All 9 pass**, described below. |
| Final halt | **All 56 pass** two QMP HLT=1/CPL=0/IF=0 observations inside the ELF halt loop, original owned CR3 and host-controlled quit/exit 0. |

| Switching case | Verified behavior |
| --- | --- |
| `switch` | Both Threads queued before entry; eight real A/B slices, three yields each, exact saved-context restoration, four stages and normal results 63/105. |
| `switch-a-fault` | A resumes phase 1 and raises actual #UD vector 6/error 0 while B is suspended; A is reclaimed and B resumes/completes. |
| `switch-b-fault` | B raises actual #UD after resumption while A is suspended; A later completes alone. |
| `switch-cross` | Resumed A reads B-only `0x406000`: actual user #PF vector 14/error 4/CR2=`0x406000`; B's saved context and sentinel survive. |
| `switch-stack-user` | A attempts its mapped supervisor entry-stack bottom: actual #PF 14/error 5, CR2=`0xffffc00000001000`; B resumes normally. |
| `switch-stack-guard` | A reads its absent low guard: actual #PF 14/error 4, CR2=`0xffffc00000000000`; B resumes normally. QMP also checks both guards absent. |
| `switch-invalid-context` | Trusted hook substitutes noncanonical saved RIP for A's second dispatch; rejected before activation/MSR setup/IRETQ. B resumes. This is rejected saved state, not a fabricated hardware exception. |
| `switch-rollback` | Five stack allocation failure points plus an invalid initial context roll back while A is queued/pinned. A's saved state is unchanged; both then alternate/exit normally. |
| `switch-stale` | After A's normal early reclamation, a real temporary Node/Thread reuses its metadata slot; the old ThreadId is rejected. Temporary resources are released while B remains suspended and then resumes. |

QMP pauses/resumes each slice briefly to observe actual CPL=3 in that phase's
user loop and CPL=0 on its selected native stack (fault-only slices instead
supply the actual exception frame). It checks root, selectors, flags, MP/TS,
zero FS/GS, full-width canaries, progress/data and persistent user-stack markers.
It reads actual TSS.RSP0, selected-stack/guard storage and common continuation
RSP, and physically walks the private stack pages/guards under each root.
Native samples include all twenty actual frame words; saved/taken records are
compared across every yield, and later user return addresses/instruction regions
prove resumption rather than restart.

The host independently simulates FIFO selection and verifies per-Thread
identity/Node/SpaceId/root, query/yield/exit counts, MSR setup/restoration, exact
fault RIP/vector/error/CR2/CS/SS/RSP/flags and private handler-stack bounds. It
checks survivor retention after sibling teardown, terminal dispatch rejection,
rollback/stale-generation evidence, empty final FIFO and numerical allocation
accounting. Native busy/selected-stack/saved-user-RSP/common-continuation storage
must all be clear at final halt. Initial serial names or pass labels alone cannot
pass. Missing observations, resets, unexpected vectors, early exits, triple
faults or the 30-second guest timeout fail the test.

The first new host-test run passed 63/64: its wrong-owner yield test expected
Association while the earlier duplicate-entry guard correctly returned InUse.
The assertion was corrected without weakening ownership/state checks; final host
and QEMU results above are subsequent runs. The independent Phase 4A checks and
older execution paths remain as follows.

| Timer case | Actual verified behavior |
| --- | --- |
| `timer` | Discovered x2APIC, MSR readbacks, three reference samples, multiple hardware timer events, per-event ISR/EOI checks, all-GPR return and quiescent shutdown. |
| `timer-xapic` | QEMU CPU explicitly hides x2APIC; actual xAPIC delivery through the UC page. QMP physically walks all four device-table entries while the CPU is stopped for observation. |
| `timer-repeat` | Two independent arm/run/mask/drain/stop cycles; one monotonically increasing tick/EOI total, with QMP observations in each enabled window. |
| `timer-spurious` | Controlled INT 0xff returns with zero ticks/EOIs and one spurious count; genuine periodic delivery then succeeds. This exercises no-EOI dispatch, not spontaneous hardware spurious generation. |
| `timer-unsupported` | Trusted hook removes APIC capability before any controller/PIC/mapping writes. Selection rejects it; APIC base, IF/root and allocation state remain unchanged. Physical APIC-less boot is unverified. |
| `timer-unexpected` | Controlled INT 0x41 reaches its actual vector-specific entry and halts with zero ticks/EOIs; no timer-success result. |
| `timer-software` | Controlled INT 0x30 without an in-service APIC bit is rejected with zero ticks/EOIs; it cannot serve as fake hardware delivery evidence. |

Historical Phase 4A timer observations at `0fba57e` were (current raw timer
results remain in the per-case JSON from this run):

| Case | Mode | Calibration samples (divided ticks) | Initial count | Delivered ticks / EOI |
| --- | --- | --- | --- | --- |
| `timer` | x2APIC | 627187, 626812, 626687 | 62680 | 32 / 32 |
| `timer-xapic` | xAPIC | 627812, 627687, 627062 | 62768 | 33 / 33 |
| `timer-repeat` | x2APIC | 627062, 627062, 626812 | 62705 | 64 / 64 across two windows |
| `timer-spurious` | x2APIC | 626937, 626875, 627062 | 62693 | 26 / 26; separate spurious count 1 |

Original CR3 stays `0x1000`. The independent xAPIC walk reads entries `0x8023`,
`0x9023`, `0xa023`, `0x80000000fee0007b`; the final leaf's A/D bits reflect
actual MMIO access while UC, supervisor RW and NX remain set. Final allocation
accounting in these images is seven allocated table frames, 118,495 free and
seven retained tables, matching each guest's baseline. These numbers are
observations, not fixed ABI or allocator promises.

The runner pauses/resumes QEMU briefly for consistent QMP CPU/LAPIC snapshots.
It requires actual IF=1/CPL=0 inside the ELF wait region, original CR3, kernel
selectors/stack, zero FS/GS and stable canaries. `info lapic` must independently
show LVTT=0x20030, divide 16, the calculated nonzero initial count, enabled SVR,
TPR=0 and other LVT sources masked. xAPIC snapshots include the physical page
walk. Final controller observations require LVTT=0x30030, zero countdown,
disabled SVR, TPR=0xff and empty ISR/IRR. The observer does not inject timer
interrupts or write guest registers/memory.

The host independently recomputes the median-based calibration, checks every
actual frame record's RIP/RSP/CS/SS/flags/root/handler-stack, requires consecutive
ticks and equal EOI totals, validates rearm ordering and compares numerical
baseline/final frame/table accounting. Both CPU/controller evidence and genuine
ISR-gated repeated entry are required; an initialization/pass label cannot pass.
The accepted per-50-ms-window delivered-event range is 8–128, with actual totals
reported in structured JSON. It is deliberately a delivery/liveness bound, not a
precision or interrupt-latency guarantee. The observed delivered totals differ
from an ideal 50 events per 50 ms, so precise 1 kHz delivery and elapsed-time
accounting are explicitly unverified despite reproducible countdown calibration.

During historical Phase 4A validation, the first complete-suite attempt failed
an existing native CPU snapshot check.
A focused audit also reproduced a partially emitted UART execution contract.
The harness now waits for complete per-run identity/contract lines and briefly
stops/resumes QEMU for coherent native register snapshots; all original privilege,
root, stack, flag and canary assertions remain. Five consecutive A-fault runs,
reverse fault, native and timer controls passed after the observer changes.
Initial failures are retained in `build/phase4a-test-initial.log` and
`build/phase4a-node-observer-audit-initial.log`; the final full suite is recorded
separately in `build/phase4a-test-run.log`. No Node/native kernel code changed.

The retained Node/native proofs continue as follows.

The host verifies emitted SYSCALL bytes `0f 05`. For **each Node** it requires
direct QMP CPU samples before the syscall at CPL=3, at CPL=0 in the guarded native
entry stack, and at CPL=3 after the first query return. It checks actual CR3,
selectors, RSP, flags, MSRs, FS/GS zero state and all six callee-saved register
canaries. Returning RAX=0/RDX=`0x100000003` and the full register-preservation
contract are checked separately. Faulted Nodes perform a real query round trip
before faulting; printed synthesized selectors alone cannot pass the CPU checks.

Observed positive roots are A=`0xc000`, B=`0x1b000`, with original kernel
CR3=`0x1000`. At VA `0x400000`, A uses PA `0x8000` and B PA `0x16000`; at VA
`0x402000`, A uses PA `0xa000` and B PA `0x18000`. Node identities are
4294967297/4294967298 and SpaceIds 257/258; rollback reuses slots with new
generations. These physical/numeric values are observations, not stable ABI or
allocation promises. Both normal Nodes make two queries plus one terminal exit,
with independent results **42/84** and status 0.

| Node case | Actual verified behavior |
| --- | --- |
| `nodes` | A and B both Ready before first execution; distinct roots/backing, actual CPL=3/0/3 per Node, normal A then B, retained outcomes and separate cleanup. |
| `nodes-a-fault` | A query returns, then actual #UD vector 6/error 0 at the payload UD2 site. A is reclaimed; B remains Ready and executes successfully. |
| `nodes-b-fault` | B faults first with actual #UD 6/error 0. B is reclaimed; the previously constructed A remains Ready and succeeds. |
| `nodes-cross` | A reads B-only VA `0x406000`: actual #PF vector 14/error 4, CR2=`0x406000`, user CS/SS=0x3b/0x33 and RSP=`0x404ff8`. A is reclaimed; B reads its intact sentinel and exits successfully. |
| `nodes-reclaim` | Normal A is destroyed before B executes. B's private tables/mappings, zero result, sentinel and Ready state survive; B then produces 84. |
| `nodes-rollback` | Ten successive B table-allocation failure points return exact pre-attempt frame/table accounting while A remains Ready/intact. B then constructs successfully; both execute and reclaim normally. |

The runner checks exact ELF-relative exception RIP, vector/error, CR2 for #PF,
actual saved RSP/CS/SS/flags, trusted handler-stack bounds, and matching
Node/Thread/SpaceId/root. It verifies both Ready records precede execution,
per-Node syscall bindings/sequences, terminal outcomes, restored root/MSRs and
ordered sibling teardown/survival. Baseline and final allocated/free/table
counts must match within every guest. All finish with seven pinned kernel table
frames; free totals can differ between images and are not hardcoded.

The original native cases remain independent regressions:

| Native case | Actual verified behavior |
| --- | --- |
| `native` | 3 real entries, 2 query returns, ABI tag `0x100000003`, exit status 0; direct user/kernel/user CPU samples. |
| `native-unknown` | 4 entries; unknown `u64::MAX` returns RAX=-1/RDX=0, user verifies it, performs another query and exits 0. |
| `native-exit-invalid` | 4 entries; exit argument 256 returns RAX=-2/RDX=0 without termination; user verifies it, queries again and exits 0. |
| `native-return-rip` | First entry reaches the kernel stack; a dedicated trusted hook substitutes noncanonical RIP `0x800000000000`, rejected before dispatch/IRETQ. |
| `native-return-rsp` | User sets actual noncanonical RSP `0x800000000000` and executes SYSCALL without CALL; actual trusted kernel entry succeeds, then RSP is rejected. |
| `native-return-flags` | User executes STD before SYSCALL; kernel entry has DF clear, original R11 retains DF, and unsafe return flags are rejected. |
| `native-return-selectors` | Dedicated hook substitutes kernel return CS/SS 0x08/0x10; validator rejects rather than returning at CPL=0. |
| `native-disabled` | SCE is intentionally cleared after preflight. Actual user SYSCALL raises #UD 6/error 0 at the exact site, with **zero native entries/returns**. Controlled negative containment is not counted as successful native entry. |

Every expected terminal native outcome restores original root, MSRs and
allocation accounting. Return-rejection cases have one entry and zero query
returns; the disabled-SCE fault has no native entry. Existing supervisor-fault
scenarios still follow their fatal exception path rather than Node containment.

The deadline remains 30 seconds per guest. Bounded observation loops are test
instrumentation, not benchmarks. Unexpected reset, triple fault, emulator exit,
wrong privilege/root/stack/context, missing CPU evidence, timeout, early halt or
permission failure cannot pass. Serial pass messages alone are insufficient.

Local Git-ignored evidence: `build/test-results.json`,
`build/test-logs/<case>.{serial.log,qemu.log,json}`, `build/phase4b-baseline.log`,
`build/phase4b-host-tests.log`, `build/phase4b-doctor.log`, `build/phase4b-check.log`,
`build/phase4b-build.log`, `build/phase4b-switch-first.log`,
`build/phase4b-focused-switches.log`, `build/phase4b-test-run.log` and
`build/phase4b-repro-build.log`. Switching JSON includes full identity/stack
bindings, FIFO phases, full saved contexts, outcomes, per-slice raw CPU/TSS/frame
observations, physical stack walks, data/witness values and exact accounting.
Existing timer, Node, ABI, exception, final halt, image/firmware and clean-exit
evidence remains in the same structured report.

## Nested-entry and security limits

IF=0 does not suppress NMI or synchronous exceptions. The brief early entry
window still has a user RSP, although its memory operands are mapped trusted
RIP-relative addresses. Native NMI/#MC use terminal IST2 and #DF retains IST1. The valid TSS pointer
is published before the IDT IST2 selectors with explicit compiler ordering;
other catastrophic early faults may escalate to #DF. A repeated native entry
selects the emergency stack and fails fatally before overwriting saved user RSP.
Those are defensive fail-stop mechanisms, **not injected/verified general nested
NMI, machine-check or corruption recovery**. Shared terminal IST2 can itself be
overwritten by unsupported nested critical events. No recovery or resumption is
attempted for them.

Kernel and user TLS/GS state, FPU/SIMD context, SMP/per-CPU entry, interrupts outside the isolated timer window,
and preemption, adversarial entry fuzzing, hardware faults and physical machines
remain unsupported or unverified. Fatal paths use no kernel GS-relative/TLS
state; a future per-CPU design must revisit this assumption. There is no CPU-time
isolation: another infinite user loop could occupy the CPU until host timeout.
Speculative/microarchitectural side channels and arbitrary privileged corruption
are outside the demonstrated boundary.

The two-Node implementation remains trusted bootstrap machinery. Node-local
capabilities, public factories/ResourcePools, multiple runnable Threads per Node,
preemptive scheduling, Channels/IPC, shared MemoryObjects, pointer-copy APIs, full ELF
loading, a userspace runtime, filesystem/network/drivers and POSIX/Linux
compatibility remain unimplemented. Construction/teardown assumes one trusted
controller with no asynchronous observers. A future concurrent object system
must revisit synchronization, references, cancellation and lifetime publication.

## Architecture relationship and recommended next milestone

[ADR 0012](adr/0012-cooperative-thread-context-switching.md) accepts only bounded
cooperative FIFO selection, owned user contexts, private entry stacks and safe
native/common-stack handoffs. ADRs 0009–0011 retain their original rationale with
dated extensions. Public ABI 0.1 and all earlier test paths remain intact.
Architecture v0.2 is unchanged. Complete capability-oriented Nodes and the full
preemptive scheduling proposal remain unimplemented; `NEU-025` and optional
NeuKMon remain open. No failed acceptance criterion remains within the tested
Phase 4B profile; hardware, SMP, extended-state and adversarial-fuzz coverage are
explicitly outside that evidence.

Recommended next milestone: **K4 / Phase 4C, controlled timer-driven userspace
preemption**. Capture real interrupt contexts, define IRQ acknowledgment and
rescheduling ownership, and allow IF=1 only after native early-entry/return and
root/TSS/current-Thread publication support it. A Ring 0 timer may request
rescheduling but must defer the switch until a proven safe boundary; it must not
switch an arbitrary Rust frame. Preserve non-preemptible memory/lifecycle
transactions until synchronization is established. The current syscall flag
contract excludes IF and needs an explicit interrupt-resumption extension before
it can serve that phase. Keep floating-point/SIMD, per-CPU/SMP and public yield/
factory/capability decisions separately scoped. Cooperative yielding alone gives
no CPU-time isolation from a Thread that never yields.

## Primary references

- [Pinned Limine protocol](https://github.com/Limine-Bootloader/limine-protocol/blob/3a0526b700e356f0eac1b71a77697b3fd1c707a3/PROTOCOL.md) and [12.9.4 release](https://github.com/Limine-Bootloader/Limine/releases/tag/v12.9.4).
- [Rust x86-64 freestanding target](https://doc.rust-lang.org/rustc/platform-support/x86_64-unknown-none.html).
- [Intel instruction reference](https://www.intel.com/content/dam/www/public/us/en/documents/manuals/64-ia-32-architectures-software-developer-vol-2b-manual.pdf): SYSCALL/SYSRET state and selector rules.
- [Intel system programming reference](https://cdrdv2-public.intel.com/819714/253668-sdm-vol-3a.pdf): paging, interrupt frames, TSS/IST and privilege boundaries.
- [QEMU 11 PIT frequency](https://github.com/qemu/qemu/blob/v11.0.0/include/hw/timer/i8254.h) and [PIT output semantics](https://github.com/qemu/qemu/blob/v11.0.0/hw/timer/i8254_common.c).
- [QEMU local APIC observation](https://github.com/qemu/qemu/blob/v11.0.0/hmp-commands-info.hx): `info lapic`.

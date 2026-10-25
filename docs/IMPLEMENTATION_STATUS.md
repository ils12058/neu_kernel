# Neu Kernel implementation status

**Verified:** 2026-10-25. **Milestone:** K2 / Phase 2B, native x86-64 SYSCALL and
first Neu scalar ABI. **Result:** complete against the supplied Phase 2B criteria
in the documented single-CPU QEMU environment. All original **23 host tests and
26 QEMU scenarios** remain passing. The current suite has **32 host tests and
34 QEMU scenarios**. This is a bootstrap-fixture ABI proof, not complete Node or
Thread lifecycle management or a stable public ABI.

The inspected tree was clean on `main` at
[`4b6887c`](https://github.com/ils12058/neu_kernel/commit/4b6887c2fb1a6b4eb288672d5e8028ed2f54b141).
The original 23/26 suite was rerun before implementation, recorded in
`build/phase2b-baseline.log`. Historical Phase 2A status remains at that commit.
Architecture v0.2, Rust/target and Limine are unchanged. Current tests are local;
physical hardware and hosted CI have not been validated.

## Created and modified files

Created:

- `kernel/src/syscall.rs`: central experimental numbers/status/version, scalar
  dispatch, assembly frame/register layout, return/MSR validation and nine tests.
- `kernel/src/arch/x86_64/native.rs`: owned MSR configuration, native session,
  trusted dispatch/return/terminal checks, diagnostics and fixture cleanup.
- `kernel/src/arch/x86_64/native.S`: stack-independent entry, saved frame,
  validated IRETQ return, emergency stack and independent user payload.
- `docs/adr/0009-experimental-native-syscall-and-trusted-return.md`: rationale.

Modified:

- `kernel/src/arch/x86_64/userspace.rs` and `userspace.S`: retain the Phase 2A
  path; add an explicit native bridge that preserves the selected SCE setting.
- `kernel/src/arch/x86_64/exceptions.rs`: native RSP0/terminal IST2 configuration
  and native user-fault classification; older fatal/#DF behavior remains.
- `kernel/src/arch/x86_64/paging.rs`: preserve the additional emergency guards.
- `kernel/src/main.rs`, `kernel/src/lib.rs`, `kernel/src/arch/x86_64/mod.rs`,
  `kernel/Cargo.toml`: module/test wiring and mutually exclusive configurations.
- `tools/neu.py`: eight native cases, emitted SYSCALL-byte/stack checks, actual
  three-phase QMP observations and independent ABI/return/cleanup verification.
- `README.md`, this status, `docs/adr/README.md`, ADR 0008: current behavior,
  evidence and a dated extension preserving the original test-gate rationale.

No linker script, boot protocol, host package or external Rust dependency was
changed. Existing BSS/linker collection includes the new guarded static stack;
normal and native cold builds revalidated the resulting image layout.

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
not required by the selected boot workflow. Phase 2B adds no host packages,
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

`abi_query` has no pointer argument or memory output. The user fixture checks its
status and complete scalar tag. An unknown operation returns -1/RDX=0 and safely
resumes Ring 3. `thread_exit` checks RDI before narrowing: 0..255 terminates the
isolated fixture; 256 or a negative value represented as unsigned 64 bits returns
-2/RDX=0. Other arguments are ignored. The conformance fixture must exit with 0;
nonzero in-range terminal statuses would be recorded as a failing fixture result,
not a returning syscall. There is no generalized Thread object or exit API yet.

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
| Syscall stack | The existing separate 32 KiB supervisor RW/NX privilege-entry stack, guarded below/above; suspended kernel frames stay on the 64 KiB normal stack. |
| Critical entry | Native builds give NMI/#MC a separate guarded 16 KiB IST2; #DF retains its original 32 KiB IST1. |

SYSCALL does **not** choose TSS.RSP0. The first instructions read/set a
supervisor-only RIP-relative non-reentrant guard, store user RSP as a scalar,
and select the known kernel stack with LEA. There is no push, call, Rust or
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

Phase 2A's independently owned PML4/private children remain unchanged: no borrowed
kernel subtree pointers. The supervisor kernel image/workspace appears in both
roots, with RX text, R/NX constants and RW/NX data/stacks. User code, constants,
data and stack use separate fully zeroed backing frames, mapped RX, R/NX, RW/NX
and RW/NX. The bounded lower fixture range starts at `0x400000`; its stack is
`0x404000..0x405000`, initial RSP `0x404ff8`, with absent adjacent guards. A wrapper
CALL gives saved syscall RSP `0x404ff0`. Native bytes are copied from immutable
kernel R/NX storage to new backing; no kernel executable page becomes user code.

The user root and every table/backing/mapping reference remain live across all
round trips. Native dispatch does not allocate, unmap or access frame contents.
The owner remains on the suspended kernel stack. After a terminal return, restore
MSRs while no user executes, deactivate/clear the private hierarchy and all its
workspace/leaf references, flush with PGE/PCIDE disabled, remove mapping records,
and release table aliases and backing identities in the established order. Exact
frame/table accounting and the original seven pinned kernel tables are checked.
No active root is reclaimed on an unexpected fatal kernel path.

The observed layout still needs ten private user tables and four user backing
frames. The static entry/emergency stacks are image-reserved, not allocations
freed with the user root. Existing 64-table/16-data-owner/64-mapping bounds remain.
**All bootloader-reclaimable memory remains reserved.** No loader reclamation,
MemoryObject or generalized AddressSpace object has been added.

## Actual validation and results

Executed from the repository root:

```sh
make doctor
make check > build/phase2b-check.log 2>&1
make build
make test > build/phase2b-test-run.log 2>&1
cargo test --locked --lib --target aarch64-apple-darwin
python3 tools/neu.py test --case native
```

The other seven native cases were also executed individually. A fresh source copy
excluding `.git`, `.cache`, `build`, `target` and Python caches then ran:

```sh
make build
python3 -c "from tools.neu import build; build('native')"
```

Cold normal/native builds both passed: fresh pinned-loader download/checksums,
compilation, four valid LOAD segments, no writable/executable ELF segment,
stack/guard validation, native SYSCALL bytes, and EFI artifacts. No host packages
or external Rust dependencies were installed.

| Check | Actual result |
| --- | --- |
| Doctor | Pass: unchanged pinned Rust/target, QEMU and hashed OVMF pair. |
| Static checks | Pass: formatting and Clippy with warnings denied, 34 kernel configurations plus host library; Python syntax passes. |
| Host tests | **32 pass**: all prior 23 plus 9 for operation/status/version encoding, dispatch, exit bounds, argument order/preservation, return root/selectors/RIP/RSP/flags, and MSR configuration. |
| Original QEMU cases | **All 26 pass**, including genuine #DF, guards, paging rollback/zeroing, direct CPL=3 execution and every Phase 2A security violation. |
| New native cases | **All 8 pass**, described below. |
| Final halt | **All 34 pass** two QMP HLT=1/CPL=0/IF=0 observations in the ELF halt loop, correct final kernel CR3, then host-requested quit/exit 0. |

The host independently checks that fixture syscall sites actually contain bytes
`0f 05`. It requires a direct QMP sample at CPL=3 before the syscall and CPL=0
inside `neu_native_kernel_wait`, with the actual RSP at the trusted frame, kernel
CS/SS, user CR3, safe flags and preserved canaries. Returning cases also require
a direct CPL=3 sample inside the post-query loop with RAX=0/RDX=ABI tag, restored
user selectors/segments and RSP. The guest validates the same output and canaries
before reaching that loop. Missing any required sample fails the test.

Observed positive flow in the full run used user CR3 `0xc000`, kernel CR3
`0x1000`, user CS/SS 0x3b/0x33 and kernel CS/SS 0x08/0x10. The fixture performed
**two successful query round trips and one terminal exit**, with six 64-bit
callee-saved canaries and preserved argument registers. Physical root values are
observations, not an ABI or allocator promise.

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

Each return-rejection case has one native entry and zero query dispatches or
returns. Every expected terminal outcome restores the original kernel root,
MSRs and allocation accounting. Exact operation sequence, arguments, statuses,
resume RIP/RSP, selector/flag values, kernel-stack bounds and event counts are
checked separately from human-readable pass labels.

The default deadline remains 30 seconds per guest. Bounded before/kernel/after
observation loops are test instrumentation, not benchmark results. An unexpected
reset, triple fault, emulator exit, wrong CPL/root/stack/context, missing syscall
MSR evidence, timeout, silent return or permission failure cannot pass. Serial
messages alone are insufficient. Existing QMP cleanup and no-debug-exit behavior
remain intact.

Local Git-ignored evidence: `build/test-results.json`,
`build/test-logs/<case>.{serial.log,qemu.log,json}`, `build/phase2b-baseline.log`,
`build/phase2b-check.log`, `build/phase2b-native-first.log`,
`build/phase2b-negative-run.log`, `build/phase2b-test-run.log`, and
`build/phase2b-repro-build.log`. Native JSON includes raw three-phase CPU snapshots,
parsed MSRs/entries/returns/terminal counters, final root/halt observations,
ELF/firmware identity, exact QEMU arguments and clean exit status.

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

Kernel and user TLS/GS state, FPU/SIMD context, SMP/per-CPU entry, interrupts and
preemption, adversarial entry fuzzing, hardware faults and physical machines
remain unsupported or unverified. Fatal paths use no kernel GS-relative/TLS
state; a future per-CPU design must revisit this assumption. There is no CPU-time
isolation: another infinite user loop could occupy the CPU until host timeout.
Speculative/microarchitectural side channels and arbitrary privileged corruption
are outside the demonstrated boundary.

No full Nodes/Threads, scheduler, public capabilities, NodeFactory/ResourcePool,
Channels, object syscalls, pointer-copying API, ELF loader, dynamic linker,
filesystem/network service, POSIX/Linux compatibility or general runtime exists.
The trusted continuation and private-root ownership remain explicit bootstrap
mechanisms, not unrestricted public authority paths.

## Architecture relationship and recommended next milestone

[ADR 0009](adr/0009-experimental-native-syscall-and-trusted-return.md) accepts only
the experimental scalar ABI and trusted single-CPU entry/return contract.
ADR 0008 retains its original test-gate rationale with a dated Phase 2B extension;
its eight regressions still pass. Other boot/paging ADR relationships remain
unchanged. Architecture v0.2 is preserved byte-for-byte. Complete Node/Thread,
startup/execution and syscall specifications are not accepted by implication;
`NEU-025` remains open, as does the older optional NeuKMon work.

Recommended next milestone: **K3 / minimal independent execution-context isolation**:
two separately owned user roots/backing sets, explicit internal lifecycle and
fault containment, initially controlled sequential entry/exit. Decide ownership
and remaining native ABI requirements before expanding beyond the fixture.
Defer preemptive scheduling, public factories/capabilities and SMP to their own
milestones rather than treating this first ABI as a full kernel-object system.

## Primary references

- [Pinned Limine protocol](https://github.com/Limine-Bootloader/limine-protocol/blob/3a0526b700e356f0eac1b71a77697b3fd1c707a3/PROTOCOL.md) and [12.9.4 release](https://github.com/Limine-Bootloader/Limine/releases/tag/v12.9.4).
- [Rust x86-64 freestanding target](https://doc.rust-lang.org/rustc/platform-support/x86_64-unknown-none.html).
- [Intel instruction reference](https://www.intel.com/content/dam/www/public/us/en/documents/manuals/64-ia-32-architectures-software-developer-vol-2b-manual.pdf): SYSCALL/SYSRET state and selector rules.
- [Intel system programming reference](https://cdrdv2-public.intel.com/819714/253668-sdm-vol-3a.pdf): paging, interrupt frames, TSS/IST and privilege boundaries.

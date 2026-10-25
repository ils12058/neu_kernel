# Neu Kernel implementation status

**Verified:** 2026-10-25. **Milestone:** K1 / Phase 1A, memory ownership, physical
frames, and exception foundation. **Result:** complete against the supplied
Phase 1A acceptance criteria on the documented QEMU environment. Phase 0's three
smoke cases remain passing. This does not complete K1's later VMM work, the full
boot-to-first-Node integration plan, or physical hardware validation.

The inspected starting tree was clean on `main` at
[`3549b56`](https://github.com/ils12058/neu_kernel/commit/3549b56931c6becd8e6e4b3a04bf927984018ffb).
That commit retains the historical Phase 0 implementation/status. Baseline tests
were rerun successfully before Phase 1A changes.

## Implemented foundation

| Area | Current implementation |
| --- | --- |
| Rust project | One dependency-free Cargo package with a freestanding binary and architecture-neutral `no_std` memory library for host tests. Abort-style kernel panic behavior. |
| Executable | Static ELF64/x86-64 at `0xffffffff80000000`, separate RW requests, RX text, R data, RW data/stack LOAD segments. |
| Entry | Small x86-64 assembly entry, IF/DF cleared, switch to a 16-byte-aligned 64 KiB kernel-owned stack before calling Rust. Never returns to the loader. |
| CPU checks | NX support, CR0 protection/paging/write-protect, CR3 presence, CR4 PAE with LA57 disabled, EFER long mode/NX, IF/DF clear, current stack within kernel bounds. |
| Boot adapter | Limine base revision 6; revision-0 firmware-type, memory-map, and executable-address requests. HHDM is requested only by the page-fault test. |
| Memory ownership | Full descriptor copy in fixed kernel storage, independent physical-image reservation, no bootloader-memory reclamation. |
| Physical frames | 4 KiB frame ownership, bitmap over usable candidate ordinals, sparse physical ranges, explicit allocate/free/errors/accounting. No frame-content access. |
| Exceptions | Private GDT/TSS/IDT, normalized assembly frames with all GPRs, static fault record, fatal diagnostics, dedicated 32 KiB #DF IST1 stack. |
| Serial | COM1 at 115200, 8N1, loopback initialization check, bounded polling, no heap or logger locks. |
| Panic/failure | Bounded emergency serial output followed by `cli`/`hlt`; recursive-panic guard; no unwinding, scheduler dependency, or interactive monitor. |
| Host tooling | Python standard library plus Cargo/curl/QEMU; build/run/test/doctor through Make. SHA-256 checks for external loader artifacts. |
| Tests | Seven host memory tests and nine QEMU cases: Phase 0 regressions, real-pool/synthetic allocation, #UD, #GP, #PF, genuine #DF, and #BP. CPU halt, expected fault context, and stack selection are checked. |

Architecture-specific mechanisms are under `kernel/src/arch/x86_64/`. The Limine
adapter is `kernel/src/boot.rs`; generic bootstrap sequencing is in `main.rs`.
`kernel/src/memory/` holds pure ownership bookkeeping and focused tests;
`lib.rs` exposes it to host tests without compiling kernel entry code for macOS.
No empty future-subsystem modules have been created.

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
not required by the selected boot workflow. Phase 1A adds no host packages,
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
and validates the kernel's ELF format, LOAD permissions, entry, and stack symbols.
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

## Verification and actual results

Commands executed from the repository root:

```sh
make doctor
make check
make build
make test
```

`make check` checks Rust formatting and runs Clippy with warnings denied for all
nine kernel configurations and the host memory library. `make test` runs the host
unit tests before all nine QEMU cases. Fault-injection features are mutually
exclusive and off in ordinary builds.

| Scenario | Actual result |
| --- | --- |
| Build | Passed: x86-64 ET_EXEC ELF, entry `0xffffffff80001000`, four LOAD segments with no writable/executable segment, 64 KiB normal stack and 32 KiB #DF stack. |
| Fresh-directory build | Passed from a source copy with no loader cache or Cargo target directory: pinned download, checksum verification, compilation, and EFI artifact creation. |
| Static checks | Passed: formatting and Clippy with warnings denied for all nine configurations and the host memory library. |
| Normal boot | Passed: Limine handoff, serial banner, CPU/stack and memory-map validation, bootstrap-success and idle messages. |
| `test-panic` | Passed: controlled panic after boot validation, bounded panic output, no bootstrap-success message. |
| `test-missing-memory-map` | Passed: the test image omits the request from Limine's delimited request area; the null response is rejected and a boot failure is reported before dereference. |
| Host ownership/allocator tests | Passed: seven tests cover exact descriptors, all non-usable types, malformed count/order/overflow/alignment/overlap, page zero/kernel exclusion, sparse high addresses, metadata capacity, 65-frame exhaustion across bitmap words, reuse/invalid/double frees, empty pools, and 5000 mixed operations against an independent reference set. |
| QEMU memory self-test | Passed: 64 unique real frames allocated/freed with exact accounting restoration, reserved and retained-loader frees rejected, reuse/double/unaligned-free checks, sparse synthetic exhaustion, and malformed-descriptor rejection. |
| #UD | Passed: hardware vector 6, normalized error 0, saved RIP at the `ud2` site, normal-stack fatal halt. |
| #GP | Passed: hardware vector 13, error `0x28`, saved RIP at the invalid selector load, normal-stack fatal halt. |
| #PF | Passed: actual tables inspected read-only through a test-only HHDM response; a present read-only static is written with WP enabled; vector 14/error 3, exact faulting RIP, CR2 equals the target static. |
| #DF | Passed: a genuine #GP whose gate has an invalid selector causes #DF delivery, vector 8/error 0, handler RSP inside the dedicated IST1 stack. No software interrupt 8 is used. |
| #BP | Passed: `int3` gives vector 3/error 0, saved RIP after the trap, normal-stack fatal halt. No resumption is attempted. |
| Halt proof for all nine | Passed: QMP observes the VM still running with its CPU halted twice, CPL=0, IF=0, and RIP inside the ELF's `neu_halt_begin`/`neu_halt_end` range. QEMU exits with status 0 only after the host requests `quit`. Unexpected reset/shutdown/triple fault cannot pass. |

The Phase 0 timeout-failure and interactive-run observations remain historical
evidence at the baseline commit. Phase 1A retains the same deadline/cleanup and
interactive-launch mechanisms; the current automated results are listed above.

Observed normal kernel output, excluding firmware/loader console output:

```text
[NEU] Neu Kernel
[NEU] Architecture: x86_64
[NEU] Early serial initialized
[NEU] CPU and kernel stack validated
[NEU] Kernel GDT/TSS/IDT initialized; double-fault IST ready
[NEU] Boot protocol initialized: Limine base 6, UEFI64
[NEU] Memory map validated: 31 regions, 485859328 usable bytes
[NEU] Boot memory map copied: 31 descriptors in kernel storage
[NEU] Physical frame allocator initialized: 118617 free, 0 allocated; loader RAM retained
[NEU] Kernel bootstrap successful
[NEU] Idle: interrupts disabled; halting
```

Memory-map totals are observations for that launch, not a minimum RAM claim or
an allocator capacity guarantee. `readelf -hW -lW` was also used to independently
inspect the ELF. It is not a required build dependency.

The default deadline is 30 seconds per guest, followed by bounded process cleanup.
An absent expected result, early exit, shutdown/reset, incorrect halt location,
unexpected success/failure marker, or QMP failure is a test failure. Serial text
alone cannot pass. The tests exercise the normal halt mechanism rather than
substituting an `isa-debug-exit` path. For slower hosts or a single scenario:

```sh
make test TEST_TIMEOUT=60
python3 tools/neu.py test --case panic --timeout 30
python3 tools/neu.py test --case memory
python3 tools/neu.py test --case invalid-opcode
python3 tools/neu.py test --case general-protection
python3 tools/neu.py test --case page-fault
python3 tools/neu.py test --case double-fault
python3 tools/neu.py test --case breakpoint
cargo test --locked --lib --target aarch64-apple-darwin
```

Evidence is written to `build/test-results.json` and
`build/test-logs/<case>.{serial.log,qemu.log,json}`. It includes
kernel/firmware identity, actual QEMU arguments, CPU-register observations, and
exit status, expected exception context, and handler stack identity. The host
target is detected automatically; the explicit host command above is the one
executed on this Mac. Build, cache, and runtime evidence are ignored by Git. These are
local tests; no hosted CI or physical-machine tests have been run.
`build/phase1a-repro-build.log` preserves the current fresh-directory build check
with an empty loader cache and Cargo target directory. The original Phase 0
`build/repro-build.log` and `build/timeout-check.json` are historical local evidence.

## Memory ownership and allocator invariants

- The adapter checks the negotiated base revision **before** response use,
  requires UEFI64, and bounds entry counts at 4096. It checks higher-half pointer
  shape/alignment/range arithmetic, sorted nonempty memory regions, overflow,
  page alignment of usable/reclaimable RAM, and their non-overlap. Reserved
  regions may overlap as permitted by the protocol. Unknown types are never
  treated as allocatable.
- Pointer shape checks cannot prove mapping validity. Mapped, initialized
  response objects remain part of the trusted Limine contract. This is not a
  fault-recovering parser for malicious loader pointers or userspace memory.
- Every descriptor is copied exactly into a kernel-owned array (maximum 4096,
  96 KiB). The copy is audited against still-live loader data before immutable
  publication. Loader descriptor pointers do not escape the adapter.
- The added Executable Address request supplies the authoritative physical base.
  Its virtual base must match linker bounds. The complete physical image extent
  independently reserves code/data, both stacks, map copy, bitmap, GDT/TSS/IDT,
  and the static fault record. Uncertain ownership remains reserved.
- A candidate frame must be fully inside a validated type-0 usable span,
  4 KiB aligned, outside page zero, and outside that image reservation. All other
  types and holes remain unavailable, even after a free attempt. Raw usable-byte
  totals are not promises of generally accessible memory.
- **No bootloader-reclaimable memory is released.** The current CR3 and its
  loader-owned page tables remain live. Response storage, firmware/ACPI/runtime
  regions, modules, and all unknown/reserved types also remain unavailable.
- A 128 KiB kernel-owned bitmap tracks candidate ordinals rather than physical
  address space. Capacity is 1,048,576 candidate frames (4 GiB of candidate RAM),
  not a 4 GiB physical-address ceiling. Oversized pools fail initialization;
  sparse high physical addresses need no metadata for holes.
- Allocate/free have exclusive mutable access on one BSP with IF=0. Free rejects
  unaligned, noncandidate, or unallocated frames before changing accounting.
  `allocated + free = total` holds; exhaustion is an explicit error. Scans are
  bounded and simple; no constant-time or performance guarantee is made.
- Allocation returns physical addresses only. No frame contents are accessed,
  mapped, initialized, or zeroed. There is no heap. Callers must own the frame
  they free; the allocator has no generation token to detect a stale address
  after that same frame has subsequently been allocated again.

## Exception behavior and remaining limitations

- CPU descriptors are kernel-owned: GDT code/data selectors `0x08`/`0x10`, TSS
  selector `0x18`, a 256-entry IDT, and IST1 for #DF. Normal exceptions use the
  64 KiB kernel stack; #DF uses a separate 32 KiB stack inside the reservation.
- Assembly normalizes error/no-error-code frames and saves 15 GPRs plus the
  hardware RIP/CS/RFLAGS/RSP/SS. CR2 is captured before Rust runs. A 192-byte
  static `NEU_LAST_FAULT` record preserves the context before bounded output.
  All current exceptions are fatal; #BP is diagnostic, not a resumable debugger.
- The tested vectors are 3, 6, 8, 13, and 14. Other CPU vectors have correctly
  categorized entry stubs but have not been separately injected. External vectors
  share a fatal fallback reporting vector 255. No device interrupt framework is
  enabled. IF remains clear; synchronous CPU exceptions still work.
- The #PF image alone requests HHDM and reads only table pages verified to lie
  completely in retained loader memory. It confirms the actual target mapping
  is present/read-only before the write. It changes no mapping. This is neither
  a general VMM nor permission to dereference arbitrary physical frames.
- The genuine #DF test verifies exception-delivery escalation and IST execution.
  Its saved RIP is not treated as a recoverable instruction location. Arbitrary
  stack corruption/overflow recovery, NMI/machine-check hardening, SIMD context,
  and the NMI window during descriptor replacement remain unverified. There are
  no guard pages or general recovery paths yet.
- ELF segment permissions are checked, but this is not a complete VMM or an
  alias-aware W^X guarantee; loader-provided direct mappings remain in place.
- Emergency output permits at most 512 bytes and 4096 status polls per byte;
  ordinary writes permit 1024 bytes per call. It uses no heap, locks, or waits.
  Serial-device loss, recursive panic, NMI, and machine-check events have not been
  separately fault-injected. The handler makes a best-effort diagnostic attempt.
- Single CPU, QEMU q35/TCG/COM1 only. SMP, Nodes, Ring 3, syscalls, capabilities,
  MemoryObjects, scheduling, IPC, userspace drivers, full NeuKMon, compatibility
  layers, and OS services remain unimplemented.

## Architecture relationship and next milestone

[ADR 0005](adr/0005-boot-memory-ownership-and-frame-allocation.md) accepts the
scoped ownership/PMM contract and succeeds the historical Phase 0 contract in
[ADR 0004](adr/0004-limine-phase-0-integration.md), retaining the loader/base pin.
[ADR 0006](adr/0006-kernel-exception-descriptors-and-fatal-handling.md) records
private CPU descriptors, fatal exceptions, and controlled fault testing. Original
rationale is preserved through the explicit supersession chain. Architecture
revision 0.2 remains unchanged. Only the PMM portion of `NEU-018` and privileged
exception portion of `NEU-023` are accepted here; later VMM/MemoryObject, Node,
and full CI designs are still proposals. `NEU-025` remains open.

The draft's Section 22 K0 outline includes basic NeuKMon, while the supplied
Phase 0 brief limits diagnostics to early serial/panic support and excludes an
interactive monitor. This implementation follows the narrower brief and does
not claim completion of that K0 monitor item. Keep optional NeuKMon as later
work with its own concrete decision rather than silently changing the baseline.

Recommended next milestone: **K1 / Phase 1B, page-table ownership and controlled
kernel mappings**. Define and test page-table lifetimes, own the relevant
translation structures, establish narrow mapping/unmapping and frame-content
initialization mechanisms, add guard pages, and test permissions/invalidation.
Reclamation requires an explicit audit showing every affected loader structure
and reference has been transferred or retired. Do not release loader memory
merely because the map copy or descriptor tables are now kernel-owned. Nodes,
Ring 3, syscalls, and scheduling remain outside that next foundation milestone.

## Primary implementation references

- [Pinned Limine protocol specification](https://github.com/Limine-Bootloader/limine-protocol/blob/3a0526b700e356f0eac1b71a77697b3fd1c707a3/PROTOCOL.md)
- [Limine 12.9.4 release](https://github.com/Limine-Bootloader/Limine/releases/tag/v12.9.4), [configuration](https://github.com/Limine-Bootloader/Limine/blob/v12.9.4/CONFIG.md), and [UEFI usage](https://github.com/Limine-Bootloader/Limine/blob/v12.9.4/USAGE.md)
- [Rust freestanding x86-64 target](https://doc.rust-lang.org/rustc/platform-support/x86_64-unknown-none.html)
- [QEMU virtual FAT](https://www.qemu.org/docs/master/system/images.html#virtual-fat-disk-images)
- [Intel SDM](https://www.intel.com/content/www/us/en/developer/articles/technical/intel-sdm.html), Volume 3A: interrupt/exception frames, error codes, TSS and IST.

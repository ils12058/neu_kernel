# Neu Kernel implementation status

**Verified:** 2026-10-25. **Milestone:** `NEU-BOOT-001`, Phase 0 bootstrap.
**Result:** complete against the supplied Phase 0 acceptance criteria on the
documented QEMU environment. This does not complete the architecture document's
full boot-to-first-Node integration plan or establish physical hardware support.

## Implemented foundation

| Area | Current implementation |
| --- | --- |
| Rust project | One Cargo workspace and one dependency-free kernel crate, `no_std`, `no_main`, abort-style panic behavior. |
| Executable | Static ELF64/x86-64 at `0xffffffff80000000`, separate RW requests, RX text, R data, RW data/stack LOAD segments. |
| Entry | Small x86-64 assembly entry, IF/DF cleared, switch to a 16-byte-aligned 64 KiB kernel-owned stack before calling Rust. Never returns to the loader. |
| CPU checks | NX support, CR0 protection/paging/write-protect, CR3 presence, CR4 PAE with LA57 disabled, EFER long mode/NX, IF/DF clear, current stack within kernel bounds. |
| Boot adapter | Limine base revision 6; only firmware-type and memory-map feature requests, both revision 0. |
| Serial | COM1 at 115200, 8N1, loopback initialization check, bounded polling, no heap or logger locks. |
| Panic/failure | Bounded emergency serial output followed by `cli`/`hlt`; recursive-panic guard; no unwinding, scheduler dependency, or interactive monitor. |
| Host tooling | Python standard library plus Cargo/curl/QEMU; build/run/test/doctor through Make. SHA-256 checks for external loader artifacts. |
| Smoke tests | Normal boot, intentional Rust panic, and missing-memory-map rejection; actual CPU halt verified through QMP. |

Architecture-specific mechanisms are under `kernel/src/arch/x86_64/`. The Limine
adapter is `kernel/src/boot.rs`; generic bootstrap sequencing is in `main.rs`.
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

The active named Rust toolchain initially lacked the freestanding target; it was
added using `rustup target add --toolchain 1.95.0 x86_64-unknown-none`. No Homebrew
packages or system-wide firmware were installed for this milestone. Installed
QEMU supplied compatible firmware. ISO utilities and mtools were absent and are
not required by the selected boot workflow.

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
make run
```

`make check` checks Rust formatting and runs Clippy with warnings denied for the
normal kernel and each fault-injection feature separately. The two test features
are mutually exclusive and off in ordinary builds.

| Scenario | Actual result |
| --- | --- |
| Build | Passed: x86-64 ET_EXEC ELF, entry `0xffffffff80001000`, four LOAD segments with no writable/executable segment, 64 KiB stack. |
| Fresh-directory build | Passed from a source copy with no loader cache or Cargo target directory: pinned download, checksum verification, compilation, and EFI artifact creation. |
| Static checks | Passed: formatting, Clippy with warnings denied for all three configurations, and Python bytecode compilation. |
| Normal boot | Passed: Limine handoff, serial banner, CPU/stack and memory-map validation, bootstrap-success and idle messages. |
| `test-panic` | Passed: controlled panic after boot validation, bounded panic output, no bootstrap-success message. |
| `test-missing-memory-map` | Passed: the test image omits the request from Limine's delimited request area; the null response is rejected and a boot failure is reported before dereference. |
| Halt proof for all three | Passed: QMP observes the VM still running with its CPU halted twice, CPL=0, IF=0, and RIP inside the ELF's `neu_halt_begin`/`neu_halt_end` range. QEMU then exits with status 0 after the host requests `quit`. |
| Runner failure behavior | Passed: `python3 tools/neu.py test --case boot --timeout 0.01` returned the expected exit 1 with a timeout, a failed JSON report, and VM cleanup. A nonfinite `--timeout nan` was rejected before launch. |
| Interactive run | Passed: `make run` displayed the normal bootstrap output over serial stdio; Ctrl-C stopped QEMU after the guest entered idle. This intentional interruption reports status 130 through Make. |

Observed normal kernel output, excluding firmware/loader console output:

```text
[NEU] Neu Kernel
[NEU] Architecture: x86_64
[NEU] Early serial initialized
[NEU] CPU and kernel stack validated
[NEU] Boot protocol initialized: Limine base 6, UEFI64
[NEU] Memory map validated: 32 regions, 486285312 usable bytes
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
```

Evidence is written to `build/test-results.json` and
`build/test-logs/{boot,panic,missing-map}.{serial.log,qemu.log,json}`. It includes
kernel/firmware identity, actual QEMU arguments, CPU-register observations, and
exit status. Build, cache, and runtime evidence are ignored by Git. These are
local tests; no hosted CI or physical-machine tests have been run.
`build/repro-build.log` and `build/timeout-check.json` preserve the additional
fresh-directory build and expected timeout-failure checks.

## Boot ownership and limitations

- The adapter checks the negotiated base revision **before** response use,
  requires UEFI64, and bounds entry counts at 4096. It checks higher-half pointer
  shape/alignment/range arithmetic, sorted nonempty memory regions, overflow,
  page alignment of usable/reclaimable RAM, and their non-overlap. Reserved
  regions may overlap as permitted by the protocol. Unknown types are never
  treated as allocatable.
- Pointer shape checks cannot prove mapping validity. Mapped, initialized
  response objects remain part of the trusted Limine contract. This is not a
  fault-recovering parser for malicious loader pointers or userspace memory.
- Only the validated memory-map summary escapes the boot adapter. No frames are
  allocated or reclaimed. The loader's GDT, page tables, response data, and all
  other bootloader-reclaimable memory remain intact throughout Phase 0.
- No kernel IDT, exception handlers, guard pages, timer, or interrupt handling
  exists. IF stays clear. Controlled Rust panic and detected validation failures
  halt safely; arbitrary CPU faults or stack overflow can still triple fault.
- ELF segment permissions are checked, but this is not a complete VMM or an
  alias-aware W^X guarantee; loader-provided direct mappings remain in place.
- Emergency output permits at most 512 bytes and 4096 status polls per byte;
  ordinary writes permit 1024 bytes per call. It uses no heap, locks, or waits.
  Serial-device loss, recursive panic, NMI, and hardware faults have not been
  separately fault-injected. The handler makes a best-effort diagnostic attempt.
- Single CPU, QEMU q35/TCG/COM1 only. SMP, Nodes, Ring 3, syscalls, capabilities,
  MemoryObjects, scheduling, IPC, userspace drivers, full NeuKMon, compatibility
  layers, and OS services remain unimplemented.

## Architecture relationship and next milestone

[ADR 0004](adr/0004-limine-phase-0-integration.md) appends acceptance of the scoped
Limine integration contract while retaining the original proposal. Architecture
revision 0.2 is preserved unchanged; its `NEU-004` entry describes the earlier
partially chosen/proposed state. `NEU-025` remains open for the full integration
plan, and the other proposed subsystem contracts are not promoted by Phase 0.

The draft's Section 22 K0 outline includes basic NeuKMon, while the supplied
Phase 0 brief limits diagnostics to early serial/panic support and excludes an
interactive monitor. This implementation follows the narrower brief and does
not claim completion of that K0 monitor item. Keep optional NeuKMon as later
work with its own concrete decision rather than silently changing the baseline.

Recommended next milestone: **K1 / Phase 1 memory and exception foundation**.
Define boot-memory ownership and reservations, preserve required boot data in
kernel-owned storage before any reclamation, install a minimal exception path,
and implement/test physical frame allocation. Ring 3 and Node execution follow
only after those foundations are ready.

## Primary implementation references

- [Pinned Limine protocol specification](https://github.com/Limine-Bootloader/limine-protocol/blob/3a0526b700e356f0eac1b71a77697b3fd1c707a3/PROTOCOL.md)
- [Limine 12.9.4 release](https://github.com/Limine-Bootloader/Limine/releases/tag/v12.9.4), [configuration](https://github.com/Limine-Bootloader/Limine/blob/v12.9.4/CONFIG.md), and [UEFI usage](https://github.com/Limine-Bootloader/Limine/blob/v12.9.4/USAGE.md)
- [Rust freestanding x86-64 target](https://doc.rust-lang.org/rustc/platform-support/x86_64-unknown-none.html)
- [QEMU virtual FAT](https://www.qemu.org/docs/master/system/images.html#virtual-fat-disk-images)

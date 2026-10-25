# Neu Kernel implementation status

**Verified:** 2026-10-25. **Milestone:** K1 / Phase 1B, kernel-owned paging and
controlled virtual memory. **Result:** complete against the supplied Phase 1B
acceptance criteria in the documented QEMU environment. The original seven host
tests and nine QEMU scenarios remain passing. This does not establish physical
hardware, SMP, userspace, or full boot-to-first-Node readiness.

The inspected starting tree was clean on `main` at
[`34ae7d6`](https://github.com/ils12058/neu_kernel/commit/34ae7d6bb66182105197269efe52ca39c15b53c3),
the pushed Phase 1A milestone. Its status and test evidence remain historical at
that commit. The baseline tests were rerun before implementation. Architecture
v0.2, the Rust target/toolchain, and the selected loader version are unchanged.

## Implemented foundation

| Area | Current implementation |
| --- | --- |
| Rust project | One dependency-free Cargo package; freestanding binary and `no_std` memory/paging library with host tests; no heap or nightly features. |
| Executable | Static ELF64/x86-64 at `0xffffffff80000000`; four page-aligned LOAD regions: RW requests, RX text, R/NX constants, RW/NX data/BSS/stacks. |
| Entry and CPU | Assembly switches to the 64 KiB normal stack, clears IF/DF, and validates long mode, NX, CR0.WP, PAE and four-level paging. Paging activation also requires PGE/PCIDE disabled. |
| Boot adapter | Limine base 6, revision-0 firmware/map/executable-address/HHDM requests; copied descriptors and HHDM scalar survive the CR3 switch without loader-response dereferences. |
| Physical ownership | Validated kernel-owned map; independent physical-image reservation; bounded sparse 4 KiB frame allocator. All loader-reclaimable RAM remains reserved. |
| Paging ownership | Fresh allocator-backed root and children; no copied loader parent pointers. Each table tracks physical owner, level, pinned state and hierarchy references. |
| Transition | Construct and verify required mappings under the loader root, then activate a distinct root and verify again. Pre-switch failure rolls back allocations; post-switch failures use fatal diagnostics. |
| Mapping and access | Internal 4 MiB test region; single/batch map, unmap, ownership references, generation identities, alias-aware W^X, scoped non-executable frame access and full-page zeroing. |
| Stacks and exceptions | Private GDT/TSS/IDT, 32 KiB double-fault IST1 stack, one genuinely absent guard page below/above each stack; fatal normalized exception record and bounded output. |
| Serial and panic | COM1 115200/8N1, bounded polling/output, no heap or locks; controlled `cli`/`hlt`, no unwinding or interactive monitor. |
| Tooling | Python standard library, Cargo/curl/QEMU/Make, pinned external-loader checksums, QMP halt/CR3/context verification. |
| Tests | 19 host tests (7 memory + 12 paging) and 18 QEMU scenarios (the prior 9 + 9 paging cases). |

Pure page-table entry/walk/transaction logic and ownership policy live in
`kernel/src/paging/`; physical pointers, scratch/table views, CPU operations and
integration self-tests live in `kernel/src/arch/x86_64/paging.rs`. The existing
`kernel/src/memory/` allocator still provides ownership only. The paging layer
adds mapping and initialization, rather than treating allocated addresses as
implicitly accessible. No empty future-subsystem modules have been created.

Created files:

- `kernel/src/paging/mod.rs` and `kernel/src/paging/tests.rs`: shared logic,
  mapping/data-owner transactions, and host tests.
- `kernel/src/arch/x86_64/paging.rs`: hardware access, table ownership, transition,
  initialization, and guest self-tests.
- `docs/adr/0007-kernel-owned-paging-and-controlled-mappings.md`: scoped decision.

Modified files:

- `kernel/src/boot.rs`, `kernel/src/main.rs`, `kernel/src/lib.rs`, and
  `kernel/src/arch/x86_64/mod.rs`: bootstrap metadata, sequencing and module wiring.
- `kernel/linker.ld`, `kernel/src/arch/x86_64/exceptions.rs`, and
  `kernel/src/arch/x86_64/exceptions.S`: page bounds, guards and bounded probes.
- `kernel/Cargo.toml` and `tools/neu.py`: isolated test features, ELF validation,
  additional scenarios and QMP CR3/context checks.
- `README.md`, this status, `docs/adr/README.md`, and ADRs 0004/0005/0006:
  current behavior, evidence and dated relationship corrections.

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
not required by the selected boot workflow. Phase 1B adds no host packages,
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

## Verification and actual results

Commands executed from the repository root:

```sh
make doctor
make check
make build
make test
cargo test --locked --lib --target aarch64-apple-darwin
```

The final full run used the first three commands above followed by
`make test > build/phase1b-test-run.log 2>&1`. `make check` verifies formatting
and Clippy with warnings denied for all 18 kernel configurations and the host
library. `make test` runs all 19 host tests before all 18 QEMU cases. Test features
are mutually exclusive and disabled in ordinary builds.

| Verification | Actual result |
| --- | --- |
| Doctor | Passed: unchanged pinned Rust/target, installed QEMU and matching hashed firmware found. |
| Static checks | Passed: formatting and Clippy, all 18 configurations and host library. |
| Normal build | Passed: ET_EXEC x86-64 ELF, entry `0xffffffff80001000`, four LOAD segments, no writable/executable segment, 64 KiB/32 KiB stacks, four page-aligned guards. |
| Clean-directory build | Passed from a source copy excluding `.git`, `.cache`, `target`, `build`, and Python caches: pinned download/checksums, compilation, ELF/guard validation and EFI artifact creation. |
| Host memory | All 7 original tests pass, including sparse pools, malformed descriptors, exclusion/capacity, exhaustion/reuse/invalid frees and 5000 operations against an independent reference set. |
| Host paging | All 12 tests pass: canonicality/indices/overflow; width/alignment/flags; boot large pages and effective permissions; conflicts/restricted parents; every child-allocation failure; side-effect-free rejection; detached-table flush/reuse and pinning; initialization/aliases/stale IDs/duplicate physical ownership; multi-page rollback across a PT boundary; metadata capacity. |
| Normal boot | Pass: distinct owned CR3, complete image/workspace permission verification, continued serial output, bootstrap success and controlled halt. |
| Existing panic/missing-map/memory cases | All pass. Panic runs after activation. Missing-map rejects the absent response before paging. Memory self-test still exercises real/synthetic frame accounting before activation. |
| Existing #UD/#GP/#PF/#DF/#BP | All pass after activation, with the original exact vector/error/RIP/stack checks. The old #PF now inspects the owned active hierarchy before writing the read-only static. Genuine #DF still escalates failed #GP delivery onto IST1. |
| `paging` | Pass: full-page dirty/free/reallocate/zero, stale-owner rejection, actual CPU writes/reads through aliases, single/batch mappings, invalid-request rejection, free-in-use rejection, W^X aliases, rollback and restored frame/table counts. |
| `paging-bootstrap-failure` | Pass: injected exhaustion during root construction returns all new frames; unchanged loader CR3 and accounting, continued serial, controlled halt. |
| New permission/guard faults | All seven pass with the exact vector, error, CR2, RIP, CS/flags and handler stack checks below. |
| Halt proof | All 18 pass: two QMP observations of HLT at CPL=0/IF=0 inside the ELF halt loop, followed by host-requested `quit` and exit 0. The 16 activated images also compare actual CR3 to the distinct reported owned root; rollback verifies the retained old root. |

Each new protection test uses its own image. It prepares and checks the actual
mapping before executing one bounded faulting instruction on the normal stack:

| QEMU case | Hardware result | Expected fault address and execution context |
| --- | --- | --- |
| `unmapped` | #PF 14, error 0 (supervisor read, absent) | `0xffffa00000000000`, exact read-site RIP; a CPU-populated translation was unmapped/invalidated and its owner released first. |
| `readonly` | #PF 14, error 3 (supervisor write, protection) | Same test VA, exact write-site RIP. |
| `nx` | #PF 14, error 17 (supervisor instruction fetch, protection) | Same test VA and RIP; NX payload contains `ret` so a silent permission failure returns and is explicitly rejected. |
| `guard-low` / `guard-high` | #PF 14, error 0 | Exact low/high normal-stack guard symbols, exact bounded-read RIP. |
| `df-guard-low` / `df-guard-high` | #PF 14, error 0 | Exact low/high IST-stack guard symbols, exact bounded-read RIP. No recursive overflow is induced. |

The original faults retain #UD 6/0, #GP 13/`0x28`, static-write #PF 14/3,
genuine #DF 8/0, and #BP 3/0. #BP records RIP after `int3`; #DF requires IST1
rather than a recoverable RIP. The static-write #PF checks CR2 against the ELF
target. All other tested handlers run on the normal stack.

The default guest deadline remains 30 seconds with bounded cleanup. An absent
expected result, reset/shutdown, early emulator exit, unexpected success, wrong
CR3/fault context/halt address, or QMP failure fails the test. Serial text alone
cannot pass; no `isa-debug-exit` shortcut is used. The original Phase 0 timeout
and interactive-run experiments remain historical at that milestone; these
mechanisms have been retained, not newly claimed as independently exercised.

For selected cases or a slower host:

```sh
make test TEST_TIMEOUT=60
python3 tools/neu.py test --case paging
python3 tools/neu.py test --case paging-bootstrap-failure
python3 tools/neu.py test --case unmapped
python3 tools/neu.py test --case readonly
python3 tools/neu.py test --case nx
python3 tools/neu.py test --case guard-low
python3 tools/neu.py test --case df-guard-high
python3 tools/neu.py test --case double-fault
```

Evidence is local and Git-ignored: `build/test-results.json`,
`build/test-logs/<case>.{serial.log,qemu.log,json}`, `build/phase1b-test-run.log`,
and `build/phase1b-repro-build.log`. Per-case records include ELF/firmware identity,
QEMU arguments, registers, CR3, exception context, stack selection, halt proof and
exit status. No hosted CI or physical-machine validation has been performed.

## Page-table construction and transition

The image's physical extent comes from the validated Executable Address response
and linker bounds. Every image page has a matching reserved physical page at the
same offset. The new hierarchy maps requests, code, constants, data/BSS, both
stacks, descriptors, map copy, allocator bitmap and fault record. The four guard
PTEs are absent; their physical pages remain image-reserved. Port-I/O serial needs
no extra MMIO mapping. Image and table physical pages cannot be data-frame owners.

Before CR3 activation, the HHDM is a temporary, non-owning bootstrap view. Loader
table reads require whole pages in retained loader-reclaimable regions and valid
physical width. The walker understands 1 GiB/2 MiB leaves and accumulated parent
permissions. Before zeroing/accessing a newly owned frame, it verifies that the
HHDM VA actually translates to that physical frame, supervisor and writable.
Mapping of the loader's own retained table pages remains a trusted Limine
prerequisite; numeric pointer checks alone cannot prove arbitrary mappings safe.

All new tables are exclusively allocator-owned and zeroed before publication.
The complete image, workspace aliases, table levels and parent counts are checked
before switching. The root has zero hierarchy parents; every child has exactly
one. The normal image initially owns seven tables. Failed inactive construction
clears every new parent/leaf reference and returns new frames, ending temporary
borrows first. It neither edits nor frees loader tables. The old HHDM still
covers RAM during this rollback, but is not an owning reference to those frames.

Activation requires NX/write-protect, four-level mode, IF=0, and PGE/PCIDE clear.
The new root is distinct from the loader root. CR3 loading flushes the relevant
translations and paging-structure caches under those requirements. Actual CR3 and
all required mappings are verified again before success. The new hierarchy has
no broad HHDM; scalar boot information was copied before the switch and loader
response pointers are not dereferenced afterwards. Failure after the switch is
fatal and does not attempt to free the active hierarchy.

## Mapping, initialization and release lifetimes

| Resource | Bound and lifetime |
| --- | --- |
| Table owners | At most 64 allocator-backed frames. Root plus image/workspace tables are pinned for kernel life; dynamic empty children can be retired. |
| Table workspace | `0xffff900000000000`, one RW/NX 4 KiB alias per table slot. New alias is installed before a parent publishes its table; removed before physical reuse. |
| Scratch view | Workspace index 511, `0xffff9000001ff000`. One exclusive R/NX or RW/NX frame borrow, removed and invalidated before return. References cannot escape the safe closure API. |
| Data owners | At most 16 live opaque slot/generation identities. Duplicate physical owners and stale identities are rejected. Allocation zeroes all 4096 bytes before returning an initialized ID. |
| Controlled mappings | At most 64 records in the 4 MiB region at `0xffffa00000000000`. Each refers to a live initialized owner and records R, RW/NX or RX permissions. |

Physical allocation, temporary access, initialization, persistent mapping and
release are separate operations. The underlying allocator does not access or
zero contents. The paging wrapper owns each allocated data frame, zeroes it via
a validated view, then publishes the initialized generation identity. The QEMU
reuse test dirties the whole frame, frees it, obtains that same physical frame,
and checks every byte is zero before mapping it again. This supplies a tested
mechanism for future userspace initialization without exposing userspace now.

Map transactions validate range/alignment/canonicality, frame initialization,
physical width, conflicts, W^X aliases, and metadata capacity. Ownership policy
and capacity are checked before leaf installation; each encoded entry also
checks physical width. A failure rolls back any earlier leaves and new children
before committing mapping references.
Parent permissions are never weakened to satisfy a child request. Internal new
entries are 4 KiB, supervisor, nonglobal WB RAM mappings. Large pages are decoded
only for bootstrap inspection, not created by the controlled manager.

Unmapping removes the leaf, executes `invlpg`, then drops its ownership record.
The backing owner is retained until explicitly freed; any remaining mapping
prevents free. Empty unpinned children are detached from their parents, then CR3
is reloaded to flush walk caches before the workspace alias is cleared and
invalidated and the table is freed. Release also refuses live parent references,
nonempty tables, pinned tables and the active root. No active-root destructor
reclaims these tables; the manager and allocator ledger remain live until halt.

Text has no writable alias in the new active hierarchy. An executable data alias
rejects writable aliases and mutable scratch access; a writable alias rejects RX.
Read-only NX aliases can coexist. Image physical pages remain reserved and cannot
enter the data registry. This W^X claim covers the supported active hierarchy;
retained inactive loader tables and arbitrary privileged writes are outside that
boundary. Raw addresses are not public ownership tokens or userspace APIs.

## Retained physical allocator and boot contract

- Descriptor count is bounded at 4096 (96 KiB kernel storage). The adapter checks
  base revision, UEFI64, pointer shape/alignment/arithmetic, ordered nonempty
  spans, overflow, page alignment of usable/reclaimable RAM, and their overlap.
  The copy is audited before immutable publication; loader descriptor pointers
  do not escape the adapter. HHDM is copied as a scalar, not a response borrow.
- Only full type-0 usable frames outside page zero and the complete physical
  image reservation are candidates. Unknown/reserved, firmware/ACPI/runtime,
  modules, holes and loader allocations remain unavailable. The old inactive
  tables remain reserved too. **No bootloader-reclaimable memory is released.**
- The 128 KiB bitmap represents candidate ordinals, supporting at most 1,048,576
  frames (4 GiB of eligible bytes), not a 4 GiB physical-address ceiling. Larger
  eligible pools fail initialization; holes consume no bitmap entries.
- Allocate/free require exclusive mutable access on one CPU with IF=0; invalid,
  unaligned and duplicate frees are rejected and `allocated + free = total`
  holds. The raw allocator cannot detect stale addresses after reallocation;
  generation checks belong to the added paging data-owner layer.
- Inactive loader CR3 is not sufficient proof for general reclamation. Other
  loader/firmware allocations and dependencies have not had a complete audit;
  reclaiming them remains a separate decision and implementation task.

## Exceptions and remaining limits

Private GDT code/data selectors remain `0x08`/`0x10`, TSS `0x18`, IDT 256 entries,
and #DF uses IST1. Assembly saves 15 GPRs and normalized hardware context, captures
CR2 before Rust, and publishes the 192-byte `NEU_LAST_FAULT` before bounded serial
output. All current exceptions are fatal, including #BP. Guard tests access fixed
absent pages from the healthy normal stack; they prove the mapping boundary, not
recovery from arbitrary stack corruption or overflow.

Verified vectors are 3, 6, 8, 13 and 14. Other categorized stubs, NMI/machine-check
hardening, the NMI window during descriptor replacement, recursive panic,
serial-device loss, and SIMD context have not been independently injected. IF
remains clear; no device interrupt framework is enabled. Emergency output remains
bounded at 512 bytes and 4096 polls per byte; ordinary output at 1024 bytes/call.

The supported configuration is one CPU on QEMU q35/TCG/COM1. Physical hardware,
other firmware versions, performance, and larger pool/capacity behavior in an
actual guest remain unverified. Mapping metadata and scans are intentionally
bounded. There are no locks/shootdowns or SMP safety claims, device cache-policy
API, recoverable faults, demand paging, copy-on-write, general VMM/heap,
MemoryObjects, Nodes, Threads, Ring 3, syscalls, capabilities, Channels, scheduler,
device drivers, full NeuKMon, compatibility layers, or OS services.

## Architecture relationship and next milestone

[ADR 0007](adr/0007-kernel-owned-paging-and-controlled-mappings.md) accepts this
bounded paging/access/guard contract, amending
[0005](adr/0005-boot-memory-ownership-and-frame-allocation.md) and
[0006](adr/0006-kernel-exception-descriptors-and-fatal-handling.md).
ADR 0005 now correctly **extends/amends** the still-Accepted
[0004 Limine integration](adr/0004-limine-phase-0-integration.md). Dated corrections
preserve the earlier Phase 1A supersession notation as history. The loader/base
pin and original rationale are retained; the ADR index is consistent.

Architecture v0.2 is unchanged. The PMM and scoped kernel-paging portions of
`NEU-018`, privileged diagnostics/guards in `NEU-023`, and current local tests in
`NEU-024` do not accept the broader Node/VMM/CI proposals. `NEU-025` remains open.
The Section 22 K0 outline also lists basic NeuKMon; the narrower supplied Phase 0
brief excluded an interactive monitor, so that outline item is not claimed here.

Recommended next milestone: **K2 / minimal Ring 3 and ABI**, starting with a
separate decision on user address-space ownership, zeroed user-page exposure,
entry/return and fault containment, then a bounded static-image fixture and
`abi_query`/`thread_exit` proof. This is a recommendation, not an accepted or
implemented userspace contract. Any reclamation work first needs its own complete
loader dependency audit; it is not a prerequisite to retain the current RAM safely.

## Primary implementation references

- [Pinned Limine protocol specification](https://github.com/Limine-Bootloader/limine-protocol/blob/3a0526b700e356f0eac1b71a77697b3fd1c707a3/PROTOCOL.md)
- [Limine 12.9.4 release](https://github.com/Limine-Bootloader/Limine/releases/tag/v12.9.4), [configuration](https://github.com/Limine-Bootloader/Limine/blob/v12.9.4/CONFIG.md), and [UEFI usage](https://github.com/Limine-Bootloader/Limine/blob/v12.9.4/USAGE.md)
- [Rust freestanding x86-64 target](https://doc.rust-lang.org/rustc/platform-support/x86_64-unknown-none.html)
- [QEMU virtual FAT](https://www.qemu.org/docs/master/system/images.html#virtual-fat-disk-images)
- [Intel SDM](https://cdrdv2-public.intel.com/819714/253668-sdm-vol-3a.pdf), Volume 3A: four-level entries, CR3/INVLPG translation and walk-cache invalidation, exception frames, TSS and IST.

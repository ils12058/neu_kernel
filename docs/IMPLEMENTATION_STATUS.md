# Neu Kernel implementation status

**Verified:** 2026-10-25. **Milestone:** K2 / Phase 2A, user address space and first
Ring 3 execution. **Result:** complete against the supplied Phase 2A criteria in
the documented single-CPU QEMU environment. All original 19 host tests and 18
QEMU scenarios still pass; the complete suite now has **23 host tests and 26
QEMU scenarios**. No full Node, native syscall ABI, scheduler or ELF loader is
implemented by this execution fixture.

The starting tree was clean on `main` at
[`c9b388d`](https://github.com/ils12058/neu_kernel/commit/c9b388d92879ff22c99b79b718fe1cd25d45cc09).
Its Phase 1B status remains historical there. The 19/18 baseline suite was rerun
successfully before changes (`build/phase2a-baseline.log`). Architecture v0.2,
Rust/target and the loader pin are unchanged. Current results are local;
physical hardware and hosted CI have not been validated.

## Implemented foundation and changed files

The freestanding dependency-free kernel retains its sparse 4 KiB physical-frame
allocator, copied boot descriptors, image reservation, owned kernel root,
controlled mappings, W^X/zeroing, GDT/TSS/IDT, normal/#DF stacks and bounded serial
panic/fault diagnostics. The kernel root initially retains seven pinned tables.
Ordinary builds keep the supervisor bootstrap/halt behavior. Ring 3 execution
and its DPL=3 gate are enabled only by explicit `test-user*` configurations.

Created:

- `kernel/src/arch/x86_64/userspace.rs`: fixture contract, origin/result checks,
  trusted continuation and ownership/cleanup sequencing.
- `kernel/src/arch/x86_64/userspace.S`: embedded independent payload, `IRETQ`
  bridge, kernel continuation, and dedicated guarded privilege-entry stack.
- `docs/adr/0008-isolated-ring-3-fixture-and-test-return.md`: scoped rationale.

Modified:

- `kernel/src/paging/mod.rs` and `kernel/src/paging/tests.rs`: lower-half policy,
  deliberate user entry encoding, unchanged supervisor checks, global backing
  alias/reference policy, and four additional host tests.
- `kernel/src/arch/x86_64/paging.rs`: private fixture root/tables, verification,
  allocation rollback, deactivation and complete teardown.
- `kernel/src/arch/x86_64/exceptions.rs` and `exceptions.S`: user descriptors,
  test gate, RSP0 selection, safe segment/base state and origin routing.
- `kernel/src/arch/x86_64/mod.rs`, `kernel/src/main.rs`, `kernel/Cargo.toml`:
  isolated test configuration and sequencing.
- `tools/neu.py`: eight user cases, fixture/stack ELF checks, direct CPL=3 QMP
  observation and independent boundary/teardown checks.
- `README.md`, this status, `docs/adr/README.md`, ADRs 0006/0007: current evidence
  and dated extensions, retaining original rationale.

No linker script, boot protocol, external dependency, production syscall,
architecture baseline or host package was changed. The existing BSS wildcard
collects the test-only assembly stack; its image reservation and guard mappings
were revalidated with clean builds.

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
not required by the selected boot workflow. Phase 2A adds no host packages,
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

## User address space, ownership and permissions

The fixture receives a distinct allocator-owned PML4 and **private children at
every level**, including the higher-half kernel image/workspace branches. No
kernel child-table pointer is copied or borrowed. Both roots map the same
permanently reserved kernel image backing supervisor-only: RX text, R/NX
constants and RW/NX data/stacks. Kernel code is never executed from a user PTE.
The serial interface is port I/O, not a user mapping; IOPL=0 and a TSS I/O-map
base beyond its limit deny user port access.

The existing table-owner ledger tracks each frame, level, parent references and
pinned state. The original kernel root/image/workspace stay pinned. The fixture
adds **10 privately owned table frames and 4 initialized backing frames** for the
observed layout. Its root has no hierarchy parent; each child has exactly one.
Workspace leaf aliases are access references, not child ownership. Both private
workspace branches expose all live table-owner slots only to supervisor mode.
There is no shared-subtree mutation or reclamation policy hidden in this design.

The user mapping interface is internal and limited to `0x400000..0x800000`.
U/S must be set at **every required level**; existing supervisor parents are
rejected instead of upgraded. The original supervisor mapper still rejects user
parents. Alignment, canonicality, overflow, physical width, duplicate locations,
W+X and incompatible physical aliases remain errors. Generation identities and
mapping references are shared across the supported kernel/user mapping domains,
so an RX user alias prevents mutable scratch access or a writable kernel alias.

| Fixture region | Virtual address | Backing and permission |
| --- | --- | --- |
| Code | `0x400000` | One freshly initialized owned page, user RX. |
| Constants | `0x401000` | Separate user R/NX page: integer 35 and test kind. |
| Writable data | `0x402000` | Separate user RW/NX page, result at offset 0; a `ret` at `+0x100` supports the bounded NX probe. |
| Low stack guard | `0x403000` | Absent; no backing allocation. |
| User stack | `0x404000` | Separate user RW/NX page; initial RSP `0x404ff8`. |
| High stack guard | `0x405000` | Absent; no backing allocation. |
| Unmapped test address | `0x406000` | Absent. Page zero is also absent. |

All backing pages are zeroed for all 4096 bytes before an identity is returned.
The trusted 267-byte position-independent fixture is stored as **data** in kernel
R/NX storage, copied through a scoped owned-frame RW/NX borrow, and then mapped
user RX. Its relative branches remain valid at the lower address; data VAs are
explicit fixture constants. It verifies zero entry GPRs, reads 35, adds 7, uses
push/pop on its own stack, writes 42 to its data page, reads its CS/SS, and executes
a bounded loop for host observation before completion or the selected violation.
No filesystem, ELF parser, dynamic linker, runtime or kernel-text alias is used.

Translation mutations and frame access/allocation/release require the original
kernel root. Once the fixture root is active, its mappings/workspace are frozen.
The fixture owner and allocator ledger remain live across execution, exception
handling, continuation and teardown. At most one fixture root can exist.

## Ring 3 entry and test-return contract

- `IRETQ` enters long-mode user **CS `0x3b`, SS `0x33`**, with canonical RIP
  `0x400000` and RSP `0x404ff8` (`RSP mod 16 = 8`). There is no usable return
  address; the fixture finishes through its test gate. All 15 GPRs start at zero,
  including RDI. No startup descriptor or `NeuStartInfo` layout is promised.
- Initial RFLAGS is 2: IF/DF/IOPL/TF/NT/AC clear. Synchronous faults remain
  deliverable. Hardware interrupts, timers and preemption stay disabled.
- FS/GS selectors and FS/GS/KERNEL_GS bases start at zero; no TLS or `swapgs`
  convention is introduced. User-origin assembly entry restores trusted kernel
  segments and clears bases before Rust runs.
- CR0.MP/TS make x87/SIMD unavailable rather than expose previous extended
  state. No extended-context implementation exists. The bridge preserves and
  restores the prior kernel CR0/EFER. EFER.SCE is clear during user execution;
  legacy SYSENTER CS is cleared too. Neither is a production syscall path.
- Kernel CS/SS remain `0x08`/`0x10`, TSS selector `0x18`, #DF uses the existing
  32 KiB IST1. Test builds append user descriptors while keeping selector
  `0x28` invalid for the original #GP regression.
- TSS.RSP0 selects a **separate 32 KiB supervisor RW/NX entry stack**, with low
  and high absent guard pages. The suspended Rust continuation remains on its
  untouched 64 KiB normal stack. Both roots preserve these kernel regions.
- Only the dedicated `int 0x80` interrupt gate has DPL=3, only during the
  explicit fixture run. It has its own entry stub and vector 128 classification.
  Other gates stay DPL=0; the #DF IST path remains separate. The gate is disabled
  and RSP0 restored after the trusted continuation resumes.

The CPU saves the full user RIP/CS/RFLAGS/RSP/SS frame on RSP0. Assembly saves all
GPRs and captures CR2. Rust records this in private `NEU_USER_RECORD` and verifies
exact expected CS/SS, vector/error/RIP/CR2, user RSP, safe flags, result registers,
actual current kernel CS, active user root and handler stack bounds. These user
values remain scalars: the boundary dereferences no user pointer and never
returns through an unchecked user frame. Ordinary CPL=3 faults terminate this
isolated fixture. CPL=0 faults and critical NMI/#DF/#MC paths retain fatal policy.

The boundary resumes a previously saved **trusted kernel continuation**, restoring
the original CR3, kernel stack and callee-saved registers. The kernel reads the
data result through its owned physical-frame view, then tears down. This is a
one-shot test completion mechanism, not a native syscall, Node exit API or
scheduler context switch. Unknown or malformed fixture outcomes fail validation;
no operation identifiers or arbitrary memory authority are dispatched.

## Deactivation, rollback and physical reuse

CR3 deactivation occurs before cleanup. PGE/PCIDE remain disabled, so the root
switch retires local user translations and walk caches. The inactive fixture's
**entire** set of parent, leaf and workspace entries is cleared before any table
is freed. Reload the kernel root, remove user mapping records, release empty
private tables after removing/invalidation of their remaining kernel workspace
aliases, then free backing identities. The original seven pinned tables remain.
Unmapping is distinct from freeing; every surviving alias/reference still blocks
backing release. The broader 64-table/16-data-owner/64-mapping bounds are unchanged.

Construction failure follows the same inactive teardown discipline. The positive
guest injects exhaustion at **all 10 table-allocation failpoints**, checking exact
frame/table-accounting restoration after each. It also creates/destroys a complete
inactive fixture before the actual execution. Successful completion and every
expected user fault restore the kernel root and exact accounting before normal
kernel diagnostics/halt. There is no automatic destructor that could reclaim an
active root. **All bootloader-reclaimable memory remains reserved.**

The retained PMM still uses the copied maximum-4096-descriptor map, independent
physical-image reservation, and 128 KiB bitmap over candidate ordinals (maximum
1,048,576 frames; 4 GiB of eligible bytes, not a physical-address ceiling). Only
usable full pages outside page zero/image are candidates. Loader, firmware,
ACPI/runtime, modules, unknown types and holes remain unavailable. General loader
reclamation still requires a separate complete dependency audit.

## Verification and actual results

Executed from the repository root:

```sh
make doctor
make check > build/phase2a-check.log 2>&1
make build
make test > build/phase2a-test-run.log 2>&1
cargo test --locked --lib --target aarch64-apple-darwin
python3 tools/neu.py test --case user
```

Each of the other seven user cases was also run individually. Clean-directory
validation copied source excluding `.git`, `.cache`, `build`, `target` and Python
caches, then ran:

```sh
make build
python3 -c "from tools.neu import build; build('user')"
```

Both cold builds passed with a fresh pinned-loader download, checksum checks,
compilation, ELF/stack/fixture validation and EFI artifact creation. No host
packages or Rust dependencies were added.

| Verification | Actual result |
| --- | --- |
| Doctor | Pass: pinned Rust/target, QEMU 11.0.0 and compatible hashed firmware. |
| Static checks | Pass: formatting and Clippy with warnings denied for all 26 kernel configurations plus host library. Python runner syntax check also passes. |
| Normal + clean builds | Pass: four LOAD segments, no writable/executable ELF segment, correct entry/stacks/guards; normal and Ring 3 EFI artifacts. |
| Original host tests | All 19 pass unchanged in intent, including sparse PMM, physical ownership, zero/reuse policy, supervisor paging, aliases, rollback and flush/reuse invariants. |
| Added host tests | All 4 pass: U/S at every level/lower-range rejection; no parent escalation between domains; user transaction rollback and cross-domain W^X/reference retention; independent private roots preserving original children on teardown. |
| Original QEMU scenarios | All 18 pass, including actual owned kernel CR3, memory, panic, missing-map, #UD/#GP/#PF/#DF/#BP, paging rollback/zeroing and four original guards. |
| New QEMU scenarios | All 8 pass: one successful computation/trap and seven expected security violations, each with direct CPL=3 observation, exact boundary context, trusted kernel entry and cleanup. |
| Final halt | All 26 pass two QMP observations of HLT=1/CPL=0/IF=0 inside the ELF halt loop, then host-requested quit/exit 0. New user cases restore the original kernel root. |

The harness connects while the fixture runs its bounded observation loop. It
requires an **actual QMP CPL=3 snapshot**, with the distinct user CR3, user RIP/RSP,
CS/SS, HLT=0, safe flags, CR0.MP/TS, EFER.SCE=0, and zero FS/GS bases. Completion
without that observation fails. It then independently checks the hardware-saved
boundary frame and final original-root halt. Serial announcements alone cannot
pass. The normal per-guest deadline remains 30 seconds; unexpected reset, shutdown,
triple fault, emulator exit, wrong context/halt/root, timeout or silent permission
failure fails the case.

Observed positive CPU state in the full run included:

```text
CPL=3  CS=003b  SS=0033  RIP=000000000040005b
RSP=0000000000404ff8  CR3=000000000000c000  HLT=0
RAX=000000000000002a  RBX=000000000000002a
kernel root=0000000000001000
```

The completion frame then recorded vector 128/error 0, RIP `0x400109`,
CS `0x3b`, SS `0x33`, RSP `0x404ff8`, result 42, kernel CS `0x8`, handler RSP
inside the dedicated entry stack and CR3 `0xc000`. These root values are observed
allocations, not a fixed physical ABI.

| User case | Actual CPU outcome | Exact target/context |
| --- | --- | --- |
| `user` | vector 128, error 0 | Post-`int 0x80` RIP; result 42 and original user RSP. |
| `user-kernel` | #PF 14, error `0x5` | Supervisor read at `0xffffffff80000000`, CR2 exact. |
| `user-readonly` | #PF 14, error `0x7` | User write to R/NX constants at `0x401000`. |
| `user-nx` | #PF 14, error `0x15` | Instruction fetch at `0x402100`; RIP=CR2, saved RSP `0x404ff0` after the user call. |
| `user-unmapped` | #PF 14, error `0x4` | User read at absent `0x406000`. |
| `user-privileged` | #GP 13, error 0 | Exact `hlt` instruction RIP at CPL=3. |
| `user-gate` | #GP 13, error `0x40a` | Exact `int 0x81` RIP; IDT gate DPL restriction. |
| `user-guard` | #PF 14, error `0x4` | User read at low stack guard `0x403000`. |

All fault RIPs are computed independently from ELF fixture labels. All eight
require the actual original CS/SS/RSP/flags and kernel handler stack. The new
kernel entry-stack guards and upper user-stack guard are verified absent by the
walker; the new fault-injection guard case accesses the lower user guard. The
original four kernel/#DF guard-access cases remain separate passing tests.

Local Git-ignored evidence: `build/test-results.json`,
`build/test-logs/<case>.{serial.log,qemu.log,json}`, `build/phase2a-baseline.log`,
`build/phase2a-check.log`, `build/phase2a-user-first.log`,
`build/phase2a-negative-run.log`, `build/phase2a-test-run.log`, and
`build/phase2a-repro-build.log`. User JSON includes the direct QMP CPL=3 register
dump and parsed boundary context in addition to final halt/root observations,
ELF/firmware hashes, QEMU command and exit status.

## Limits, architecture relationship and next milestone

Supported execution is one trusted embedded integer-only fixture, one user root,
one CPU, IF=0, no resumption of user faults. It does not supply CPU-time isolation:
a different infinite user loop could occupy the CPU until the host timeout.
Arbitrary image loading, adversarial entry-context fuzzing, user pointer copies,
TLS, FP/SIMD context support, port-I/O/syscall-instruction/FPU rejection injections,
physical hardware, alternate firmware, NMI/#MC hardening and performance remain
unverified or deferred. No speculative side-channel, SMP/shootdown, arbitrary
kernel-corruption recovery or production-readiness claim is made.

There is no NodeFactory/lifecycle/capability API, native syscall dispatch,
preemptive scheduler, Channels, MemoryObjects, ELF loader, demand paging, COW,
userspace driver, POSIX compatibility or general runtime. The emergency privileged
fault/panic path remains bounded/fatal, including genuine #DF on IST1.

[ADR 0008](adr/0008-isolated-ring-3-fixture-and-test-return.md) accepts only this
execution-test contract and extends ADRs 0006/0007 with dated history. Their
original rationale is preserved; 0005 still amends the Accepted 0004 Limine
integration. The broader Node, execution, startup and native syscall drafts
remain proposals. Architecture v0.2 is unchanged; `NEU-025` remains open. The
historical K0 outline's optional NeuKMon item is still not implemented.

Recommended **Phase 2B**: separately decide and implement native `SYSCALL` entry,
trusted kernel stack selection, register/result/clobber conventions, safe validated
return, and the smallest `abi_query`/`thread_exit` fixture. Define any startup
wire layout and user-copy requirements before exposing them; test invalid syscall
numbers and malicious contexts. Reuse these ownership/context/cleanup proofs
without expanding into scheduling, capabilities, Nodes or an OS runtime.

## Primary references

- [Pinned Limine protocol](https://github.com/Limine-Bootloader/limine-protocol/blob/3a0526b700e356f0eac1b71a77697b3fd1c707a3/PROTOCOL.md) and [12.9.4 release](https://github.com/Limine-Bootloader/Limine/releases/tag/v12.9.4).
- [Rust x86-64 freestanding target](https://doc.rust-lang.org/rustc/platform-support/x86_64-unknown-none.html).
- [QEMU virtual FAT](https://www.qemu.org/docs/master/system/images.html#virtual-fat-disk-images).
- [Intel SDM, Volume 3A](https://cdrdv2-public.intel.com/819714/253668-sdm-vol-3a.pdf): U/S permissions, long-mode interrupt/IRET frames, TSS and CPU privilege state.

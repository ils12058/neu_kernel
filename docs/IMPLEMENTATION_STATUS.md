# Neu Kernel implementation status

**Verified:** 2026-10-25. **Milestone:** K3 / Phase 3A, independent bootstrap
Nodes, private address spaces and user-fault containment. **Result:** complete
against the supplied Phase 3A criteria in the documented single-CPU QEMU profile.
The suite has **43 host tests and 40 QEMU scenarios**, preserving all original
**32 host tests and 34 QEMU scenarios**. Two Nodes retain their resources before
either initial Thread executes; execution is sequential through trusted control.
This does not complete capability governance or implement scheduling.

The inspected tree was clean on `main` at
[`ae69d2e`](https://github.com/ils12058/neu_kernel/commit/ae69d2ea73da4332c0bd122af0a50fb60347dfdc).
The original 32/34 suite was rerun before implementation, recorded in
`build/phase3a-baseline.log`. Historical Phase 2B status remains at that commit.
Architecture v0.2, Rust/target and Limine are unchanged. Tests are local;
physical hardware and hosted CI have not been validated.

## Created and modified files

Created:

- `kernel/src/node.rs`: typed Node/Thread identities, associations, bounded
  lifecycle, copied outcomes and seven host tests.
- `kernel/src/arch/x86_64/nodes.rs`: trusted two-domain construction, execution,
  survivor validation, fault checks, rollback and independent reclamation.
- `docs/adr/0010-independent-bootstrap-nodes-and-fault-containment.md`: rationale.

Modified:

- `kernel/src/paging/mod.rs`: two generation-checked user address spaces, frame
  owners, mapping keys `(SpaceId, VA)`, root binding and active-resource guards.
- `kernel/src/paging/tests.rs`: four additional host tests for coexistence,
  cross-owner/root rejection, rollback, stale identities and exhaustion.
- `kernel/src/arch/x86_64/paging.rs`: multiple private roots, per-owner cleanup,
  own-only user workspace aliases, kernel-root-only backend access.
- `kernel/src/arch/x86_64/native.rs` and `native.S`: per-execution trusted binding,
  session reset, copied Node outcomes, user fault capture and distinct payloads.
- `kernel/src/arch/x86_64/userspace.rs`: explicitly deactivate ownership metadata
  after the original interrupt-gate fixture restores the kernel root.
- `kernel/src/main.rs`, `kernel/src/lib.rs`, `kernel/src/arch/x86_64/mod.rs`,
  `kernel/Cargo.toml`: module wiring and six mutually exclusive Node scenarios.
- `tools/neu.py`: per-Node three-phase QMP observations, independent identity,
  syscall, actual exception, ordering, ownership and accounting verification.
- `README.md`, this status, `docs/adr/README.md`, ADRs 0007 and 0009: current
  behavior and dated extensions preserving earlier rationale.

No linker, boot protocol, exception assembly, syscall numbers/status definitions,
host package or external Rust dependency changed. Architecture v0.2 is preserved
byte-for-byte (SHA-256
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
not required by the selected boot workflow. Phase 3A adds no host packages,
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
| Node | Created → Configuring → Ready → Active → Exited → Reclaimed. Construction failure can exit Created/Configuring. |
| Thread | Configuring → Ready → Running → Exited → Reclaimed; one initial Thread per Node is current bootstrap policy. |
| Normal exit | Ends the bound Thread. Node exits only after its final live associated Thread ends; a model test checks that rule independently of bootstrap policy. |
| User fault | Ends only that Node's associations, copies actual fault metadata, returns to the trusted controller without resuming the Thread. |
| Construction failure | Separate `ConstructionFailure` outcome; owned paging resources roll back before model reclamation. |
| Reclamation | Requires Exited, no active context/live Thread and released address-space association. Outcomes remain until metadata-slot reuse. |

`ExitReason` separates `Normal(u8)`, `UserFault(Fault)` and
`ConstructionFailure`. `Fault` distinguishes actual CPU exceptions from rejected
return state and copies vector, error, RIP, RSP, CS, SS, RFLAGS and CR2. No
user-memory diagnostic structure is dereferenced. Generation exhaustion refuses
reuse rather than wrapping. This is a small internal model, not a scheduler,
public object API or multiple-runnable-Thread implementation.

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
| `0x402000` | Private RW/NX data, result 42 | Different RW/NX data, result 84 |
| `0x403000` | Absent low stack guard | Absent low stack guard |
| `0x404000` | Private RW/NX stack | Different RW/NX stack |
| `0x405000` | Absent high stack guard | Absent high stack guard |
| `0x406000` | Absent | Private R/NX sentinel `0x5152535461626364` |

All allocated backing is zeroed before initialization/publication. Initial RSP is
`0x404ff8`; the syscall wrapper CALL gives saved RSP `0x404ff0`. The payload
checks the complete query output and register canaries, stores seed+7, and B
actually reads/checks its private-only sentinel. Fixture values are test data,
not ABI requirements. No user code shares the kernel payload's physical backing.

Both scopes are constructed and verified before either executes. The observed
profile needs ten private table frames per root, A's four backing frames and B's
five: 29 domain frames plus seven pinned kernel table frames. Static guarded
entry/emergency stacks remain image-reserved. The existing bounds are unchanged:
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
make doctor > build/phase3a-doctor.log 2>&1
make check > build/phase3a-check.log 2>&1
make build > build/phase3a-build.log 2>&1
make test > build/phase3a-test-run.log 2>&1
cargo test --locked --lib --target aarch64-apple-darwin
python3 tools/neu.py test --case nodes
python3 tools/neu.py test --case nodes-a-fault
python3 tools/neu.py test --case nodes-b-fault
python3 tools/neu.py test --case nodes-cross
python3 tools/neu.py test --case nodes-reclaim
python3 tools/neu.py test --case nodes-rollback
```

All six focused Node cases were also executed before the complete suite. A fresh
source copy excluding `.git`, `.cache`, `build`, `target` and Python caches ran:

```sh
make build
python3 -c "from tools.neu import build; build('nodes')"
```

Both cold builds pass with a fresh pinned-loader download/checksum verification,
Cargo `--locked`, ELF64/x86-64 four LOAD segments, no writable/executable ELF
segment, validated stacks/guards, native SYSCALL bytes and EFI artifacts. No host
packages or external Rust dependencies were installed.

| Check | Actual result |
| --- | --- |
| Doctor | Pass: unchanged Rust/target, QEMU and hashed OVMF pair. |
| Static checks | Pass: formatting and Clippy with warnings denied, 40 kernel configurations plus host library; Python syntax passes. |
| Host tests | **43 pass**: all prior 32 plus 7 lifecycle/association/outcome tests and 4 scoped paging/ownership/rollback/capacity tests. |
| Original QEMU cases | **All 34 pass**, including genuine #DF, supervisor faults/guards, paging rollback/zeroing, all 8 Phase 2A cases and all 8 native ABI cases below. |
| New Node cases | **All 6 pass**, described below, with two live roots before either execution. |
| Final halt | **All 40 pass** two QMP HLT=1/CPL=0/IF=0 observations in the ELF halt loop, correct final kernel CR3 and host-requested quit/exit 0. |

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
`build/test-logs/<case>.{serial.log,qemu.log,json}`, `build/phase3a-baseline.log`,
`build/phase3a-doctor.log`, `build/phase3a-check.log`, `build/phase3a-build.log`,
`build/phase3a-nodes-first.log`, `build/phase3a-isolation-run.log`,
`build/phase3a-test-run.log`, and `build/phase3a-repro-build.log`. Each Node JSON
contains separate identity/root/outcome records, raw three-phase CPU snapshots,
actual fault values, accounting, final halt observations, ELF/firmware identities,
QEMU arguments and clean exit status.

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

The two-Node implementation remains trusted bootstrap machinery. Node-local
capabilities, public factories/ResourcePools, multiple runnable Threads per Node,
scheduling, Channels/IPC, shared MemoryObjects, pointer-copy APIs, full ELF
loading, a userspace runtime, filesystem/network/drivers and POSIX/Linux
compatibility remain unimplemented. Construction/teardown assumes one trusted
controller with no asynchronous observers. A future concurrent object system
must revisit synchronization, references, cancellation and lifetime publication.

## Architecture relationship and recommended next milestone

[ADR 0010](adr/0010-independent-bootstrap-nodes-and-fault-containment.md) accepts
the scoped Node/Thread boundaries, two live private roots, sequential bindings,
local user-fault policy and ordered ownership cleanup. ADRs 0007 and 0009 retain
their original rationale with dated extensions. The accepted native ABI 0.1 and
all earlier test paths remain intact. Architecture v0.2 is unchanged; complete
capability-oriented Nodes and the full MEM/EXEC/SEC/DIAG/TEST proposals are not
accepted or implemented by implication. `NEU-025` and optional NeuKMon remain open.

Recommended next milestone: **K4 / single-CPU timer-driven scheduling**, beginning
with an explicit ADR for interrupt/context ownership, saved CPU state, safe
native-entry interactions and scheduling policy. Establish timer delivery and
controlled context switching with focused fault/cleanup tests before claiming
preemption or CPU-time isolation. Keep public capabilities/factories, SMP and
multithreaded Nodes as separate scoped decisions rather than extending this
single-active-context mechanism without a new safety contract.

## Primary references

- [Pinned Limine protocol](https://github.com/Limine-Bootloader/limine-protocol/blob/3a0526b700e356f0eac1b71a77697b3fd1c707a3/PROTOCOL.md) and [12.9.4 release](https://github.com/Limine-Bootloader/Limine/releases/tag/v12.9.4).
- [Rust x86-64 freestanding target](https://doc.rust-lang.org/rustc/platform-support/x86_64-unknown-none.html).
- [Intel instruction reference](https://www.intel.com/content/dam/www/public/us/en/documents/manuals/64-ia-32-architectures-software-developer-vol-2b-manual.pdf): SYSCALL/SYSRET state and selector rules.
- [Intel system programming reference](https://cdrdv2-public.intel.com/819714/253668-sdm-vol-3a.pdf): paging, interrupt frames, TSS/IST and privilege boundaries.

# Neu Kernel implementation status

**Verified:** 2026-10-26. **Milestone:** K4 / Phase 4D, multiple preemptible
Threads in one Node, trusted bootstrap on one CPU. **Result: 87 host tests and
83 QEMU scenarios pass**, including all previous 72/71 regressions. The implementation and
verification below apply to the bounded QEMU profile, not hosted CI or physical
hardware. The starting tree was clean on `main` at
[`51b9e30`](https://github.com/ils12058/neu_kernel/commit/51b9e30c41aeefa5a38fc6cb03131293c5516f1b).
All **72 host / 71 QEMU** baseline tests were rerun before implementation
(`build/phase4d-baseline.log`). Architecture v0.2 remains byte-for-byte unchanged:
SHA-256 `dede67c23c5b3044454e07356d262af2d8392b5e5a15a35651c0f960296b5cbd`.
No dependencies, boot protocol, linker, Rust pin, loader or firmware changed.

## Phase 4D files and ownership model

Created `kernel/src/arch/x86_64/multithread.rs` and `multithread.S` for trusted
member construction, real shared-data computation, independent outcomes,
rollback/reuse and timer-driven execution. [ADR 0014](adr/0014-multiple-threads-in-one-node.md)
records why one Node root and separate Thread lifetimes were chosen.

Modified:

- `kernel/src/node.rs` and `scheduler.rs`: three generation-checked Thread slots,
  bounded member admission, persistent Node Active state, independent Thread
  reclamation, fault cancellation and FIFO purge. Nine new host tests exercise
  multiple-member lifecycle, capacity, identity, outcomes and edit boundaries.
- `kernel/src/paging/mod.rs` and `paging/tests.rs`: tagged Thread stack backing,
  scoped edits to otherwise pinned spaces and a checked release receipt; six new
  host tests cover ownership, permissions, duplicate/stale release, rollback,
  active/pinned references and shared-table lifetime.
- `kernel/src/arch/x86_64/paging.rs`: dynamic primary user stack location, a second
  member stack, three entry-stack slots, Node-owned dynamic table masks, guarded
  construction/rollback and exact per-Thread teardown.
- `kernel/src/arch/x86_64/native.rs` and `preemptive.rs`: selected Thread user-stack
  validation, existing single-use IRQ handoff, per-Thread high-water checks and
  common-stack handling of a pending one-shot before another dispatch.
- `kernel/src/arch/x86_64/cooperative.rs`, `cooperative.S`, `interrupts.rs`,
  `interrupts.S`, `exceptions.rs`, `nodes.rs`, `mod.rs`, `kernel/src/main.rs` and
  `kernel/Cargo.toml`: shared bridge/controller helpers, arena bounds and twelve
  isolated new test configurations. Earlier fixture behavior remains supported.
- `tools/neu.py`: actual three-member CPU/IRQ/PTE/TSS/stack observations, independent
  FIFO and atomic-progress reconciliation, fault and exact cleanup checks.
- `README.md`, this status, ADR index and dated extensions to ADRs 0010–0013.

A Node owns its SpaceId/root, image, data and page-table hierarchy. A Thread owns
its user stack, guarded supervisor entry stack, complete saved integer context
and exactly one Node association. Node Ready means configured but not yet run;
once run, Active persists while any live member remains. The model's separate
CPU binding identifies the sole Running Thread. A suspended or queued Node does
not return to Ready. The two earlier host assertions that expected Ready after
suspension now assert Active; their admission/context checks remain.

A normal `thread_exit` records only its caller's `Normal(status)`. A1 exits with
status 1, A2 with 2 and B1 with 3. A1's early exit leaves A2's saved frame, root pin,
code/data and stacks intact. A2 subsequently checks a shared completion flag. The
last live member's normal exit sets the Node outcome. A supported unrecoverable
fault records the offending Thread's actual exception metadata and the Node
fault, sets its live siblings to `Cancelled { by: ThreadId }`, clears their saved
records and removes them from the FIFO. Previously terminal outcomes remain.
Resources and outcome retention have separate lifetimes; metadata reuse advances
the generation and refuses stale identities. None of this exposes public creation
or treats same-Node Threads as separate authority domains.

## Phase 4D shared root, stacks and capacity

A1 and A2 have the same NodeId, SpaceId, root and RX/constant/RW data backing.
B1's identical numeric code/data VAs resolve to private backing in another root.
Node A additionally maps `0x410000` to its own data frame; that alias is absent in
B. B owns a private read-only sentinel at `0x406000`, absent in A. This supports
fault probes in both directions without cross-Node shared memory.

| Member / slot | Node | User stack page | Initial RSP | Supervisor stack, exclusive end |
| --- | --- | --- | --- | --- |
| A1 / 0 | A | `0x404000` | `0x404ff8` | `0xffffc00000001000..0xffffc00000003000` |
| B1 / 1 | B | `0x408000` | `0x408ff8` | `0xffffc00000005000..0xffffc00000007000` |
| A2 / 2 | A | `0x40c000` | `0x40cff8` | `0xffffc00000009000..0xffffc0000000b000` |

Each stack has absent 4 KiB low/high guards. User pages are RW/NX with U/S;
8 KiB entry stacks are supervisor RW/NX. User loop RSP is initial RSP minus 64;
the syscall wrapper adds its eight-byte CALL offset. Initial ABI alignment is
preserved. Each Thread checks three independent stack markers and full-width
integer canaries. Same-Node siblings can access each other's user pages: these
are distinct allocated stack ranges in one protection domain. Both A entry stacks
are supervisor-mapped in Root A; B's stack is mapped only in Root B. The common
kernel root maps none of these private stacks.

| Bound | Phase 4D value / reason |
| --- | --- |
| Nodes / live spaces | 2 / 2, unchanged |
| Threads / live members per Node | 3 / 2; the only metadata/arena expansion |
| Data-frame owners | 16, unchanged: A image/constants/data + two user stacks = 5; B image/constants/data + user stack + sentinel = 5; six entry-stack pages = 6 |
| Controlled mappings | 64, unchanged; Node A data alias adds a reference, not a frame owner |
| Table frames | 64, unchanged; observed live totals are 33 with x2APIC and 42 with private xAPIC UC views |
| Root pins | At most 2 per root, unchanged; A has 2 and B 1 before execution |

The x2APIC profile peaks at 49 allocated frames (33 tables + 16 data); xAPIC
peaks at 58 (42 + 16). The normal common baseline is seven allocated table frames.
No heap, broad pool expansion, public ResourcePool or MemoryObject is added.
Data-owner exhaustion is intentional at the three-member peak. Slot reuse is
performed after A1's stack release, while A2 remains pinned.

Each installed resumable Thread retains a pin while queued, running, saved or
in the IRQ handoff. Ordinary pinned-root mapping/frame/root mutations still fail.
A non-copyable `StackEdit` holds an exclusive model borrow and can be minted only
for an inactive uninstalled or terminal Thread, without queued/saved references.
The paging backend additionally requires common CR3 and IF=0. It checks the full
Thread generation, SpaceId/root, stack class and fixed slot range. It cannot use
this exception to edit shared image/data or another member's backing. Healthy
sibling pins remain held throughout construction, rollback and early cleanup.

After terminal IRQ/native/handoff completion, remove only the terminating Thread's
two entry-stack pages and user-stack page, release its own pin and free empty
page-table branches. Tables belong to the Node hierarchy, including shared stack
branches; they do not disappear with the first creator. A persistent dynamic
stack-table mask keeps these frames out of user workspace aliases after early
teardown. A `StackRelease` receipt is issued only when that generation has no
scoped mappings or tagged backing; consuming it permits metadata reclamation.
The final live member's cleanup then releases shared backing/root and Node state.
Active roots, retained continuations, pin under/overflow, duplicate release and
stale-generation stack edits remain rejected. Flush/invalidation precedes reuse.

## Phase 4D timer execution and atomic computation

The unchanged FIFO admits **A1, B1, A2** before any execution. Labels identify
fixture parameters, not kernel identities or a Node scheduling rule. Each hardware
capture preserves all fifteen GPRs and RIP/RSP/RFLAGS/CS/SS by value. The IRQ
acknowledges vector 48, clears its ISR bit, publishes one exact-owner handoff and
abandons its private-stack call chain. Common stack/root restoration and handoff
consumption precede any model/allocator/FIFO mutation. No suspended Rust stack
frame survives a switch. TSS.RSP0/native stack publication follows the selected
Thread even when the next Thread uses the same CR3. Return validation uses that
Thread's user range; an A2 context using A1's valid user page is rejected before
native setup or root activation.

The clockevent remains the calibrated provisional 10 ms LAPIC one-shot, armed at
the final user-entry boundary. Userspace requires IF=1; native entry through FMASK
and every scheduler/resource transaction retain IF=0. A lone runnable member
receives real IRQs and resumes directly without FIFO switching. Empty FIFO is a
controlled completion/halt, not an implemented idle/wait policy.

A first integration run exposed an expired timer pending after terminal SYSCALL
processing. Masking/stopping a source does not erase IRR. Phase 4D now settles a
pending event on the common stack through the existing bounded kernel-only IRQ
window before another user arm. That path acknowledges at most one stopped-source
event and never schedules the interrupted kernel frame. ISR/IRR, tick/EOI counts,
root and IF are checked. Final shutdown still masks/stops, drains if necessary,
disables APIC software delivery and clears native/IRQ mailbox state.

A1/A2 execute aligned `LOCK XADD` against one Node A data frame. Per-member stored
progress and six callee-preserved canaries are independent; additional volatile
GPR and stack witnesses are checked in real CPU/IRQ samples. B computes against
its private frame. There are no yield/syscall instructions in the measured work
loop. Query/exit occur only at the terminal boundary; ABI 0.1 remains query=1,
exit=2 and feature information `0x100000003`.

An IRQ can arrive after the atomic increment but before the separate per-Thread
progress store. The fixture therefore reconciles the saved RIP/RAX: at
`neu_multi_after_atomic`, successful count is stored+1; at `neu_multi_after_inc`,
RAX is stored+1; otherwise the stored count is authoritative. Cancellation and
rejected returns retain these pending completions before clearing saved state.
QMP independently recomputes the shared sum from actual memory and active/saved
frames, including a canceled sibling's last observed IRQ memory. It does not
pretend that the atomic increment plus following instructions form one atomic
transaction. This proves this bounded shared-memory fixture, not a general
synchronization library or cross-Node sharing mechanism.

FIFO fairness is per Thread. A Node with two eligible Threads can receive more
aggregate execution than a Node with one. No per-Node budget, priority, clocksource,
accurate CPU accounting or hard-real-time guarantee is claimed.

## Phase 4D QMP evidence and focused cases

The initial focused pass completed all twelve new scenarios. The observer stops
actual QEMU execution at selected restore, CPL=3 work-loop and hardware IRQ probes.
It reads CR3, all GPRs, selectors/flags, physical page-table walks, TSS.RSP0,
selected entry-stack storage, stack markers, Node data and LAPIC ISR before EOI.
The final oracle independently replays the Thread FIFO, matches consumed and
restored frames to the prior hardware frame, checks shared atomic totals, verifies
retired stack leaves disappear while sibling mappings remain, and checks terminal
pin/resource/controller accounting. An expected serial banner alone cannot pass.

A representative `threads` focused run recorded these **actual resumed CPL=3**
samples; all three had RIP `0x4000e1`, the loop's conditional branch, and matching
hardware IRQ/resume frames. Initial RIP was `0x400000`; contexts were not restarted.

| Dispatch | Member | CR3 | User RSP | Actual TSS.RSP0 | Data VA `0x402000` physical frame | Own stored progress | Node shared count |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 4 | A1 | `0xc000` | `0x404fb8` | `0xffffc00000003000` | `0xa000` | 2153161 | 4334557 |
| 5 | B1 | `0x1b000` | `0x408fb8` | `0xffffc00000007000` | `0x18000` | 2143492 | 2143492 |
| 6 | A2 | `0xc000` | `0x40cfb8` | `0xffffc0000000b000` | `0xa000` | 2202409 | 6562728 |

The same run made 18 selections and 15 copied preemptive handoffs (A1=4, A2=5,
B1=6), with 18 user timer IRQs/18 EOIs, including three no-peer direct returns.
A1 normally ended first with 8916497 increments; A2 ended later with 11877148.
Their shared total was **20793645**. B1's independent total was **21224118**.
All three returned statuses 1/2/3 respectively. Final accounting was exactly the
per-run baseline: allocated=7, free=118272, tables=7. Root addresses, work totals,
IRQs and elapsed durations are observations, not stable ABI constants or timing
promises. Free counts can differ across feature images; each case checks its own
baseline rather than another image's number.

| Added case | Checked outcome / boundary |
| --- | --- |
| `threads` | Three live identities before execution; shared A root/data, B isolation, real IRQ alternation, early A1 exit, later A2/B1 normal completion |
| `threads-xapic` | Same profile with x2APIC hidden; supervisor UC LAPIC PTEs in each root and real MMIO-controller IRQs |
| `threads-a1-fault` | Actual A1 #UD; saved A2 canceled, B1 completes |
| `threads-a2-fault` | Actual A2 #UD; saved A1 canceled, B1 completes |
| `threads-b-fault` | B1 #UD while both A members live; A1/A2 continue normally |
| `threads-cross` | B1 reads A-only `0x410000`: #PF error 4; both A members survive |
| `threads-a-cross` | A1 reads B-only `0x406000`: #PF error 4; A2 canceled, B1 survives |
| `threads-rollback` | Four A2 resource failpoints and invalid context; exact frames/tables/pins restored while A1/B1 stay queued with unchanged contexts |
| `threads-stale` | Early A1 retirement, real same-slot generation reuse/stack construction while A2 remains pinned; old admission rejected; replacement rollback exact |
| `threads-stack` | A1 user access to A2's supervisor stack: #PF error 5; A Node canceled, B1 survives |
| `threads-user-guard` | A2 reads its absent low guard `0x40b000`: #PF error 4; A1 canceled, B1 survives |
| `threads-return-stack` | A2 saved RSP changed to A1's valid user page; rejected before native/root activation, A1 canceled, B1 survives |

The return-stack case is an explicit trusted corruption injection, not a hardware
exception. Slot-reuse replacements are constructed internally and never executed;
stale identities cannot authorize the replacement. The fault cases preserve
copied per-Thread and Node outcomes while invalidating resumable references.
All new cases also exercise the final lone Thread's hardware IRQ return and
empty-queue completion.

The first B-fault integration attempt completed without a no-peer timer event:
the remaining finite workload could finish within one quantum. The fixture now
extends the final runnable member's limit before dispatch so each new case must
observe a real no-peer interrupt. This changes test work, not FIFO policy or the
10 ms clockevent. Early harness syntax/serialization issues were corrected before
the focused acceptance pass; failed attempts are retained in local development
logs. Timeout, reset, triple fault, early QEMU exit, fake IRQ and cooperative-yield
paths remain rejected as successful preemption evidence.

## Phase 4D full validation results

The final `make test` pass completed **87 host tests and 83 QEMU scenarios** at
the unchanged 30-second per-guest deadline. All original 72 host test names and
71 scenario feature bindings remain present; the earlier QMP observation/check
functions are unchanged. The complete original baseline was also rerun before
production edits. No acceptance criterion remains open within this profile.

| Check | Actual result |
| --- | --- |
| `make doctor` | Pass; Rust/Cargo 1.95.0, Python 3.14.7, QEMU 11.0.0 and the existing firmware hashes |
| `make check` | Pass; formatting and Clippy with warnings denied for all 83 kernel configurations and the host library |
| Python harness | `python3 -m py_compile tools/neu.py` passes |
| `make build` | Pass; ordinary EFI artifact, ELF64/x86-64, four LOAD segments |
| Host tests | **87 pass**: 72 retained + nine member/model tests + six scoped-stack paging tests |
| QEMU full suite | **83 pass**: 71 retained + all twelve new focused scenarios; clean QMP exit and validated kernel halt in every case |
| Initial focused pass | All twelve added cases pass with actual restore/user/IRQ observations |
| Clean-directory compilation | Four independent empty target directories: `boot`, `threads`, `threads-xapic`, `threads-rollback`; all pass ELF validation |
| Repeated scheduling | **9 pass**: three rounds each of `threads`, `threads-xapic` and `threads-a2-fault`, without rebuilding or changing the 30-second deadline |
| Clean-image QMP boots | **4 pass**: independently built `boot`, `threads`, `threads-xapic` and `threads-rollback` images |

In the full run, normal x2APIC/xAPIC guests made 18 selections each and 15 copied
handoffs (A1=4/A2=5/B1=6). Their total timer ticks/EOIs were 18/18 and 18/18;
the xAPIC run included one separately counted kernel drain. The new basic run
observed 33 table + 16 data frames = **49** allocated; xAPIC observed 42 + 16 =
**58**. Both restored allocated=7/free=118270/tables=7. Rollback/stale feature images
restored their own different free-count baselines exactly. All twelve cases
finished without active/saved/queued members, private stacks, native entry state,
IRQ mailbox ownership or live Node roots. LAPIC ISR/IRR were empty, timer counts
zero, vector masked and software delivery disabled before the controlled IF=0
halt. The longest full-suite guest was `threads-xapic`, **26.571 s**.

The three repeat durations were 17.561/18.522/18.541 s for `threads`,
26.639/24.959/24.989 s for `threads-xapic` and 16.675/15.852/16.668 s for
`threads-a2-fault`. Tick/drain counts varied while exact acknowledgments, context
ownership, shared sums and per-image cleanup remained valid. Every clean-built
image also passed the same CPU/controller/accounting oracle. No repeats required
a relaxed timeout or a source change.

Commands and local evidence:

```sh
make doctor
make check
make build
make test
python3 tools/neu.py test --case threads
python3 tools/neu.py test --case threads-xapic
python3 tools/neu.py test --case threads-a2-fault
python3 tools/neu.py test --case threads-rollback
python3 tools/neu.py test --case threads-stale
```

`build/phase4d-doctor.log`, `phase4d-check.log`, `phase4d-build.log` and
`phase4d-test.log` retain command output. `build/test-results.json` contains the
full 83-case report; `build/test-logs/<case>.json` contains the actual CPU/register,
PTE, stack, data and controller observations. `build/phase4d-focused.json` retains
the earlier twelve-case pass. Clean-build output is in `build/phase4d-cold.log`
and `build/phase4d-cold/builds.json`. Repeat results and isolated per-round
observations are under `build/phase4d-repeat/`; clean-image QMP results are in
`build/phase4d-cold/qmp-results.json`. The local driver scripts
`build/phase4d-repeat.py`, `phase4d-cold.py` and `phase4d-cold-qmp.py` record the
exact supplemental procedures. These are ignored local build artifacts;
rerunning the commands produces current observations, not immutable CI evidence.

## Retained Phase 4C and earlier references

The following sections retain earlier fixture details and measurements. Where
those profiles use one Thread per Node, fixed `0x404000` stacks or two-Thread
capacities, they describe that earlier execution profile; the current shared-Node
layout and lifecycle are specified above.

**Phase 4C historical verification:** 2026-10-26. **Milestone:** K4 / Phase 4C, single-CPU timer-driven
userspace preemption. **Result:** all supplied acceptance criteria pass within
the documented QEMU profile: **72 host tests and 71 QEMU scenarios**, preserving
all previous 64/56 regressions.
The inspected tree was clean on `main` at
[`77ce538`](https://github.com/ils12058/neu_kernel/commit/77ce5383992620cab375f10afefb2016a99c85ea).
The original **64 host tests and 56 QEMU scenarios** were rerun successfully
before implementation (`build/phase4c-baseline.log`). Architecture v0.2,
Rust/target, Limine and firmware remain unchanged. Local QEMU evidence does not
establish hosted CI or physical-hardware behavior.

## Created and modified files

Created:

- `kernel/src/preemption.rs`: bounded single-use handoff/publication policy and eight host tests.
- `kernel/src/arch/x86_64/preemptive.rs`: copied IRQ capture/records, safe common-boundary controller, work/terminal/lifetime checks and scoped negative hooks.
- `kernel/src/arch/x86_64/preemptive.S`: independent CPU-bound payload, stack witnesses, optional ABI/fault variants and unsupported-origin test window.
- `docs/adr/0013-single-cpu-user-preemption.md`: rationale, alternatives, consequences and scoped acceptance.

Modified:

- `kernel/src/context.rs`, `node.rs`, `scheduler.rs`: preemptible/interrupt origins, exact integer-state validity, timer suspension through the existing FIFO/lifecycle model.
- `kernel/src/syscall.rs`: explicit IF-enabled native-return validation; prior ABI constants and strict legacy return validation remain intact.
- `kernel/src/arch/x86_64/interrupts.rs` and `interrupts.S`: shared controller initialization, 10 ms one-shot lifecycle, actual user IRQ frame/probe and safe legacy drain support; original periodic timer intent preserved.
- `kernel/src/arch/x86_64/cooperative.rs` and `cooperative.S`: shared checked bootstrap/resource helpers, final arming boundary and existing common-stack transition.
- `kernel/src/arch/x86_64/native.rs`: preemptible native contract, per-slice IRQ outcome and common-boundary completion; ABI 0.1 unchanged.
- `kernel/src/arch/x86_64/paging.rs`: owned private UC LAPIC views and controlled test work-limit writes through owned backing.
- `kernel/src/arch/x86_64/exceptions.rs`, `mod.rs`, `kernel/src/main.rs`, `lib.rs`, `kernel/Cargo.toml`: feature wiring and isolated images.
- `tools/neu.py`: fifteen added cases, per-slice hardware/restore/user observations and independent FIFO/frame/controller/cleanup checking.
- `README.md`, this status, ADR index and dated extensions to ADRs 0009–0012.

Boot protocol, linker, dependencies and toolchain are unchanged. Architecture
v0.2 stays byte-for-byte unchanged (SHA-256
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
not required by the selected boot workflow. Phase 4C adds no host packages,
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

## Phase 4C user preemption contract

Two configured Threads enter the existing FIFO before either executes. The
selection policy still contains only generation-checked ThreadIds; it never
reads CPU registers, APIC state or paging structures. A timer-origin handoff
makes the Running Thread Runnable and appends it to the tail. Terminal Threads
are removed; an empty queue with no Running context closes the timer and reaches
the controlled IF=0 halt. With no competitor, the IRQ acknowledges and returns
directly to the same hardware frame without a common-stack/FIFO switch.

`SavedContext` now distinguishes initial, yielded and interrupt-origin records
and explicitly records whether execution is preemptible. The 160-byte user frame
remains fifteen GPRs plus RIP/CS/RFLAGS/RSP/SS. Interrupt capture preserves every
GPR, including RAX/RDX and RCX/R11; those are not syscall output/clobber fields in
this path. Initial preemptible flags are `0x202`. An interrupt record requires IF
and permits only the prior arithmetic flags, reserved bit 1 and hardware RF.
Initial/native contexts retain stricter RF rules. Selectors, canonical addresses,
owned RX range and private user stack bounds remain checked. Native resumption
still requires the sealed syscall continuation; an interrupt resumes its actual
position anywhere inside the verified embedded RX payload.

The IRQ assembly frame is **176 bytes**: GPRs at 0, vector at 120, normalized
zero error at 128, RIP at 136, CS at 144, flags at 152, RSP at 160 and SS at 168.
Compile-time assertions cover every boundary. The processor supplies the five
user-origin hardware words; the IRQ gate clears current IF and selects TSS.RSP0.
Actual CPL=0 IRQ SS is the long-mode null selector; saved user SS remains `0x33`.
The common bridge restores SS/DS/ES=`0x10` and verifies kernel CS/SS before its
Rust continuation and any later kernel drain window.
The capture does not synthesize a yield address or dereference a user pointer.

The narrow handler validates its exact frame at private-stack top minus 176,
its own stack bounds, IF/DF, current native binding/root, TSS.RSP0, selected stack,
clear native busy guard, user selectors and saved return context. It accesses
only copied publication/session values, its stack frame, bounded records and the
controller. It does not access the suspended mutable Nodes, Queue, Paging or
allocator values on the common stack. No live borrow into IRQ storage escapes.

A `Mailbox` grants one publication containing the full active identity, stack,
monotonic sequence and whether a peer is runnable. It refuses rearming, duplicate
publication, wrong owner/sequence, invalid context, double consumption and
sequence wrap. After proving the timer ISR bit, the handler masks/stops the
one-shot, sends exactly one EOI and verifies ISR clear, then fills the mailbox.
It abandons its call chain through the Phase 4B common continuation. RSP moves
to the permanent normal image stack **before** CR3 changes. The IRQ busy guard
is released only at that common-root/IF=0 boundary. The controller consumes the
record exactly once and updates lifecycle/FIFO there. No Rust IRQ or syscall
call chain is suspended on a Thread stack.

## IF, native entry and publication

The controller constructs/pins resources and changes scheduler/Node state at
IF=0. Native session, selected Thread stack and TSS.RSP0 are published and read
back before activating the matching private root. The final assembly bridge
validates that publication again and arms the clockevent immediately before
restoring registers/IRETQ. Selected user execution restores IF=1. There is no STI
inside a normal syscall, allocator, paging operation, scheduler or switch.

SYSCALL atomically applies the existing FMASK, including clearing IF before its
early RIP-relative guard/RSP operations. It selects the already validated private
stack before a push, call or user-memory dereference. Returning calls require IF
in this explicitly preemptible profile and restore it through validated IRETQ.
`abi_query=1`, `thread_exit=2`, ABI info `0x100000003`, statuses and register
conventions remain unchanged. Earlier cooperative/native/user images retain
IF=0 and all their prior assertions. The private yield remains confined to its
original cooperative tests; Phase 4C tests cannot pass by using it.

A timer expiring during a syscall remains pending in hardware while IF=0. The
validated return consumes that deferred interrupt after reopening Ring 3; it
captures a user frame rather than switching the Rust syscall frame. General
Ring 0 rescheduling is not supported. The only enabled Ring 0 path is the
existing bounded common-stack drain window after active user/native state ends;
it may acknowledge residual delivery without scheduling. Any other enabled
kernel origin fails with a precise terminal classification.

Integer-only execution retains MP/TS, zero FS/GS and disabled unsupported CR4
OSFXSR/OSXMMEXCPT/FSGSBASE/OSXSAVE controls in this profile. Unsupported controls
are rejected before user construction; they are not silently inherited as a
new supported extended-state contract. The x87 negative case verifies actual
#NM before FNINIT changes unsaved state. No x87/SSE/AVX/TLS state is advertised.
NMI/#MC IST2 and #DF IST1 retain their separate fatal policy; nested critical
recovery is not established by these tests.

## Clockevent, ownership and shutdown

Shared controller discovery/masking/readback and calibration are reused from
Phase 4A. A **provisional 10,000 us one-shot** is derived from three bounded PIT
channel-2 reference samples. Old timer images still use their original 1 ms
periodic settings and kernel windows. Arming occurs after slow validation and
test observations; kernel setup does not consume a newly selected slice.
Event, EOI, scheduling-selection and computation counters are recorded separately.
TCG/observer/handler overhead means none is precise wall time or CPU accounting.

x2APIC uses the verified MSR interface. xAPIC uses supervisor RW/NX **UC** device
mappings in the common root and each private root, with private child tables and
QMP leaf walks. Device pages never become allocator-owned RAM. User-root device
tables join that root's original own-only workspace aliases, before publication;
subsequent stack tables retain Phase 4B's common-workspace-only access rule.
The existing 64-table/16-data-owner/64-mapping and two-space/Node/Thread bounds
remain unchanged. Thirteen data owners still suffice for the two fixtures and
four private kernel-stack pages. xAPIC adds three device-table pages per root;
all fit without borrowing sibling hierarchy or increasing the pools.

Pins remain held through queued, running, interrupted and handed-off execution.
They block root/mapping/stack/frame destruction while private contexts remain
resumable. Per-dispatch tests attempt premature stack/model reclamation and
require rejection without changing accounting. Fault/exit first removes saved
and queue ownership on the common stack, then unpins, destroys the private stack,
root/backing and Node/Thread associations. The surviving saved context, private
sentinel and hierarchy remain valid. Stale-generation tests really reuse a
metadata slot; a historical ID never selects the replacement.

After the final Thread ends, require empty FIFO, no Running/current publication,
no pending mailbox or busy guard, and common CR3. Mask/zero the timer and admit
only the bounded common-stack drain; require ISR/IRR empty, ticks equal EOIs and
stable after stopping. Disable APIC software delivery, keep other LVT/PIC
sources masked, release the common xAPIC view and restore exact pre-test frame
and table counts. Native selected stack/user RSP/continuation slots must be zero,
SCE must be restored off, and two QMP observations must show the controlled halt.

## Phase 4C test instrumentation and verification

The embedded CPU-bound loops do not yield or invoke SYSCALL during the required
initial A/B alternations. Each increments its own computation counter and writes
it into private data. A separate entry counter must remain **one**. Three private
user-stack markers and full-width register canaries persist across IRQs.
After four real preemptions per Thread, the common controller uses an owned-frame
view to set a finite **test work limit**; it never advances the computation
counter, edits saved GPRs/RIP or injects an interrupt. This bounds successful
completion. The survivor performs a further long loop, including hardware IRQs
that return without a FIFO switch. Early rejection tests also bound the survivor
rather than waiting for a vanished competitor. Native/fault variants add their
explicit operation only within their stated test scope.

Test instrumentation is bounded to 64 controller dispatches and 256 captured
user IRQ records, with fail-stop overflow. The fixtures finish within those
budgets; this is not evidence of an indefinite production execution service.

The host independently simulates FIFO order and compares every saved/taken full
frame. QMP observes private restore frames, CPL=3 work, and actual IRQ-stack
frames under both roots. It reads TSS.RSP0 and common continuation storage,
walks guards/supervisor stack pages (and UC device pages for xAPIC), checks
canaries/markers/entry and work counters, and observes the in-service LAPIC vector
**before EOI**. Actual hardware frames must match the kernel's copied records.
Post-resumption CPU/data observations and entry-count checks reject restarts.
Serial alternation labels alone cannot satisfy these requirements.

| New QEMU case | Required evidence |
| --- | --- |
| `preempt` | Repeated A/B captures with no cooperative/syscall cause; exact resumption, independent work, no-peer IRQ returns, normal exits and complete cleanup. |
| `preempt-xapic` | Same behavior with x2APIC hidden; actual UC device leaves under both private roots. |
| `preempt-a-fault`, `preempt-b-fault` | Actual #UD after preemptive resumption while the healthy peer remains suspended; reclaim only the offender, resume the survivor. |
| `preempt-cross` | A's actual #PF vector 14/error 4/CR2=`0x406000`; B's private memory and context survive. |
| `preempt-native` | Repeated ABI query/return during CPU work with IF=1 user state, IF=0 kernel entry, timer capture and correct terminal exit. |
| `preempt-invalid-context` | Noncanonical saved RIP rejected before native/root activation; healthy suspended Thread continues. |
| `preempt-stack-mismatch` | Mismatched saved kernel stack rejected before entry. |
| `preempt-unsafe` | IF-clear saved state cannot establish the interruptible user window. |
| `preempt-handoff` | Duplicate publication and wrong sequence rejected while the original valid record remains consumable once. |
| `preempt-stale` | Old ThreadId rejected after real metadata reuse while the peer remains suspended. |
| `preempt-fpu` | Actual #NM vector 7/error 0 on unsupported user FNINIT; sibling completes. |
| `preempt-unexpected` | Controlled Ring 0 INT 0x41 rejected with zero ticks/EOIs; it is not hardware-preemption evidence. |
| `preempt-software` | Controlled INT 0x30 with no ISR bit rejected with zero ticks/EOIs. |
| `preempt-origin` | Genuine LAPIC event in an unsupported enabled Ring 0 loop fails before scheduling or EOI; QMP retains ISR 48 and sees the stopped source. |

The initial implementation run rejected real IRQ RF as though it were a native
return; the context-origin policy above corrects that without broadening native
flags. Observer development also caught a wrong QMP method call, the long-mode
null handler SS expectation, an incorrect fault-log parser and terminal pre-entry
ELF images whose unreachable payloads were correctly removed by the linker.
The focused parser/ELF failures are retained in the local logs; the earlier
RF/QMP development failures were observed during implementation. Only subsequent
complete checked runs count as acceptance evidence. A repeated xAPIC run also reached the 30-second QMP deadline
in the final survivor loop: repeated observation holds and polling prolonged that
loop. Final instrumentation holds only the first IRQ per dispatch and stops QMP
polling once restore/user/IRQ evidence is complete. Every later IRQ still has its
own captured frame, ISR/EOI check and numerical accounting; the deadline stays
30 seconds. The common bridge also explicitly restores normal kernel segment
selectors after the processor's null-SS user IRQ entry. The early-rejection survivor bound and symmetric fault
work limits ensure negative tests still exercise meaningful healthy continuation.

## Phase 4C actual validation results

Executed from the repository root:

```sh
make doctor > build/phase4c-doctor.log 2>&1
make check > build/phase4c-check.log 2>&1
make build > build/phase4c-build.log 2>&1
make test > build/phase4c-test-run.log 2>&1
cargo test --locked --lib --target aarch64-apple-darwin
python3 tools/neu.py test --case preempt
python3 tools/neu.py test --case preempt-xapic
python3 tools/neu.py test --case preempt-native
```

All fifteen focused cases in the table above passed, including actual fault,
entry/stack ownership, acknowledgment and precisely classified negative cases.
The complete final suite then passed all 71 scenarios. A final supplemental
integer-context replay checked **167 actual recorded user IRQ frames**, including
**146 ordinary-loop full GPR invariants** and eleven corrupted-RDX rejection
controls. The same ordinary-loop check is part of the maintained checker; it
checks all fourteen invariant GPRs plus the changing RAX counter, detecting a
wrong restore order even when copied privileged-frame bytes were correct.

| Validation | Final actual result |
| --- | --- |
| Doctor | Pass: pinned toolchain/target, QEMU and hashed OVMF pair. |
| Static checks | Pass: formatting, Python syntax and Clippy with warnings denied across 71 kernel configurations plus the host library. |
| Host | **72 pass**: previous 64 plus eight handoff/interrupt-context/FIFO tests. |
| QEMU | **71 pass**: all previous 56 plus fifteen Phase 4C cases; timeout/reset/triple-fault and expected-failure verification preserved. |
| Final halt | Every case has two QMP HLT=1/CPL=0/IF=0 observations under the original common CR3, then host-controlled quit and QEMU exit 0. |
| Repeated scheduling | **9/9 pass** after correcting observation overhead: three identical-image runs each for `preempt`, `preempt-xapic` and `preempt-native`, under the unchanged 30-second deadline. |
| Clean-directory builds | **5/5 pass** on final source: ordinary, x2APIC preemption, xAPIC preemption, native workload and terminal pre-entry negative; fresh loader cache/download/hash checks and fresh target directory. |
| Source/history | Original 56 case names/features preserved, architecture SHA unchanged, earlier ADR rationale retained with dated extensions. |

The final repeat guest durations were 14.881/15.277/15.171 seconds for x2APIC,
19.822/18.287/18.250 for xAPIC, and 15.988/14.744/14.735 for the native workload.
These include boot, observations and test instrumentation; they are not measured
scheduler quanta or CPU-time shares. Tick totals varied while contexts, progress
and exact cleanup stayed correct.

Final complete-suite `preempt` samples were:

| Dispatch | Thread | CPL=3 sampled RIP | CR3 | Private work counter | Actual interrupted RIP |
| --- | --- | --- | --- | --- | --- |
| 1 | A | `0x40011c` | `0xc000` | 24964 | `0x40011c` |
| 2 | B | `0x4000d3` | `0x20000` | 26057 | `0x4000d3` |
| 3 | A | `0x4000d3` | `0xc000` | 1896823 | `0x40011c` |
| 4 | B | `0x4000d3` | `0x20000` | 1932792 | `0x4000d3` |
| 5 | A | `0x4000d3` | `0xc000` | 3804824 | `0x4000d3` |
| 6 | B | `0x4000d3` | `0x20000` | 3861910 | `0x40011c` |
| 7 | A | `0x4000d3` | `0xc000` | 5781138 | `0x40011c` |
| 8 | B | `0x40011c` | `0x20000` | 5799678 | `0x40011c` |
| 9 | A | `0x4000d3` | `0xc000` | 7714887 | Normal terminal slice |
| 10 | B | `0x4000d3` | `0x20000` | 7725088 | `0x4000d3`, then no-peer IRQ returns |

All samples had user CS/SS=`0x3b/0x33`, IF=1 and an entry count of one. Ordinary
loop RSP and captured RSP were `0x404fe0`. TSS.RSP0 was
`0xffffc00000003000` for A and `0xffffc00000007000` for B, with actual IRQ frames
at top minus 176. Each of the first eight dispatches had a genuine captured
frame, single-use handoff and later exact owned-context restoration. Final work
was **7794691 / 27722637**, both normal exits. There were **15 timer events and
15 EOIs**, including seven no-peer IRQ returns, but only **ten dispatches**.
Final allocation/free/table counts returned to **7 / 118310 / 7**, equal to that
image's baseline. Addresses and work/tick counts are observed values for this
build, not fixed ABI or timing promises. xAPIC walks and alternate roots are
recorded separately in its case JSON.

No failed acceptance criterion remains within this profile. Physical hardware,
hosted CI, arbitrary user-program loading, adversarial entry/flag/stack fuzzing,
nested NMI/#MC recovery, long-running unbounded execution, general kernel
preemption and SMP remain unverified or outside scope. Unsupported IRQ contract
conditions fail-stop; the tested Node-local containment covers the documented
CPU faults and rejected saved-publication cases, not arbitrary kernel corruption.

Local Git-ignored evidence: `build/phase4c-baseline.log`,
`build/phase4c-doctor.log`, `build/phase4c-check.log`, `build/phase4c-build.log`,
`build/phase4c-host-tests.log`, `build/phase4c-focused.log`,
`build/phase4c-repeat.log` (retained initial timeout),
`build/phase4c-repeat-final.log`, `build/phase4c-repeat-results.json`,
`build/phase4c-repro-build-final.log`, `build/phase4c-context-replay.log`,
`build/phase4c-test-run.log`, `build/phase4c-full-results.json`,
`build/test-results.json` and `build/test-logs/<case>.{serial.log,qemu.log,json}`.
The complete and repeat JSON preserve raw QMP CPU/LAPIC/frame/stack observations,
image hashes, distinct event/selection counts, outcomes and exact cleanup.

## Cooperative Thread scheduling and saved contexts

The retained Phase 4B configurations are explicitly cooperative. A two-entry FIFO contains
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

## Retained Phase 4B entry stacks, pins and switching order

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

The following describes the retained Phase 4A `timer-test` profile. Shared
controller support also compiles in `preempt-test`; its user-only handoff is
described above. Ordinary boot and earlier supervisor/Ring 3/Node/native images
retain IF=0. The original timer profile uses no Node or Thread and performs no
context switch.

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
scheduling quantum is absent from this kernel-only timer profile; Phase 4C
uses the separate provisional one-shot described above.

## Interrupt entry, ownership and shutdown

This section describes the retained Phase 4A kernel-only timer windows; the
Phase 4C user-origin capture/handoff contract is documented above.

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
above. These earlier configurations do not schedule from a hardware timer. The
[Phase 4A prerequisite analysis](adr/0011-local-timer-interrupt-foundation.md#constraints-before-phase-4b-context-switching)
remains historical rationale. [ADR 0012](adr/0012-cooperative-thread-context-switching.md#phase-4c-prerequisites)
records the earlier prerequisites now extended by
[ADR 0013](adr/0013-single-cpu-user-preemption.md).

## Internal Node and Thread model

`node::Nodes` has separate two-slot Node and three-slot Thread arrays. `NodeId` and
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
| Node | Created → Configuring → Ready → Active → Exited → Reclaimed; Active persists across member suspension. Construction failure can exit Created/Configuring. |
| Thread | Configuring → Ready → Runnable ↔ Running → Exited → Reclaimed; earlier run-once fixtures go directly from Ready to Running. Phase 4D admits two live members per Node within three total Thread slots; earlier fixtures retain one initial Thread. |
| Normal exit | Ends the bound Thread. Node exits only after its final live associated Thread ends; a model test checks that rule independently of bootstrap policy. |
| User fault | Ends only that Node's associations, copies actual fault metadata, returns to the trusted controller without resuming the Thread. |
| Construction failure | Separate `ConstructionFailure` outcome; owned paging resources roll back before model reclamation. |
| Reclamation | Requires Exited, no active/live/queued/saved context, released kernel stack and released address-space association. Outcomes remain until metadata-slot reuse. |

`ExitReason` separates `Normal(u8)`, `UserFault(Fault)`,
`Cancelled { by: ThreadId }` and `ConstructionFailure`. `Fault` distinguishes actual CPU exceptions from rejected
return state and copies vector, error, RIP, RSP, CS, SS, RFLAGS and CR2. No
user-memory diagnostic structure is dereferenced. Generation exhaustion refuses
reuse rather than wrapping. FIFO policy is implemented separately; this remains
a small internal model without a public object API. The Phase 4D member and
stack-edit transactions are detailed above.

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
- Required flag bit 1 and arithmetic flags, legacy mask `0x8d7`; Phase 4C
  additionally requires IF for its explicitly preemptible native session.
  Unsafe direction, trap, privilege and reserved flags remain rejected.
  Interrupt-origin RF handling is separate, as described above.

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

## Address-space ownership and retained earlier layouts

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

## Phase 4B validation reference

The following commands/results describe the retained Phase 4B milestone at
`77ce538`; current Phase 4C commands/results are recorded above.

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

Kernel and user TLS/GS state, FPU/SIMD context, SMP/per-CPU entry, interrupts outside the explicit timer/user windows,
general kernel preemption, adversarial entry fuzzing, hardware faults and physical machines
remain unsupported or unverified. Fatal paths use no kernel GS-relative/TLS
state; a future per-CPU design must revisit this assumption. There are no CPU budgets
or per-Node CPU-time isolation. An infinite user loop remains eligible indefinitely;
other runnable Threads can still progress, and the test deadline bounds completion.
Speculative/microarchitectural side channels and arbitrary privileged corruption
are outside the demonstrated boundary.

The two-Node implementation remains trusted bootstrap machinery. Node-local
capabilities, public Node/Thread creation or factories/ResourcePools,
general kernel preemption, Channels/IPC, shared MemoryObjects, pointer-copy APIs, full ELF
loading, a userspace runtime, filesystem/network/drivers and POSIX/Linux
compatibility remain unimplemented. Construction/teardown assumes one trusted
controller with no asynchronous observers. A future concurrent object system
must revisit synchronization, references, cancellation and lifetime publication.

## Architecture relationship and recommended next milestone

[ADR 0014](adr/0014-multiple-threads-in-one-node.md) accepts this bounded extension
to [ADR 0013](adr/0013-single-cpu-user-preemption.md). Earlier ADR rationale and
execution profiles are preserved. Architecture v0.2 and all decision-register
statuses remain unchanged. This accepts neither every scheduling proposal nor
public capability/object authority. ABI 0.1 remains unchanged.

The recommended path into **K5** is a separately scoped capability and object
lifecycle foundation: implement Node-local handle tables, typed rights, close and
kernel-validated authorization, with invalid/wrong-type/stale/unauthorized negative
tests. Define NodeFactory/ResourcePool ownership before exposing creation operations. Carry
these root pins, generation checks, cancellation and rollback rules into that
authority model. Record significant new or revised decisions in an ADR. Keep
kernel transactions non-preemptible until synchronization is defined. Wait queues,
priorities/budgets, SMP/per-CPU state, TLS/extended state, IPC and ELF/runtime support
remain distinct work; no future ADR placeholders are generated.

## Primary references

- [Pinned Limine protocol](https://github.com/Limine-Bootloader/limine-protocol/blob/3a0526b700e356f0eac1b71a77697b3fd1c707a3/PROTOCOL.md) and [12.9.4 release](https://github.com/Limine-Bootloader/Limine/releases/tag/v12.9.4).
- [Rust x86-64 freestanding target](https://doc.rust-lang.org/rustc/platform-support/x86_64-unknown-none.html).
- [Intel instruction reference](https://www.intel.com/content/dam/www/public/us/en/documents/manuals/64-ia-32-architectures-software-developer-vol-2b-manual.pdf): SYSCALL/SYSRET state and selector rules.
- [Intel system programming reference](https://cdrdv2-public.intel.com/819714/253668-sdm-vol-3a.pdf): paging, interrupt frames, TSS/IST and privilege boundaries.
- [QEMU 11 PIT frequency](https://github.com/qemu/qemu/blob/v11.0.0/include/hw/timer/i8254.h) and [PIT output semantics](https://github.com/qemu/qemu/blob/v11.0.0/hw/timer/i8254_common.c).
- [QEMU local APIC observation](https://github.com/qemu/qemu/blob/v11.0.0/hmp-commands-info.hx): `info lapic`.

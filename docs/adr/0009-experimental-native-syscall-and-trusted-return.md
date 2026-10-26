# ADR 0009: Establish a scalar native syscall and trusted return boundary

- **Date:** 2026-10-25
- **Status:** Accepted — K2 / Phase 2B, experimental bootstrap-fixture ABI only.
- **Extends:** [0008: isolated execution](0008-isolated-ring-3-fixture-and-test-return.md)
  and [0007: owned paging](0007-kernel-owned-paging-and-controlled-mappings.md).
- **Related NEU decisions:** `NEU-008` (native ABI, initial subset),
  `NEU-017 / EXEC` (bootstrap fixture only), `NEU-021 / SEC` (trusted boundary),
  `NEU-023 / DIAG` (terminal containment), `NEU-024 / TEST` (local tests).
- **Architecture:** [Section 10](../Neu_Kernel_Architecture_v0.2.md#10-native-syscalls-and-abi)
  and [Section 14](../Neu_Kernel_Architecture_v0.2.md#14-node-image-bootstrap-and-execution-abi--neu-exec-001).

## Context

Phase 2A deliberately disables SYSCALL and uses a DPL=3 test interrupt gate. Its
TSS.RSP0 entry stack does not by itself solve native entry: SYSCALL changes
privilege/RIP/selectors and saves return RIP/flags in RCX/R11, while leaving RSP
unchanged. Calling Rust or pushing onto that user-selected stack would break the
privilege boundary. The existing exception path also assumes TLS-free zero
FS/GS state, rather than a per-CPU SWAPGS convention.

We need a small actual native round trip and terminal exit, preserving owned
mapping lifetimes, before general Nodes, Threads, syscalls with pointers or
scheduling. The architecture's register proposal is useful, but its numeric ABI
and complete lifecycle remain unfrozen.

## Decision

Keep the current toolchain, loader, four-level layout, single CPU and IF=0.
Compile native entry and fixtures only in explicit `test-native*` configurations.
Ordinary boot and every Phase 2A interrupt-gate regression remain available.
The native configuration does not enable the DPL=3 completion gate.

Define the experimental **Neu scalar ABI 0.1** centrally in `kernel/src/syscall.rs`:

| Operation/status | Value and meaning |
| --- | --- |
| `abi_query` | Operation 1; RAX=0, RDX contains version/features. |
| `thread_exit` | Operation 2; RDI in 0..255 terminates this isolated fixture. |
| `NEU_NOT_SUPPORTED` | RAX=-1 for unknown operations, RDX=0; return to user. |
| `NEU_INVALID_ARGUMENT` | RAX=-2 for an out-of-range exit value, RDX=0; return. |

Arguments are unsigned 64-bit registers RDI, RSI, RDX, R10, R8, R9. Unused arguments
are ignored. RAX encodes signed status in 64-bit two's complement; RDX is the
scalar output. Other GPRs and RSP are preserved except architectural RCX/R11
clobbers. A returning call leaves RCX/R11 carrying user return RIP/flags, with no
kernel pointers. Version/features use 16-bit major, 16-bit minor and 32-bit
feature fields; 0.1 with query/exit bits gives `0x0000000100000003`. No user-memory
structures or pointer-copy API is introduced. These numbers and encodings remain
experimental until an explicit compatibility decision freezes them.

Check CPU SYSCALL support. Preserve unrelated EFER bits and enable SCE only for
the native session. Configure/read back STAR=`0x0028000800000000`, LSTAR at the
supervisor RX entry, and FMASK=`0x77700` (TF/IF/DF/IOPL/NT/RF/VM/AC). The kernel
selector pair stays 0x08/0x10; STAR's upper base 0x28 corresponds to user SS 0x33
and long-mode CS 0x3b. We use IRETQ, not SYSRETQ. Keep legacy SYSENTER CS zero.
Read back the native MSRs again at dispatch; restore the prior EFER/STAR/LSTAR/
FMASK before releasing the fixture. Native configuration requires SCE initially
disabled, rather than adopting an unknown previous syscall service.

Choose **RIP-relative supervisor entry storage** instead of SWAPGS for this
single-CPU experiment. A non-reentrant guard precedes the saved-RSP store. Copy
user RSP as a scalar, then select the guarded 32 KiB kernel entry stack with LEA.
No push, call, Rust or user-address dereference occurs before that stack switch.
The original suspended kernel continuation remains on its separate normal stack.
Save all GPRs and build a private 160-byte IRET frame before the Rust call.
User CS/SS in this native frame are synthesized from the validated contract;
SYSCALL does not hardware-save the old selectors.

There is no kernel GS/per-CPU pointer, SWAPGS or TLS convention. Clear user
FS/GS bases before Rust and retain zero bases on user return; restore DS/ES to
the user selector. The existing interrupt path remains TLS-free. Native builds
route NMI/#MC to a separate guarded 16 KiB terminal IST2 stack, retaining #DF on
IST1. This avoids dependence on the early user RSP for those critical entries;
it does not prove general nested NMI/#MC recovery. Unexpected native re-entry
selects the emergency stack and fails fatally. IF=0 is not treated as disabling
NMI or synchronous faults.

Validate the sealed root, actual kernel selectors/stack/flags/MSRs, and copied
user return state before dispatch. Returning contexts require canonical lower
RIP/RSP, an approved fixture resume instruction in RX code, an aligned RSP in
the live RW/NX user stack, fixed user selectors and only arithmetic flags plus
required bit 1. Recheck before IRETQ. Reject unsafe states without dereferencing
user pointers or attempting a privileged return. The dispatcher returns scalar
errors for unknown operations and invalid exit arguments.

Successful `thread_exit` never executes the user return path: resume the trusted
kernel continuation, restore MSRs/root, inspect owned result backing, then tear
down the inactive private hierarchy and mapping references before frame reuse.
This is not a generalized Thread lifecycle. User tables/backing stay live until
that terminal path; all bootloader-reclaimable RAM remains reserved.

Require direct QMP observations at CPL=3 before the call, CPL=0 on the actual
native stack, and CPL=3 after query return, plus user-side scalar/canary checks.
Add isolated cases for unknown operations, invalid exit, rejected return states
and disabled SCE. A disabled-SCE #UD is negative evidence of failed entry and
must never count as a successful syscall. Preserve all earlier tests.

## Alternatives considered

- **SWAPGS/per-CPU entry structure now.** Appropriate for a future SMP design,
  but adds base ownership, swap state and nested-entry protocols to the current
  one-CPU TLS-free kernel. RIP-relative storage avoids that extra state here.
- **Rely on TSS.RSP0 or push before switching.** SYSCALL does not select RSP0;
  this would let the user choose privileged stack memory. An actual noncanonical
  RSP test must still reach the trusted native stack before rejection.
- **SYSRETQ optimization.** Offers no necessary benefit for the first proof and
  adds return-state hazards. An explicitly validated IRETQ frame is auditable.
- **Pointer-based ABI query or object dispatcher.** Adds user copying and object
  authority/lifetime work before scalar entry/return is established.
- **Replace `int 0x80` tests.** Would discard useful independent regressions.
  The older path remains scoped to its original configurations.

## Consequences

- Real native entry, scalar errors, repeated query returns and terminal fixture
  exit now have a concrete implementation contract, without accepting complete
  Node/Thread, startup descriptor, capability or ELF specifications.
- Static entry state permits only one active CPU/context. SMP requires a new
  per-CPU ownership and nested-entry design; this is not implicitly SMP-safe.
- Approved resume sites and one fixed user stack intentionally restrict the
  prototype. Arbitrary executable images, TLS and extended CPU state are absent.
- Bounded observation loops are test instrumentation. Missing a required CPU
  sample fails the test; no performance or CPU-time isolation claim is made.
- Critical IST routing and nested-entry fail-stop are defensive mechanisms,
  not verified NMI/machine-check/corruption recovery. Hardware, side channels,
  interrupts/preemption and malicious-context fuzzing need later work.

## History

- **2026-10-25:** Accepted under the explicit Phase 2B request. ADR 0008's
  interrupt-gate rationale and original tests remain intact. Current commands,
  CPU observations, counts and limits are in [implementation status](../IMPLEMENTATION_STATUS.md).
  Architecture v0.2 is unchanged; the numeric ABI is not frozen by this record.

References: [Intel instruction reference, SYSCALL/SYSRET](https://www.intel.com/content/dam/www/public/us/en/documents/manuals/64-ia-32-architectures-software-developer-vol-2b-manual.pdf),
[system programming reference](https://cdrdv2-public.intel.com/819714/253668-sdm-vol-3a.pdf).

### 2026-10-25 — Phase 3A extension

[ADR 0010](0010-independent-bootstrap-nodes-and-fault-containment.md) extends the
singleton lifecycle to two independently owned bootstrap Nodes. The native
session is reset and bound to a trusted Node/Thread/SpaceId/root per sequential
execution. Valid `thread_exit` ends that Thread and ends its Node only when no
live associated Thread remains. Supported user faults record a Node-local
terminal outcome; the trusted kernel can then execute the healthy sibling.
Operation numbers, scalar/register/status/version contract, guarded native entry
and validated IRETQ stay unchanged. This ADR remains Accepted; its original
Phase 2B rationale and fixture regressions are preserved. Capability governance
and scheduling are not supplied by this extension.

### 2026-10-26 — Phase 4B extension

[ADR 0012](0012-cooperative-thread-context-switching.md) adds feature-gated
cooperative execution. Its native entry selects a validated Thread-owned stack;
yield copies user state and abandons the entry call chain through the common
stack before another Thread runs. The entry guard stays held until that handoff.
Earlier configurations keep their original stack and ABI behavior. Query/exit
numbers, results, register conventions and advertised 0.1 features remain
accepted and unchanged; no public yield number is allocated by this extension.

### 2026-10-26 — Phase 4C extension

[ADR 0013](0013-single-cpu-user-preemption.md) admits IF=1 only for explicitly
preemptible user contexts and captures real timer-origin frames through the
common-stack boundary. Native kernel execution and resource/scheduler mutation
remain IF=0. Earlier fixtures retain their original behavior; the accepted ABI,
protection-domain and ownership directions remain unchanged. This extension does
not add general kernel preemption, SMP or public object/scheduler interfaces.


### 2026-10-26 — Phase 5A extension

[ADR 0015](0015-node-local-capabilities-and-typed-objects.md) adds one generation-bound
Node-local table and concrete typed test objects with explicit READ/WRITE grants.
A movable execution lease preserves shared Node authority without borrowing the
controller's suspended mutable model from native handlers. Close invalidates one
handle; pins retain already authorized operations, and terminal Node cleanup
releases remaining references. ABI 0.1 remains unchanged; private capability test
operations are feature-gated. Public creation, transfer, derivation and revocation
remain deferred. The historical scope and rationale above are preserved.

### 2026-10-26 — Phase 5B extension

[ADR 0016](0016-capability-abi-and-live-address-space-inspection.md) adds explicitly
versioned capability ABI 0.2 in a separate configuration. Query=1, exit=2 and
legacy ABI 0.1 behavior remain unchanged in earlier configurations. Private
counter operations remain test-only; they are not promoted to public numbers.
The [0.2 specification](../abi/experimental-v0.2.md) defines discovery, close and
own-AddressSpace inspection. This extends the scoped decision; original rationale
and historical implementation limits above remain intact.

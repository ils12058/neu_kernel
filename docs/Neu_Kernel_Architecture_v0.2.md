# Neu Kernel — Architecture & Design Specification

> **Document revision:** 0.2 (consolidated architecture draft; individual subsystem drafts remain v0.1)  
> **Date:** 2026-10-25  
> **Project:** Neu Kernel  
> **Status:** Design working draft; not frozen, implemented, verified, or production-ready  
> **Primary implementation language:** Rust  
> **Initial target:** x86-64, QEMU/UEFI  
> **Project scope:** **Kernel only**

---

## Document status and conventions

This document consolidates the original Neu Kernel architecture discussion **and the eight additional subsystem specifications supplied for this revision**. It is intended to be a **working specification** for the planned development restart, **not** a report that the described subsystems have been implemented.

Status terms used throughout:

- **Chosen direction** — explicitly favored in the design discussion; still subject to revision through an architectural decision record (ADR).
- **Proposed** — a concrete recommendation needing detailed design, prototyping, or explicit confirmation.
- **Deferred** — intentionally excluded from the first implementation, but retained as a possible future experiment.
- **Out of scope** — not a responsibility of the Neu Kernel project.

All APIs, Rust structures, syscall names, register conventions, diagrams, and numerical requirements below are **illustrative drafts unless explicitly stated otherwise**. No production compatibility or security guarantees are implied.

### Revision 0.2 — integration notes

This revision **retains the original foundational model (§§1–13)** and integrates the later detailed discussions as **specification dossiers (§§14–21)** rather than copying conversational transcripts. The implementation roadmap, risks, decision register, and prospective adoption notes now follow in §§22–27. All added subsystem specifications are still **v0.1 proposals**; the document revision number is not a kernel release or an ABI version.

| Added dossier | Sections | Draft identifier |
| --- | --- | --- |
| Node Image, initial bootstrap and execution ABI | §14 | `NEU-EXEC-001` |
| Physical/virtual memory and MemoryObjects | §15 | `NEU-MEM-001` |
| Synchronization, Events and atomic waits | §16 | `NEU-SYNC-001` |
| Preemptive scheduler, timer architecture and SMP | §17 | `NEU-SCHED-001` |
| Threat model, security limits and invariants | §18 | `NEU-SEC-001` |
| Device authorization, interrupts and DMA | §19 | `NEU-DEV-001` |
| Fault handling, panic and NeuKMon diagnostics | §20 | `NEU-DIAG-001` |
| QEMU integration tests, security negatives and CI | §21 | `NEU-TEST-001` |

**Review convention:** When a summary section and a detailed dossier describe the same mechanism, treat the dossier as elaborating the **proposal**, not as overruling a previously chosen core principle. Open contradictions, ABI encodings, startup layouts and security claims must be resolved and tested before publication as a normative implementation contract.

## 1. Executive summary

**Neu is the kernel. The operating system ecosystem belongs to whoever chooses to build on top of it.**

Neu Kernel is envisioned as a **Rust-based, capability-oriented hybrid kernel** with a deliberately small, security-sensitive core and selected performance-critical functionality in kernel space. Higher-level services and suitable device drivers may run as isolated **Nodes** in userspace.

Neu is **not** intended to reproduce UNIX or Linux process semantics. Its native abstractions are:

- **Nodes:** isolated execution and authority domains, each capable of hosting multiple Threads.
- **Capabilities:** kernel-enforced, Node-local handles authorizing operations on specific objects.
- **Kernel objects:** typed resources with explicit operations, references, lifetime rules, and resource accounting.
- **Channels:** authorized inter-Node communication, initially using small, bounded messages and later supporting capability transfer and zero-copy memory sharing.
- **Neu-native syscall ABI:** a distinct binary interface, not constrained by POSIX/Linux compatibility.

The kernel should be **portable by design but initially focused on one platform**: x86-64 under QEMU. AArch64 and RISC-V 64 remain future porting targets. The original asymmetric **Manager Core / Worker Core** concept is **deferred** in favor of simple single-core scheduling first and conventional symmetric multiprocessing (SMP) later.

### Design at a glance

| Topic | Direction | Status |
| --- | --- | --- |
| Scope | Standalone kernel project, not a full OS | **Chosen direction** |
| Language | Rust, bare metal | **Chosen direction** |
| Kernel organization | Small-core hybrid, selectively integrated subsystems | **Chosen direction** |
| First ISA/platform | x86-64 under QEMU | **Chosen direction** |
| Bootloader | Reuse an external bootloader; Limine is preferred | External bootloader **chosen**; Limine **proposed** |
| Execution abstraction | Multithreaded **Node** | **Chosen direction** |
| Scheduling unit | Thread; preemptive scheduling | **Proposed baseline** |
| Scheduling topology | Single-core first, symmetric SMP next | **Chosen direction** |
| Security | Object-capability authority; no conventional `root`/`sudo` bypass | **Chosen direction** |
| Kernel API | Neu-native, no POSIX requirement | **Chosen direction** |
| IPC | Bounded Channel messages, capability moves; shared-memory zero-copy later | **Proposed** |
| Drivers | Essential low-level drivers in kernel; suitable drivers as isolated Nodes | **Proposed** |
| Debugging | Optional in-kernel **NeuKMon** over serial | **Proposed** |
| Initial RAM target | 256 MiB minimum target, 512 MiB development default | **Proposed, unmeasured** |
| Manager/Worker cores | Asymmetric processing experiment | **Deferred** |
| Linux compatibility layer | Third-party userspace responsibility | **Out of scope** |
| Node Image / loader | ELF64 static first; loader in userspace with bootstrap exception | **Proposed** |
| Memory implementation | Bitmap PMM, 4 KiB pages, per-Node mappings, eager MemoryObjects | **Proposed** |
| Synchronization | Wait queues, Event, Node-local atomic word waiting | **Proposed** |
| Scheduling detail | Equal-priority round robin, 10 ms experimental quantum | **Proposed** |
| Security threat model | Malicious ordinary Nodes; defined TCB, isolation limits | **Proposed** |
| Hardware authority | Capability-governed MMIO/IRQ; no untrusted DMA without isolation | **Proposed** |
| Failure policy | Node containment; fail-stop kernel panic; emergency serial | **Proposed** |
| Verification | Host/unit + QEMU/security/stress + CI exit criteria | **Proposed** |

---

## 2. Mission, boundaries, and philosophy

### 2.1 What Neu will build

Neu Kernel is responsible for the kernel mechanisms necessary to isolate, execute, communicate with, and account for Nodes:

- CPU and architecture initialization; interrupt and exception handling.
- Physical and virtual memory management, page tables, and address-space isolation.
- Node and Thread creation, lifecycle, scheduling, and synchronization.
- Typed kernel objects, references, handle tables, and capability enforcement.
- Authorized IPC, capability transport, and eventually shared-memory mechanisms.
- Timers, essential platform support, and a limited set of kernel-space drivers.
- Security boundaries and kernel-side interfaces needed for independently isolated driver Nodes.
- Minimal test fixtures and diagnostics, including an optional kernel monitor.

### 2.2 What Neu will not build

**Explicitly out of scope:**

- A general-purpose desktop or mobile operating system.
- Desktop UI, compositor, application store, installer, package manager, or full userspace platform.
- A custom bootloader as a prerequisite for kernel progress.
- A complete filesystem, network stack, graphics stack, or general application runtime merely to make a demonstration look like a consumer OS.
- Built-in POSIX, UNIX, or Linux syscall compatibility solely for legacy software.
- A complete device-driver ecosystem for every board, processor, or peripheral.
- A platform-wide application permission UI, account system, or superuser command.

**Exception:** Tiny userspace test Nodes, an initial bootstrap Supervisor fixture, and minimal object/runtime wrappers are permitted **only** to validate kernel mechanisms. These test fixtures must not silently turn into a full OS project.

### 2.3 Design principles

1. **Kernel only.** Features must earn a place inside the kernel's responsibility boundary.
2. **Capability-first.** Every protected object operation is authorized by an appropriate capability, not by ambient user identity.
3. **Isolation by default.** Separate Nodes do not automatically share address spaces or authority.
4. **Multithreading without authority fragmentation.** A Thread is a scheduling unit; the Node is the protection domain.
5. **Mechanism separate from policy.** The kernel enforces rules; higher-level service orchestration belongs outside it.
6. **Native-first, compatibility-neutral.** Neu's ABI should be coherent in its own right. Third parties may implement compatibility layers.
7. **Portability without premature ports.** Keep CPU-specific details behind explicit architecture/platform interfaces, but develop one backend at a time.
8. **Bounded resources.** Creation authority must not imply unlimited allocation.
9. **Fault containment where feasible.** A failing userspace Node should not, by itself, take down an otherwise healthy kernel.
10. **Measured simplicity.** Minimize unnecessary scope, not necessary correctness, performance, or security.

---

## 3. High-level architecture

The following is a conceptual boundary map, **not an assertion that all components exist**.

```text
                  OPTIONAL THIRD-PARTY USERSPACE
    +---------------------------------------------------+
    | Application Nodes | Driver Nodes | Service Nodes  |
    |   Threads, address spaces, Node-local caps        |
    +--------------------------+------------------------+
                               |
                  Neu native syscalls / Channels
                               |
    +--------------------------v------------------------+
    |               NEU HYBRID KERNEL                  |
    |  Node/Thread management  | Capability enforcement |
    |  Scheduler               | Kernel object system   |
    |  Virtual/physical memory | Channels / IPC          |
    |  Interrupts / timers     | Core platform drivers   |
    |  Resource accounting     | Optional NeuKMon       |
    +--------------------------+------------------------+
                               |
          Architecture and platform implementations
                               |
                  CPU / Memory / Devices
```

**Hybrid** describes where code executes and where functionality is integrated; **capability-based** describes how protected operations are authorized. These are complementary, not synonymous. Anything executing in privileged kernel space remains part of the trusted computing base, even if Neu uses capabilities at the userspace boundary.

### 3.1 Kernel-space responsibilities

The initial kernel core should include the execution, memory, interrupt, scheduling, capability, and IPC mechanisms that must participate in trusted resource management. Selected performance-critical device or platform mechanisms may be integrated into Ring 0 when justified.

### 3.2 Userspace responsibilities

Potential userspace Nodes may provide services and device-specific logic. No mandatory application composition model is imposed: an application may be one multithreaded Node or a group of collaborating Nodes. The Neu project itself only supplies the *kernel primitives* to support those choices.

### 3.3 Trusted boundary

Kernel-space code, memory-management mechanisms, IRQ handling, and IOMMU programming require special care. A userspace driver being isolated by page tables does **not** by itself prevent erroneous hardware DMA or device disruption.

---

## 4. Hardware and portability

### 4.1 Architecture policy

**Chosen direction:** *Multi-architecture by design, single-architecture during initial development.*

| ISA | Role | Notes |
| --- | --- | --- |
| **x86-64** | Primary | QEMU first; physical PCs later and separately validated. |
| **AArch64 / ARM64** | Future port | Requires ARM MMU, exception/interrupt, timer, and platform backends. |
| **RISC-V 64** | Optional research port | Requires privileged-ISA, paging, interrupt, and timer support. |

Neu should separate architecture-neutral abstractions (Node, capability logic, most object semantics, scheduler policy) from architecture-dependent mechanisms (context switching, syscall entry, page tables, interrupt routing, timer, atomics where necessary).

**CPU architecture support is not device support.** Working on generic AArch64 hardware does not imply support for any particular mobile SoC, Apple Silicon device, or Raspberry Pi board.

### 4.2 Proposed x86-64 machine profiles

These figures are **targets**, not benchmarked or proven requirements.

| Resource | Provisional minimum / H1 target | Development default |
| --- | --- | --- |
| ISA | x86-64 | x86-64 |
| Logical processors | 1 | 4 vCPUs (but initially boot/test one) |
| Guest RAM | **256 MiB** | **512 MiB** |
| Firmware | UEFI | QEMU + OVMF |
| Boot protocol | Compatible external loader | Limine (preferred) |
| Paging | 4 KiB pages, 4-level paging | Same |
| CPU baseline | Long Mode, NX/XD, required atomic/interrupt features | Same |
| Interrupt/timer | Required core mechanisms | APIC/timer virtual hardware |
| Graphics | Not required | Not required |
| Storage/network | No post-boot dependency required initially | Optional test devices |
| Console | Serial debugging interface for development | Emulated COM1 / QEMU stdio |

**Test profiles:** 128 MiB (low-memory experiment), 256 MiB (provisional minimum), 512 MiB (default), and 1 GiB (headroom). No minimum should be advertised as verified until the same feature set passes boot and stress testing within that configuration.

The CPU feature set should be checked at boot. AVX, AVX2, AVX-512, and other nonessential optimizations should **not** be mandatory for a basic boot; if used later, related register state must be correctly saved/restored per Thread.

### 4.3 QEMU baseline

Proposed *partial* QEMU options (not a complete boot invocation):

```sh
-machine q35 \
-cpu max \
-smp 4 \
-m 512M \
-serial stdio
```

The first development milestone should still run deterministically with **one vCPU**. Using four vCPUs in the reference machine is a later SMP testing convenience, not a requirement for initial functionality. QEMU x86-64 emulation is suitable even on an ARM-based development host, albeit slower than native execution.

### 4.4 Bootloader boundary

The existing experimental **NeuBoot** work may be retained as archived prior work; **the restart should not be blocked by developing or maintaining a custom bootloader**. Limine is the preferred candidate, but its adoption and exact protocol version should be recorded when implemented.

Boot protocol parsing belongs in a narrow adapter. The kernel should transform bootloader-specific inputs into internally validated boot information (memory map, CPU/platform information, initial mappings, modules, etc.) without spreading bootloader assumptions across core objects.

---

## 5. Node model

### 5.1 Definition

> **A Node is Neu Kernel's fundamental, capability-governed protection and execution domain, with its own isolated virtual address space, a shared capability space, and one or more independently schedulable Threads.** Nodes may operate independently or compose larger applications through authorized communication and resource sharing.

A **Node Image** is the executable representation; a **Node** is its live kernel-managed instance. ELF may be reused as an initial binary container without adopting POSIX semantics.

### 5.2 Node vs. Thread

| Node | Thread |
| --- | --- |
| Protection and authority boundary | CPU scheduling/execution unit |
| Owns or refers to an address space | Own register context and per-Thread stacks |
| Holds a Node-local capability table | Uses its Node's capability table |
| Contains one or more Threads | Belongs to exactly one Node |
| Has Node lifecycle | Has scheduling and execution states |
| Not independently scheduled | Scheduled by the kernel |

**Chosen direction:** Nodes support **multiple Threads**. All Threads in one Node share its address and capability spaces. For the initial prototype, creating one Thread per Node is a deliberate **implementation staging choice**, not a permanent design limit.

Different Threads inside one Node are **not separate security principals**. If two components need different authority or isolation, they should be separate Nodes.

### 5.3 Node lifecycle

Proposed principal states:

```text
CREATED --> CONFIGURING --> READY --> ACTIVE --> EXITED --> RECLAIMED
              |             |        |
              +-------------+--------+--> failure / termination --> EXITED
```

- **CREATED:** Node structure and resource accounting established; no execution yet.
- **CONFIGURING:** Authorized code installs mappings, stack, entry context, initial capabilities, and Threads.
- **READY:** The configuration is valid; the first Thread may be started.
- **ACTIVE:** The Node has started; its Threads can independently be runnable, running, blocked, or suspended.
- **EXITED:** No active Threads remain; an exit result may remain inspectable.
- **RECLAIMED:** Remaining references/resources have safely released their ownership; the Node structure can be freed.
- **TERMINATING:** A *transient teardown condition*, not necessarily a durable public lifecycle state.

An individual Thread exiting does not necessarily terminate its Node. The Node normally exits when its last Thread finishes, or when an authorized Node-wide termination is effected.

### 5.4 Node creation and ownership

**Proposed:** Explicit construction, **no mandatory `fork()`** and **no implicit authority inheritance**.

A creator presents valid `NodeFactory` and `ResourcePool` capabilities. The kernel authorizes creation, allocates an initially non-running Node, and gives the configurator only defined setup/control capabilities. Entry points, memory, initial handles, and Thread context must be explicitly configured.

The creator is **not necessarily the permanent owner** of the new Node. Nodes can outlive their creators if lifecycle and resource policies permit. Resource charges must remain accountable, including for abandoned objects.

A minimal initial Supervisor fixture receives explicitly bootstrapped authority and may delegate it. It is **not** a `root` identity with magical permission bypass. The full supervisor policy/application is outside the kernel-only scope.

### 5.5 Fault behavior

A userspace exception should be classified as recoverable or unrecoverable. On an unrecoverable Node-local fault, the proposed default is to terminate the offending Node (or report to an authorized fault endpoint) rather than crash the kernel. Recovery/restart policy belongs to userspace. A Ring 0 defect or corrupted kernel state is **not** guaranteed to be contained.

For the proposed ELF image, startup state and first launch authority protocol, see **§14 — Node Image, Bootstrap and Execution ABI**.

### 5.6 Initial Node milestone

- Create two Node objects with separate address spaces.
- Start one Ring 3 Thread in each.
- Schedule both Threads.
- Reject an unauthorized cross-Node access.
- Show that a user-mode fault in Node A does not automatically stop healthy Node B.
- Expand to multiple Threads per Node **after** basic scheduler/memory correctness is established.

---

## 6. Capability security model

### 6.1 Core rule

> **Authority is explicit: a Node may access a protected kernel object only through a valid capability granting the required rights.**

Neu does not have a conventional kernel-level `root` account, `sudo`, or an identity-based universal override. Powerful authority still exists, especially during boot, but it is represented by **explicit, constrained capabilities**.

### 6.2 Internal representation

A capability conceptually comprises:

1. A reference to a typed kernel object.
2. A set of object-specific rights.
3. Kernel-managed metadata needed for ownership, delegation, and eventual revocation.

Userspace receives an **opaque handle**, not a raw kernel pointer. Handle lookup is scoped to the calling **Node**. The same numeric handle in two Nodes does not grant the same authority merely because the numbers match.

A candidate implementation uses slot + generation validation to help reject stale handles; generation reuse/wraparound must also be designed safely. Handle secrecy is not the protection boundary: kernel validation is.

### 6.3 Object-specific rights (draft names)

| Object | Illustrative rights |
| --- | --- |
| `MemoryObject` | `READ`, `WRITE`, `MAP`, `EXECUTE` |
| `AddressSpace` | `MAP`, `UNMAP`, `INSPECT` |
| `ChannelEndpoint` | `SEND`, `RECEIVE`, `SEND_CAPS` |
| `Node` | `START`, `STOP`, `CONFIGURE`, `INSPECT` |
| `Thread` | `START`, `SUSPEND`, `CONFIGURE`, `INSPECT` |
| `NodeFactory` | `CREATE` |
| `ResourcePool` | `ALLOCATE` |
| Device-related objects | Explicit MMIO, interrupt, DMA or device-specific rights |
| Authority-management rights | `TRANSFER`, `DERIVE`, `REVOKE` (precise model TBD) |

No global, universal `ADMIN` permission is assumed. Multi-object operations (e.g., mapping memory) must validate **all** relevant capabilities.

### 6.4 Delegation without amplification

A derived capability must not contain rights absent from the source:

`rights(derived) ⊆ rights(source)`

Proposed operations:

- **Close:** invalidate one Node-local handle, releasing that reference.
- **Move:** transfer a capability to another authorized recipient, removing the sender's handle.
- **Derive:** make a further capability with equal or **reduced** rights, subject to delegation rights.
- **Revoke:** invalidate a defined derivation subtree according to future precisely specified semantics.

**Important sequencing:** Initial IPC transfers may support **move-only** behavior. *Attenuation* with `DERIVE` is a later phase and should not be represented as already functional. A move-only transfer does not itself narrow rights.

### 6.5 Transfer and concurrency

Cross-Node transfers should go through authorized **Channel** mechanisms, rather than arbitrary writes into another Node's capability table. A transfer must be **atomic** with respect to handle ownership and message enqueue: on failure, no half-delivered authority and no unintended loss of the source handle. Queued messages can temporarily hold kernel-managed transferred references.

Because Nodes are multithreaded, capability operations need well-defined concurrency semantics. The intended model is **linearizable** handle operations: a successful lookup can safely pin its target object while another Thread closes the handle, but new lookups after the effective close must fail. Revocation is stricter and requires distinct semantics.

### 6.6 Revocation and mappings

**Closing ≠ revoking.** Closing one handle does not necessarily invalidate other handles or existing memory mappings.

Future derivation-aware revocation must account for:

- Capability descendants and independent grants.
- Outstanding kernel references and queued transfers.
- Authorized page mappings that outlive the original handle.
- TLB invalidation/shootdown across affected CPUs.
- Concurrent operations and mapping teardown.

Revocation cannot undo data already observed or copied by a recipient. Because this topic is difficult, design hooks early but **defer full revocation implementation** until the memory and handle systems are robust.

### 6.7 Bootstrap authority

The kernel creates the initial objects/capabilities and assigns a carefully selected bootstrap set to a minimal initial Node. That Node can configure or delegate authority for additional Nodes, without any separate global superuser bypass. The initial authority should be auditable and reduced/delegated as practical.

### 6.8 Minimum security tests

- Authorized capability operation succeeds.
- Wrong object type or insufficient rights returns an error.
- A guessed handle from another Node has no authority.
- Closing a handle prevents subsequent new lookups using it.
- Concurrent close/use is memory-safe and follows specified ordering.
- Transferring a handle commits completely or not at all.

---

## 7. Kernel object model

### 7.1 Basic distinction

**Kernel object:** the managed resource.  
**Capability:** an authorized reference to that resource.  
**Handle:** the Node-local identifier used to present that authority.  
**Internal reference:** kernel-owned lifetime reference, not necessarily exposed to Nodes.  
**Resource charge:** accounting attached to actual resource consumption.

Several capabilities may reference the same object with different rights; closing one handle need not destroy the object.

### 7.2 Foundational object vocabulary

| Object | Role | Notes |
| --- | --- | --- |
| `Node` | Protection/lifecycle domain | Hosts multiple Threads |
| `Thread` | Schedulable context | Belongs to one Node |
| `AddressSpace` | Virtual mappings | Usually one per Node initially |
| `MemoryObject` | Backing storage/pages | Shareable by authorized mappings |
| `ChannelEndpoint` | IPC endpoint | Endpoint-specific rights |
| `NodeFactory` | Node creation authority | Complements resource budgets |
| `ResourcePool` | Bounded allocations/accounting | May eventually be hierarchical |
| `Event`, `Timer` | Waiting/notifications | Later |
| `Interrupt`, `DeviceResource`, DMA-related objects | Mediated device access | Later |

These names are **categories, not a required object inheritance hierarchy**. A capability table can remain a private kernel data structure rather than a publicly creatable object.

### 7.3 Lifetimes and reference management

An object's lifetime may be extended by handles, mappings, queued messages, active operations, and kernel-internal references. Reference counting may be a useful starting mechanism, but cycles and teardown order require explicit control. No tracing kernel GC is assumed.

For example: a Node closes its `MemoryObject` handle, yet a mapping still refers to the object; the object must not be freed until that mapping and all other references disappear.

### 7.4 Resource accounting

**Creation right ≠ unlimited budget.** Generalized Node creation should check both a `NodeFactory` and a `ResourcePool` (or equivalent allocation authority). Pool accounting must include physical memory, kernel metadata, Threads, and queued IPC buffers as appropriate.

An allocation charge persists until the resource can actually be reclaimed; merely moving or closing a handle must not enable quota evasion. Hierarchical pools and cross-pool transfers are **future design questions**.

### 7.5 Node/object interaction sequence

```text
Node A holds NodeFactory + ResourcePool capabilities
    -> kernel validates creation & budget
    -> creates Node B + AddressSpace
    -> authorized configurator maps MemoryObjects
    -> installs initial capabilities + Thread context
    -> starts Node B
    -> Node A/B communicate through ChannelEndpoints
    -> Node B exits or is terminated
    -> kernel cleans up references and resource charges
```

---

## 8. Memory architecture and zero-copy IPC

### 8.1 Separate address space from backing memory

- `AddressSpace` represents a Node's virtual mappings and access permissions.
- `MemoryObject` represents the underlying memory backing, potentially shared by more than one Node.
- Mapping requires the necessary rights on both the destination address space and the memory object.
- Writable-and-executable mappings should be **denied by default**; exceptions, if any, need a narrowly defined policy.

### 8.2 Two IPC payload paths

**Small messages:** Kernel-managed bounded Channel payloads, copied/validated across the syscall boundary.  
**Large payloads:** An authorized `MemoryObject` capability is sent to a recipient, which maps the same physical pages in its own address space.

```text
Node A / AddressSpace A             Node B / AddressSpace B
      READ | WRITE                       READ ONLY
            \                               /
             \     same physical pages     /
              +------ MemoryObject -------+
```

This is **zero-copy at the payload-transfer level**, not zero-cost communication. Mapping changes, cache coherence, synchronization, IPI/TLB work, and object accounting still have costs.

**Moving a handle is not automatically exclusive ownership.** Existing duplicate capabilities or page mappings may retain authority. A true exclusive handoff or borrowed-memory scheme would need additional guarantees and is **deferred research**.

### 8.3 Revocation implications

A future strong memory revocation operation must track authorization provenance for mappings, remove or downgrade relevant mappings, and synchronize TLB state across CPUs. Removing only a Node-local capability handle is insufficient.

For physical allocator, x86-64 memory layout, shared-object alias permissions, TLB synchronization and low-memory behavior, see **§15 — Detailed Physical and Virtual Memory**.

### 8.4 Memory correctness properties

- Page-map changes are validated against source rights.
- User pointers are untrusted.
- Reference lifetimes prevent use-after-free.
- Physical frames are not exposed between unrelated Nodes without explicit authorization.
- User-mode faults do not automatically escalate into kernel compromise.

---

## 9. Inter-Node communication

### 9.1 Channels

**Proposed:** Point-to-point, bidirectional channels, each consisting of two endpoints. Each direction has a bounded message queue; messages are FIFO **per direction**. Endpoint rights control sending, receiving, and capability transport.

For the initial implementation, a Channel endpoint is exclusively held by **one Node at a time**, but any Thread in that Node may invoke operations using it. The precise rules for shared or duplicated endpoint rights can be revisited later.

### 9.2 Proposed semantics

| Property | Draft rule |
| --- | --- |
| Message size | Bounded inline payload |
| Queue memory | Finite and resource-accounted |
| Empty receive | Nonblocking status or explicit wait |
| Full send | Backpressure and defined blocking/nonblocking behavior |
| Ordering | FIFO per channel direction |
| Peer closure | Explicit distinguishable result/notification |
| Capability transfer | Authorized atomic move, added after basic messages |
| Shared memory | Capability transport + separate mapping operation |

Synchronous syscall *entry* can coexist with operations that block the calling Thread. Fully asynchronous completions are not a requirement for the initial ABI.

### 9.3 Composition is optional

One Node can contain all Threads for a workload. A more isolated design can split work among Nodes connected by Channels. The kernel offers mechanisms but does not require multi-Node application construction.

---

## 10. Native syscalls and ABI

### 10.1 Principles

A **syscall** is a request from a userspace Thread to the kernel; the **ABI** defines the binary contract for entry, registers, pointers, arguments, results, object handles, and compatibility.

**Chosen direction:** Neu has its **own** syscall semantics. No `fork`, `execve`, file-descriptor or POSIX compatibility requirement is imposed on the kernel. Reusing ordinary hardware calling patterns, ELF images, or `#[repr(C)]` layouts does **not** mean adopting UNIX/POSIX.

**Proposed style:** Explicit, typed, capability-oriented operations initially, rather than one universal `object_invoke` interface.

### 10.2 Proposed operation groups

| Group | Illustrative names |
| --- | --- |
| Information | `abi_query`, `object_get_info` |
| Capability | `cap_close`, `cap_derive`, `cap_revoke` |
| Node | `node_create`, `node_configure`, `node_start`, `node_terminate` |
| Thread | `thread_create`, `thread_start`, `thread_exit`, `thread_yield` |
| Memory | `memory_create`, `memory_map`, `memory_unmap` |
| IPC | `channel_create`, `channel_send`, `channel_receive` |
| Synchronization | `wait`, `wake` (later) |

`cap_move` should be modeled primarily through authorized Channel transfer rather than unrestricted access to another Node's handle table. All names and numbers above remain **unassigned draft concepts**.

### 10.3 Proposed x86-64 syscall convention

x86-64 `SYSCALL` is the initial mechanism under consideration. The CPU uses the configured syscall entry MSRs and saves the return instruction pointer/flags in `RCX`/`R11`. A vital detail is that **SYSCALL does not automatically select a safe kernel stack**; Neu must implement that transition securely.

| Register | Proposed use |
| --- | --- |
| `RAX` | Operation identifier on entry; status on return |
| `RDI` | Argument 1 |
| `RSI` | Argument 2 |
| `RDX` | Argument 3 on entry; optional scalar value on return |
| `R10` | Argument 4 |
| `R8` | Argument 5 |
| `R9` | Argument 6 |
| `RCX`, `R11` | Architecturally clobbered by `SYSCALL` |

**Proposed return contract:** `RAX = 0` for success or a defined negative Neu error; `RDX` may contain a scalar/handle result on success. This convention requires explicit caller/callee clobber definitions before implementation. It has **not** been finalized.

Example **concept only**, with a placeholder number:

```asm
; x86-64 Neu userspace — not an assigned ABI number
mov rax, 0x01       ; hypothetical cap_close
mov rdi, 0x1001     ; hypothetical handle
syscall
; inspect RAX for Neu status
```

The entry handler must save user context, establish kernel stack and safe privilege state, validate the operation, handle interruption/preemption, and return correctly. Initially, a carefully validated `IRETQ` path may be simpler to audit than early `SYSRETQ` optimization.

### 10.4 User/kernel structure ABI

For complex operations, register arguments may include an address/length for a fixed-layout descriptor. For instance:

```rust
// Illustrative ABI layout; NOT a finalized structure or Rust API.
#[repr(C)]
struct NeuNodeCreateArgs {
    size: u32,
    flags: u32,
    factory: u64,
    resource_pool: u64,
    reserved: u64,
}
```

The ABI must eventually specify field offsets, alignment, width, endianness, extensibility, and invalid/reserved values. **Do not put Rust-owned types (`String`, `Vec`, references, `usize` whose width is assumed) into stable cross-boundary layouts.** `#[repr(C)]` indicates layout convention; it does not create POSIX compatibility.

The kernel must implement fault-aware, checked user-memory access (`copy_from_user`/`copy_to_user` or equivalent). The caller's other Threads can change memory concurrently, so validate kernel-owned snapshots rather than trust a previously checked mutable buffer.

### 10.5 Status codes and compatibility

Proposed error categories include `BAD_HANDLE`, `WRONG_TYPE`, `ACCESS_DENIED`, `INVALID_ARGUMENT`, `NO_RESOURCES`, `WOULD_BLOCK`, and `NOT_SUPPORTED`. Numeric values must be allocated once the ABI is frozen.

Kernel implementation version and **Neu ABI version** are independent. Keep an unstable experimental period, avoid recycling syscall numbers after publication, provide feature discovery (`abi_query`), and maintain written behavioral contracts.

Long term, a machine-readable syscall definition could generate constants, wrapper declarations, dispatch tables, and conformance tests. This is useful infrastructure, **not** a demand to build a full SDK.

### 10.6 ISA portability

AArch64 will use its own supervisor-call mechanism; RISC-V its environment-call mechanism. A successful port should preserve **Neu operation semantics**, not the same CPU registers or instruction encoding. Native Node binaries still need architecture-specific machine code.

### 10.7 First ABI test

Boot a single minimal Ring 3 test Node and perform `abi_query` through the kernel boundary. Validate register preservation, safe stack switching, correct result, and rejection of invalid operation numbers. Only then add a typed handle operation and eventually Channel IPC.

---

## 11. Scheduling, SMP, and deferred asymmetric cores

This section captures high-level choices. For round-robin quantum, timer/clocksource separation, safe preemption and future SMP details, see **§17**. For wait queues, Events and word waits, see **§16**.

### 11.1 Immediate scheduling sequence

1. One CPU: simple preemptive round-robin Thread scheduling.
2. More than one CPU: symmetric multiprocessor scheduling after AP startup, IRQ, synchronization, and memory infrastructure work.
3. Performance improvements: per-CPU runnable queues, affinity, load balancing, work stealing when justified.
4. Research only: asymmetric Manager Core/Worker Core scheduling.

A bootstrap processor may bring up secondary CPUs without permanently becoming a scheduling manager. Neu **schedules Threads**, including multiple Threads from the same Node on separate cores.

### 11.2 Manager/Worker concept — deferred, not deleted

The earlier idea assigns one or more **Manager Cores** for coordination and **Worker Cores** for general execution. It may offer research opportunities, but it can create coordination bottlenecks, underutilized cores, and complex interrupt/scheduling semantics.

**Decision:** Do not hard-code core roles into Node, Thread, CPU, or scheduler data structures. Preserve modular CPU topology and scheduling-policy interfaces so the idea can be benchmarked later against ordinary SMP.

### 11.3 First SMP safety concerns

Per-CPU kernel stacks, context switching, locks, interrupt routing, shootdowns, memory barriers, and scheduler run-queue concurrency must be specified and tested before promising multicore safety.

---

## 12. Driver placement and fault isolation

This section captures the hybrid boundary; the proposed device discovery, hardware-capability and interrupt/DMA contracts appear in **§19**. Threat assumptions and exceptions are in **§18**.

### 12.1 Hybrid driver model

**Proposed split:**

| Kernel-resident mechanisms | Potential userspace driver Nodes |
| --- | --- |
| CPU/interrupt-controller handling | Network adapters |
| Core timer and scheduling support | Audio |
| Early serial debugging | Many storage devices |
| IOMMU control where available | Many USB/device-class drivers |
| Essential low-level platform interfaces | Other drivers that do not require kernel residence |

This table is **illustrative, not an exhaustive rule**. Driver location should be justified by isolation, performance, platform requirements, and implementation cost.

### 12.2 What isolation provides

If a userspace network-driver Node accesses invalid memory, Neu may terminate that Node and leave the kernel and unrelated Nodes operational. The device/service may still be unavailable until an external supervisor recovers or restarts the driver. Restart policy belongs in userspace.

### 12.3 What isolation does not automatically provide

A faulty driver can still hang hardware, flood interrupts, corrupt a device, or misuse DMA. **Page-table isolation is not sufficient for unrestricted DMA-capable devices.** Device assignment should require explicit MMIO/IRQ/DMA capabilities; suitable platforms may need a correctly configured **IOMMU** for DMA containment. Kernel-resident driver bugs continue to endanger the whole system.

### 12.4 Project boundary

Neu may implement **driver-support mechanisms** (interrupt delivery, device resource capabilities, DMA isolation primitives), but complete hardware driver collections, service restart managers, and production device orchestration are **not** part of the kernel-only promise.

---

## 13. Neu Kernel Monitor (NeuKMon)

For the separate emergency panic path, structured fault records, exception stacks and QEMU/GDB diagnostics, see **§20**.

### 13.1 What it is

**NeuKMon** is a proposed **optional, in-kernel diagnostic monitor**, not a Node, shell, or UNIX single-user mode. It can inspect the kernel directly and does not require the Neu syscall ABI to exist first.

### 13.2 Modes

| Mode | Meaning | Status |
| --- | --- | --- |
| `off` | No interactive monitor; normal kernel initialization | Proposed |
| `boot` | Enter serial monitor during kernel boot; `continue` resumes | Recommended for development |
| `panic` | Attempt diagnostic entry after panic when safe | Deferred / best effort only |

Development builds may enter `boot` mode by default, while non-development builds should omit or disable the privileged interactive monitor. It should be possible to control the initial mode from validated boot configuration.

### 13.3 First commands

`help`, `info`, `cpu`, `memory`, `continue`; later `nodes`, `threads`, `caps`, `objects`. Begin with read-only inspection and avoid arbitrary memory writes or uncontrolled object modification. Interactive monitor behavior under active SMP needs special deadlock and stop-the-world consideration.

Example **hypothetical**, not current output:

```text
NEU Kernel [x86_64]  — development build
NeuKMon: boot
neu-kmon> info
Kernel: Neu
Mode: kernel diagnostic monitor
neu-kmon> memory
Physical allocator: initialized
neu-kmon> continue
[NEU] Boot continuing...
```

QEMU's serial interface is sufficient for early monitor I/O; no graphics, filesystem, or user shell is necessary.

---

## 14. Node Image, bootstrap, and execution ABI — `NEU-EXEC-001`

**Status:** Proposed detailed design; binary layouts and launch semantics are **not frozen**. This section expands the Node and syscall concepts in §§5 and 10. Its scope is *loading and executing a minimal Node*, not designing a general-purpose userspace platform.

### 14.1 Three separate contracts

| Contract | Purpose | Initial proposal |
| --- | --- | --- |
| **Node Image** | Encodes executable code and initial data | Static ELF64 for x86-64 |
| **Node Entry ABI** | Defines the CPU and startup state of the first Thread | Explicit register, stack, and `NeuStartInfo` contract |
| **Neu Syscall ABI** | Defines how running Threads invoke kernel mechanisms | Native `SYSCALL` operations described in §10 |

A **Node Image** is not automatically a kernel object. A **Node** is the live protection and authority domain, and a **Thread** is the schedulable execution context. Several Nodes may load the same image with different independent capabilities. Reusing ELF or the System V function calling convention does not imply POSIX or Linux compatibility.

### 14.2 Loader boundary and image profile

**Proposed:** Ordinary image interpretation and loading are performed by an **authorized userspace loader**, which requests Node creation, MemoryObject allocation/mapping, Thread configuration, initial capability provisioning, and start operations. The kernel validates authority, object lifetimes, mapping permissions, and execution-state safety; it need not parse arbitrary file formats, implement dynamic linking, or dictate package policies.

The *first* bootstrap Node is a narrow exception: the kernel needs a minimal trusted mechanism to enter userspace before any userspace loader exists. A statically embedded test image is the simplest first step; a Limine-supplied boot module is a later alternative. Code from the image must execute at user privilege, not as arbitrary privileged kernel functions.

**Initial Node Image profile (proposed):**

| Property | v0.1 profile |
| --- | --- |
| Binary format | ELF64, little-endian, x86-64 |
| Image | Statically linked `ET_EXEC` |
| Relevant load segments | `PT_LOAD` |
| Program entry | ELF `e_entry`, validated as an executable userspace address |
| Read-only code/data | Read/execute or read-only mappings as appropriate |
| Writable data and stack | Read/write, non-executable |
| Guard pages | Unmapped |
| `.bss` / segment tail | Zero-filled when memory size exceeds file size |
| Dynamic linker, PIE/`ET_DYN`, relocations, TLS | Deferred for ordinary images |

A loader must reject malformed ELF metadata: segment ranges extending beyond the image, address/size arithmetic overflow, invalid alignment, contradictory overlaps, invalid entry points, or unsupported constructs. The kernel independently rejects unauthorized mappings and forbidden W+X aliases. A trusted bootstrap parser must be deliberately limited rather than turn into a full general-purpose ELF loader.

### 14.3 Initial x86-64 Thread state

**Candidate Node Entry ABI:** `RIP = validated user entry`, `RSP = prepared user stack`, `RDI = pointer to startup descriptor`, safe user `CS`/`SS` and `RFLAGS`, defined `FS_BASE` (zero in the earliest TLS-free experiment), and initialized general-purpose/extended CPU state without information leaks. Stack alignment and the function-entry convention must be specified exactly. Later Threads receive their own entry address, stack, and argument and continue to share the Node's AddressSpace and CapabilityTable.

```rust
// ABI sketch only. Field widths/layout, lifetime, and reserved values are not frozen.
#[repr(C)]
pub struct NeuStartInfo {
    pub size: u32,
    pub abi_version: u32,
    pub flags: u64,
    pub bootstrap_channel: u64, // invalid sentinel before Channels exist
}

// Conceptual Rust Node entry:
// extern "sysv64" fn _neu_start(info: *const NeuStartInfo) -> !
```

The startup descriptor lives in the new Node's validated userspace memory. The kernel/loader must define whether its memory is read-only, its lifetime, the descriptor-size rules, and the invalid-handle sentinel before promising stable compatibility.

### 14.4 Bootstrap-channel chicken-and-egg problem

Ordinary inter-Node capability transport uses authorized Channels, but a brand-new Node cannot receive its **first** Channel endpoint over a Channel it does not yet hold. A narrowly scoped **launch-configuration capability** should permit an authorized configurator to install an initial endpoint or defined bootstrap capabilities **before the Node starts**. This does not create a general-purpose way to inject arbitrary authority into running Nodes. Once started, further transfers obey the normal Channel protocol.

**Proposed creation sequence:** validate `NodeFactory` + `ResourcePool` → create non-running Node → obtain and populate backing MemoryObjects → map with safe rights → prepare stack and startup descriptor → configure initial Thread → install limited bootstrap authority → validate/seal launch configuration (`READY`) → start (`ACTIVE`) → exchange later capabilities via Channels.

**Invariants:** (a) no Thread enters user mode from an invalid/partial launch state; (b) an image doesn't automatically grant authority; (c) a Node's creator isn't a permanent owner by default; (d) initial provisioning requires specific capability authority; (e) executable mappings do not override the kernel's security policy.

### 14.5 First execution proof

`NEU-EXEC-001`: boot a single Ring 3 fixture, invoke `abi_query`, return correctly across privilege boundaries, invoke `thread_exit(0)`, and record the Node's exit. Validate register preservation, kernel stack transition, bad syscall-number handling, and resource cleanup. A displayed `Hello World`, dynamic linker, filesystem, and network stack are **not** acceptance criteria. Static userspace linker settings must be reviewed separately from the kernel's bare-metal Rust target defaults.

**Outstanding:** exact entry-stack contract; thread-entry ABI beyond the first Thread; ABI struct offsets and version negotiation; bootstrap descriptor lifetime; startup capability installation and rollback; ELF permissions when segments share pages; how bootstrap image bytes are safely accessed.

---

## 15. Detailed physical and virtual memory — `NEU-MEM-001`

**Status:** Proposed detailed design. This expands §8; CPU/page-size/RAM values remain provisional. Initial priority is isolation and correct lifetime behavior, not complex virtual memory features.

### 15.1 Three memory responsibilities

1. **Physical Memory Manager (PMM):** track allocatable RAM frames and their allocation/reclamation; no Node-specific policy.
2. **Virtual Memory Manager (VMM):** create and protect page tables, mappings, and `AddressSpace` objects.
3. **MemoryObject Manager:** represent capability-governed, RAM-backed allocations that may be mapped into multiple address spaces.

**Boot memory lifecycle:** ingest and validate the Limine memory map; copy essential boot information into kernel-owned storage; separate immediately usable, reserved, kernel/module, and reclaimable regions; reserve stacks, page tables and loader-owned data still in use; initialize allocator metadata; reclaim eligible bootloader memory only after all dependencies disappear. Neither installed RAM nor a raw memory-map total equals safe allocatable RAM.

### 15.2 Frame allocator and kernel allocations

**Initial PMM proposal:** 4 KiB frames with a bitmap allocator. At **256 MiB of actually allocatable frames**, one bitmap bit/frame occupies about **8 KiB**, excluding metadata for address holes, ownership state, and reservations. A buddy allocator or other contiguous-allocation mechanism can be investigated when measurements justify it. A separate trusted kernel heap supports kernel objects and metadata; Nodes do **not** automatically receive access to kernel-heap storage or a kernel implementation of `malloc()`.

All newly assigned user-visible frames must be initialized so a Node cannot read residual data from a previous unrelated allocation. Allocation/free bookkeeping must detect invalid frees, double frees, and accounting underflow in tested conditions.

### 15.3 Virtual address layout and page permissions

| Component | Initial target |
| --- | --- |
| x86-64 paging | Four levels, 4 KiB pages, canonical-address validation |
| Kernel region | Higher canonical half, supervisor-only mappings |
| Node region | Lower canonical half, per-Node page tables |
| User stacks | Independently mapped, non-executable, with guard pages |
| NX/XD | Required baseline feature, checked during boot |
| Large pages / ASLR / swap | Deferred (ASLR planned as hardening) |

A common supervisor-only kernel mapping can appear in Node page tables, but this is not a complete speculative-execution defense. Physical and virtual addresses must never be conflated. Two Nodes can use identical virtual addresses for unrelated frames; different virtual addresses can deliberately map the same MemoryObject.

### 15.4 MemoryObject semantics

**Initial MemoryObjects:** anonymous, RAM-backed, fixed-size, page-aligned, **eagerly allocated** and zero-initialized. A MemoryObject's size, frames, rights constraints, mapping references, lifetime state, and resource-account association are kernel-managed. Mapping is a separate authorized operation: validate both `AddressSpace` and `MemoryObject` authority, addresses, offsets, rights, alignment, overlaps, resource budget and overflow.

The mapping operation must not leave incomplete page tables, leaked charges, or partially visible unauthorized mappings on failure. A mapping can outlive a capability handle; **`cap_close` does not silently unmap existing memory**.

### 15.5 Shared-memory / zero-copy rules

Nodes can use different capability rights and virtual addresses to map the same backing frames. This supports large-payload zero-copy transfer **after** authorized capability exchange, though it still incurs mapping, synchronization, accounting and possibly TLB work. Zero-copy is not an immutable snapshot: a sender retaining a writable alias can mutate data another Node reads.

**W^X policy:** prohibit writable-and-executable mappings by default, including **conflicting writable and executable aliases to the same backing pages** (even in different Nodes), subject to a separately defined trusted policy if ever necessary. A security-sensitive revocation of memory requires tracking mapping provenance and coordinating page-table and TLB invalidation; merely closing the originating handle is insufficient. Previously read/copied data cannot be retroactively revoked.

### 15.6 Resource accounting and exhaustion

Charge physical backing pages **once** to their responsible `ResourcePool` while separately accounting for page-table frames, mappings, kernel metadata and capability handles. Transferring a handle does not erase outstanding charges. Duplicate mappings need not double-charge backing RAM. Normal exhaustion should return `NEU_NO_RESOURCES` or a defined error and roll back incomplete work; corruption of essential kernel accounting may be fatal.

### 15.7 Page faults, user pointers and SMP

Initially, **no demand pager**: an unrecoverable userspace protection/unmapped-page fault is recorded and contained within the offending Node, not converted into a general kernel panic. Safe syscall boundary helpers validate ranges/overflow and copy data with recoverable faults; a previous validation is not proof that concurrent userspace Threads haven't modified the buffer. Copy small descriptors into private kernel snapshots.

Local TLB invalidation is necessary from the first mapping implementation. SMP adds cross-CPU shootdowns and delayed frame reclamation: never reuse frames while another CPU might still legally have a stale translation. Concurrent mapping updates, COW if later implemented, and revocation must have explicit ordering.

### 15.8 Scope and tests

**Deferred:** demand paging, copy-on-write, resizing, swap, NUMA allocation, huge-page optimization, compaction, compression, full revocation, broad IOMMU/DMA domains, sophisticated ASLR. **Minimum proof:** two independent Node address spaces; same virtual address maps private distinct frames; unauthorized cross-Node mapping is denied; shared MemoryObject maps with enforced rights; double free, frame reuse, exhausted budgets and overlapping mappings are tested.

---

## 16. Thread synchronization, blocking and waiting — `NEU-SYNC-001`

**Status:** Proposed. This expands the Node/Channel contracts without committing to an in-kernel implementation of every userspace synchronization primitive.

### 16.1 Distinct mechanisms

| Mechanism | Where | Intent |
| --- | --- | --- |
| **Wait queue** | Internal kernel implementation | Atomically block/wake Threads on a kernel condition |
| **Event** | Capability-governed kernel object | Explicit signal/wait notification between authorized participants |
| **Atomic word wait/wake** | Userspace atomics + kernel waiting | Efficient contended locks without a kernel object per Mutex |
| **WaitSet** | Deferred kernel object | Wait on multiple Channels, Events, timers or IRQ sources |

Thread states are `RUNNING`, `RUNNABLE`, `BLOCKED`, and `TERMINATED` for core scheduling purposes; creation/suspension may have additional states. `RUNNING -> BLOCKED -> RUNNABLE` must not lose the signal that made the awaited condition true. **A wakeup grants eligibility to run, not an immediate execution or lock ownership guarantee.** Node lifecycle is independent: an `ACTIVE` Node may have every Thread blocked.

### 16.2 Lost-wakeup safety

For a blocking Channel receive, checking queue emptiness and registering a waiter must be coordinated with message enqueue and notification. Similarly, a transition from `BLOCKED` to `RUNNABLE` must happen exactly once under concurrent wake/timeout/close/termination. State transitions should have defined linearization points. The kernel must not sleep while holding ordinary spinlocks whose ownership would prevent the waker from progressing.

### 16.3 Proposed `Event` object

A first Event may be a **single-bit, auto-reset notification**. It starts unsignaled; a signal sets a pending bit; one waiter consumes that signal, or waits until signaled. Multiple signals while pending are **coalesced**, not counted. Candidate rights are `WAIT`, `SIGNAL`, and `INSPECT`, plus explicit transfer control. Thus an Event is not a semaphore, and it does not spawn its own worker Thread. Creation, outstanding references, closure and cancellation require ordinary kernel-object lifetime rules.

### 16.4 Atomic word waiting

Candidate operations: `wait_word(address, expected_value, deadline)` and `wake_word(address, max_waiters)`. The initial scope is aligned **32-bit**, **Node-local** atomic words. The kernel validates the word, compares with `expected_value`, and atomically coordinates wait registration; if the value differs it returns `NEU_VALUE_CHANGED` (draft name) without sleeping. Userspace lock algorithms use atomic memory ordering and **must re-check the condition after waking**.

The wait identity cannot be a naked virtual address without lifetime handling: reusing an address for a different mapping must not accidentally wake old waiters. Future cross-Node waiting on shared backing should identify a `MemoryObject` + offset and explicitly account for unmap/revoke races. Ordinary Channel messaging and Event signaling are the simpler inter-Node options initially.

### 16.5 Timeouts, cancellation and kernel locking

Use **absolute monotonic deadlines**, including a defined infinite-wait representation. `NEU_OK`, `NEU_TIMED_OUT`, `NEU_CANCELLED`, `NEU_BAD_HANDLE`, `NEU_ACCESS_DENIED`, `NEU_VALUE_CHANGED`, and `NEU_PEER_CLOSED` are *candidate* result categories; applicability depends on the operation. A deadline passing makes a Thread runnable, not guaranteed to execute at that nanosecond.

Node termination cancels any blocked Threads and removes wait references safely. Concurrency among signal, timeout, close, and Node termination requires a single defined completion result. Inside Ring 0 use interrupt-safe spinlocks for short critical sections, sleeping locks only where safe, atomics with correct memory ordering, and a documented lock hierarchy. Interrupt handlers must not block on ordinary sleeping waits. Priority inversion/inheritance is recognized but **deferred**.

**Milestones:** internal wait queues → blocking Channel semantics → Events → Node-local atomic word waits → full monotonic deadlines and cancellation → SMP safe wakeups → optional WaitSets/shared-memory waits. A basic timer is required earlier for preemption even if public deadline APIs come later.

**Tests:** send-before-block/no lost wakeup; competing waiters; signal coalescing; timeout vs. wake; Node killed while waiting; cancellation vs. handle close; under heavy SMP load no double runnable enqueue or dangling waiter reference.

---

## 17. Scheduling, clocks and CPU topology — `NEU-SCHED-001`

**Status:** First algorithm is proposed, while *single-core first and conventional SMP later* is a chosen direction. This expands §11.

### 17.1 Scheduling contract

**Threads, not Nodes**, are scheduled. The initial implementation is one CPU, **preemptive equal-priority round robin**, one runnable FIFO queue, and a **provisional 10 ms time quantum**. All durations and actual fairness must be measured. Repeated tests at 2/5/10/20 ms are useful; neither the quantum nor deadline wakeups constitute hard real-time guarantees. Equal priority per Thread can give a multi-Thread Node greater aggregate CPU time than a single-Thread Node; future CPU budgets may address that policy.

A busy-looping Ring 3 Thread must not prevent another runnable Thread from progressing. Voluntary `thread_yield`, blocking calls, expiration of a time quantum, and termination lead to defined state transitions. Waking from a blocked condition makes a Thread runnable; the scheduler chooses when it executes.

### 17.2 Kernel preemption, interrupt handling and context switches

Initially allow timer-driven preemption **of userspace Threads**; kernel execution is **not generally preemptible**. Interrupts can still be serviced at valid points in kernel execution, and a per-CPU `need_resched` flag can defer switching until safe. This is not equivalent to masking all interrupts throughout kernel entry. Context switches preserve register state, kernel stacks, address-space selection, CPU-local state, and any exposed floating-point/SIMD context. Never execute a single Thread simultaneously on two CPUs. Fully preemptible kernel execution remains deferred.

### 17.3 Clocksource vs. clockevent

| Subsystem | Job |
| --- | --- |
| **Clocksource** | Read a validated non-decreasing monotonic elapsed-time counter |
| **Clockevent** | Arrange future timer interrupts |
| **Timer manager** | Maintain and expire waiter/sleeper deadlines |
| **Scheduler/accounting** | Select Threads and measure executed CPU time |

The initial public-ish time domain is **monotonic nanoseconds** from a defined origin (not wall-clock/calendar time). Absolute deadlines should have overflow/saturation and infinite-wait semantics defined before ABI freeze. Sleep/wakeup provides *eligibility after deadline*, not precise immediate dispatch.

For x86-64, a calibrated **Local APIC timer** is the proposed scheduler clockevent; a validated **TSC** is a candidate clocksource, with PIT/HPET as possible boot calibration references when available. Do not require optional TSC-deadline hardware for a basic boot. A periodic 1 ms tick with 10 ms slice counting is an illustrative early option; high-resolution clock readings need not share tick granularity. A tickless earliest-deadline approach is a later power/efficiency optimization.

### 17.4 Idle, SMP and affinity

When nothing is runnable, an internal per-CPU idle context waits for an interrupt (e.g., x86 `HLT` under a race-free interrupt/wakeup protocol), not an ordinary user Node. Future topology progression: one CPU/global queue → initialize application processors → per-CPU run queues → balancing and migration → affinity/topology features → experimental asymmetric policies. Scheduler wakeups, queue changes, local/APIC inter-processor interrupts, lock ordering and TLB shootdowns must be concurrency-safe.

**The initial bootstrap processor is not a permanent Manager Core.** The original dedicated Manager/Worker architecture stays deferred research; avoid encoding dedicated core roles in base types.

### 17.5 Draft syscall and acceptance outline

Candidate operations: `thread_yield`, `thread_sleep_until`, `thread_exit`, `thread_get_info`, and `time_monotonic`. Operations controlling *another* Thread require proper capabilities. Do not promise priority inheritance, global CPU budgets or real-time scheduling initially.

**Minimum tests:** two CPU-bound Ring 3 Threads progress without yielding; blocking frees CPU time; timer-driven wakeup doesn't double-queue; canceled/terminated Threads never execute; idle CPUs don't miss new runnable work; SMP version prevents duplicate simultaneous dispatch of one Thread. Timing benchmarks under QEMU are not physical-machine latency guarantees.

---
## 18. Threat model and security boundaries — `NEU-SEC-001`

**Status:** Proposed explicit threat model, not a certification, formal proof, or security guarantee for unimplemented software. This section makes the capability-first philosophy in §6 concrete.

### 18.1 Security objectives and assumed adversary

**Confidentiality:** a Node cannot observe unrelated Node/kernel private memory without authority. **Integrity:** a Node cannot modify protected objects or fabricate stronger capabilities. **Availability:** a contained Node failure should not *by itself* require a system panic; resource budgets mitigate, but do not eliminate, exhaustion or denial of service.

The initial adversary can execute arbitrary instructions in an **unprivileged Ring 3 Node**, craft malformed syscalls, race its own Threads, guess opaque handles, and attempt unauthorized MMIO/mappings. Drivers may be buggy or malicious within their granted authority. The Supervisor may be compromised and can abuse any powerful capabilities it legitimately holds. The kernel must never mistake a Node's origin, name, or conventional user identity for extra authority.

### 18.2 Trusted Computing Base and non-goals

The TCB contains the Neu syscall/exception boundary, capability enforcement, memory isolation, scheduler/context switch, core IPC, platform/interrupt mechanisms, and any **privileged kernel drivers**. CPU/MMU/IOMMU mechanisms and firmware assumptions influence security; a userspace driver is **not** automatically DMA-isolated by CPU page tables. Threads *within one Node* are not separate security principals: they share memory and capabilities.

**Initially not promised:** formal verification; prevention of all speculative/microarchitectural side channels; defense against malicious firmware or physical attacks; secure/measured boot; arbitrary Ring 0 driver-fault containment; hard real-time availability; general untrusted DMA isolation without suitable IOMMU setup; undoing data an authorized recipient has already obtained.

### 18.3 Named security invariants

| ID | Invariant | Required behavior |
| --- | --- | --- |
| `NEU-SEC-I01` | **No ambient authority** | Object names, IDs and addresses are not sufficient authority. |
| `NEU-SEC-I02` | **No authority amplification** | Derived rights are a subset of source rights. |
| `NEU-SEC-I03` | **Node-local handles** | A handle from Node A has no special meaning in Node B. |
| `NEU-SEC-I04` | **Atomic transfer** | Failure doesn't half-move or silently duplicate capabilities. |
| `NEU-SEC-I05` | **Safe object lifetime** | Close, transfer, mapping, revocation and in-flight work cannot cause use-after-free. |
| `NEU-SEC-I06` | **Memory sanitation** | Reassigned private frames don't reveal old Node data. |
| `NEU-SEC-I07` | **Fail within fault domain** | Unrecoverable userspace faults should terminate only that Node when possible. |

### 18.4 Syscall boundary hardening

Treat every syscall as adversarial: verify handle generation/type/rights; validate *every* object in multi-object operations; reject invalid flags, unaligned or noncanonical pointers, integer overflow and out-of-bounds offsets; pin references during operations; and protect against TOCTOU by copying small mutable descriptors to kernel-owned memory before acting. Safe user-copy operations must recover from expected user faults rather than turn malformed pointers into uncontrolled kernel faults.

Protect mappings using page permissions, guard pages, W^X including aliases, frame zeroing, and supervisor/user separation. Consider SMEP/SMAP and stronger user/kernel page-table separation where supported later. Floating-point/SIMD state must not leak between Threads/Nodes. Capabilities and Rust reduce classes of mistakes but cannot eliminate unsafe-code, assembly, hardware, or concurrency vulnerabilities.

### 18.5 Drivers, recovery and denial of service

A userspace driver can fault without forcing a kernel panic **if** the kernel and hardware authority model contain the effects. DMA remains unsafe to delegate to untrusted drivers until proper DMA translation/restrictions are configured. Device reset, IRQ storms, and shared buses can still affect availability. User driver **restart policy** belongs outside Neu; the kernel offers fault reports and resource containment.

`ResourcePool` budgets prevent unbounded allocations *within defined resource categories*, not arbitrary CPU starvation, syscall floods, or contention. Equal-priority per-Thread round robin is not fair-share per Node. Future budgeted CPU authority can be considered, but must not be claimed as implemented.

The privileged NeuKMon debug interface can bypass ordinary isolation by design. Exclude or disable it in production-oriented builds, restrict output of sensitive information, and avoid privileged memory mutation commands as defaults.

**Policy principle — `NEU-SEC-P01`: Least Authority by Construction.** Give components only the authority required, and keep the privileged TCB as small as reasonably possible. A rejected unauthorized operation is a **passing security test** when it also preserves kernel invariants and service continuity.

---

## 19. Device, interrupt and driver interfaces — `NEU-DEV-001`

**Status:** Proposed framework mechanisms; not a promise of a complete driver ecosystem. This expands §12. No general untrusted-DMA driver support is in the initial scope.

### 19.1 Placement, discovery and resource assignment

Neu's privileged side handles exceptions, IDT, Local APIC/I/O APIC and scheduler timers, essential serial debug output, hardware authority bookkeeping and trusted controller operations. Peripheral protocol logic (network, storage, audio, device classes) is **preferably delegated to isolated Driver Nodes** when practical. Privileged residency is justified case by case, not by default.

The initial x86-64 platform backend may inspect Limine-provided data, limited **ACPI MADT** entries for processor/interrupt topology, **PCI configuration space**, and **ACPI MCFG** where applicable to locate PCIe configuration access. Parsing must be bounded and validated. A complete ACPI AML interpreter, device manager and hotplug stack are *not* required for the first kernel milestone. A future userspace policy component may select drivers, but the kernel enforces who holds hardware authority.

**Exclusive claim baseline:** two unrelated Nodes should not silently acquire conflicting write authority over one device function. Device claims, ownership, release, reassignment, failure quarantine, and resource lifetime need an explicit protocol before supporting general external driver launch.

### 19.2 Capability-governed hardware resources

Proposed, not finalized, capability types:

| Type | Intent | Candidate authorization |
| --- | --- | --- |
| `DeviceResource` | Specific discovered function and authority to claim/configure it | `INSPECT`, `CLAIM`, `CONFIGURE` |
| `MmioRegion` | Device-register physical region, not normal RAM | `MAP_READ`, `MAP_WRITE` |
| `IoPortRange` | x86 programmed port access | `IO_READ`, `IO_WRITE` |
| `Interrupt` | Delegated hardware interrupt notification | `WAIT`, `ACKNOWLEDGE` |
| `DmaDomain` | Future restricted device-visible address space | `MAP_DMA`, `UNMAP_DMA` |

A device's discoverability **does not** imply permission to map its registers or alter global PCI configuration. The kernel validates exact physical ranges, exclusive/conflicting assignments, appropriate MMIO caching attributes, and interrupt linkage. Initial port I/O can use small kernel-mediated operations rather than broad Ring 3 I/O privilege. Ordinary `MemoryObject` handles do not authorize arbitrary MMIO or DMA.

### 19.3 Interrupt delivery to a Node

```
Hardware IRQ → trusted Neu interrupt entry/controller code
             → identify authorized source; mask/record pending as needed
             → notify Interrupt kernel object
             → make waiting Driver Thread RUNNABLE
             → driver services device condition
             → driver acknowledges device status and requests interrupt rearm
```

A hardware **source**, CPU interrupt **vector**, controller state and PCI interrupt configuration are distinct concepts. The driver must never execute directly as a privileged IRQ entry function. Candidate operations: `interrupt_wait(interrupt_cap, deadline)` and `interrupt_ack(interrupt_cap)`, with future controlled event/WaitSet binding.

**Acknowledgment is hardware-sensitive:** clearing the device's own pending state is not the same as sending the controller's end-of-interrupt command; safe masking, EOI and rearm ordering varies by source. For an unresponsive driver, leave the relevant source masked/quarantined where applicable, rather than permitting unbounded re-delivery. Interrupt notifications may be coalesced; do not treat them as an infallible packet count. Start with one waiting Thread per interrupt object; MSI/MSI-X and complex routing can follow later.

### 19.4 DMA and IOMMU restriction

DMA devices operate outside ordinary CPU memory-translation rules. **No general untrusted DMA delegation** until suitable IOMMU/device isolation and (where needed) interrupt remapping have been implemented and tested. Later DMA support needs pinned or otherwise stable buffers, validated device-visible addresses, proper cache/coherency handling, domain mapping, safe unmap, device quiescence and reclamation. Removing a Node's memory mapping is **not** proof that pending device DMA has finished; physical pages must not be reused until safe.

Userspace driver termination requires authority cleanup, IRQ quarantine, and possibly device reset *before* another driver takes over. Automatic restart remains external supervisor policy, and some devices cannot be independently reset safely.

### 19.5 Practical test target: QEMU EDU

The educational PCI device is a useful future test for limited discovery, assigned MMIO access, IRQ notification and acknowledgment **without initially enabling DMA**. QEMU option: `-device edu` (a partial QEMU argument); example PCI vendor/device `1234:11e8`. Its virtual hardware supports more features, but we do not need them all for the first proof. Official reference: [QEMU EDU device specification](https://www.qemu.org/docs/master/specs/edu.html).

**Progression:** IDT/exceptions → Local APIC/timer/MADT → trusted resource discovery/claim → scoped MMIO/port capabilities → Interrupt objects and acknowledgment → QEMU EDU test Node → deliberate user-driver failure containment → advanced MSI, DMA/IOMMU and reset support. D1/D2 partly precede Node execution; most driver support does not.

**Policy principle — `NEU-DEV-P01`: Hardware authority is delegated explicitly, never inferred.**

---

## 20. Fault handling, panic and diagnostics — `NEU-DIAG-001`

**Status:** Proposed diagnostics and failure classification. Related summary and NeuKMon policy appear in §§5.5 and 13.

### 20.1 Classify failures before responding

| Class | Examples | Initial policy |
| --- | --- | --- |
| **Operational error** | Bad handle, denied rights, quota exceeded, Channel full | Return a defined Neu status; avoid panic. |
| **Node fault** | Unrecoverable Ring 3 page fault, invalid instruction | Record fault; terminate offending Node by default. |
| **Fatal kernel error** | Critical allocator corruption, unrecoverable privileged fault | Stop normal execution; minimal panic diagnostics; halt. |

Node-wide termination on an unrecoverable Thread execution fault is the initial conservative default because other Threads share the affected Node's mutable state. Recoverable faults may resume once legitimately handled; demand paging isn't part of first implementation. A Node failure isn't proof the Node caused any subsequent kernel bug.

A kernel-internal **FaultRecord** should capture a sanitized classification, Node/Thread/CPU identity, instruction address, relevant fault address, architecture exception code, monotonic timestamp when available, and exit reason. An authorized future fault endpoint may receive a safe subset without disclosing privileged or unrelated memory.

### 20.2 Panic path and Rust runtime

**Fail-stop** rather than silent continued execution after known core invariant corruption. A first Rust `#[panic_handler]` should follow **abort-style**, non-unwinding behavior and never return. The emergency path avoids heap allocation, normal logger locks, blocking IPC, ordinary waiting or scheduler operations; prevent recursive panic reporting. Under SMP, claim a primary panic reporter with an atomic state and make a best-effort attempt to halt other CPUs; no guarantee is possible after arbitrary state corruption.

Preserve a separate **emergency serial writer** which emits bounded, minimal diagnostics without depending on ordinary buffered logging. Routine diagnostics may use an in-memory ring buffer, optional levels `TRACE`, `DEBUG`, `INFO`, `WARN`, `ERROR`, and special `PANIC`. Don't leak Node private buffers or raw secrets into public logs.

### 20.3 Exception-specific emergency machinery

Use x86-64 exception frames with careful privilege classification. A dedicated **IST stack** for double fault improves the odds of handling severe stack faults; NMI and machine-check paths are exceptionally sensitive and should have conservative limited handling. A triple fault may reset/halt hardware before any useful logging. Expected user-pointer faults occurring during safe kernel copies must be handled using an explicit recoverable path rather than blindly classified as kernel corruption.

### 20.4 NeuKMon and external debugger

Retain `NeuKMon=off|boot|panic` as a candidate configuration, with **`boot` for development** and `panic` **experimental, not entered automatically** in the first panic implementation. A corrupted kernel may not safely support interactive commands, locking, or heap-backed inspection. Production-oriented builds should exclude or disable privileged interaction.

For QEMU, `-s -S` (partial options) enables a paused startup with a remote GDB endpoint. Preserve matching unstripped ELF/debug symbols and build IDs for backtraces. Serial output is not a replacement for stepping through CPU state and memory under a debugger.

### 20.5 Expected diagnostics and tests

Candidate panic identifiers are human-facing diagnostic labels, **not ABI syscall errors**. Initial panic policy is `halt`; an optional `reboot` policy is deferred to deployment needs. Crash dumps to disk or network are not needed for the kernel-only initial milestone.

**Test:** invalid Node memory/invalid instruction cause controlled Node exit; malformed syscall doesn't turn into arbitrary Ring 0 panic; intentional development panic prints bounded diagnostics and halts; recursive panic falls back to minimal handling; double-fault emergency path is attempted where feasible; SMP has at most one main panic reporter (best-effort).

**Policy principle — `NEU-DIAG-P01`: Fail Safely, Explain When Possible.**

---

## 21. Verification, fault injection and CI — `NEU-TEST-001`

**Status:** Proposed test program, not a claim of executed tests or formal verification. This gives detailed acceptance criteria for the roadmap in §22.

### 21.1 Validation levels

| Level | Scope | Examples |
| --- | --- | --- |
| **Host unit tests** | Portable Rust functions with no hardware dependency | Capability generations, rights algebra, accounting, parsers |
| **QEMU integration** | Real kernel entry, paging, interrupts, Nodes and syscalls | Boot, `abi_query`, Ring 3 transitions, Channel operations |
| **Negative security tests** | Intentionally unauthorized inputs | Forged/stale/wrong-type handles, forbidden mappings |
| **Concurrency/stress** | Coordinated interleavings and repeated racing operations | close/use, wake/timeout/kill, frame reclamation and TLB |
| **Regression/platform matrix** | Supported memory and CPU configurations | Repeated boot profiles, SMP (when supported), toolchain versions |

Host tests cannot prove hardware isolation; QEMU alone cannot establish all memory safety and security properties. Tests are evidence of observed behavior, **not formal proof**. Model-based/property tests are particularly attractive for capability tables, rights non-amplification, resource accounting and Channel transfer state machines.

### 21.2 Test runner and output contract

A host runner should build a test kernel, boot **QEMU + OVMF + Limine**, capture **structured serial output**, apply a timeout, and classify a **known test-only guest-exit result**. `isa-debug-exit` (`-device isa-debug-exit,iobase=0xf4,iosize=0x04`) is a possible QEMU-only exit mechanism. Its guest-written value is transformed into a host process exit status; the runner must decode it rather than assume conventional zero success. This device is **never a public Neu syscall or mandatory production hardware requirement**.

Test result records should distinguish *expected successful completion*, *expected Node fault*, *expected kernel panic* and *unexpected reset/hang*. A `PASS` log line alone isn't sufficient if the guest crashes immediately afterward. Tests should record kernel build ID and chosen QEMU/OVMF/Limine toolchain versions. Keep simulator performance tests separate from native hardware timing claims.

### 21.3 Required positive and negative scenarios

| Subsystem | Positive requirement | Negative/failure requirement |
| --- | --- | --- |
| Frame allocation | Reserve, allocate, free and reuse frames | Invalid/double-free detection and zeroed reuse |
| AddressSpace | Valid per-Node mapping | No unauthorized cross-Node read/write or prohibited W+X alias |
| Node/Thread | Ring 3 entry, exit, preemption | Unrecoverable Node fault contained; terminated Thread never runs |
| Capabilities | Correct type/rights lookup | Stale/guessed/wrong-type/insufficient-rights handle denied |
| Kernel objects | References preserve lifetimes | No use-after-free amid handle close or queued transfer |
| Channels | FIFO messages, peer closure, backpressure | Full/empty/closed queue and atomic move failure correctly handled |
| Waiting | Block and resume correctly | No lost wakeups; timeout/termination racing is safe |
| Syscalls | `abi_query` round trip and correct register state | Invalid syscall/pointer/struct fails safely |
| Devices | Authorized MMIO/IRQ path on selected emulated device | Unassigned device access rejected; failed driver contained where possible |
| Panic | Explicit test panic stops with diagnostics | Recursive/privileged fault doesn't enter uncontrolled retry loop |

**Concurrency tests:** deliberate pause hooks near capability lookup/close, mapping teardown, queue insertion/wakeup, and object reclamation; combine these with noninstrumented stress to reduce instrumentation bias. At SMP stage, repeat with 2 and 4 CPUs, including TLB shootdowns and simultaneous scheduling assertions.

### 21.4 Hardware profile and CI plan

| Profile | CPU | RAM | Purpose | Availability |
| --- | --- | --- | --- | --- |
| Basic boot | 1 vCPU | 256 MiB | Initialization and logging | First |
| Standard integration | 1 vCPU | 512 MiB | Nodes, syscalls, capabilities, IPC | First |
| Low-memory stress | 1 vCPU | 128 MiB | Experimental graceful limits | When useful |
| SMP correctness | 2 vCPUs | 512 MiB | Concurrent Threads | After SMP exists |
| SMP stress | 4 vCPUs | 1 GiB | Cross-CPU races | After SMP exists |

**CI tiers:** every push/PR runs formatting, lint, host tests, build and inexpensive QEMU smoke/integration tests; extended/nightly/manual suites run repeated race, fuzz and stress testing. Pin versions and preserve reproducible build and test command lines. No minimum RAM claim is verified until the relevant feature profile passes against it.

### 21.5 Milestone exit criteria

A milestone is complete only when its authorized operations work, expected illegal operations are rejected, cleanup/resource limits hold, earlier regression tests pass, and known limitations are written down. A failed unauthorized operation is a **successful security test** only if the kernel remains correct and responsive within the intended fault-containment boundary; a panic or indefinite hang is not a pass.

**`NEU-TEST-P01`: A Rejected Operation Is a Successful Security Test** (provided the intended security invariant actually remains intact).

Formal analysis may be considered later for bounded subsystems; **Neu is not formally verified**.

---

## 22. Development roadmap and acceptance tests

The roadmap intentionally avoids building an entire OS before meaningful kernel milestones are achieved. **Detailed verification contracts are in §21.** The next planned *discussion*, `NEU-BOOT-001` (full boot-to-first-Node integration plan), remains **open**; the stages below are a working implementation outline, not an agreed, frozen build sequence.

| Stage | Focus | Minimum proof |
| --- | --- | --- |
| **K0: Boot** | External bootloader integration, x86-64 initialization, serial diagnostics | Deterministic QEMU boot, readable logs and basic NeuKMon |
| **K1: Memory** | Physical allocator, page tables, kernel mappings, exceptions | Allocate/free and map/unmap safely; reject invalid regions |
| **K2: Ring 3 + ABI** | Minimal static Node Image; bootstrap Node entry, syscall entry/return | `abi_query` round trip, `thread_exit`, safe return to kernel |
| **K3: Node isolation** | Two Nodes, one Thread each | Separate address spaces; fault containment |
| **K4: Thread scheduling** | Timer-driven preemption; one Thread per Node first, then multiple Threads | CPU-bound Threads both progress; Node-local shared state correct |
| **K5: Capabilities** | Handle tables, typed rights, close, authorization tests | Invalid, wrong-type, stale, and unauthorized operations denied |
| **K6: Channels** | Bounded message passing | Reliable FIFO, defined backpressure and peer closure |
| **K7: Capability moves** | Atomic transfer via Channels | Ownership consistent on success and failure |
| **K8: Advanced memory** | `MemoryObject` sharing | Same backing pages, permission isolation, stress tests |
| **K9: SMP** | Multiple x86-64 CPUs | Concurrent scheduling and synchronization stress tests |
| **K10: Advanced authority** | Derivation and revocation | No amplification; invalidation and mapping safety under concurrency |
| **K11: Device primitives** | Selected userspace driver support, potentially QEMU EDU | Authorized MMIO/IRQ access; untrusted DMA unsupported without suitable isolation |

**Ordering may shift** in response to dependencies discovered during implementation. In particular, certain internal object-allocation mechanisms will appear before their fully generalized NodeFactory/ResourcePool public interfaces; these bootstrap-only paths must not become unrestricted userspace backdoors.

### 22.1 High-value integration demo

```text
[NEU] Boot complete
[NEU] Node A and Node B constructed
[NEU] Two isolated address spaces established
[NEU] Threads scheduled
[NEU] Unauthorized mapping denied
[NEU] Channel A->B message delivered
[NEU] Node-local user fault contained
```

**All output above is hypothetical.** The real goal is reproducible tests verifying behavior, not a cosmetic boot transcript.

### 22.2 Test categories

- **Correctness:** resource lifecycle, page-table invariants, syscall validation, accounting.
- **Security:** wrong rights/type, unauthorized cross-Node operations, stale handles, fault containment.
- **Concurrency:** simultaneous close/use, IPC races, SMP synchronization and TLB shootdowns.
- **Resource limits:** creation under quotas, queue exhaustion, allocation failure and cleanup.
- **Performance:** context switch, syscall latency, small-message IPC, shared-memory mapping overhead; compare meaningful baselines before optimizing.
- **Portability:** keep architecture-neutral components buildable/testable without relying on x86-64 structures.

---

### 22.3 Integration objective and dependency order (preliminary)

The end-to-end kernel-only demonstration goal is:

```text
Limine → Neu x86-64 bootstrap → validated memory map / serial logging
       → page allocator and protected page tables → exception handlers
       → static bootstrap Node image → Ring 3 initial Thread
       → native abi_query → thread_exit → kernel records exit
```

This is a **dependency sketch**, not the full `NEU-BOOT-001` design. It should not assume Channels, generalized NodeFactory, SMP, or all ResourcePool features are present for the *first* Ring 3 program. Narrow internal bootstrap operations are permissible but must not become unrestricted public syscalls. First build a small repeatable kernel, then expand to dual-Node isolation, preemption, capabilities, IPC and beyond.

**Next specification not yet decided:** `NEU-BOOT-001`, including repository/build layout, Limine version/configuration, bootstrap module packaging, precise serial/debug setup, startup-memory ownership and milestone-by-milestone QEMU acceptance commands.

---

## 23. Proposed Rust workspace organization

An **illustrative** layout (do not precreate every folder before needed):

```text
neu/
├── kernel/
│   ├── core/
│   │   ├── node/
│   │   ├── thread/
│   │   ├── capability/
│   │   ├── object/
│   │   ├── memory/
│   │   ├── scheduler/
│   │   └── ipc/
│   ├── arch/
│   │   └── x86_64/             # first backend
│   ├── platform/
│   │   └── qemu_x86_64/        # first validation target
│   ├── drivers/                # strictly required kernel drivers
│   ├── syscall/
│   └── debug/
│       └── kmon/
├── boot/                       # external boot protocol integration
├── tests/                      # kernel fixtures & test Nodes
├── docs/
│   └── adr/                    # tracked decisions / rationale
└── NEU_ARCHITECTURE.md
```

Optionally add a `kernel/test_support/` module compiled only for test builds, an `arch/x86_64/` user-entry and interrupt-stack implementation, `memory/` frame + address-space modules, and a tiny `tests/nodes/` fixture folder **only when those pieces are needed**. Do not pre-create large unused scaffolding for future architectures or user applications.

Keep internal Rust structures **private to the kernel**. A small ABI crate or generated definitions can be introduced later for kernel/test-Node agreement; this need not become a full standalone application SDK.

---

## 24. Architectural risks, limitations, and open questions

| Question / risk | Current handling |
| --- | --- |
| Kernel scope expanding into a complete OS | Enforce kernel-only boundary and minimal fixtures |
| Overly complex initial scheduler | One CPU first; symmetric SMP later; defer core roles |
| Security of capability transfer | Move-only before derivation; atomic commit semantics |
| Mapping lifetime vs. revocation | Track mappings and references; full revocation deferred |
| Rust memory safety across interrupt/FFI/assembly | Explicit unsafe boundaries, invariants and tests required |
| Userspace pointer races | Fault-aware copies and kernel snapshots |
| Cyclic object references | Define object ownership and teardown; avoid uncontrolled cycles |
| Userspace driver DMA faults | Require guarded DMA/device capabilities; IOMMU when available |
| ABI ossification too early | Mark v0.x experimental; freeze only after exercised behavior |
| Premature portability complexity | One x86-64 backend first, interfaces kept narrow |
| Hardware requirements not empirically validated | Label as targets; measure/test multiple RAM profiles |
| Linux application compatibility | Third-party userspace project; not a kernel requirement |

### Open design questions (not yet settled)

1. Exact Node lifecycle transition rules, including suspension and asynchronous termination.
2. Exact `NodeFactory` / `ResourcePool` representation, quota hierarchy, and transfer accounting.
3. Capability handle encoding, generation exhaustion, copy/derive permissions, and revocation granularity.
4. Full object ownership model, queued-transfer lifetime, and lock/refcount strategy.
5. IPC message maximum sizes, atomicity when receivers cannot accept handles, wait/cancel semantics.
6. Memory mapping provenance and whether explicit mapping handles are ever exposed.
7. Detailed syscall register clobbers, scalar outputs, ABI wire types, status-number allocations.
8. User exception/fault-channel format and trusted first-Node bootstrap protocol.
9. Interrupt and DMA resource-capability semantics; minimum hardware isolation guarantees.
10. SMP scheduling topology, CPU affinity policies, and synchronization primitives.
11. Precise supported CPU baseline and empirically verified minimum RAM.
12. Stable first boot protocol selection/version and firmware/platform configuration.

### Further questions from the new subsystem drafts

13. Exact initial Node entry stack alignment, runtime ABI field offsets, descriptor lifetime, and launch configuration authority.
14. ELF segment alignment/overlap and transition to PIE/`ET_DYN` without weakening W^X.
15. Physical allocator metadata for fragmented memory maps, transaction rollback and frame ownership.
16. Atomic word wait mapping identity, close/unmap cancellation, Event signal coalescing and eventual WaitSet lifetime.
17. Clocksource calibration and monotonic time guarantees, timer tick rates, scheduling budgets and low-power/idle races.
18. Explicit threat assumptions for malicious Supervisor, MMIO, DMA, debugging access and CPU side channels.
19. PCI/device-exclusive claim and revocation, interrupt-acknowledgment state machines, and safe device reset/quarantine.
20. Panic owner ordering, emergency logger fallback, kernel trace security, and test-only exit result encoding.
21. QEMU test protocol schema, host runner, CI version pinning, model-based validation and formal-method candidates.
22. **`NEU-BOOT-001` remains undecided:** an exact boot-to-first-Node implementation and testing plan.

### Deferred research, not rejected

- Manager/Worker asymmetric CPU-core scheduling.
- Memory borrowing/temporary lending with enforceable lifetime and revocation semantics.
- Advanced zero-copy ownership transfer with exclusive access guarantees.
- Full derivation-aware capability revocation.
- AArch64, then possible RISC-V 64 porting.
- Dynamic policy-controlled kernel residency for specialized subsystems.
- Live SMP-aware NeuKMon and post-panic diagnostics.
- Broader device and mobile-specific hardware support.

---

## 25. Decision register (seed for ADRs)

| ID | Decision | Status | Rationale |
| --- | --- | --- | --- |
| **NEU-001** | Neu is a **kernel-only** project | Chosen | Prevent OS/ecosystem scope creep |
| **NEU-002** | Rust-based **small-core hybrid kernel** | Chosen | Controlled trusted core with selective integration |
| **NEU-003** | x86-64 first; portability boundaries for other ISAs | Chosen | Feasible testing without permanent lock-in |
| **NEU-004** | Use an external bootloader; Limine preferred | Part chosen / part proposed | No custom bootloader blocker |
| **NEU-005** | **Node** is the protection/authority domain | Chosen | Native execution abstraction |
| **NEU-006** | Nodes support **multiple Threads** | Chosen | Parallel and concurrent execution |
| **NEU-007** | Capability-first, no `root`/`sudo` kernel bypass | Chosen | Explicit and delegable authority |
| **NEU-008** | Neu-native non-POSIX syscall contract | Chosen | Architecture driven by Nodes and objects |
| **NEU-009** | Typed kernel objects and capability-mediated access | Proposed baseline | Uniform protected-resource model |
| **NEU-010** | Channels first; zero-copy shared-memory IPC later | Proposed | Simpler correctness path, future efficiency |
| **NEU-011** | One CPU first, then symmetric SMP | Chosen direction | Reduce scheduler complexity |
| **NEU-012** | Manager/Worker core scheduling postponed | Deferred | Needs a working SMP baseline to compare |
| **NEU-013** | Optional development-only NeuKMon | Proposed | Debugging without creating an OS shell |
| **NEU-014** | Selective kernel drivers + optional driver Nodes | Proposed | Isolation with pragmatic performance |
| **NEU-015** | 256 MiB provisional minimum, 512 MiB dev default | Proposed / unverified | Testable reference constraints |
| **NEU-016** | Linux/POSIX compatibility via third parties only | Out of scope | Preserve native ABI and project focus |
| **NEU-017 / EXEC** | ELF64 static image, userspace loading, narrow first-Node bootstrap | Proposed | Separate binary format from kernel execution mechanisms |
| **NEU-018 / MEM** | Bitmap PMM, per-Node VMM, eager MemoryObjects and alias-aware W^X | Proposed | Auditable first memory implementation |
| **NEU-019 / SYNC** | Wait queues, Events, Node-local atomic word waits | Proposed | Minimal blocking/wakeup machinery |
| **NEU-020 / SCHED** | Round robin, experimental 10 ms quantum, separate timekeeping | Proposed | Start simple; measure before optimizing |
| **NEU-021 / SEC** | Documented untrusted-Node threat model and explicit TCB limits | Proposed | Security claims must match enforceable boundaries |
| **NEU-022 / DEV** | Scoped MMIO/IRQ capability model, QEMU EDU test, defer untrusted DMA | Proposed | Fault isolation without unsafe arbitrary hardware access |
| **NEU-023 / DIAG** | Node-fault containment and fail-stop allocation-free panic path | Proposed | Avoid continuing after core invariant loss |
| **NEU-024 / TEST** | Automated host/QEMU, negative, stress and CI tests with exit criteria | Proposed | Verify actual behavior, not boot messages |
| **NEU-025 / BOOT** | Detailed boot-to-first-Node integration specification | Open / next | Convert architecture to a reproducible implementation plan |

Each entry should eventually move to a separate ADR only when implementation pressure reveals real alternatives and tradeoffs. Avoid presenting draft details as ratified specifications merely because they appear here.

---

## 26. Prospective adoption

Neu should expose well-documented mechanisms that *could* support future third-party desktop, server, embedded, or mobile operating environments. Adoption is an aspiration, **not a deliverable**. A future platform integrator would still need user-facing services, policy, drivers, app runtimes, release engineering, and device-specific work.

A third-party Linux compatibility environment may be possible only to the extent that Neu's general mechanisms can emulate the relevant Linux semantics; neither full fidelity nor performance can be assumed. Neu should **not add Linux-only kernel syscalls just to chase compatibility**, but should avoid gratuitously blocking legitimate compatibility work where a general kernel mechanism is appropriate.

---

## 27. External technical references for later study

These are reading pointers for future architectural comparison, **not dependencies or copied specifications**. Their inclusion does not mean Neu has their guarantees or implementation maturity.

- [seL4 Documentation](https://docs.sel4.systems/) — capabilities, object authority, kernel API concepts.
- [Fuchsia / Zircon Documentation](https://fuchsia.dev/fuchsia-src/concepts/kernel) — handles, objects, VMOs, channels, driver-related design.
- [Limine Bootloader](https://github.com/limine-bootloader/limine) — potential external bootloader and protocols.
- [Rust bare-metal target documentation](https://doc.rust-lang.org/rustc/platform-support.html) — target support and toolchain constraints.
- [Linux kernel debugging documentation](https://docs.kernel.org/process/debugging/index.html) — comparative kernel-debugging mechanisms.
- [QEMU EDU device](https://www.qemu.org/docs/master/specs/edu.html) — virtual PCI device with MMIO and interrupt support for a future driver Node test.
- [QEMU documentation](https://www.qemu.org/docs/master/) — VM boot, GDB debugging, virtual-device configuration and host-side testing.
- [ELF Generic ABI](https://gabi.xinuos.com/) — executable program headers and segment semantics.
- [Rust standard library development guide](https://doc.rust-lang.org/beta/embedded-book/) — embedded/no_std concepts; confirm actual target and nightly toolchain requirements during implementation.

---

## Closing principle

> **Nodes execute. Capabilities authorize. Kernel objects provide mechanisms.**
>
> **Neu builds the kernel. The rest of the operating system is someone else's decision.**

The next planning deliverable is the still-open **`NEU-BOOT-001` boot-to-first-Node integration plan**; the next engineering deliverable is a small, testable x86-64 boot-and-memory foundation followed by the first Ring 3 Node and syscall round-trip. Neither should require implementing all later dossiers. Keep these architectural boundaries reviewable and revise them in response to real test evidence.

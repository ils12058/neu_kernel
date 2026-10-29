# Architecture Decision Records

This directory records **why** Neu Kernel makes architectural choices. The
[architecture document](../Neu_Kernel_Architecture_v0.2.md) remains the design
baseline; its [Section 25 register](../Neu_Kernel_Architecture_v0.2.md#25-decision-register-seed-for-adrs)
seeds ADRs without ratifying its proposals.

## Format and numbering

Use `NNNN-short-title.md`, starting at `0001`, with the next unused number.
Numbers are independent of `NEU-001` through `NEU-025`; a record may cover part
of an entry or several related entries. Keep filenames stable; never reuse IDs.

Each ADR includes a title, date, **Status**, **Related NEU decisions**, architecture
links, and sections for **Context**, **Decision**, **Alternatives considered**,
**Consequences**, and dated **History**. Explain the constraints, rationale,
rejected options, costs, benefits, and unresolved obligations.

## Lifecycle and historical record

| Status | Meaning |
| --- | --- |
| Proposed | A recommendation whose stated questions or tradeoffs remain unresolved. |
| Accepted | An explicitly chosen direction within the record's stated scope. |
| Rejected | A considered proposal that was not adopted; retain its rationale. |
| Superseded | A previously accepted decision replaced by a linked successor ADR. |

Proposed becomes Accepted or Rejected through an explicit decision; record its
date and reason. Accepted means chosen, **not implemented or verified**. Related
NEU identifiers do not imply acceptance of entire entries or neighboring proposals.

For future milestones, create or update an ADR with each significant architectural
choice or revision. Preserve original rationale; append dated clarifications and
proposal updates. To replace an accepted decision, write a successor, mark the
old ADR Superseded when the replacement is accepted, and link both records in
their status/history and this index. Retain rejected and superseded files.

When changing the baseline, explicitly cross-reference the affected register
entry and explain the change. Keep register identifiers stable; unrecorded entries
retain their existing statuses. Create ADRs when real tradeoffs arise, without
placeholder files for all 25 decisions or future subsystems.

Implementation documentation explains **how** the system currently works:
versions, layouts, commands, and actual test results. ADRs explain **why** and link
to evidence when available. Acceptance does not assert Phase 0 completion.

## Phase 0 records

| ADR | Status | Related register entries |
| --- | --- | --- |
| [0001: Kernel-only project scope](0001-kernel-only-project-scope.md) | Accepted | NEU-001; NEU-016 boundary |
| [0002: Rust and x86-64 foundation](0002-rust-and-x86-64-foundation.md) | Accepted | NEU-002 language; NEU-003 |
| [0003: External bootloader boundary](0003-external-bootloader-boundary.md) | Accepted | NEU-004 external-loader direction |
| [0004: Limine integration for Phase 0](0004-limine-phase-0-integration.md) | Accepted; amended by 0005/0007 | NEU-004 Limine details; NEU-025 / BOOT remains open |

The external-loader direction and the Limine integration contract have separate
records so their scope, status, and future replacements can evolve independently.

## Phase 1A records

| ADR | Status | Related register entries |
| --- | --- | --- |
| [0005: Boot ownership and physical frames](0005-boot-memory-ownership-and-frame-allocation.md) | Accepted | NEU-004; NEU-018 PMM only; NEU-025 remains open |
| [0006: Private descriptors and fatal exceptions](0006-kernel-exception-descriptors-and-fatal-handling.md) | Accepted | NEU-003; NEU-023 privileged faults; NEU-024 local tests only |

ADR 0005 extends the Phase 0 boot contract; loader version/base revision remain
unchanged. A dated Phase 1B correction preserves the earlier supersession notation
as history while keeping ADR 0004 Accepted. No future Node or CI proposal is
accepted by these scoped records.

## Phase 1B record

| ADR | Status | Related register entries |
| --- | --- | --- |
| [0007: Owned paging and controlled mappings](0007-kernel-owned-paging-and-controlled-mappings.md) | Accepted | NEU-004 bootstrap access; NEU-018 kernel paging only; NEU-023 guards; NEU-024 local tests |

ADR 0007 amends the paging/access portions of 0005 and the guard/#PF inspection
assumptions of 0006. Nodes, general userspace VMM, and bootloader reclamation remain
outside these accepted implementation contracts.

## Phase 2A record

| ADR | Status | Related register entries |
| --- | --- | --- |
| [0008: Isolated Ring 3 fixture and test return](0008-isolated-ring-3-fixture-and-test-return.md) | Accepted, test scope only | NEU-017 embedded execution; NEU-018 bounded user mappings; NEU-021 privilege boundary; NEU-023 origin; NEU-024 local tests |

ADR 0008 extends 0006/0007 with private user tables, a separate RSP0 stack and
hardware-observed execution. Its `int 0x80` gate is test-only. The Node lifecycle,
startup descriptor, native syscall and full execution specifications remain
proposals; Phase 2B requires a separate concrete ABI decision.

## Phase 2B record

| ADR | Status | Related register entries |
| --- | --- | --- |
| [0009: Experimental native syscall and trusted return](0009-experimental-native-syscall-and-trusted-return.md) | Accepted, experimental fixture ABI | NEU-008 initial subset; NEU-017 bootstrap execution; NEU-021 boundary; NEU-023 containment; NEU-024 local tests |

ADR 0009 adds real SYSCALL entry and validated IRETQ return. ADR 0008's test-only
interrupt gate remains an independent regression path. Operation numbers/statuses
are provisional; this does not freeze the full Node, Thread or execution ABI.

## Phase 3A record

| ADR | Status | Related register entries |
| --- | --- | --- |
| [0010: Independent bootstrap Nodes and fault containment](0010-independent-bootstrap-nodes-and-fault-containment.md) | Accepted, two live Nodes with sequential initial Threads | NEU-005/006 boundaries; NEU-007 incomplete; NEU-008/011; NEU-017/018/021/023/024 scoped subsets |

ADR 0010 extends the singleton ownership/lifecycle portions of 0007–0009 while
preserving the accepted ABI 0.1 contract. Full capability governance, multiple
runnable Threads and scheduling remain unimplemented; related proposals are not
accepted wholesale. Architecture v0.2 and the original ADR rationale are retained.

## Phase 4A record

| ADR | Status | Related register entries |
| --- | --- | --- |
| [0011: Local timer interrupt foundation](0011-local-timer-interrupt-foundation.md) | Accepted, isolated kernel interrupt window only | NEU-003/011; NEU-018 device view; NEU-020 clockevent subset; NEU-021/023/024 scoped safety/tests |

ADR 0011 adds calibrated APIC timer delivery, returning IRQ entries and bounded
enable/drain/shutdown. All earlier Node/syscall and ordinary boot paths retain
IF=0. It records prerequisites for future userspace preemption without accepting
or implementing the complete scheduling proposal or a context switch.

## Phase 4B record

| ADR | Status | Related register entries |
| --- | --- | --- |
| [0012: Cooperative Thread context switching](0012-cooperative-thread-context-switching.md) | Accepted, two bootstrap Threads with IF=0 | NEU-005/006/008/011; NEU-017/018; NEU-020 cooperative FIFO subset; NEU-021/023/024 scoped safety/tests |

ADR 0012 adds real yielded-context ownership, private guarded entry stacks,
space-lifetime pins and FIFO Thread selection. ABI 0.1 remains unchanged; the
private yield operation is test-only. Timer-driven userspace preemption is
deferred to Phase 4C, and the full scheduling/capability proposals remain open.

## Phase 4C record

| ADR | Status | Related register entries |
| --- | --- | --- |
| [0013: Single-CPU user preemption](0013-single-cpu-user-preemption.md) | Accepted, two bootstrap Threads, user-only preemption | NEU-005/006/008/011; NEU-018 ownership; NEU-020 bounded round robin; NEU-021/023/024 scoped safety/tests |

ADR 0013 extends the historical Phase 4B boundary with real LAPIC IRQ capture,
a single-use acknowledged handoff and IF=1 user execution. Earlier cooperative
and kernel-timer profiles remain supported. General kernel preemption, SMP,
multithreaded Nodes, public scheduling/object interfaces and the complete
scheduling proposal remain outside its accepted scope.


## Phase 4D record

| ADR | Status | Related register entries |
| --- | --- | --- |
| [0014: Multiple Threads in one Node](0014-multiple-threads-in-one-node.md) | Accepted, internal two-Node / three-Thread profile | NEU-005/006/008/011; NEU-017/018; NEU-020 per-Thread FIFO; NEU-021/023/024 scoped safety, containment and tests |

ADR 0014 extends the earlier one-initial-Thread bootstrap policy with shared
Node roots, independent Thread stack lifetimes, early Thread exit and Node-wide
fault cancellation. It revises the scoped pinned-stack mutation rule with an
exclusive inactive-Thread edit proof and a checked cleanup receipt. Public
creation, capability authority, SMP and full scheduling proposals remain open.
Earlier ADR rationale is preserved through dated extensions.


## Phase 5A record

| ADR | Status | Related register entries |
| --- | --- | --- |
| [0015: Node-local capabilities and typed objects](0015-node-local-capabilities-and-typed-objects.md) | Accepted, fixed test objects, Node-local authority, private scalar requests | NEU-005/006/007/009; NEU-008/011; NEU-021/023/024 |

ADR 0015 implements the initial typed lookup/rights/close foundation, explicit
bootstrap grants and safe reference lifetimes shared by a Node's Threads. It does
not accept transfer, public derivation/attenuation, revocation or unrestricted
factories. Earlier rationale remains in place with dated extensions.

## Phase 5B record

| ADR | Status | Related register entries |
| --- | --- | --- |
| [0016: Capability ABI and live AddressSpace inspection](0016-capability-abi-and-live-address-space-inspection.md) | Accepted, experimental ABI 0.2 and own-Node read-only mappings | NEU-005/006/007/008/009; NEU-018/021/023/024 |

ADR 0016 extends 0009/0015 with explicit capability discovery/close and a real
AddressSpace INSPECT operation. Earlier ABI 0.1 configurations and private test
operations remain separate. Transfer, general mapping authority and revocation
remain future decisions. See the [versioned ABI specification](../abi/experimental-v0.2.md).

## Phase 5C record — bridge to K6

| ADR | Status | Related register entries |
| --- | --- | --- |
| [0017: Bounded scalar Channel endpoints](0017-bounded-scalar-channel-endpoints.md) | Accepted, exclusive Node endpoints, nonblocking two-message directional queues, experimental ABI 0.3 | NEU-005/006/007/008/009/010/011/021/023/024 scoped subsets |

This implements a narrow K6 communication subset while continuing K5 authority
enforcement. K6 is not declared complete; K7 moves and K10 revocation remain open.
Earlier ADR rationale and ABI 0.1/0.2 configurations remain intact.

## Phase 6A clarification

| Record | Decision | Related register entries |
| --- | --- | --- |
| [0018: Channel conservation and retirement invariants](0018-channel-conservation-and-retirement.md) | Accepted within the nonblocking single-CPU Channel contract | NEU-005/006/007/008/009/010/011/021/023/024 scoped subsets |

This clarifies ADR 0017; wait/wake, capability movement and revocation remain
future work.

## Phase 6B record — blocking Channel waits

| ADR | Status | Related register entries |
| --- | --- | --- |
| [0019: Blocking Channel waits, suspension and wakeup](0019-blocking-channel-waits.md) | Accepted, single-CPU IF=0 transactions, experimental ABI 0.4 | NEU-005/006/007/008/009/010/011/013/014/021/023/024 scoped subsets |

ADR 0019 extends ADR 0017/0018 with a real `Blocked` Thread state, owned wait
registrations, lost-wakeup-safe check/register/wake ordering, continuation-based
resume, peer-close and Node-fault cancellation, and bounded no-runnable
quiescence. Capability movement and revocation remain future work.

## Phase 7A record — atomic capability moves

| ADR | Status | Related register entries |
| --- | --- | --- |
| [0020: Atomic move-only capability transfer](0020-atomic-capability-moves.md) | Accepted, bounded Flag/Counter movement, experimental ABI 0.5 | NEU-005/006/007/008/009/010/011/013/014/021/023/024 scoped subsets |

ADR 0020 extends the existing FIFO with owned capability messages and separate
sender/receiver commits. It preserves prior ADR rationale and does not accept
AddressSpace/endpoint transfer, derivation or revocation proposals.

## Phase 7B record — transfer lifecycle hardening

| ADR | Status | Related register entries |
| --- | --- | --- |
| [0021: Capability-transfer lifecycle hardening](0021-capability-transfer-lifecycle-hardening.md) | Accepted, bounded single-CPU lifecycle scope; terminal-order scenario explicitly unverified | NEU-005/006/007/008/009/010/011/013/014/021/023/024 scoped subsets |

ADR 0021 hardens fault/commit boundaries, mixed scalar and capability FIFO
ordering, blocking scalar wait settlement and exactly-once scheduler admission.
It does not change the ABI or fixed limits. The separate terminal-order alias is
not claimed by this historical record. Subsequent work routed and checked the
queued-waiter ordering; see [implementation status](../IMPLEMENTATION_STATUS.md)
for the exact evidence and the still-unverified Ready ordering.

## K8 / Phase 8A-1 record

| ADR | Status | Related register entries |
| --- | --- | --- |
| [0022: MemoryObject backing and lifetime](0022-memoryobject-backing-and-lifetime.md) | Accepted for bounded Phase 8A-1; dynamic inventory and independent QMP verified | NEU-018; NEU-005; NEU-024 |

## K8 / Phase 8A-2 record — live Ring 3 sharing

| ADR | Status | Related register entries |
| --- | --- | --- |
| [0023: Live Ring 3 MemoryObject sharing and fault containment](0023-live-userspace-memoryobject-sharing.md) | Accepted for bounded Phase 8A-2; stopped-CPU live execution and hardware fault evidence verified | NEU-005; NEU-017; NEU-018; NEU-021; NEU-023; NEU-024 |

ADR 0023 builds directly on 0022: trusted setup attaches the same two-page object
to two roots, real Ring 3 writes/reads cross the shared backing, a read-only store
produces a genuine `#PF`, and both Node teardown orders preserve correct lifetime.
It does not accept a public MemoryObject ABI, transfer, mmap or SMP. Architecture
v0.2 and ABI 0.5 remain unchanged.

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

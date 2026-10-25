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

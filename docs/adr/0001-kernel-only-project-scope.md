# ADR 0001: Keep Neu a kernel-only project

- **Date:** 2026-10-25
- **Status:** Accepted — project responsibility boundary.
- **Related NEU decisions:** `NEU-001`; `NEU-016` (the existing out-of-scope
  compatibility boundary, whose register status is unchanged).
- **Architecture:** [Sections 1–2: mission and boundaries](../Neu_Kernel_Architecture_v0.2.md#2-mission-boundaries-and-philosophy),
  [Section 25: decision register](../Neu_Kernel_Architecture_v0.2.md#25-decision-register-seed-for-adrs).

## Context

The architecture defines Neu as the kernel on which others may build operating
environments. Building a visually complete OS demonstration would introduce
services, policy, and applications before the kernel mechanisms they depend on
are reliable. Phase 0 needs a small, reproducible boot foundation whose success
can be assessed independently of an OS ecosystem.

## Decision

Preserve the chosen kernel-only scope. Neu supplies kernel mechanisms and the
minimum infrastructure needed to validate them. Phase 0 is limited to kernel
bootstrapping, early diagnostics, a basic panic path, and build/run/smoke-test
support. Small additions must have a direct bootstrap correctness justification.

Desktop interfaces, package management, full filesystem and networking stacks,
general application runtimes, and Linux/POSIX compatibility are outside this
milestone and the project's kernel-only promise. Later tiny test Nodes or a
bootstrap Supervisor fixture may validate kernel mechanisms; that exception
does not authorize a userspace platform or introduce Nodes into Phase 0.

## Alternatives considered

- **Build a complete operating system alongside the kernel.** This would make
  kernel progress depend on application and service policy, expanding the work
  beyond the project's stated responsibility and obscuring bootstrap failures.
- **Make Linux/POSIX compatibility a kernel requirement.** This would let legacy
  OS semantics drive the native architecture. Third-party compatibility work may
  use Neu's general mechanisms, but is not a prerequisite for kernel progress.

## Consequences

- Milestones can demonstrate kernel behavior with minimal fixtures and clear
  acceptance evidence, reducing unrelated dependencies and review scope.
- Neu alone will not provide a complete user-facing OS; downstream integrators
  must supply the surrounding services, policy, and application environment.
- New work needs a kernel-mechanism or necessary validation rationale. Future
  subsystem scaffolding and ADR placeholders add no such proof.
- An exception that materially expands this boundary requires a successor ADR;
  an implementation convenience cannot silently redefine project scope.

## History

- **2026-10-25:** Recorded the already chosen `NEU-001` scope from architecture
  revision 0.2 and its application to the Phase 0 brief. No implementation or
  milestone completion is asserted.

# ADR 0002: Use Rust and x86-64 for the initial foundation

- **Date:** 2026-10-25
- **Status:** Accepted — implementation language and initial architecture.
- **Related NEU decisions:** `NEU-002` (Rust foundation only); `NEU-003`
  (x86-64 first and explicit portability boundaries).
- **Architecture:** [Section 1: chosen directions](../Neu_Kernel_Architecture_v0.2.md#1-executive-summary),
  [Section 4: hardware and portability](../Neu_Kernel_Architecture_v0.2.md#4-hardware-and-portability),
  [Section 23: illustrative organization](../Neu_Kernel_Architecture_v0.2.md#23-proposed-rust-workspace-organization).

## Context

Phase 0 needs one freestanding kernel target before work on later kernel
mechanisms. The architecture already chooses Rust and x86-64 under QEMU, while
retaining future ISA ports. The development host is Apple Silicon; choosing a
kernel architecture solely from the host would change that baseline.

Supporting several ISAs immediately would multiply entry, CPU-state, paging,
and testing work before a single bootstrap path exists. Conversely, spreading
x86-64 assumptions throughout generic logic would make future ports harder.

## Decision

Use Rust for the bare-metal kernel, with `no_std` and `no_main` where appropriate,
and x86-64 as the first kernel architecture. Use QEMU's x86-64 environment for
initial execution and validation, with the UEFI/OVMF environment specified by the
Phase 0 brief. A macOS host executable or a standalone UEFI application is not
the kernel deliverable.

Localize necessary unsafe Rust, assembly, CPU-specific entry code, and platform
I/O behind explicit, narrow boundaries. Keep generic logic independent where
practical, but implement only the backend needed now. Rust is chosen to support
clear ownership and checked interfaces; privileged code still needs documented
invariants and validation.

This record does not freeze a Rust toolchain release, target configuration,
workspace layout, or the detailed hybrid subsystem split in `NEU-002`. Select
and document compatible build details during implementation. AArch64 and RISC-V
remain future ports rather than empty modules to create in Phase 0.

## Alternatives considered

- **Use C or C++ as the primary kernel language.** These could support the
  bootstrap, but would replace the chosen Rust foundation and its ownership
  discipline without a demonstrated Phase 0 requirement for that change.
- **Start with AArch64 to match the host.** This would prioritize host convenience
  over the selected x86-64 validation platform and require a different first
  architecture implementation.
- **Implement multiple ISAs from the start.** This would consume effort on
  parallel backends before the shared abstractions have been exercised.

## Consequences

- One architecture and validation environment keep the initial bootstrap small
  enough to reason about and reproduce.
- Hosts with a different ISA need cross-compilation and x86-64 system emulation;
  the host architecture must not silently determine the Cargo kernel target.
- Rust does not remove obligations around raw pointers, boot data, inline
  assembly, CPU state, or hardware access. Unsafe boundaries need explicit
  invariants and review.
- Portability is an organization constraint, not a claim of working AArch64,
  RISC-V, physical-PC, or Apple Silicon device support. Future ports require
  their own implementation evidence and significant choices recorded in ADRs.

## History

- **2026-10-25:** Recorded the chosen Rust and initial x86-64 directions from
  architecture revision 0.2. Illustrative workspace and subsystem designs remain
  proposals; no compiler, build, or boot result is asserted.

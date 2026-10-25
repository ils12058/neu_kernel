# ADR 0003: Reuse an external bootloader

- **Date:** 2026-10-25
- **Status:** Accepted — external-bootloader boundary only.
- **Related NEU decisions:** `NEU-004` (the chosen external-loader portion).
- **Architecture:** [Section 2.2: scope exclusions](../Neu_Kernel_Architecture_v0.2.md#22-what-neu-will-not-build),
  [Section 4.4: bootloader boundary](../Neu_Kernel_Architecture_v0.2.md#44-bootloader-boundary),
  [Section 25: decision register](../Neu_Kernel_Architecture_v0.2.md#25-decision-register-seed-for-adrs).
- **Related ADR:** [0004: proposed Limine integration](0004-limine-phase-0-integration.md).

## Context

The kernel needs a reliable firmware-to-kernel handoff to make progress on early
execution and diagnostics. Developing and maintaining a custom loader would add
a separate project before the kernel can be exercised. The architecture keeps
experimental NeuBoot as prior work and explicitly avoids making it a restart
dependency. `NEU-004` separates that chosen boundary from the proposed Limine
implementation.

## Decision

Use an external bootloader to load and enter the freestanding Neu kernel. Do not
develop a custom bootloader, import NeuBoot, or make NeuBoot maintenance a
prerequisite for Phase 0.

Confine bootloader-specific data handling to a narrow adapter. Validate required
responses, pointers, ranges, and lifetime assumptions before exposing internal
boot information to the rest of the kernel. Reusing a loader reduces loading
work; it does not remove the kernel's responsibility for its execution stack,
required mappings, CPU conditions, and safe handling of initialization failure.

This accepts the external-loader direction only. The Limine-specific integration
proposal is recorded separately in ADR 0004 so unresolved compatibility choices
are not presented as part of this acceptance.

## Alternatives considered

- **Resume NeuBoot or write another custom bootloader.** This would make kernel
  progress depend on separate firmware, image-loading, and handoff engineering,
  contrary to the kernel-only boundary and the Phase 0 brief.
- **Maintain a Neu-specific UEFI loading stub.** Even a small stub leaves Neu
  responsible for the loader-to-kernel transition and compatibility work. There
  is no identified Phase 0 requirement that justifies owning that extra layer.

## Consequences

- Bootstrap work can focus on kernel entry and observable diagnostics while
  reusing an external loading mechanism.
- The build and boot workflow gains an external dependency. Its release,
  protocol compatibility, artifact provenance, and handoff assumptions must be
  pinned and documented when integration is implemented.
- The adapter must preserve boot-data lifetimes and prevent premature reuse of
  loader-owned memory. External-loader data is not automatically safe to use.
- Loader-specific assumptions stay localized, making a later replacement
  reviewable. A change in loader choice or handoff contract needs an ADR update
  or successor with its rationale and compatibility impact preserved.

## History

- **2026-10-25:** Recorded the already chosen external-loader portion of
  `NEU-004` from architecture revision 0.2. Limine adoption details retain their
  separate proposal status; this acceptance does not claim a working boot path.

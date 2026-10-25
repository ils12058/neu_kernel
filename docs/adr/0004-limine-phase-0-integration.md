# ADR 0004: Integrate Limine for Phase 0

- **Date:** 2026-10-25
- **Status:** Proposed — Limine-specific integration contract remains unresolved.
- **Related NEU decisions:** `NEU-004` (the proposed Limine portion);
  `NEU-025 / BOOT` (the open integration plan; not resolved by this record).
- **Architecture:** [Section 4.4: preferred loader and adapter](../Neu_Kernel_Architecture_v0.2.md#44-bootloader-boundary),
  [Section 22.3: preliminary integration objective](../Neu_Kernel_Architecture_v0.2.md#223-integration-objective-and-dependency-order-preliminary),
  [Section 25: decision register](../Neu_Kernel_Architecture_v0.2.md#25-decision-register-seed-for-adrs).
- **Related ADR:** [0003: accepted external-bootloader boundary](0003-external-bootloader-boundary.md).

## Context

The architecture prefers Limine but leaves its adoption and exact protocol
version to implementation. The Phase 0 brief calls for Limine with x86-64 QEMU
and UEFI/OVMF. Limine is therefore the intended implementation candidate, rather
than a reason to reopen the accepted external-loader boundary.

No Limine release, boot protocol revision, Rust binding, or compatible boot
artifact has been selected in this documentation change. Those choices affect
the kernel handoff and reproducibility and need a concrete integration contract.

## Decision

Propose Limine integration for Phase 0 through the narrow boot adapter established
in ADR 0003. Following the preferred loader and the Phase 0 brief avoids starting
a separate bootloader project and concentrates work on the first kernel entry.
Request only boot information genuinely needed by the milestone, validating it
before use. Do not couple later kernel abstractions to Limine response types.

Resolve the following as part of implementing and accepting this proposal:

- Select and pin a compatible Limine release and boot protocol/base revision,
  plus a Rust binding version if a binding is used. Explain compatibility
  constraints and artifact provenance; do not rely on an unpinned latest release.
- Define the required responses, entry conditions, initial stack/mappings, and
  boot-data ownership/lifetime rules for that selected protocol.
- Document the matching x86-64 UEFI/OVMF boot artifact and reproducible workflow,
  with actual timeout-bounded QEMU smoke-test evidence in implementation docs.

Record explicit acceptance and the resolved rationale in History when those
choices are settled. Architectural acceptance and successful integration tests
are distinct facts: tests establish observed behavior, not acceptance by
themselves. This proposal does not settle the later boot-to-first-Node design in
`NEU-025` or require Phase 0 to implement Nodes, a scheduler, or full NeuKMon.

## Alternatives considered

- **Select another external bootloader.** This would preserve ADR 0003's boundary
  but require a different handoff and compatibility evaluation. Departing from
  the preferred candidate needs a concrete benefit or compatibility blocker;
  reconsider with evidence if integration exposes one.
- **Leave loader choice entirely unspecified.** This retains flexibility but
  gives Phase 0 no concrete integration target. Naming Limine while keeping its
  unresolved contract explicit provides a bounded next implementation step.
- **Reuse NeuBoot or build a custom loader.** Rejected by ADR 0003 and the Phase 0
  scope; this proposal does not create an exception to that boundary.

## Consequences

- Phase 0 has a specific integration candidate and explicit questions to resolve
  before its boot workflow can be considered reproducible.
- Compatibility work remains necessary. This ADR supplies no guessed version,
  protocol constants, ready-to-run commands, or claimed successful boot.
- Loader releases and protocol changes can affect kernel entry and boot-data
  handling. Record significant contract changes and preserve the earlier
  rationale through the ADR lifecycle.
- The register's partially chosen/proposed `NEU-004` status and open `NEU-025`
  remain intact. Choosing an external loader does not ratify all bootstrap or
  subsystem proposals.

## History

- **2026-10-25:** Recorded the Limine integration proposal from architecture
  revision 0.2 and the intended loader in the Phase 0 brief. Exact release and
  protocol choices remain unresolved; no implementation evidence is asserted.

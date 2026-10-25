# ADR 0006: Own exception descriptors and halt on privileged faults

- **Date:** 2026-10-25
- **Status:** Accepted — K1 / Phase 1A exception foundation.
- **Related NEU decisions:** `NEU-003` (x86-64 backend), `NEU-023 / DIAG`
  (privileged fatal path), `NEU-024 / TEST` (focused local fault tests only).
- **Architecture:** [Section 20](../Neu_Kernel_Architecture_v0.2.md#20-fault-handling-panic-and-diagnostics--neu-diag-001)
  and [Section 25](../Neu_Kernel_Architecture_v0.2.md#25-decision-register-seed-for-adrs).
- **Related ADR:** [0005: current boot and memory contract](0005-boot-memory-ownership-and-frame-allocation.md).

## Context

Phase 0 kept the loader GDT and had no kernel IDT or TSS. An exception could
therefore triple fault instead of producing useful diagnostics. Reliable double
fault delivery needs a known-good emergency stack and a valid kernel-controlled
TSS/GDT, rather than assumptions about loader descriptor layout or ownership.

The pinned stable Rust target avoids FP/SIMD and uses the System V ABI. Exception
entry still needs explicit handling of CPU frames and hardware error-code
differences before calling ordinary Rust code.

## Decision

Install a private GDT, 64-bit TSS, and IDT in reserved kernel storage. Reload
segment selectors, load TR, and provide a dedicated, aligned **32 KiB IST1 stack
for #DF**. The existing 64 KiB kernel stack serves other current exception paths.
Do not enable external interrupts, scheduling, or userspace transitions.

Use assembly stubs for vectors 0–31 with the hardware error-code differences
normalized explicitly. Preserve 15 GPRs and the CPU RIP/CS/RFLAGS/RSP/SS frame;
capture CR2 before entering Rust. Align the Rust call stack and preserve the
complete normalized context in a static fault record. Other IDT vectors share
a fatal unexpected-interrupt fallback; this is not an interrupt-controller API.

All current privileged exceptions, including #BP, are fatal. Emit at most 512
bytes through the allocation-free emergency serial path, then use the existing
`cli`/`hlt` loop. A guard bounds nested reporting. Preserve the separate Rust
panic handler; no exception path attempts unwinding, resumption, or NeuKMon.

Test #UD with `ud2`, #GP with an out-of-bounds GDT selector, and #BP with `int3`.
For #PF, request HHDM only in the test image, verify each inspected table page is
retained bootloader RAM, and inspect the actual four-level mapping. Attempt a
write only after proving the target static is present and read-only with CR0.WP
enabled. No page-table entries are changed.

For a genuine #DF, invalidate the test image's #GP gate selector and then cause
a real #GP. The contributory fault during delivery invokes #DF through intact
IST1. A software interrupt to vector 8 is not used or treated as equivalent.
Require vector 8/error 0 and the handler RSP within the IST allocation. Do not
treat the saved #DF RIP as a recoverable instruction location.

QEMU tests require the expected vector/error, RIP where architecturally useful,
CR2 for #PF, the correct handler stack, and two QMP observations of controlled
halt. Unexpected reset, triple fault, shutdown, or emulator exit cannot pass.

## Alternatives considered

- **Reuse/extend the loader GDT/TSS.** This would depend on unowned descriptor
  layout and writable lifetime assumptions. A small private table makes them
  explicit while retaining loader memory conservatively.
- **Use a nightly interrupt ABI or a descriptor library.** Neither is needed for
  these fatal stubs; assembly preserves the stable toolchain and exact layout.
- **Handle #DF on the ordinary stack.** A stack-related fault may make that stack
  unusable. A separate IST provides an independent delivery stack.
- **Resume after privileged faults or enter a monitor.** Correct recovery needs
  invariants and execution machinery outside Phase 1A. Fatal handling is easier
  to assess than continuing from potentially corrupted kernel state.

## Consequences

- Tested exception frames and emergency stack selection now have concrete
  evidence. This does not establish recovery from arbitrary corruption.
- No stack guard pages, general NMI/machine-check hardening, SIMD context,
  userspace fault containment, or interrupt-driven execution is promised.
  Descriptor installation is not an atomic transition against NMI; that window
  and hardware-specific fault cases remain unverified.
- The #DF test proves genuine exception delivery and IST execution, not safe
  recovery from arbitrary stack overflow. All handlers terminate execution.
- The read-only test walker relies on the selected Limine mapping contract and
  remains test-only. Page-table ownership and controlled mappings remain Phase 1B.

## History

- **2026-10-25:** Accepted under the Phase 1A request. The current contract in
  ADR 0005 replaces ADR 0004's loader-GDT retention with this private descriptor
  foundation while keeping all bootloader-reclaimable RAM reserved. Results are
  recorded in [implementation status](../IMPLEMENTATION_STATUS.md).

Reference: [Intel SDM](https://www.intel.com/content/www/us/en/developer/articles/technical/intel-sdm.html),
Volume 3A, interrupt/exception handling, error codes, TSS and Interrupt Stack Table.

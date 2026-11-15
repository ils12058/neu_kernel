# ADR 0033: Eager x87/MMX/SSE2 context ownership and current-Thread TLS

Date: 2026-11-15.

Status: accepted for bounded local x86-64/QEMU completion;
verification is recorded in [CPU-state completion](../CPU_STATE_COMPLETION.md).

Related NEU decisions: scoped portions of NEU-003, NEU-005, NEU-011, NEU-018,
NEU-020, NEU-021, NEU-023 and NEU-024. Architecture v0.2
[execution, scheduling and security](../Neu_Kernel_Architecture_v0.2.md)
remain the design baseline; their complete register entries are not accepted
by this bounded implementation.

## Context

K9 through the PCI/DMA completion preserve integer contexts and deliberately
disable unsaved floating-point state. A program using floating-point math or a
runtime TLS register needs CPU ownership beyond the existing GPR frame.
SMP scheduling, a returning interrupt, blocked execution and Node cancellation
must all obey that same ownership; a lazy save could expose a previous Thread's
register values before a trap arrives.

The user authorized a complete bounded FPU/SIMD/TLS implementation after
PCI/IOMMU/DMA. This adds no numbered roadmap milestone and does not move ELF,
TLS memory allocation or language-runtime policy into the kernel.

## Decision

Use eager FXSAVE64/FXRSTOR64 transport for x87/MMX and all sixteen SSE/SSE2 XMM
registers, with an aligned 512-byte image per Thread and CPU-owned transport.
Bind every saved image to full Thread, Node, CR3 and AddressSpace identities.
Reject a second loaded Thread on a CPU, a second CPU owner, stale identity,
invalid MXCSR/TLS or unbalanced retirement before publishing a state change.

Capture user FP and FS_BASE in assembly before Rust at native entry, supported
user exceptions and all user-origin timer/IPI/device interrupts. Restore the
kernel's original FP image, FS selector/base and controls before handlers.
Reinstall the captured user state on a direct IRQ return. At a scheduling
boundary, save the owned image under the existing short shared transaction;
blocked continuations and migration reuse it. Cancelled queued/blocked siblings
also retire their images when the Node's fault transition completes.

Initialize every user-visible register and image byte. Clear saved image,
identity, TLS, counters and live CPU transport at retirement; retain only the
generation watermark needed to reject stale identities. Test-only diagnostic
snapshots retain historical copies for the independent stopped-VM oracle.

Check every participating CPU for x87/MMX/FXSR/SSE/SSE2 and matching normalized
MXCSR masks before constructing the runtime. Keep OSXSAVE and FSGSBASE disabled
in userspace. This baseline does not need AVX for ordinary execution and cannot
expose additional registers without implementing their save/lifetime contract.

[ABI 0.9](../abi/experimental-v0.9.md) exposes only current-Thread FS_BASE SET/GET.
Accept full-width lower canonical addresses, validate all reserved arguments,
normalize the user FS selector to zero on return, and let ordinary user mappings
govern FS-relative memory access. Kernel mechanisms do not allocate TLS memory
or define a language-specific thread-control block.

## Alternatives considered

- Lazy `#NM` ownership leaves a window for speculative reads of another
  Thread's register bank and adds ownership transitions to an exception path.
  Eager capture fits the existing bounded scheduler and avoids that window.
- XSAVE/AVX would add XCR0 negotiation, variable layouts and more state to
  initialize, save and scrub. Defer it behind a separate execution contract.
- A Node-wide TLS base would incorrectly combine sibling Thread runtimes.
  Bind the register to the Thread while retaining Node-owned memory mappings.
- Importing ELF TLS templates into this change would add loader/runtime policy.
  The kernel provides the register and isolation mechanism; userspace supplies
  storage and runtime layout.

## Consequences

The fixed two/four-CPU, two-Node, three-Thread model now has an extended-state
execution profile and a combined DMA profile. Earlier profiles keep their
integer guards, ABI versions and masks. The kernel remains nonpreemptible
outside its existing controlled interrupt windows.

[The QMP protocol](../testing/cpu-state-qmp.md) requires actual stopped CPU
register banks, physical startup/final images, real normalized native/fault/IRQ
frames, complete per-Thread image/FS transitions, raw ownership and exact final
kernel/resource restoration. Physical hardware, public factories, general
scheduling policy, ELF/TLS runtime construction and AVX/XSAVE are not accepted
by this decision.

The stock TCG backend validates SIMD state/status but does not deliver
unmasked `#XM`. An FXSR-disabled CPU also cannot reach Neu through the
pinned OVMF. These remain explicit verification limits; no synthetic
exception or firmware failure is counted as a successful kernel test.

## History

- 2026-11-15: implement the user-authorized post-PCI/DMA FPU/SIMD/TLS slice on
  `codex/fpu-simd-tls`, based on `c9b370ffa47ed3ef4009e0774a207d94516c2b74`.
- 2026-11-15: accept 231 runnable QEMU profiles, 16 exact-image repeats,
  five 385-test host configurations, 64 observer regressions and the frozen
  source/image/conservation audit; retain the documented hardware limits.

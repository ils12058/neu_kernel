# x86-64 FPU/SIMD/TLS completion contract

Status: **accepted for bounded local x86-64/QEMU completion**, 2026-11-15.
Base: accepted PCI/IOMMU/DMA commit
`c9b370ffa47ed3ef4009e0774a207d94516c2b74`.
Implementation branch: `codex/fpu-simd-tls`. This work assigns no new K number.
Architecture v0.2, prior ABI profiles and dated completion records remain.

## Implemented behavior

Every extended-state Thread owns an initialized, aligned 512-byte FXSAVE64
image containing x87/MMX state and all sixteen SSE/SSE2 XMM registers. The image
is bound to complete Thread, Node, CR3 and AddressSpace identities, including
generations. A Thread has one CPU owner, and a CPU has one loaded Thread.
Saved copies, live transport and generation reuse obey the same lifecycle.

The assembly bridge captures user FP and FS_BASE before Rust on native entry,
timer/IPI/device IRQ and supported user exceptions. It restores the original
kernel FP image, FS selector/base and CR0/CR4 before handlers execute. Direct
IRQ returns restore the captured user state. Scheduling, real Channel blocking,
resumption and CPU1-to-CPU3 migration preserve the exact owned image and TLS.
Node faults also scrub cancelled queued/blocked siblings after their CPU owners
release. Retirement clears image bytes, identity, FS_BASE and counters; only
the stale-generation watermark remains.

[Experimental ABI 0.9](abi/experimental-v0.9.md) adds current-Thread TLS SET
(`0x29`) and GET (`0x2a`). Bases and all reserved arguments are checked at full
width. Zero and lower canonical addresses are accepted; upper/noncanonical
addresses and reserved values fail before mutation. Equal TLS VAs in unrelated
Nodes use different physical backing; siblings use separate offsets in their
shared AddressSpace. Ordinary user page tables still govern every TLS access.
The conventional mask is `0x9000018ff`; combined DMA uses `0x900001fff`.

Every participating CPU must advertise x87/MMX/FXSR/SSE/SSE2 and matching
normalized MXCSR masks before any extended-state Node is constructed. Eager
userspace enables CR0 MP/NE and clears EM/TS; CR4 enables OSFXSR/OSXMMEXCPT.
OSXSAVE and FSGSBASE stay clear. Real AVX and WRFSBASE attempts produce contained
`#UD` faults. Earlier profiles retain their integer guards, ABI versions and
NotSupported results for the new operations.

## Accepted local validation

All commands use the frozen source and observer inputs. Per-guest deadlines
remain 30 seconds. Normal profiles retain 512 MiB; stress retains 1 GiB.

| Validation | Measured result | Retained artifact |
| --- | --- | --- |
| Rust host tests | 385/385 in default, authority, device, DMA and combined CPU-state/DMA configurations | `build/cpu-state-full-test.log`, `build/cpu-state-host-*.log` |
| Python observers | 64/64, including ten FP/TLS mutations and two new journal regressions | `build/cpu-state-check.log`, `build/cpu-state-full-test.log` |
| Complete QEMU matrix | 231/231 registered runnable profiles | `build/cpu-state-full-test-results.json`, `build/cpu-state-full-test-evidence/` |
| Exact-image repeats | All 16 new runnable profiles pass again without rebuilding | `build/cpu-state-repeat-results.json`, `build/cpu-state-repeat-evidence/` |
| Formatting and lint | All 231 registered selections and host-library lint pass | `build/cpu-state-check.log` |
| Toolchain and default build | Doctor/build pass; default image equals the full-run boot image | `build/cpu-state-doctor.log`, `build/cpu-state-build.log` |
| Frozen evidence audit | 111 inputs, image/firmware hashes, all 59 scheduled profiles, full FP/TLS continuity and resource restoration | `build/cpu-state-validation-summary.json` |
| Prior-file preservation | All 1166 inventoried K11/PCI-DMA evidence files and the unrelated file match their original hashes | `build/cpu-state-prior-files.json`, `build/cpu-state-validation-summary.json` |

Source aggregate SHA-256:
`8b28e4df05cff150f18d4028503fe8b97f6397662e12f23913a5b8943ad90b6c`.
Default kernel SHA-256:
`96ec7cf98b9401c0610966093293d62cdb6622359c37f052b779b63f91c5564c`.
Longest full-run guest: 28.468 seconds.
Longest repeated guest: 15.204 seconds.
No project QEMU process remains.

## Independent evidence

The stopped-VM observer reads actual FPR/XMM banks, FCW/FSW/FTW/MXCSR, FS
selector/base, controls and owned storage. It computes expected values outside
the guest and compares all eight x87 values, eight 64-bit MMX operands and sixteen
XMM registers. Startup images are zero-initialized. Every owned entry/departure
is correlated with its actual normalized frame; per-Thread image and TLS streams
are reconstructed across native returns, interrupts, blocking and migration.
Serial success and guest policy counters cannot replace these reads.

Real x87 `#MF` includes unmasked divide-by-zero status. The SSE-status fixture
executes unmasked DIVSS, preserves its actual infinity/status through a native
round trip, then uses real `#UD` for containment. Faulted Node A's running and
queued/blocked siblings retire while Node B preserves its FP/TLS storage.
The kernel-sentinel profile uses distinct nonzero x87/XMM banks and supervisor
FS state on all four CPUs, then verifies exact final restoration.

The combined profile retains the complete independent PCI/VT-d/DMA model:
typed authority, actual transfer bytes/translations, real IRQ/EOI, blocking,
invalidation and host/device scrub before reassignment. Every TLS journal row
must match its actual native entry and returning frame. All 59 scheduled
profiles restore their original allocator bitmap/table inventory with no final
Thread/CPU owner, live root, backing, handle, grant, queued transfer or wait.

[The QMP protocol](testing/cpu-state-qmp.md) lists all 16 runnable profiles,
exact layouts, checks and retained platform limits.

## Development corrections and retained limits

- The first combined DMA image exhausted its 64 table slots while preparing
  Node B. Its payload was 4,025 bytes and fit the existing one-page limit;
  two 2,048-row extended streams occupied 18 MiB of diagnostic storage. The
  combined fixture now has 96 slots and owns 75 tables at preparation.
  Other profiles retain 64 slots. The original failure and image measurements
  remain in `build/cpu-state-dma-capacity-failure/`.
- The combined device oracle initially did not account for TLS journal rows.
  It now reconciles all rows and returning frames; regressions reject missing,
  fabricated and cross-Thread TLS results. The disabled build also exposed a
  compile-time feature-gating error, fixed with actual conditional compilation.
- Stock QEMU 11 TCG records unmasked SSE status without delivering `#XM`.
  Kernel vector-19 gates/control setup and portable containment are verified;
  actual `#XM` delivery remains **not validated**. No exception is synthesized
  and `#UD` is not presented as `#XM`.
- The attempted FXSR-disabled CPU does not reach Neu through the pinned OVMF.
  Its empty transcript and stopped firmware CPUs are retained separately;
  firmware behavior is excluded from the runnable pass count. QEMU rejects
  changing this feature after realization. Actual SSE2- and MMX-disabled CPUs
  do reach Neu and prove kernel refusal with zero installed/entered Threads.
  Portable policy tests cover all five missing required bits. Kernel refusal
  on an FXSR-deficient boot path remains **not validated**.

Earlier focused failures and snapshots remain in the development artifacts.
The accepted full run and exact-image repeats are retained separately.

## Boundary

This acceptance uses x86-64 QEMU 11 TCG/q35, two/four CPUs, two Nodes, three
Threads and fixed pools. Kernel FP/SIMD ownership and the current-Thread FS_BASE
mechanism are implemented. TLS allocation, ELF `PT_TLS`, dynamic linking,
thread-control-block layout and language runtimes belong to userspace.
AVX/AVX-512/XSAVE, public factories, general driver loading, broader scheduling,
kernel preemption, physical hardware and hosted CI remain outside this scope.
This record completes this bounded slice; it does not claim a mature general OS.

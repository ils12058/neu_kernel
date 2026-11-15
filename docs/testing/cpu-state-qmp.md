# Stopped-VM x87/MMX/SSE2/TLS acceptance

`tools/cpu_state_qmp.py` extends the existing scheduled-SMP observer. It imports
no kernel CPU-state policy. Every snapshot stops all vCPUs, reads actual CPU
registers and raw memory, and resumes only after completing its checks. The
existing per-guest 30-second deadline, normalized frames, GPR/stack guards,
capability/queue accounting and exact allocator/table restoration remain.

## Registered cases

| Case | CPUs | Required additional evidence |
| --- | --- | --- |
| `cpu-state-2` | 2 | Three distinct x87/MMX/XMM banks, same-Node concurrency, native/timer capture and TLS isolation |
| `cpu-state-4` | 4 | Separate B owner and an idle fourth CPU; complete kernel restoration |
| `cpu-state-migrate` | 4 | Actual A2 FP/TLS banks on CPU1 and CPU3 plus exact saved-image continuity |
| `cpu-state-kernel` | 4 | Restoration on all four CPUs with distinct nonzero kernel x87/XMM banks, supervisor FS selectors and full-width kernel FS bases |
| `cpu-state-blocked` | 2 | One real scalar receive blocks and resumes with its owned FP/TLS image |
| `cpu-state-blocked-4` | 4 | Blocked B owner actually enters STI/HLT and later executes its restored FP/TLS loop |
| `cpu-state-negative` | 2 | Three invalid full-width bases and all eleven reserved SET/GET arguments fail on every Thread; zero/highest-lower bases succeed |
| `cpu-state-x87-fault` | 4 | Real `#MF` at FWAIT with unmasked divide-by-zero status; stop A's sibling and preserve B |
| `cpu-state-sse-status` | 4 | Actual unmasked DIVSS status/result survive a native round trip, followed by real `#UD` containment |
| `cpu-state-avx-denied` | 4 | Real `#UD` at VXORPS with OSXSAVE clear and unchanged owned FP/TLS state |
| `cpu-state-fsgsbase-denied` | 4 | Real `#UD` at WRFSBASE with FSGSBASE clear and unchanged TLS |
| `cpu-state-stress` | 4 | Ninety BSP ticks, migration and complete context streams; retains the existing 1 GiB stress setting |
| `cpu-state-dma` | 4 | The full positive DMA/IRQ/retirement protocol together with distinct user x87/XMM/TLS banks |
| `cpu-state-disabled` | 2 | ABI 0.5 and integer guards; both TLS calls return NotSupported on every Thread |
| `cpu-state-no-sse2` | 2 | Actual `max,sse2=off` CPU refusal with zero owned state and no architectural fault |
| `cpu-state-no-mmx` | 2 | Actual `max,mmx=off` refusal; the advertised baseline never silently omits MMX |

The ordinary payload executes SSE2 floating-point addition, runs an eight-value
x87 stack and changes all sixteen XMM registers to distinct Thread patterns.
Its busy loop performs reversible x87 addition/subtraction and XMM XORs. The
observer classifies the exact stopped RIP to distinguish a transient arithmetic
value from corruption. MMX is checked through two consecutive native boundaries
before EMMS: eight actual 64-bit operands, tags and complete captured bytes must
survive restoration. The x87 exponent portion is not treated as a MMX operand.

A blocked receiver uses a fixed 500-million-TSC-cycle payload wait after waking
to make the actual FP/TLS loop observable. Kernel scheduling policy and the
overall deadline are unchanged. The dedicated kernel-sentinel fixture seeds
FP/FS before its baseline and leaves original CR0/CR4 intact. Neither fixture
is a public syscall or a runtime policy mechanism.

## Independent evidence

- **Baseline:** Actual HMP FPR/XMM/FCW/FSW/FTW/MXCSR, FS selector/base and CR0/CR4
  match CPU-owned baseline FXSAVE bytes. All participating CPUID snapshots have
  the required features and compatible normalized MXCSR masks. The dedicated
  sentinel case also has independently computed, CPU-specific kernel values.
- **Prepared:** Raw policy slots contain exact initialized images, zero TLS and
  counters, complete Thread/Node/CR3/AddressSpace identities and no CPU owner.
  The ordinary root observer independently checks user/supervisor permissions,
  private backing, guarded stacks and the exact linked payload bytes.
- **Execution:** Actual stopped Ring 3 register banks match each Thread's
  independent patterns. FS points into the correct TLS storage, eager controls
  are set and OSXSAVE/FSGSBASE clear. Raw ownership excludes duplicate CPUs or
  Threads. Actual Rust handler probes show the restored kernel FP/FS/controls.
- **Boundaries:** Extended rows correlate one-for-one with real native/IRQ/fault
  frames, including full Thread/Node/root/CPU identity. Every captured kernel
  image equals that CPU's baseline; user images are read from the actual
  aligned CPU transport. All required hardware IRQs have existing ISR/EOI proof.
- **Continuation:** Reconstruct each Thread's ordered entries and departing
  boundaries across its CPU owners. Each loaded image is byte-for-byte equal to
  the previous captured image. FS changes only for a successful, independently
  validated SET; native return status/scalar matches the current-Thread model.
  Blocking and CPU1-to-CPU3 migration retain the same image and TLS identity.
- **Physical user evidence:** Read untouched startup FXSAVE destinations, final
  images, SSE2 math results and actual TLS words from separate physical pages.
  Every initial register is zero. Siblings have separate TLS offsets, and equal
  TLS VAs in unrelated Nodes have different backing. Fault cases preserve the
  healthy Node's bank/storage and require actual normalized exception frames.
- **Retirement:** Read actual saved slots and CPU transport bytes after all
  Threads terminate. Images, identities, TLS, counters and CPU ownership are
  zero/empty; generation watermarks remain one. Final hardware FP/FS/CR0/CR4
  and baseline/final FXSAVE bytes agree exactly, along with the ordinary raw
  allocator bitmap, roots, pins, handles, waits and LAPIC accounting.

ABI 0.9 over DMA runs the existing independent native authority, hardware
translation, transfer, IRQ and retirement models with its exact feature mask.
TLS calls have their own independent model and actual native-frame checks;
the combined profile also reconciles every TLS journal row with its actual
entry and returning frame. Missing or fabricated journal rows cannot pass.
A serial success line
alone cannot accept any scheduled case or unsupported-CPU control.

## Storage layout

Version-one `neu_cpu_state_layout` locates 512-byte images aligned to 16 bytes,
592-byte Thread slots and 1152-byte extended rows. Extended CPU blocks have a 3008-byte
stride, the original 672-byte bridge offset and a 32-word bridge. Kernel/user/
baseline/final images begin at offsets 928/1440/1952/2464. Earlier profiles
retain their 832-byte blocks, 16-word bridges and original trace capacities.
CPU-only profiles use 256 rows; the combined DMA profile retains 2048.
The combined diagnostic image uses 96 page-table slots to map its two full FP
streams privately in both Node roots; other profiles retain their 64-slot
budget. This capacity changes diagnostic storage, not DMA authority or the
30-second deadline. Every allocated table remains independently accounted for.

The exported addresses locate raw storage; live counts and policy pass bits
do not replace reading its bytes. Reserved image bytes are initialized and
checked. Test-only diagnostic rows retain historical images for this oracle;
live ownership and transport are scrubbed independently.

## SSE exception-delivery limit

Stock QEMU 11 TCG records SSE exception flags and arithmetic results but does
not implement delivery of unmasked SIMD exceptions. The initial DIVSS probe
produced infinity and MXCSR=`0x1d84` with OSXMMEXCPT enabled, then continued
instead of raising `#XM`. Its failed run and raw stopped-VM diagnosis remain in
`build/cpu-state-sse-debug.log` and `build/cpu-state-sse-debug.json`.
QEMU's [MXCSR helpers](https://github.com/qemu/qemu/blob/v11.0.0/target/i386/tcg/fpu_helper.c)
and [SSE helpers](https://github.com/qemu/qemu/blob/v11.0.0/target/i386/ops_sse.h)
confirm the flag-only behavior; exact copies are retained under `build/`.

The kernel installs the architectural `#XM` gate, sets OSXMMEXCPT, and shares
the capture/containment path with the tested user exceptions. Portable tests
cover the vector-19 lifecycle. Actual hardware `#XM` delivery remains **not
validated** on this backend. The registered SSE-status case proves its real
status/result, owned restoration and subsequent actual `#UD` cleanup. It does
not synthesize a vector or claim that `#UD` is `#XM`.

`tools/test_cpu_state_qmp.py` mutates masks, reserved bytes, identities,
generations, balanced ownership, retired/new images, every x87/XMM register,
TLS arguments and actual-register availability. These mutation regressions
prevent a weaker observer from accepting synthesized or incomplete evidence.

## Unsupported FXSR firmware limit

The attempted `max,fxsr=off` control does not reach Neu: the pinned x86-64 OVMF
remains in firmware with an empty kernel serial transcript. Single-CPU,
XSAVE-disabled and SSE-disabled variants behave the same way. QEMU also rejects
changing `fxsr` after CPU realization, so there is no accepted post-firmware
feature mutation. The stopped CPU/QOM records and variants remain in
`build/cpu-state-fxsr-firmware-debug.json` and
`build/cpu-state-fxsr-firmware-variations.json`; the original failed control is
retained with the development evidence.

The unbootable FXSR case is excluded from the runnable acceptance matrix;
firmware behavior is not counted as a successful kernel refusal. Portable policy
tests reject missing x87/MMX/FXSR/SSE/SSE2 before installation. The registered
`max,sse2=off` and `max,mmx=off` controls independently verify genuine kernel
CPUID refusal, zero owned state and no user entry on both participating CPUs.
Kernel refusal on an actual FXSR-deficient boot path remains **not validated**.

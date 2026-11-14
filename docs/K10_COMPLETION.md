# K10 completion contract

Started 2026-11-14 from `c3ced96c31a14420964f73bc0e6e7cbffeafe1d8`
on `codex/k10-advanced-authority`. **Completion accepted 2026-11-14** after
the complete legacy/new matrix, focused repeats and retained-evidence audit
passed for the bounded scope below.

K10 implements architecture v0.2 sections 6, 8, 15 and 22: capability
derivation without rights amplification, subtree revocation, and safe mapping
invalidation under concurrent execution. It retains K9's x86-64/QEMU target,
four-CPU/two-Node/three-Thread limit, fixed pools, integer contexts and
IRQ-disabled kernel transactions. General object factories and K11 devices
remain separate work.

## Authority contract

- Explicit DERIVE and REVOKE rights are independent of object access rights.
  Legacy grants acquire neither right implicitly. Derivation installs one
  fresh Node-local handle with a subset of the source rights; cross-Node
  delivery continues to use the existing atomic Channel move.
- A generation-checked lineage identifies each independently granted root,
  descendant, queued transfer, pending operation and authorized mapping.
  Closing or moving a handle preserves descendants and existing mappings.
- Revoke removes the source grant's strict descendants. It preserves the
  source itself, unrelated branches outside that subtree and independent
  grants to the same object. Completed revocation permits new derivation from
  the surviving source. Reused storage cannot resurrect an older lineage.
- A revocation first prevents new descendant use, derivation, delivery and
  mapping. It purges affected handles and queued transfers, cancels affected
  pending operations and removes every affected mapping. Its successful
  return occurs only after all affected CPUs have invalidated translations.
- Mapping authorization keeps its lineage and backing alive after handle
  close. Leaf removal retains mapping/backing ownership until an epoch-bound
  hardware shootdown completes. A timeout fails closed and retains storage.
  Revocation cannot undo data already copied by an authorized recipient.
- Ordinary rejected operations do not change rights, handles, queues, mappings
  or reference ownership. Generation/capacity exhaustion is a defined error.
  Unexpected corruption after invalidation starts is fail-stop.

## Required completion evidence

1. Host tests of rights subsets, independent and nested grants, source close,
   atomic moves, stale generations, pool exhaustion and reference conservation.
2. Concurrent operation tests covering derive/use/close/move/revoke, queued
   transfers, blocked Channel operations and Node termination.
3. Real Ring 3 requests using an explicitly versioned experimental ABI; older
   ABI profiles retain their feature masks and reject the new operations.
4. Two/four-CPU QEMU execution with real shared MemoryObject mappings, actual
   remote invalidations, rejected stale access and surviving independent grants.
5. An independent stopped-CPU observer checks production lineage, handle/queue
   ownership, actual page tables, CPU invalidation acknowledgements, and exact
   allocator/table restoration. Serial success strings alone are insufficient.
6. The complete legacy/new host and QEMU matrix, formatting, every-feature
   lint and default build pass. Retained results identify the tested sources
   and kernel images; docs distinguish this accepted scope from later work.

## Accepted completion evidence — 2026-11-14

| Contract | Verified result |
| --- | --- |
| Rights and lineage | Explicit management rights, exact subsets, nested/independent grants, close/move lifetime, fresh generations and inert exhaustion failures |
| Concurrent operations | Production-lock host transactions, queued delegation, send/receive wait cancellation, pre-sweep settlement rejection and Node termination |
| Real ABI | Ring 3 CAP_DERIVE/CAP_REVOKE with ABI 0.6; every Thread in the ABI 0.5 rejection image receives NotSupported for both calls |
| Mapping invalidation | Four affected mappings per active authority case; all online CPUs observed after actual INVLPG/CR3 and before ACK; independent B mappings/data survive |
| Ownership and cleanup | All 20 scheduled SMP profiles restore the exact allocator bitmap/table inventory; zero final user roots, references, pins, grants, handles, mappings, queued authority or waits |
| Full regression | 324 Rust host tests in each of default and authority configurations, 29 Python observer regressions, all 189 registered QEMU executions, five focused repeats, formatting, lint and default build |

The eight new cases are `smp-authority-2`, `smp-authority-4`,
`smp-authority-queued`, `smp-authority-close`, `smp-authority-wait`,
`smp-authority-stress`, `smp-authority-terminate` and `smp-authority-disabled`.
[The observer protocol](testing/authority-qmp.md) defines their exact execution
and evidence requirements. Full acceptance includes 96 required page/CPU
invalidation windows. Four-CPU revoke, queued delegation, wait cancellation,
repeated derivation and requester termination pass again with the same images,
adding 80 required windows. No per-case deadline was increased: it remains
30 seconds. The longest full-run guest took
16.581 seconds including QMP observation.

`make doctor`, `make check`, `make test` and `make build` pass. The authority
host configuration also passes with `cargo test --lib --target
aarch64-apple-darwin --no-default-features --features authority-test`.

## Retained source and execution evidence

The audited source inventory covers **88 source/build files**, including all
kernel, tooling, Cargo and boot inputs. Documentation is outside that freeze.
The source aggregate SHA-256 is:

`05a608549d8bb608f0d73bb7002af2e110837b217416f43c35e6579aa45fd3fe`

The aggregate hashes sorted UTF-8 lines of each file hash, two spaces, its
relative path and a newline. Individual hashes and the exact scheme are in
`build/k10-validation-source-manifest.json`. Every source hash remained
unchanged throughout the final check/test run and evidence audit. All **189
kernel images** match their full-run records and saved image manifests; all
five repeats use those same images. The default build matches the full boot
execution with kernel SHA-256:

`fa38f2f5be6a8e74af7ffee87ac72dc08d087a7920442ceece286c87ed6c65ec`

| Artifact | Purpose |
| --- | --- |
| `build/k10-validation-summary.json` | Audited counts, image/capture/log hashes, 20 scheduled-profile conservation audits, five repeat audits and process cleanup |
| `build/k10-full-test-results.json` | Complete 189-case report; SHA-256 `06d36eb28271ce4d2fede93ee80b7f013389627f9e2c98baf357cee1d17a85bf` |
| `build/k10-full-test-evidence/` | Original full-run captures, serial/QEMU logs and per-image manifests |
| `build/k10-repeat-results.json` | Five repeat results; SHA-256 `02f093d3e0bee204125a977da3f05fa8cc9c9a5dd10204318e2f0cb54b844daf` |
| `build/k10-repeat-evidence/` | Separate repeated-run captures and manifests |
| `build/k10-doctor.log`, `build/k10-check.log`, `build/k10-full-test.log` | Toolchain, lint, host/observer and full guest execution logs |
| `build/k10-host-authority-feature.log`, `build/k10-build.log` | Authority-feature host tests and default build |
| `build/k10-audit-validation.py` | Reproducible checks of the retained evidence |

These artifacts are local generated evidence under `build/`. No project QEMU
process remained after the full run and focused repeats.

## Validation corrections retained with the evidence

The first complete-matrix attempt stopped after 66 passing cases at
`memory-object-cap`: two legacy MemoryObject observer schema checks still
expected a one-byte Rights field after its representation became two bytes.
Both capability and runtime observer checks now require the actual 16-bit
representation. The failing image passed with the corrected observer, the
runtime mapping path passed, and the complete replacement matrix passed.
That correction changed no kernel source or timeout. The original report,
logs, source manifest and observer remain in
`build/k10-development/full-regression-attempt-1/`; the two focused rechecks
remain in `build/k10-development/legacy-observer-correction/`.

The second full attempt stopped after 128 passes at `channels-wait-deadlock`.
It recorded one common-root timer drain after a blocked native call:
ticks=1, EOIs=1, user=0, drain=1. All three user capture counts stayed zero,
all waits were cancelled and exact cleanup completed. The old checker required
zero total ticks, conflating that documented drain with user execution.
The corrected accounting requires zero user IRQs/captures for quiescence and
retains exact tick/EOI/drain coverage, safe-root checks and final hardware
shutdown checks. Four regressions cover valid drains and reject user progress,
foreign roots, replayed/missing events and counter mismatches. The original
evidence remains in `build/k10-development/full-regression-attempt-2/`;
deadlock, normal-wait and capability-backpressure rechecks are preserved in
`build/k10-development/wait-accounting-correction/`. Kernel source and the
guest deadline were unchanged.

The third full attempt stopped after 178 passes at `smp-tlb-2`. Native and
IRQ probes were complete, but a direct snapshot of A1 and A2 simultaneously
at CPL3 was missing. The fixed 20 ms polling period can align with CPU0's two
10 ms scheduler quanta. Polling now cycles through 3/7/11 ms until the actual
same-Node overlap is captured, then returns to 20 ms. Required evidence and
the deadline are unchanged. Two cadence regressions and thirteen focused SMP
runs pass, including three runs of the previously failing image. Original
evidence remains in `build/k10-development/full-regression-attempt-3/`, with
rechecks in `build/k10-development/smp-observation-correction/`. This correction
also changed no kernel source.

The fourth full attempt stopped after 188 passes at `smp-authority-terminate`.
Its prepared marker was published while the serial line was still being written,
before the BSP reached the real wait window. Baseline/prepared inventory
acceptance now waits for the actual RIP. Advancing to execution or cleanup
without the required baseline/prepared checkpoint still fails. Three regressions
cover early publication, rejection of invalid inventory after wait entry, and
missing checkpoints. Six focused runs pass with unchanged images, including
three termination runs, negative TLB, ordinary scheduling and two-CPU authority.
Evidence remains in `build/k10-development/full-regression-attempt-4/` and
`build/k10-development/checkpoint-publication-correction/`. Kernel source,
required observations and the 30-second deadline remain unchanged.

A fifth, unchanged-source attempt stopped after 184 passes when
`smp-authority-queued` rejected timer-calibration consistency before scheduler
startup. The original serial failure and 30-second timeout capture remain in
`build/k10-development/full-regression-attempt-5/`. No calibration threshold,
source or deadline was changed. Seven focused runs, including three executions
of that same queued-authority image, and the complete replacement matrix pass.
Rechecks are in `build/k10-development/calibration-recheck/`. The rejected
sample values were not captured, so these records do not establish a specific
host-scheduling cause.

Earlier bring-up also corrected the authority ELF-symbol filter, provided two
K10-only shared-backing frame slots, and updated the host pool assertion while
retaining the legacy 16-frame limit. A termination capture caught a diagnostic
flag immediately before its wait function; the observer now waits for actual
RIP entry and still requires every terminal observation. The original capture
and host assertion failure remain in `build/k10-development/` and
`build/k10-host-authority-feature-initial.log`. The validation summary records
the individual correction paths. Failed evidence was not counted as acceptance.

## Scope after K10

[ADR 0030](adr/0030-bounded-capability-derivation-and-revocation.md) and
[experimental ABI 0.6](abi/experimental-v0.6.md) define this implementation.
Earlier images retain their original ABI and earlier ADRs retain their recorded
scope. The test bootstrap provides independent roots and the initial tracked
mappings; there is no new public factory, remote-table edit or map/unmap call.
Missing hardware ACKs fail stop with backing retained. Previously copied data
cannot be withdrawn.

**K11 device primitives are the next roadmap milestone:** selected userspace
driver support with authorized MMIO/IRQ access, potentially using QEMU EDU.
Untrusted DMA is unsupported without suitable isolation. General allocation
and mapping APIs, kernel preemption, balancing, FPU/SIMD, physical hardware,
COW and demand paging remain later work. These acceptance results cover local
Rust/Python tests and QEMU TCG/q35; hosted CI and physical hardware were not run.

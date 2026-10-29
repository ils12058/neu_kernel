# MemoryObject QMP evidence (bounded Phase 8A-1 and 8A-2)

The Phase 8A-1 `memory-object-*` images run trusted kernel code with one CPU,
IF=0 and inactive user roots; they do not execute shared-memory accesses in
Ring 3. The Phase 8A-2 `memory-object-user-*` images then execute real Ring 3
writes/reads through the installed shared leaves and, in the fault profile,
produce a genuine hardware `#PF`. Architecture v0.2 and ABI 0.5 remain the design
and ABI baselines.

## Profiles

| Case | Purpose |
| --- | --- |
| `memory-object-lifetime` | Existing user leaf tables; A destroyed before B |
| `memory-object-lifetime-reverse` | Existing tables; B destroyed before A |
| `memory-object-new-tables` | One additional user PT per root; ordinary unmap/prune/remap; A first |
| `memory-object-new-tables-reverse` | Same new-table transitions; B first |
| `memory-object-map-rollback` | Allocation failure before a leaf, second-leaf write failure after new PT allocation, and allocation failure after a first leaf |
| `memory-object-fail-stop` | Deliberate rollback backend corruption; actual panic halt with a poisoned partial leaf |

Use `python3 tools/neu.py test --case <name>`. Each case keeps the existing
30-second default timeout, failure reporting and final QMP CPU halt checks.
Repeated runs and clean-directory runs are not additional unique scenarios.

## Live Ring 3 profiles (Phase 8A-2)

| Case | Purpose |
| --- | --- |
| `memory-object-user-share` | A1 stores and reads back; B1 reads the same physical bytes at a different VA; B destroyed first |
| `memory-object-user-readonly-fault` | B1's read succeeds, then a user store to its read-only alias raises a real `#PF`; B is contained and reclaimed |
| `memory-object-user-a-exit` | A finishes and is destroyed first; B waits, then reads the surviving backing before its own teardown |
| `memory-object-user-xapic` | Same live sharing/teardown contract on the legacy local-APIC fallback |

### Fixed live contract

- One trusted RAM-backed two-page MemoryObject, zeroed before execution.
- Node A maps both pages at `0x600000` with user RW/NX leaves; Node B maps the
  same physical pages at `0x602000` with user R/NX leaves. The VAs are distinct,
  page-aligned and inside the existing `[0x400000, 0x800000)` user range.
- Only A1 (writer, label 1, slot 0) and B1 (reader or faulting writer, label 3,
  slot 1) are installed. The two live Threads peak at 14 data frames within the
  fixed 16-frame pool. The third assembly role remains present but is not run.
- The trusted controller writes only private gate/counter/observation words in
  each Node's DATA page. It never writes the shared payload or synthesizes the
  reader's result. A1 and B1 execute ordinary user stores/loads.
- The live assembly roles are compiled into the shared 4 KiB user RX fixture
  page only for `memory-object-live-test`; the legacy multi-Thread payload
  remains 3688 bytes and no other feature's page budget changes.
- Preemption is armed with `peer=true` unconditionally so even a single spinning
  Thread is checkpointable. Queue order is `[A1, B1]`.
- Share/fault/xAPIC destroy B first while A still maps the object. A-exit destroys
  A first while B waits at its pre-gate, then B reads and reaches its post gate.

### Live checkpoints

The live descriptor is `neu_memory_live_witness` (128 atomic u64 words). QMP
stops the sole CPU inside the bounded `neu_memory_live_wait` countdown, rechecks
RIP/CPL/CR3/IF, then reads only production state. All eight stages must appear in
order:

| Stage | Observed state |
| --- | --- |
| 0 | Exact allocator/table baseline; no test object, Node or mapping |
| 1 | Two-page object live with creator reference and zeroed physical backing |
| 2 | Both Nodes/spaces mapped, both Threads installed and queued with fresh saved frames |
| 3 | Creator released; both mappings still retain the object |
| 4 | A1 at its post gate and B1 read/gate or the B hardware fault; mode-specific raw blocks and frames |
| 5 | First Node reclaimed (B for share/fault/xAPIC, A for A-exit); survivor mapping still valid |
| 6 | Second Node reclaimed (share/fault/xAPIC), or B's post-A-exit read checkpoint (A-exit) |
| 7 | Object collected; exact baseline bitmap, table and frame inventory restored |

The descriptor carries only addresses/layouts/constants: magic, stage, mode,
kernel root, A/B VAs, payload constants, schema, Node/Space identities, the
`NEU_LIVE_FAULT` address, registry layout, allocator layout, table-owner layout,
both UserSpace root/id/inventory/teardown/stack-table pointers, saved-frame
pointers, private-block offsets and Thread/stack addresses. It contains no
guest-calculated “verified” scalar and no permission summary.

### What the live oracle proves

For every stage the observer reads:

- the actual object/frame/space/mapping records and table-owner pool;
- allocator counters, bitmap and actual memory-map spans, including the exact
  final bitmap;
- physical page-table pages, reconstructing each user leaf's physical target and
  effective U/S, R/W and NX bits;
- both roots' inventories, workspace aliases and supervisor-only status;
- the production saved Thread frames from `Nodes.threads[].saved`, including
  Ring 3 CS/SS/RFLAGS/RSP/RBX and live gate RIP ranges;
- the physical object pages and each Node's private DATA block;
- the raw `NEU_LIVE_FAULT` record written by the real `user_fault` path.

In the share profile the physical first page must contain the two A1-written
values and the second page the sentinel; B1's private block must contain the same
values read through its R/NX VAs. In the fault profile B1's block must contain
the successful read, its gate/marker state must match the controller model, and
the fault record must show vector 14, error `0x7`, `CR2 = 0x602000`, the actual
store-site RIP, `CS = 0x3b`, `SS = 0x33`, CPL 3, Node/Thread/Space/CR3 of B and a
handler stack inside B's kernel stack. B's Node must be `Exited`, its mappings
gone exactly once, and A must still complete. The A-exit profile proves B reads
the unchanged physical values after A's root is destroyed.

Serial output alone never satisfies these profiles: missing or mismatched raw QMP
state fails the case.

## Production integration

`Paging::map_memory_backing` checks the exact registered root, inactive/unpinned
space, layout reservations, grant, backing generation and permissions. The lower
64 KiB of the user fixture is reserved. The next 2 MiB region, `0x600000`, lies
inside the existing `[0x400000, 0x800000)` eligible range, clear of existing code,
data, stacks, guards and the private probe. It needs an additional leaf table.

`PageTree::required_user_tables` counts distinct missing intermediate tables
without allocating. The wrapper preflights the unchanged 64-slot table pool and
physical budget. The ordinary page-tree/object transaction rolls back on failure.
After mapping commits, the wrapper records every new table in `UserSpace.tables`
and installs its supervisor RW/NX alias only in that root's workspace. Unexpected
publication or verification failure is fail-stop. No partial success is returned.

Normal unmap removes the leaf/reference, detaches empty owned intermediate tables,
removes their per-root workspace aliases, flushes and releases their common
workspace aliases and physical allocation, then updates the token inventory.
Whole-root teardown preflights the fully prepared root and all aliases/parent
references before shared-leaf detachment and existing table/private-frame cleanup.
A partially constructed root remains covered by the older construction-rollback
path; it is never published as prepared.

All operations use exclusive live Paging ownership at the common kernel root.
There is no alias to a suspended mutable controller, no asynchronous IRQ access,
no public mapping syscall, and no change to object/capability transfer semantics.

## Frozen checkpoints and schema

`neu_memory_witness` is a test-only array of 128 atomic u64 words. It contains
schema identifiers, addresses, strides and field offsets into production storage.
It does not contain a guest-calculated verification result. Registry identities,
frame owners, mapping provenance and fixed object backing use defined `repr(C)`
layouts; tagged owners/provenance use `repr(C, u64)`. Layout offsets are described
by `Registry::diagnostic_layout`; allocator addresses come from its diagnostic
layout. No production data is copied into an independent fake registry.

The first words identify the checkpoint, profile, common root and selected VAs.
Words 16–53 describe registry arrays, 64–71 identify allocator counters/bitmap and
memory-map descriptors, 80–85 describe actual table ownership, and 90–97 point to
both real UserSpace tokens and inventories. These addresses remain kernel-only.

After publication, the guest enters the established bounded countdown probe.
The host treats running CPU/descriptor reads only as hints, issues QMP `stop`,
then rechecks RIP, CPL=0, IF=0/DF=0, CR3 and the descriptor before reading state.
The entire snapshot occurs with the sole CPU stopped. It resumes only afterward.
Every required stage must be observed in order; missing a probe fails the case.

| Stage | Observed state |
| --- | --- |
| 1 | Exact allocator/table baseline, no test object |
| 2 | Live two-page object, creator reference, real zeroed backing |
| 12 | Both distinct user roots before shared mappings |
| 3 | Both roots mapped with the same backing, A RW/NX and B R/NX |
| 4 | Creator released; mappings retain the object |
| 5 | First root retired; second mapping remains valid |
| 6 | Both roots retired; object live without mappings |
| 7 | Object/frame retirement and exact baseline bitmap restored |
| 10 | Reused physical pages with new object/frame generations, zeroed again |
| 11 | Replacement collected; exact baseline restored again |
| 8/9 | New-table ordinary unmap/remap, or rollback/fail-stop profile boundary |
| 13 | New-PT/second-leaf failure rolled back before the boundary-allocation failure |

The host scans actual live registry records and allocator bitmap, reconstructs
physical candidate ordinals from the actual memory map, and reconciles each
allocated bit with live data/table owners. It reads physical page-table pages via
QMP `xp`, walks all four levels and combines effective U/S, R/W and NX bits. It
checks all user workspace slots, root reachability against each inventory,
private backing isolation and immutability, and the exact surviving root in both
teardown orders. Backing pages are read physically to verify zero sanitation.

Machine-readable per-case JSON retains descriptors, CPU states, registry fields,
raw PTE walks, inventories, object/frame generations, bitmap words and allocator
counts under `build/test-logs/<case>.json`. The oracle is implemented in
`tools/memory_object_qmp.py`; `tools/neu.py` controls execution and failure handling.

## Failure boundaries

Ordinary preflight/exhaustion errors preserve the token, registry and allocation
state. Allocation of a new PT followed by a later leaf failure rolls back that PT
and its aliases. Backend writes must fail before modifying the specified entry.
Unexpected failures during cleanup or after publication remain fail-stop.

The fail-stop case deliberately rejects clearing a leaf after the next-table
allocation fails. QMP verifies the actual first leaf remains, the second leaf is
absent, no shared record was committed and the allocator bitmap did not change.
The CPU reaches the panic halt. This is an expected terminal-failure test, not a
successful rollback or a reusable address space.

No Phase 8A-1 checkpoint claims Ring 3 shared execution, a hardware read-only
fault, capability transfer of a MemoryObject, SMP safety or production readiness.
The Phase 8A-2 live checkpoints above do claim the bounded trusted Ring 3 access
and fault containment described in [ADR 0023](../adr/0023-live-userspace-memoryobject-sharing.md).

## Live observer corrections retained during finalization

Three initial live-oracle checks were rejected and corrected before acceptance;
none was accepted as guest success:

- the descriptor-root membership test ran before the production walk populated
  its root map, so a valid root was reported missing;
- kernel-stack tables are recorded in both `tables` and `stack_tables` but only
  non-stack tables receive user-root workspace aliases, so aliases were compared
  against the wrong inventory;
- the A-exit wait gate increments its bounded spin counter, so requiring every
  private word to be zero misclassified a legitimate wait loop as an early read.

The corrected oracle still requires every stage, all raw production assertions,
the exact fault record and the exact final bitmap. A guest that prints the
expected completion line but lacks this evidence fails.

## Observer failures retained during finalization

An initial probe using PAUSE on every countdown iteration timed out at its first
checkpoint. It was replaced by the existing native countdown pattern without
changing the 30-second test timeout. The first full-suite attempt later rejected
an expired running RIP hint in the new reverse-table profile. The checker now
retries that hint until it obtains the required stopped probe snapshot; the
required stage sequence and all raw-state assertions remain mandatory.

A subsequent full attempt failed the legacy `user-unmapped` CPU entry assertion.
Its exact trigger remains unconfirmed. The legacy observer previously validated
a running sample from any payload instruction, including instructions that
briefly push/pop the stack. It now stops and rereads the CPU in the existing
bounded user wait interval, preserving the original register/privilege assertions.
No fixture iteration count or timeout was increased. The rejected runs remain in
`build/k8-final-test-first-attempt.log`, `k8-final-test-second-attempt.log` and
`k8-final-user-unmapped-rejected.json`. These observations do not establish the
cause of the older historical `user-kernel` failure, nor attribute it to concurrent
QEMU runs.

# K10 authority: independent stopped-CPU acceptance

`tools/authority_qmp.py` extends the [scheduled SMP observer](smp-qmp.md) and
uses production layouts, not serial success messages, to verify K10.
The unchanged per-case guest deadline is 30 seconds. Run a case with:

```sh
python3 tools/neu.py test --case smp-authority-4
python3 tools/test_authority_qmp.py
```

## Profiles and actual user execution

| Case | CPUs | Required behavior |
| --- | --- | --- |
| `smp-authority-2` | 2 | Nested derivation, rights rejection, four mapping invalidations, revoked store faults |
| `smp-authority-4` | 4 | Same contract, including ACKs from idle/unrelated CPUs |
| `smp-authority-queued` | 4 | Queued descendant is discarded; recipient sees Empty after revocation |
| `smp-authority-close` | 2 | Mappings and descendant survive source-handle close, then revoke and finish normally |
| `smp-authority-wait` | 4 | A derived endpoint wait blocks, is revoked and resumes with `-38`, before memory revoke |
| `smp-authority-stress` | 4 | Sixteen repeated derivations/closes after revoke, with fresh handle generations |
| `smp-authority-terminate` | 4 | Sibling fault cancels the blocked revoker and closes its Node; controller still retires every mapping |
| `smp-authority-disabled` | 2 | ABI 0.5 retains its version and rejects both new operations on all three Threads |

All profiles retain the real native/timer bridge, user progress/canaries, full
saved-context reconstruction, same-Node concurrent execution and exact final
allocator/table checks. The 0.6 fixtures execute CAP_DERIVE/CAP_REVOKE in Ring 3;
the Python model independently computes return values from the captured inputs.

Node A's root creates a child and grandchild. The child maps the two-page shared
MemoryObject RW at `0x410000`/`0x411000`; its handle then closes. The grandchild
moves through a Channel to Node B and maps R at `0x420000`/`0x421000`. An
independent B root maps the same pages RW at `0x430000`/`0x431000`. B writes
`0x2202` through its independent mapping and must still read it after revoke.

The ordinary fault profiles store at `0x410010` only after successful revoke;
the actual normalized exception must be user `#PF`, error 6. The termination
profile faults at the separately unmapped `0x440000` while the requester is
blocked, observes that inactive sibling at the pre-commit fault checkpoint,
and closes A before the first mapping invalidation. The requester never resumes.
The controller publishes a test barrier only after completing its pending work,
allowing B to verify surviving independent authority and finish normally.

## Production state and mapping ownership

The observer stops every CPU, checks the shared lock is free and reads raw
capability tables, generation counters, FIFO tokens, wait slots and both mapping
registries. `neu_authority_layout` locates the actual lineage and mapping fields;
the existing scheduler/paging schemas locate the remaining state.

For every live grant it checks exact handle/transfer/wait/mapping owner counts,
matching object/rights, parent presence, absence of cycles and rights subsets.
Structural ancestors and the active revoke source are the only permitted live
entries without direct owners. Handles and queued tokens cannot retain revoked
authority. Every capability mapping binding must match exactly one paging
record's space, address, grant, pending epoch and target mask, and both must
refer to the same retained MemoryObject. No missing or duplicated owner passes.

The paging observer reads actual PTEs, frames and table pages. A pending revoke
has an absent PTE but a retained live registry record and capability pin. The
prepared user page tables contain exactly the expected private stacks, image,
barrier and six shared mappings; no extra user alias is accepted. K10 uses
18 data-frame slots; legacy images retain 16.

## Actual invalidations and syscall results

`neu_authority_invalidations` describes at most eight transactions, each with
16 words. Four memory revocations are required in every authority fixture.
The observer checks one post-instruction, pre-ACK window for **every page and
every online CPU**. The linked bytes must really be INVLPG/CR3 instructions;
the live CPU's root, instruction pointer, operand registers, CPL and flags must
match the request. Its local ACK and shared ACK bit must still be unpublished.
Mappings/backing remain owned in this window. A completed record alone is
insufficient: missing even one CPU/epoch observation fails terminal acceptance.

Diagnostic probe flags are published a few instructions before entering the
observable wait. The observer waits for RIP to enter that window; it does not
accept the flag as proof. The required terminal observation sets remain exact.
Before same-Node Ring 3 overlap is captured, polling cycles through 3, 7 and
11 ms intervals. A fixed 20 ms cadence can match CPU0's two 10 ms scheduling
quanta and repeatedly sample the unrelated Thread. After the actual overlap
is observed, polling returns to 20 ms. The guest deadline is unchanged.
The baseline and prepared markers also precede serial output and the BSP's
wait call. The observer defers inventory acceptance until the actual wait RIP;
a later phase still fails if either required checkpoint was never observed.

The 128 by 16-word operation ledger records sequence, phase, CPU, Node, Thread,
operation, all six inputs, result and actual CPU entry/root. Each row must match
the saved normalized hardware syscall frame. Phase 0 records the operation;
phase 1 completes a suspended revoke and names its original entry. The Python
grant/table/FIFO model reconstructs those operations in publication order and
computes rights, allocation, moves, cancellation and return values independently.
Every resumed frame must contain the modeled RAX/RDX and preserve all other
required registers. A completion has exactly one matching pending request.

At terminal observation, exactly four selected mappings have retired after all
ACKs. Both independent B mappings and their data survive. At final cleanup,
every lineage entry, binding, handle, queued reference and wait is gone;
allocator bitmap, free count and table inventory equal the pre-launch baseline.
Every CPU has restored its kernel root and control state with an empty LAPIC.

## Host regressions and retained evidence

The Rust tests exercise exhaustive rights subsets, nested/independent grants,
close/move conservation, real host-thread transactions through the production
SpinLock, send/receive wait cancellation, pre-sweep settlement rejection,
generation/counter exhaustion, map permission/range rejection and partial-table
rollback. They check wrong/duplicate receipts, missing acknowledgements, failed
shootdown retention and mapping completion after all requester handles close.

Twelve Python regressions deliberately tamper with results, handles, rights,
provenance, epochs, roots, addresses, modes and ACK evidence. They also check
that one missing CPU or page observation prevents terminal acceptance. `make
check` and full `make test` run these alongside eight preemption-accounting
tests, four blocked-wait accounting regressions and two scheduler-cadence
regressions, plus three checkpoint-publication regressions. Those tests reject
invalid inventory after entering the wait and reject missing baseline/prepared
checkpoints when execution advances. The wait tests distinguish
documented kernel drains from user execution while preserving exact EOI,
tick, no-progress and final-quiescence checks.

Acceptance captures and hashes are enumerated in
[K10 completion](../K10_COMPLETION.md). Focused runs overwrite ordinary
`build/test-results.json` and per-case logs, so full and repeated-run evidence
is preserved separately. These are local QEMU/host results, not physical-device
or hosted CI evidence.

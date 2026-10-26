# ADR 0014: Share one Node root across independently owned Threads

- **Date:** 2026-10-26
- **Status:** Accepted — K4 / Phase 4D, trusted bootstrap, one CPU, two Nodes and three Threads.
- **Extends:** [0010](0010-independent-bootstrap-nodes-and-fault-containment.md), [0012](0012-cooperative-thread-context-switching.md) and [0013](0013-single-cpu-user-preemption.md).
- **Related NEU decisions:** NEU-005/006 (Node protection domain, Thread scheduling); NEU-008 (ABI unchanged); NEU-011 (one CPU); NEU-017/018 (internal execution/ownership subset); NEU-020 (per-Thread FIFO); NEU-021/023/024 (bounded isolation, fault cancellation and tests).
- **Architecture:** [Section 5](../Neu_Kernel_Architecture_v0.2.md#5-node-model), [Section 6](../Neu_Kernel_Architecture_v0.2.md#6-capability-security-model), [Section 17](../Neu_Kernel_Architecture_v0.2.md#17-scheduling-clocks-and-cpu-topology--neu-sched-001).

## Context

A per-Thread root would defeat same-Node sharing. The existing one-stack-per-root
handle and global mutation prohibition while pinned cannot release one terminal
Thread's stack without risking a healthy sibling's mappings or root. Dynamic
stack page tables can themselves be shared by sibling stack leaves, so their
lifetime must belong to the Node hierarchy rather than the first stack creator.

## Decision

Keep one SpaceId/root and shared RX/code/constant/RW data backing per Node.
Use three independently generation-checked Thread slots: A1 and A2 share Node A;
B1 belongs to an isolated Node B. Each Thread retains its own complete saved CPU
context, bounded user stack and guarded 8 KiB supervisor entry stack. User stack
ranges are distinct allocated regions in a shared protection domain; siblings
can access each other's user memory and are not separate security principals.

Node Ready means configured but not yet executed. Once started, Active persists
while live Threads remain, including when all are queued/suspended. The CPU's
single active Thread binding is separate. Normal exit records that Thread's own
result and leaves live siblings eligible. The final live Thread determines normal
Node termination. An unrecoverable user fault records the faulting Thread's CPU
metadata and Node outcome, cancels other live members, clears their resumable
records and purges them from the FIFO before any shared teardown. Preserve
already terminal Thread outcomes. Reclaim Thread metadata independently from
Node resources, with generation reuse refusing stale IDs.

Retain one root pin per installed resumable Thread, including queued, running
and IRQ-handoff lifetimes. Two pins per root suffice for the tested member bound.
Ordinary mapping/frame/root mutation remains forbidden while pinned. Introduce a
borrowed, non-copyable Thread stack-edit proof, minted only at an inactive model
boundary for an uninstalled or terminal Thread with no saved/queued/CPU references.
The proof holds an exclusive model borrow. Paging additionally requires the
common hardware root and IF=0. It authorizes only that full Thread generation's
stack ranges and tagged stack frames, never shared code/data or another Thread's
resources. A completion receipt is minted only after that generation has no tagged backing
or stack mappings; metadata retirement consumes it. Construction and rollback
obey the same scope; never unpin healthy
siblings to bypass the root lifetime rule.

Tag backing with its owning Thread as well as SpaceId. Pin release and precise
user/kernel stack unmapping occur only after the terminal handoff is consumed.
Keep shared root/code/data alive while siblings remain. Stack page-table frames
belong to the Node hierarchy and persist until their last leaf is detached;
retain a Node-scoped dynamic-table mask so early stack teardown cannot create
stale or missing workspace aliases. Flush/invalidate before reuse.

Increase MAX_THREADS and the guarded stack arena from two to three. Keep two
Nodes/live spaces, 16 data owners, 64 mappings and 64 table frames. A uses three
shared image/data pages plus two user stacks (five); B uses code/constant/data/user-stack pages plus
its private sentinel (five); three two-page kernel stacks add six: exactly
**16 data owners**. Same-Node extra stack leaves use existing user/kernel table
branches. No heap, cross-Node shared MemoryObject or blanket pool expansion is
needed. Document and test a maximum of two live members per Node in this profile.

Reuse the 10 ms provisional one-shot, single-use IRQ mailbox and common-stack
handoff. Publish exact Thread/Node/SpaceId/root/user-stack bounds/entry stack and
TSS.RSP0 with IF=0 before IF=1 IRETQ. No IRQ mutates model/paging/allocator borrows.
Native ABI 0.1 query/exit numbers, results and feature bits remain unchanged;
thread_exit ends only its caller. No public creation or scheduling operation is
introduced. Retain integer-only execution, zero TLS and separate fatal IST policy.

Use aligned LOCK XADD for the shared counter, with per-Thread progress witnesses
and bootstrap-supplied stack parameters. Preemption may occur between surrounding
instructions; plain load/increment/store is not sufficient. Reconcile the final
shared count against recorded atomic completions, without pretending that a
partially observed fixture sequence is an atomic multi-word transaction.

FIFO remains per Thread, with no per-Node budget: a Node with two eligible Threads
can receive more aggregate CPU time. Calibration and QEMU observations do not
establish exact timing or per-Node fairness.

## Alternatives considered

- **Clone a root per Thread.** Does not implement a shared Node address space.
- **Keep all stacks until Node exit.** Safe but misses precise independent Thread
  ownership, early cleanup and stale-slot reuse.
- **Temporarily drop all pins to mutate one stack.** Invalidates the guarantee
  protecting healthy suspended siblings and permits unrelated mutations.
- **Make stack table pages Thread-owned.** Shared parent/leaf branches outlive
  the first creator; Node ownership follows their actual references.
- **Raise every pool or expose public factories.** Neither is required by the
  three-Thread profile; capability authority remains a separate K5 obligation.

## Consequences

Same-Node sharing is real and testable at the physical mapping level. Thread
exit/cancellation and Node teardown become distinct transactions. Borrowed edit
proofs and bounded resource tagging constrain the privileged exception to pinned
mutation. Fixed capacities, sealed payloads, one CPU and non-preemptible kernel
transactions remain limits, not a general object system or production threading
API. Kernel corruption and unsupported nested critical events remain fail-stop.

## History

- **2026-10-26:** Chosen for Phase 4D before implementation. Earlier ADR rationale
  and architecture v0.2 remain intact. Actual verification and limitations belong
  in [implementation status](../IMPLEMENTATION_STATUS.md).

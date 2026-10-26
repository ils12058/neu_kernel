# ADR 0010: Independently own bootstrap Nodes and contain user faults

- **Date:** 2026-10-25
- **Status:** Accepted — K3 / Phase 3A, two live bootstrap Nodes, sequential
  initial Threads, single CPU with IF=0.
- **Extends:** [0007: owned paging](0007-kernel-owned-paging-and-controlled-mappings.md),
  [0008: isolated execution](0008-isolated-ring-3-fixture-and-test-return.md),
  [0009: native ABI](0009-experimental-native-syscall-and-trusted-return.md).
- **Related NEU decisions:** `NEU-005` (Node boundary), `NEU-006` (distinct Thread
  association; multithreading remains unimplemented), `NEU-007` (capability
  obligation remains incomplete), `NEU-008` (unchanged ABI 0.1), `NEU-011`
  (one CPU first), `NEU-017 / EXEC` (internal bootstrap only), `NEU-018 / MEM`
  (private roots/ownership subset), `NEU-021 / SEC` (bounded threat model),
  `NEU-023 / DIAG` (user-fault containment), `NEU-024 / TEST` (local evidence).
- **Architecture:** [Section 5](../Neu_Kernel_Architecture_v0.2.md#5-node-model),
  [Section 15](../Neu_Kernel_Architecture_v0.2.md#15-detailed-physical-and-virtual-memory--neu-mem-001),
  [Section 20](../Neu_Kernel_Architecture_v0.2.md#20-fault-handling-panic-and-diagnostics--neu-diag-001).

## Context

Phase 2B proves native entry/return for one fixture. A singular live root and
VA-only mapping records cannot represent two domains at identical virtual
addresses. Repeating singleton construction would not prove independent
ownership. Mirroring every global table alias into each user root would also
leave sibling aliases after a teardown, or require unsafe synchronization of all
roots whenever global table slots change.

The native session/continuation is single-active-context state. Reusing it
requires an explicit trusted association and fresh return contract per execution.
A user fault must return to that association's trusted controller rather than
panic the kernel or accidentally terminate the sibling. This can be established
without adding timer/preemption, a scheduler or public creation authority.

## Decision

Represent Nodes and Threads separately in a bounded internal model. A Node owns
the domain and its address-space association; a Thread owns an execution context
and refers to its Node by a typed identity. Use independently generation-checked
`NodeId`, `ThreadId` and `SpaceId`; reject stale identities and exhaustion without
generation wrap. The bootstrap constructor permits one initial Thread per Node.
That restriction is policy, not a permanently embedded one-to-one type.

Nodes follow Created → Configuring → Ready → Active → Exited → Reclaimed.
Threads follow Configuring → Ready → Running → Exited → Reclaimed. Normal
`thread_exit` ends the active Thread; its Node exits only when no live associated
Threads remain. A supported user fault terminates its Node's associations, never
another Node. Construction failure is a separate terminal reason. Keep normal
status or copied fault metadata after resource reclamation, until identity-slot
reuse; this is bounded diagnostic retention, not durable event storage.

Give each domain a fresh PML4 and private child tables, including the supervisor
image/workspace branches. Share only permanently reserved supervisor image
backing. User code, constants, data, stack and any private test leaf have separate
initialized frame owners. Key mappings by **(SpaceId, virtual address)**, validate
the destination root against that identity, and refuse another owner's frames.
No user-frame sharing is authorized. Keep initialization, mapping references,
global physical uniqueness and W^X checks intact.

The pinned kernel workspace aliases all owned table frames for construction and
inspection, exclusively under the original kernel root. Each user root contains
only its own supervisor table aliases. It never borrows a sibling's tables or
the kernel root's child tables. Reject backend table access while a user root is
active. This avoids updating surviving roots when another root is constructed
or reclaimed. PGE/PCIDE stay disabled; CR3 changes and local invalidation precede
reuse of detached table/backing frames.

Retain the 64-table, 16-data-owner and 64-mapping pools. Add bounds of two live
user address spaces, two Nodes and two Threads, with capacity, stale-identity and
failure tests. This profile fits two private roots and nine user backing frames
without enlarging existing pools. No heap, capability-space placeholder,
ResourcePool or generalized MemoryObject is introduced.

Construct both Nodes and retain their resources before either Thread runs. Use
trusted kernel bootstrap code to choose each Thread and obtain an immutable
active binding containing its Node, Thread, SpaceId, root, entry and initial RSP.
Bind a freshly reset native session to it; returning syscall state is validated
against that root and the existing sealed payload sites/stack. The userspace
registers do not select a Node or kernel context. Keep ABI 0.1, operation numbers,
registers, statuses, version encoding and validated IRETQ unchanged.

Reuse the guarded syscall stack and suspended kernel continuation only while
one context executes. Reset observation state, require a clear entry guard,
configure/read back native MSRs, and keep resources live through query returns
and terminal exit. After trusted return, restore/check MSRs, original CR3 and
entry guard before admitting another Thread. The native handler returns an
outcome by value; the controller updates the lifecycle on its trusted stack.
It does not access a suspended mutable Node/paging-manager borrow.

For supported noncritical CPL=3 exceptions, copy vector, error, RIP, RSP, CS, SS,
flags and CR2 from the actual exception path, verify the active root/entry stack,
record a Node-local fault and resume only the trusted kernel continuation.
Invalid return state is classified separately from a CPU exception. Never
dereference user pointers to produce diagnostics or resume the faulting Thread.
Unexpected CPL=0 faults, NMI, #DF, #MC and unsafe entry invariants retain the fatal
policy. This decision does not claim recovery from kernel corruption.

Reclaim an inactive domain independently: clear its hierarchy/workspace
references, flush the kernel root, remove only its scoped mapping records,
release its table aliases/frames and user backing, clear/retire the address-space
identity, then reclaim Node/Thread metadata. Refuse active or referenced
resources. Construction rollback uses the same owned cleanup path and checks
exact pre-attempt accounting while preserving a live sibling. Fatal invariant
failures halt with ownership retained rather than pretending safe recovery.

Bootstrap creation is explicitly trusted internal code, compiled in test
configurations. **Node-local capability governance is incomplete.** There is no
unrestricted userspace factory, capability operation or authority bypass API.
Accepted here means this scoped direction; it does not accept the complete Node,
Thread, MEM, EXEC, SEC, DIAG or TEST proposals in the architecture register.

## Alternatives considered

- **Destroy the singleton before creating its replacement.** Easier to reuse,
  but cannot demonstrate coexistence, private ownership or survivor safety.
- **VA-only mapping keys or a mutable global current-owner selector.** Identical
  addresses would collide or silently bind operations to the wrong domain.
  Explicit typed keys and root checks make the association reviewable.
- **Share child tables or mirror all workspace aliases in every root.** Adds
  borrowed lifetimes and cross-root alias updates to sibling reclamation. Private
  children and own-only aliases keep teardown local at a modest frame cost.
- **Embed one Thread permanently inside Node.** Obscures execution versus
  authority and would make any future Thread exit incorrectly destroy its Node.
- **Add a scheduler, per-CPU/SWAPGS machinery or public factories now.** Those
  require separate interrupt, context-switch and capability decisions. Sequential
  control is enough to test this isolation boundary first.
- **Resume every user fault or halt on every fault.** Resumption needs recovery
  semantics absent here; global halt would prevent the required healthy sibling
  from continuing. Terminal Node-local containment preserves that distinction.

## Consequences

- Two live domains may reuse numeric VAs with different owned physical frames.
  One domain's normal exit, fault or teardown need not affect the other.
- Private tables cost ten pages per domain in the tested layout. Fixed pools,
  conservative scans/flushes and sealed payloads favor auditability over scale.
- One active context, one CPU, IF=0 and non-reentrant static native state are
  hard restrictions. There is no CPU-time isolation from a looping user Thread.
- Multiple runnable Threads, capability spaces, ELF loading, shared memory,
  asynchronous cancellation and general scheduling remain unimplemented.
- QMP must independently sample CPL/root/stack/syscall state for both Threads.
  Fault frames, both execution orders, same-VA backing, private-only access,
  early cleanup, injected rollback and exact accounting need separate checks.
  Serial pass labels alone are insufficient evidence.

## History

- **2026-10-25:** Accepted under the explicit Phase 3A request. ADR 0009's native
  ABI remains accepted; its singleton lifecycle is extended only in this scope.
  Architecture v0.2 remains unchanged. Commands, actual results and remaining
  limits are recorded in [implementation status](../IMPLEMENTATION_STATUS.md).

- **2026-10-25, Phase 4A:** [ADR 0011](0011-local-timer-interrupt-foundation.md)
  establishes a separate kernel-only timer window. These Nodes still execute
  sequentially with IF=0; their model, ownership and ABI remain unchanged. Safe
  Thread switching requires the new record's stack/context prerequisites first.

- **2026-10-26, Phase 4B:** [ADR 0012](0012-cooperative-thread-context-switching.md)
  adds FIFO Thread admission/suspension, by-value user contexts, private entry
  stacks and lifetime pins. Root/backing ownership remains Node-scoped and all
  switches retain IF=0. Newly allocated stack tables are accessed only through
  the common kernel workspace; no sibling or stale user workspace aliases are
  added. The accepted protection-domain distinction and earlier tests remain.

### 2026-10-26 — Phase 4C extension

[ADR 0013](0013-single-cpu-user-preemption.md) admits IF=1 only for explicitly
preemptible user contexts and captures real timer-origin frames through the
common-stack boundary. Native kernel execution and resource/scheduler mutation
remain IF=0. Earlier fixtures retain their original behavior; the accepted ABI,
protection-domain and ownership directions remain unchanged. This extension does
not add general kernel preemption, SMP or public object/scheduler interfaces.


### 2026-10-26 — Phase 4D extension

[ADR 0014](0014-multiple-threads-in-one-node.md) extends the trusted bootstrap to
three Threads across two Nodes, with two members sharing one Node root. It
replaces the one-initial-Thread restriction for that profile, separates persistent
Node Active state from CPU Thread selection, and scopes pinned stack edits to an
inactive uninstalled/terminal Thread's full identity. Normal exit retains healthy
siblings; an unrecoverable Node fault cancels its live members before shared
teardown. Per-Thread FIFO and the user-only timer/common-stack boundary remain.
Earlier rationale and execution profiles are retained; public authority, general
kernel preemption, SMP and extended CPU state are still deferred.

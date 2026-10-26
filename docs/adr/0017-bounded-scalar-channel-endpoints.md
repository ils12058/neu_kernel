# ADR 0017: Capability-backed endpoints and bounded nonblocking scalar Channels

- **Date:** 2026-10-26
- **Status:** Accepted — scoped K5 / Phase 5C bridge into K6; one CPU, scalar IPC.
- **Extends:** [0014](0014-multiple-threads-in-one-node.md), [0015](0015-node-local-capabilities-and-typed-objects.md), [0016](0016-capability-abi-and-live-address-space-inspection.md).
- **Related NEU decisions:** NEU-005/006 Node authority and Threads; NEU-007/009 typed explicit authority; NEU-008 native ABI; NEU-010 bounded Channels only; NEU-011 one CPU; NEU-021/023/024 scoped safety, containment and tests.
- **Architecture:** [Sections 6–7](../Neu_Kernel_Architecture_v0.2.md#6-capability-security-model), [Section 9](../Neu_Kernel_Architecture_v0.2.md#9-inter-node-communication), [Section 10](../Neu_Kernel_Architecture_v0.2.md#10-native-syscalls-and-abi), [K6 roadmap](../Neu_Kernel_Architecture_v0.2.md#22-development-roadmap-and-acceptance-tests).

## Context

Architecture v0.2 assigns Channels to K6 and capability moves to K7. Phase 5C
introduces a narrow communication mechanism to exercise K5 authority across real
Nodes; it does not complete K6 or accept future capability transport. The current
two-Node/three-Thread profile supplies independent roots, shared same-Node tables,
non-copyable execution leases, and IF=0 native transactions. Its four object
slots are already occupied by two AddressSpaces and two Flag fixtures.

A Channel cannot be a fabricated counter or a cached test record. It needs real
bounded message storage, independent directions, Node-owned endpoints and an
explicit relationship between handle references, operation pins and peer closure.

## Decision

Add one architecture-neutral Channel pool entry, two generation-checked endpoint
slots, and two independent two-scalar FIFO queues. Each message is exactly one
u64; head/tail/length and aggregate accounting belong to the queue, not to an
embedded capability or userspace pointer. ChannelId and EndpointId are distinct
internal Rust types; neither is a userspace handle. Slot generations advance on
reuse and exhausted generations retire capacity instead of wrapping.

A ChannelEndpoint object owns a non-copyable endpoint token. Its exact owning
NodeId is checked both when granting authority and when operating. A1/A2 share
Node A's endpoint through its actual table; B1 owns the opposite endpoint in
Node B's separate table. Rights are object-specific SEND (32), RECEIVE (64), or
both (96); Counter READ/WRITE and AS INSPECT never authorize IPC. A checked guard
also cannot amplify the rights requested by its lookup.

Use six object metadata slots for the Channel profile, precisely the four
existing objects plus two real endpoint objects. Preserve a four-slot allocation
limit in legacy Objects::new profiles even though storage can hold six. Retain
eight handles per Node. Extend the sealed manifest storage to eight entries but
keep its legacy effective bound at four. The trusted pair constructor alone
extends it to eight, preserving meanings 0–3 and installing indices 4/5 as two
independent SEND|RECEIVE handles, 6 as SEND-only and 7 as RECEIVE-only. These
separate initial grants are trusted fixtures, not public derivation.

Construction validates both actual Node generations/tables, no running lease,
manifest state, all eight needed handle slots, two object slots, Channel capacity
and endpoint generations before mutation. With exclusive bootstrap access,
publication then has no fallible allocation or unchecked user input. No partial
pair is published on ordinary failure, no existing messages are changed, and
failed preflight consumes no generations. Original object/handle references and
creator references are accounted separately. Startup ends with six objects,
sixteen handles/references, two endpoints, one Channel, four scalar slots and two
retained AS roots. There is no public channel_create or direct cross-Node grant.

Add experimental ABI 0.3, feature mask 0x3f, using 0x13 channel_send and 0x14
channel_receive. Preserve query=1, exit=2, operations 0x10–0x12 and ABI 0.1/0.2
version/feature results in their configurations. The new Channel bit is 32.
Return distinct Empty (-32), Full (-33), PeerClosed (-34) and internal terminal
state (-35), with zero scalar on errors. Send success returns zero; receive
success returns the scalar, including zero. See the [0.3 specification](../abi/experimental-v0.3.md).

Each successful push/pop has one commit point in the exclusive transaction.
Diagnostic failure after that point is fail-stop; it must never turn a committed
operation into a returned operational failure that invites an unsafe retry.
Ordering follows successful commits, not intended Thread scheduling. Empty/full,
owner/type/rights/generation failures leave both queues and prior messages intact.
No operation blocks, spins waiting for a peer, or changes a Thread to a wait state.

Closing one handle releases one reference. The endpoint stays live while another
handle/creator or authorized operation pin remains. Last reference and pin release
retires it once: discard only messages addressed to that dead receiver, preserve
already queued messages for the surviving receiver, and reject future sends to
the dead receiver. A survivor drains its inbound queue before empty receive
reports PeerClosed. Both retired endpoints imply no queued messages and release
the Channel slot. New lookups through closed handles fail; close is not revocation.
Peer references are generation checked before queue mutation.

Store Channel state inside the same Objects owner as endpoint values. Native
entry passes its exclusive registry and non-copyable table lease to the IPC
adapter; no suspended mutable Nodes, Paging or allocator pointer is dereferenced.
Rust disjoint field borrows allow guard-pinned queue mutation without a second
mutable registry alias. Diagnostics use a separate concrete exclusive IF=0 gate;
no borrowed mutable reference escapes to Ring 3. The scoped immutable paging view
and root-reference rules from 0016 remain intact. IRQ/NMI handlers never access
Channel/capability state. This is not an SMP or kernel-preemptible contract.

On normal Thread exit, retain Node authority for siblings. On terminal Node
exit/fault, return execution leases, cancel continuations, drain the actual Node
table, retire its endpoint and account queued/discarded scalars, reap AS roots,
then reclaim page tables and Node metadata. Peer queues and endpoint references
are independent of the dead Node's page-table root. No ownership cycle is created.

## Alternatives considered

- **Reuse one of the four live objects.** Would overwrite existing AS/Flag
  authority or fake an endpoint instead of representing real communication state.
- **Grow every profile's usable pools.** Would silently change legacy capacity
  and rollback contracts; explicit profile limits preserve those negatives.
- **One shared queue.** Conflates directional ownership/backpressure and FIFO order.
- **Buffers, shared MemoryObjects or capabilities in messages.** Introduces
  unrelated copy, mapping, transfer and revocation contracts beyond scalar IPC.
- **Blocking IPC or a kernel retry loop.** Requires scheduler wait queues and
  wake/cancel semantics; nonblocking scalar results permit bounded user retries.
- **Destroy on any handle close, or flush both queues.** Breaks independent
  references and discards valid messages owned by a surviving receiver.
- **Use a mutable controller alias from native entry.** IF=0 is not a Rust
  ownership argument; the existing moved table and owned registry suffice.

## Consequences

The accepted subset provides real capability-authorized communication across
independent roots with auditable fixed storage and scalar ABI semantics. It has
no public creation, quota governance, dynamic topology, blocking, transfer,
derivation, revocation, variable-length payloads, shared memory or SMP. Completing
K6 needs broader lifecycle/backpressure/stress evidence and a separate decision
before adding blocking semantics. K7 moves and K10 revocation remain deferred.

## History

- **2026-10-26:** Selected and implemented for Phase 5C. Preserve architecture
  v0.2 and all prior ADR rationale; actual validation belongs in
  [implementation status](../IMPLEMENTATION_STATUS.md).

- **2026-10-26 validation:** 159 host tests, 104 QEMU scenarios, twelve repeated
  interleaving runs and four independent clean-directory QMP builds/boots pass.
  These demonstrate the scoped scalar bridge, not full K6, SMP or hardware safety.

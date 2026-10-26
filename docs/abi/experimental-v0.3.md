# Experimental Neu x86-64 ABI 0.3 — bounded scalar Channels

Status: experimental, implemented by the `channel-test` image family. This is
the Phase 5C bridge to architectural K6, not a frozen production IPC ABI or a
claim that K6 is complete. [ADR 0017](../adr/0017-bounded-scalar-channel-endpoints.md)
records the decision; [implementation status](../IMPLEMENTATION_STATUS.md)
records executed validation and limits.

## Compatibility and calling convention

Retain the [ABI 0.2](experimental-v0.2.md) register/preservation rules: RAX selects
operation and returns signed status; arguments use RDI/RSI/RDX/R10/R8/R9; RDX is
the optional scalar result. RCX/R11 are SYSCALL clobbers, other GPRs and user RSP
are preserved on returning calls. Return uses the checked IRETQ boundary. Unused
arguments carry no authority. No userspace pointers are accepted.

`abi_query=1` returns `(major<<48)|(minor<<32)|features`. Version 0.3 uses features
0x3f and returns `0x000000030000003f`: the previous query/exit/bootstrap/close/
inspect bits 1/2/4/8/16 plus CHANNEL=32 for both scalar operations. Earlier images
retain exactly ABI 0.1/0.2 and feature masks 3/0x1f, rejecting 0x13/0x14 with -1.
Query before using an extension. Published numbers are never reassigned.
Private Phase 5A counter operations and the scheduler's test yield remain private.

| Number | Name | Arguments | Success scalar |
| --- | --- | --- | --- |
| 1 | abi_query | none | 0x30000003f |
| 2 | thread_exit | RDI: status 0–255 | does not return |
| 0x10 | bootstrap_cap_get | RDI: index | opaque existing Node-local handle |
| 0x11 | cap_close | RDI: handle | 0 |
| 0x12 | address_space_inspect | RDI: handle, RSI: page VA | current mapping attributes |
| 0x13 | channel_send | RDI: endpoint handle, RSI: u64 payload | 0 |
| 0x14 | channel_receive | RDI: endpoint handle | received u64 |

Operations 0x10–0x12 retain their original contracts. No channel_create, buffer
copy, blocking flag, capability argument, derive/move or cancellation operation
is defined. Internal ChannelId, EndpointId, object IDs and pointers are not handles
and are never returned through these operations.

## Audited bootstrap grants

Both initial Nodes preserve indices 0–3: own-AS INSPECT, independent own-AS
INSPECT, own-AS NONE and own-Flag READ. The 0.3 constructor adds:

| Index | Grant |
| --- | --- |
| 4 | Own endpoint, SEND|RECEIVE primary handle |
| 5 | Same own endpoint, independent SEND|RECEIVE handle |
| 6 | Same own endpoint, SEND-only handle |
| 7 | Same own endpoint, RECEIVE-only handle |

Endpoint A belongs to Node A (A1/A2); Endpoint B belongs to Node B (B1). These are
four explicit initial references per endpoint, not a public attenuation API.
Bootstrap discovery returns original live handles; closing/reusing a slot does
not replace a closed original or recreate authority. Index >=8 fails; legacy
0.2 profiles still reject index >=4. Userspace must discover opaque handles,
not rely on current slot/generation values. Equal numbers in different Nodes
resolve to those Nodes' own entries; no argument can select a foreign Node table.

The constructor preflights every bounded resource before publication. Startup
uses six objects, sixteen handle references, one Channel, two endpoints and two
queues containing two scalar slots each. AS root references remain separate.

## Nonblocking operations and ordering

`channel_send` requires ChannelEndpoint SEND (internal right bit 32).
`channel_receive` requires ChannelEndpoint RECEIVE (bit 64). Combined authority
is 96. READ/WRITE/INSPECT, zero rights and cross-object combinations do not
implicitly authorize communication. The kernel validates Node/table/handle
identity, object generation/type/rights, exact endpoint owner and Channel/peer
generations before changing queue state. Authority comes from trusted active
Node/Thread/SpaceId/root and native/scheduler/stack bindings, not unused registers.

Each endpoint sends into the opposite endpoint's inbound queue and receives from
its own inbound queue. Each direction has capacity two. A successful send commits
one scalar; successful receive removes exactly the oldest committed scalar.
Independent directions do not impose a global message order. Scalar zero is a
valid message: success RAX=0, RDX=0 differs from Empty RAX=-32, RDX=0.

Diagnostic publication after a committed operation cannot change its result into
a returned error. Diagnostic failure stops the kernel instead of permitting a
retry of an allegedly uncommitted operation.

Full send and empty receive return immediately. They never overwrite queued
values or invent messages. All validation/operational failures return scalar
zero and leave prior queue contents intact. Userspace may retry after a later
scheduling opportunity; this does not implement blocking or kernel wait queues.

## Close, peer closure and lifetime

Close releases one handle reference. Independent references keep the endpoint
live. A successful guard pins its endpoint through the entire operation, even
if its final handle was closed by that authorized guard. Final reference/pin
release retires the endpoint, closes its receiving side and discards undeliverable
messages addressed to it. Messages already queued for the surviving peer remain
receivable. The survivor drains them in FIFO order; only then does empty receive
return PeerClosed. Send to a closed peer returns PeerClosed without mutation.

Both retired endpoints release the Channel and all remaining queue resources.
Closed/stale handles remain invalid through slot/object/endpoint/Channel reuse;
generations do not wrap. Closing a handle is not revocation or capability transfer.
Normal Thread exit retains its Node's endpoint while a sibling survives. Terminal
Node cleanup drains that Node's handles after execution leases end, accounts
queued/discarded values, and preserves the healthy peer independently of roots.

| Signed RAX | Meaning |
| --- | --- |
| 0 | Success |
| -1 | Unsupported operation/configuration |
| -2, -16 through -24 | Existing ABI 0.2 argument/capability/inspection meanings |
| -32 | Own inbound queue empty while peer is live |
| -33 | Peer inbound queue full |
| -34 | Peer closed (for receive, own inbound already drained) |
| -35 | Internal terminal/inconsistent Channel identity |

No valid endpoint handle remains after both endpoints retire; ordinary attempts
through old handles fail as stale/expired authority. -35 guards a terminal
mechanism identity, not an alternate unchecked ChannelId interface.

## Execution and limits

Operations run at IF=0 on one CPU and have one queue commit point. Endpoint
objects, references, pins and Channel queues share the exclusively borrowed
registry. No borrowed mutable reference escapes to Ring 3, no suspended controller
alias is accessed, and no IRQ/NMI handler reads or mutates this state. This does
not claim SMP or kernel-preemptible safety.

The test image retains ABI 0.2's 96-record base-operation budget. Channel
diagnostics allow 110 native Channel/endpoint-close requests plus two terminal
cleanup records; exhaustion returns Capacity before mutation. These diagnostic
bounds are not resource-pool governance. The fixture has at most 96 dispatches;
older profiles retain their previous bound and the 30-second guest deadline.

The fixture's common-boundary pulse and gate words pace user tests once per
slice, ensure full-queue observations and order terminal tests. They carry no
message payload or authority and are not part of the ABI. Messages still cross
Nodes only through actual authorized enqueue/dequeue. QMP reads coherent paused
CPU frames and supervisor FIFO storage; serial banners alone are not evidence.

Deferred: public creation, blocking/waits, priority donation, public Node/Thread
creation, capability transport/moves, derivation, revocation, MemoryObjects,
variable-length or zero-copy payloads, generic pointer copying, ELF runtime,
SMP and Linux/POSIX compatibility.

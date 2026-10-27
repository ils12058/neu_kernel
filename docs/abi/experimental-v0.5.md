# Experimental Neu x86-64 ABI 0.5 — move-only capability messages

Status: experimental; implemented by the cap-move-test image family. This is a
bounded Flag/Counter movement subset, not a frozen production ABI.
[ADR 0020](../adr/0020-atomic-capability-moves.md) records its ownership rationale.

## Version, features and registers

`abi_query=1` returns `(major<<48)|(minor<<32)|features`. Version 0.5 advertises
CAP_MOVE=0x80 plus the existing 0x7f features, yielding `0x00000005000000ff`.
ABI 0.1/0.2/0.3/0.4 images retain their exact masks (3/0x1f/0x3f/0x7f), versions
and unsupported-operation behavior. Query before using an extension. Published
numbers are never reassigned; future compatible additions require feature
discovery and documented argument/result semantics.

RAX selects the operation and returns signed status. RDI/RSI/RDX/R10/R8/R9 are
scalar argument registers; RDX is the result. RCX/R11 are architectural SYSCALL
clobbers. Other GPRs and the user RSP are preserved on returning calls, using
the existing checked IRETQ path. Error results have RDX=0. No pointers are accepted.

| Number | Operation | Arguments | Success |
| --- | --- | --- | --- |
| 1 | abi_query | none | RDX=0x5000000ff |
| 2 | thread_exit | RDI=status 0–255 | no return |
| 0x10 | bootstrap_cap_get | RDI=index | existing opaque Node-local handle |
| 0x11 | cap_close | RDI=handle | RDX=0 |
| 0x12 | address_space_inspect | RDI=handle, RSI=user page VA | mapping attributes |
| 0x13 | channel_send | RDI=endpoint handle, RSI=u64 | RDX=0 |
| 0x14 | channel_receive | RDI=endpoint handle | scalar payload |
| 0x15 | channel_send_wait | RDI=endpoint handle, RSI=u64 | RDX=0, may suspend |
| 0x16 | channel_receive_wait | RDI=endpoint handle | scalar payload, may suspend |
| 0x17 | channel_send_cap | RDI=endpoint handle, RSI=source handle | RDX=0 |
| 0x18 | channel_receive_cap | RDI=endpoint handle | fresh recipient-local handle |

Operations through 0x16 retain the [0.4 contracts](experimental-v0.4.md).
Capability send/receive are nonblocking. There is no wait flag, deadline, buffer,
recipient NodeId, object ID or physical address argument.

## Grants and transfer authority

The fixture retains eight handles and bootstrap indices 0–7. Index 3's Flag.READ
entry is explicitly transfer-eligible. Index 4's own ChannelEndpoint has
SEND|RECEIVE|SEND_CAPS (internal bits 32|64|128); index 5 remains SEND|RECEIVE,
6 SEND-only and 7 RECEIVE-only. Existing AddressSpace indices 0–2 retain their
meanings. Both initial Nodes get their own distinct objects and grants.
Bootstrap discovery returns existing handles and never recreates moved or closed
authority. Equality of numeric handles in different Nodes does not confer access
to another Node's objects. Caller identity comes from trusted execution state.

Source transfer eligibility is separate private metadata, not an implied READ or
WRITE right. Trusted bootstrap may grant eligible Flag.READ and Counter READ,
WRITE or READ|WRITE entries. A recipient preserves the exact original object
rights and eligibility; no derive or attenuation operation is exposed.
AddressSpace, ChannelEndpoint and other control/device/memory types cannot move.

## Send, delivery and capacity

A successful 0x17 atomically invalidates exactly the source handle and queues
one owned reference to the same object generation, with unchanged rights. It
does not revoke independent references. Full, peer close and authorization
failures leave the source valid and the FIFO untouched.

A successful 0x18 consumes the capability at the FIFO head and installs it in
the caller's own table using that table's next reusable slot generation. It
does not copy a sender handle. If the table is full (including generation-retired
slots), Capacity leaves the token queued and at the head. Close a suitable local
handle and retry; no recipient slot or authority is reserved by send.

Scalar and capability messages share each direction's existing two-slot FIFO.
A receive of the wrong kind returns MessageKind without removing or skipping
the head. Zero and full-width scalar values remain ordinary scalar messages.
Blocking scalar receive also returns MessageKind at a capability head rather
than parking or interpreting authority as data. An already blocked scalar
receiver is eligible for wake on cap enqueue and rechecks at settlement.
Capability dequeue creates capacity for an existing blocked scalar sender;
wake is a notification and settlement rechecks before committing.

Closing a received handle uses ordinary reference accounting. A sender's later
exit does not undo queued ownership. Final receiver endpoint retirement discards
its inbound capability tokens, releasing their references once. The surviving
peer drains its own pending inbound messages before PeerClosed. Close is not
revocation, and sending is not a promise of eventual delivery.

## Status meanings

| Signed RAX | Meaning |
| --- | --- |
| 0 | success |
| -1 | unsupported operation/configuration |
| -2 | existing argument rejection |
| -16 | malformed/unallocated handle |
| -17 | stale/closed/moved handle generation |
| -18 | missing or inappropriate object-specific rights |
| -19 | wrong/unsupported object type |
| -20 | expired object generation/reference |
| -21 | wrong/terminal table or endpoint owner |
| -22 | bounded capacity or accounting counter exhausted |
| -23 | existing in-use/registration failure |
| -24 | existing invalid mapping inspection |
| -32 | empty while peer lives |
| -33 | full peer inbound FIFO |
| -34 | peer closed; receive's own inbound is already drained |
| -35 | terminal/inconsistent Channel state |
| -36 | FIFO head has the other message kind |
| -37 | source entry is not explicitly transfer-eligible |

## Fixture-only operation and bounds

`0xfffffffffffffc00` is a private, feature-gated test read: RDI=Flag handle,
RAX=status, RDX=actual Flag value. It uses the same typed lookup/READ guard as
host tests. It exists to verify exercise of received authority without inventing
a public generic object ABI. It is not advertised by a feature bit and is rejected
by older images. No Counter manipulation or public factory is exposed by it.

The image retains six objects, eight handles per Node, two FIFO slots per
direction, three waits, the 112-record IPC diagnostic bound, existing Channel
128-dispatch guard, and 30-second guest deadline. A reused 170-word (1360-byte)
supervisor witness adds before/after table, token and object-accounting evidence;
it is not a queue, quota or public ABI structure. Its addresses are read by QMP
only while stopped at the native probe. The fixture includes three initial
computation slices per member to permit direct CPU observations.

All operations and ownership changes are single-CPU IF=0 transactions. No Rust
borrow persists across a context switch, and IRQ/NMI does not touch the registry.
Deferred: capability copying, public derivation/attenuation, revocation, more
transferable object classes, blocking moves, public factories, shared memory,
SMP, general kernel preemption, deadlines and POSIX/Linux compatibility.

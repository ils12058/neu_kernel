# Experimental Neu x86-64 ABI 0.6 — derived authority and revocation

Status: experimental; implemented by `authority-test` profiles. This extends
[ABI 0.5](experimental-v0.5.md) in the bounded K10 scheduler environment.
[ADR 0030](../adr/0030-bounded-capability-derivation-and-revocation.md) records
the semantics and [K10 completion](../K10_COMPLETION.md) records validation.

## Version and register contract

ABI_QUERY (`RAX=1`) returns success and `RDX=0x00000006000001ff`: minor version
6, earlier feature bits `0xff`, and AUTHORITY=`0x100`. Earlier image families
retain their exact ABI values. Query before using these operations.

RAX selects the call and returns signed status; RDI/RSI/RDX/R10/R8/R9 carry
scalar arguments. RDX returns the scalar result and is zero on error. The
existing native SYSCALL/validated IRETQ convention and preserved GPR/stack
contract remain unchanged. A suspended call preserves its full continuation.

| Number | Operation | Inputs | Success |
| --- | --- | --- | --- |
| `0x19` | CAP_DERIVE | RDI=source handle; RSI=requested rights; all remaining arguments zero | RDX=fresh local handle |
| `0x1a` | CAP_REVOKE | RDI=source handle; all remaining arguments zero | RDX=number of affected live descendant grants at begin |

Nonzero reserved arguments return InvalidArgument (`-2`) before handle lookup.
No call accepts a user pointer, Node ID, object ID, grant ID, mapping descriptor
or physical address. Caller identity comes from the executing Thread. Old
profiles reject both numbers with NotSupported (`-1`) and RDX=0.

## Rights and derivation

The existing access rights retain their bit values: READ=1, WRITE=2, MAP=4,
TRANSFER=8, INSPECT=`0x10`, SEND=`0x20`, RECEIVE=`0x40`, SEND_CAPS=`0x80`.
DERIVE=`0x100` and REVOKE=`0x200` are new independent management rights.
The valid authority encoding contains only bits `0x3ff` and must be nonzero.
The earlier rights decoder is unchanged.

CAP_DERIVE requires DERIVE on the source. Every requested bit must already be
present, and the subset must be appropriate for its object type. Counter access
uses READ/WRITE; Flag uses READ; AddressSpace uses INSPECT; ChannelEndpoint uses
SEND/RECEIVE/SEND_CAPS; MemoryObject uses READ/WRITE/MAP/TRANSFER. Existing
AddressSpace grants may have no access bits. Management rights never imply
object access or movement eligibility.

Derivation leaves the source intact and installs one independently closable
handle in the caller's table. The handle carries the same object generation,
the exact requested rights and inherited transfer eligibility. No bootstrap
manifest entry is added. Full tables, exhausted lineage storage and generation
or reference-counter exhaustion return Capacity without partial publication.

Delegation to another Node uses CHANNEL_SEND_CAP/CHANNEL_RECEIVE_CAP with
their existing FIFO and atomic move semantics. A moved grant retains its
lineage; delivery changes its Node-local handle, not its rights or ancestry.

## Revocation, close and mapping completion

CAP_REVOKE requires REVOKE. It withdraws the source's strict descendants,
including descendants whose handles have closed but still authorize mappings.
The source itself, an independent grant to the same object, and branches
outside the chosen subtree survive. The success count is the number of live
descendant lineage entries selected at begin, not a count of handles or pages.
Revoking a source with no descendants succeeds with zero.

At begin, affected grants stop authorizing lookup, further derivation, delivery
or new mappings. The transaction removes affected handles, discards affected
queued capabilities and cancels their pending Channel operations. A blocked
operation returns Revoked (`-38`) with zero scalar; readiness does not permit it
to consume a message after revocation. Surviving FIFO messages retain order.

If mappings exist, CAP_REVOKE blocks the requester. The controller removes
each affected PTE and waits for every targeted CPU's actual invalidation before
retiring mapping/backing ownership. Success means all selected mappings have
completed this process. A requester cancelled by its Node's fault stays
cancelled while the controller completes reclamation. A hardware timeout fails
stop with backing retained; it does not produce a successful return.

One revocation can be active at a time. A second revoke returns InUse (`-23`).
Derivation from the active source also returns InUse; revoked descendants
cannot be used. After completion, the surviving source may derive fresh grants.
Closing a handle preserves its descendants and mappings. Closing/moving is
never an implicit revoke. Already copied data cannot be withdrawn.

## Error precedence and bounds

After reserved-argument validation, CAP_DERIVE decodes rights, validates the
local handle/object/lineage, checks authority/subset/type, preflights local
capacity and asks lineage to derive. CAP_REVOKE validates the handle/object/
lineage and REVOKE, preflights queue discard accounting and begins its epoch.

| Signed RAX | Meaning relevant to K10 |
| --- | --- |
| `-1` | Operation unsupported by this ABI profile |
| `-2` | Nonzero reserved argument |
| `-16` | Malformed or never allocated local handle |
| `-17` | Stale, closed, moved or purged handle generation |
| `-18` | Unknown/empty/inappropriate rights, missing management right or amplification |
| `-19` | Wrong object type |
| `-20` | Invalid object generation/reference |
| `-21` | Wrong or terminal owner/table |
| `-22` | Bounded capacity, reference counter or generation exhausted |
| `-23` | Active revocation or conflicting use |
| `-38` | Authority revoked, including a cancelled pending operation |

Handle/grant generation zero is invalid; exhausted 32-bit generations never
wrap. Revocation and shootdown epochs are checked 64-bit counters. Limits are
64 lineage entries, 64 mapping bindings, eight handles per Node, six capability
objects, two FIFO entries per direction and three waits. These are experimental
implementation limits, not a stable allocation ABI.

K10 exposes no new factory, direct remote-table edit or public map/unmap call.
The test bootstrap supplies independent roots and initial tracked mappings.
K11 device primitives, general mapping APIs and physical hardware remain
outside this ABI's accepted scope.

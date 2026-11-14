# Experimental Neu x86-64 ABI 0.7 — device primitives

Status: experimental; implemented by `device-test` profiles. This extends the
scalar register convention and derived authority of [ABI 0.6](experimental-v0.6.md)
on the bounded SMP scheduler. [ADR 0031](../adr/0031-bounded-userspace-device-primitives.md)
defines the trusted backend; [K11 completion](../K11_COMPLETION.md) records
acceptance. This is a selected QEMU EDU driver interface, not a general PCI ABI.

## Version, arguments and results

ABI_QUERY (`RAX=1`) returns success and `RDX=0x00000007000003ff`: minor version
7, previous feature bits `0x1ff`, and DEVICE=`0x200`. Earlier image families keep
their previous version and reject `0x1b` through `0x21` with NotSupported (`-1`)
and RDX=0. Query the active image before using the interface.

RAX selects the operation and returns signed status. RDI/RSI/RDX/R10/R8/R9
carry six scalar arguments; RDX carries the scalar result and is zero on error.
The existing validated SYSCALL/IRETQ and preserved-register/stack contract is
unchanged, including a blocked call's saved continuation. All unused arguments
of the seven new operations must be zero; nonzero reserved inputs return `-2`
before capability lookup. No operation accepts a user pointer, physical address,
Node ID, raw object ID, PCI address or interrupt-controller configuration.

| RAX | Operation | Used arguments, in register order | Successful RDX |
| --- | --- | --- | --- |
| `0x1b` | DEVICE_INSPECT | resource handle, field | Requested resource field |
| `0x1c` | DEVICE_CLAIM | resource handle | Fresh MmioRegion handle |
| `0x1d` | DEVICE_INTERRUPT | original MmioRegion handle | Already installed original Interrupt handle |
| `0x1e` | MMIO_READ | MmioRegion handle, byte offset, width | Zero-extended 32-bit register value |
| `0x1f` | MMIO_WRITE | MmioRegion handle, byte offset, width, value | 0 |
| `0x20` | INTERRUPT_WAIT | Interrupt handle | Delivered nonzero sequence; may block |
| `0x21` | INTERRUPT_ACK | Interrupt handle, delivered sequence | 0 |

DEVICE_INSPECT fields are 0: PCI ID `0x11e81234`; 1: BAR size `0x100000`;
2: supported operations mask `7` (mediated register read, write and interrupt);
3: current device phase. Other fields return `-2`. The phase values are absent
0, free 1, awaiting rearm 2, armed 3, operation in flight 4, pending interrupt 5
and quarantined 6. Phase inspection is informational; it grants no lease.

## Typed authority and claims

The three object types are DeviceResource=6, MmioRegion=7 and Interrupt=8.
Access rights use existing bit positions with object-specific meanings:

| Type | Access rights | Initial grant |
| --- | --- | --- |
| DeviceResource | CLAIM=`4`, INSPECT=`0x10` | `0x314` |
| MmioRegion | READ=`1`, WRITE=`2` | `0x303` |
| Interrupt | WAIT=`1`, ACKNOWLEDGE=`2` | `0x303` |

Initial grants also carry DERIVE=`0x100` and REVOKE=`0x200`. At least one
type-appropriate access bit is required. CAP_DERIVE (`0x19`) retains its exact
rights-subset rules; management rights never imply access. The trusted bootstrap
places the resource in manifest index 5 of each Node. A handle is meaningful
only in its own Node table. Device authority has no TRANSFER right and is not
eligible for Channel capability movement.

DEVICE_CLAIM requires CLAIM and exclusive ownership of a free resource. It
atomically publishes a MmioRegion/Interrupt pair with a new nonzero 64-bit
device generation. A failure leaves no provisional handle or object authority;
generations consumed during provisional allocation are never reused. Existing
32-bit handle/object/grant generation rules still apply. A second claim returns
Busy while ownership is active and Quarantined while cleanup is incomplete.

DEVICE_INTERRUPT requires READ on the original MmioRegion handle returned by
claim. It returns the original Interrupt handle without creating more authority.
A derived READ-only MmioRegion cannot use this operation to obtain WAIT/ACK
rights. Its caller must instead receive an explicitly derived Interrupt handle
within the same Node.

Every register/interrupt call validates type, rights, live local generation,
executing Node and active device generation. The claiming resource handle and
original pair are lifecycle anchors; a blocked wait also records its exact
grant. Closing any MmioRegion/Interrupt handle, closing the claiming handle,
revoking a selected anchor or blocked-wait grant, or stopping the owner Node
quarantines the entire claim. Closing a separate, unused resource handle does
not release the claim.

CAP_REVOKE still selects strict descendants and reports its existing selected
grant count. Quarantine is an additional terminal device effect, not a change
to ordinary handle-close or revocation semantics. After hardware retirement,
all remaining MmioRegion/Interrupt descendants of that device generation are
removed. Independent DeviceResource grants remain usable for another claim.

## Exact register policy

The width is exactly 4 bytes. Offsets must be aligned and exactly allowlisted;
values must fit in u32 without truncation. The kernel performs one volatile
32-bit access after checking authority and protocol state.

| Offset | Read | Write |
| --- | --- | --- |
| `0x00` | EDU identification `0x010000ed` | Denied |
| `0x04` | Liveness register | Any u32; EDU stores its bitwise inverse |
| `0x08` | Factorial register | Operand 0–12; requires armed state and hardware status exactly `0x80` |
| `0x20` | Factorial status | Exactly `0x80`, enabling completion interrupt |
| `0x24` | Interrupt status | Denied |
| `0x60` | Denied | Exactly `2`, raising the manual interrupt |

All other offsets are denied, including acknowledgement `0x64` and DMA
`0x80`–`0x98`. Register writes require armed state; reads may inspect an active
claim while arming, computing or awaiting acknowledgement. Reads do not wait
for computation and do not renew the watchdog. A triggering write records the
expected completion bit: factorial=1, manual=2. There is at most one in-flight
command. The backend owns acknowledgement and all PCI/I/O APIC configuration.

## Interrupt delivery, cancellation and bounds

EDU uses Q35 INTx/GSI 21 and BSP vector 240. Hard IRQ entry masks the route
before EOI and publishes the generation/status event without taking the
scheduler lock. The BSP consumes it under the normal shared transaction.

INTERRUPT_WAIT supports one blocked Thread or one receiving Thread. With no
event it saves the continuation and blocks. A matching IRQ wakes that Thread;
an IRQ arriving first remains pending until a later wait. A second waiter or
wait while a receiver still owns delivery returns Busy. Sequence numbers are
checked and increase across successful deliveries, including later claims.

INTERRUPT_ACK requires ACKNOWLEDGE, the receiving Thread and its exact nonzero
sequence. The backend verifies device status, clears the delivered bits, checks
the readback and only then permits BSP rearm. A stale, duplicate, fabricated or
foreign-Thread ACK returns Sequence. A hardware mismatch quarantines the claim.

Each claim permits 64 successful register reads/writes. Denied calls consume no
budget. The next otherwise valid register operation after exhaustion returns
Capacity (`-48`) and quarantines the lease. An 80-BSP-timer-tick watchdog bounds
claim, wait, command and acknowledgement progress. Successful protocol progress
sets the next deadline; polling and rejected requests cannot extend it. Ticks
are LAPIC events with a nominal 10 ms scheduling quantum, not a wall-clock SLA.
The BSP also arms its timer while idle with outstanding device work.
Claim, acknowledgement and quarantine transitions use the existing generation-
checked wake/IPI path to notify the BSP, including when a new claim arrives on
an AP after the free-device BSP has disabled its idle timer.

Close/revoke cancellation returns Quarantined (`-46`) to a surviving blocked
waiter; watchdog cancellation returns Timeout (`-47`). A waiter cancelled by
Node termination is never resumed. During quarantine, operations cannot touch
hardware and another Node cannot claim the resource. The backend masks IRQs,
waits for any old command's actual completion status/event, drains pending
interrupts and scrubs liveness/factorial data. The neutral factorial(0) reset
must itself raise completion while delivery is masked; only then are its status
and interrupt-enable bit cleared. COMPUTING=0 alone is insufficient for either
the old command or this reset because EDU can raise completion after clearing
that flag. Reuse requires no
computation, pending status, LAPIC event, remote IRR or mailbox event. Failure to
prove quiescence retains quarantine and authority retirement remains pending.

## Errors and implementation limits

Existing capability errors keep their meanings: `-16` malformed, `-17` stale,
`-18` rights, `-19` type, `-20` object, `-21` owner, `-22` allocation capacity,
`-23` conflicting use and `-38` revoked authority. New device errors are:

| Signed RAX | Meaning |
| --- | --- |
| `-40` | Busy claim, waiter or receiving Thread |
| `-41` | Device unavailable |
| `-42` | Wrong owner/device generation or inactive lease |
| `-43` | Invalid interrupt sequence, receiver or protocol state |
| `-44` | Device not ready for the requested operation |
| `-45` | Denied register offset, width or value |
| `-46` | Quarantined claim or cancelled device wait |
| `-47` | Blocked device wait cancelled by watchdog |
| `-48` | Exhausted register-operation budget or device counter capacity |

These profiles have nine capability-object slots, eight handles per Node,
64 lineage entries, one claimed device and one IRQ waiter. Earlier images
retain six object slots and their existing bounds. The common environment
remains two/four CPUs, two Nodes, three Threads, fixed pools and integer
contexts. This interface exposes no raw writable device mapping, port I/O,
PCI configuration, DMA, IOMMU, MSI/MSI-X, device enumeration or driver loader.
The first MMIO page is supervisor RW/NX/UC in every relevant root; its DMA
registers cannot be separated by page permissions. Bus mastering stays disabled.

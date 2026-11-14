# K11 device primitives: independent QMP acceptance

`tools/device_qmp.py` extends the [SMP observer](smp-qmp.md). It verifies actual
PCI configuration, EDU registers, I/O APIC/LAPIC state, normalized interrupt
frames, capability ownership and syscall continuations. Serial success text
alone cannot pass. [ABI 0.7](../abi/experimental-v0.7.md) defines the interface;
[K11 completion](../K11_COMPLETION.md) records accepted evidence and limitations.

```sh
python3 tools/neu.py test --case smp-device-4
python3 tools/test_device_qmp.py
```

The guest deadline remains 30 seconds. Device cases add exactly
`-device edu,id=edu0,addr=05.0` to QEMU 11.0.0 TCG/q35. The disabled case and all
legacy profiles retain their existing machine configuration without EDU.

## Required scenarios

| Case | CPUs | Required additional behavior |
| --- | --- | --- |
| `smp-device-2` | 2 | Exclusive claim, liveness, blocking manual IRQ, pending factorial completion, close and reassignment |
| `smp-device-4` | 4 | Same flow with idle/unrelated online CPUs |
| `smp-device-fault` | 4 | A2 faults by writing supervisor MMIO; A1 is cancelled, B1 reclaims a quiescent device |
| `smp-device-flight-fault` | 4 | Fault while a submitted factorial command is still pending in the controller; late completion stays with the old generation |
| `smp-device-close` | 2 | A1 closes A2's actual blocked IRQ-wait handle; A2 resumes with `-46` |
| `smp-device-revoke` | 4 | A1 revokes the original IRQ's strict descendant used by A2's blocked wait; count=1 and resumed status=`-46` |
| `smp-device-timeout` | 4 | A2 leaves a delivered IRQ unacknowledged; the real BSP timer reaches the watchdog deadline |
| `smp-device-budget` | 2 | Repeated authorized reads reach the 64-operation limit; the next read returns `-48` |
| `smp-device-disabled` | 2 | ABI 0.5 remains unchanged; every Thread rejects all seven new operations |

All enabled cases retain native/timer probes, actual same-Node CPL3 overlap,
saved GPR reconstruction, private stacks, fault containment and final exact
allocator/page-table accounting. A2's CPL3 driver derives resource/IRQ authority
and reads all four resource-inspection fields through an INSPECT-only child,
rejects an invalid field, and demonstrates wrong rights, wrong type, DMA offset,
wrong width, truncation,
reserved-input and sequence denials. B1's concurrent claim returns `-40`; an
intentionally leaked scalar handle returns stale (`-17`) in B's local table.

A2 first blocks before A1 raises a manual IRQ through MMIO_WRITE. Normal cases
acknowledge sequence 1, submit factorial(6), observe sequence 2 already pending,
then wait, read 720, acknowledge and close the claiming anchor. Close/revoke
cases observe a second blocked wait separately before A1 cancels it. Fault
cases require actual CPL3 `#PF`, error 7, at `0xffffb00000001000`. B1 survives,
claims generation 2, checks liveness, raises/receives/acknowledges a real IRQ
and calculates factorial(5)=120 before closing the lease. Surviving Threads
exit with code 128.

## Hardware and ownership observations

Before user execution the observer reads actual ECAM PCI identity, BAR, command,
pin and MSI control, using the Q35 PCIEXBAR to verify the ECAM aperture. It
requires the exact `1234:11e8` EDU at 00:05.0 and its aligned 1 MiB BAR. Port
decoding, bus mastering and MSI must stay disabled; memory decoding is enabled
only until final device cleanup. All four 64-bit DMA registers must be zero.

EDU permits only 32-bit accesses below offset `0x80`. QEMU HMP can combine
`xp /4wx` into an 8-byte callback, which EDU rejects. The observer therefore
requests **one** `xp /1wx` per ordinary register and `xp /1gx` per DMA register.
It never substitutes an expected value for an unsuccessful hardware read.
After final PCI memory decoding is disabled, registers are no longer readable;
the final record contains no invented register values. Actual zero DMA and
scrubbed data are required at the immediately preceding terminal observation.

Actual page-table walks verify supervisor RW/NX/UC EDU and I/O APIC leaves in
the kernel and both user roots, with no user permission in the translation
chain. The existing full root walk rejects unexpected user aliases. I/O APIC
GSI 21 must target the BSP at vector 240, active-low and level-triggered. All
unrelated I/O APIC entries remain at their masked baseline.

Each hardware IRQ has a separate probe. The observer reads the actual 22-word
normalized stack frame and compares it to the recorded frame address, CR3,
vector 240, CPL and saved GPRs. The real I/O APIC low word changes from `0xe0f0`
(in service, remote IRR set) to `0x1a0f0` (masked, EOI completed). The matching
LAPIC vector must no longer be in service, and the mailbox must contain the
exact generation and actual device status. Missing any required IRQ fails.

The first blocked wait, normal factorial pending-before-wait, and first
quiescent release each require their own actual BSP probe RIP. Both generations
also require a separate neutral-reset completion probe: factorial=1, status=0x80,
IRQ status=1, masked route with no remote IRR and an empty mailbox. The actual
reset raise must precede clearing status/IRQFACT and releasing ownership. Close/revoke
require a separate observation of the second blocked wait; the first cannot
satisfy both. The factorial checkpoint is generation 1, sequence 2, status 1
and result 720. A later owner's manual IRQ cannot satisfy it. Reassignment is
allowed only after the release checkpoint proves liveness/status zero,
factorial 1, masked/no-remote-IRR route and retired hardware epoch.

## Independent transaction model

The 64-word `neu_device_layout` locates production state rather than copying
expected results: the 24-word Device, claim handles/grants, atomic mailbox,
timer ticks, epoch, journal and existing lineage layouts. While all CPUs are
stopped and the shared lock is free, the observer reconciles actual tables,
handle generations, object kinds, device generations and every live lineage
owner. No device capability may become transferable. A blocked device Thread
must really be blocked in the scheduler with a live generation-bound WAIT
handle; it cannot be inferred from a printed message.

`neu_device_operations` holds at most 512 records of 32 words. Each contains
operation category, CPU/Node/Thread, native-entry identity, six arguments,
RAX/RDX result and the resulting device generation, phase, waiter/receiver,
sequence, status, budget, deadline and counters. Categories distinguish native
calls, wake, cancellation without resumption, IRQ consumption, rearm,
quarantine, neutral-reset completion, release and Node authority drain.

The device profiles reserve 512 entry/IRQ trace rows per CPU so that APs retain
every continuation while the BSP holds the IRQ/reset observation windows and
the watchdog expires. Legacy profiles retain their 256-row allocation. The
observer checks the exact profile-specific layout and rejects counters beyond
either fixed allocation before reading records. No record is dropped or wrapped.

The Python model independently computes local allocation, exact rights,
register allowlists, liveness inversion, factorial values, exclusive claims,
IRQ ordering, budgets and cancellation. It matches each record to actual
SYSCALL arguments and each returning call to the resumed normalized GPR frame.
A cancelled Thread must have no continuation; all other returning calls need
one. The complete native trace must contain exactly the modeled calls.

The in-flight fault case captures the old generation's factorial IRQ while its
event is still pending. Its Node faults before controller consumption. The
quarantine record must still carry flight=1; later consumption records status 1
in quarantine without delivering another sequence to either Node. This proves
that event retirement outlives the faulted requester.

The fault profiles place B1 on CPU2. When the old owner is gone, the BSP can
be idle with no device timer armed. B1's new claim and later ACK must notify
the BSP through the existing generation-checked wake/IPI protocol; merely
changing the shared device phase cannot make an idle CPU perform rearm.

At terminal observation each enabled profile has exactly two claims, two
quarantines and two verified releases. Normal cases have four actual IRQs
and four ACKs; the in-flight fault has four IRQs and three ACKs; other profiles
have three IRQs, with ACK counts determined by their cancellation path. The
two masked neutral-reset completions must never produce hardware IRQ delivery. Final
cleanup requires no device epoch/mailbox, no supervisor device views, PCI
decoding disabled, the original masked I/O APIC route restored, no live
capability/lineage/wait state and exact allocator/table conservation.

## Regression tests and retained evidence

Rust host tests cover resource/offset/width/value bounds, owner/generation
rejection, exclusive claims, pending versus blocked delivery, duplicate/wrong
ACKs, every quarantine cause, late completion, safe release prerequisites,
budget/deadline exhaustion, typed rights and two-stage capability-allocation
rollback. ABI tests retain rejection by the legacy dispatcher.

Thirteen Python adversarial tests reject fabricated MMIO success/results, foreign
handles, rights amplification, state/budget changes, missing/mismatched native
frames, enabled bus mastering/MSI, upper-half DMA state, wrong routes, invalid
EDU access width, missing lifecycle windows and a neutral reset that has only
cleared COMPUTING without raising its actual masked completion. They run in both `make check`
and full `make test` alongside the earlier 29 observer regressions.

Full and repeated evidence must be copied before another run overwrites
`build/test-results.json` or `build/test-logs/`. The frozen source inventory
includes newly added source files, every build/observer input and exact image
hashes. The final audit rechecks raw retained conservation and hardware
observations, full/repeated image identity and process cleanup. No hardware,
hosted CI, general PCI routing, DMA isolation or production-driver acceptance
is implied by these local QEMU tests.

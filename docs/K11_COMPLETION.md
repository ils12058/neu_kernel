# K11 completion contract — device primitives

Status: **accepted for bounded x86-64/QEMU (2026-11-14)**.

K11 adds a bounded userspace driver path on the existing x86-64 SMP execution
model: one QEMU EDU (`1234:11e8`) at Q35 PCI 00:05.0, two Nodes, three Threads,
two or four CPUs, fixed pools, integer contexts and IRQ-disabled shared
transactions. A driver must claim the device exclusively, use typed capabilities
for exact register access, receive a real hardware interrupt through a blocking
wait, acknowledge the device, and survive the loss of a driver through contained
cleanup and safe reassignment.

The DMA controls share EDU's first register page. Consequently K11 keeps that
page supervisor-only, UC and NX and exposes mediated 32-bit register operations.
PCI bus mastering stays disabled. The backend configures the Q35 INTx route
through I/O APIC GSI 21 to BSP vector 240; MSI stays disabled and interrupt
configuration is never userspace authority. This initial routing choice is
recorded in [ADR 0031](adr/0031-bounded-userspace-device-primitives.md).

K11 starts from completed K10 commit
`40df53f2f20ad9aa85b1b20334dec464933f7985` on
`codex/k11-device-primitives`. Architecture v0.2 remains the baseline.
[Experimental ABI 0.7](abi/experimental-v0.7.md) defines the seven new scalar
operations, typed rights, register allowlist, error precedence and bounds.
[The device QMP protocol](testing/device-qmp.md) defines independent acceptance;
the K9/K10 observers and earlier ABI versions remain in force.

Required acceptance:

- Generation-checked exclusive claims and typed DeviceResource, MmioRegion and
  Interrupt handles; rights attenuation, denial and finite-capacity rollback.
- Exact offset, width, value, owner and lifecycle checks before every register
  access. No DMA-register, arbitrary PCI, port-I/O or raw writable device map ABI.
- Real EDU liveness/factorial/manual interrupt operations from CPL3, one waiter,
  pending-before-wait and wait-before-pending behavior, sequence-checked ACK,
  masked delivery, bounded operation budget and watchdog quarantine.
- Close, descendant revocation, Node termination and deliberate driver fault
  cancel pending work; the IRQ is masked and the hardware is quiescent before
  another claim can be accepted. Stale generations never reach hardware.
- An independent QMP observer checks PCI configuration, device registers,
  supervisor translations, actual interrupt frames/EOI, capability and scheduler
  state, every returning syscall, survivor progress and exact final accounting.
- Meaningful host and observer adversarial tests, unchanged legacy acceptance,
  full QEMU matrix at the existing 30-second deadline, same-image repeats,
  formatting/clippy/default build and a frozen source/image evidence audit.

## Implemented contract

The trusted BSP discovers exactly one EDU, validates its slot, memory BAR,
Q35/ICH9 identities and unshared GSI, and programs the level-triggered route.
DeviceResource authority permits an exclusive claim. Claim atomically publishes
generation-bound MmioRegion and Interrupt handles; derivation can attenuate
rights without allowing a read-only MMIO child to obtain interrupt authority.
The original claiming handle and both lease handles anchor lifecycle ownership.
Device authority cannot move between Nodes.

The device state machine separates free, awaiting rearm, armed,
in-flight, pending and quarantined states. A real IRQ masks before EOI,
publishes its hardware generation/status and never takes the shared lock.
The BSP consumes it and either wakes one actual blocked Thread or leaves the
sequence pending. ACK is bound to the receiving Thread and exact sequence;
status must clear before rearm. There are 64 successful register operations per
lease and an 80-BSP-timer-tick watchdog. Polling/denied requests cannot renew it.

Close, selected descendant revocation and Node loss immediately deny the old
claim. A surviving waiter receives its terminal result once; a terminated Node
never resumes. BSP cleanup outlives the driver. A pending factorial command
must finish its actual completion raise, not merely clear COMPUTING. IRQ status,
remote IRR, LAPIC IRR/ISR and the mailbox drain before reuse. Liveness is scrubbed
to 0 and factorial to 1 using a neutral factorial(0). Delivery remains masked
until its actual completion raise has been independently observed, then status
and IRQFACT are cleared. This prevents the neutral reset itself from raising
an interrupt after a new owner enables factorial completion.
All remaining typed lease handles are retired before the next claim can run.
If quiescence cannot be proven, the device stays quarantined.

The normal CPL3 driver calculates factorial(6)=720 and performs liveness and
manual-interrupt operations. The fault fixture writes the supervisor device
address and produces actual user #PF/error 7, cancels its sibling and leaves
the other Node running. The in-flight fault fixture requires the old command's
IRQ to remain pending during the fault, then observes its retirement without
delivering it to the new generation. Every enabled profile completes a second
claim in Node B, calculates factorial(5)=120 there and verifies final cleanup.

## Validation evidence

The complete final matrix and every required repeat passed on frozen sources.

| Check | Accepted result | Retained evidence |
| --- | --- | --- |
| Rust host tests | 339/339 in each of default, authority and device configurations | `build/k11-full-test.log`, `build/k11-host-authority-test.log`, `build/k11-host-device-test.log` |
| Python regressions | 42/42: 17 scheduler/accounting, 12 authority, 13 device | `build/k11-check.log`, `build/k11-full-test.log` |
| Formatting and clippy | All 198 profile selections and host library pass | `build/k11-check.log` |
| Complete QEMU matrix | 198/198 registered profiles, 30-second deadline | `build/k11-full-test-results.json`, `build/k11-full-test-evidence/` |
| Same-image repeats | All nine K11 profiles pass again | `build/k11-repeat-results.json`, `build/k11-repeat-evidence/` |
| Toolchain and default build | Doctor/build pass; default image equals the full-run boot image | `build/k11-doctor.log`, `build/k11-build.log` |
| Source/image/conservation audit | 94 frozen inputs, 198 image hashes, 29 scheduled profiles, every required device/reset observation and nine repeats verified | `build/k11-validation-summary.json` |

The source inventory is `build/k11-validation-source-manifest.json`; exact
copies are in `build/k11-source-snapshot/`. Aggregate SHA-256:
`55ead8fc0a45a71c3f2e0d7a4fddc9a5f524f7f87df82837c4bbd72953d66ba8`. Default kernel SHA-256:
`522c2dc9622b34e04ee5bb5513f1d7c428092e57c281beac1741bced43f74964`. The longest full-run guest took
21.078 seconds; the longest repeat took 10.466 seconds. No project QEMU process
remained after validation.

The audit checks the registered case list, boot/firmware/loader hashes, every
image/capture identity and raw retained hardware/resource conservation. Full
evidence is preserved before repeated runs overwrite ordinary test logs. The
11-run `build/k11-focused-05/` suite and final inspection/watchdog checks in
`build/k11-focused-07/` are development evidence, additional to the accepted
full matrix and repeats.

## Development corrections retained with the evidence

- `build/k11-development/001-observer-schema/` retains initial bring-up where
  the old generic observer expected six object slots. The device-only schema
  has nine; earlier images retain six.
- `build/k11-development/002-factorial-policy/` retains the independent
  oracle's rejection of an accidental factorial offset `0x28`. The actual
  register is `0x08`; the policy and host test were corrected, and the Python
  regression explicitly rejects `0x28`.
- `build/k11-focused-01/` retains four passing profiles followed by the close
  profile's failed factorial checkpoint. It selected sequence 2 from the
  second owner's manual IRQ. Selection now requires generation 1 and factorial
  status 1; the observer still requires the actual pending result 720. The
  second close/revoke blocked wait also has its own mandatory observation.
  The complete replacement focused suite passed without a longer deadline.
- `build/k11-focused-03/` retains two passing strengthened-reset cases followed
  by a fault-profile timeout. A same-image diagnostic run in
  `build/k11-debug-fault/` captured generation 2 awaiting rearm, its new owner
  on CPU2, and the BSP halted with no device timer. Claim/ACK transitions now
  notify BSP service through the existing generation-checked wake/IPI path;
  quarantine transitions also publish that work. The diagnostic record
  distinguishes this missing wakeup from reset completion or capability denial.
- `build/k11-focused-04/` retains six passing profiles and a watchdog-profile
  trace-capacity failure. The diagnostic replay in `build/k11-debug-timeout/`
  shows both device generations fully released, but an AP exhausted the old
  256-row trace allocation before user completion. The added BSP IRQ/reset
  windows let APs accumulate more timer continuations. Device profiles now
  reserve 512 rows per CPU, and the observer still requires every native/IRQ
  frame and rejects allocation overflow. Legacy allocations remain 256; the
  watchdog, register budget and guest deadline are unchanged.
- `build/k11-focused-06/` retains a final operation-coverage check that wrongly
  treated a blocked wait's initial suspension as its final result. Coverage now
  uses the independently modeled resumed result; native-frame matching still
  requires the real continuation. All four DEVICE_INSPECT fields and invalid
  field rejection are exercised from a derived INSPECT-only userspace handle.

The first full run was deliberately stopped after 66 passing cases when source
review identified the neutral-reset completion race. That is a review-triggered
restart, not a claimed guest regression. The interrupted run, source snapshot
and passing captures are retained in
`build/k11-development/full-attempt-1-review-stop/`. The replacement requires
two actual masked reset-completion observations and the new owner's own
factorial request. The old command's status also remains intact until any
already queued LAPIC/mailbox event has drained.

QEMU source inspection also ruled out MSI with bus mastering disabled:
QEMU sends MSI through `bus_master_as`. The chosen backend therefore uses
Q35 INTx while retaining disabled bus mastering. The source references are
QEMU v11.0.0 `hw/misc/edu.c`, `hw/pci/pci.c`, `hw/pci/msi.c` and
`hw/isa/lpc_ich9.c`; local inspected copies are retained under `build/k11-*-v11.0.0.c`.

## Scope after K11

General PCI enumeration/routing, MSI/MSI-X, IOMMU/DMA,
automated restart, hardware beyond EDU and a stable public driver ABI remain
outside this initial milestone. These are limitations of the implementation,
not completed hardware validation.

The driver is a static userspace payload using mediated accesses, not a general
driver loader or direct MMIO map. The kernel still uses fixed pools, integer
contexts and non-preemptible shared transactions. Public object/mapping/Thread
factories, arbitrary AP allocation, load balancing, FPU/SIMD/TLS scheduling,
COW and demand paging remain future work. Physical devices and hosted CI are
not covered by local host/QEMU evidence. The architecture roadmap ends at K11;
it does not define or approve a K12 or imply that a complete production OS exists.

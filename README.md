# Neu Kernel

Neu is a kernel-only project with a Rust-based, capability-oriented hybrid
architecture and x86-64 as its initial target under QEMU.

## Project status

**PCI discovery → IOMMU isolation → authorized DMA is complete for the bounded
x86-64/QEMU platform.** Reusable ACPI/PCI code discovers functions, bridges,
capabilities and BARs. The Q35 backend selects EDU dynamically, including an
alternate slot and a secondary PCIe bus. Intel VT-d constrains the requester to
explicitly mapped buffers before bus mastering is enabled.

Experimental ABI 0.8 adds generation-bound DmaDomain/DmaBuffer capabilities and
seven scalar operations. Userspace performs real DMA in both directions across
noncontiguous physical pages, using the existing blocking IRQ/ACK protocol.
Actual IOMMU faults prove rejection of unmapped, read-only, other-requester and
revoked translations. Close, descendant revocation and Node fault are tested
while hardware RUN is still active; ownership persists through completion,
invalidation and verified host/device-buffer scrub before reassignment.

Validation passes **368 Rust tests in each of four configurations; 52 Python
observer regressions; all 215 QEMU profiles; and all 17 new profiles repeated
using the exact same images**. Formatting, lint for all 215 profiles and the host
library, doctor and default build pass. The audit verifies 106 frozen inputs, image and
firmware hashes, native/IRQ/DMA evidence, and exact final resource conservation
in all 45 scheduled profiles. Earlier K11 evidence is preserved.

[PCI/DMA completion](docs/PCI_DMA_COMPLETION.md) records scope, measured results,
hashes and development corrections. [ABI 0.8](docs/abi/experimental-v0.8.md),
[ADR 0032](docs/adr/0032-pci-iommu-authorized-dma.md) and
[the DMA QMP protocol](docs/testing/dma-qmp.md) define the interface and evidence.
This is post-K11 work; the architecture roadmap has not assigned a K12.

K11 supplies exclusive device claims, mediated registers, real INTx delivery and
safe reassignment; its ABI 0.7 profiles retain disabled DMA. K10 supplies derived
authority and remote TLB invalidation, K9 supplies SMP scheduling and K8 shared
MemoryObjects. Their dated completion records and older ABI contracts remain.

Executable acceptance uses QEMU 11.0.0 TCG/q35, Intel legacy VT-d, EDU,
two/four CPUs, two Nodes, three Threads, fixed pools and integer contexts.
Discovery is reusable; interrupt routing and the DMA engine remain platform
and device specific. MSI/MSI-X delivery, interrupt remapping, AMD IOMMU,
hotplug/AML, ATS/PASID/SVA, general driver loading, public factories,
balancing, kernel preemption, FPU/SIMD/TLS, COW and demand paging remain future
work. Physical hardware and hosted CI have not been validated.

Default and legacy images use one vCPU; SMP profiles select 1, 2 or 4. Normal
profiles use 512 MiB, and scheduling stress uses 1 GiB. The verified host is
Apple Silicon with Rust 1.95.0, Python 3.14.7, UEFI/OVMF and Limine 12.9.4/base 6.

## Build and run

Requirements: Rust 1.95.0 with `x86_64-unknown-none`, Python 3, `curl`, Make,
`qemu-system-x86_64`, and a compatible x86-64 OVMF code/variables pair. See
[implementation status](docs/IMPLEMENTATION_STATUS.md) for setup, tested versions,
firmware discovery, and limitations.

```sh
make doctor
make build
make run
make test
make check
```

The first build downloads the pinned Limine binary and checks its SHA-256. The
boot artifact is `build/boot/esp/`; QEMU serves a private copy as a read-only FAT
disk. No ISO tools, graphical display, kernel filesystem, or custom bootloader
are required. `make run` stays halted after successful bootstrap; Ctrl-C stops
QEMU. `make test` runs host memory/paging/lifecycle/ABI/timer tests, observes each
guest's expected result, active CR3 and halt through QMP, then shuts QEMU down
itself. Run `python3 tools/neu.py test --case channels` for real bidirectional
scalar IPC, `--case channels-xapic` for its controller fallback,
`--case channels-a-fault` or `--case channels-b-fault` for peer/message cleanup,
`--case channels-rollback` for atomic construction failure, and
`--case address-space-channel-disabled` for legacy ABI 0.2 rejection.
Run `--case channels-wait` for blocking receive/wake, `--case channels-wait-send`
for blocking send/backpressure, `--case channels-wait-fifo` for multiple waiters,
`--case channels-wait-close-receive` or `--case channels-wait-close-send` for peer
closure, `--case channels-wait-fault` for Node-fault cancellation, or
`--case channels-wait-deadlock` for bounded all-blocked quiescence.
`--case channels-legacy` rechecks immediate ABI 0.3 behavior, and
`--case native-abi04-disabled` checks legacy rejection of the blocking operations.
Run `--case cap-moves` for Ring 3 move, Full rollback, recipient-capacity retry
and mixed FIFO evidence; `--case cap-moves-sender-fault-before` and
`--case cap-moves-sender-fault` for pre/post-commit sender faults;
`--case cap-moves-receiver-fault-after` for post-delivery cleanup;
`--case cap-moves-double` for repeated ownership transfer; and
`--case cap-moves-backpressure-wait` (or `-xapic`) for mixed FIFO plus blocking
scalar wait/wake/settlement. `--case cap-moves-discard` and `--case cap-moves-xapic`
retain their receiver-cleanup and controller-fallback profiles. The
`native-move-send-disabled`,
`native-move-receive-disabled`, `address-space-move-disabled`,
`channels-move-disabled` and `channels-wait-move-disabled` images test rejection
of movement by earlier ABI versions.
Run `python3 tools/neu.py test --case address-space` for live ABI 0.2
inspection, `--case address-space-a2-fault` for capability/root teardown,
`--case address-space-rollback` for failed grants and pool exhaustion, or
`--case native-abi02-disabled` for ABI 0.1 rejection of new operations.
Run `python3 tools/neu.py test --case capabilities` for live authorization,
`--case capabilities-a-fault` for Node fault cleanup, `--case capabilities-rollback`
for bounded exhaustion, or `--case native-cap-disabled` for private-op rejection.
Run `python3 tools/neu.py test --case threads` for shared-Node scheduling,
`--case threads-xapic` for its controller fallback, `--case threads-a2-fault`
for Node-wide sibling cancellation, `--case threads-rollback` for partial member
construction, or `--case threads-stale` for generation reuse.
Run `python3 tools/neu.py test --case preempt` for the retained two-Thread profile,
`--case preempt-xapic` for its controller fallback, `--case preempt-native` for
IF-enabled native entry, or `--case preempt-a-fault` for fault containment.
Run `python3 tools/neu.py test --case switch` for cooperative switching,
`--case switch-a-fault` for suspended-sibling containment, or
`--case switch-rollback` for construction failure. Earlier `timer`, `timer-xapic`,
`nodes`, `native` and `user` cases remain available with the same command.

Run `python3 tools/neu.py test --case memory-object-user-share` for real Ring 3
stores/loads through one shared two-page object, `--case memory-object-user-readonly-fault`
for the hardware write-protection `#PF` and containment, `--case memory-object-user-a-exit`
for Node A's exit while B still reads, or `--case memory-object-user-xapic` for
the live contract on the legacy controller fallback. These four cases fail unless
their stopped-CPU QMP evidence matches the raw production state.

Run `python3 tools/neu.py test --case smp-bootstrap-1`, `--case smp-bootstrap-2`
or `--case smp-bootstrap-4` to boot the genuine 1-, 2- or 4-vCPU SMP profiles;
the tool selects the matching `-smp` count. `--case smp-bootstrap-reject` boots
four vCPUs against the 2-CPU image and must fail the count check before any AP
is released. `--case smp-cpu-identity` and `--case smp-ap-isolation` re-run the
4-CPU path for their named observer checks. Every SMP case fails unless the
stopped-CPU QMP evidence matches the published per-CPU blocks, live registers,
descriptor tables, local-APIC state and translations.

Run `python3 tools/neu.py test --case smp-work-2` for sequential work on one
AP, `--case smp-work-4` for distinct parallel commands on three APs,
`--case smp-work-overlap` for the two-CPU synchronized-gate overlap proof,
`--case smp-work-repeat` for repeated commands on one AP, or
`--case smp-work-invalid` for the six negative protocol checks. Run
`--case smp-ipi-2` or `--case smp-ipi-4` for IPI-driven wakeup with per-AP
vector 0xF0 accounting, exactly one EOI per IPI and zero spurious samples.
Each case fails unless the stopped-VM QMP oracle matches the raw mailbox,
journal, gate, evidence and live-register state.

Run `python3 tools/neu.py test --case smp-user-2` or
`--case smp-user-overlap` for the retained static two-CPU Ring 3 contract;
`--case smp-user-ap-fault` contains a real AP protection fault while its
peer completes.

Run `python3 tools/neu.py test --case smp-schedule-2` or
`--case smp-schedule-4` for scheduled Ring 3 execution on two/four CPUs.
`--case smp-schedule-shared` checks actual concurrent atomic increments;
`--case smp-schedule-migrate` checks A2's saved context on CPU1 then CPU3.
`--case smp-schedule-idle` uses four CPUs for blocked-CPU STI/HLT wakeup and
cross-CPU Channel transactions; `--case smp-schedule-ipc` checks scalar/Flag
transactions while same-Node Threads run on two CPUs. Use
`--case smp-schedule-fault` for running-sibling cancellation,
`--case smp-schedule-reject` for malformed user-return containment, and
`--case smp-schedule-stress` for longer scheduling/migration with native/IRQ
probe pauses disabled.

Run `python3 tools/neu.py test --case smp-tlb-2` or `--case smp-tlb-4` for
actual permission removal, remote invalidation and a denied hardware store.
`--case smp-tlb-invalid` checks 22 inert mapping/coordinator rejections before
the real transaction. Every scheduled profile requires independent CPU,
frame, queue, authority and final allocator/table evidence.

Run `python3 tools/neu.py test --case smp-authority-2` or `--case smp-authority-4`
for Ring 3 derivation/revocation, four mapping retirements and denied stale
hardware access. `--case smp-authority-queued` checks discarded delegation;
`--case smp-authority-close` checks mappings that outlive their handle;
`--case smp-authority-wait` checks revoked blocked operations; and
`--case smp-authority-stress` repeats derivation after revoke.
`--case smp-authority-terminate` cancels the blocked requester through a real
sibling fault and requires reclamation to finish. `--case smp-authority-disabled`
checks ABI 0.5 rejection. Every page/CPU invalidation requires an actual
post-instruction, pre-ACK observation, followed by exact final conservation.

Run `python3 tools/neu.py test --case smp-device-2` or `--case smp-device-4`
for actual userspace claims, register operations, blocking IRQs, factorials and
safe reassignment. `--case smp-device-fault` contains a direct-MMIO protection
fault; `--case smp-device-flight-fault` also retires an old in-flight command.
`--case smp-device-close` and `--case smp-device-revoke` cancel actual blocked
IRQ waits. `--case smp-device-timeout` and `--case smp-device-budget` enforce
progress limits; `--case smp-device-disabled` verifies legacy ABI rejection.
These original K11 profiles use EDU with DMA disabled. Each requires actual
hardware IRQ and masked-reset completion observations, every returning syscall
and exact final device/allocator/authority cleanup.

Run `python3 tools/neu.py test --case smp-dma-4` for actual authorized DMA,
`--case smp-dma-slot` or `--case smp-dma-bridge` for dynamic PCI placement,
`--case smp-dma-requester` for a different requester's rejected hardware write,
and `--case smp-dma-submit-revoke` for revocation during active DMA. The
[DMA test protocol](docs/testing/dma-qmp.md) lists all 17 new profiles, including
no-IOMMU and ABI 0.5/0.7 controls. DMA profiles add Intel VT-d; ordinary K11
profiles keep bus mastering disabled.

## Documentation

- [Architecture and design specification](docs/Neu_Kernel_Architecture_v0.2.md)
- [Accepted PCI/IOMMU/DMA completion contract](docs/PCI_DMA_COMPLETION.md)
- [Experimental DMA ABI 0.8](docs/abi/experimental-v0.8.md)
- [DMA QMP evidence](docs/testing/dma-qmp.md)
- [Accepted K11 completion contract](docs/K11_COMPLETION.md)
- [Experimental device ABI 0.7](docs/abi/experimental-v0.7.md)
- [Device QMP evidence](docs/testing/device-qmp.md)
- [Accepted K10 completion contract](docs/K10_COMPLETION.md)
- [Experimental authority ABI 0.6](docs/abi/experimental-v0.6.md)
- [Accepted K9 completion contract](docs/K9_COMPLETION.md)
- [Experimental blocking Channel ABI 0.4](docs/abi/experimental-v0.4.md)
- [Experimental capability-move ABI 0.5](docs/abi/experimental-v0.5.md)
- [Experimental scalar Channel ABI 0.3](docs/abi/experimental-v0.3.md)
- [Experimental native ABI 0.2](docs/abi/experimental-v0.2.md)
- [Architecture Decision Records](docs/adr/README.md)
- [SMP bootstrap, coordination and scheduling QMP evidence](docs/testing/smp-qmp.md)
- [MemoryObject QMP evidence](docs/testing/memory-object-qmp.md)
- [Implementation status, commands, and verification](docs/IMPLEMENTATION_STATUS.md)

ADRs explain why architectural choices exist. Implementation documentation
describes how the current kernel works and what has actually been tested.

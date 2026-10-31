# Neu Kernel

Neu is a kernel-only project with a Rust-based, capability-oriented hybrid
architecture and x86-64 as its initial target under QEMU.

## Project status

K9 / Phase 9A adds a bounded SMP bootstrap foundation. The pinned Limine 12.9.4
SMP request discovers the configured 1-, 2- or 4-vCPU topology; the BSP
publishes a fixed per-CPU array, releases each application processor into a real
kernel entry stub with its own stack and private GDT/TSS/IDT, and each AP
validates its CPUID identity, privileged state, tables, GS-base block and
canaries before switching to the verified owned root and parking with
interrupts disabled. The BSP independently verifies every AP's publication and
the whole-VM QMP oracle reads production blocks, live vCPU registers,
descriptor tables, local-APIC state and address translations while the VM is
stopped. **Multiple CPUs enter the kernel; one CPU still owns kernel state.**
Every existing scheduler, capability, paging, Channel, frame-allocator,
preemption, timer and serial subsystem remains BSP-only.

K8 / Phase 8C remains the latest Node/capability milestone: a running Node maps
a received MemoryObject capability at a scheduler safe point. Trusted setup
creates one two-page object; Node A sends the capability over the existing
Channel `SEND_CAP`/`RECEIVE_CAP` operations; Node B receives the exact identity
and rights, issues a bounded `TEST_MAP` request, is suspended with its Ring 3
context owned by the scheduler, and the kernel installs a new mapping from the
received handle before resuming it.

**Nodes execute. Capabilities authorize. MemoryObjects share real backing. APs
enter, prove themselves and park.**

Current validation is **253 host tests and 156 QEMU scenario executions** (154
unique: the 150-scenario Phase 8C baseline plus `smp-bootstrap-1`,
`smp-bootstrap-2`, `smp-bootstrap-4` and `smp-bootstrap-reject`;
`smp-cpu-identity` and `smp-ap-isolation` re-execute the 4-CPU path for their
named observer checks). The live, transfer and runtime cases and every SMP
profile require stopped-CPU QMP evidence; serial output alone is insufficient.
[Implementation status](docs/IMPLEMENTATION_STATUS.md) records the evidence and
observer corrections. [ADR 0026](docs/adr/0026-smp-bootstrap-and-percpu-foundations.md)
covers the SMP bootstrap and per-CPU foundations; [ADR 0025](docs/adr/0025-runtime-memoryobject-mapping.md)
covers the scheduler safe point, suspended-boundary runtime mapping and
terminal cancellation; [the SMP QMP protocol](docs/testing/smp-qmp.md) and
[the MemoryObject QMP protocol](docs/testing/memory-object-qmp.md) explain the
raw production-state checks.

Architecture v0.2 and [experimental ABI 0.5](docs/abi/experimental-v0.5.md) remain
the baselines. K7 capability movement and Channel behavior retain their earlier
contracts in [ADR 0020](docs/adr/0020-atomic-capability-moves.md) and
[ADR 0021](docs/adr/0021-capability-transfer-lifecycle-hardening.md). Public
MemoryObject syscalls, a public mmap/munmap API, derivation, revocation,
concurrent Ring 3 scheduling, per-CPU run queues, migration, AP preemption, IPI
and TLB shootdown, concurrent allocation or capability mutation, COW and demand
paging remain future work.

The supported execution model is one CPU owning kernel state, user-only LAPIC
preemption, IF=0 kernel transactions, integer-only execution, fixed metadata
pools and guarded private Thread stacks. Legacy images boot one vCPU; the SMP
profiles boot the configured 1, 2 or 4 vCPUs. The platform remains QEMU 11.0.0
TCG/q35, 512 MiB, UEFI/OVMF, Rust 1.95.0 and Limine 12.9.4/base 6 on Apple
Silicon.

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

## Documentation

- [Architecture and design specification](docs/Neu_Kernel_Architecture_v0.2.md)
- [Experimental blocking Channel ABI 0.4](docs/abi/experimental-v0.4.md)
- [Experimental capability-move ABI 0.5](docs/abi/experimental-v0.5.md)
- [Experimental scalar Channel ABI 0.3](docs/abi/experimental-v0.3.md)
- [Experimental native ABI 0.2](docs/abi/experimental-v0.2.md)
- [Architecture Decision Records](docs/adr/README.md)
- [SMP bootstrap QMP evidence](docs/testing/smp-qmp.md)
- [MemoryObject QMP evidence](docs/testing/memory-object-qmp.md)
- [Implementation status, commands, and verification](docs/IMPLEMENTATION_STATUS.md)

ADRs explain why architectural choices exist. Implementation documentation
describes how the current kernel works and what has actually been tested.

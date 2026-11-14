# Neu Kernel

Neu is a kernel-only project with a Rust-based, capability-oriented hybrid
architecture and x86-64 as its initial target under QEMU.

## Project status

**K9 / Phase 9 (9A–9D) is complete for bounded x86-64/QEMU SMP:** at most
four CPUs, two Nodes and three Threads. In the scheduled SMP images, every
CPU runs the same synchronized scheduler with a local FIFO queue, explicit
Thread ownership and affinity. Same-Node Threads run concurrently through
one root and capability table with separate guarded stacks. Native syscall
and calibrated 10 ms LAPIC timer boundaries preserve full integer contexts.

Shared lifecycle, capability and Channel transactions use one explicit lock
with IF=0; IRQ handlers publish CPU-local state without acquiring that lock.
Queued work migrates with its saved context, and blocked Channel continuations
wake exactly once through generation-checked IPI/idle transitions. The BSP
owns allocation, root preparation, final teardown and serial.

Node faults and unsafe user return frames quiesce running siblings before
reclamation while an unrelated Node completes. A live write-permission change
uses an epoch-bound remote TLB protocol: every target executes its actual
invalidation or root flush before acknowledging. Final teardown restores the
exact allocator bitmap and original page-table inventory, with zero remaining
user roots, pins, references, authority or waiters.

K8 / Phase 8C supplies runtime mapping from a received MemoryObject capability
at a scheduler safe point. K7 capability movement and Channel behavior retain
their contracts in [ADR 0020](docs/adr/0020-atomic-capability-moves.md) and
[ADR 0021](docs/adr/0021-capability-transfer-lifecycle-hardening.md);
[ADR 0025](docs/adr/0025-runtime-memoryobject-mapping.md) covers the suspended
runtime mapping boundary and terminal cancellation.

Current validation passes **306 Rust host tests, eight Python observer tests
and 181 registered QEMU executions**, plus **five focused SMP repeats**.
Formatting, every-feature/host-library clippy and the default build pass;
the default kernel image matches the full regression's boot image. Live,
transfer, runtime and every SMP profile require stopped-CPU QMP evidence.
[Implementation status](docs/IMPLEMENTATION_STATUS.md) records the accepted
evidence and preserved observer correction;
[the K9 completion contract](docs/K9_COMPLETION.md) defines the bounded scope.

[ADR 0029](docs/adr/0029-bounded-scheduled-smp.md) covers scheduled SMP,
shared transactions, migration, fault quiescence and remote invalidation.
Earlier contracts remain in [ADR 0028](docs/adr/0028-static-smp-user-execution.md)
for static Ring 3 placement,
[ADR 0027](docs/adr/0027-smp-crosscpu-work-and-coordination.md) for work/IPI
coordination, and
[ADR 0026](docs/adr/0026-smp-bootstrap-and-percpu-foundations.md) for bootstrap.
[The SMP QMP protocol](docs/testing/smp-qmp.md) and
[the MemoryObject QMP protocol](docs/testing/memory-object-qmp.md) explain the
raw production-state checks.

Architecture v0.2 and [experimental ABI 0.5](docs/abi/experimental-v0.5.md)
remain the baselines. The execution model uses fixed metadata pools, guarded
private Thread stacks, integer execution and non-preemptible kernel
transactions. **K10 capability derivation/revocation is next.** Work stealing,
load balancing, general kernel preemption, arbitrary AP allocation, public
MemoryObject/mapping/Thread-creation APIs, TLS/FPU/SIMD scheduling, physical
hardware, COW and demand paging remain later work.

Default and legacy images boot one vCPU; SMP profiles select 1, 2 or 4.
Normal profiles use 512 MiB and the longer four-CPU stress profile uses 1 GiB.
The verified platform is QEMU 11.0.0 TCG/q35, UEFI/OVMF, Rust 1.95.0 and
Limine 12.9.4/base 6 on Apple Silicon.

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

## Documentation

- [Architecture and design specification](docs/Neu_Kernel_Architecture_v0.2.md)
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

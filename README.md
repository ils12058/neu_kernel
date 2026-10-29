# Neu Kernel

Neu is a kernel-only project with a Rust-based, capability-oriented hybrid
architecture and x86-64 as its initial target under QEMU.

## Project status

K8 / Phase 8B makes the MemoryObject a real capability-governed resource.
Trusted setup retains one paging reference, installs a generation-checked
Node-local MemoryObject capability, and maps the object through that capability
before execution. Node A moves the capability over the existing Channel
`SEND_CAP`/`RECEIVE_CAP` operations and exits; Node B receives the exact object
identity and rights. Teardown drains the single reference exactly once, and the
stopped-CPU oracle verifies handles, queue state, rights, mappings, Ring 3
frames and the final allocator/table inventory.

**Nodes execute. Capabilities authorize. MemoryObjects share real backing.**

Current validation is **236 host tests and 143 unique QEMU scenarios**, including
138 earlier regressions, four live Ring 3 profiles, the new
`memory-object-cap` transfer profile and five focused host tests. The live and
transfer cases require stopped-CPU QMP evidence: raw capability slots, Channel
FIFO transfers, physical backing bytes, user PTE rights, saved Ring 3 frames and
exact final allocator restoration (serial output alone is insufficient).
[Implementation status](docs/IMPLEMENTATION_STATUS.md) records the evidence and
observer corrections. [ADR 0022](docs/adr/0022-memoryobject-backing-and-lifetime.md)
covers the backing/lifetime foundation; [ADR 0023](docs/adr/0023-live-userspace-memoryobject-sharing.md)
covers live sharing and fault containment; [ADR 0024](docs/adr/0024-memoryobject-capability-authority-and-transfer.md)
covers capability authority, transfer and cross-registry lifetime; [the QMP
protocol](docs/testing/memory-object-qmp.md) explains the raw production-state
checks.

Architecture v0.2 and [experimental ABI 0.5](docs/abi/experimental-v0.5.md) remain
the baselines. K7 capability movement and Channel behavior retain their earlier
contracts in [ADR 0020](docs/adr/0020-atomic-capability-moves.md) and
[ADR 0021](docs/adr/0021-capability-transfer-lifecycle-hardening.md). Public
MemoryObject syscalls, runtime mapping from a received capability, derivation,
revocation, mmap, SMP, TLB shootdown, COW and demand paging remain future work.

The supported model remains one CPU, user-only LAPIC preemption, IF=0 kernel
transactions, integer-only execution, fixed metadata pools and guarded private
Thread stacks. The platform remains QEMU 11.0.0 TCG/q35, one CPU/512 MiB,
UEFI/OVMF, Rust 1.95.0 and Limine 12.9.4/base 6 on Apple Silicon.

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

## Documentation

- [Architecture and design specification](docs/Neu_Kernel_Architecture_v0.2.md)
- [Experimental blocking Channel ABI 0.4](docs/abi/experimental-v0.4.md)
- [Experimental capability-move ABI 0.5](docs/abi/experimental-v0.5.md)
- [Experimental scalar Channel ABI 0.3](docs/abi/experimental-v0.3.md)
- [Experimental native ABI 0.2](docs/abi/experimental-v0.2.md)
- [Architecture Decision Records](docs/adr/README.md)
- [Implementation status, commands, and verification](docs/IMPLEMENTATION_STATUS.md)

ADRs explain why architectural choices exist. Implementation documentation
describes how the current kernel works and what has actually been tested.

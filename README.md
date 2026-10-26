# Neu Kernel

Neu is a kernel-only project with a Rust-based, capability-oriented hybrid
architecture and x86-64 as its initial target under QEMU.

## Project status

K5 / Phase 5C adds capability-backed Channel endpoints and bounded nonblocking
scalar IPC as a bridge into the architecture's K6 work. A1/A2 share Node A's
endpoint; B1 owns the opposite endpoint. Two independent two-message FIFOs carry
actual scalar messages across isolated address spaces through typed SEND/RECEIVE
authority. Close and Node cleanup preserve messages for a surviving receiver.

**Nodes execute. Capabilities authorize. Kernel objects provide mechanisms.**

The suite passes **159 host tests and 104 QEMU scenarios**, retaining the 129/97
baseline, plus twelve scheduling repeats and four clean-build QMP boots.
[Implementation status](docs/IMPLEMENTATION_STATUS.md) records evidence and limits. [Experimental ABI 0.3](docs/abi/experimental-v0.3.md) adds scalar send
and receive while preserving ABI 0.1/0.2 configurations. [ADR 0017](docs/adr/0017-bounded-scalar-channel-endpoints.md)
records the ownership and lifetime decision. The architecture v0.2 baseline and
accepted ADR history remain intact; K6 is not declared complete.

The supported model remains one CPU, user-only LAPIC preemption, IF=0 kernel
transactions, integer-only execution, fixed metadata pools and guarded private
Thread stacks. Public creation, blocking/wait queues, capability transport,
derivation, revocation, shared memory, SMP, TLS/SIMD and ELF runtime remain deferred.
The platform remains QEMU 11.0.0 TCG/q35, one CPU/512 MiB, UEFI/OVMF, Rust 1.95.0
and Limine 12.9.4/base 6 on Apple Silicon.

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

## Documentation

- [Architecture and design specification](docs/Neu_Kernel_Architecture_v0.2.md)
- [Experimental scalar Channel ABI 0.3](docs/abi/experimental-v0.3.md)
- [Experimental native ABI 0.2](docs/abi/experimental-v0.2.md)
- [Architecture Decision Records](docs/adr/README.md)
- [Implementation status, commands, and verification](docs/IMPLEMENTATION_STATUS.md)

ADRs explain why architectural choices exist. Implementation documentation
describes how the current kernel works and what has actually been tested.

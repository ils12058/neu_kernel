# Neu Kernel

Neu is a kernel-only project with a Rust-based, capability-oriented hybrid
architecture and x86-64 as its initial target under QEMU.

## Project status

K5 / Phase 5A adds a Node-local typed capability foundation. Every Node has a
bounded generation-checked handle table; A1/A2 share Node A's authority and B1
uses a separate table. READ/WRITE rights authorize actual kernel-managed test
objects. Close invalidates one handle, and operation pins protect object lifetime.

**Nodes execute. Capabilities authorize. Kernel objects provide mechanisms.**

The suite contains **113 host tests and 90 QEMU scenarios**, retaining all previous
87/83 regressions. New cases exercise real Ring 3/native capability calls, shared
Thread authority, numeric handle reuse across isolated Nodes, rights/type/stale
failures, close, recycled object/handle generations, fault cleanup and capacity
rollback. [Implementation status](docs/IMPLEMENTATION_STATUS.md) records actual
CPU/object evidence, validation and limits; [ADR 0015](docs/adr/0015-node-local-capabilities-and-typed-objects.md)
records the scoped architectural decision.

These are internal trusted-bootstrap test objects and private test operations.
Experimental ABI 0.1 query/exit and feature results remain unchanged. The profile
retains one CPU, user-only 10 ms provisional preemption, distinct guarded Thread
stacks and a shared Node protection domain. Public creation, capability transfer,
derivation, revocation, IPC, MemoryObjects, SMP and TLS/SIMD remain deferred.
Architecture v0.2 and accepted ADR history are preserved. The validated platform
remains QEMU 11.0.0 TCG/q35, one CPU/512 MiB, UEFI/OVMF, Rust 1.95.0 and Limine
12.9.4/base 6 on Apple Silicon.

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
itself. Run `python3 tools/neu.py test --case capabilities` for live authorization,
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
- [Architecture Decision Records](docs/adr/README.md)
- [Implementation status, commands, and verification](docs/IMPLEMENTATION_STATUS.md)

ADRs explain why architectural choices exist. Implementation documentation
describes how the current kernel works and what has actually been tested.

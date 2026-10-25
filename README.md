# Neu Kernel

Neu is a kernel-only project with a Rust-based, capability-oriented hybrid
architecture and x86-64 as its initial target under QEMU.

## Project status

K4 / Phase 4B adds cooperative FIFO switching between two independently owned
Ring 3 Threads, one per Node. Yield saves a complete user context; the kernel
finishes that slice on its common stack before resuming another Thread. Each
Thread has a private guarded 8 KiB kernel-entry stack and a pinned address space.
Eight A/B dispatches preserve registers and user-stack witnesses, producing 63
and 105 after four computation stages per Thread.

The suite contains **64 host tests and 56 QEMU scenarios**, retaining all previous
52/47 tests. It covers actual suspended-context resumption, private stack/TSS/CR3
selection, fault containment in both directions, cross-Node isolation, guard and
supervisor permissions, invalid saved state, rollback and stale identities.
The environment remains 1-vCPU/512 MiB QEMU 11.0.0 TCG + UEFI/OVMF on Apple
Silicon, Rust 1.95.0 and Limine 12.9.4/base 6.

Switching and userspace retain IF=0; Phase 4A timer tests stay independent.
`abi_query=1`, `thread_exit=2` and ABI 0.1 results/features are unchanged. Yield
uses a private test-only operation without allocating a public ABI number.
Timer-driven preemption, SMP, multiple Threads per Node and capability/factory
interfaces remain unimplemented. See [ADR 0012](docs/adr/0012-cooperative-thread-context-switching.md)
for the decision and Phase 4C boundary. Architecture v0.2 remains the historical
design baseline.

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
itself. Run `python3 tools/neu.py test --case switch` for cooperative switching,
`--case switch-a-fault` for suspended-sibling containment, or
`--case switch-rollback` for construction failure. Earlier `timer`, `timer-xapic`,
`nodes`, `native` and `user` cases remain available with the same command.

## Documentation

- [Architecture and design specification](docs/Neu_Kernel_Architecture_v0.2.md)
- [Architecture Decision Records](docs/adr/README.md)
- [Implementation status, commands, and verification](docs/IMPLEMENTATION_STATUS.md)

ADRs explain why architectural choices exist. Implementation documentation
describes how the current kernel works and what has actually been tested.

# Neu Kernel

Neu is a kernel-only project with a Rust-based, capability-oriented hybrid
architecture and x86-64 as its initial target under QEMU.

## Project status

K4 / Phase 4C adds single-CPU timer-driven **userspace preemption**. Two CPU-bound
Ring 3 Threads run without yielding or making syscalls during the demonstrated
alternations. The LAPIC captures their actual interrupted registers/RIP/stack;
an acknowledged, uniquely owned handoff reaches the common kernel stack before
FIFO selection. Each Thread retains a private guarded 8 KiB entry stack and a
pinned, independently owned Node address space.

The suite now contains **72 host tests and 71 QEMU scenarios**, including all
previous 64/56 regressions. New cases cover x2APIC and UC-mapped xAPIC, exact
context restoration, independent computation, syscall compatibility, faults in
both directions, isolation, invalid publications, handoff misuse, stale IDs and
controlled negative IRQ delivery. See [implementation status](docs/IMPLEMENTATION_STATUS.md)
for actual verification results, CPU observations and limits.

The provisional calibrated clockevent is 10 ms; QEMU evidence establishes
progress and context preservation, not exact timing or hard real time. Selected
userspace runs with IF=1; native entry, scheduler/resource updates and switching
retain IF=0. General kernel preemption, SMP, multiple Threads per Node and public
capability/factory interfaces remain unimplemented. ABI 0.1 query/exit and the
original cooperative test-only yield remain unchanged.

[ADR 0013](docs/adr/0013-single-cpu-user-preemption.md) records the scoped decision.
Architecture v0.2 and previous ADR rationale remain historical baselines. The
validated environment remains QEMU 11.0.0 TCG/q35, one CPU/512 MiB, UEFI/OVMF,
Rust 1.95.0 and Limine 12.9.4/base 6 on Apple Silicon.

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
itself. Run `python3 tools/neu.py test --case preempt` for hardware scheduling,
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

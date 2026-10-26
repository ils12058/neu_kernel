# Neu Kernel

Neu is a kernel-only project with a Rust-based, capability-oriented hybrid
architecture and x86-64 as its initial target under QEMU.

## Project status

K4 / Phase 4D adds multiple independently preemptible Threads in one Node.
Node A's A1/A2 share a SpaceId, CR3, RX code and writable data frame; B1 runs in
an isolated Node B. All three retain distinct guarded user/entry stacks and
complete saved integer contexts. Real LAPIC interrupts drive the ThreadId FIFO.
A normal exit releases only that Thread's stacks; the last live Thread ends its
Node. A supported unrecoverable Node fault cancels its live siblings.

The suite contains **87 host tests and 83 QEMU scenarios**, retaining all previous
72/71 regressions. Twelve new scenarios cover shared atomic computation,
x2APIC/xAPIC, early exit, faults in either A member or B, isolation, guards,
construction rollback, stale identities and selected-Thread return validation.
See [implementation status](docs/IMPLEMENTATION_STATUS.md) for actual verification,
CPU observations, resource accounting and limits.

The provisional calibrated clockevent remains 10 ms. QEMU observations establish
progress and preserved contexts, not exact timing or per-Node fairness. FIFO
schedules Threads; two runnable members can give a Node more aggregate CPU time.
Selected userspace runs with IF=1; native entry and scheduler/resource transactions
retain IF=0. The fixed profile supports two Nodes, three Threads, at most two live
members per Node, and integer-only execution. Same-Node user stacks share a
protection domain. General kernel preemption, SMP, TLS/SIMD, public creation,
capabilities/factories and IPC remain deferred. Experimental ABI 0.1 is unchanged.

[ADR 0014](docs/adr/0014-multiple-threads-in-one-node.md) records the scoped decision.
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
itself. Run `python3 tools/neu.py test --case threads` for shared-Node scheduling,
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

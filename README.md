# Neu Kernel

Neu is a kernel-only project with a Rust-based, capability-oriented hybrid
architecture and x86-64 as its initial target under QEMU.

## Project status

K3 / Phase 3A constructs two simultaneously live bootstrap Nodes with distinct
Node, Thread and address-space identities, private roots and owned user backing.
Their initial Threads run sequentially through trusted kernel control, using the
existing native ABI 0.1 (`abi_query=1`, `thread_exit=2`). Node A computes 42 and
Node B computes 84. A supported user fault ends its own Node; the healthy sibling
can still execute after independent teardown.

Forty-three host tests and forty QEMU scenarios cover the original 32/34 suite
plus both directions of fault containment, same-VA isolation, a B-only mapping,
early sibling reclamation and failed second-Node construction. QMP samples actual
CPL=3 → CPL=0 → CPL=3 and distinct CR3 values for both Threads. The environment
remains 1-vCPU/512 MiB QEMU 11.0.0 TCG + UEFI/OVMF on Apple Silicon, Rust 1.95.0
and Limine 12.9.4/base 6. Loader-reclaimable RAM stays reserved.

The Node/Thread model is kernel-internal and bounded to two of each. Capability
governance, public factories, multiple runnable Threads per Node, scheduling and
ELF loading remain future work. One Thread per Node is a bootstrap policy, not
the permanent object model. Only explicit test configurations launch userspace;
ordinary builds launch no Node. Architecture directions and proposals remain
distinct from implemented features and stable contracts.

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
QEMU. `make test` runs host memory/paging/lifecycle/ABI tests, observes each
guest's expected result, active CR3 and halt through QMP, then shuts QEMU down
itself. See the
implementation status for individual user, paging, memory, and exception
commands. Run `python3 tools/neu.py test --case nodes` for the two-Node proof,
`python3 tools/neu.py test --case native` for the original native ABI proof, or
`python3 tools/neu.py test --case user` for the earlier test-gate fixture.

## Documentation

- [Architecture and design specification](docs/Neu_Kernel_Architecture_v0.2.md)
- [Architecture Decision Records](docs/adr/README.md)
- [Implementation status, commands, and verification](docs/IMPLEMENTATION_STATUS.md)

ADRs explain why architectural choices exist. Implementation documentation
describes how the current kernel works and what has actually been tested.

# Neu Kernel

Neu is a kernel-only project with a Rust-based, capability-oriented hybrid
architecture and x86-64 as its initial target under QEMU.

## Project status

K1 / Phase 1B adds an allocator-backed, kernel-owned four-level page-table root,
a verified CR3 transition, controlled 4 KiB mappings, zeroed frame access, and
guard pages around both kernel stacks. The active hierarchy maps text RX,
constants R/NX, and data/stacks RW/NX with no userspace access or retained HHDM.
Limine 12.9.4 and protocol base revision 6 remain pinned under QEMU + UEFI/OVMF.

Nineteen host tests and eighteen QEMU scenarios cover the original Phase 0/1A
behavior plus paging, rollback, permissions, and all four stack guards on the
documented 1-vCPU, 512 MiB TCG configuration on Apple Silicon. Mapping references
prevent premature frame reuse; unmapping invalidates local translations before
release. Bootloader-reclaimable RAM remains reserved.

Nodes, userspace, capabilities, scheduling, and the other architecture dossiers
remain future work. The architecture document describes design directions and
proposals, not a list of implemented features or stable contracts.

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
QEMU. `make test` runs host memory/paging tests, observes each guest's expected
result, active CR3, and halt through QMP, then shuts QEMU down itself. See the
implementation status for individual paging, memory, and exception commands.

## Documentation

- [Architecture and design specification](docs/Neu_Kernel_Architecture_v0.2.md)
- [Architecture Decision Records](docs/adr/README.md)
- [Implementation status, commands, and verification](docs/IMPLEMENTATION_STATUS.md)

ADRs explain why architectural choices exist. Implementation documentation
describes how the current kernel works and what has actually been tested.

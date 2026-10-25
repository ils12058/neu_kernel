# Neu Kernel

Neu is a kernel-only project with a Rust-based, capability-oriented hybrid
architecture and x86-64 as its initial target under QEMU.

## Project status

K1 / Phase 1A extends the bootable Rust x86-64 foundation with a kernel-owned
memory map, bounded 4 KiB physical-frame allocator, and private GDT/TSS/IDT with a
double-fault IST stack. Frame allocation tracks ownership only; it does not map
or initialize frame contents. Limine 12.9.4 and protocol base revision 6 remain
pinned under QEMU + UEFI/OVMF.

Seven host memory tests and nine QEMU smoke scenarios cover the original Phase 0
behavior, allocation, and controlled exceptions on the documented 1-vCPU,
512 MiB TCG configuration on Apple Silicon. Bootloader-reclaimable RAM remains
reserved. Page-table ownership and controlled mappings belong to Phase 1B.

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
QEMU. `make test` runs host memory tests, observes each guest's expected result
and halt through QMP, then shuts QEMU down itself. See the implementation status
for individual memory and exception test commands.

## Documentation

- [Architecture and design specification](docs/Neu_Kernel_Architecture_v0.2.md)
- [Architecture Decision Records](docs/adr/README.md)
- [Implementation status, commands, and verification](docs/IMPLEMENTATION_STATUS.md)

ADRs explain why architectural choices exist. Implementation documentation
describes how the current kernel works and what has actually been tested.

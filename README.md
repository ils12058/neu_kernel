# Neu Kernel

Neu is a kernel-only project with a Rust-based, capability-oriented hybrid
architecture and x86-64 as its initial target under QEMU.

## Project status

Phase 0 has a bootable, freestanding Rust x86-64 kernel, loaded by Limine 12.9.4
under QEMU + UEFI/OVMF. Serial diagnostics, boot-information validation, a bounded
panic handler, and controlled halt behavior are implemented. Normal boot,
intentional panic, and missing-boot-information smoke tests passed on the
documented 1-vCPU, 512 MiB TCG configuration on Apple Silicon.

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
QEMU. `make test` observes the halt through QMP, then shuts QEMU down itself.

## Documentation

- [Architecture and design specification](docs/Neu_Kernel_Architecture_v0.2.md)
- [Architecture Decision Records](docs/adr/README.md)
- [Implementation status, commands, and verification](docs/IMPLEMENTATION_STATUS.md)

ADRs explain why architectural choices exist. Implementation documentation
describes how the current kernel works and what has actually been tested.

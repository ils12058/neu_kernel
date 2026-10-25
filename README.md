# Neu Kernel

Neu is a kernel-only project with a Rust-based, capability-oriented hybrid
architecture and x86-64 as its initial target under QEMU.

## Project status

This repository currently contains architecture and decision documentation.
The kernel bootstrap has not been implemented or verified. Proposed subsystem
designs are not implemented features or stable contracts.

Phase 0 targets a minimal freestanding kernel bootstrap with an external
bootloader, early serial diagnostics, a basic panic handler, and reproducible
QEMU smoke tests. Limine is the intended loader; its release and protocol
integration details remain unresolved.

## Documentation

- [Architecture and design specification](docs/Neu_Kernel_Architecture_v0.2.md)
- [Architecture Decision Records](docs/adr/README.md)

ADRs explain why architectural choices exist. Implementation documentation will
describe how the kernel works as milestones are implemented and tested.

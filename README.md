# Neu Kernel

Neu is a kernel-only project with a Rust-based, capability-oriented hybrid
architecture and x86-64 as its initial target under QEMU.

## Project status

K2 / Phase 2A adds a test-only Ring 3 fixture with a private user root and child
page tables, initialized user RX/R/NX/RW/NX pages, a guarded TSS.RSP0 kernel entry
stack, and controlled return through `int 0x80`. QMP observes real CPL=3 execution;
the kernel verifies the CPU-saved context, result 42, and complete teardown back
to its original root. Ordinary builds retain the supervisor boot/halt path.

Twenty-three host tests and twenty-six QEMU scenarios cover the original Phase
0/1A/1B behavior plus user computation, privilege transitions, seven controlled
security violations, and allocation-failure cleanup. The tested environment
remains 1-vCPU/512 MiB QEMU TCG + UEFI/OVMF on Apple Silicon, Rust 1.95.0 and
Limine 12.9.4/base revision 6. All bootloader-reclaimable RAM remains reserved.

General Nodes, native syscalls, capabilities, scheduling, an ELF loader, and the
other architecture dossiers remain future work. The architecture document
describes design directions and proposals, not a list of implemented features or
stable contracts.

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
implementation status for individual user, paging, memory, and exception
commands. For the Ring 3 fixture, run `python3 tools/neu.py test --case user`; this completion gate is
explicitly temporary and is not the final Neu syscall ABI.

## Documentation

- [Architecture and design specification](docs/Neu_Kernel_Architecture_v0.2.md)
- [Architecture Decision Records](docs/adr/README.md)
- [Implementation status, commands, and verification](docs/IMPLEMENTATION_STATUS.md)

ADRs explain why architectural choices exist. Implementation documentation
describes how the current kernel works and what has actually been tested.

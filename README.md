# Neu Kernel

Neu is a kernel-only project with a Rust-based, capability-oriented hybrid
architecture and x86-64 as its initial target under QEMU.

## Project status

K2 / Phase 2B adds real x86-64 `SYSCALL` entry, a guarded kernel stack switch,
scalar `abi_query`, validated `IRETQ` return, and terminal `thread_exit` for the
isolated bootstrap fixture. QMP observes actual Ring 3 → Ring 0 → Ring 3 state,
then verifies original-root restoration, cleanup and controlled halt. Operation
numbers/version encoding remain experimental. The Phase 2A `int 0x80` path stays
available in its original test configurations; ordinary builds launch no fixture.

Thirty-two host tests and thirty-four QEMU scenarios cover all previous behavior
plus native queries, scalar errors, unsafe return-state rejection and disabled
SCE. The environment remains 1-vCPU/512 MiB QEMU 11.0.0 TCG + UEFI/OVMF on Apple
Silicon, Rust 1.95.0 and Limine 12.9.4/base 6. Loader-reclaimable RAM stays reserved.

General Nodes/Threads, capability operations, scheduling, an ELF loader, and the
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
commands. Run `python3 tools/neu.py test --case native` for the native ABI proof,
or `python3 tools/neu.py test --case user` for the earlier test-gate fixture.

## Documentation

- [Architecture and design specification](docs/Neu_Kernel_Architecture_v0.2.md)
- [Architecture Decision Records](docs/adr/README.md)
- [Implementation status, commands, and verification](docs/IMPLEMENTATION_STATUS.md)

ADRs explain why architectural choices exist. Implementation documentation
describes how the current kernel works and what has actually been tested.

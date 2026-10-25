# Neu Kernel

Neu is a kernel-only project with a Rust-based, capability-oriented hybrid
architecture and x86-64 as its initial target under QEMU.

## Project status

K4 / Phase 4A adds calibrated local-APIC timer delivery in an isolated kernel
interrupt window. QEMU's verified x2APIC MSR interface is preferred; a separate
xAPIC test verifies a supervisor UC MMIO mapping. Dedicated returning entries
preserve GPRs and the interrupted stack, count real timer events and send EOI.
Shutdown masks/stops the source, drains pending delivery and returns to IF=0.

The suite contains **52 host tests and 47 QEMU scenarios**, retaining all previous
43/40 tests. It covers repeated timer delivery and rearming, spurious no-EOI
handling, unsupported configuration and software/unexpected-vector rejection.
QMP observes the enabled CPU and programmed controller independently. The tested
environment remains 1-vCPU/512 MiB QEMU 11.0.0 TCG + UEFI/OVMF on Apple Silicon,
Rust 1.95.0 and Limine 12.9.4/base 6.

The two bootstrap Nodes and native ABI 0.1 remain available in their original
IF=0 configurations. Ordinary boot also retains IF=0. There is no Thread context
switch, preemptive scheduler, public capability interface or general IRQ routing.
The approximately 1 ms clockevent is calibrated against the emulated PIT;
TCG delivery counts are not a precise clocksource or CPU-time guarantee. See
[ADR 0011](docs/adr/0011-local-timer-interrupt-foundation.md) for the safety
boundary and prerequisites for Phase 4B.

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
itself. See the implementation status for the individual test commands. Run `python3 tools/neu.py test --case timer` for hardware timer delivery,
`python3 tools/neu.py test --case timer-xapic` for the MMIO fallback, or
`python3 tools/neu.py test --case nodes` for the two-Node proof; use
`python3 tools/neu.py test --case native` for the original native ABI proof, or
`python3 tools/neu.py test --case user` for the earlier test-gate fixture.

## Documentation

- [Architecture and design specification](docs/Neu_Kernel_Architecture_v0.2.md)
- [Architecture Decision Records](docs/adr/README.md)
- [Implementation status, commands, and verification](docs/IMPLEMENTATION_STATUS.md)

ADRs explain why architectural choices exist. Implementation documentation
describes how the current kernel works and what has actually been tested.

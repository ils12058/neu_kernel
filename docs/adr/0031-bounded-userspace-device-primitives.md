# ADR 0031: bounded userspace device primitives

Status: accepted for bounded K11 completion; local x86-64/QEMU verified.

## Context

The architecture roadmap requires controlled device authority and real userspace
interrupt delivery without exposing unisolated DMA. QEMU 11.0.0 EDU places its
DMA command and address registers at offsets 0x80–0x98 in the same page as its
liveness, factorial and interrupt registers. Page permissions cannot separate
those registers. QEMU sends MSI through its bus-master address space, so
disabling bus mastering also prevents MSI delivery. Existing NEU does not
discover ACPI routing.

## Decision

Use one bounded Q35 PCI EDU backend at 00:05.0. Probe the assigned BAR with
decoding disabled and validate its exact 1 MiB extent. Map its first register
page and the Q35 I/O APIC page UC/RW/NX/supervisor. Keep bus mastering and MSI
disabled. Validate Q35/ICH9 identities, the fixed slot-five INTx-to-GSI-21 route,
and the absence of another PCI interrupt source sharing that route. Only the
BSP performs PCI configuration and I/O APIC selector/window cycles with
interrupts disabled. Program a masked, level-triggered route to the verified BSP
APIC ID and vector 240. This is a board-specific INTx backend, not general
firmware-routing discovery.

Userspace receives DeviceResource authority and must claim an exclusive lease.
The claim creates generation-bound MmioRegion and Interrupt capabilities in its
Node's existing table. Register operations use an explicit allowlist with 32-bit
accesses; the kernel owns interrupt acknowledgement and all configuration.
DMA, raw PCI configuration, port access and direct writable MMIO mapping have no
public operation. Device capabilities are not transferable between Nodes.

The IRQ stub masks the I/O APIC route before EOI and publishes a bounded atomic
event. It never takes the scheduler lock. The BSP consumes that event under the
normal scheduler transaction, preserves pending delivery without a waiter, or
wakes one blocked Thread. ACK requires the delivered sequence and receiving
Thread. Hardware status must clear before rearming. A 64-operation register
budget and an 80-BSP-timer-tick watchdog quarantine an unresponsive lease;
polling and denied requests cannot renew the deadline. The BSP timer remains
available while idle with outstanding device work.
State transitions that need BSP service publish a generation-checked wake/IPI,
so a claim or ACK on an AP also progresses when the BSP was idle without a timer.

The original claiming authority and both lease handles remain lifecycle anchors.
Closing a lease handle, closing the claiming handle, revoking a selected anchor
or waiter, or stopping its Node invalidates the whole lease. Descendants retain
no access to a retired device generation. Teardown first denies operations, then
masks delivery and waits for the old command's actual completion status/event.
EDU clears COMPUTING before raising its completion IRQ, so that flag alone
cannot authorize reuse. The backend drains status and pending LAPIC events,
then scrubs liveness to 0 and factorial to 1 using a neutral factorial(0) with
completion IRQ enabled while delivery is masked. The reset must finish its own
actual completion raise before the backend clears status and IRQFACT; otherwise
a new owner enabling IRQFACT could receive a late reset interrupt. Only a
verified quiescent device is made claimable
again; inability to prove quiescence leaves it quarantined. Resource authority
in other Nodes is preserved. This terminal device effect is separate from K10's
ordinary handle-close behavior and strict-descendant revocation semantics.

The interface is [experimental ABI 0.7](../abi/experimental-v0.7.md). It adds
seven scalar operations at 0x1b–0x21 and three typed objects. The device profile
has nine object slots; earlier images retain six. No syscall permits arbitrary
physical addresses, interrupt routing or PCI configuration. Trusted bootstrap
supplies the resource; device creation/enumeration and driver loading remain
outside the interface.

## Consequences

Driver policy and request processing execute at CPL3, while trusted code performs
each individual register access. This provides an end-to-end driver/IRQ proof
without claiming direct mapped MMIO or DMA isolation. Device-specific mediation
is necessary until safe resource granularity and DMA isolation exist. The bounded
backend is deliberately unsuitable for arbitrary hardware discovery or production
drivers. [The independent QMP observer](../testing/device-qmp.md) checks actual
registers, PCI/controller state, normalized IRQ/native frames, generation-bound
authority, cancellation and exact final conservation. Acceptance and measured
limits belong in [K11 completion](../K11_COMPLETION.md).

References: QEMU v11.0.0 [EDU source](https://github.com/qemu/qemu/blob/v11.0.0/hw/misc/edu.c),
[PCI MSI delivery](https://github.com/qemu/qemu/blob/v11.0.0/hw/pci/pci.c),
[MSI support](https://github.com/qemu/qemu/blob/v11.0.0/hw/pci/msi.c),
[ICH9 routing](https://github.com/qemu/qemu/blob/v11.0.0/hw/isa/lpc_ich9.c)
and [EDU specification](https://github.com/qemu/qemu/blob/v11.0.0/docs/specs/edu.rst).

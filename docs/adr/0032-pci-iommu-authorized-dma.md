# ADR 0032: PCI discovery, IOMMU isolation and authorized DMA

Date: 2026-11-14.

Status: accepted for bounded local x86-64/QEMU completion; validation recorded
in [PCI/DMA completion](../PCI_DMA_COMPLETION.md).

Related NEU decisions: scoped portions of NEU-003, NEU-005, NEU-007, NEU-009,
NEU-018, NEU-021, NEU-022, NEU-023 and NEU-024. Architecture v0.2
[§19](../Neu_Kernel_Architecture_v0.2.md)
and its NEU-022 / DEV register entry remain the baseline. This is authorized
post-K11 work; the roadmap has not assigned a K12.

## Context

[ADR 0031](0031-bounded-userspace-device-primitives.md) supports a selected EDU
driver with bus mastering disabled. Broader device discovery must not itself
grant hardware authority. DMA bypasses CPU page tables; capability checks alone
cannot constrain a device's memory transactions. Closing a handle or terminating
a Node also does not stop a transfer that hardware has already accepted.

The requested progression is reusable PCI support, actual IOMMU isolation, then
capability-authorized DMA. The accepted platform is local x86-64 QEMU q35 with
EDU and Intel VT-d legacy translation. Public object factories, general driver
loading and physical-hardware validation remain separate work.

## Decision

Copy and validate bounded RSDP/root/MCFG/DMAR data from retained firmware memory.
Separate the reusable ACPI, PCI, VT-d encoding and DMA lifetime modules from
the x86-64 hardware backend. Enumerate all firmware-advertised PCI buses and
multifunction functions, validate bridge topology, standard/extended capability
chains and 32/64-bit memory or I/O BARs. Probe with decode disabled, restore BARs
and use halfword command writes so PCI W1C status is not replayed. Reject
malformed, unassigned, overlapping or unsupported selected resources.

The Q35 backend retains trusted INTx policy, computes swizzling through validated
PCIe bridges, and discovers EDU by identity rather than assuming slot five.
All discovered requesters first lose bus mastering, INTx and MSI/MSI-X authority.
Conventional/PCIe-to-PCI requester aliases are rejected; no public isolation-group
assignment exists. General AML routing, hotplug and resource reassignment are
outside this backend.

Install empty Intel VT-d roots and complete global context/IOTLB invalidation
before enabling translation and the selected bus-master path. Explicit DMAR
device scopes take precedence over include-all ownership. RMRR-dependent
requesters and unsupported hardware encodings fail closed. No identity mapping
serves as a fallback. Fault MSI is masked; bounded polling preserves the actual
fault source, address, access and reason. Register views are supervisor RW/NX/UC.
Cache flushes make table and buffer publication explicit when coherency is not
advertised.

Keep one exclusive selected-device domain in reserved kernel table storage.
Its application leaves exist only under a live, generation-bound claim. Expose
two nontransferable capability types and seven scalar operations through
[ABI 0.8](../abi/experimental-v0.8.md). The original device/domain anchors alone
may allocate the next authority object; derivation cannot amplify allocation
rights. Map and submission direction rights are checked independently.

Each claim has one 8192-byte buffer, one mapping, one in-flight operation and
six tracked grant identities: original domain/buffer, mapping domain/buffer,
submission domain/buffer. The two data pages have stable, noncontiguous physical
backing. Applications never select physical addresses or program raw DMA/PCI
registers. Transfers are bounded to 4096 bytes and sixteen submissions per claim.

On close/revocation/Node loss, withdraw authority and retain quarantine until
actual old completion, invalidation, host-buffer clearing and verified device
SRAM clearing finish. Scrub EDU SRAM with a zero-page DMA write and a full DMA
readback, requiring actual masked IRQ completions in both directions. Remove
these private mappings before the existing factorial reset and generation
release. Hardware work outlives the requester when necessary. If retirement
cannot be proved, preserve backing and fail closed. After final cleanup,
translation stays enabled against empty reserved roots.

## Alternatives considered

- Raw user MMIO would expose DMA programming registers in the same page as
  ordinary EDU registers. Continue scalar mediation; an IOMMU does not authorize
  arbitrary interrupt/configuration changes.
- Physical or identity-mapped DMA addresses would bypass the capability-bound
  mapping contract. Use a bounded IOVA and only manager-owned backing.
- Releasing pages at capability close would race hardware. Retain them through
  real completion, invalidation and both host/device scrub checks.
- Assuming coherent table walks would leave noncoherent implementations outside
  the contract. Require the explicit supported cache synchronization mechanism.
- Combining this work with AMD IOMMU, MSI remapping, hotplug or a driver loader
  would add independent platform and lifecycle contracts. Keep this acceptance
  focused on the implemented q35/EDU/legacy VT-d path.

## Consequences

The reusable discovery and policy layers now support broader PCI descriptions,
while executable acceptance remains deliberately bounded. Two/four-CPU tests
must prove positive transfers, alternate placement/bridge routing, four actual
remapping-fault cases, in-flight lifecycle cancellation, quota/watchdog handling
and legacy/no-IOMMU refusal. [The DMA QMP protocol](../testing/dma-qmp.md) requires
raw hardware evidence and separate native/lifetime models, plus preserved K11
regression checks and exact final resource conservation.

AMD IOMMU, interrupt remapping/MSI delivery, ATS/PASID/SVA, PCIe peer-to-peer/ACS
policy, hotplug/AML, arbitrary drivers and physical devices are not accepted by
this decision. The existing fixed pools and integer-only scheduler remain.

## History

- 2026-11-14: implement the user's post-K11 PCI → IOMMU → authorized-DMA
  progression. ADR 0031 remains applicable to ABI 0.7 profiles; this record
  extends its platform/authority contract only for DMA profiles.

References: QEMU v11.0.0 [EDU implementation](https://github.com/qemu/qemu/blob/v11.0.0/hw/misc/edu.c),
[Intel IOMMU implementation](https://github.com/qemu/qemu/blob/v11.0.0/hw/i386/intel_iommu.c),
[Intel IOMMU register definitions](https://github.com/qemu/qemu/blob/v11.0.0/hw/i386/intel_iommu_internal.h),
and [Limine protocol](https://github.com/limine-bootloader/limine/blob/v12.x/PROTOCOL.md).

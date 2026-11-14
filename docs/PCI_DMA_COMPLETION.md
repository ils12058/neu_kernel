# PCI, IOMMU isolation and authorized DMA

Status: **accepted for bounded x86-64/QEMU (2026-11-14)**.

This post-K11 work starts at `813b3e677a43c72fa813542c9d433183e0d8810a`
on `codex/pci-iommu-dma`. It implements the user-authorized progression from
general PCI discovery to IOMMU isolation and capability-authorized DMA.
It does not assign a new numbered roadmap milestone.

## Required implementation and acceptance — satisfied

- Discover PCI configuration apertures and DMA-remapping units from validated
  ACPI MCFG/DMAR tables. Check signatures, checksums, sizes, ranges, reserved
  fields and finite capacities before using firmware addresses.
- Enumerate PCI functions and bridge topology through a reusable configuration
  interface. Handle multifunction devices, standard capabilities and 32/64-bit
  BARs; preserve PCI status bits during command writes and reject conflicting
  or unsupported assignments. Exercise alternative slots and a secondary bus.
- Install Intel VT-d second-level translation with default-deny requester
  contexts. A device receives only its domain's explicitly mapped buffers;
  no identity-map fallback authorizes DMA. Validate capabilities, complete
  context/IOTLB invalidation, record hardware faults and retain safe ownership
  if quiescence or invalidation cannot be proved.
- Expose typed, generation-bound DMA authority with exact buffer bounds,
  direction permissions, finite capacity and lifetime accounting. User inputs
  cannot select physical addresses, arbitrary PCI registers or IOMMU tables.
- Demonstrate real QEMU EDU DMA in both directions, including page-boundary
  transfers, real completion IRQs and independently checked buffer contents.
- Prove hardware rejection for unmapped addresses, wrong permissions, another
  requester and revoked translations. Preserve victim/canary memory and match
  the actual remapping fault's source ID, address and access type.
- Exercise close, revocation and Node loss with work in flight. Device commands,
  IRQs and DMA translations must be retired before a buffer or device generation
  is reassigned. Scrub both host buffers and retained device-buffer data.
- Keep K11 and earlier ABI behavior and regression evidence. Require meaningful
  host/observer tests, the complete registered QEMU matrix, exact-image repeats,
  formatting/lint/build checks and an audit of frozen sources and captured
  hardware/resource state before declaring completion.

## Platform contract

The implementation target is local x86-64 QEMU q35 with Intel VT-d emulation
and EDU as the DMA test device. PCI discovery and resource validation are
separate from EDU-specific register policy. Existing fixed CPU/Node/Thread
and bootstrap-pool limits remain until a separately implemented public factory
interface replaces them. No physical-hardware validation is implied.

Hotplug, ACPI AML policy, AMD IOMMU, ATS/PASID/SVA and unrestricted userspace
device-register writes require additional platform contracts. Unsupported
firmware/device configurations must fail closed. DMA remains unavailable when
the required isolation backend cannot be established.

## Implemented contract

The reusable ACPI adapter validates signatures, checksums, declared extents,
reserved fields and finite capacities before copying MCFG/DMAR ownership data.
It supports XSDT/RSDT selection, up to eight ECAM apertures, four remapping
units, thirty-two scopes and sixteen RMRRs. Device-scope paths have at most
eight hops. The hardware path uses the copied data after leaving the loader's
page tables; firmware pointers are not retained as mutable authority.

PCI inventory holds up to 128 functions and covers all advertised buses,
multifunction functions and bridge topology. It validates standard and PCIe
extended capability chains, probes memory/I/O and 64-bit BARs, restores their
contents, preserves W1C status during command writes and rejects selected BAR
conflicts across functions. ECAM base addresses use the bus-zero convention,
including a region with a nonzero first bus. Segment-zero conventional accesses
use BSP/IF=0 CF8/CFC cycles; extended/nonzero-segment accesses use temporary UC
ECAM views. Unsupported CardBus/resource/topology cases fail closed.

The Q35 backend selects EDU by identity and validates its exact register BAR,
firmware memory exclusions, bridge requester identity and INTx swizzling. It
supports the demonstrated root placements and PCIe root-port path. Every
discovered requester first loses bus mastering and interrupt authority. MSI and
MSI-X remain disabled. Conventional/PCIe-to-PCI requester aliases are rejected
until a separate isolation-group contract exists.

Intel VT-d legacy translation uses reserved kernel roots, contexts and domain
pages. Capability encodings, physical address widths, register extents and
fault records are checked. The encoding layer supports three/four levels;
the QEMU acceptance matrix exercises its emulated unit. Explicit DMAR scopes
take precedence over include-all ownership; RMRR-dependent requesters are
rejected by this backend. Translation starts against empty default-deny roots
before the selected bus-master path is enabled. Every map/unmap completes
synchronous context/IOTLB invalidation. CLFLUSH and fences make table/buffer
visibility explicit without assuming coherent remapping walks.

[ABI 0.8](abi/experimental-v0.8.md) adds DmaDomain/DmaBuffer objects and seven
scalar operations. The original claimed MMIO/domain anchors alone may mint the
next authority object. Rights may be attenuated but never amplified through
allocation; DMA authority is nontransferable. Each operation checks Node,
handle/type/rights and the active device generation. Mapping and submission
grants are tracked independently, including a SEND-only submit descendant.

Each claim owns one logical 8192-byte buffer backed by two noncontiguous reserved
pages. The intervening canary has no application DMA translation. Userspace
reads/writes aligned u64s through mediated operations, maps the exact two-page
IOVA range with device-read/write rights, and submits 1–4096-byte transfers.
There is one active command and sixteen successful submissions per claim.
The existing device watchdog and IRQ wait/ACK protocol remain in force.

Close, selected descendant revocation and Node loss deny the old lease
immediately while hardware ownership persists. The close/revoke/fault fixtures
observe actual RUN=1 after quarantine, so cancellation cannot be satisfied by
an already completed transfer. Completion IRQs, masks, EOI, mailbox and retired
generation are checked independently. Old work never completes into a later
driver's authority.

After actual completion, application leaves are removed and invalidated before
host-buffer clearing. Private read-only zero and write-only verification pages
scrub EDU's 4096-byte SRAM through a real DMA write and complete DMA readback.
Both need their actual masked IRQ completion; every verification word must be
zero. Private mappings are removed before the existing neutral factorial reset
and final typed-handle retirement. A new owner independently reads zero SRAM,
performs its own transfers and reaches the same retirement checks. An inability
to prove isolation or quiescence retains quarantine and reserved backing.

Final source/destination/count registers are zero. EDU's last command remains
`6`, the completed device-to-host scrub command with RUN=0: QEMU ignores writes
without RUN, so claiming this register was cleared would be incorrect. Final
PCI bus mastering and memory decoding are disabled, register aliases are
removed, the masked IRQ route is restored, and VT-d remains enabled against
empty reserved roots. There is no identity-map fallback at cleanup.

## Validation evidence

The complete final matrix and every required exact-image repeat passed on the
same frozen code/build/observer input set.

| Check | Accepted result | Retained evidence |
| --- | --- | --- |
| Rust host tests | 368/368 in default, authority, device and DMA configurations | `build/pci-dma-full-test.log`, `build/pci-dma-host-authority-test.log`, `build/pci-dma-host-device-test.log`, `build/pci-dma-host-dma-test.log` |
| Python regressions | 52/52: 17 scheduler/accounting, 12 authority, 13 device, 10 DMA | `build/pci-dma-check.log`, `build/pci-dma-full-test.log` |
| Formatting and clippy | All 215 profile selections and host library pass | `build/pci-dma-check.log` |
| Complete QEMU matrix | 215/215 unique registered profiles; 30-second deadline | `build/pci-dma-full-test-results.json`, `build/pci-dma-full-test-evidence/` |
| Same-image repeats | All 17 new profiles pass again | `build/pci-dma-repeat-results.json`, `build/pci-dma-repeat-evidence/` |
| Toolchain/default build | Doctor/build pass; default kernel equals full-run boot image | `build/pci-dma-doctor.log`, `build/pci-dma-build.log` |
| Frozen evidence audit | 106 inputs, 215 images, 45 scheduled profiles, 14 DMA profiles, 9 device profiles and controls verified | `build/pci-dma-validation-summary.json` |

Source inventory: `build/pci-dma-validation-source-manifest.json`. Exact bytes:
`build/pci-dma-source-snapshot/`. Aggregate SHA-256:
`d9596bb7eb290928871bd030128475a22181dc4dbadafbf2fc0cb703711735ba`. Default kernel SHA-256:
`fce9c7187af9af7e9ea44d3fd2e48d4fcd9ca0d7245166c8f57e1622309b837c`. The longest full-run guest took
28.347 seconds; the longest repeat took 28.234 seconds.
No project QEMU process remained. Hash checks preserve 1520 prior
K11 evidence/unrelated files, including `SIMPLIFY-3-COMPA.txt`.

The retained audit reconstructs every DMA syscall/continuation from native
frames and independently recomputes every captured DMA result from buffer
writes and transfer bounds. It checks real IRQ frames, fault SID/address/access/
reason, exact observed IOVA leaves, completed invalidation registers, mandatory
RUN/mapping/scrub/reassignment windows and final hardware/resource accounting.
The frozen live observer additionally requires physical zero-buffer readbacks,
intact canaries and empty final raw roots at their hardware observation points.
K9/K10/K11 conservation checks remain in force. Full evidence is copied before
repeats overwrite ordinary `build/test-logs/` captures.

All seventeen new cases are listed in [the DMA QMP protocol](testing/dma-qmp.md).
The four fault cases require hardware rejection of unmapped/read-only/
other-requester/revoked translations. The three controls require startup refusal
without IOMMU and complete ABI 0.5/0.7 rejection of new calls. The latter also
runs the complete K11 protocol with bus mastering disabled. A successful serial
message alone cannot pass any scheduled DMA case.

The final development focus suite is in `build/pci-dma-final-focused/`; it
checks the requester, revoked translation and submission-only revocation paths
after the resource/cache-visibility changes. The separate bridge pass is in
`build/pci-dma-final-focused-bridge.log`. These support development review and do
not replace the frozen matrix and repeats.

## Development corrections retained with the evidence

- `build/pci-dma-bringup-1.*` records an early fixture that submitted before the
  device had rearmed, then exhausted its trace while its sibling waited.
  Native fixtures now wait for armed state before expecting permission/bounds
  errors. `build/pci-dma-readonly-failure-1.*` retains the same error-precedence
  correction in the read-only case.
- `build/pci-dma-unmapped-failure-1.*` through `-3.*` retain hardware-fault
  bring-up. Legacy fault PP=0 leaves PASID-value bits undefined; QEMU places
  `0xffff` there. The parser now ignores those undefined values while rejecting
  unsupported PP and reserved fields. EDU also applies a default 28-bit DMA
  address mask; the unmapped probe now targets the valid, deliberately unmapped
  IOVA just beyond the buffer instead of relying on a truncated physical address.
- `build/pci-dma-close-failure-1.*` records cancellation after DMA completion.
  The 250 ms blocked-wait observation exceeded EDU's 100 ms virtual transfer
  timer. The helper now waits for that observation before submitting and
  immediately withdrawing authority. Required checkpoint 33 proves actual
  RUN=1 under quarantine; deadlines and hardware checks were not relaxed.
- Submission-only revocation tracks the precise SEND descendant independently
  of the grant that installed the mapping. Completion clears those active
  submission dependencies; release clears all generation anchors.
- Final review added cross-function BAR conflicts, raw QMP inventory and
  capability-chain completeness checks, explicit 64-byte CLFLUSH publication,
  reserved VT-d encoding checks and IOTLB/fault-register overlap rejection.
  The complete accepted matrix uses these final sources.

`build/pci-dma-focused-1.log` through `build/pci-dma-focused-5.log` retain the
development sequence. No failed or incomplete development run is counted as
acceptance. Existing `build/k11-*` records remain associated with accepted K11.

## Scope after this work

The requested progression is implemented and verified for local QEMU q35,
EDU and Intel legacy VT-d, with two/four CPUs, two Nodes, three Threads,
fixed pools and IRQ-disabled shared transactions. Reusable discovery/encoding
support does not establish physical-hardware compatibility beyond this evidence.

General driver loading, physical devices, AMD IOMMU, interrupt remapping/MSI
delivery, hotplug/ACPI AML, PCIe peer-to-peer/ACS policy, ATS/PASID/SVA and
production device orchestration need separate contracts and validation. Public
object/mapping/Thread factories, arbitrary AP allocation, balancing, kernel
preemption, FPU/SIMD/TLS, COW and demand paging also remain future work. Hosted
CI was not run. The architecture baseline and K0–K11 history are preserved;
this work does not invent a K12 or claim a complete production OS.

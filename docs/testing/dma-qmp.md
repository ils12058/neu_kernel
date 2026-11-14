# PCI and DMA: independent QMP acceptance

`tools/dma_qmp.py` extends the existing device and scheduled-SMP observers. It
checks raw firmware/PCI data, VT-d registers and translation tables, actual DMA
bytes and IRQ frames, native capability operations and cleanup. The guest's
serial result or journal alone cannot establish success. See
[ABI 0.8](../abi/experimental-v0.8.md) and
[the completion record](../PCI_DMA_COMPLETION.md) for interface and acceptance.

```sh
python3 tools/neu.py test --case smp-dma-4
python3 tools/neu.py test --case smp-dma-bridge
python3 tools/neu.py test --case smp-dma-requester
python3 tools/test_dma_qmp.py
make check
make test
```

The deadline stays at 30 seconds. DMA profiles use QEMU q35/TCG with
`-device intel-iommu,caching-mode=on,intremap=off` and EDU. MSI/MSI-X remain
disabled; IRQ delivery uses the trusted Q35 INTx route to BSP vector 240.

## Registered scenarios

| Case | CPUs | Required behavior |
| --- | --- | --- |
| `smp-dma-2` | 2 | Both DMA directions across a physical page boundary, real IRQs and two owner generations |
| `smp-dma-4` | 4 | Same flow with concurrent/idle unrelated CPUs |
| `smp-dma-slot` | 2 | EDU at 00:07.0, dynamically selected GSI 23 |
| `smp-dma-bridge` | 4 | EDU behind a PCIe root port, secondary-bus discovery and INTx swizzling |
| `smp-dma-unmapped` | 2 | Actual device write to unmapped IOVA `0x102000` produces a remapping fault |
| `smp-dma-readonly` | 2 | Device write through a read-only translation is rejected in hardware |
| `smp-dma-requester` | 4 | A second EDU with no context cannot use the first requester's mapped IOVA |
| `smp-dma-revoked` | 4 | A real transfer primes translation; invalidated/unmapped IOVA rejects a later device write |
| `smp-dma-close` | 2 | Close cancels a real waiter while the third DMA command still has RUN=1 |
| `smp-dma-revoke` | 4 | Mapping-grant descendant revocation cancels active DMA |
| `smp-dma-submit-revoke` | 4 | SEND-only submission descendant is revoked independently of the mapping anchor |
| `smp-dma-fault` | 4 | Actual user MMIO #PF cancels the Node while DMA is still running; successor progresses |
| `smp-dma-timeout` | 2 | Delivered completion left unacknowledged reaches the BSP watchdog |
| `smp-dma-quota` | 2 | Sixteen successful submissions; the next valid request returns quota and quarantines |
| `dma-no-iommu` | 2 | No DMAR: refuse startup before domain publication; every PCI requester has bus mastering disabled |
| `smp-dma-disabled` | 2 | ABI 0.5 rejects all seven DMA operations for all three Threads |
| `smp-device-dma-disabled` | 2 | ABI 0.7 rejects all seven DMA operations, then completes the full K11 device protocol |

The four hardware fault probes are trusted fixture injections. No public syscall
can program an unmapped address or another requester. Their role is to verify
the IOMMU itself independently of the software rejection paths.

## Raw discovery and translation checks

Read RSDP, XSDT, MCFG and DMAR from physical memory and verify their signatures,
checksums and declared extents. Reconcile copied ECAM regions and remapping-unit
addresses with those tables. Compare every enumerated function to raw config
space **and** QMP's recursive `query-pci` inventory. Recheck BAR restoration,
standard/extended capability chains and disabled message interrupts. Walk bridge
parentage to derive the selected GSI independently. Only the selected endpoint
and required bridge path may bus master during normal execution.

Read CAP/ECAP, GSTS, root pointer, context-command/IOTLB registers and actual
fault records. Require enabled translation, masked fault MSI and completed
invalidation granularity. Walk the legacy root, requester context and every
second-level table. The only application leaves are the two authorized buffer
pages with exactly the current direction bits; other requesters have no context.
The two private scrub leaves are allowed only during retirement.

Verify five distinct, aligned backing pages, physically noncontiguous application
pages, supervisor CPU mappings and the intact unmapped canary. MMIO and IOMMU
register mappings must be supervisor RW/NX/UC in each relevant root. Existing
full CPU page-table walks continue to reject unexpected userspace aliases.

## Native model and real hardware observations

`DmaModel` independently implements local handles, rights attenuation, buffer
bounds, mapping permissions, submission quota and byte-level DMA results.
`LeaseModel` separately checks owner/generation, map/unmap/invalidation epochs,
busy state, quarantine and scrub/release ordering. Every transaction must agree
with both relevant models and raw production fields. Every syscall is matched
to its actual entry and resumed frame; cancelled Threads must not resume.

The CPL3 fixture writes four distinct u64s at offsets 4080, 4088, 4096 and 4104,
then round-trips them through EDU SRAM. Expected bytes are computed from the
native writes and transfer direction. Each hardware IRQ requires its own
actual stopped-BSP probe, normalized 22-word frame, CR3, generation, status 256,
completed DMA registers, mailbox and post-EOI controller state. The observer
reads EDU's ordinary registers individually at 32-bit width and DMA registers
at 64-bit width; invalid-width reads cannot substitute for evidence.

Required additional checkpoints are:

| ID | Required observation |
| --- | --- |
| 1 | Actual blocked IRQ waiter, separately for each observed wait |
| 3 | First generation completely retired before reassignment |
| 4 | Actual masked factorial-reset completion, for both generations |
| 30 | Initial authorized mapping and zero data before application DMA |
| 31 | Actual masked DMA SRAM readback, all bytes zero, for both generations |
| 32 | Real VT-d fault with exact requester, address, write access and reason |
| 33 | Quarantined authority while actual hardware RUN remains 1 |

Fault reasons are missing context=2 for the other requester and write denied=5
for unmapped/read-only/revoked translations. Victim data and the canary must
remain unchanged. Legacy fault records with PP=0 have undefined PASID-value
bits; these are ignored, while unsupported PP/reserved fields are rejected.
The revoked-IOVA profile must first prove successful DMA, then completed unmap,
before its actual rejected write.

Close/revoke helpers wait until A2 is independently observed blocked, then issue
the next DMA command and withdraw authority immediately. Checkpoint 33 cannot
be satisfied by cancellation after the transfer has already completed. Masked
late completions are separate journal events and are never delivered to a later
device generation.

## Retirement and conservation

Application translations disappear before host/SRAM scrub. The SRAM zero write
and complete zero readback both require real completion status while delivery
is masked. The next factorial reset must follow that readback. Both generations
must pass each required scrub observation before release. Address/count
registers finish at zero; EDU's final non-running command is 6 because its
RUN-clear writes are ignored.

Terminal state has exactly two claims, quarantines and releases, no typed lease
authority or queued completion, zero host/scrub data and an intact guard. Final
state has disabled PCI decoding/bus mastering, restored masked interrupt route,
no register aliases, and VT-d still enabled with empty reserved roots. The
normal scheduler audit requires exact baseline allocator bitmap/table inventory,
zero final user roots, handles, grants, waits, queued authority, mappings and
object references/pins. Reserved DMA tables/buffers belong to the kernel image
and are never counted as newly allocatable frames.

DMA profiles reserve 2048 entry/IRQ trace rows per CPU and 64 actual device IRQ
captures. K11 profiles retain 512/16; earlier profiles retain their original
allocations. Observation windows and the sixteen-transfer quota require this
capacity; no record is dropped or wrapped and the deadline is unchanged.

## Regression and retained evidence

Host tests cover ACPI malformed/reserved/checksum/range cases, PCI enumeration,
BAR restoration/overlap/W1C preservation, VT-d encoding/faults, finite DMA
lifetime, rights/owner/type/transfer rejection and allocation rollback. Ten
adversarial Python tests cover forged results, permission/bounds/overflow errors,
byte-level cross-page transfers, raw table aliases/pass-through, invalidation
receipts and premature release. The earlier observer suites remain required.

Freeze every code/build/observer input before the complete registered matrix.
Preserve full captures/image manifests before exact-image repeats overwrite
ordinary test logs. Repeat all seventeen new scenarios, audit source/image and
firmware hashes plus raw retained lifecycle/hardware state, and verify no
project QEMU process survives. Existing `build/k11-*` evidence remains intact.
The completion record contains measured counts, hashes and any retained failures.

# ADR 0023: Live Ring 3 MemoryObject sharing and fault containment

- Date: 2026-10-28
- Status: Accepted for the bounded Phase 8A-2 scope (2026-10-28)
- Related decisions: NEU-005, NEU-017, NEU-018, NEU-021, NEU-023, NEU-024
- Architecture: [memory and object lifetimes](../Neu_Kernel_Architecture_v0.2.md#15-memory-architecture)
- Predecessor: [ADR 0022](0022-memoryobject-backing-and-lifetime.md)

## Context

ADR 0022 established generation-checked MemoryObject ownership, trusted grants,
per-root mapping accounting, dynamic table inventory, independent teardown orders
and exact physical collection. Its evidence stopped at kernel-mode setup and
inspection: the fixture mapped both roots but did not execute Ring 3 accesses
through them, and no hardware page fault had been produced by an unauthorized
store. That leaves three questions unanswered:

- Do the installed user leaves actually carry the requested effective rights and
  permit a real Ring 3 store/load of the same physical bytes?
- Does a read-only alias reject a Ring 3 store with the architecturally expected
  `#PF`, and does the existing Node fault path contain it without disturbing the
  sibling Node?
- Can either Node be destroyed first while the other still accesses the shared
  backing, with no kernel copy refreshing it and with exact final reclamation?

The Phase 8A-1 mechanisms are sufficient if the fixture attaches mappings before
Thread execution pins, runs real user code on the installed roots, and publishes
only addresses/layouts for independent inspection. No new public ABI is needed.

## Decision

Add a trusted, test-only `memory-object-live-test` feature family with four
profiles: `memory-object-user-share`, `memory-object-user-readonly-fault`,
`memory-object-user-a-exit` and `memory-object-user-xapic`. The xAPIC profile is
the same live contract with the legacy local-APIC controller fallback.

### Fixed live contract

- One RAM-backed two-page MemoryObject created by trusted kernel code.
- Node A maps both pages at `0x600000`, user RW/NX.
- Node B maps the same two physical pages at `0x602000`, user R/NX.
- The VAs differ, are page-aligned and lie inside the existing
  `[0x400000, 0x800000)` user range; neither root aliases the sibling VAs.
- Thread A1 (label 1, slot 0) is the writer. Thread B1 (label 3, slot 1) is the
  reader or faulting writer. The third assembly role (A2) remains present for
  the existing three-Thread fixture layout but is not installed, so the live
  profile uses two live Threads and peaks at 14 data frames within the fixed
  16-frame limit.
- The controller uses the existing `create`, `add_member`, `install`, queue,
  preemption and retirement primitives. It never writes the shared payload or
  synthesizes an observation block.

### Real Ring 3 access

A1 writes `0x4e45554d4f4c4956` and `0x5a5a0123456789ab` to the first shared
page and a sentinel to the second, using ordinary user stores. B1 performs
ordinary user loads from its own read-only VAs and records the values in its
private DATA block. The kernel only observes/reports those private words; QMP
reads the physical backing bytes directly. Thus the same physical values must
appear in the object and in the reader's private record without a kernel copy.

All multi-Thread fixture roles share one 4 KiB user RX page. The live role block
is therefore compiled into the fixture only when `memory-object-live-test` is
enabled; the legacy multi-Thread payload remains 3688 bytes within its page.

The controller publishes per-label gate and counter words in each Node's private
DATA page. Preemption is armed with `peer=true` unconditionally so a single
spinning Thread can still be checkpointed. The stage machine opens gates only
after the required raw state is present.

### Hardware read-only fault

In the fault profile, B1 first proves the read succeeds, then executes a real
store to `0x602000`. The CPU raises `#PF` with vector 14 and error code `0x7`
(present, write, user). The existing `user_fault` path records the raw frame in
`NEU_LIVE_FAULT` before Node termination. Expected evidence includes `CR2 =
0x602000`, `RIP` inside the actual store site, `CS = 0x3b`, `SS = 0x33`, CPL 3,
Node B/Thread B/Space B identities, B's CR3, and a handler stack inside B's
kernel stack. Result: B's Thread terminates, its mappings are detached exactly
once, B's Node is reclaimed, and Node A continues to its normal exit and
teardown.

### Teardown orders

- Share, fault and xAPIC profiles destroy B first while A still owns a live
  mapping.
- The A-exit profile lets A finish and destroys Node A first while B waits at
  its pre-gate. B then loads the same bytes from its surviving read-only
  mapping and reaches the post gate before its own teardown.

Both orders collect the object once, verify exact allocator/table/frame
restoration, and never expose a public unmap/transfer path.

### Independent evidence

QMP stops the sole CPU at each of eight checkpoints. The descriptor contains
addresses, strides, field offsets, fixed payload constants and saved-frame
pointers only; it is not a success scalar. The observer reads:

- production registry arrays (objects, frames, spaces, mappings, table owners),
- the live allocator counters, bitmap and memory-map spans,
- raw physical page-table pages and effective U/S, R/W and NX bits,
- the production saved Thread frames (`Nodes::threads[].saved`),
- the raw `NEU_LIVE_FAULT` record written by the real fault path,
- physical object bytes and each Node's private DATA page.

Serial evidence alone is not accepted for these four profiles.

## Alternatives considered

- Copy values through a kernel buffer or synthesize the reader's block: does not
  prove user-visible shared backing or hardware permission enforcement.
- Add a public MemoryObject syscall/mmap/capability transfer now: expands the
  security and ABI boundary before the trusted live contract is established.
- Use demand paging, COW or TLB shootdown: unnecessary for two pre-populated
  mappings on one CPU and outside the fixed Phase 8 scope.
- Run A2 simultaneously: would exceed the established two-live-Thread profile
  and add scheduler surface without testing a new sharing property.
- Trust the guest's own printed values: cannot distinguish a kernel copy from a
  real Ring 3 access or prove the hardware fault record.

## Consequences

- Phase 8A-2 demonstrates real Ring 3 shared stores/loads, hardware read-only
  fault containment, sibling independence and both teardown orders.
- Fixed limits remain: 2 Nodes, 3 Threads, 16 data frames, 64 tables, 64
  mappings, 4 MemoryObjects and 2 pages per object.
- Architecture v0.2 and ABI 0.5 are unchanged. There is no ABI 0.6, public
  MemoryObject operation, capability transfer, mmap, SMP, TLB shootdown, COW or
  demand paging.
- The trusted controller and fixture code are test-only. They are not a
  production object interface.
- The QMP observer depends on the defined `repr(C)`/`repr(C, u64)` production
  layouts and on the single stopped-CPU execution model. Host tests exercise the
  generic mapping/teardown contract; they do not execute x86 Ring 3 code.

## History

- 2026-10-28: Proposed and accepted for the bounded Phase 8A-2 scope after the
  live profiles and independent stopped-CPU oracle passed, including two clean
  builds/boots. An initial full-suite attempt exposed the shared 4 KiB fixture
  page bound; the live assembly block was feature-gated before final acceptance.
  Earlier Phase 8A-1 rationale remains in ADR 0022.

## Scoped acceptance (2026-10-28)

Accepted after 231 host tests and 142 unique QEMU scenarios passed, including
four live profiles whose QMP evidence verifies real saved Ring 3 frames, raw
physical payload bytes, user PTE rights, the hardware fault static, Node/Thread/
Space identities and exact final allocator restoration. Focused live cases were
repeated, and two independent clean source/target builds/boots passed. This
accepts only the trusted test contract above; it does not accept a public
MemoryObject ABI or general shared-memory facility.

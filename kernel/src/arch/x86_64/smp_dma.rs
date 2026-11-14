//! DMA is a child of the exclusive device lease. Every public operation runs
//! in the scheduler transaction; no caller supplies a physical address or RID.
use super::*;
use crate::arch::{iommu as vtd, pci as bus};
use abi::v08 as dop;
use neu_kernel::{dma as policy, pci::Bdf};

const SCRUB_IOVA: u64 = 0x180000;
const INTERNAL: u64 = 0x40000;
const CANARY: u64 = 0xdad0_cafe_55aa_33cc;
const SECOND: u64 = paging::LAPIC_VIEW + 3 * PAGE;
#[unsafe(no_mangle)]
static neu_dma_pool: [vtd::Page; 5] = [const { vtd::Page::new() }; 5];
static POOL_PA: [AtomicU64; 5] = [const { AtomicU64::new(0) }; 5];
#[unsafe(no_mangle)]
static neu_dma_layout: [AtomicU64; 32] = [const { AtomicU64::new(0) }; 32];
#[unsafe(no_mangle)]
static neu_dma_events: [[AtomicU64; 20]; 128] = [const { [const { AtomicU64::new(0) }; 20] }; 128];

pub(super) struct Controller {
    lease: policy::Lease,
    domain: Option<vtd::Domain>,
    handles: [u64; 2],
    grants: [cap::GrantId; 6],
    scrub: u64,
    events: usize,
    test: u64,
    second: Option<(Bdf, u64)>,
    cutoff: bool,
}
impl Controller {
    pub const fn new() -> Self {
        Self {
            lease: policy::Lease::new(),
            domain: None,
            handles: [0; 2],
            grants: [cap::GrantId::NONE; 6],
            scrub: 0,
            events: 0,
            test: 0,
            second: None,
            cutoff: false,
        }
    }
}
fn load64(offset: u64) -> u64 {
    unsafe { ptr::read_volatile((MMIO + offset) as *const u64) }
}
fn store64(offset: u64, value: u64) {
    unsafe {
        ptr::write_volatile((MMIO + offset) as *mut u64, value);
    }
}
fn issue(address: u64, length: u64, direction: u64) {
    for page in &neu_dma_pool {
        page.sync();
    }
    core::sync::atomic::fence(Ordering::SeqCst);
    store64(0x80, if direction == 1 { address } else { INTERNAL });
    store64(0x88, if direction == 1 { INTERNAL } else { address });
    store64(0x90, length);
    store64(0x98, if direction == 1 { 5 } else { 7 });
}
fn idle() -> bool {
    load64(0x98) & 1 == 0
}
fn zero(page: usize, value: u64) {
    for word in &neu_dma_pool[page].0 {
        word.store(value, Ordering::Release);
    }
}
fn physical(page: usize) -> u64 {
    POOL_PA[page].load(Ordering::Acquire)
}
fn word(index: usize) -> &'static AtomicU64 {
    &neu_dma_pool[if index < 512 { 0 } else { 2 }].0[index % 512]
}
fn intact() -> bool {
    neu_dma_pool[1]
        .0
        .iter()
        .all(|v| v.load(Ordering::Acquire) == CANARY)
}
fn no_faults() -> Result<(), &'static str> {
    for unit in 0..vtd::count() {
        if vtd::pending_fault(unit)?.is_some() {
            return Err("unexpected hardware DMA fault");
        }
    }
    Ok(())
}
fn poll_completion() -> Result<(), &'static str> {
    // Boot only, before any userspace owner. QEMU's 100ms virtual DMA timer
    // must actually raise completion, not merely clear RUN.
    for _ in 0..20_000_000 {
        if reg(0x24) == policy::IRQ as u32 && idle() {
            for page in &neu_dma_pool {
                page.sync();
            }
            return no_faults();
        }
        core::hint::spin_loop();
    }
    Err("DMA boot scrub did not complete")
}
fn event(s: &mut State, kind: u64, a: u64, b: u64) {
    let d = &mut s.device.dma;
    assert!(d.events < neu_dma_events.len(), "DMA event capacity");
    let l = &d.lease;
    let domain = d.domain.expect("DMA domain initialized");
    let row = [
        d.events as u64 + 1,
        kind,
        l.owner,
        l.generation,
        l.rights,
        l.busy,
        l.quarantined,
        l.epoch,
        l.invalidated,
        l.submissions,
        l.completions,
        l.offset,
        l.length,
        l.direction,
        a,
        b,
        TICKS.load(Ordering::Acquire),
        domain.unit as u64,
        domain.slot as u64 + 1,
        u64::from(domain.requester),
    ];
    publish(&neu_dma_events[d.events], row);
    d.events += 1;
}
pub(super) fn snapshot(s: &State) {
    let d = &s.device.dma;
    for (i, value) in [
        (0, 1),
        (1, ptr::from_ref(&d.lease) as u64),
        (2, size_of::<policy::Lease>() as u64),
        (3, neu_dma_pool.as_ptr() as u64),
        (4, POOL_PA.as_ptr() as u64),
        (5, 5),
        (6, d.handles.as_ptr() as u64),
        (7, d.grants.as_ptr() as u64),
        (8, ptr::from_ref(&d.scrub) as u64),
        (9, neu_dma_events.as_ptr() as u64),
        (10, ptr::from_ref(&d.events) as u64),
        (11, 128),
        (12, 20),
        (13, policy::IOVA),
        (14, SCRUB_IOVA),
        (15, CANARY),
        (16, ptr::from_ref(&d.test) as u64),
    ] {
        neu_dma_layout[i].store(value, Ordering::Release);
    }
}
pub(super) fn init(paging: &mut Paging<'_, '_>) -> Result<(), &'static str> {
    let pat = LAPIC_PAT.load(Ordering::Acquire);
    let selected = bus::discover(paging, pat)?;
    let bdf = selected.function.bdf;
    SELECTED.store(u64::from(bdf.requester()), Ordering::Release);
    GSI.store(u64::from(selected.gsi), Ordering::Release);
    paging
        .map_edu(selected.bar.base, pat)
        .map_err(|_| "DMA EDU UC maps")?;
    BAR.store(selected.bar.base, Ordering::Release);
    if io_read(1) & 0x00ff_00ff != 0x0017_0020 || route() & (MASK | 0x4000) != MASK {
        return Err("DMA requires quiescent Q35 I/O APIC");
    }
    let old_route = route();
    let old_destination = io_read(0x11 + gsi() * 2);
    let destination = smp::neu_smp_cpus[0].load(cpu::block::APIC_ID);
    if destination >= 255 {
        return Err("DMA xAPIC destination");
    }
    mask();
    io_write(0x11 + gsi() * 2, (destination as u32) << 24);
    vtd::initialize(paging, pat)?;
    let domain = vtd::Domain::attach(selected.requester)?;
    for (index, page) in neu_dma_pool.iter().enumerate() {
        POOL_PA[index].store(vtd::page_physical(paging, page)?, Ordering::Release);
        zero(index, if index == 1 { CANARY } else { 0 });
    }
    // Explicitly map only the selected endpoint, after default-deny translation
    // is active. Firmware's bus-master state is never treated as authority.
    bus::command(bdf, (bus::read(bdf, 4) as u16 & !0x407) | 2);
    bus::bus_master_path(bdf, true);
    if bus::read(bdf, 4) & 0x407 != 6 || reg(0) != protocol::IDENT || !idle() || reg(0x20) & 1 != 0
    {
        return Err("DMA EDU configuration readback");
    }
    put(0x20, 0);
    put(0x64, u32::MAX);
    domain.map(SCRUB_IOVA, &[physical(3)], 1, 0)?;
    domain.map(SCRUB_IOVA + PAGE, &[physical(4)], 2, 0)?;
    zero(4, CANARY);
    issue(SCRUB_IOVA, 4096, 1);
    poll_completion()?;
    put(0x64, policy::IRQ as u32);
    issue(SCRUB_IOVA + PAGE, 4096, 2);
    poll_completion()?;
    if neu_dma_pool[4]
        .0
        .iter()
        .any(|v| v.load(Ordering::Acquire) != 0)
    {
        return Err("DMA boot scrub readback");
    }
    put(0x64, policy::IRQ as u32);
    domain.unmap(SCRUB_IOVA, 2, 0)?;
    for offset in [0x80, 0x88, 0x90] {
        store64(offset, 0);
    }
    let fw = crate::boot::pci_firmware();
    let ecam = fw.ecam[..fw.ecam_count]
        .iter()
        .find(|e| e.segment == bdf.segment && (e.first..=e.last).contains(&bdf.bus))
        .ok_or("DMA PCI ECAM owner")?;
    for (index, value) in [
        (0, 1),
        (3, selected.bar.base),
        (4, selected.bar.size),
        (5, MMIO),
        (6, IOAPIC),
        (7, u64::from(selected.gsi)),
        (8, u64::from(bdf.requester())),
        (9, ecam.base),
        (10, EVENT.as_ptr() as u64),
        (11, ptr::from_ref(&TICKS) as u64),
        (12, ptr::from_ref(&EPOCH) as u64),
        (13, neu_device_operations.as_ptr() as u64),
        (14, JOURNAL as u64),
        (15, cap::Table::grant_offset()),
        (16, u64::from(selected.function.msi)),
        (40, u64::from(selected.function.command)),
        (41, u64::from(old_route)),
        (42, u64::from(old_destination)),
        (43, destination),
    ] {
        neu_device_layout[index].store(value, Ordering::Release);
    }
    with_state(0, |s| {
        s.device.dma.domain = Some(domain);
        s.device.device.discover().expect("DMA discovery state");
        event(s, 1, physical(0), physical(2));
    });
    if cfg!(feature = "test-smp-dma-requester") {
        let (second, bar) = bus::extra_edu(paging, pat, bdf)?;
        paging
            .map_platform_registers(SECOND, bar.base, pat)
            .map_err(|_| "DMA adversary view")?;
        // Seed the adversary's private SRAM under a temporary, separate domain.
        // Detach and invalidate it before any application runs. Its later write
        // must be rejected even though the selected device has a valid mapping.
        let adversary = vtd::Domain::attach(second)?;
        adversary.map(SCRUB_IOVA, &[physical(1)], 1, 0)?;
        bus::command(second, 0x406);
        unsafe {
            ptr::write_volatile((SECOND + 0x80) as *mut u64, SCRUB_IOVA);
            ptr::write_volatile((SECOND + 0x88) as *mut u64, INTERNAL);
            ptr::write_volatile((SECOND + 0x90) as *mut u64, 4096);
            ptr::write_volatile((SECOND + 0x98) as *mut u64, 5);
        }
        let mut complete = false;
        for _ in 0..20_000_000 {
            if unsafe { ptr::read_volatile((SECOND + 0x24) as *const u32) } == policy::IRQ as u32
                && unsafe { ptr::read_volatile((SECOND + 0x98) as *const u64) } & 1 == 0
            {
                complete = true;
                break;
            }
            core::hint::spin_loop();
        }
        if !complete {
            return Err("DMA adversary seed timeout");
        }
        no_faults()?;
        for page in &neu_dma_pool {
            page.sync();
        }
        unsafe {
            ptr::write_volatile((SECOND + 0x64) as *mut u32, policy::IRQ as u32);
        }
        adversary.unmap(SCRUB_IOVA, 1, 0)?;
        adversary.detach()?;
        bus::command(second, 0x402);
        with_state(0, |s| s.device.dma.second = Some((second, bar.base)));
        neu_dma_layout[17].store(second.key(), Ordering::Release);
        neu_dma_layout[18].store(bar.base, Ordering::Release);
    }
    Ok(())
}
pub(super) fn claim(s: &mut State, owner: u64, generation: u64) {
    s.device
        .dma
        .lease
        .claim(owner, generation)
        .expect("DMA retired lease before claim");
    s.device.dma.scrub = 0;
    s.device.dma.cutoff = false;
    event(s, 2, 0, 0);
}
pub(super) fn quarantine(s: &mut State) {
    s.device.dma.lease.quarantined = 1;
    event(s, 3, load64(0x98), u64::from(reg(0x24)));
}
pub(super) fn revoke_selects(s: &State, ticket: &cap::Revocation) -> bool {
    s.device.dma.grants.iter().any(|g| {
        *g != cap::GrantId::NONE
            && s.objects
                .revoke_selects(ticket, *g)
                .expect("DMA revoke selection")
    })
}
fn err<T>(r: policy::Result<T>) -> core::result::Result<T, i64> {
    r.map_err(|e| e as i64)
}
fn authorize(
    s: &mut State,
    table: &cap::Table,
    node: NodeId,
    h: Handle,
    kind: ObjectType,
    rights: Rights,
) -> core::result::Result<(), i64> {
    let generation = token(table, &mut s.objects, node, h, kind, rights)?;
    d(s.device.device.authorize(node.raw(), generation))?;
    err(s.device.dma.lease.authorize(node.raw(), generation))
}
pub(super) fn operation(
    s: &mut State,
    table: &mut cap::Table,
    active: ActiveContext,
    number: u64,
    args: [u64; 6],
) -> core::result::Result<u64, i64> {
    let h = Handle::from_raw(args[0]);
    let node = active.node;
    let domain = s.device.dma.domain.ok_or(policy::Error::Isolation as i64)?;
    match number {
        dop::DMA_DOMAIN => {
            authorize(s, table, node, h, ObjectType::MmioRegion, Rights::READ)?;
            if h.raw() != s.device.handles[1] {
                return Err(cap::Error::Rights as i64);
            }
            if s.device.dma.handles[0] == 0 {
                let value = c(s.objects.install_dma_capability(
                    table,
                    ObjectType::DmaDomain,
                    s.device.device.generation,
                ))?;
                s.device.dma.handles[0] = value.raw();
                s.device.dma.grants[0] = c(table.grant_id(&s.objects, node, value))?;
            }
            Ok(s.device.dma.handles[0])
        }
        dop::DMA_BUFFER => {
            authorize(s, table, node, h, ObjectType::DmaDomain, Rights::CLAIM)?;
            // Allocation returns the original buffer anchor. An attenuated
            // domain cannot turn MAP-only permission back into buffer writes.
            if h.raw() != s.device.dma.handles[0] {
                return Err(cap::Error::Rights as i64);
            }
            if s.device.dma.handles[1] == 0 {
                let value = c(s.objects.install_dma_capability(
                    table,
                    ObjectType::DmaBuffer,
                    s.device.device.generation,
                ))?;
                s.device.dma.handles[1] = value.raw();
                s.device.dma.grants[1] = c(table.grant_id(&s.objects, node, value))?;
            }
            Ok(s.device.dma.handles[1])
        }
        dop::DMA_READ | dop::DMA_WRITE => {
            authorize(
                s,
                table,
                node,
                h,
                ObjectType::DmaBuffer,
                if number == dop::DMA_READ {
                    Rights::READ
                } else {
                    Rights::WRITE
                },
            )?;
            let index = err(s.device.dma.lease.word(args[1]))?;
            if number == dop::DMA_READ {
                Ok(word(index).load(Ordering::Acquire))
            } else {
                word(index).store(args[2], Ordering::Release);
                Ok(0)
            }
        }
        dop::DMA_MAP | dop::DMA_UNMAP | dop::DMA_SUBMIT => {
            authorize(
                s,
                table,
                node,
                h,
                ObjectType::DmaDomain,
                if number == dop::DMA_SUBMIT {
                    Rights::SEND
                } else {
                    Rights::CLAIM
                },
            )?;
            let buffer = Handle::from_raw(args[1]);
            let permissions = if number == dop::DMA_MAP {
                args[2]
            } else if number == dop::DMA_SUBMIT {
                args[4]
            } else {
                0
            };
            if number != dop::DMA_UNMAP
                && (permissions == 0
                    || permissions & !3 != 0
                    || number == dop::DMA_SUBMIT && permissions == 3)
            {
                return Err(policy::Error::Permission as i64);
            }
            let required = Rights::from_authority_bits(4 | permissions).expect("DMA access mask");
            authorize(s, table, node, buffer, ObjectType::DmaBuffer, required)?;
            if number == dop::DMA_MAP {
                let epoch = err(s.device.dma.lease.map(permissions))?;
                domain
                    .map(
                        policy::IOVA,
                        &[physical(0), physical(2)],
                        permissions as u8,
                        epoch,
                    )
                    .map_err(|_| policy::Error::Isolation as i64)?;
                err(s.device.dma.lease.complete_invalidation(epoch))?;
                s.device.dma.grants[2] = c(table.grant_id(&s.objects, node, h))?;
                s.device.dma.grants[3] = c(table.grant_id(&s.objects, node, buffer))?;
                event(s, 4, policy::IOVA, permissions);
                Ok(policy::IOVA)
            } else if number == dop::DMA_UNMAP {
                let epoch = err(s.device.dma.lease.unmap())?;
                domain
                    .unmap(policy::IOVA, 2, epoch)
                    .map_err(|_| policy::Error::Isolation as i64)?;
                err(s.device.dma.lease.complete_invalidation(epoch))?;
                s.device.dma.grants[2..4].fill(cap::GrantId::NONE);
                event(s, 5, policy::IOVA, 0);
                Ok(0)
            } else {
                if s.device.device.phase != phase::ARMED || !idle() || reg(0x24) != 0 {
                    return Err(policy::Error::Busy as i64);
                }
                no_faults().map_err(|_| policy::Error::Isolation as i64)?;
                let address = err(s.device.dma.lease.submit(args[2], args[3], args[4]))?;
                s.device.device.flight = policy::IRQ;
                s.device.device.phase = phase::FLIGHT;
                s.device.device.deadline = TICKS
                    .load(Ordering::Acquire)
                    .saturating_add(protocol::WATCHDOG);
                s.device.dma.grants[4] = c(table.grant_id(&s.objects, node, h))?;
                s.device.dma.grants[5] = c(table.grant_id(&s.objects, node, buffer))?;
                issue(address, args[3], args[4]);
                event(s, 6, address, args[3]);
                Ok(s.device.dma.lease.submissions)
            }
        }
        _ => Err(abi::NOT_SUPPORTED),
    }
}
pub(super) fn completion(s: &mut State, status: u64) -> Result<(), &'static str> {
    if status == policy::IRQ && s.device.dma.lease.busy != 0 {
        if !idle() {
            return Err("DMA IRQ preceded RUN retirement");
        }
        no_faults()?;
        for page in &neu_dma_pool {
            page.sync();
        }
        core::sync::atomic::fence(Ordering::SeqCst);
        s.device
            .dma
            .lease
            .complete()
            .map_err(|_| "DMA completion without submit")?;
        s.device.dma.grants[4..].fill(cap::GrantId::NONE);
        if !intact() {
            return Err("DMA guard corruption");
        }
        event(s, 7, status, 0);
    }
    Ok(())
}
/// Asynchronous quarantine scrub, with actual masked completions in both
/// directions. The device's internal SRAM is part of the old owner's data.
pub(super) fn retire(s: &mut State) -> Result<bool, &'static str> {
    if s.device.dma.lease.owner == 0 {
        return Ok(true);
    }
    if s.device.dma.lease.busy != 0 || !idle() {
        return Ok(false);
    }
    no_faults()?;
    if !intact() {
        return Err("DMA guard changed before retirement");
    }
    let domain = s.device.dma.domain.expect("DMA retirement domain");
    match s.device.dma.scrub {
        0 => {
            if s.device.dma.lease.rights != 0 {
                let epoch = s
                    .device
                    .dma
                    .lease
                    .unmap()
                    .map_err(|_| "DMA retire map state")?;
                domain.unmap(policy::IOVA, 2, epoch)?;
                s.device
                    .dma
                    .lease
                    .complete_invalidation(epoch)
                    .map_err(|_| "DMA retire invalidation")?;
            }
            zero(0, 0);
            zero(2, 0);
            zero(3, 0);
            zero(4, CANARY);
            domain.map(SCRUB_IOVA, &[physical(3)], 1, 0)?;
            domain.map(SCRUB_IOVA + PAGE, &[physical(4)], 2, 0)?;
            put(0x64, u32::MAX);
            issue(SCRUB_IOVA, 4096, 1);
            s.device.dma.scrub = 1;
            event(s, 8, 1, 0);
        }
        1 if reg(0x24) == policy::IRQ as u32 => {
            put(0x64, policy::IRQ as u32);
            issue(SCRUB_IOVA + PAGE, 4096, 2);
            s.device.dma.scrub = 2;
            event(s, 8, 2, 0);
        }
        2 if reg(0x24) == policy::IRQ as u32 => {
            neu_dma_pool[4].sync();
            if neu_dma_pool[4]
                .0
                .iter()
                .any(|v| v.load(Ordering::Acquire) != 0)
            {
                return Err("DMA device buffer scrub readback");
            }
            s.device.dma.scrub = 3;
            event(s, 8, 3, 0);
        }
        4 => {
            put(0x64, policy::IRQ as u32);
            domain.unmap(SCRUB_IOVA, 2, 0)?;
            for offset in [0x80, 0x88, 0x90] {
                store64(offset, 0);
            }
            s.device
                .dma
                .lease
                .release(true, true)
                .map_err(|_| "DMA lease release")?;
            s.device.dma.handles = [0; 2];
            s.device.dma.grants = [cap::GrantId::NONE; 6];
            s.device.dma.scrub = 5;
            event(s, 9, 0, 0);
            return Ok(true);
        }
        _ => (),
    }
    Ok(false)
}
pub(super) fn fixture_service(s: &mut State) -> Result<u64, &'static str> {
    if s.device.dma.lease.quarantined != 0
        && s.device.dma.lease.busy != 0
        && !s.device.dma.cutoff
        && !idle()
    {
        s.device.dma.cutoff = true;
        return Ok(33);
    }
    if barrier_word(26).load(Ordering::Acquire) == 1
        && barrier_word(27).load(Ordering::Acquire) == 0
        && s.device.device.phase == phase::ARMED
    {
        return Ok(30);
    }
    if s.device.dma.scrub == 3 {
        return Ok(31);
    }
    if !(10..=13).contains(&mode()) || barrier_word(22).load(Ordering::Acquire) == 0 {
        return Ok(0);
    }
    let d = &mut s.device.dma;
    let domain = d.domain.expect("DMA probe domain");
    let view = if mode() == 12 { SECOND } else { MMIO };
    match d.test {
        0 => {
            if s.device.device.phase != phase::ARMED || d.lease.busy != 0 {
                return Ok(0);
            }
            mask();
            no_faults()?;
            let address = if mode() == 10 {
                policy::IOVA + 2 * PAGE
            } else {
                policy::IOVA
            };
            if let Some((second, _)) = d.second {
                bus::command(second, 0x406);
            }
            unsafe {
                ptr::write_volatile((view + 0x80) as *mut u64, INTERNAL);
                ptr::write_volatile((view + 0x88) as *mut u64, address);
                ptr::write_volatile((view + 0x90) as *mut u64, 32);
                ptr::write_volatile((view + 0x98) as *mut u64, 7);
            }
            d.test = 1;
            neu_dma_layout[19].store(address, Ordering::Release);
        }
        1 => {
            let status = unsafe { ptr::read_volatile((view + 0x24) as *const u32) };
            let command = unsafe { ptr::read_volatile((view + 0x98) as *const u64) };
            if status != policy::IRQ as u32 || command & 1 != 0 {
                return Ok(0);
            }
            let fault =
                vtd::pending_fault(domain.unit)?.ok_or("DMA isolation probe reached memory")?;
            let requester = d.second.map_or(domain.requester, |s| s.0.requester());
            let address = neu_dma_layout[19].load(Ordering::Acquire) & !4095;
            let reason = if mode() == 12 { 2 } else { 5 };
            if fault.requester != requester
                || fault.address != address
                || fault.read
                || fault.reason != reason
                || !intact()
            {
                serial::emergency(format_args!(
                    "[NEU] DMA fault mismatch: actual={fault:?} expected={requester:#x}/{address:#x}/{reason} guard={}\n",
                    intact()
                ));
                return Err("DMA isolation fault identity/access/guard");
            }
            for (i, value) in [
                (20, u64::from(fault.requester)),
                (21, fault.address),
                (22, u64::from(fault.reason)),
                (23, u64::from(fault.read)),
            ] {
                neu_dma_layout[i].store(value, Ordering::Release);
            }
            d.test = 2;
            event(s, 10, address, u64::from(requester));
            return Ok(32);
        }
        3 => {
            unsafe {
                ptr::write_volatile((view + 0x64) as *mut u32, policy::IRQ as u32);
            }
            vtd::clear_faults(domain.unit)?;
            if let Some((second, _)) = d.second {
                bus::command(second, 0x402);
            }
            d.test = 4;
            io_write(0x10 + gsi() * 2, ROUTE);
            barrier_word(23).store(1, Ordering::Release);
        }
        _ => (),
    }
    Ok(0)
}
pub(super) fn checkpoint_done(s: &mut State, checkpoint: u64) {
    match checkpoint {
        30 => barrier_word(27).store(1, Ordering::Release),
        31 => s.device.dma.scrub = 4,
        32 => s.device.dma.test = 3,
        _ => (),
    }
}
pub(super) fn cleanup(paging: &mut Paging<'_, '_>) -> Result<(), &'static str> {
    if cfg!(feature = "test-smp-dma-requester") {
        with_state(0, |s| {
            let (second, _) = s.device.dma.second.expect("DMA adversary state");
            bus::command(second, 0x400);
        });
        paging
            .unmap_platform_registers(SECOND)
            .map_err(|_| "DMA adversary view cleanup")?;
    }
    with_state(0, |s| -> Result<(), &'static str> {
        if s.device.dma.lease.owner != 0
            || s.device.dma.lease.rights != 0
            || s.device.dma.lease.busy != 0
            || s.device.dma.handles != [0; 2]
            || !intact()
            || [0, 2, 3, 4].iter().any(|&i| {
                neu_dma_pool[i]
                    .0
                    .iter()
                    .any(|v| v.load(Ordering::Acquire) != 0)
            })
            || !idle()
            || [0x80, 0x88, 0x90].iter().any(|&o| load64(o) != 0)
        {
            return Err("DMA residual ownership/buffer/command state");
        }
        s.device
            .dma
            .domain
            .take()
            .ok_or("DMA missing final domain")?
            .detach()?;
        Ok(())
    })?;
    vtd::cleanup(paging)?;
    neu_dma_layout[31].store(1, Ordering::Release);
    Ok(())
}

// Trusted fixture selection is rendezvous data, never a public privilege.
pub(super) const fn mode() -> u64 {
    if cfg!(feature = "test-smp-dma-close") {
        2
    } else if cfg!(feature = "test-smp-dma-revoke") {
        3
    } else if cfg!(feature = "test-smp-dma-timeout") {
        4
    } else if cfg!(feature = "test-smp-dma-quota") {
        5
    } else if cfg!(feature = "test-smp-dma-fault") {
        6
    } else if cfg!(feature = "test-smp-dma-submit-revoke") {
        7
    } else if cfg!(feature = "test-smp-dma-unmapped") {
        10
    } else if cfg!(feature = "test-smp-dma-readonly") {
        11
    } else if cfg!(feature = "test-smp-dma-requester") {
        12
    } else if cfg!(feature = "test-smp-dma-revoked") {
        13
    } else {
        0
    }
}

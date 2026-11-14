//! Q35/EDU backend. PCI and I/O APIC selector cycles have one BSP owner.
//! Native operations share the scheduler transaction; the hard IRQ only masks,
//! captures a bounded event and EOIs. No IRQ takes the shared lock.
use super::*;
use abi::v07 as op;
use neu_kernel::device::{self as protocol, Device, phase, reason};
pub(super) const MMIO: u64 = paging::LAPIC_VIEW + PAGE;
pub(super) const IOAPIC: u64 = paging::LAPIC_VIEW + 2 * PAGE;
const GSI: u32 = 21;
const ROUTE: u32 = 0xa000 | protocol::VECTOR as u32;
const MASK: u32 = 1 << 16;
const JOURNAL: usize = 512;
static BAR: AtomicU64 = AtomicU64::new(0);
static EPOCH: AtomicU64 = AtomicU64::new(0);
static TICKS: AtomicU64 = AtomicU64::new(0);
static EVENT: [AtomicU64; 2] = [const { AtomicU64::new(0) }; 2];
#[unsafe(no_mangle)]
static neu_device_layout: [AtomicU64; 64] = [const { AtomicU64::new(0) }; 64];
#[unsafe(no_mangle)]
static neu_device_irq: [[AtomicU64; 40]; 16] = [const { [const { AtomicU64::new(0) }; 40] }; 16];
#[unsafe(no_mangle)]
static neu_device_operations: [[AtomicU64; 32]; JOURNAL] =
    [const { [const { AtomicU64::new(0) }; 32] }; JOURNAL];
#[unsafe(no_mangle)]
static neu_device_checkpoint: AtomicU64 = AtomicU64::new(0);
struct Pending {
    active: ActiveContext,
    frame: Frame,
    cpu: usize,
    entry: u64,
}
pub(super) struct Controller {
    device: Device,
    handles: [u64; 4],
    grants: [cap::GrantId; 4],
    wait: Option<Pending>,
    wakes: u64,
    operations: usize,
    wait_checkpoints: u64,
    pending_checkpoint: bool,
    release_checkpoint: bool,
    scrub: u64,
}
impl Controller {
    pub const fn new() -> Self {
        Self {
            device: Device::new(),
            handles: [0; 4],
            grants: [cap::GrantId::NONE; 4],
            wait: None,
            wakes: 0,
            operations: 0,
            wait_checkpoints: 0,
            pending_checkpoint: false,
            release_checkpoint: false,
            scrub: 0,
        }
    }
}
pub(super) fn physical() -> u64 {
    BAR.load(Ordering::Acquire)
}
fn reg(offset: u64) -> u32 {
    unsafe { ptr::read_volatile((MMIO + offset) as *const u32) }
}
fn put(offset: u64, value: u32) {
    unsafe { ptr::write_volatile((MMIO + offset) as *mut u32, value) }
}
fn pci_select(bdf: u32, offset: u32) {
    assert!(current() == 0 && flags() & 0x200 == 0);
    unsafe {
        asm!("out dx, eax", in("dx") 0xcf8u16, in("eax") 0x8000_0000u32 | (bdf << 8) | (offset & 0xfc), options(nostack, preserves_flags));
    }
}
fn pci(bdf: u32, offset: u32) -> u32 {
    pci_select(bdf, offset);
    let value;
    unsafe {
        asm!("in eax, dx", in("dx") 0xcfcu16, out("eax") value, options(nostack, preserves_flags));
    }
    value
}
fn pci32(bdf: u32, offset: u32, value: u32) {
    pci_select(bdf, offset);
    unsafe {
        asm!("out dx, eax", in("dx") 0xcfcu16, in("eax") value, options(nostack, preserves_flags));
    }
}
fn pci16(bdf: u32, offset: u32, value: u16) {
    pci_select(bdf, offset);
    unsafe {
        asm!("out dx, ax", in("dx") (0xcfcu16 + (offset & 2) as u16), in("ax") value, options(nostack, preserves_flags));
    }
}
fn io_read(index: u32) -> u32 {
    assert!(current() == 0 && flags() & 0x200 == 0);
    unsafe {
        ptr::write_volatile(IOAPIC as *mut u32, index);
        ptr::read_volatile((IOAPIC + 0x10) as *const u32)
    }
}
fn io_write(index: u32, value: u32) {
    assert!(current() == 0 && flags() & 0x200 == 0);
    unsafe {
        ptr::write_volatile(IOAPIC as *mut u32, index);
        ptr::write_volatile((IOAPIC + 0x10) as *mut u32, value);
    }
}
fn route() -> u32 {
    io_read(0x10 + GSI * 2)
}
fn mask() {
    io_write(0x10 + GSI * 2, ROUTE | MASK);
}
pub(super) fn init(paging: &mut Paging<'_, '_>) -> Result<(), &'static str> {
    if pci(0, 0) != 0x29c0_8086 || pci(0xf8, 0) != 0x2918_8086 {
        return Err("K11 requires validated Q35/ICH9");
    }
    let mut found = None;
    for bdf in 0..256 {
        let id = pci(bdf, 0);
        if id == protocol::PCI_ID {
            if found.replace(bdf).is_some() {
                return Err("K11 conflicting EDU inventory");
            }
        } else if id as u16 != 0xffff && bdf >> 3 < 25 {
            let pin = (pci(bdf, 0x3c) >> 8) & 0xff;
            if (1..=4).contains(&pin) && 20 + (((bdf >> 3) + pin - 1) % 4) == GSI {
                return Err("K11 shared INTx route unsupported");
            }
        }
    }
    let bdf = found.ok_or("K11 EDU absent")?;
    if bdf != 0x28 || pci(bdf, 0x3c) >> 8 & 0xff != 1 || pci(bdf, 0x0c) >> 16 & 0x7f != 0 {
        return Err("K11 unsupported EDU placement/header/pin");
    }
    let command = pci(bdf, 4) as u16;
    // Write only the command halfword: never write back W1C status bits.
    pci16(bdf, 4, (command & !7) | 0x400);
    let bar = pci(bdf, 0x10);
    pci32(bdf, 0x10, u32::MAX);
    let size = (!(pci(bdf, 0x10) & !15)).wrapping_add(1) as u64;
    pci32(bdf, 0x10, bar);
    if bar & 15 != 0 || !protocol::valid_resource(bdf as u64, bar as u64, size, GSI as u64) {
        return Err("K11 invalid EDU aperture");
    }
    let mut cap = pci(bdf, 0x34) & 0xff;
    let mut seen = [false; 64];
    let mut msi = None;
    for _ in 0..48 {
        if cap == 0 {
            break;
        }
        if !(0x40..=0xf0).contains(&cap) || cap & 3 != 0 || seen[(cap / 4) as usize] {
            return Err("K11 malformed PCI capabilities");
        }
        seen[(cap / 4) as usize] = true;
        let value = pci(bdf, cap);
        if value & 0xff == 5 {
            if msi.replace(cap).is_some() {
                return Err("K11 duplicate MSI capability");
            }
            pci16(bdf, cap + 2, (value >> 16) as u16 & !1);
        }
        cap = (value >> 8) & 0xff;
    }
    if cap != 0 || msi.is_none() {
        return Err("K11 missing/bounded PCI capabilities");
    }
    let pat = LAPIC_PAT.load(Ordering::Acquire);
    paging
        .map_edu(bar as u64, pat)
        .map_err(|_| "K11 device UC maps")?;
    BAR.store(bar as u64, Ordering::Release);
    if io_read(1) & 0x00ff_00ff != 0x0017_0020 || route() & MASK == 0 || route() & 0x4000 != 0 {
        return Err("K11 I/O APIC is not a quiescent Q35 controller");
    }
    let old_route = route();
    let old_destination = io_read(0x11 + GSI * 2);
    let destination = smp::neu_smp_cpus[0].load(cpu::block::APIC_ID);
    if destination >= 255 {
        return Err("K11 xAPIC destination");
    }
    mask();
    io_write(0x11 + GSI * 2, (destination as u32) << 24);
    pci16(bdf, 4, (command & !0x407) | 2);
    if pci(bdf, 4) as u16 & 0x407 != 2 || reg(0) != protocol::IDENT || reg(0x20) & 1 != 0 {
        return Err("K11 device configuration readback");
    }
    put(0x20, 0);
    put(0x64, u32::MAX);
    let ecam = (u64::from(pci(0, 0x64)) << 32) | u64::from(pci(0, 0x60));
    if ecam & 7 != 1 {
        return Err("K11 requires Q35 256 MiB ECAM for independent observation");
    }
    for (index, value) in [
        (0, 1),
        (3, bar as u64),
        (4, size),
        (5, MMIO),
        (6, IOAPIC),
        (7, GSI as u64),
        (8, bdf as u64),
        (9, ecam & !0x0fff_ffff),
        (10, EVENT.as_ptr() as u64),
        (11, ptr::from_ref(&TICKS) as u64),
        (12, ptr::from_ref(&EPOCH) as u64),
        (13, neu_device_operations.as_ptr() as u64),
        (14, JOURNAL as u64),
        (15, cap::Table::grant_offset()),
        (16, msi.unwrap() as u64),
        (40, command as u64),
        (41, old_route as u64),
        (42, old_destination as u64),
        (43, destination),
    ] {
        neu_device_layout[index].store(value, Ordering::Release);
    }
    with_state(0, |s| s.device.device.discover()).map_err(|_| "K11 discovery state")?;
    Ok(())
}
pub(super) fn bootstrap(s: &mut State) -> Result<(), &'static str> {
    let resource = s
        .objects
        .device_resource()
        .map_err(|_| "K11 resource object")?;
    for node in s.node_ids.into_iter().flatten() {
        s.nodes
            .authority_mut(node)
            .map_err(|_| "K11 resource table")?
            .configure_devices(&mut s.objects, &resource)
            .map_err(|_| "K11 resource grant")?;
    }
    s.objects
        .release(resource)
        .map_err(|_| "K11 resource creator")?;
    // Fixture mode is data, never a privilege checked by a public operation.
    let mode = if cfg!(feature = "test-smp-device-flight-fault") {
        6
    } else if cfg!(feature = "test-smp-device-fault") {
        1
    } else if cfg!(feature = "test-smp-device-close") {
        2
    } else if cfg!(feature = "test-smp-device-revoke") {
        3
    } else if cfg!(feature = "test-smp-device-timeout") {
        4
    } else if cfg!(feature = "test-smp-device-budget") {
        5
    } else {
        0
    };
    barrier_word(12).store(mode, Ordering::Release);
    Ok(())
}
pub(super) fn snapshot(s: &State) {
    neu_device_layout[1].store(ptr::from_ref(&s.device.device) as u64, Ordering::Release);
    neu_device_layout[2].store(size_of::<Device>() as u64, Ordering::Release);
    neu_device_layout[17].store(
        ptr::from_ref(&s.device.operations) as u64,
        Ordering::Release,
    );
    neu_device_layout[18].store(s.device.handles.as_ptr() as u64, Ordering::Release);
    neu_device_layout[19].store(s.device.grants.as_ptr() as u64, Ordering::Release);
    for (cell, value) in neu_device_layout[20..34]
        .iter()
        .zip(s.objects.authority_layout())
    {
        cell.store(value, Ordering::Release);
    }
    barrier_word(16).store(s.device.device.phase, Ordering::Release);
    barrier_word(17).store(s.device.device.generation, Ordering::Release);
}
fn append(s: &mut State, mut row: [u64; 32]) {
    let index = s.device.operations;
    assert!(index < JOURNAL, "K11 bounded journal");
    row[0] = index as u64 + 1;
    row[15..24].copy_from_slice(&[
        s.device.device.generation,
        s.device.device.phase,
        s.device.device.owner,
        s.device.device.sequence,
        s.device.device.status,
        s.device.device.waiter,
        s.device.device.receiver,
        s.device.device.reads,
        s.device.device.writes,
    ]);
    row[24..32].copy_from_slice(&[
        TICKS.load(Ordering::Acquire),
        s.device.device.deadline,
        s.device.device.remaining,
        s.device.device.flight,
        s.device.device.claims,
        s.device.device.interrupts,
        s.device.device.acknowledgements,
        s.device.device.reason,
    ]);
    publish(&neu_device_operations[index], row);
    s.device.operations += 1;
}
pub(super) fn record(
    s: &mut State,
    cpu: usize,
    active: ActiveContext,
    frame: Frame,
    result: Outcome,
) {
    let entry = neu_smp_sched_cpus[cpu].get(field::ENTRIES);
    record_at(s, cpu, active, frame, result, 0, entry);
}
fn record_at(
    s: &mut State,
    cpu: usize,
    active: ActiveContext,
    frame: Frame,
    result: Outcome,
    phase: u64,
    entry: u64,
) {
    let (status, scalar) = match result {
        Outcome::Return { status, scalar } => (status, scalar),
        Outcome::Blocked => (-100, 0),
        Outcome::Exit(value) => (-101, u64::from(value)),
    };
    let mut row = [0; 32];
    row[1..7].copy_from_slice(&[
        phase,
        cpu as u64,
        active.node.raw(),
        active.thread.raw(),
        entry,
        frame.registers.rax,
    ]);
    row[7..13].copy_from_slice(&frame.registers.arguments());
    row[13] = status as u64;
    row[14] = scalar;
    append(s, row);
}
fn event(s: &mut State, kind: u64, value: u64, extra: u64) {
    let mut row = [0; 32];
    row[1] = kind;
    row[7] = value;
    row[8] = extra;
    append(s, row);
}
fn settle(s: &mut State, thread: u64, status: i64, scalar: u64, wake: bool) {
    let pending = s
        .device
        .wait
        .take()
        .expect("K11 retained wait continuation");
    assert_eq!(pending.active.thread.raw(), thread);
    s.device.handles[3] = 0;
    s.device.grants[3] = cap::GrantId::NONE;
    if wake {
        s.nodes
            .settle_blocked_smp(pending.active.thread, status, scalar)
            .expect("K11 blocked result");
        let target = s
            .scheduler
            .wake(&mut s.nodes, pending.active.thread)
            .expect("K11 interrupt wake");
        s.device.wakes |= 1 << target;
    }
    record_at(
        s,
        pending.cpu,
        pending.active,
        pending.frame,
        Outcome::Return { status, scalar },
        if wake { 1 } else { 2 },
        pending.entry,
    );
}
fn quarantine(s: &mut State, why: u64, wake: bool) {
    let before = s.device.device.quarantines;
    if let Some(thread) = s.device.device.quarantine(why) {
        settle(
            s,
            thread,
            if why == reason::TIMEOUT { -47 } else { -46 },
            0,
            wake,
        );
    }
    if s.device.device.quarantines != before {
        s.device.wakes |= 1;
        event(s, 5, why, 0);
    }
}
pub(super) fn stop_node(s: &mut State, node: NodeId) {
    if s.device.device.owner == node.raw() {
        quarantine(s, reason::TERMINATE, false);
    }
}
pub(super) fn node_exit(s: &mut State, node: NodeId) {
    stop_node(s, node);
    event(s, 8, node.raw(), 0);
}
pub(super) fn take_wakes(s: &mut State) -> u64 {
    core::mem::take(&mut s.device.wakes)
}
pub(super) fn pending(s: &State) -> bool {
    s.device.device.phase >= phase::REARM
}
fn c<T>(r: cap::Result<T>) -> core::result::Result<T, i64> {
    r.map_err(|e| e as i64)
}
fn d<T>(r: protocol::Result<T>) -> core::result::Result<T, i64> {
    r.map_err(|e| e as i64)
}
fn token(
    table: &cap::Table,
    objects: &mut Objects,
    caller: NodeId,
    handle: Handle,
    kind: ObjectType,
    rights: Rights,
) -> core::result::Result<u64, i64> {
    c(c(table.lookup(objects, caller, handle, kind, rights))?.device_generation())
}
pub(super) fn operation(s: &mut State, cpu: usize, active: ActiveContext, f: Frame) -> Outcome {
    let number = f.registers.rax;
    let args = f.registers.arguments();
    let mut revoke = None;
    let mut quarantine_reason = None;
    let mut lease = s
        .nodes
        .take_authority_on(cpu, active)
        .expect("K11 capability lease");
    let h = Handle::from_raw(args[0]);
    let now = TICKS.load(Ordering::Acquire);
    let result = (|| -> core::result::Result<Outcome, i64> {
        if (op::owns(number) && !op::valid_arguments(number, args))
            || (abi::v06::owns(number) && !abi::v06::valid_arguments(number, args))
        {
            return Err(abi::INVALID_ARGUMENT);
        }
        let scalar = match number {
            abi::v02::CAP_CLOSE => {
                let (object, _) = c(lease.table().describe(active.node, h))?;
                if s.device.device.owner == active.node.raw()
                    && (s.device.handles[0] == h.raw()
                        || matches!(
                            object.kind(),
                            ObjectType::MmioRegion | ObjectType::Interrupt
                        ))
                {
                    quarantine_reason = Some(reason::CLOSE);
                }
                c(lease.table_mut().close(&mut s.objects, active.node, h))?;
                0
            }
            abi::v06::CAP_DERIVE => {
                let rights = c(Rights::from_authority_bits(args[1]))?;
                c(lease
                    .table_mut()
                    .derive(&mut s.objects, active.node, h, rights))?
                .raw()
            }
            abi::v06::CAP_REVOKE => {
                revoke = Some(c(lease.table().begin_revoke(
                    &mut s.objects,
                    active.node,
                    h,
                ))?);
                0
            }
            op::DEVICE_INSPECT => {
                token(
                    lease.table(),
                    &mut s.objects,
                    active.node,
                    h,
                    ObjectType::DeviceResource,
                    Rights::INSPECT,
                )?;
                match args[1] {
                    0 => protocol::PCI_ID as u64,
                    1 => protocol::BAR_SIZE,
                    2 => 7,
                    3 => s.device.device.phase,
                    _ => return Err(abi::INVALID_ARGUMENT),
                }
            }
            op::DEVICE_CLAIM => {
                token(
                    lease.table(),
                    &mut s.objects,
                    active.node,
                    h,
                    ObjectType::DeviceResource,
                    Rights::CLAIM,
                )?;
                let next = d(s.device.device.can_claim(active.node.raw()))?;
                let source = c(lease.table().grant_id(&s.objects, active.node, h))?;
                let (mmio, irq) = c(s.objects.install_device_lease(lease.table_mut(), next))?;
                s.device.scrub = 0;
                s.device.handles[..3].copy_from_slice(&[h.raw(), mmio.raw(), irq.raw()]);
                s.device.grants[..3].copy_from_slice(&[
                    source,
                    c(lease.table().grant_id(&s.objects, active.node, mmio))?,
                    c(lease.table().grant_id(&s.objects, active.node, irq))?,
                ]);
                assert_eq!(d(s.device.device.claim(active.node.raw(), now))?, next);
                mmio.raw()
            }
            op::DEVICE_INTERRUPT => {
                let generation = token(
                    lease.table(),
                    &mut s.objects,
                    active.node,
                    h,
                    ObjectType::MmioRegion,
                    Rights::READ,
                )?;
                d(s.device.device.authorize(active.node.raw(), generation))?;
                // Return the already published original handle; do not allocate
                // new authority or grant ACK through a read-only MMIO child.
                if h.raw() != s.device.handles[1] {
                    return Err(cap::Error::Rights as i64);
                }
                s.device.handles[2]
            }
            op::MMIO_READ | op::MMIO_WRITE => {
                let write = number == op::MMIO_WRITE;
                let generation = token(
                    lease.table(),
                    &mut s.objects,
                    active.node,
                    h,
                    ObjectType::MmioRegion,
                    if write { Rights::WRITE } else { Rights::READ },
                )?;
                let trigger = match s.device.device.check_io(
                    active.node.raw(),
                    generation,
                    args[1],
                    args[2],
                    write.then_some(args[3]),
                ) {
                    Ok(value) => value,
                    Err(error) => {
                        if error == protocol::Error::Capacity {
                            quarantine_reason = Some(reason::BUDGET);
                        }
                        return Err(error as i64);
                    }
                };
                if trigger == 1 && reg(0x20) != 0x80 {
                    return Err(protocol::Error::NotReady as i64);
                }
                let result = if write {
                    put(args[1], args[3] as u32);
                    0
                } else {
                    reg(args[1]) as u64
                };
                s.device.device.commit_io(write, trigger, now);
                result
            }
            op::INTERRUPT_WAIT => {
                let generation = token(
                    lease.table(),
                    &mut s.objects,
                    active.node,
                    h,
                    ObjectType::Interrupt,
                    Rights::WAIT,
                )?;
                let grant = c(lease.table().grant_id(&s.objects, active.node, h))?;
                match d(s.device.device.wait(
                    active.node.raw(),
                    generation,
                    active.thread.raw(),
                    now,
                ))? {
                    Some(sequence) => sequence,
                    None => {
                        s.device.handles[3] = h.raw();
                        s.device.grants[3] = grant;
                        s.device.wait = Some(Pending {
                            active,
                            frame: f,
                            cpu,
                            entry: neu_smp_sched_cpus[cpu].get(field::ENTRIES),
                        });
                        return Ok(Outcome::Blocked);
                    }
                }
            }
            op::INTERRUPT_ACK => {
                let generation = token(
                    lease.table(),
                    &mut s.objects,
                    active.node,
                    h,
                    ObjectType::Interrupt,
                    Rights::ACKNOWLEDGE,
                )?;
                let status = d(s.device.device.check_ack(
                    active.node.raw(),
                    generation,
                    active.thread.raw(),
                    args[1],
                ))?;
                if reg(0x24) as u64 != status || reg(0x20) & 1 != 0 {
                    quarantine_reason = Some(reason::HARDWARE);
                    return Err(-46);
                }
                put(0x64, status as u32);
                if reg(0x24) != 0 {
                    quarantine_reason = Some(reason::HARDWARE);
                    return Err(-46);
                }
                s.device.device.acknowledge(now);
                0
            }
            _ => return Err(abi::NOT_SUPPORTED),
        };
        Ok(Outcome::Return { status: 0, scalar })
    })();
    s.nodes
        .restore_authority_on(cpu, lease)
        .expect("K11 capability lease restoration");
    if let Some(why) = quarantine_reason {
        quarantine(s, why, true);
    }
    let mut outcome = result.unwrap_or_else(|status| {
        s.device.device.rejected += 1;
        Outcome::Return { status, scalar: 0 }
    });
    if let Some(ticket) = revoke {
        if s.device.grants.iter().any(|grant| {
            *grant != cap::GrantId::NONE
                && s.objects
                    .revoke_selects(&ticket, *grant)
                    .expect("K11 revoke selection")
        }) {
            quarantine(s, reason::REVOKE, true);
        }
        s.objects
            .purge_revoked_operations()
            .expect("K11 revoked operation sweep");
        s.nodes
            .purge_revoked_authority(&mut s.objects)
            .expect("K11 revoked table sweep");
        let count = s
            .objects
            .finish_revoke(&ticket)
            .expect("K11 synchronous revoke completion");
        outcome = Outcome::Return {
            status: 0,
            scalar: count as u64,
        };
    }
    // A claim or ACK on an AP may arrive after the BSP has gone idle with
    // no device timer armed. Publish service work through the normal wake/IPI
    // protocol, including its generation check against the STI/HLT boundary.
    if matches!(s.device.device.phase, phase::REARM | phase::QUARANTINE) {
        s.device.wakes |= 1;
    }
    outcome
}
pub(super) fn tick(cpu: usize) {
    if cpu == 0 {
        TICKS.fetch_add(1, Ordering::Release);
    }
}
pub(super) fn irq(f: ExceptionFrame, address: u64) {
    let cpu = current();
    if cpu != 0 || !bitmap(0x100, protocol::VECTOR) {
        reject(cpu, 70);
    }
    let before = route();
    mask();
    let status = reg(0x24) as u64;
    if EVENT[0].load(Ordering::Acquire) != 0 {
        reject(cpu, 71);
    }
    let epoch = EPOCH.load(Ordering::Acquire);
    if epoch == 0 {
        reject(cpu, 72);
    }
    EVENT[1].store(status, Ordering::Relaxed);
    EVENT[0].store(epoch, Ordering::Release);
    let local = &neu_smp_sched_cpus[cpu];
    local.inc(56);
    write(0xb0, 0);
    if bitmap(0x100, protocol::VECTOR) || route() & (MASK | 0x4000) != MASK {
        reject(cpu, 73);
    }
    local.inc(57);
    local.words[field::GENERATION].fetch_add(1, Ordering::Release);
    if f.cs == abi::USER_CS {
        trace(cpu, f, 0, address, 1, 0);
        local.inc(58);
    }
    {
        let index = local.get(56) as usize - 1;
        if index >= neu_device_irq.len() {
            reject(cpu, 74);
        }
        let mut row = [0; 40];
        row[..10].copy_from_slice(&[
            index as u64 + 1,
            cpu as u64,
            epoch,
            status,
            before as u64,
            route() as u64,
            1,
            0,
            address,
            paging::current_root(),
        ]);
        let normalized = unsafe { core::slice::from_raw_parts(address as *const u64, 22) };
        row[10..32].copy_from_slice(normalized);
        publish(&neu_device_irq[index], row);
        neu_device_layout[46].store(index as u64 + 1, Ordering::Release);
        barrier_word(20).store(index as u64 + 1, Ordering::Release);
        neu_device_checkpoint.store(10, Ordering::Release);
        probe_wait();
        neu_device_checkpoint.store(0, Ordering::Release);
    }
    if f.cs == abi::USER_CS {
        stage(cpu, f, 0);
    }
}
pub(super) fn service() {
    if bitmap(0x200, protocol::VECTOR) {
        unsafe {
            neu_smp_sched_drain();
        }
    }
    let (wakes, checkpoint) = with_state(0, |s| {
        let mut scrub_probe = false;
        let now = TICKS.load(Ordering::Acquire);
        let epoch = EVENT[0].swap(0, Ordering::AcqRel);
        if epoch != 0 {
            let status = EVENT[1].swap(0, Ordering::AcqRel);
            match s.device.device.interrupt(epoch, status, now) {
                Ok(Some((thread, sequence))) => settle(s, thread, 0, sequence, true),
                Ok(None) => (),
                Err(_) => quarantine(s, reason::HARDWARE, true),
            }
            event(s, 3, epoch, status);
        }
        if s.device.device.expired(now) {
            quarantine(s, reason::TIMEOUT, true);
        }
        if s.device.device.phase == phase::REARM {
            if reg(0x24) != 0
                || reg(0x20) & 1 != 0
                || route() & 0x4000 != 0
                || bitmap(0x200, protocol::VECTOR)
                || bitmap(0x100, protocol::VECTOR)
            {
                quarantine(s, reason::HARDWARE, true);
            } else {
                EPOCH.store(s.device.device.generation, Ordering::Release);
                io_write(0x10 + GSI * 2, ROUTE);
                s.device
                    .device
                    .armed(s.device.device.generation)
                    .expect("K11 arm transaction");
                event(s, 4, s.device.device.generation, 0);
            }
        }
        if s.device.device.phase == phase::QUARANTINE {
            mask();
            // Read completion before controller state: a completion may have
            // queued an IRQ after the drain at service entry. Preserve its
            // status until that event has actually been consumed.
            let computing = reg(0x20) & 1 != 0;
            let status = reg(0x24) as u64;
            let pending = bitmap(0x200, protocol::VECTOR)
                || bitmap(0x100, protocol::VECTOR)
                || route() & 0x4000 != 0
                || EVENT[0].load(Ordering::Acquire) != 0;
            if !pending {
                s.device.device.observe_completion(computing, status);
            }
            if s.device.device.flight == 0 && !pending {
                if s.device.scrub == 0 {
                    // The neutral command also clears COMPUTING before
                    // checking IRQFACT. Require its actual completion raise
                    // while the route is masked, before a new owner can enable
                    // factorial interrupts. Flag polling alone is insufficient.
                    put(0x64, u32::MAX);
                    put(4, u32::MAX);
                    put(0x20, 0x80);
                    put(8, 0);
                    s.device.scrub = 1;
                } else if s.device.scrub == 1 && !computing && status == 1 && reg(8) == 1 {
                    s.device.scrub = 2;
                    event(s, 7, s.device.device.generation, status);
                    scrub_probe = true;
                } else if s.device.scrub == 2 {
                    put(0x20, 0);
                    put(0x64, u32::MAX);
                    s.device.scrub = 3;
                }
                if s.device.scrub == 3
                    && s.device
                        .device
                        .release(
                            reg(0x20) & 1 != 0 || reg(4) != 0 || reg(8) != 1,
                            reg(0x24) as u64,
                            route() & MASK == 0,
                            bitmap(0x200, protocol::VECTOR)
                                || bitmap(0x100, protocol::VECTOR)
                                || route() & 0x4000 != 0,
                            EVENT[0].load(Ordering::Acquire) != 0,
                        )
                        .is_ok()
                {
                    EPOCH.store(0, Ordering::Release);
                    s.nodes
                        .retire_device_authority(&mut s.objects, s.device.device.generation)
                        .expect("K11 lease retirement");
                    s.device.handles = [0; 4];
                    s.device.grants = [cap::GrantId::NONE; 4];
                    event(s, 6, s.device.device.generation, 0);
                }
            }
        }
        let d = &s.device.device;
        let checkpoint = if scrub_probe {
            4
        } else if d.generation == 1 && d.waiter != 0 && d.blocked > s.device.wait_checkpoints {
            s.device.wait_checkpoints = d.blocked;
            1
        } else if d.phase == phase::PENDING
            && d.generation == 1
            && d.receiver == 0
            && d.sequence == 2
            && d.status == 1
            && !s.device.pending_checkpoint
        {
            s.device.pending_checkpoint = true;
            2
        } else if d.releases == 1 && !s.device.release_checkpoint {
            s.device.release_checkpoint = true;
            3
        } else {
            0
        };
        (take_wakes(s), checkpoint)
    });
    notify(0, wakes);
    if checkpoint != 0 {
        neu_device_checkpoint.store(checkpoint, Ordering::Release);
        probe_wait();
        neu_device_checkpoint.store(0, Ordering::Release);
        match checkpoint {
            1 => {
                barrier_word(11).fetch_add(1, Ordering::Release);
            }
            2 => barrier_word(13).store(1, Ordering::Release),
            3 => barrier_word(19).store(1, Ordering::Release),
            4 => (),
            _ => unreachable!(),
        }
    }
}
pub(super) fn idle_timer(cpu: usize, enabled: bool) {
    if cpu == 0 && enabled {
        write(0x320, TIMER as u32);
        write(0x380, INITIAL.load(Ordering::Acquire) as u32);
    } else if cpu == 0 {
        write(0x320, TIMER as u32 | MASK);
        write(0x380, 0);
    }
}
pub(super) fn cleanup(paging: &mut Paging<'_, '_>) -> Result<(), &'static str> {
    let clean = with_state(0, |s| {
        s.device.device.phase == phase::FREE
            && s.device.wait.is_none()
            && s.device.handles == [0; 4]
            && s.objects.live_grants() == 0
    });
    if !clean
        || EVENT[0].load(Ordering::Acquire) != 0
        || reg(0x20) != 0
        || reg(0x24) != 0
        || reg(4) != 0
        || reg(8) != 1
        || reg(0x80) != 0
        || reg(0x88) != 0
        || reg(0x90) != 0
        || reg(0x98) != 0
        || route() & (MASK | 0x4000) != MASK
    {
        return Err("K11 residual device/DMA/IRQ state");
    }
    pci16(0x28, 4, (pci(0x28, 4) as u16 & !7) | 0x400);
    let msi = neu_device_layout[16].load(Ordering::Acquire) as u32;
    if pci(0x28, 4) & 0x407 != 0x400 || pci(0x28, msi) & 0x10000 != 0 {
        return Err("K11 final PCI authority");
    }
    io_write(
        0x10 + GSI * 2,
        neu_device_layout[41].load(Ordering::Acquire) as u32,
    );
    io_write(
        0x11 + GSI * 2,
        neu_device_layout[42].load(Ordering::Acquire) as u32,
    );
    paging
        .unmap_edu()
        .map_err(|_| "K11 peripheral map cleanup")?;
    neu_device_layout[44].store(1, Ordering::Release);
    Ok(())
}

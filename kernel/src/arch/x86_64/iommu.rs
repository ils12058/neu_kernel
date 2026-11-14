//! Intel VT-d backend. Boot owns discovery; runtime map/unmap transactions are
//! serialized by the scheduler lock with IF=0. Tables reside in reserved,
//! page-aligned kernel storage and outlive every DMA generation.
use super::{
    paging::{self, Paging},
    pci,
};
use core::{
    arch::{asm, x86_64::__cpuid},
    cell::UnsafeCell,
    ptr,
    sync::atomic::{AtomicBool, AtomicU64, Ordering, fence},
};
use neu_kernel::{
    acpi::MAX_UNITS,
    iommu::{self as logic, Capabilities},
    pci::Bdf,
};
pub const VIEW: u64 = paging::LAPIC_VIEW + 0x4000;
const DOMAINS: usize = 8;
const CONTEXTS: usize = 8;
const POLLS: usize = 100_000;
#[repr(C, align(4096))]
pub struct Page(pub [AtomicU64; 512]);
impl Page {
    pub const fn new() -> Self {
        Self([const { AtomicU64::new(0) }; 512])
    }
    fn zero(&self) {
        for word in &self.0 {
            word.store(0, Ordering::Release);
        }
    }
    /// Initialization verifies CLFLUSH and 64-byte cache lines. Table updates
    /// are visible even when ECAP does not advertise coherent remapping walks.
    pub fn sync(&self) {
        for line in self.0.chunks_exact(8) {
            unsafe {
                asm!("clflush [{}]", in(reg) line.as_ptr(), options(nostack, preserves_flags));
            }
        }
        fence(Ordering::SeqCst);
    }
}
#[unsafe(no_mangle)]
static neu_iommu_roots: [Page; MAX_UNITS] = [const { Page::new() }; MAX_UNITS];
#[unsafe(no_mangle)]
static neu_iommu_contexts: [Page; CONTEXTS] = [const { Page::new() }; CONTEXTS];
#[unsafe(no_mangle)]
static neu_iommu_pages: [[Page; 4]; DOMAINS] = [const { [const { Page::new() }; 4] }; DOMAINS];
static ROOT_PA: [AtomicU64; MAX_UNITS] = [const { AtomicU64::new(0) }; MAX_UNITS];
static CONTEXT_PA: [AtomicU64; CONTEXTS] = [const { AtomicU64::new(0) }; CONTEXTS];
static PAGE_PA: [[AtomicU64; 4]; DOMAINS] = [const { [const { AtomicU64::new(0) }; 4] }; DOMAINS];
static CONTEXT_KEYS: [AtomicU64; CONTEXTS] = [const { AtomicU64::new(0) }; CONTEXTS];
static DOMAIN_KEYS: [AtomicU64; DOMAINS] = [const { AtomicU64::new(0) }; DOMAINS];
static READY: AtomicBool = AtomicBool::new(false);
struct Caps(UnsafeCell<[Capabilities; MAX_UNITS]>);
// SAFETY: The BSP initializes capabilities before READY's release publication.
unsafe impl Sync for Caps {}
static CAPS: Caps = Caps(UnsafeCell::new(
    [Capabilities {
        physical_bits: 0,
        levels: 0,
        domain_bits: 0,
        iotlb: 0,
        fault: 0,
        faults: 0,
        drain: 0,
        write_buffer_flush: false,
    }; MAX_UNITS],
));
#[unsafe(no_mangle)]
pub static neu_iommu_layout: [AtomicU64; 32] = [const { AtomicU64::new(0) }; 32];
#[unsafe(no_mangle)]
pub static neu_iommu_units: [[AtomicU64; 16]; MAX_UNITS] =
    [const { [const { AtomicU64::new(0) }; 16] }; MAX_UNITS];
#[unsafe(no_mangle)]
pub static neu_iommu_invalidations: [[AtomicU64; 8]; 128] =
    [const { [const { AtomicU64::new(0) }; 8] }; 128];
static INVALIDATIONS: AtomicU64 = AtomicU64::new(0);
fn irq_disabled() {
    let flags: u64;
    unsafe {
        asm!("pushfq", "pop {}", out(reg) flags, options(preserves_flags));
    }
    assert_eq!(flags & 0x200, 0);
}
pub fn count() -> usize {
    crate::boot::pci_firmware().unit_count
}
pub fn physical(unit: usize) -> u64 {
    crate::boot::pci_firmware().units[unit].base
}
fn read32(unit: usize, offset: u16) -> u32 {
    unsafe { ptr::read_volatile((VIEW + unit as u64 * 4096 + u64::from(offset)) as *const u32) }
}
fn read64(unit: usize, offset: u16) -> u64 {
    unsafe { ptr::read_volatile((VIEW + unit as u64 * 4096 + u64::from(offset)) as *const u64) }
}
fn write32(unit: usize, offset: u16, value: u32) {
    unsafe {
        ptr::write_volatile(
            (VIEW + unit as u64 * 4096 + u64::from(offset)) as *mut u32,
            value,
        );
    }
}
fn write64(unit: usize, offset: u16, value: u64) {
    unsafe {
        ptr::write_volatile(
            (VIEW + unit as u64 * 4096 + u64::from(offset)) as *mut u64,
            value,
        );
    }
}
fn capabilities(unit: usize) -> Capabilities {
    assert!(READY.load(Ordering::Acquire));
    unsafe { (*CAPS.0.get())[unit] }
}
pub fn page_physical(paging: &Paging<'_, '_>, page: &Page) -> Result<u64, &'static str> {
    let address = page as *const Page as u64;
    let view = paging
        .translate(address)
        .map_err(|_| "IOMMU table translation")?
        .ok_or("IOMMU table not mapped")?;
    if address & 4095 != 0
        || view.physical & 4095 != 0
        || view.user
        || !view.permissions.write
        || view.permissions.execute
    {
        return Err("IOMMU table ownership/permissions");
    }
    Ok(view.physical)
}
fn wait32(unit: usize, offset: u16, mask: u32, expected: u32) -> Result<u32, &'static str> {
    for _ in 0..POLLS {
        let value = read32(unit, offset);
        if value & mask == expected {
            return Ok(value);
        }
        core::hint::spin_loop();
    }
    Err("IOMMU command did not complete")
}
fn wait64(unit: usize, offset: u16, mask: u64, expected: u64) -> Result<u64, &'static str> {
    for _ in 0..POLLS {
        let value = read64(unit, offset);
        if value & mask == expected {
            return Ok(value);
        }
        core::hint::spin_loop();
    }
    Err("IOMMU invalidation did not complete")
}
pub fn initialize(paging: &mut Paging<'_, '_>, pat: u64) -> Result<(), &'static str> {
    irq_disabled();
    assert!(super::smp::is_bsp());
    if count() == 0 || count() > MAX_UNITS {
        return Err("DMA unavailable: no validated IOMMU");
    }
    if READY.load(Ordering::Acquire) {
        return Err("IOMMU already initialized");
    }
    let cpu = __cpuid(1);
    if cpu.edx & (1 << 19) == 0 || (cpu.ebx >> 8) & 255 != 8 {
        return Err("IOMMU requires 64-byte CLFLUSH visibility");
    }
    let cpu_bits = (__cpuid(0x80000008).eax & 255) as u8;
    let host_bits = cpu_bits.min(crate::boot::pci_firmware().host_address_bits);
    for unit in 0..count() {
        paging
            .map_platform_registers(VIEW + unit as u64 * 4096, physical(unit), pat)
            .map_err(|_| "IOMMU UC register mapping")?;
        let version = read32(unit, 0);
        let cap = read64(unit, 8);
        let ecap = read64(unit, 0x10);
        let c = Capabilities::parse(version, cap, ecap, host_bits)
            .map_err(|_| "unsupported IOMMU capabilities")?;
        if read32(unit, 0x1c) & 0x86000000 != 0 {
            return Err("IOMMU has unknown active translation/queue/remapping");
        }
        write32(unit, 0x38, 1 << 31); // Mask fault MSI; polling preserves source records.
        for index in 0..c.faults {
            write64(unit, c.fault + index * 16 + 8, 1 << 63);
        }
        write32(unit, 0x34, 0x11);
        neu_iommu_roots[unit].zero();
        let root = page_physical(paging, &neu_iommu_roots[unit])?;
        logic::address(root, c.physical_bits).map_err(|_| "IOMMU root physical range")?;
        ROOT_PA[unit].store(root, Ordering::Release);
        unsafe {
            (*CAPS.0.get())[unit] = c;
        }
        for (cell, value) in neu_iommu_units[unit].iter().zip([
            physical(unit),
            VIEW + unit as u64 * 4096,
            u64::from(version),
            cap,
            ecap,
            root,
            u64::from(c.iotlb),
            u64::from(c.fault),
            u64::from(c.faults),
            u64::from(c.levels),
            u64::from(c.physical_bits),
            u64::from(c.domain_bits),
            0,
            0,
            0,
            0,
        ]) {
            cell.store(value, Ordering::Release);
        }
    }
    for (index, page) in neu_iommu_contexts.iter().enumerate() {
        page.zero();
        CONTEXT_PA[index].store(page_physical(paging, page)?, Ordering::Release);
    }
    for (index, pages) in neu_iommu_pages.iter().enumerate() {
        for (level, page) in pages.iter().enumerate() {
            page.zero();
            PAGE_PA[index][level].store(page_physical(paging, page)?, Ordering::Release);
        }
    }
    READY.store(true, Ordering::Release);
    for unit in 0..count() {
        write64(unit, 0x20, ROOT_PA[unit].load(Ordering::Acquire));
        neu_iommu_roots[unit].sync();
        write32(unit, 0x18, logic::SET_ROOT);
        wait32(unit, 0x1c, logic::SET_ROOT, logic::SET_ROOT)?;
        invalidate(unit, 0, 0)?;
        write32(unit, 0x18, logic::TRANSLATION_ENABLE);
        wait32(
            unit,
            0x1c,
            logic::TRANSLATION_ENABLE,
            logic::TRANSLATION_ENABLE,
        )?;
        neu_iommu_units[unit][12].store(u64::from(read32(unit, 0x1c)), Ordering::Release);
    }
    for (cell, value) in neu_iommu_layout.iter().zip([
        1,
        count() as u64,
        neu_iommu_units.as_ptr() as u64,
        16,
        neu_iommu_roots.as_ptr() as u64,
        neu_iommu_contexts.as_ptr() as u64,
        neu_iommu_pages.as_ptr() as u64,
        CONTEXT_KEYS.as_ptr() as u64,
        DOMAIN_KEYS.as_ptr() as u64,
        DOMAINS as u64,
        CONTEXTS as u64,
        neu_iommu_invalidations.as_ptr() as u64,
        &INVALIDATIONS as *const AtomicU64 as u64,
        128,
        8,
        0,
    ]) {
        cell.store(value, Ordering::Release);
    }
    Ok(())
}
pub fn invalidate(unit: usize, domain: usize, epoch: u64) -> Result<(), &'static str> {
    irq_disabled();
    let c = capabilities(unit);
    let index = INVALIDATIONS.load(Ordering::Acquire) as usize;
    if index >= neu_iommu_invalidations.len() {
        return Err("IOMMU invalidation evidence capacity");
    }
    for page in &neu_iommu_roots {
        page.sync();
    }
    for page in &neu_iommu_contexts {
        page.sync();
    }
    for pages in &neu_iommu_pages {
        for page in pages {
            page.sync();
        }
    }
    fence(Ordering::SeqCst);
    if c.write_buffer_flush {
        write32(
            unit,
            0x18,
            (read32(unit, 0x1c) & logic::TRANSLATION_ENABLE) | (1 << 27),
        );
        wait32(unit, 0x1c, 1 << 27, 0)?;
    }
    write64(unit, 0x28, logic::CONTEXT_INVALIDATE);
    let context = wait64(unit, 0x28, (1 << 63) | (3 << 59), logic::CONTEXT_COMPLETED)?;
    write64(unit, c.iotlb, logic::IOTLB_INVALIDATE | c.drain);
    let iotlb = wait64(unit, c.iotlb, (1 << 63) | (3 << 57), logic::IOTLB_COMPLETED)?;
    fence(Ordering::SeqCst);
    for (cell, value) in neu_iommu_invalidations[index].iter().zip([
        index as u64 + 1,
        unit as u64,
        domain as u64,
        epoch,
        logic::CONTEXT_INVALIDATE,
        context,
        logic::IOTLB_INVALIDATE | c.drain,
        iotlb,
    ]) {
        cell.store(value, Ordering::Relaxed);
    }
    INVALIDATIONS.store(index as u64 + 1, Ordering::Release);
    Ok(())
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Domain {
    pub slot: usize,
    pub unit: usize,
    pub context: usize,
    pub requester: u16,
}
impl Domain {
    pub fn attach(bdf: Bdf) -> Result<Self, &'static str> {
        irq_disabled();
        let fw = crate::boot::pci_firmware();
        if fw
            .requires_reserved_memory(pci::inventory(), bdf)
            .map_err(|_| "IOMMU reserved scope")?
        {
            return Err("DMA device requires unsupported reserved memory");
        }
        let unit = fw
            .unit_for(pci::inventory(), bdf)
            .map_err(|_| "IOMMU requester ownership")?;
        let c = capabilities(unit);
        let key = (unit as u64 + 1) << 32 | bdf.key();
        if DOMAIN_KEYS
            .iter()
            .any(|cell| cell.load(Ordering::Acquire) == key)
        {
            return Err("IOMMU duplicate domain");
        }
        let slot = DOMAIN_KEYS
            .iter()
            .position(|cell| cell.load(Ordering::Acquire) == 0)
            .ok_or("IOMMU domain capacity")?;
        let context_key = (unit as u64 + 1) << 8 | u64::from(bdf.bus);
        let context = CONTEXT_KEYS
            .iter()
            .position(|cell| cell.load(Ordering::Acquire) == context_key)
            .or_else(|| {
                CONTEXT_KEYS
                    .iter()
                    .position(|cell| cell.load(Ordering::Acquire) == 0)
            })
            .ok_or("IOMMU context capacity")?;
        let root = PAGE_PA[slot][0].load(Ordering::Acquire);
        let entry = c
            .context(root, slot as u16 + 1)
            .map_err(|_| "IOMMU domain encoding")?;
        let r = logic::root_entry(CONTEXT_PA[context].load(Ordering::Acquire), c.physical_bits)
            .map_err(|_| "IOMMU root encoding")?;
        for level in 0..usize::from(c.levels) - 1 {
            neu_iommu_pages[slot][level].0[0].store(
                logic::entry(
                    PAGE_PA[slot][level + 1].load(Ordering::Acquire),
                    3,
                    c.physical_bits,
                )
                .map_err(|_| "IOMMU branch encoding")?,
                Ordering::Release,
            );
        }
        neu_iommu_contexts[context].0[bdf.devfn as usize * 2 + 1]
            .store(entry[1], Ordering::Release);
        neu_iommu_contexts[context].0[bdf.devfn as usize * 2].store(entry[0], Ordering::Release);
        neu_iommu_roots[unit].0[bdf.bus as usize * 2 + 1].store(r[1], Ordering::Release);
        neu_iommu_roots[unit].0[bdf.bus as usize * 2].store(r[0], Ordering::Release);
        CONTEXT_KEYS[context].store(context_key, Ordering::Release);
        DOMAIN_KEYS[slot].store(key, Ordering::Release);
        invalidate(unit, slot + 1, 0)?;
        Ok(Self {
            slot,
            unit,
            context,
            requester: bdf.requester(),
        })
    }
    /// Only manager-owned reserved backing reaches this function. Public
    /// callers supply typed buffer handles, never these physical addresses.
    pub fn map(
        self,
        iova: u64,
        physical: &[u64],
        rights: u8,
        epoch: u64,
    ) -> Result<(), &'static str> {
        irq_disabled();
        let c = capabilities(self.unit);
        let offset = ((iova >> 12) & 511) as usize;
        if iova >= 1 << 21
            || iova & 4095 != 0
            || physical.is_empty()
            || offset + physical.len() > 512
        {
            return Err("IOMMU mapping range");
        }
        for (index, address) in physical.iter().enumerate() {
            logic::entry(*address, rights, c.physical_bits).map_err(|_| "IOMMU DMA backing")?;
            if neu_iommu_pages[self.slot][usize::from(c.levels) - 1].0[offset + index]
                .load(Ordering::Acquire)
                != 0
            {
                return Err("IOMMU overlapping mapping");
            }
        }
        for (index, address) in physical.iter().enumerate() {
            neu_iommu_pages[self.slot][usize::from(c.levels) - 1].0[offset + index].store(
                logic::entry(*address, rights, c.physical_bits).unwrap(),
                Ordering::Release,
            );
        }
        invalidate(self.unit, self.slot + 1, epoch)
    }
    pub fn unmap(self, iova: u64, pages: usize, epoch: u64) -> Result<(), &'static str> {
        irq_disabled();
        let c = capabilities(self.unit);
        let offset = (iova >> 12) as usize;
        if iova & 4095 != 0 || pages == 0 || offset.checked_add(pages).is_none_or(|end| end > 512) {
            return Err("IOMMU unmap range");
        }
        for index in offset..offset + pages {
            neu_iommu_pages[self.slot][usize::from(c.levels) - 1].0[index]
                .store(0, Ordering::Release);
        }
        invalidate(self.unit, self.slot + 1, epoch)
    }
    pub fn detach(self) -> Result<(), &'static str> {
        let devfn = self.requester as u8;
        neu_iommu_contexts[self.context].0[devfn as usize * 2].store(0, Ordering::Release);
        invalidate(self.unit, self.slot + 1, 0)?;
        for page in &neu_iommu_pages[self.slot] {
            page.zero();
        }
        neu_iommu_contexts[self.context].0[devfn as usize * 2 + 1].store(0, Ordering::Release);
        DOMAIN_KEYS[self.slot].store(0, Ordering::Release);
        Ok(())
    }
}
pub fn pending_fault(unit: usize) -> Result<Option<logic::Fault>, &'static str> {
    let c = capabilities(unit);
    if read32(unit, 0x34) & 0x11 != 0 {
        return Err("IOMMU fault overflow/queue failure");
    }
    for index in 0..c.faults {
        let low = read64(unit, c.fault + index * 16);
        let high = read64(unit, c.fault + index * 16 + 8);
        if high & (1 << 63) != 0 {
            return logic::fault(low, high)
                .map(Some)
                .map_err(|_| "invalid IOMMU fault record");
        }
    }
    Ok(None)
}
pub fn clear_faults(unit: usize) -> Result<(), &'static str> {
    let c = capabilities(unit);
    for index in 0..c.faults {
        write64(unit, c.fault + index * 16 + 8, 1 << 63);
    }
    write32(unit, 0x34, 0x11);
    if read32(unit, 0x34) & 0x13 != 0 {
        return Err("IOMMU fault did not retire");
    }
    Ok(())
}
pub fn cleanup(paging: &mut Paging<'_, '_>) -> Result<(), &'static str> {
    irq_disabled();
    assert!(super::smp::is_bsp());
    if DOMAIN_KEYS.iter().any(|v| v.load(Ordering::Acquire) != 0) {
        return Err("live DMA domain at cleanup");
    }
    for (unit, root) in neu_iommu_roots.iter().enumerate().take(count()) {
        root.zero();
        invalidate(unit, 0, 0)?;
        if read32(unit, 0x1c) & logic::TRANSLATION_ENABLE == 0 || pending_fault(unit)?.is_some() {
            return Err("IOMMU final isolation state");
        }
        paging
            .unmap_platform_registers(VIEW + unit as u64 * 4096)
            .map_err(|_| "IOMMU register cleanup")?;
    }
    for (i, context) in neu_iommu_contexts.iter().enumerate() {
        context.zero();
        CONTEXT_KEYS[i].store(0, Ordering::Release);
    }
    // Translation stays enabled against reserved, empty root tables. No DMA
    // address becomes identity mapped when the diagnostic session finishes.
    neu_iommu_layout[15].store(1, Ordering::Release);
    Ok(())
}

//! BSP-owned PCI configuration transport and Q35 resource selection. The
//! inventory/parser is independent of this platform's interrupt-routing policy.
use super::paging::{self, Paging};
use core::{
    arch::asm,
    cell::UnsafeCell,
    ptr,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};
use neu_kernel::pci::{self as logic, Bar, BarKind, Bdf, Config, Function, Inventory};
const VIEW: u64 = paging::LAPIC_VIEW + 0x10000;
struct Transport<'a, 'b, 'c> {
    paging: &'a mut Paging<'b, 'c>,
    pat: u64,
    mapped: Option<u64>,
}
fn bsp() {
    let flags: u64;
    unsafe {
        asm!("pushfq", "pop {}", out(reg) flags, options(preserves_flags));
    }
    assert!(super::smp::is_bsp() && flags & 0x200 == 0);
}
fn select(bdf: Bdf, offset: u16) {
    bsp();
    assert_eq!(bdf.segment, 0);
    assert!(offset < 256);
    unsafe {
        asm!("out dx, eax", in("dx") 0xcf8u16, in("eax") 0x80000000u32 | (u32::from(bdf.requester()) << 8) | u32::from(offset & !3), options(nostack, preserves_flags));
    }
}
pub fn read(bdf: Bdf, offset: u16) -> u32 {
    select(bdf, offset);
    let value;
    unsafe {
        asm!("in eax, dx", in("dx") 0xcfcu16, out("eax") value, options(nostack, preserves_flags));
    }
    value
}
pub fn command(bdf: Bdf, value: u16) {
    write16(bdf, 4, value);
}
pub fn write16(bdf: Bdf, offset: u16, value: u16) {
    select(bdf, offset);
    unsafe {
        asm!("out dx, ax", in("dx") (0xcfcu16 + (offset & 2)), in("ax") value, options(nostack, preserves_flags));
    }
}
impl Transport<'_, '_, '_> {
    fn view(&mut self, bdf: Bdf, offset: u16) -> logic::Result<u64> {
        bsp();
        let fw = crate::boot::pci_firmware();
        let region = fw.ecam[..fw.ecam_count]
            .iter()
            .find(|r| r.segment == bdf.segment && (r.first..=r.last).contains(&bdf.bus))
            .ok_or(logic::Error::Range)?;
        let address = region.address(bdf, offset, 4)?;
        let page = address & !4095;
        if self.mapped != Some(page) {
            self.finish()?;
            self.paging
                .map_platform_registers(VIEW, page, self.pat)
                .map_err(|_| logic::Error::Access)?;
            self.mapped = Some(page);
        }
        Ok(VIEW + (address & 4095))
    }
    fn finish(&mut self) -> logic::Result<()> {
        if self.mapped.take().is_some() {
            self.paging
                .unmap_platform_registers(VIEW)
                .map_err(|_| logic::Error::Access)?;
        }
        Ok(())
    }
}
impl Config for Transport<'_, '_, '_> {
    fn read32(&mut self, bdf: Bdf, offset: u16) -> logic::Result<u32> {
        if offset & 3 != 0 || offset >= 4096 {
            return Err(logic::Error::Range);
        }
        if bdf.segment == 0 && offset < 256 {
            Ok(read(bdf, offset))
        } else {
            let p = self.view(bdf, offset)?;
            Ok(unsafe { ptr::read_volatile(p as *const u32) })
        }
    }
    fn write32(&mut self, bdf: Bdf, offset: u16, value: u32) -> logic::Result<()> {
        if offset & 3 != 0 || offset >= 4096 {
            return Err(logic::Error::Range);
        }
        if bdf.segment == 0 && offset < 256 {
            select(bdf, offset);
            unsafe {
                asm!("out dx, eax", in("dx") 0xcfcu16, in("eax") value, options(nostack, preserves_flags));
            }
        } else {
            let p = self.view(bdf, offset)?;
            unsafe {
                ptr::write_volatile(p as *mut u32, value);
            }
        }
        Ok(())
    }
    fn write16(&mut self, bdf: Bdf, offset: u16, value: u16) -> logic::Result<()> {
        if offset & 1 != 0 || offset >= 4096 {
            return Err(logic::Error::Range);
        }
        if bdf.segment == 0 && offset < 256 {
            write16(bdf, offset, value);
        } else {
            let p = self.view(bdf, offset & !3)? + u64::from(offset & 2);
            unsafe {
                ptr::write_volatile(p as *mut u16, value);
            }
        }
        Ok(())
    }
}
struct Cell(UnsafeCell<Inventory>);
// SAFETY: BSP initializes before userspace; the published inventory is immutable.
unsafe impl Sync for Cell {}
static INVENTORY: Cell = Cell(UnsafeCell::new(Inventory::new()));
static READY: AtomicBool = AtomicBool::new(false);
#[unsafe(no_mangle)]
pub static neu_pci_layout: [AtomicU64; 16] = [const { AtomicU64::new(0) }; 16];
pub fn inventory() -> &'static Inventory {
    assert!(READY.load(Ordering::Acquire));
    unsafe { &*INVENTORY.0.get() }
}
#[derive(Clone, Copy)]
pub struct Selection {
    pub function: Function,
    pub bar: Bar,
    pub gsi: u32,
    pub requester: Bdf,
}
pub fn discover(paging: &mut Paging<'_, '_>, pat: u64) -> Result<Selection, &'static str> {
    bsp();
    let fw = crate::boot::pci_firmware();
    let mut c = Transport {
        paging,
        pat,
        mapped: None,
    };
    let inventory_result = Inventory::enumerate(&mut c, &fw.ecam[..fw.ecam_count]);
    c.finish().map_err(|_| "PCI configuration-view cleanup")?;
    let inv = inventory_result.map_err(|_| "PCI inventory validation")?;
    if c.read32(Bdf::default(), 0) != Ok(0x29c08086)
        || c.read32(
            Bdf {
                devfn: 0xf8,
                ..Bdf::default()
            },
            0,
        ) != Ok(0x29188086)
    {
        return Err("PCI requires validated Q35/ICH9 routing");
    }
    // At kernel entry no userspace authority exists. Quarantine all discovered
    // requesters before establishing translation, including unassigned devices.
    for f in inv.functions() {
        c.write16(f.bdf, 4, (f.command & !4) | 0x400)
            .map_err(|_| "PCI bus-master quarantine")?;
        if f.msi != 0 {
            let control = (c
                .read32(f.bdf, u16::from(f.msi))
                .map_err(|_| "PCI MSI read")?
                >> 16) as u16;
            c.write16(f.bdf, u16::from(f.msi) + 2, control & !1)
                .map_err(|_| "PCI MSI disable")?;
        }
        if f.msix != 0 {
            let control = (c
                .read32(f.bdf, u16::from(f.msix))
                .map_err(|_| "PCI MSI-X read")?
                >> 16) as u16;
            c.write16(f.bdf, u16::from(f.msix) + 2, (control & !0x8000) | 0x4000)
                .map_err(|_| "PCI MSI-X disable")?;
        }
    }
    let selected = *inv
        .functions()
        .iter()
        .find(|f| f.id == neu_kernel::device::PCI_ID)
        .ok_or("PCI EDU test endpoint absent")?;
    if selected.bdf.segment != 0 || selected.header != 0 {
        return Err("unsupported Q35 EDU segment/header");
    }
    let bars = logic::probe_bars(&mut c, &selected).map_err(|_| "PCI BAR validation")?;
    c.finish().map_err(|_| "PCI BAR view cleanup")?;
    let bar = bars[0].ok_or("PCI EDU BAR absent")?;
    if bar.kind != BarKind::Memory32 || bar.size != neu_kernel::device::BAR_SIZE || bar.prefetch {
        return Err("PCI EDU register aperture");
    }
    // Firmware assigns resources, but assignment is not proof of exclusive
    // ownership. Probe other functions with decode disabled and restore their
    // quarantined command halfwords before comparing memory windows.
    for f in inv.functions().iter().filter(|f| f.bdf != selected.bdf) {
        let resources = logic::probe_bars(&mut c, f);
        c.write16(f.bdf, 4, (f.command & !4) | 0x400)
            .map_err(|_| "PCI resource command restoration")?;
        for other in resources
            .map_err(|_| "PCI resource inventory")?
            .iter()
            .flatten()
        {
            if bar.conflicts(*other).map_err(|_| "PCI resource extent")? {
                return Err("PCI selected BAR overlaps another function");
            }
        }
    }
    c.finish().map_err(|_| "PCI resource view cleanup")?;
    let (root, pin) = inv
        .root_interrupt(selected.bdf, selected.pin)
        .map_err(|_| "PCI interrupt topology")?;
    if root.bus != 0 || root.devfn >> 3 >= 25 {
        return Err("PCI unsupported root interrupt route");
    }
    let gsi = 20 + u32::from(((root.devfn >> 3) + pin - 1) % 4);
    // PCI-to-PCI bridges alias requester IDs. A shared alias cannot receive
    // per-function authority, so reject it until an isolation-group API exists.
    let mut child = selected.bdf;
    while let Some(bridge) = inv.parent(child) {
        if bridge.pcie == 0 {
            return Err("PCI DMA requester aliases a conventional bridge");
        }
        let pcie_type = (read(bridge.bdf, u16::from(bridge.pcie)) >> 20) & 15;
        if !matches!(pcie_type, 4..=6) {
            return Err("PCI DMA bridge needs requester isolation group");
        }
        command(bridge.bdf, read(bridge.bdf, 4) as u16 & !0x400);
        child = bridge.bdf;
    }
    if READY.load(Ordering::Acquire) {
        return Err("PCI inventory already initialized");
    }
    unsafe {
        ptr::write(INVENTORY.0.get(), inv);
    }
    READY.store(true, Ordering::Release);
    let inv = inventory();
    for (i, value) in [
        1,
        inv.functions().as_ptr() as u64,
        inv.functions().len() as u64,
        core::mem::size_of::<Function>() as u64,
        selected.bdf.key(),
        bar.base,
        bar.size,
        u64::from(gsi),
        fw.rsdp,
        fw.root,
        fw.mcfg,
        fw.dmar,
        fw.ecam.as_ptr() as u64,
        fw.ecam_count as u64,
        core::mem::size_of::<logic::Ecam>() as u64,
        selected.bdf.key(),
    ]
    .into_iter()
    .enumerate()
    {
        neu_pci_layout[i].store(value, Ordering::Release);
    }
    Ok(Selection {
        function: selected,
        bar,
        gsi,
        requester: selected.bdf,
    })
}
pub fn bus_master_path(mut bdf: Bdf, enabled: bool) {
    loop {
        let value = read(bdf, 4) as u16;
        command(bdf, if enabled { value | 4 } else { value & !4 });
        match inventory().parent(bdf) {
            Some(bridge) => bdf = bridge.bdf,
            None => break,
        }
    }
}
/// Trusted isolation adversary only: an independently quarantined second EDU.
pub fn extra_edu(
    paging: &mut Paging<'_, '_>,
    pat: u64,
    selected: Bdf,
) -> Result<(Bdf, Bar), &'static str> {
    let f = *inventory()
        .functions()
        .iter()
        .find(|f| f.id == neu_kernel::device::PCI_ID && f.bdf != selected)
        .ok_or("PCI requester-isolation adversary absent")?;
    if f.bdf.segment != 0 || f.bdf.bus != 0 {
        return Err("PCI adversary placement");
    }
    let mut c = Transport {
        paging,
        pat,
        mapped: None,
    };
    let result = logic::probe_bars(&mut c, &f);
    c.finish().map_err(|_| "PCI adversary view cleanup")?;
    let bar = result.map_err(|_| "PCI adversary BAR")?[0].ok_or("PCI adversary BAR absent")?;
    let base = neu_pci_layout[5].load(Ordering::Acquire);
    let size = neu_pci_layout[6].load(Ordering::Acquire);
    if bar.kind != BarKind::Memory32
        || bar.size != neu_kernel::device::BAR_SIZE
        || bar.prefetch
        || (bar.base < base + size && base < bar.base + bar.size)
    {
        return Err("PCI adversary resource conflict");
    }
    Ok((f.bdf, bar))
}

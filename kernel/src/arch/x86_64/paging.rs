//! Kernel-owned paging. The loader HHDM is used only before CR3 activation;
//! afterwards a bounded RW/NX table workspace and one temporary access slot
//! replace it. All raw pointer/CPU operations stay in this backend.

use crate::memory::{FrameAllocator, FrameStats, MemoryMap};
use core::{
    arch::{asm, x86_64::__cpuid},
    ptr,
};
use neu_kernel::paging::{
    self as logic, ADDRESS_FIELD, Error, FrameId, PAGE, PageTree, Permissions, Registry, Result,
    TEST_BASE, TableMemory, Translation,
};

const WINDOW: u64 = 0xffff_9000_0000_0000;
const TABLES: usize = 64;
const SCRATCH_INDEX: usize = 511;
const SCRATCH: u64 = WINDOW + SCRATCH_INDEX as u64 * PAGE;

unsafe extern "C" {
    static __requests_start: u8;
    static __requests_end: u8;
    static __text_start: u8;
    static __text_end: u8;
    static __rodata_start: u8;
    static __rodata_end: u8;
    static __data_start: u8;
    static __kernel_end: u8;
    static __stack_guard_low: u8;
    static __stack_guard_high: u8;
    static __df_guard_low: u8;
    static __df_guard_high: u8;
}

pub fn guards() -> [u64; 4] {
    [
        ptr::addr_of!(__stack_guard_low) as u64,
        ptr::addr_of!(__stack_guard_high) as u64,
        ptr::addr_of!(__df_guard_low) as u64,
        ptr::addr_of!(__df_guard_high) as u64,
    ]
}
fn image_guard(address: u64) -> bool {
    #[cfg(feature = "user-test")]
    if super::userspace::kernel_guards().contains(&address) {
        return true;
    }
    guards().contains(&address)
}
fn image_regions() -> [(u64, u64, Permissions); 4] {
    [
        (
            ptr::addr_of!(__requests_start) as u64,
            ptr::addr_of!(__requests_end) as u64,
            Permissions::RW,
        ),
        (
            ptr::addr_of!(__text_start) as u64,
            ptr::addr_of!(__text_end) as u64,
            Permissions::RX,
        ),
        (
            ptr::addr_of!(__rodata_start) as u64,
            ptr::addr_of!(__rodata_end) as u64,
            Permissions::R,
        ),
        (
            ptr::addr_of!(__data_start) as u64,
            ptr::addr_of!(__kernel_end) as u64,
            Permissions::RW,
        ),
    ]
}
pub fn current_root() -> u64 {
    let root: u64;
    // SAFETY: Ring 0, four-level mode. PCID is disabled by the supported contract.
    unsafe {
        asm!("mov {}, cr3", out(reg) root, options(nomem, nostack, preserves_flags));
    }
    root & ADDRESS_FIELD
}
fn invalidate(address: u64) {
    // SAFETY: Ring 0, one CPU, IF=0. The memory clobber orders page-table writes
    // before invalidation and prevents frame access/reuse moving ahead of it.
    unsafe {
        asm!("invlpg [{}]", in(reg) address, options(nostack, preserves_flags));
    }
}
fn load_root(root: u64) {
    // SAFETY: Caller proved all live code/data/stacks/descriptors are mapped.
    // PGE/PCIDE are disabled, so this flushes old translations and walk caches.
    unsafe {
        asm!("mov cr3, {}", in(reg) root, options(nostack, preserves_flags));
    }
}

struct BootAccess<'a, 'm> {
    root: u64,
    hhdm: u64,
    bits: u8,
    map: &'a MemoryMap<'m>,
}
impl BootAccess<'_, '_> {
    fn view_owned(&self, physical: u64) -> Result<*mut u8> {
        logic::physical(physical, self.bits)?;
        if !self.map.is_allocatable(physical) {
            return Err(Error::NotOwned);
        }
        let address = self.hhdm.checked_add(physical).ok_or(Error::Address)?;
        let view = PageTree {
            root: self.root,
            bits: self.bits,
        }
        .translate(self, address)?
        .ok_or(Error::NotMapped)?;
        if view.physical != physical || !view.permissions.write || view.user {
            return Err(Error::Flags);
        }
        Ok(address as *mut u8)
    }
}
impl TableMemory for BootAccess<'_, '_> {
    fn allocate(&mut self, _: u8) -> Result<u64> {
        Err(Error::Flags)
    }
    fn release(&mut self, _: u64) -> Result<()> {
        Err(Error::Flags)
    }
    fn owned(&self, address: u64, _: u8) -> bool {
        logic::physical(address, self.bits).is_ok() && self.map.contains_region(address, PAGE, 5)
    }
    fn pinned(&self, _: u64) -> bool {
        true
    }
    fn read(&self, table: u64, index: usize) -> Result<u64> {
        if index >= 512 || !self.owned(table, 1) {
            return Err(Error::NotOwned);
        }
        let address = self
            .hhdm
            .checked_add(table)
            .and_then(|base| base.checked_add(index as u64 * 8))
            .ok_or(Error::Address)?;
        if !super::valid_boot_range(address as usize, 8, 8) {
            return Err(Error::Address);
        }
        // SAFETY: Limine base 6 guarantees HHDM mappings of retained table pages;
        // their physical extent is independently checked as loader-reclaimable.
        Ok(unsafe { ptr::read_volatile(address as *const u64) })
    }
    fn write(&mut self, _: u64, _: usize, _: u64) -> Result<()> {
        Err(Error::Flags)
    }
    fn invalidate(&mut self, _: u64) {}
    fn flush(&mut self) {}
}

#[derive(Clone, Copy)]
struct TableOwner {
    physical: u64,
    level: u8,
    pinned: bool,
}
impl TableOwner {
    const EMPTY: Self = Self {
        physical: 0,
        level: 0,
        pinned: false,
    };
}
struct Memory<'a, 'm> {
    frames: &'a mut FrameAllocator<'m>,
    boot: BootAccess<'a, 'm>,
    tables: [TableOwner; TABLES],
    control: u64,
    root: u64,
    active: bool,
    budget: Option<usize>,
}
impl Memory<'_, '_> {
    fn slot(&self, physical: u64) -> Result<usize> {
        self.tables
            .iter()
            .position(|t| t.physical == physical && physical != 0)
            .ok_or(Error::NotOwned)
    }
    fn pointer(&self, physical: u64) -> Result<*mut u64> {
        let slot = self.slot(physical)?;
        if self.active {
            Ok((WINDOW + slot as u64 * PAGE) as *mut u64)
        } else {
            Ok(self.boot.view_owned(physical)?.cast())
        }
    }
    fn access<R>(
        &mut self,
        physical: u64,
        write: bool,
        operation: impl FnOnce(*mut u8) -> R,
    ) -> Result<R> {
        if self.active && current_root() != self.root {
            return Err(Error::InUse);
        }
        logic::physical(physical, self.boot.bits)?;
        if !self.boot.map.is_allocatable(physical) {
            return Err(Error::NotOwned);
        }
        if !self.active {
            return Ok(operation(self.boot.view_owned(physical)?));
        }
        let control = self.pointer(self.control)?;
        let entry = logic::leaf_entry(
            physical,
            if write {
                Permissions::RW
            } else {
                Permissions::R
            },
            self.boot.bits,
        )?;
        // SAFETY: The pinned control PT is RW/NX-mapped in the workspace. Only
        // this exclusive BSP operation uses scratch; its borrow ends before the
        // PTE is removed and invalidated. Callers justify physical ownership.
        unsafe {
            if ptr::read_volatile(control.add(SCRATCH_INDEX)) & 1 != 0 {
                return Err(Error::InUse);
            }
            ptr::write_volatile(control.add(SCRATCH_INDEX), entry);
            invalidate(SCRATCH);
            let result = operation(SCRATCH as *mut u8);
            ptr::write_volatile(control.add(SCRATCH_INDEX), 0);
            invalidate(SCRATCH);
            Ok(result)
        }
    }
    fn zero(&mut self, physical: u64) -> Result<()> {
        self.access(physical, true, |pointer| {
            // SAFETY: Exclusive owned-frame borrow, validated bootstrap alias
            // or the scoped RW/NX scratch mapping; initialize before any read.
            unsafe {
                ptr::write_bytes(pointer, 0, PAGE as usize);
            }
        })
    }
    fn parent_references(&self, child: u64) -> Result<usize> {
        let mut count = 0;
        for table in self
            .tables
            .iter()
            .filter(|t| t.physical != 0 && t.level > 1)
        {
            for index in 0..512 {
                let entry = self.read(table.physical, index)?;
                if entry & 1 != 0 && entry & ADDRESS_FIELD == child {
                    count += 1;
                }
            }
        }
        Ok(count)
    }
    fn count(&self) -> usize {
        self.tables.iter().filter(|t| t.physical != 0).count()
    }
    fn abort_inactive(&mut self) -> Result<()> {
        if self.active {
            return Err(Error::InUse);
        }
        // Abandon every inactive parent/leaf reference before returning frames.
        // The active loader hierarchy is never modified or freed.
        for index in 0..TABLES {
            let physical = self.tables[index].physical;
            if physical != 0 {
                self.zero(physical)?;
                self.tables[index].pinned = false;
            }
        }
        for index in (0..TABLES).rev() {
            if self.tables[index].physical != 0 {
                self.release(self.tables[index].physical)?;
            }
        }
        Ok(())
    }
}
impl TableMemory for Memory<'_, '_> {
    fn allocate(&mut self, level: u8) -> Result<u64> {
        if self.active && current_root() != self.root {
            return Err(Error::InUse);
        }
        if self.budget == Some(0) {
            return Err(Error::Exhausted);
        }
        let slot = self
            .tables
            .iter()
            .position(|t| t.physical == 0)
            .ok_or(Error::Capacity)?;
        let control = if self.active {
            let pointer = self.pointer(self.control)?;
            // SAFETY: A pinned, owned control table; a free ownership slot must
            // have no leftover alias before any allocation changes accounting.
            if unsafe { ptr::read_volatile(pointer.add(slot)) } & 1 != 0 {
                return Err(Error::Corrupt);
            }
            Some(pointer)
        } else {
            None
        };
        let physical = self.frames.allocate().map_err(|_| Error::Exhausted)?;
        if let Err(error) = self.zero(physical) {
            self.frames.free(physical).map_err(|_| Error::Corrupt)?;
            return Err(error);
        }
        self.tables[slot] = TableOwner {
            physical,
            level,
            pinned: false,
        };
        if let Some(control) = control {
            // SAFETY: Slot was unused, and the frame is owned/zeroed. Installing
            // its NX alias precedes any parent pointer referencing the table.
            unsafe {
                ptr::write_volatile(
                    control.add(slot),
                    logic::leaf_entry(physical, Permissions::RW, self.boot.bits)?,
                );
            }
            invalidate(WINDOW + slot as u64 * PAGE);
        }
        if let Some(budget) = &mut self.budget {
            *budget -= 1;
        }
        Ok(physical)
    }
    fn release(&mut self, physical: u64) -> Result<()> {
        if self.active && current_root() != self.root {
            return Err(Error::InUse);
        }
        let slot = self.slot(physical)?;
        if self.tables[slot].pinned
            || (self.active && physical == self.root)
            || self.parent_references(physical)? != 0
        {
            return Err(Error::InUse);
        }
        for index in 0..512 {
            if self.read(physical, index)? & 1 != 0 {
                return Err(Error::InUse);
            }
        }
        if self.active {
            let control = self.pointer(self.control)?;
            // SAFETY: Every parent reference was removed and walk caches flushed
            // by PageTree before release. Remove this final workspace alias too.
            unsafe {
                ptr::write_volatile(control.add(slot), 0);
            }
            invalidate(WINDOW + slot as u64 * PAGE);
        }
        self.frames.free(physical).map_err(|_| Error::Corrupt)?;
        self.tables[slot] = TableOwner::EMPTY;
        Ok(())
    }
    fn owned(&self, address: u64, level: u8) -> bool {
        self.tables
            .iter()
            .any(|t| t.physical == address && address != 0 && t.level == level)
    }
    fn pinned(&self, address: u64) -> bool {
        self.tables
            .iter()
            .any(|t| t.physical == address && t.pinned)
    }
    fn read(&self, table: u64, index: usize) -> Result<u64> {
        if index >= 512 {
            return Err(Error::Address);
        }
        // SAFETY: pointer() accepts only a live owned table and returns its
        // validated alias. All entries were zeroed before publication.
        Ok(unsafe { ptr::read_volatile(self.pointer(table)?.add(index)) })
    }
    fn write(&mut self, table: u64, index: usize, value: u64) -> Result<()> {
        if self.active && current_root() != self.root {
            return Err(Error::InUse);
        }
        if index >= 512 {
            return Err(Error::Address);
        }
        // SAFETY: Exclusive mutable BSP operation on an owned table alias.
        unsafe {
            ptr::write_volatile(self.pointer(table)?.add(index), value);
        }
        Ok(())
    }
    fn invalidate(&mut self, address: u64) {
        if self.active {
            invalidate(address);
        }
    }
    fn flush(&mut self) {
        if self.active {
            load_root(self.root);
        }
    }
}

pub struct Paging<'a, 'm> {
    tree: PageTree,
    memory: Memory<'a, 'm>,
    registry: Registry,
    #[cfg(feature = "user-test")]
    user_root: Option<u64>,
}
impl<'a, 'm> Paging<'a, 'm> {
    pub fn initialize(
        frames: &'a mut FrameAllocator<'m>,
        map: &'a MemoryMap<'m>,
        hhdm: u64,
    ) -> Result<Self> {
        let max = __cpuid(0x8000_0000).eax;
        if max < 0x8000_0008 {
            return Err(Error::Physical);
        }
        let bits = (__cpuid(0x8000_0008).eax & 255) as u8;
        logic::physical(0, bits)?;
        let (cr4, flags): (u64, u64);
        // SAFETY: Read current privileged state before constructing a root.
        unsafe {
            asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack, preserves_flags));
            asm!("pushfq", "pop {}", out(reg) flags, options(preserves_flags));
        }
        if cr4 & ((1 << 7) | (1 << 12) | (1 << 17)) != 0 || flags & 512 != 0 {
            return Err(Error::Flags);
        }
        let before = frames.stats();
        let mut memory = Memory {
            frames,
            boot: BootAccess {
                root: current_root(),
                hhdm,
                bits,
                map,
            },
            tables: [TableOwner::EMPTY; TABLES],
            control: 0,
            root: 0,
            active: false,
            budget: if cfg!(feature = "test-paging-bootstrap-failure") {
                Some(2)
            } else {
                None
            },
        };
        let construction = (|| {
            let tree = PageTree::new(&mut memory, bits)?;
            memory.root = tree.root;
            let image_base = ptr::addr_of!(__requests_start) as u64;
            for (start, end, permissions) in image_regions() {
                if !start.is_multiple_of(PAGE) || !end.is_multiple_of(PAGE) || start >= end {
                    return Err(Error::Alignment);
                }
                for va in (start..end).step_by(PAGE as usize) {
                    if !image_guard(va) {
                        let pa = map
                            .kernel()
                            .start
                            .checked_add(va - image_base)
                            .ok_or(Error::Physical)?;
                        tree.map(&mut memory, va, pa, permissions)?;
                    }
                }
            }
            // Allocate the control branch, then map every table frame (including
            // root/control) into its own non-executable workspace slot.
            tree.map(&mut memory, WINDOW, tree.root, Permissions::RW)?;
            let path = logic::indices(WINDOW)?;
            let mut control = tree.root;
            for index in &path[..3] {
                control = memory.read(control, *index)? & ADDRESS_FIELD;
            }
            memory.control = control;
            for slot in 0..TABLES {
                let pa = memory.tables[slot].physical;
                if pa != 0 {
                    memory.write(control, slot, logic::leaf_entry(pa, Permissions::RW, bits)?)?;
                }
            }
            Ok(tree)
        })();
        let tree = match construction {
            Ok(tree) => tree,
            Err(error) => {
                memory.abort_inactive()?;
                if memory.frames.stats() != before {
                    return Err(Error::Corrupt);
                }
                return Err(error);
            }
        };
        let mut result = Self {
            tree,
            memory,
            registry: Registry::new(),
            #[cfg(feature = "user-test")]
            user_root: None,
        };
        if let Err(error) = result.verify() {
            result.memory.abort_inactive()?;
            if result.stats() != before {
                return Err(Error::Corrupt);
            }
            return Err(error);
        }
        for table in &mut result.memory.tables {
            if table.physical != 0 {
                table.pinned = true;
            }
        }
        if result.tree.root == result.memory.boot.root {
            result.memory.abort_inactive()?;
            return Err(Error::Corrupt);
        }
        result.memory.active = true;
        load_root(result.tree.root);
        if current_root() != result.tree.root {
            return Err(Error::Corrupt);
        }
        result.verify()?;
        Ok(result)
    }

    pub fn root(&self) -> u64 {
        self.tree.root
    }
    pub fn old_root(&self) -> u64 {
        self.memory.boot.root
    }
    pub fn table_count(&self) -> usize {
        self.memory.count()
    }
    pub fn stats(&self) -> FrameStats {
        self.memory.frames.stats()
    }
    pub fn translate(&self, address: u64) -> Result<Option<Translation>> {
        self.tree.translate(&self.memory, address)
    }
    pub fn verify(&self) -> Result<()> {
        let image_base = ptr::addr_of!(__requests_start) as u64;
        for (start, end, permissions) in image_regions() {
            for address in (start..end).step_by(PAGE as usize) {
                let actual = self.translate(address)?;
                if image_guard(address) {
                    if actual.is_some() {
                        return Err(Error::Flags);
                    }
                } else {
                    let expected = self
                        .memory
                        .boot
                        .map
                        .kernel()
                        .start
                        .checked_add(address - image_base)
                        .ok_or(Error::Physical)?;
                    if actual.is_none_or(|a| {
                        a.physical != expected
                            || a.permissions != permissions
                            || a.user
                            || a.page_size != PAGE
                    }) {
                        return Err(Error::Flags);
                    }
                }
            }
        }
        for (slot, table) in self
            .memory
            .tables
            .iter()
            .enumerate()
            .filter(|(_, t)| t.physical != 0)
        {
            let view = self
                .translate(WINDOW + slot as u64 * PAGE)?
                .ok_or(Error::NotMapped)?;
            if view.physical != table.physical || view.permissions != Permissions::RW || view.user {
                return Err(Error::Flags);
            }
            if self.memory.parent_references(table.physical)?
                != usize::from(table.physical != self.tree.root)
            {
                return Err(Error::Corrupt);
            }
        }
        if self.translate(SCRATCH)?.is_some() {
            return Err(Error::InUse);
        }
        Ok(())
    }
    pub fn allocate_frame(&mut self) -> Result<FrameId> {
        if current_root() != self.tree.root {
            return Err(Error::InUse);
        }
        let physical = self
            .memory
            .frames
            .allocate()
            .map_err(|_| Error::Exhausted)?;
        let id = match self.registry.insert(physical) {
            Ok(id) => id,
            // A duplicate would violate the allocator's ownership invariant.
            // Never free a physical page already held by a live identity.
            Err(Error::InUse) => return Err(Error::Corrupt),
            Err(error) => {
                self.memory
                    .frames
                    .free(physical)
                    .map_err(|_| Error::Corrupt)?;
                return Err(error);
            }
        };
        if let Err(error) = self.memory.zero(physical) {
            self.registry.retire(id)?;
            self.memory
                .frames
                .free(physical)
                .map_err(|_| Error::Corrupt)?;
            return Err(error);
        }
        self.registry.initialize(id)?;
        Ok(id)
    }
    pub fn free_frame(&mut self, id: FrameId) -> Result<()> {
        if current_root() != self.tree.root {
            return Err(Error::InUse);
        }
        let physical = self.registry.releasable(id)?;
        self.memory
            .frames
            .free(physical)
            .map_err(|_| Error::Corrupt)?;
        self.registry.retire(id)
    }
    pub fn with_frame_mut<R>(
        &mut self,
        id: FrameId,
        operation: impl FnOnce(&mut [u8; 4096]) -> R,
    ) -> Result<R> {
        self.registry.writable(id)?;
        self.memory
            .access(self.registry.physical(id)?, true, |pointer| {
                // SAFETY: Live initialized owner, no executable alias, exclusive
                // BSP borrow, scratch mapping removed before this method returns.
                operation(unsafe { &mut *pointer.cast::<[u8; 4096]>() })
            })
    }
    pub fn with_frame<R>(
        &mut self,
        id: FrameId,
        operation: impl FnOnce(&[u8; 4096]) -> R,
    ) -> Result<R> {
        self.memory
            .access(self.registry.physical(id)?, false, |pointer| {
                // SAFETY: Live initialized owner; the temporary alias is read-only
                // and NX, and no mutable manager operation can overlap this borrow.
                operation(unsafe { &*pointer.cast::<[u8; 4096]>() })
            })
    }
    pub fn map_frames(
        &mut self,
        base: u64,
        frames: &[FrameId],
        permissions: Permissions,
    ) -> Result<()> {
        logic::map_frames(
            &self.tree,
            &mut self.memory,
            &mut self.registry,
            base,
            frames,
            permissions,
        )
    }
    pub fn unmap(&mut self, address: u64) -> Result<FrameId> {
        let record = self.registry.mapping(address)?;
        let actual = self.tree.unmap(&mut self.memory, address)?;
        if actual.physical != self.registry.physical(record.frame)? {
            return Err(Error::Corrupt);
        }
        self.registry.remove(address)
    }

    pub fn self_test(&mut self) -> Result<()> {
        let before = self.stats();
        let tables = self.table_count();
        let a = self.allocate_frame()?;
        let physical = self.registry.physical(a)?;
        if !self.with_frame(a, |bytes| bytes.iter().all(|b| *b == 0))? {
            return Err(Error::Corrupt);
        }
        self.with_frame_mut(a, |bytes| bytes.fill(0xa5))?;
        self.free_frame(a)?;
        let b = self.allocate_frame()?;
        if self.registry.physical(b)? != physical
            || self.free_frame(a) != Err(Error::NotOwned)
            || !self.with_frame(b, |bytes| bytes.iter().all(|b| *b == 0))?
        {
            return Err(Error::Corrupt);
        }
        let c = self.allocate_frame()?;
        let batch = TEST_BASE + 511 * PAGE;
        self.memory.budget = Some(3);
        let accounting = self.stats();
        if self.map_frames(batch, &[b, c], Permissions::RW) != Err(Error::Exhausted) {
            return Err(Error::Corrupt);
        }
        self.memory.budget = None;
        if self.stats() != accounting
            || self.table_count() != tables
            || self.translate(batch)?.is_some()
            || self.translate(batch + PAGE)?.is_some()
        {
            return Err(Error::Corrupt);
        }
        self.map_frames(TEST_BASE, &[b, c], Permissions::RW)?;
        self.map_frames(TEST_BASE + 2 * PAGE, &[b], Permissions::RW)?;
        // SAFETY: These controlled addresses are now mapped RW to initialized
        // owned frames. Volatile accesses test real CPU translations and aliases.
        unsafe {
            ptr::write_volatile(TEST_BASE as *mut u64, 0x1234_5678);
            ptr::write_volatile((TEST_BASE + PAGE) as *mut u64, 0xabcdef);
            if ptr::read_volatile((TEST_BASE + 2 * PAGE) as *const u64) != 0x1234_5678
                || ptr::read_volatile((TEST_BASE + PAGE) as *const u64) != 0xabcdef
            {
                return Err(Error::Corrupt);
            }
        }
        if self.free_frame(b) != Err(Error::InUse)
            || self.map_frames(TEST_BASE, &[b], Permissions::RW) != Err(Error::AlreadyMapped)
            || self.map_frames(TEST_BASE + 3 * PAGE, &[b], Permissions::RX) != Err(Error::Wx)
            || self.map_frames(TEST_BASE + 1, &[b], Permissions::RW) != Err(Error::Alignment)
            || self.map_frames(TEST_BASE - PAGE, &[b], Permissions::RW) != Err(Error::Address)
            || self.map_frames(
                TEST_BASE + 3 * PAGE,
                &[b],
                Permissions {
                    write: true,
                    execute: true,
                },
            ) != Err(Error::Wx)
        {
            return Err(Error::Corrupt);
        }
        self.unmap(TEST_BASE)?;
        if self.free_frame(b) != Err(Error::InUse) {
            return Err(Error::Corrupt);
        }
        self.unmap(TEST_BASE + 2 * PAGE)?;
        self.unmap(TEST_BASE + PAGE)?;
        self.map_frames(TEST_BASE, &[b], Permissions::RX)?;
        if self.with_frame_mut(b, |_| ()).err() != Some(Error::Wx) {
            return Err(Error::Corrupt);
        }
        self.unmap(TEST_BASE)?;
        self.free_frame(b)?;
        self.free_frame(c)?;
        if self.stats() != before
            || self.table_count() != tables
            || self.translate(TEST_BASE)?.is_some()
        {
            return Err(Error::Corrupt);
        }
        self.verify()
    }

    /// Set up one independently verified protection-fault image. No uncontrolled
    /// overflow or arbitrary unmapped physical access is used for guard tests.
    pub fn prepare_fault(&mut self, kind: usize) -> Result<u64> {
        if kind >= 3 {
            let address = *guards().get(kind - 3).ok_or(Error::Address)?;
            if self.translate(address)?.is_some() {
                return Err(Error::Flags);
            }
            return Ok(address);
        }
        let frame = self.allocate_frame()?;
        if kind == 2 {
            self.with_frame_mut(frame, |bytes| bytes[0] = 0xc3)?;
        }
        self.map_frames(
            TEST_BASE,
            &[frame],
            if kind == 1 {
                Permissions::R
            } else {
                Permissions::RW
            },
        )?;
        if kind == 0 {
            // Populate a real TLB entry before removal and subsequent reuse.
            unsafe {
                ptr::write_volatile(TEST_BASE as *mut u64, 0x1234);
            }
            self.unmap(TEST_BASE)?;
            self.free_frame(frame)?;
            if self.translate(TEST_BASE)?.is_some() {
                return Err(Error::Flags);
            }
        } else {
            let mapping = self.translate(TEST_BASE)?.ok_or(Error::NotMapped)?;
            if mapping.user
                || mapping.permissions.execute
                || (kind == 1 && mapping.permissions.write)
            {
                return Err(Error::Flags);
            }
        }
        Ok(TEST_BASE)
    }
}

// This bootstrap fixture owns every new table; no kernel child-table pointers
// are shared. Its supervisor image backing remains permanently image-reserved.
#[cfg(feature = "user-test")]
pub struct UserSpace {
    pub root: u64,
    tables: [bool; TABLES],
    frames: [Option<FrameId>; 4],
    before: FrameStats,
    before_tables: usize,
}

#[cfg(feature = "user-test")]
impl Paging<'_, '_> {
    pub fn prepare_user(
        &mut self,
        bytes: &[u8],
        kind: u64,
        budget: Option<usize>,
    ) -> Result<UserSpace> {
        use super::userspace::{CODE, CONSTANTS, DATA, STACK};
        if self.user_root.is_some()
            || current_root() != self.tree.root
            || bytes.is_empty()
            || bytes.len() > PAGE as usize
        {
            return Err(Error::InUse);
        }
        let old_tables = self.memory.tables.map(|t| t.physical != 0);
        let mut space = UserSpace {
            root: 0,
            tables: [false; TABLES],
            frames: [None; 4],
            before: self.stats(),
            before_tables: self.table_count(),
        };
        self.user_root = Some(0); // construction excludes a second root
        self.memory.budget = budget;
        let result = (|| {
            for slot in &mut space.frames {
                *slot = Some(self.allocate_frame()?);
            }
            let [Some(a), Some(b), Some(c), Some(d)] = space.frames else {
                return Err(Error::Corrupt);
            };
            let ids = [a, b, c, d];
            self.with_frame_mut(ids[0], |frame| frame[..bytes.len()].copy_from_slice(bytes))?;
            self.with_frame_mut(ids[1], |frame| {
                frame[..8].copy_from_slice(&35u64.to_le_bytes());
                frame[8..16].copy_from_slice(&kind.to_le_bytes());
            })?;
            self.with_frame_mut(ids[2], |frame| frame[0x100] = 0xc3)?;
            let tree = PageTree::new(&mut self.memory, self.tree.bits)?;
            space.root = tree.root;
            let image_base = ptr::addr_of!(__requests_start) as u64;
            for (start, end, permissions) in image_regions() {
                for va in (start..end).step_by(PAGE as usize) {
                    if !image_guard(va) {
                        let pa = self
                            .memory
                            .boot
                            .map
                            .kernel()
                            .start
                            .checked_add(va - image_base)
                            .ok_or(Error::Physical)?;
                        tree.map(&mut self.memory, va, pa, permissions)?;
                    }
                }
            }
            // A private workspace branch, populated only after every allocation.
            tree.map(&mut self.memory, WINDOW, tree.root, Permissions::RW)?;
            for (va, id, permission) in [
                (CODE, ids[0], Permissions::RX),
                (CONSTANTS, ids[1], Permissions::R),
                (DATA, ids[2], Permissions::RW),
                (STACK, ids[3], Permissions::RW),
            ] {
                logic::map_user_frames(
                    &tree,
                    &mut self.memory,
                    &mut self.registry,
                    va,
                    &[id],
                    permission,
                )?;
            }
            let mut control = tree.root;
            for index in &logic::indices(WINDOW)?[..3] {
                control = self.memory.read(control, *index)? & ADDRESS_FIELD;
            }
            for slot in 0..TABLES {
                let table = self.memory.tables[slot].physical;
                if table != 0 {
                    self.memory.write(
                        control,
                        slot,
                        logic::leaf_entry(table, Permissions::RW, tree.bits)?,
                    )?;
                }
            }
            self.verify_user(&space)?;
            Ok(())
        })();
        self.memory.budget = None;
        space.tables =
            core::array::from_fn(|i| !old_tables[i] && self.memory.tables[i].physical != 0);
        self.user_root = Some(space.root);
        if let Err(error) = result {
            self.destroy_user(space)?;
            return Err(error);
        }
        Ok(space)
    }

    fn verify_user(&self, space: &UserSpace) -> Result<()> {
        use super::userspace::{CODE, CONSTANTS, DATA, GUARD, STACK, UNMAPPED};
        let tree = PageTree {
            root: space.root,
            bits: self.tree.bits,
        };
        if space.root == self.tree.root || !self.memory.owned(space.root, 4) {
            return Err(Error::NotOwned);
        }
        let base = ptr::addr_of!(__requests_start) as u64;
        for (start, end, permissions) in image_regions() {
            for address in (start..end).step_by(PAGE as usize) {
                let actual = tree.translate(&self.memory, address)?;
                if image_guard(address) {
                    if actual.is_some() {
                        return Err(Error::Flags);
                    }
                } else if actual.is_none_or(|a| {
                    self.memory
                        .boot
                        .map
                        .kernel()
                        .start
                        .checked_add(address - base)
                        != Some(a.physical)
                        || a.permissions != permissions
                        || a.user
                        || a.page_size != PAGE
                }) {
                    return Err(Error::Flags);
                }
            }
        }
        for (index, (address, permission)) in [
            (CODE, Permissions::RX),
            (CONSTANTS, Permissions::R),
            (DATA, Permissions::RW),
            (STACK, Permissions::RW),
        ]
        .into_iter()
        .enumerate()
        {
            let id = space.frames[index].ok_or(Error::NotOwned)?;
            let actual = tree
                .translate(&self.memory, address)?
                .ok_or(Error::NotMapped)?;
            if actual.physical != self.registry.physical(id)?
                || actual.permissions != permission
                || !actual.user
                || actual.page_size != PAGE
            {
                return Err(Error::Flags);
            }
        }
        for (slot, owner) in self
            .memory
            .tables
            .iter()
            .enumerate()
            .filter(|(_, t)| t.physical != 0)
        {
            let view = tree
                .translate(&self.memory, WINDOW + slot as u64 * PAGE)?
                .ok_or(Error::NotMapped)?;
            if view.physical != owner.physical || view.user || view.permissions != Permissions::RW {
                return Err(Error::Flags);
            }
            let parents = self.memory.parent_references(owner.physical)?;
            if parents
                != usize::from(owner.physical != space.root && owner.physical != self.tree.root)
            {
                return Err(Error::Corrupt);
            }
        }
        for address in [0, GUARD, STACK + PAGE, UNMAPPED, SCRATCH] {
            if tree.translate(&self.memory, address)?.is_some() {
                return Err(Error::Flags);
            }
        }
        Ok(())
    }

    pub fn activate_user(&self, space: &UserSpace) -> Result<()> {
        if self.user_root != Some(space.root) || current_root() != self.tree.root {
            return Err(Error::InUse);
        }
        self.verify_user(space)?;
        // SAFETY: Private, verified hierarchy maps every live kernel mechanism,
        // including both stacks and both roots' supervisor-only table aliases.
        load_root(space.root);
        if current_root() != space.root {
            return Err(Error::Corrupt);
        }
        Ok(())
    }

    pub fn user_result(&mut self, space: &UserSpace) -> Result<u64> {
        if current_root() != self.tree.root {
            return Err(Error::InUse);
        }
        self.with_frame(space.frames[2].ok_or(Error::NotOwned)?, |bytes| {
            u64::from_le_bytes(bytes[..8].try_into().unwrap())
        })
    }

    pub fn destroy_user(&mut self, space: UserSpace) -> Result<()> {
        use super::userspace::{CODE, CONSTANTS, DATA, STACK};
        if current_root() != self.tree.root || self.user_root != Some(space.root) {
            return Err(Error::InUse);
        }
        // The user root is inactive. Remove ALL of its hierarchy and workspace
        // references before freeing any table, including its own workspace PT.
        for slot in 0..TABLES {
            if space.tables[slot] {
                self.memory.zero(self.memory.tables[slot].physical)?;
            }
        }
        load_root(self.tree.root); // no global/PCID translations survive deactivation
        for address in [CODE, CONSTANTS, DATA, STACK] {
            if self.registry.mapping(address).is_ok() {
                self.registry.remove(address)?;
            }
        }
        for slot in (0..TABLES).rev() {
            if space.tables[slot] {
                self.memory.release(self.memory.tables[slot].physical)?;
            }
        }
        for id in space.frames.into_iter().flatten() {
            self.free_frame(id)?;
        }
        self.user_root = None;
        if self.stats() != space.before || self.table_count() != space.before_tables {
            return Err(Error::Corrupt);
        }
        self.verify()
    }
}

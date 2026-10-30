//! Kernel-owned paging. The loader HHDM is used only before CR3 activation;
//! afterwards a bounded RW/NX table workspace and one temporary access slot
//! replace it. All raw pointer/CPU operations stay in this backend.

use crate::memory::{FrameAllocator, FrameStats, MemoryMap};
use core::{
    arch::{asm, x86_64::__cpuid},
    ptr,
};
use neu_kernel::paging::{
    self as logic, ADDRESS_FIELD, Error, FrameId, MEMORY_OBJECT_PAGES, MemoryObjectId, PAGE,
    PageTree, Permissions, Registry, Result, SpaceId, TEST_BASE, TableMemory, Translation,
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
    #[cfg(feature = "native-test")]
    if super::native::guards().contains(&address) {
        return true;
    }
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
    #[cfg(feature = "memory-object-test")]
    reject_leaf_index: Option<usize>,
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
            if current_root() != self.root {
                return Err(Error::InUse);
            }
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
    /// Equivalent parent-count invariant, with each parent read once instead
    /// of once per child. read()/pointer() retain common-root ownership checks.
    fn verify_parent_references(&self) -> Result<()> {
        let mut counts = [0usize; TABLES];
        for parent in self
            .tables
            .iter()
            .filter(|t| t.physical != 0 && t.level > 1)
        {
            for index in 0..512 {
                let entry = self.read(parent.physical, index)?;
                if entry & 1 != 0
                    && let Ok(slot) = self.slot(entry & ADDRESS_FIELD)
                {
                    counts[slot] += 1;
                }
            }
        }
        for (slot, table) in self.tables.iter().enumerate() {
            if table.physical != 0 && counts[slot] != usize::from(table.level != 4) {
                return Err(Error::Corrupt);
            }
        }
        Ok(())
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
        #[cfg(feature = "memory-object-test")]
        if value & 1 != 0 && self.owned(table, 1) && self.reject_leaf_index == Some(index) {
            self.reject_leaf_index = None;
            return Err(Error::Exhausted);
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
            #[cfg(feature = "memory-object-test")]
            reject_leaf_index: None,
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
        }
        self.memory.verify_parent_references()?;
        if self.translate(SCRATCH)?.is_some() {
            return Err(Error::InUse);
        }
        Ok(())
    }
    pub fn allocate_frame(&mut self) -> Result<FrameId> {
        self.allocate_frame_for(SpaceId::KERNEL)
    }
    /// Trusted setup only; no userspace API is exposed. Each frame is zeroed
    /// before the registry can publish the object. On ordinary allocation or
    /// adoption failure, every successfully allocated frame is returned.
    #[allow(dead_code)] // Used by the paging test image; future trusted setup will use it.
    pub fn create_memory_object(&mut self, pages: usize) -> Result<MemoryObjectId> {
        if pages == 0 || pages > MEMORY_OBJECT_PAGES {
            return Err(Error::Address);
        }
        let mut frames = [None; MEMORY_OBJECT_PAGES];
        for index in 0..pages {
            match self.allocate_frame() {
                Ok(frame) => frames[index] = Some(frame),
                Err(error) => {
                    for frame in frames.into_iter().flatten() {
                        self.free_frame(frame)
                            .expect("memory object allocation rollback failed");
                    }
                    return Err(error);
                }
            }
        }
        let mut backing = [frames[0].ok_or(Error::Corrupt)?; MEMORY_OBJECT_PAGES];
        for index in 0..pages {
            backing[index] = frames[index].ok_or(Error::Corrupt)?;
        }
        match self.registry.create_memory_object(&backing[..pages]) {
            Ok(id) => Ok(id),
            Err(error) => {
                for frame in frames.into_iter().flatten() {
                    self.free_frame(frame)
                        .expect("memory object adoption rollback failed");
                }
                Err(error)
            }
        }
    }
    /// Trusted setup retains one paging reference per capability object. The
    /// reference is non-copyable and independent of handle counts.
    #[cfg(feature = "abi-02")]
    pub fn retain_capability_reference(
        &mut self,
        object: MemoryObjectId,
    ) -> Result<logic::MemoryObjectRef> {
        self.registry.retain_memory_object(object)
    }
    #[cfg(feature = "abi-02")]
    pub fn release_capability_reference(
        &mut self,
        reference: logic::MemoryObjectRef,
    ) -> Result<()> {
        self.registry
            .release_memory_object_ref(reference)
            .map_err(|(error, _)| error)
    }
    #[cfg(feature = "memory-object-cap-test")]
    pub fn memory_object_capability_refs(&self, object: MemoryObjectId) -> Result<u8> {
        self.registry.memory_object_capability_refs(object)
    }
    #[cfg(feature = "memory-object-runtime-test")]
    #[allow(dead_code)]
    pub fn authorize_memory_mapping(
        &self,
        object: MemoryObjectId,
        space: SpaceId,
        writable: bool,
    ) -> Result<logic::MemoryMapAuthority> {
        self.registry
            .authorize_memory_mapping(object, space, writable)
    }
    /// Mapping authority derived from the exact live capability reference.
    #[cfg(feature = "memory-object-live-test")]
    pub fn authorize_capability_mapping(
        &self,
        reference: &logic::MemoryObjectRef,
        space: SpaceId,
        writable: bool,
    ) -> Result<logic::MemoryMapAuthority> {
        self.registry
            .authorize_capability_mapping(reference, space, writable)
    }
    /// Collect only after creator release and mapping removal. Unexpected
    /// allocator corruption is fatal to the caller, as in free_frame.
    #[allow(dead_code)] // Paired with trusted MemoryObject setup.
    pub fn collect_memory_object(&mut self, id: MemoryObjectId) -> Result<()> {
        if current_root() != self.tree.root {
            return Err(Error::InUse);
        }
        let frames = self.registry.collectible_memory_object(id)?;
        let mut physical = [0u64; MEMORY_OBJECT_PAGES];
        let mut count = 0;
        for frame in frames.iter().flatten() {
            physical[count] = self.registry.physical(*frame)?;
            count += 1;
        }
        self.memory
            .frames
            .validate_free_batch(&physical[..count])
            .map_err(|_| Error::Corrupt)?;
        // From here the exclusive BSP borrow makes preflight stable. Unexpected
        // invariant failure is fail-stop, never a retryable partial collection.
        let released = self
            .registry
            .collect_memory_object(id)
            .expect("validated object collection");
        self.memory
            .frames
            .free_batch(&physical[..count])
            .expect("validated frame batch");
        for frame in released.into_iter().flatten() {
            self.registry
                .retire(frame)
                .expect("validated backing retirement");
        }
        Ok(())
    }
    fn allocate_frame_for(&mut self, owner: SpaceId) -> Result<FrameId> {
        self.registry.can_allocate(owner)?;
        if current_root() != self.tree.root {
            return Err(Error::InUse);
        }
        let physical = self
            .memory
            .frames
            .allocate()
            .map_err(|_| Error::Exhausted)?;
        let id = match self.registry.insert_for(owner, physical) {
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
            self.registry
                .retire(id)
                .expect("failed to retire uninitialized backing");
            self.memory
                .frames
                .free(physical)
                .expect("failed to free uninitialized backing");
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
        let object = self.create_memory_object(2)?;
        let first = self.registry.memory_object_frame(object, 0)?;
        let second = self.registry.memory_object_frame(object, 1)?;
        let first_physical = self.registry.physical(first)?;
        if self.registry.physical(first)? == self.registry.physical(second)?
            || self.stats().allocated != before.allocated + 2
            || self.registry.memory_object_counts(object)? != (1, 0, 2)
            || !self.with_frame(first, |bytes| bytes.iter().all(|byte| *byte == 0))?
            || !self.with_frame(second, |bytes| bytes.iter().all(|byte| *byte == 0))?
            || self.free_frame(first) != Err(Error::InUse)
        {
            return Err(Error::Corrupt);
        }
        self.with_frame_mut(first, |bytes| bytes.fill(0xa5))?;
        self.registry.release_memory_object(object)?;
        self.collect_memory_object(object)?;
        if self.collect_memory_object(object) != Err(Error::NotOwned) || self.stats() != before {
            return Err(Error::Corrupt);
        }
        let recycled = self.allocate_frame()?;
        if self.registry.physical(recycled)? != first_physical
            || !self.with_frame(recycled, |bytes| bytes.iter().all(|byte| *byte == 0))?
        {
            return Err(Error::Corrupt);
        }
        self.free_frame(recycled)?;
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

// Each root owns private table pages and only its OWN table workspace aliases.
// All backend mutation/inspection uses the separate, pinned kernel workspace.
#[cfg(feature = "user-test")]
pub struct UserSpace {
    pub root: u64,
    id: SpaceId,
    teardown: UserTeardown,
    prepared: bool,
    tables: [bool; TABLES],
    frames: [Option<FrameId>; 4],
    private: Option<FrameId>,
    user_stack_base: u64,
    #[cfg(feature = "multi-test")]
    extra_user: Option<FrameId>,
    #[cfg(feature = "multi-test")]
    user_owners: [Option<neu_kernel::node::ThreadId>; neu_kernel::node::MAX_THREADS],
    #[cfg(feature = "switch-test")]
    stacks: [Option<OwnedStack>; neu_kernel::node::MAX_THREADS],
    #[cfg(feature = "switch-test")]
    stack_tables: [bool; TABLES],
}
#[cfg(feature = "user-test")]
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum UserTeardown {
    Live,
    Started,
    Finished,
}
#[cfg(feature = "user-test")]
impl UserSpace {
    #[cfg(feature = "node-test")]
    pub fn id(&self) -> SpaceId {
        self.id
    }
}

#[cfg(feature = "user-test")]
impl Paging<'_, '_> {
    /// Trusted mapping integration: install new user tables in the owning
    /// token and publish only that root's supervisor workspace aliases.
    #[allow(dead_code, clippy::too_many_arguments)]
    pub fn map_memory_backing(
        &mut self,
        space: &mut UserSpace,
        grant: &logic::MemoryMapAuthority,
        offset: usize,
        base: u64,
        pages: usize,
        permissions: Permissions,
    ) -> Result<()> {
        self.map_memory_backing_at(
            space,
            grant,
            offset,
            base,
            pages,
            permissions,
            logic::Boundary::Exclusive,
        )
    }

    /// Runtime mapping at the scheduler safe point. `owner` is the Thread whose
    /// saved Ring 3 context the controller holds; its execution pin must be the
    /// only pin on the destination root for the whole transaction.
    #[cfg(feature = "memory-object-runtime-test")]
    #[allow(dead_code, clippy::too_many_arguments)]
    pub fn map_memory_backing_suspended(
        &mut self,
        space: &mut UserSpace,
        owner: neu_kernel::node::ThreadId,
        grant: &logic::MemoryMapAuthority,
        offset: usize,
        base: u64,
        pages: usize,
        permissions: Permissions,
    ) -> Result<()> {
        self.member_boundary(space)?;
        let slot = owner.index();
        if space.user_owners.get(slot) != Some(&Some(owner))
            || space
                .stacks
                .get(slot)
                .and_then(Option::as_ref)
                .is_none_or(|s| !s.pinned || s.binding.thread != owner)
            || self.registry.pins(space.id)? != 1
        {
            return Err(Error::NotOwned);
        }
        self.map_memory_backing_at(
            space,
            grant,
            offset,
            base,
            pages,
            permissions,
            logic::Boundary::Suspended,
        )
    }

    #[allow(dead_code, clippy::too_many_arguments)]
    fn map_memory_backing_at(
        &mut self,
        space: &mut UserSpace,
        grant: &logic::MemoryMapAuthority,
        offset: usize,
        base: u64,
        pages: usize,
        permissions: Permissions,
        boundary: logic::Boundary,
    ) -> Result<()> {
        if space.teardown != UserTeardown::Live || !space.prepared {
            return Err(Error::NotOwned);
        }
        self.registry.check_map_boundary(space.id, boundary)?;
        self.verify_user(space)?;
        // The low 64 KiB is reserved for the embedded image and Thread guards.
        logic::user_range(base, pages)?;
        if base < logic::USER_BASE + 0x10000 {
            return Err(Error::Address);
        }
        let tree = PageTree {
            root: space.root,
            bits: self.tree.bits,
        };
        let needed = tree.required_user_tables(&self.memory, base, pages)?;
        if needed
            > self
                .memory
                .tables
                .iter()
                .filter(|t| t.physical == 0)
                .count()
            || needed > self.stats().free
        {
            return Err(Error::Exhausted);
        }
        let old = self.memory.tables.map(|t| t.physical);
        let before = self.stats();
        let mut control = tree.root;
        for index in &logic::indices(WINDOW)?[..3] {
            control = self.memory.read(control, *index)? & ADDRESS_FIELD;
        }
        if let Err(error) = match boundary {
            logic::Boundary::Exclusive => logic::map_memory_object(
                &tree,
                &mut self.memory,
                &mut self.registry,
                grant,
                offset,
                base,
                pages,
                permissions,
            ),
            logic::Boundary::Suspended => logic::map_memory_object_suspended(
                &tree,
                &mut self.memory,
                &mut self.registry,
                grant,
                offset,
                base,
                pages,
                permissions,
            ),
        } {
            assert!(
                self.memory.tables.map(|t| t.physical) == old && self.stats() == before,
                "shared mapping rollback changed table ownership"
            );
            return Err(error);
        }
        // Generic mapping has committed. All following operations were
        // preflighted, and unexpected failures are fail-stop.
        for (slot, previous) in old.iter().copied().enumerate() {
            if previous == 0 && self.memory.tables[slot].physical != 0 {
                assert!(!space.tables[slot]);
                let leaf = logic::leaf_entry(
                    self.memory.tables[slot].physical,
                    Permissions::RW,
                    tree.bits,
                )
                .expect("owned table encoding");
                self.memory
                    .write(control, slot, leaf)
                    .expect("user table alias publication");
                space.tables[slot] = true;
            }
        }
        self.verify_user(space)
            .expect("shared mapping inventory verification");
        for i in 0..pages {
            match boundary {
                logic::Boundary::Exclusive => logic::validate_memory_object_mapping(
                    &tree,
                    &self.memory,
                    &self.registry,
                    space.id,
                    base + i as u64 * PAGE,
                ),
                logic::Boundary::Suspended => logic::validate_memory_object_mapping_suspended(
                    &tree,
                    &self.memory,
                    &self.registry,
                    space.id,
                    base + i as u64 * PAGE,
                ),
            }
            .expect("committed shared leaf");
        }
        Ok(())
    }

    /// Normal shared unmap also prunes empty owned tables and their aliases.
    #[allow(dead_code)]
    pub fn unmap_memory_backing(&mut self, space: &mut UserSpace, address: u64) -> Result<()> {
        if space.teardown != UserTeardown::Live {
            return Err(Error::NotOwned);
        }
        self.registry.can_allocate(space.id)?;
        self.verify_user(space)?;
        let tree = PageTree {
            root: space.root,
            bits: self.tree.bits,
        };
        logic::validate_memory_object_mapping(
            &tree,
            &self.memory,
            &self.registry,
            space.id,
            address,
        )?;
        let path = logic::indices(address)?;
        let mut tables = [tree.root; 4];
        for depth in 0..3 {
            tables[depth + 1] = self.memory.read(tables[depth], path[depth])? & ADDRESS_FIELD;
        }
        let mut control = tree.root;
        for index in &logic::indices(WINDOW)?[..3] {
            control = self.memory.read(control, *index)? & ADDRESS_FIELD;
        }
        for table in tables {
            if !space.tables[self.memory.slot(table)?] {
                return Err(Error::NotOwned);
            }
        }
        logic::unmap_memory_object_preserving_tables(
            &tree,
            &mut self.memory,
            &mut self.registry,
            space.id,
            address,
        )?;
        let cleanup: Result<()> = (|| {
            for depth in (1..4).rev() {
                let table = tables[depth];
                if self.memory.pinned(table) {
                    break;
                }
                let mut empty = true;
                for i in 0..512 {
                    if self.memory.read(table, i)? & 1 != 0 {
                        empty = false;
                        break;
                    }
                }
                if !empty {
                    break;
                }
                let slot = self.memory.slot(table)?;
                self.memory.write(tables[depth - 1], path[depth - 1], 0)?;
                self.memory.write(control, slot, 0)?;
                self.memory.flush();
                self.memory.release(table)?;
                space.tables[slot] = false;
            }
            self.verify_user(space)
        })();
        cleanup.expect("shared unmap inventory cleanup");
        Ok(())
    }
    #[cfg(not(feature = "switch-test"))]
    pub fn prepare_user(
        &mut self,
        bytes: &[u8],
        kind: u64,
        budget: Option<usize>,
    ) -> Result<UserSpace> {
        self.prepare_domain(bytes, kind, 35, None, budget)
    }
    pub fn prepare_domain(
        &mut self,
        bytes: &[u8],
        kind: u64,
        seed: u64,
        private_value: Option<u64>,
        budget: Option<usize>,
    ) -> Result<UserSpace> {
        use super::userspace::{CODE, CONSTANTS, DATA, STACK, UNMAPPED};
        if current_root() != self.tree.root || bytes.is_empty() || bytes.len() > PAGE as usize {
            return Err(Error::InUse);
        }
        let id = self.registry.create_space()?;
        let before = self.stats();
        let before_tables = self.table_count();
        let old_tables = self.memory.tables.map(|t| t.physical != 0);
        let mut space = UserSpace {
            root: 0,
            id,
            teardown: UserTeardown::Live,
            prepared: false,
            tables: [false; TABLES],
            frames: [None; 4],
            private: None,
            user_stack_base: STACK,
            #[cfg(feature = "multi-test")]
            extra_user: None,
            #[cfg(feature = "multi-test")]
            user_owners: [None; neu_kernel::node::MAX_THREADS],
            #[cfg(feature = "switch-test")]
            stacks: core::array::from_fn(|_| None),
            #[cfg(feature = "switch-test")]
            stack_tables: [false; TABLES],
        };
        self.memory.budget = budget;
        let result = (|| {
            for slot in &mut space.frames {
                *slot = Some(self.allocate_frame_for(id)?);
            }
            let [Some(a), Some(b), Some(c), Some(d)] = space.frames else {
                return Err(Error::Corrupt);
            };
            let ids = [a, b, c, d];
            self.with_frame_mut(a, |frame| frame[..bytes.len()].copy_from_slice(bytes))?;
            self.with_frame_mut(b, |frame| {
                frame[..8].copy_from_slice(&seed.to_le_bytes());
                frame[8..16].copy_from_slice(&kind.to_le_bytes());
            })?;
            self.with_frame_mut(c, |frame| frame[0x100] = 0xc3)?;
            if let Some(value) = private_value {
                let frame = self.allocate_frame_for(id)?;
                space.private = Some(frame);
                self.with_frame_mut(frame, |bytes| {
                    bytes[..8].copy_from_slice(&value.to_le_bytes())
                })?;
            }
            let tree = PageTree::new(&mut self.memory, self.tree.bits)?;
            space.root = tree.root;
            self.registry.bind_root(id, tree.root)?;
            let base = ptr::addr_of!(__requests_start) as u64;
            for (start, end, permissions) in image_regions() {
                for va in (start..end).step_by(PAGE as usize) {
                    if !image_guard(va) {
                        let pa = self
                            .memory
                            .boot
                            .map
                            .kernel()
                            .start
                            .checked_add(va - base)
                            .ok_or(Error::Physical)?;
                        tree.map(&mut self.memory, va, pa, permissions)?;
                    }
                }
            }
            #[cfg(feature = "preempt-test")]
            if let Some((physical, pat)) = super::interrupts::user_device() {
                tree.map_device_uc(&mut self.memory, LAPIC_VIEW, physical, pat)?;
            }
            tree.map(&mut self.memory, WINDOW, tree.root, Permissions::RW)?;
            for (va, frame, permission) in [
                (CODE, ids[0], Permissions::RX),
                (CONSTANTS, ids[1], Permissions::R),
                (DATA, ids[2], Permissions::RW),
                (STACK, ids[3], Permissions::RW),
            ] {
                logic::map_user_frames_for(
                    &tree,
                    &mut self.memory,
                    &mut self.registry,
                    id,
                    va,
                    &[frame],
                    permission,
                )?;
            }
            if let Some(frame) = space.private {
                logic::map_user_frames_for(
                    &tree,
                    &mut self.memory,
                    &mut self.registry,
                    id,
                    UNMAPPED,
                    &[frame],
                    Permissions::R,
                )?;
            }
            space.tables =
                core::array::from_fn(|i| !old_tables[i] && self.memory.tables[i].physical != 0);
            let mut control = tree.root;
            for index in &logic::indices(WINDOW)?[..3] {
                control = self.memory.read(control, *index)? & ADDRESS_FIELD;
            }
            // Remove the temporary WINDOW->root leaf before publishing exactly
            // this root's private aliases. No sibling table is borrowed/mapped.
            self.memory.write(control, 0, 0)?;
            for slot in 0..TABLES {
                if space.tables[slot] {
                    self.memory.write(
                        control,
                        slot,
                        logic::leaf_entry(
                            self.memory.tables[slot].physical,
                            Permissions::RW,
                            tree.bits,
                        )?,
                    )?;
                }
            }
            self.verify_user(&space)?;
            Ok(())
        })();
        self.memory.budget = None;
        space.tables =
            core::array::from_fn(|i| !old_tables[i] && self.memory.tables[i].physical != 0);
        if let Err(error) = result {
            self.destroy_user(&mut space)?;
            if self.stats() != before || self.table_count() != before_tables {
                return Err(Error::Corrupt);
            }
            return Err(error);
        }
        space.prepared = true;
        Ok(space)
    }
    pub fn verify_user(&self, space: &UserSpace) -> Result<()> {
        use super::userspace::{CODE, CONSTANTS, DATA, GUARD, STACK, UNMAPPED};
        if current_root() != self.tree.root || self.registry.root(space.id)? != space.root {
            return Err(Error::InUse);
        }
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
        for (index, (address, permissions)) in [
            (CODE, Permissions::RX),
            (CONSTANTS, Permissions::R),
            (DATA, Permissions::RW),
            (space.user_stack_base, Permissions::RW),
        ]
        .into_iter()
        .enumerate()
        {
            #[cfg(feature = "multi-test")]
            if index == 3 && space.frames[index].is_none() {
                if tree.translate(&self.memory, address)?.is_some() {
                    return Err(Error::Flags);
                }
                continue;
            }
            let frame = space.frames[index].ok_or(Error::NotOwned)?;
            let record = self.registry.mapping_for(space.id, address)?;
            let actual = tree
                .translate(&self.memory, address)?
                .ok_or(Error::NotMapped)?;
            if record.frame != frame
                || self.registry.owner(frame)? != space.id
                || actual.physical != self.registry.physical(frame)?
                || actual.permissions != permissions
                || !actual.user
                || actual.page_size != PAGE
            {
                return Err(Error::Flags);
            }
        }
        #[cfg(feature = "multi-test")]
        if let Some(frame) = space.extra_user {
            self.verify_user_stack_leaf(space, frame, logic::thread_user_stack(2))?;
        }
        for guard in [space.user_stack_base - PAGE, space.user_stack_base + PAGE] {
            if tree.translate(&self.memory, guard)?.is_some() {
                return Err(Error::Flags);
            }
        }
        #[cfg(feature = "preempt-test")]
        if let Some((physical, _)) = super::interrupts::user_device() {
            let mapping = tree
                .translate(&self.memory, LAPIC_VIEW)?
                .ok_or(Error::NotMapped)?;
            if mapping.physical != physical
                || mapping.user
                || mapping.permissions != Permissions::RW
            {
                return Err(Error::Flags);
            }
        }
        for slot in 0..TABLES {
            let actual = tree.translate(&self.memory, WINDOW + slot as u64 * PAGE)?;
            #[cfg(feature = "switch-test")]
            let stack_table = space.stack_tables[slot];
            #[cfg(not(feature = "switch-test"))]
            let stack_table = false;
            if space.tables[slot] && !stack_table {
                let table = self.memory.tables[slot];
                if table.physical == 0
                    || actual.is_none_or(|a| {
                        a.physical != table.physical || a.user || a.permissions != Permissions::RW
                    })
                {
                    return Err(Error::Flags);
                }
            } else if actual.is_some() {
                return Err(Error::Flags);
            }
        }
        self.memory.verify_parent_references()?;
        let private = tree.translate(&self.memory, UNMAPPED)?;
        if let Some(frame) = space.private {
            let record = self.registry.mapping_for(space.id, UNMAPPED)?;
            if record.frame != frame
                || private.is_none_or(|p| {
                    p.physical != self.registry.physical(frame).unwrap_or(0)
                        || !p.user
                        || p.permissions != Permissions::R
                })
            {
                return Err(Error::Flags);
            }
        } else if private.is_some() {
            return Err(Error::Flags);
        }
        for address in [0, GUARD, STACK + PAGE, SCRATCH] {
            if tree.translate(&self.memory, address)?.is_some() {
                return Err(Error::Flags);
            }
        }
        Ok(())
    }
    pub fn activate_user(&mut self, space: &UserSpace) -> Result<()> {
        if current_root() != self.tree.root {
            return Err(Error::InUse);
        }
        self.verify_user(space)?;
        self.registry.activate_space(space.id)?;
        load_root(space.root);
        if current_root() != space.root {
            return Err(Error::Corrupt);
        }
        Ok(())
    }
    pub fn deactivate_user(&mut self, space: &UserSpace) -> Result<()> {
        if current_root() != self.tree.root || self.registry.root(space.id)? != space.root {
            return Err(Error::InUse);
        }
        self.registry.deactivate_space(space.id)
    }
    #[cfg(not(feature = "switch-test"))]
    pub fn user_result(&mut self, space: &UserSpace) -> Result<u64> {
        if current_root() != self.tree.root || self.registry.root(space.id)? != space.root {
            return Err(Error::InUse);
        }
        self.with_frame(space.frames[2].ok_or(Error::NotOwned)?, |bytes| {
            u64::from_le_bytes(bytes[..8].try_into().unwrap())
        })
    }
    #[cfg(feature = "node-test")]
    pub fn private_value(&mut self, space: &UserSpace) -> Result<Option<u64>> {
        if current_root() != self.tree.root || self.registry.root(space.id)? != space.root {
            return Err(Error::InUse);
        }
        space
            .private
            .map(|id| {
                self.with_frame(id, |bytes| {
                    u64::from_le_bytes(bytes[..8].try_into().unwrap())
                })
            })
            .transpose()
    }
    #[cfg(feature = "node-test")]
    pub fn user_physical(&self, space: &UserSpace, address: u64) -> Result<u64> {
        if self.registry.root(space.id)? != space.root {
            return Err(Error::NotOwned);
        }
        let mapping = self.registry.mapping_for(space.id, address)?;
        self.registry.physical(mapping.frame)
    }
    #[cfg(feature = "memory-object-runtime-test")]
    pub fn user_translate(&self, space: &UserSpace, address: u64) -> Result<Option<Translation>> {
        if self.registry.root(space.id)? != space.root {
            return Err(Error::NotOwned);
        }
        PageTree {
            root: space.root,
            bits: self.tree.bits,
        }
        .translate(&self.memory, address)
    }
    /// Preflight failures retain the live token for retry. Once teardown
    /// begins, an unexpected error leaves it permanently non-retryable.
    pub fn destroy_user(&mut self, space: &mut UserSpace) -> Result<()> {
        use super::userspace::{CODE, CONSTANTS, DATA, UNMAPPED};
        if space.teardown != UserTeardown::Live {
            return Err(Error::NotOwned);
        }
        if current_root() != self.tree.root || self.registry.root(space.id)? != space.root {
            return Err(Error::InUse);
        }
        self.registry.can_destroy_space(space.id)?; // execution pins and object references
        if space.prepared {
            self.verify_user(space)?;
        }
        #[cfg(feature = "switch-test")]
        if space.stacks.iter().any(Option::is_some) {
            return Err(Error::InUse);
        }
        #[cfg(feature = "multi-test")]
        if space.extra_user.is_some() || space.user_owners.iter().any(Option::is_some) {
            return Err(Error::InUse);
        }
        let tree = PageTree {
            root: space.root,
            bits: self.tree.bits,
        };
        let mut shared = [0u64; logic::MAPPINGS];
        let mut shared_count = 0;
        for mapping in self.registry.memory_object_mappings(space.id)? {
            logic::validate_memory_object_mapping(
                &tree,
                &self.memory,
                &self.registry,
                space.id,
                mapping.address,
            )?;
            // Every walk table must belong to this token's inventory. A table
            // allocated later without being recorded cannot safely be freed.
            let mut table = tree.root;
            let path = logic::indices(mapping.address)?;
            for (depth, index) in path.iter().copied().enumerate() {
                let slot = self.memory.slot(table)?;
                if !space.tables[slot] {
                    return Err(Error::NotOwned);
                }
                if depth < 3 {
                    table = self.memory.read(table, index)? & ADDRESS_FIELD;
                }
            }
            shared[shared_count] = mapping.address;
            shared_count += 1;
        }
        let before = self.stats();
        let old_tables = self.table_count();
        let tables = space.tables.iter().filter(|v| **v).count();
        let released =
            tables + space.frames.iter().flatten().count() + usize::from(space.private.is_some());
        space.teardown = UserTeardown::Started;
        let teardown: Result<()> = (|| {
            for address in shared[..shared_count].iter().copied() {
                logic::unmap_memory_object_preserving_tables(
                    &tree,
                    &mut self.memory,
                    &mut self.registry,
                    space.id,
                    address,
                )?;
            }
            for slot in 0..TABLES {
                if space.tables[slot] {
                    self.memory.zero(self.memory.tables[slot].physical)?;
                }
            }
            load_root(self.tree.root);
            for address in [CODE, CONSTANTS, DATA, space.user_stack_base, UNMAPPED] {
                if self.registry.mapping_for(space.id, address).is_ok() {
                    self.registry.remove_for(space.id, address)?;
                }
            }
            #[cfg(feature = "multi-test")]
            if self.registry.mapping_for(space.id, 0x410000).is_ok() {
                self.registry.remove_for(space.id, 0x410000)?;
            }
            for slot in (0..TABLES).rev() {
                if space.tables[slot] {
                    self.memory.release(self.memory.tables[slot].physical)?;
                }
            }
            for frame in space.frames.iter().copied().flatten().chain(space.private) {
                self.free_frame(frame)?;
            }
            self.registry.clear_root(space.id, space.root)?;
            self.registry.retire_space(space.id)?;
            let after = self.stats();
            if after.total != before.total
                || after.allocated + released != before.allocated
                || after.free != before.free + released
                || self.table_count() + tables != old_tables
            {
                return Err(Error::Corrupt);
            }
            self.verify()?;
            Ok(())
        })();
        // Nothing may resume or retry against a partially dismantled root.
        teardown.expect("irreversible user-space teardown failed");
        space.teardown = UserTeardown::Finished;
        Ok(())
    }
}

#[cfg(any(feature = "timer-test", feature = "preempt-test"))]
pub const LAPIC_VIEW: u64 = 0xffff_b000_0000_0000;
#[cfg(any(feature = "timer-test", feature = "preempt-test"))]
impl Paging<'_, '_> {
    pub fn map_lapic_uc(&mut self, physical: u64, pat: u64) -> Result<u64> {
        if current_root() != self.tree.root || super::interrupts::flags() & 0x200 != 0 {
            return Err(Error::InUse);
        }
        logic::physical(physical, self.tree.bits)?;
        // This device view cannot alias any described RAM, executable image,
        // loader allocation or framebuffer, even if that RAM is not allocatable.
        if self
            .memory
            .boot
            .map
            .regions()
            .iter()
            .any(|r| r.kind != 1 && physical < r.base + r.length && physical + PAGE > r.base)
        {
            return Err(Error::NotOwned);
        }
        self.tree
            .map_device_uc(&mut self.memory, LAPIC_VIEW, physical, pat)?;
        let mut table = self.tree.root;
        let path = logic::indices(LAPIC_VIEW)?;
        for index in &path[..3] {
            table = self.memory.read(table, *index)? & ADDRESS_FIELD;
        }
        let leaf = self.memory.read(table, path[3])?;
        let view = self
            .tree
            .translate(&self.memory, LAPIC_VIEW)?
            .ok_or(Error::NotMapped)?;
        if leaf & (0x9f | logic::NX) != (0x1b | logic::NX)
            || view.physical != physical
            || view.user
            || view.permissions != Permissions::RW
        {
            return Err(Error::Flags);
        }
        Ok(leaf)
    }
    pub fn unmap_lapic(&mut self) -> Result<()> {
        if current_root() != self.tree.root || super::interrupts::flags() & 0x200 != 0 {
            return Err(Error::InUse);
        }
        self.tree.unmap(&mut self.memory, LAPIC_VIEW)?;
        if self.tree.translate(&self.memory, LAPIC_VIEW)?.is_some() {
            return Err(Error::Corrupt);
        }
        self.verify()
    }
}

#[cfg(feature = "switch-test")]
struct OwnedStack {
    binding: neu_kernel::context::StackBinding,
    frames: [Option<FrameId>; logic::KSTACK_PAGES],
    #[cfg(feature = "multi-test")]
    pinned: bool,
}
#[cfg(feature = "switch-test")]
impl Paging<'_, '_> {
    #[cfg(not(feature = "multi-test"))]
    pub fn prepare_thread_stack(
        &mut self,
        space: &mut UserSpace,
        active: neu_kernel::node::ActiveContext,
        budget: Option<usize>,
    ) -> Result<neu_kernel::context::StackBinding> {
        use neu_kernel::context::StackBinding;
        if current_root() != self.tree.root
            || space
                .stacks
                .get(active.thread.index())
                .is_none_or(Option::is_some)
            || active.context.root != space.root
            || active.context.space != space.id
        {
            return Err(Error::InUse);
        }
        self.registry.can_allocate(space.id)?;
        let binding = StackBinding::new(active);
        if !binding.valid(active) {
            return Err(Error::NotOwned);
        }
        let before = self.stats();
        let tables = self.table_count();
        let old = self.memory.tables.map(|t| t.physical != 0);
        let mut stack = OwnedStack {
            binding,
            frames: [None; logic::KSTACK_PAGES],
            #[cfg(feature = "multi-test")]
            pinned: false,
        };
        let mut remaining = budget;
        let result = (|| {
            for frame in &mut stack.frames {
                if let Some(left) = &mut remaining {
                    if *left == 0 {
                        return Err(Error::Exhausted);
                    }
                    *left -= 1;
                }
                let id = self.allocate_frame_for(space.id)?;
                *frame = Some(id);
                self.with_frame_mut(id, |bytes| bytes.fill(0xa5))?;
            }
            let [Some(a), Some(b)] = stack.frames else {
                return Err(Error::Corrupt);
            };
            self.memory.budget = remaining;
            logic::map_stack_frames_for(
                &PageTree {
                    root: space.root,
                    bits: self.tree.bits,
                },
                &mut self.memory,
                &mut self.registry,
                space.id,
                binding.bottom,
                &[a, b],
            )
        })();
        self.memory.budget = None;
        for (i, was_owned) in old.iter().enumerate() {
            let added = !*was_owned && self.memory.tables[i].physical != 0;
            space.tables[i] |= added;
            space.stack_tables[i] |= added;
        }
        space.stacks[active.thread.index()] = Some(stack);
        if let Err(error) = result {
            self.destroy_thread_stack(space, active.thread)?;
            if self.stats() != before || self.table_count() != tables {
                return Err(Error::Corrupt);
            }
            return Err(error);
        }
        self.verify_thread_stack(space, binding)?;
        Ok(binding)
    }
    pub fn verify_thread_stack(
        &self,
        space: &UserSpace,
        binding: neu_kernel::context::StackBinding,
    ) -> Result<()> {
        self.verify_user(space)?;
        let stack = space
            .stacks
            .get(binding.thread.index())
            .and_then(Option::as_ref)
            .ok_or(Error::NotOwned)?;
        if stack.binding != binding || binding.space != space.id || binding.root != space.root {
            return Err(Error::NotOwned);
        }
        let tree = PageTree {
            root: space.root,
            bits: self.tree.bits,
        };
        for (index, frame) in stack.frames.iter().enumerate() {
            let frame = frame.ok_or(Error::NotOwned)?;
            let va = binding.bottom + index as u64 * PAGE;
            let pte = tree.translate(&self.memory, va)?.ok_or(Error::NotMapped)?;
            if pte.user
                || pte.permissions != Permissions::RW
                || pte.page_size != PAGE
                || pte.physical != self.registry.physical(frame)?
                || self.registry.owner(frame)? != space.id
                || self.registry.mapping_for(space.id, va)?.frame != frame
                || self.tree.translate(&self.memory, va)?.is_some()
            {
                return Err(Error::Flags);
            }
        }
        for va in [binding.bottom - PAGE, binding.top] {
            if tree.translate(&self.memory, va)?.is_some() {
                return Err(Error::Flags);
            }
        }
        for slot in 0..neu_kernel::node::MAX_THREADS {
            let base = logic::KSTACK_BASE + slot as u64 * logic::KSTACK_STRIDE + PAGE;
            if space.stacks[slot].is_none() {
                for offset in [0, PAGE] {
                    if tree.translate(&self.memory, base + offset)?.is_some() {
                        return Err(Error::Flags);
                    }
                }
            }
        }
        Ok(())
    }
    #[cfg(not(feature = "multi-test"))]
    pub fn pin_context(&mut self, space: &UserSpace) -> Result<()> {
        let stack = space
            .stacks
            .iter()
            .flatten()
            .next()
            .ok_or(Error::NotOwned)?;
        self.verify_thread_stack(space, stack.binding)?;
        self.registry.pin_space(space.id)
    }
    #[cfg(not(feature = "multi-test"))]
    pub fn unpin_context(&mut self, space: &UserSpace) -> Result<()> {
        if current_root() != self.tree.root || self.registry.root(space.id)? != space.root {
            return Err(Error::InUse);
        }
        self.registry.unpin_space(space.id)
    }
    pub fn context_reclaimable(&self, space: &UserSpace) -> Result<()> {
        if current_root() != self.tree.root || self.registry.root(space.id)? != space.root {
            return Err(Error::InUse);
        }
        self.registry.can_allocate(space.id)
    }
    #[cfg(not(feature = "multi-test"))]
    pub fn destroy_thread_stack(
        &mut self,
        space: &mut UserSpace,
        owner: neu_kernel::node::ThreadId,
    ) -> Result<()> {
        self.context_reclaimable(space)?;
        if space
            .stacks
            .get(owner.index())
            .and_then(Option::as_ref)
            .is_none_or(|s| s.binding.thread != owner)
        {
            return Err(Error::NotOwned);
        }
        let stack = space.stacks[owner.index()].take().ok_or(Error::NotOwned)?;
        let tree = PageTree {
            root: space.root,
            bits: self.tree.bits,
        };
        for index in 0..logic::KSTACK_PAGES {
            let va = stack.binding.bottom + index as u64 * PAGE;
            if tree.translate(&self.memory, va)?.is_some() {
                if self.registry.mapping_for(space.id, va)?.frame
                    != stack.frames[index].ok_or(Error::Corrupt)?
                {
                    return Err(Error::Corrupt);
                }
                tree.unmap(&mut self.memory, va)?;
                self.registry.remove_for(space.id, va)?;
            }
        }
        for frame in stack.frames.into_iter().flatten() {
            self.free_frame(frame)?;
        }
        for slot in 0..TABLES {
            space.tables[slot] &= self.memory.tables[slot].physical != 0;
            space.stack_tables[slot] &= self.memory.tables[slot].physical != 0;
        }
        self.verify_user(space)?;
        self.verify()
    }
    pub fn thread_stack_high_water(
        &mut self,
        space: &UserSpace,
        owner: neu_kernel::node::ThreadId,
    ) -> Result<()> {
        let s = space
            .stacks
            .get(owner.index())
            .and_then(Option::as_ref)
            .filter(|s| s.binding.thread == owner)
            .ok_or(Error::NotOwned)?;
        let frame = s.frames[0].ok_or(Error::NotOwned)?;
        if !self.with_frame(frame, |bytes| bytes[..512].iter().all(|b| *b == 0xa5))? {
            return Err(Error::Corrupt);
        }
        Ok(())
    }
    #[cfg(not(feature = "multi-test"))]
    pub fn user_words(&mut self, space: &UserSpace) -> Result<[u64; 3]> {
        if current_root() != self.tree.root || self.registry.root(space.id)? != space.root {
            return Err(Error::InUse);
        }
        self.with_frame(space.frames[2].ok_or(Error::NotOwned)?, |bytes| {
            core::array::from_fn(|i| {
                u64::from_le_bytes(bytes[i * 8..i * 8 + 8].try_into().unwrap())
            })
        })
    }
}

#[cfg(feature = "preempt-test")]
impl Paging<'_, '_> {
    #[cfg(not(feature = "multi-test"))]
    pub fn user_limit(&mut self, space: &UserSpace, limit: u64) -> Result<()> {
        if current_root() != self.tree.root || self.registry.root(space.id)? != space.root {
            return Err(Error::InUse);
        }
        self.with_frame_mut(space.frames[2].ok_or(Error::NotOwned)?, |bytes| {
            bytes[24..32].copy_from_slice(&limit.to_le_bytes())
        })
    }
}

#[cfg(feature = "multi-test")]
impl Paging<'_, '_> {
    fn member_boundary(&self, space: &UserSpace) -> Result<()> {
        if current_root() != self.tree.root
            || super::interrupts::flags() & 0x200 != 0
            || self.registry.root(space.id)? != space.root
        {
            return Err(Error::InUse);
        }
        Ok(())
    }
    pub fn relocate_user_stack(&mut self, space: &mut UserSpace, base: u64) -> Result<()> {
        self.member_boundary(space)?;
        self.registry.can_allocate(space.id)?;
        if ![logic::thread_user_stack(0), logic::thread_user_stack(1)].contains(&base)
            || space.user_owners.iter().any(Option::is_some)
        {
            return Err(Error::Address);
        }
        if base == space.user_stack_base {
            return Ok(());
        }
        let frame = space.frames[3].ok_or(Error::NotOwned)?;
        let tree = PageTree {
            root: space.root,
            bits: self.tree.bits,
        };
        tree.unmap(&mut self.memory, space.user_stack_base)?;
        self.registry.remove_for(space.id, space.user_stack_base)?;
        logic::map_user_frames_for(
            &tree,
            &mut self.memory,
            &mut self.registry,
            space.id,
            base,
            &[frame],
            Permissions::RW,
        )?;
        space.user_stack_base = base;
        self.verify_user(space)
    }
    pub fn map_shared_probe(&mut self, space: &UserSpace) -> Result<()> {
        self.member_boundary(space)?;
        logic::map_user_frames_for(
            &PageTree {
                root: space.root,
                bits: self.tree.bits,
            },
            &mut self.memory,
            &mut self.registry,
            space.id,
            0x410000,
            &[space.frames[2].ok_or(Error::NotOwned)?],
            Permissions::RW,
        )
    }
    fn verify_user_stack_leaf(&self, space: &UserSpace, frame: FrameId, base: u64) -> Result<()> {
        let tree = PageTree {
            root: space.root,
            bits: self.tree.bits,
        };
        let leaf = tree
            .translate(&self.memory, base)?
            .ok_or(Error::NotMapped)?;
        if leaf.physical != self.registry.physical(frame)?
            || !leaf.user
            || leaf.permissions != Permissions::RW
            || leaf.page_size != PAGE
            || self.registry.owner(frame)? != space.id
            || self.registry.mapping_for(space.id, base)?.frame != frame
        {
            return Err(Error::Flags);
        }
        for guard in [base - PAGE, base + PAGE] {
            if tree.translate(&self.memory, guard)?.is_some() {
                return Err(Error::Flags);
            }
        }
        Ok(())
    }
    fn allocate_member_frame(
        &mut self,
        edit: &neu_kernel::node::StackEdit<'_>,
        kind: logic::StackKind,
        budget: &mut Option<usize>,
    ) -> Result<FrameId> {
        if current_root() != self.tree.root {
            return Err(Error::InUse);
        }
        if let Some(left) = budget {
            if *left == 0 {
                return Err(Error::Exhausted);
            }
            *left -= 1;
        }
        let pa = self
            .memory
            .frames
            .allocate()
            .map_err(|_| Error::Exhausted)?;
        let frame = match self.registry.insert_stack_frame(edit, pa, kind) {
            Ok(frame) => frame,
            Err(Error::InUse) => return Err(Error::Corrupt),
            Err(error) => {
                self.memory.frames.free(pa).map_err(|_| Error::Corrupt)?;
                return Err(error);
            }
        };
        if let Err(error) = self.memory.zero(pa) {
            self.free_member_frame(edit, frame)?;
            return Err(error);
        }
        self.registry.initialize(frame)?;
        Ok(frame)
    }
    fn free_member_frame(
        &mut self,
        edit: &neu_kernel::node::StackEdit<'_>,
        frame: FrameId,
    ) -> Result<()> {
        let pa = self.registry.releasable_stack_frame(edit, frame)?;
        self.memory.frames.free(pa).map_err(|_| Error::Corrupt)?;
        self.registry.retire_stack_frame(edit, frame)
    }
    /// params are scalar fixture tag/seed/state offset/action, never kernel IDs.
    pub fn prepare_member(
        &mut self,
        space: &mut UserSpace,
        edit: &neu_kernel::node::StackEdit<'_>,
        params: [u64; 4],
        budget: Option<usize>,
    ) -> Result<neu_kernel::context::StackBinding> {
        self.member_boundary(space)?;
        let a = edit.active();
        let slot = a.thread.index();
        if edit.terminal()
            || a.context.space != space.id
            || a.context.root != space.root
            || slot >= neu_kernel::node::MAX_THREADS
            || a.context.rsp != logic::thread_user_stack(slot) + PAGE - 8
            || space.stacks[slot].is_some()
            || space.user_owners[slot].is_some()
        {
            return Err(Error::NotOwned);
        }
        let user_base = logic::thread_user_stack(slot);
        if user_base != space.user_stack_base && slot != 2 {
            return Err(Error::Address);
        }
        let mut remaining = budget;
        let before = self.stats();
        let tables = self.table_count();
        let old = self.memory.tables.map(|t| t.physical != 0);
        let primary = user_base == space.user_stack_base;
        let old_user = if primary {
            space.frames[3]
        } else {
            space.extra_user
        };
        let user = if let Some(frame) = old_user {
            self.registry.adopt_user_stack(edit, frame)?;
            frame
        } else {
            let frame = self.allocate_member_frame(edit, logic::StackKind::User, &mut remaining)?;
            if primary {
                space.frames[3] = Some(frame);
            } else {
                space.extra_user = Some(frame);
            }
            frame
        };
        space.user_owners[slot] = Some(a.thread);
        let binding = neu_kernel::context::StackBinding::new(a);
        space.stacks[slot] = Some(OwnedStack {
            binding,
            frames: [None; logic::KSTACK_PAGES],
            pinned: false,
        });
        let result = (|| {
            if old_user.is_none() {
                logic::map_thread_stack_frames(
                    &PageTree {
                        root: space.root,
                        bits: self.tree.bits,
                    },
                    &mut self.memory,
                    &mut self.registry,
                    edit,
                    user_base,
                    &[user],
                    logic::StackKind::User,
                )?;
            }
            self.with_frame_mut(user, |bytes| {
                for (index, value) in params.iter().enumerate() {
                    bytes[4088 - index * 8..4096 - index * 8].copy_from_slice(&value.to_le_bytes());
                }
            })?;
            for index in 0..logic::KSTACK_PAGES {
                let frame =
                    self.allocate_member_frame(edit, logic::StackKind::Kernel, &mut remaining)?;
                space.stacks[slot].as_mut().ok_or(Error::Corrupt)?.frames[index] = Some(frame);
                self.with_frame_mut(frame, |bytes| bytes.fill(0xa5))?;
            }
            let frames = space.stacks[slot]
                .as_ref()
                .ok_or(Error::Corrupt)?
                .frames
                .map(|f| f.unwrap());
            logic::map_thread_stack_frames(
                &PageTree {
                    root: space.root,
                    bits: self.tree.bits,
                },
                &mut self.memory,
                &mut self.registry,
                edit,
                binding.bottom,
                &frames,
                logic::StackKind::Kernel,
            )?;
            if budget == Some(3) {
                return Err(Error::Exhausted);
            } // Post-map rollback hook.
            Ok(())
        })();
        for (i, was_owned) in old.iter().enumerate() {
            let added = !*was_owned && self.memory.tables[i].physical != 0;
            space.tables[i] |= added;
            space.stack_tables[i] |= added;
        }
        if let Err(error) = result {
            self.destroy_member(space, edit)?;
            // New-member failure never consumes the primary stack of a live owner.
            if old_user.is_none() && (self.stats() != before || self.table_count() != tables) {
                return Err(Error::Corrupt);
            }
            return Err(error);
        }
        self.verify_member(space, a, binding)?;
        Ok(binding)
    }
    pub fn verify_member(
        &self,
        space: &UserSpace,
        active: neu_kernel::node::ActiveContext,
        binding: neu_kernel::context::StackBinding,
    ) -> Result<()> {
        self.member_boundary(space)?;
        let slot = active.thread.index();
        let base = logic::thread_user_stack(slot);
        if slot >= neu_kernel::node::MAX_THREADS
            || space.user_owners[slot] != Some(active.thread)
            || active.context.rsp != base + PAGE - 8
            || active.context.space != space.id
            || active.context.root != space.root
            || !binding.valid(active)
        {
            return Err(Error::NotOwned);
        }
        let frame = if base == space.user_stack_base {
            space.frames[3]
        } else {
            space.extra_user
        }
        .ok_or(Error::NotOwned)?;
        self.verify_user_stack_leaf(space, frame, base)?;
        self.verify_thread_stack(space, binding)
    }
    pub fn pin_member(
        &mut self,
        space: &mut UserSpace,
        owner: neu_kernel::node::ThreadId,
    ) -> Result<()> {
        self.member_boundary(space)?;
        let stack = space
            .stacks
            .get(owner.index())
            .and_then(Option::as_ref)
            .filter(|s| s.binding.thread == owner && !s.pinned)
            .ok_or(Error::InUse)?;
        self.verify_thread_stack(space, stack.binding)?;
        if space.user_owners[owner.index()] != Some(owner) {
            return Err(Error::NotOwned);
        }
        self.registry.pin_space(space.id)?;
        space.stacks[owner.index()].as_mut().unwrap().pinned = true;
        Ok(())
    }
    pub fn destroy_member(
        &mut self,
        space: &mut UserSpace,
        edit: &neu_kernel::node::StackEdit<'_>,
    ) -> Result<logic::StackRelease> {
        self.member_boundary(space)?;
        let a = edit.active();
        let slot = a.thread.index();
        if a.context.space != space.id
            || a.context.root != space.root
            || space.user_owners.get(slot) != Some(&Some(a.thread))
        {
            return Err(Error::NotOwned);
        }
        let stack = space.stacks[slot]
            .as_ref()
            .filter(|s| s.binding.thread == a.thread)
            .ok_or(Error::NotOwned)?;
        if stack.pinned {
            if !edit.terminal() {
                return Err(Error::InUse);
            }
            self.registry.unpin_space(space.id)?;
        }
        let stack = space.stacks[slot].take().ok_or(Error::NotOwned)?;
        let tree = PageTree {
            root: space.root,
            bits: self.tree.bits,
        };
        for index in 0..logic::KSTACK_PAGES {
            let va = stack.binding.bottom + index as u64 * PAGE;
            if tree.translate(&self.memory, va)?.is_some() {
                if self.registry.mapping_for(space.id, va)?.frame
                    != stack.frames[index].ok_or(Error::Corrupt)?
                {
                    return Err(Error::Corrupt);
                }
                tree.unmap(&mut self.memory, va)?;
                self.registry.remove_stack_mapping(edit, va)?;
            }
        }
        for frame in stack.frames.into_iter().flatten() {
            self.free_member_frame(edit, frame)?;
        }
        let base = logic::thread_user_stack(slot);
        let frame = if base == space.user_stack_base {
            space.frames[3].take()
        } else {
            space.extra_user.take()
        }
        .ok_or(Error::NotOwned)?;
        if tree.translate(&self.memory, base)?.is_some() {
            if self.registry.mapping_for(space.id, base)?.frame != frame {
                return Err(Error::Corrupt);
            }
            tree.unmap(&mut self.memory, base)?;
            self.registry.remove_stack_mapping(edit, base)?;
        }
        self.free_member_frame(edit, frame)?;
        space.user_owners[slot] = None;
        for slot in 0..TABLES {
            space.tables[slot] &= self.memory.tables[slot].physical != 0;
            space.stack_tables[slot] &= self.memory.tables[slot].physical != 0;
        }
        self.verify_user(space)?;
        self.verify()?;
        self.registry.finish_stack_edit(edit)
    }
    pub fn member_pins(&self, space: &UserSpace) -> Result<u8> {
        self.registry.pins(space.id)
    }
    pub fn member_words(&mut self, space: &UserSpace) -> Result<[u64; 32]> {
        self.member_boundary(space)?;
        self.with_frame(space.frames[2].ok_or(Error::NotOwned)?, |bytes| {
            core::array::from_fn(|i| {
                u64::from_le_bytes(bytes[i * 8..i * 8 + 8].try_into().unwrap())
            })
        })
    }
    pub fn member_limit(&mut self, space: &UserSpace, label: u64, limit: u64) -> Result<()> {
        self.member_boundary(space)?;
        if !(1..=4).contains(&label) {
            return Err(Error::Address);
        }
        self.with_frame_mut(space.frames[2].ok_or(Error::NotOwned)?, |bytes| {
            let at = (64 + (label - 1) * 32 + 8) as usize;
            bytes[at..at + 8].copy_from_slice(&limit.to_le_bytes());
        })
    }
    pub fn member_init_state(&mut self, space: &UserSpace, label: u64) -> Result<()> {
        self.member_boundary(space)?;
        if !(1..=4).contains(&label) {
            return Err(Error::Address);
        }
        self.with_frame_mut(space.frames[2].ok_or(Error::NotOwned)?, |bytes| {
            let at = (64 + (label - 1) * 32) as usize;
            bytes[at..at + 32].fill(0);
            bytes[at + 8..at + 16].copy_from_slice(&u64::MAX.to_le_bytes());
        })
    }
}
#[cfg(feature = "multi-test")]
impl Paging<'_, '_> {
    pub fn member_done_flag(&mut self, space: &UserSpace) -> Result<()> {
        self.member_boundary(space)?;
        self.with_frame_mut(space.frames[2].ok_or(Error::NotOwned)?, |bytes| {
            bytes[32..40].copy_from_slice(&1u64.to_le_bytes())
        })
    }
}

#[cfg(any(feature = "cap-test", feature = "abi-02"))]
impl Paging<'_, '_> {
    pub fn member_cap_words(&mut self, space: &UserSpace) -> Result<[u64; 4]> {
        self.member_boundary(space)?;
        self.with_frame(space.frames[2].ok_or(Error::NotOwned)?, |bytes| {
            core::array::from_fn(|i| {
                u64::from_le_bytes(bytes[256 + i * 8..264 + i * 8].try_into().unwrap())
            })
        })
    }
    #[cfg(feature = "cap-test")]
    pub fn member_cap_write(&mut self, space: &UserSpace, index: usize, value: u64) -> Result<()> {
        self.member_boundary(space)?;
        if index >= 4 {
            return Err(Error::Address);
        }
        self.with_frame_mut(space.frames[2].ok_or(Error::NotOwned)?, |bytes| {
            bytes[256 + index * 8..264 + index * 8].copy_from_slice(&value.to_le_bytes())
        })
    }
}

/// A shared borrow freezes structural Paging mutation until the native slice
/// ends. Only verified, supervisor aliases for this root may be read. Hardware
/// A/D writes are allowed; no Rust references to table entry storage exist.
#[cfg(feature = "abi-02")]
pub struct LiveInspection<'p> {
    registry: &'p Registry,
    tables: &'p [TableOwner; TABLES],
    visible: [bool; TABLES],
    active: neu_kernel::node::ActiveContext,
    bits: u8,
}
#[cfg(feature = "abi-02")]
impl LiveInspection<'_> {
    pub fn inspect(
        &self,
        active: neu_kernel::node::ActiveContext,
        owner: neu_kernel::node::NodeId,
        reference: &logic::SpaceRef,
        va: u64,
    ) -> core::result::Result<u64, i64> {
        if !neu_kernel::inspection::scope_matches(
            self.active,
            active,
            owner,
            reference,
            self.registry,
            current_root(),
        ) {
            return Err(neu_kernel::capability::Error::Owner as i64);
        }
        logic::user_range(va, 1).map_err(|_| neu_kernel::syscall::INVALID_ARGUMENT)?;
        neu_kernel::inspection::inspect(self.registry, reference, self, self.bits, va)
            .map_err(|_| neu_kernel::syscall::v02::BAD_MAPPING)
    }
}
#[cfg(feature = "abi-02")]
impl TableMemory for LiveInspection<'_> {
    fn allocate(&mut self, _: u8) -> Result<u64> {
        Err(Error::InUse)
    }
    fn release(&mut self, _: u64) -> Result<()> {
        Err(Error::InUse)
    }
    fn owned(&self, pa: u64, level: u8) -> bool {
        self.tables
            .iter()
            .enumerate()
            .any(|(i, t)| self.visible[i] && pa != 0 && t.physical == pa && t.level == level)
    }
    fn pinned(&self, _: u64) -> bool {
        true
    }
    fn read(&self, pa: u64, index: usize) -> Result<u64> {
        if current_root() != self.active.context.root || index >= 512 {
            return Err(Error::InUse);
        }
        let slot = self
            .tables
            .iter()
            .enumerate()
            .position(|(i, t)| self.visible[i] && pa != 0 && t.physical == pa)
            .ok_or(Error::NotOwned)?;
        // SAFETY: activate_user verified these exact owned aliases immediately
        // before this borrow. Shared Paging borrow forbids replacement/release;
        // current CR3 and bounds checked here. IF=0, IRQ never reads this view.
        Ok(unsafe { ptr::read_volatile(((WINDOW + slot as u64 * PAGE) as *const u64).add(index)) })
    }
    fn write(&mut self, _: u64, _: usize, _: u64) -> Result<()> {
        Err(Error::InUse)
    }
    fn invalidate(&mut self, _: u64) {}
    fn flush(&mut self) {}
}
#[cfg(feature = "abi-02")]
impl Paging<'_, '_> {
    pub fn inspection_view<'p>(
        &'p self,
        space: &UserSpace,
        active: neu_kernel::node::ActiveContext,
    ) -> Result<LiveInspection<'p>> {
        if self.registry.active_space() != Some(space.id)
            || self.registry.root(space.id)? != space.root
            || current_root() != space.root
            || active.context.space != space.id
            || active.context.root != space.root
        {
            return Err(Error::InUse);
        }
        Ok(LiveInspection {
            registry: &self.registry,
            tables: &self.memory.tables,
            visible: core::array::from_fn(|i| space.tables[i] && !space.stack_tables[i]),
            active,
            bits: self.tree.bits,
        })
    }
    pub fn retain_object_space(&mut self, space: &UserSpace) -> Result<logic::SpaceRef> {
        self.registry.retain_space(space.id)
    }
    pub fn release_object_space(&mut self, reference: logic::SpaceRef) -> Result<()> {
        self.registry
            .release_space_ref(reference)
            .map_err(|(e, _)| e)
    }
}

#[cfg(feature = "channel-test")]
impl Paging<'_, '_> {
    pub fn channel_control(&mut self, space: &UserSpace, epoch: u64, gates: u64) -> Result<()> {
        self.member_boundary(space)?;
        self.with_frame_mut(space.frames[2].ok_or(Error::NotOwned)?, |bytes| {
            bytes[264..272].copy_from_slice(&epoch.to_le_bytes());
            bytes[336..344].copy_from_slice(&gates.to_le_bytes());
        })
    }
}

#[cfg(feature = "memory-object-test")]
#[path = "memory_object.rs"]
mod memory_object;
#[cfg(feature = "memory-object-live-test")]
#[path = "memory_object_live.rs"]
pub(crate) mod memory_object_live;

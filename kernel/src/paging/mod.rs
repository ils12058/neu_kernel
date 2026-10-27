//! Pure x86-64 paging logic. CPU access and physical-frame storage are supplied
//! by a backend; host tests exercise the same table transactions as the kernel.

pub const PAGE: u64 = 4096;
pub const TEST_BASE: u64 = 0xffff_a000_0000_0000;
pub const TEST_SIZE: u64 = 4 * 1024 * 1024;
pub const USER_BASE: u64 = 0x0040_0000;
pub const USER_SIZE: u64 = 4 * 1024 * 1024;
pub const ADDRESS_FIELD: u64 = 0x000f_ffff_ffff_f000;
pub const NX: u64 = 1 << 63;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Alignment,
    Address,
    Physical,
    Flags,
    Wx,
    AlreadyMapped,
    NotMapped,
    Capacity,
    Exhausted,
    NotOwned,
    InUse,
    Uninitialized,
    Corrupt,
}
pub type Result<T> = core::result::Result<T, Error>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct Permissions {
    pub write: bool,
    pub execute: bool,
}
impl Permissions {
    pub const R: Self = Self {
        write: false,
        execute: false,
    };
    pub const RW: Self = Self {
        write: true,
        execute: false,
    };
    pub const RX: Self = Self {
        write: false,
        execute: true,
    };
    pub fn validate(self) -> Result<()> {
        if self.write && self.execute {
            Err(Error::Wx)
        } else {
            Ok(())
        }
    }
}

pub fn canonical(address: u64) -> bool {
    (((address << 16) as i64 >> 16) as u64) == address
}
pub fn indices(address: u64) -> Result<[usize; 4]> {
    if !canonical(address) {
        return Err(Error::Address);
    }
    Ok([39, 30, 21, 12].map(|shift| ((address >> shift) & 511) as usize))
}
pub fn physical(address: u64, bits: u8) -> Result<()> {
    if !(32..=52).contains(&bits) || address >= (1u64 << bits) {
        return Err(Error::Physical);
    }
    if !address.is_multiple_of(PAGE) {
        return Err(Error::Alignment);
    }
    Ok(())
}
pub fn page_address(address: u64) -> Result<()> {
    indices(address)?;
    if !address.is_multiple_of(PAGE) {
        return Err(Error::Alignment);
    }
    Ok(())
}
pub fn test_range(base: u64, pages: usize) -> Result<()> {
    page_address(base)?;
    let length = (pages as u64).checked_mul(PAGE).ok_or(Error::Address)?;
    let end = base.checked_add(length).ok_or(Error::Address)?;
    if pages == 0 || base < TEST_BASE || end > TEST_BASE + TEST_SIZE {
        return Err(Error::Address);
    }
    Ok(())
}
/// Only the bounded lower-half fixture range is available in Phase 2A.
pub fn user_range(base: u64, pages: usize) -> Result<()> {
    page_address(base)?;
    let length = (pages as u64).checked_mul(PAGE).ok_or(Error::Address)?;
    let end = base.checked_add(length).ok_or(Error::Address)?;
    if pages == 0 || base < USER_BASE || end > USER_BASE + USER_SIZE {
        return Err(Error::Address);
    }
    Ok(())
}
pub const KSTACK_BASE: u64 = 0xffff_c000_0000_0000;
pub const KSTACK_STRIDE: u64 = 4 * PAGE;
pub const KSTACK_PAGES: usize = 2;
pub fn stack_range(base: u64, pages: usize) -> Result<()> {
    page_address(base)?;
    if pages != KSTACK_PAGES
        || !(0..crate::node::MAX_THREADS)
            .any(|slot| base == KSTACK_BASE + slot as u64 * KSTACK_STRIDE + PAGE)
    {
        return Err(Error::Address);
    }
    Ok(())
}
pub const USER_STACK_BASE: u64 = 0x404000;
pub const USER_STACK_STRIDE: u64 = 4 * PAGE;
pub fn thread_user_stack(slot: usize) -> u64 {
    USER_STACK_BASE + slot as u64 * USER_STACK_STRIDE
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StackKind {
    User,
    Kernel,
}
pub fn table_entry(address: u64, bits: u8) -> Result<u64> {
    physical(address, bits)?;
    Ok(address | 3)
}
pub fn leaf_entry(address: u64, permissions: Permissions, bits: u8) -> Result<u64> {
    physical(address, bits)?;
    permissions.validate()?;
    Ok(address
        | 1
        | if permissions.write { 2 } else { 0 }
        | if permissions.execute { 0 } else { NX })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Translation {
    pub physical: u64,
    pub permissions: Permissions,
    pub user: bool,
    pub page_size: u64,
}

/// Inspect existing entries, including bootloader 1 GiB/2 MiB leaves and PAT.
/// New tables emitted by this module use only 4 KiB supervisor leaves.
pub fn decode(raw: u64, level: u8, bits: u8) -> Result<Option<(u64, u64)>> {
    if raw & 1 == 0 {
        return Ok(None);
    }
    if !(1..=4).contains(&level) {
        return Err(Error::Corrupt);
    }
    let address = raw & ADDRESS_FIELD;
    physical(address, bits)?;
    if raw & !(ADDRESS_FIELD | NX | 0xfff) != 0 {
        return Err(Error::Flags);
    }
    let size = if level == 1 {
        PAGE
    } else if raw & 128 != 0 {
        match level {
            2 => 1 << 21,
            3 => 1 << 30,
            _ => return Err(Error::Flags),
        }
    } else {
        0
    };
    if size > PAGE && address & (size - 1) & !0x1000 != 0 {
        return Err(Error::Alignment);
    }
    Ok(Some((
        address & !if size > PAGE { size - 1 } else { 0 },
        size,
    )))
}

/// Backends allocate zeroed, exclusively owned table frames. Writes/reads never
/// borrow an unowned table. A failed write must not mutate its entry.
/// flush() completes before any detached table release.
pub trait TableMemory {
    fn allocate(&mut self, level: u8) -> Result<u64>;
    fn release(&mut self, address: u64) -> Result<()>;
    fn owned(&self, address: u64, level: u8) -> bool;
    fn pinned(&self, address: u64) -> bool;
    fn read(&self, table: u64, index: usize) -> Result<u64>;
    fn write(&mut self, table: u64, index: usize, value: u64) -> Result<()>;
    fn invalidate(&mut self, address: u64);
    fn flush(&mut self);
}

pub struct PageTree {
    pub root: u64,
    pub bits: u8,
}
impl PageTree {
    /// Count distinct absent intermediate tables without mutating the tree.
    /// Bounded to a MemoryObject transaction (at most two pages).
    pub fn required_user_tables(
        &self,
        memory: &impl TableMemory,
        base: u64,
        pages: usize,
    ) -> Result<usize> {
        user_range(base, pages)?;
        if pages > MEMORY_OBJECT_PAGES {
            return Err(Error::Capacity);
        }
        let mut missing = [(0u8, 0u64); 6];
        let mut count = 0;
        for i in 0..pages {
            let address = base + i as u64 * PAGE;
            let path = indices(address)?;
            let mut table = Some(self.root);
            for (depth, index) in path[..3].iter().copied().enumerate() {
                if let Some(current) = table {
                    if !memory.owned(current, 4 - depth as u8) {
                        return Err(Error::NotOwned);
                    }
                    if let Some((next, size)) =
                        decode(memory.read(current, index)?, 4 - depth as u8, self.bits)?
                    {
                        if size != 0 {
                            return Err(Error::AlreadyMapped);
                        }
                        table = Some(next);
                        continue;
                    }
                }
                let key = (depth as u8, address >> (39 - depth * 9));
                if !missing[..count].contains(&key) {
                    missing[count] = key;
                    count += 1;
                }
                table = None;
            }
        }
        Ok(count)
    }
    pub fn new(memory: &mut impl TableMemory, bits: u8) -> Result<Self> {
        physical(0, bits)?;
        Ok(Self {
            root: memory.allocate(4)?,
            bits,
        })
    }

    pub fn translate(
        &self,
        memory: &impl TableMemory,
        virtual_address: u64,
    ) -> Result<Option<Translation>> {
        let path = indices(virtual_address)?;
        let mut table = self.root;
        let (mut writable, mut executable, mut user) = (true, true, true);
        for (depth, index) in path.into_iter().enumerate() {
            let level = 4 - depth as u8;
            if !memory.owned(table, level) {
                return Err(Error::NotOwned);
            }
            let entry = memory.read(table, index)?;
            let Some((address, size)) = decode(entry, level, self.bits)? else {
                return Ok(None);
            };
            writable &= entry & 2 != 0;
            executable &= entry & NX == 0;
            user &= entry & 4 != 0;
            if size != 0 {
                return Ok(Some(Translation {
                    physical: address + (virtual_address & (size - 1)),
                    permissions: Permissions {
                        write: writable,
                        execute: executable,
                    },
                    user,
                    page_size: size,
                }));
            }
            table = address;
        }
        Err(Error::Corrupt)
    }

    pub fn map(
        &self,
        memory: &mut impl TableMemory,
        va: u64,
        pa: u64,
        permissions: Permissions,
    ) -> Result<()> {
        self.map_kind(memory, va, pa, permissions, false, 0)
    }

    pub fn map_user(
        &self,
        memory: &mut impl TableMemory,
        va: u64,
        pa: u64,
        permissions: Permissions,
    ) -> Result<()> {
        user_range(va, 1)?;
        self.map_kind(memory, va, pa, permissions, true, 0)
    }

    /// Device-only supervisor leaf. PAT index 3 MUST be verified as UC first.
    /// This does not allocate or transfer ownership of the device's physical page.
    pub fn map_device_uc(
        &self,
        memory: &mut impl TableMemory,
        va: u64,
        pa: u64,
        pat: u64,
    ) -> Result<()> {
        if (pat >> 24) & 0xff != 0 {
            return Err(Error::Flags);
        }
        self.map_kind(memory, va, pa, Permissions::RW, false, 0x18)
    }

    fn map_kind(
        &self,
        memory: &mut impl TableMemory,
        va: u64,
        pa: u64,
        permissions: Permissions,
        user: bool,
        cache: u64,
    ) -> Result<()> {
        page_address(va)?;
        let user_bit = if user { 4 } else { 0 };
        let leaf = leaf_entry(pa, permissions, self.bits)? | user_bit | cache;
        let path = indices(va)?;
        let mut table = self.root;
        let mut created = [(0u64, 0usize, 0u64); 3];
        let mut count = 0;
        let outcome = (|| {
            for (depth, index) in path[..3].iter().copied().enumerate() {
                let level = 4 - depth as u8;
                if !memory.owned(table, level) {
                    return Err(Error::NotOwned);
                }
                let entry = memory.read(table, index)?;
                table = if let Some((address, size)) = decode(entry, level, self.bits)? {
                    if size != 0 {
                        return Err(Error::AlreadyMapped);
                    }
                    if entry & 4 != user_bit
                        || (permissions.write && entry & 2 == 0)
                        || (permissions.execute && entry & NX != 0)
                    {
                        return Err(Error::Flags);
                    }
                    if !memory.owned(address, level - 1) {
                        return Err(Error::NotOwned);
                    }
                    address
                } else {
                    let child = memory.allocate(level - 1)?;
                    let encoded = match table_entry(child, self.bits) {
                        Ok(value) => value | user_bit,
                        Err(error) => {
                            memory
                                .release(child)
                                .expect("failed to release unlinked table");
                            return Err(error);
                        }
                    };
                    if let Err(error) = memory.write(table, index, encoded) {
                        memory
                            .release(child)
                            .expect("failed to release unlinked table");
                        return Err(error);
                    }
                    created[count] = (table, index, child);
                    count += 1;
                    child
                };
            }
            if memory.read(table, path[3])? & 1 != 0 {
                return Err(Error::AlreadyMapped);
            }
            memory.write(table, path[3], leaf)?;
            memory.invalidate(va);
            Ok(())
        })();
        if outcome.is_err() {
            for (parent, index, child) in created[..count].iter().copied().rev() {
                memory
                    .write(parent, index, 0)
                    .expect("page-table map rollback detach failed");
                memory.flush();
                memory
                    .release(child)
                    .expect("page-table map rollback release failed");
            }
        }
        outcome
    }

    pub fn unmap(&self, memory: &mut impl TableMemory, va: u64) -> Result<Translation> {
        page_address(va)?;
        let mapping = self.translate(memory, va)?.ok_or(Error::NotMapped)?;
        if mapping.page_size != PAGE {
            return Err(Error::Flags);
        }
        let path = indices(va)?;
        let mut tables = [self.root; 4];
        for depth in 0..3 {
            tables[depth + 1] = memory.read(tables[depth], path[depth])? & ADDRESS_FIELD;
        }
        memory.write(tables[3], path[3], 0)?;
        memory.invalidate(va);
        // After the leaf is removed, a backend failure cannot be reported as a
        // retryable unmap. Fail-stop rather than leave untracked table state.
        let cleanup: Result<()> = (|| {
            for depth in (1..4).rev() {
                let table = tables[depth];
                if memory.pinned(table) {
                    break;
                }
                let mut empty = true;
                for index in 0..512 {
                    if memory.read(table, index)? & 1 != 0 {
                        empty = false;
                        break;
                    }
                }
                if !empty {
                    break;
                }
                memory.write(tables[depth - 1], path[depth - 1], 0)?;
                memory.flush(); // Parent detached and walk caches invalidated before reuse.
                memory.release(table)?;
            }
            Ok(())
        })();
        cleanup.expect("page-table unmap cleanup failed after leaf removal");
        Ok(mapping)
    }

    /// Remove only the leaf; the caller remains responsible for every table.
    /// This is used during whole-root teardown, whose table inventory and
    /// workspace aliases must stay intact until that teardown owns them.
    pub fn unmap_leaf_preserving_tables(
        &self,
        memory: &mut impl TableMemory,
        va: u64,
    ) -> Result<Translation> {
        page_address(va)?;
        let mapping = self.translate(memory, va)?.ok_or(Error::NotMapped)?;
        if mapping.page_size != PAGE {
            return Err(Error::Flags);
        }
        let path = indices(va)?;
        let mut table = self.root;
        for &index in &path[..3] {
            table = memory.read(table, index)? & ADDRESS_FIELD;
        }
        memory.write(table, path[3], 0)?;
        memory.invalidate(va);
        Ok(mapping)
    }
}

pub const DATA_FRAMES: usize = 16;
pub const MAPPINGS: usize = 64;
pub const USER_SPACES: usize = 2;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct SpaceId {
    slot: usize,
    generation: u64,
}
impl SpaceId {
    pub const KERNEL: Self = Self {
        slot: usize::MAX,
        generation: 0,
    };
    pub fn raw(self) -> u64 {
        if self == Self::KERNEL {
            0
        } else {
            (self.generation << 8) | (self.slot as u64 + 1)
        }
    }
}
/// Owned reference to a generation-checked root. Separate from execution pins.
#[must_use = "release the space reference at an inactive common-root boundary"]
#[derive(Debug)]
pub struct SpaceRef {
    id: SpaceId,
    root: u64,
}
impl SpaceRef {
    pub fn id(&self) -> SpaceId {
        self.id
    }
    pub fn root(&self) -> u64 {
        self.root
    }
}
#[derive(Clone, Copy)]
#[repr(C)]
struct Space {
    generation: u64,
    live: bool,
    root: u64,
    pins: u8,
    object_refs: u8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct FrameId {
    slot: usize,
    generation: u64,
}
/// Physical backing has exactly one owner; a mapping is never an owner.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C, u64)]
pub enum FrameOwner {
    Space(SpaceId),
    MemoryObject(MemoryObjectId),
}

pub const MEMORY_OBJECTS: usize = 4;
pub const MEMORY_OBJECT_PAGES: usize = 2;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct MemoryObjectId {
    slot: usize,
    generation: u64,
}
#[derive(Clone, Copy)]
#[repr(C)]
struct MemoryObject {
    generation: u64,
    live: bool,
    creator: bool,
    frames: [FrameId; MEMORY_OBJECT_PAGES],
    pages: usize,
}
/// Trusted internal setup authority. Its fields cannot be supplied by userspace;
/// it never grants access to a different generation, space or stronger rights.
pub struct MemoryMapAuthority {
    object: MemoryObjectId,
    space: SpaceId,
    writable: bool,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct Frame {
    physical: u64,
    generation: u64,
    live: bool,
    initialized: bool,
    owner: FrameOwner,
    stack_owner: Option<(crate::node::ThreadId, StackKind)>,
}
#[derive(Clone, Copy)]
#[repr(C)]
pub struct Mapping {
    pub space: SpaceId,
    pub address: u64,
    pub frame: FrameId,
    pub permissions: Permissions,
    pub object: MappingObject,
    live: bool,
}
/// Explicit provenance also gives the test-only debugger a defined raw layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C, u64)]
pub enum MappingObject {
    Private,
    Shared(MemoryObjectId),
}
impl FrameId {
    const EMPTY: Self = Self {
        slot: 0,
        generation: 0,
    };
}

/// Completion receipt is minted only after this generation has no tagged
/// backing or scoped mappings. Architecture code cannot forge its fields.
pub struct StackRelease {
    active: crate::node::ActiveContext,
}
impl StackRelease {
    pub(crate) fn matches(&self, active: crate::node::ActiveContext) -> bool {
        self.active == active
    }
}
pub struct Registry {
    frames: [Frame; DATA_FRAMES],
    mappings: [Mapping; MAPPINGS],
    objects: [MemoryObject; MEMORY_OBJECTS],
    spaces: [Space; USER_SPACES],
    active: Option<SpaceId>,
}
impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}
impl Registry {
    /// Test-only schema: bases, strides and offsets point to production fields.
    /// No data values are projected, and no address authorizes a kernel action.
    #[cfg(feature = "memory-object-test")]
    pub fn diagnostic_layout(&self) -> [u64; 38] {
        use core::mem::{offset_of as off, size_of as size};
        [
            1,
            self.frames.as_ptr() as u64,
            size::<Frame>() as u64,
            DATA_FRAMES as u64,
            off!(Frame, physical) as u64,
            off!(Frame, generation) as u64,
            off!(Frame, live) as u64,
            off!(Frame, initialized) as u64,
            off!(Frame, owner) as u64,
            self.mappings.as_ptr() as u64,
            size::<Mapping>() as u64,
            MAPPINGS as u64,
            off!(Mapping, space) as u64,
            off!(Mapping, address) as u64,
            off!(Mapping, frame) as u64,
            off!(Mapping, permissions) as u64,
            off!(Mapping, object) as u64,
            off!(Mapping, live) as u64,
            self.objects.as_ptr() as u64,
            size::<MemoryObject>() as u64,
            MEMORY_OBJECTS as u64,
            off!(MemoryObject, generation) as u64,
            off!(MemoryObject, live) as u64,
            off!(MemoryObject, creator) as u64,
            off!(MemoryObject, frames) as u64,
            off!(MemoryObject, pages) as u64,
            self.spaces.as_ptr() as u64,
            size::<Space>() as u64,
            USER_SPACES as u64,
            off!(Space, generation) as u64,
            off!(Space, live) as u64,
            off!(Space, root) as u64,
            off!(Space, pins) as u64,
            off!(Space, object_refs) as u64,
            size::<FrameId>() as u64,
            size::<SpaceId>() as u64,
            size::<MappingObject>() as u64,
            size::<FrameOwner>() as u64,
        ]
    }
    pub const fn new() -> Self {
        Self {
            frames: [Frame {
                physical: 0,
                generation: 0,
                live: false,
                initialized: false,
                owner: FrameOwner::Space(SpaceId::KERNEL),
                stack_owner: None,
            }; DATA_FRAMES],
            mappings: [Mapping {
                space: SpaceId::KERNEL,
                address: 0,
                frame: FrameId {
                    slot: 0,
                    generation: 0,
                },
                permissions: Permissions::R,
                object: MappingObject::Private,
                live: false,
            }; MAPPINGS],
            objects: [MemoryObject {
                generation: 0,
                live: false,
                creator: false,
                frames: [FrameId::EMPTY; MEMORY_OBJECT_PAGES],
                pages: 0,
            }; MEMORY_OBJECTS],
            spaces: [Space {
                generation: 0,
                live: false,
                root: 0,
                pins: 0,
                object_refs: 0,
            }; USER_SPACES],
            active: None,
        }
    }
    pub fn create_space(&mut self) -> Result<SpaceId> {
        if self.active.is_some() {
            return Err(Error::InUse);
        }
        let slot = self
            .spaces
            .iter()
            .position(|s| !s.live && s.generation < (u64::MAX >> 8))
            .ok_or(Error::Capacity)?;
        let generation = self.spaces[slot].generation + 1;
        self.spaces[slot] = Space {
            generation,
            live: true,
            root: 0,
            pins: 0,
            object_refs: 0,
        };
        Ok(SpaceId { slot, generation })
    }
    fn space(&self, id: SpaceId) -> Result<&Space> {
        self.spaces
            .get(id.slot)
            .filter(|s| s.live && s.generation == id.generation)
            .ok_or(Error::NotOwned)
    }
    fn valid_space(&self, id: SpaceId) -> Result<()> {
        if id == SpaceId::KERNEL {
            Ok(())
        } else {
            self.space(id).map(|_| ())
        }
    }
    pub fn can_allocate(&self, id: SpaceId) -> Result<()> {
        self.valid_space(id)?;
        if self.active.is_some() || (id != SpaceId::KERNEL && self.space(id)?.pins != 0) {
            Err(Error::InUse)
        } else {
            Ok(())
        }
    }
    pub fn retain_space(&mut self, id: SpaceId) -> Result<SpaceRef> {
        let space = self.space(id)?;
        if self.active.is_some() || space.root == 0 {
            return Err(Error::InUse);
        }
        if space.object_refs == 2 {
            return Err(Error::Capacity);
        }
        let root = space.root;
        self.spaces[id.slot].object_refs += 1;
        Ok(SpaceRef { id, root })
    }
    pub fn validate_space_ref(&self, reference: &SpaceRef) -> Result<()> {
        let s = self.space(reference.id)?;
        if s.root != reference.root || s.root == 0 || s.object_refs == 0 {
            return Err(Error::NotOwned);
        }
        Ok(())
    }
    pub fn release_space_ref(
        &mut self,
        reference: SpaceRef,
    ) -> core::result::Result<(), (Error, SpaceRef)> {
        if let Err(e) = self.validate_space_ref(&reference) {
            return Err((e, reference));
        }
        if self.active.is_some() {
            return Err((Error::InUse, reference));
        }
        self.spaces[reference.id.slot].object_refs -= 1;
        Ok(())
    }
    pub fn object_references(&self, id: SpaceId) -> Result<u8> {
        Ok(self.space(id)?.object_refs)
    }
    pub fn can_destroy_space(&self, id: SpaceId) -> Result<()> {
        self.can_allocate(id)?;
        if self.space(id)?.object_refs != 0 {
            return Err(Error::InUse);
        }
        Ok(())
    }
    pub fn active_space(&self) -> Option<SpaceId> {
        self.active
    }
    pub fn pin_space(&mut self, id: SpaceId) -> Result<()> {
        let space = self.space(id)?;
        if self.active.is_some() || space.root == 0 {
            return Err(Error::InUse);
        }
        if space.pins == 2 {
            return Err(Error::Capacity);
        }
        self.spaces[id.slot].pins += 1;
        Ok(())
    }
    pub fn unpin_space(&mut self, id: SpaceId) -> Result<()> {
        let space = self.space(id)?;
        if self.active.is_some() || space.pins == 0 {
            return Err(Error::InUse);
        }
        self.spaces[id.slot].pins -= 1;
        Ok(())
    }
    pub fn pins(&self, id: SpaceId) -> Result<u8> {
        Ok(self.space(id)?.pins)
    }
    fn stack_edit_valid(&self, edit: &crate::node::StackEdit<'_>) -> Result<()> {
        let active = edit.active();
        if self.active.is_some()
            || active.thread.index() >= crate::node::MAX_THREADS
            || active.context.rsp != thread_user_stack(active.thread.index()) + PAGE - 8
            || self.root(active.context.space)? != active.context.root
        {
            return Err(Error::InUse);
        }
        Ok(())
    }
    pub fn insert_stack_frame(
        &mut self,
        edit: &crate::node::StackEdit<'_>,
        address: u64,
        kind: StackKind,
    ) -> Result<FrameId> {
        self.stack_edit_valid(edit)?;
        if edit.terminal() {
            return Err(Error::InUse);
        }
        physical(address, 52)?;
        if self.frames.iter().any(|f| f.live && f.physical == address) {
            return Err(Error::InUse);
        }
        let slot = self
            .frames
            .iter()
            .position(|f| !f.live && f.generation != u64::MAX)
            .ok_or(Error::Capacity)?;
        let generation = self.frames[slot].generation + 1;
        self.frames[slot] = Frame {
            physical: address,
            generation,
            live: true,
            initialized: false,
            owner: FrameOwner::Space(edit.active().context.space),
            stack_owner: Some((edit.active().thread, kind)),
        };
        Ok(FrameId { slot, generation })
    }
    pub fn adopt_user_stack(
        &mut self,
        edit: &crate::node::StackEdit<'_>,
        id: FrameId,
    ) -> Result<()> {
        self.stack_edit_valid(edit)?;
        let a = edit.active();
        let f = self.frame(id)?;
        if edit.terminal()
            || f.owner != FrameOwner::Space(a.context.space)
            || f.stack_owner.is_some()
            || self
                .mappings
                .iter()
                .filter(|m| m.live && m.frame == id)
                .count()
                != 1
            || self
                .mapping_for(a.context.space, thread_user_stack(a.thread.index()))?
                .frame
                != id
        {
            return Err(Error::NotOwned);
        }
        self.frames[id.slot].stack_owner = Some((a.thread, StackKind::User));
        Ok(())
    }
    fn stack_frame_valid(
        &self,
        edit: &crate::node::StackEdit<'_>,
        id: FrameId,
    ) -> Result<StackKind> {
        self.stack_edit_valid(edit)?;
        let f = self.frame(id)?;
        let a = edit.active();
        match f.stack_owner {
            Some((owner, kind))
                if owner == a.thread && f.owner == FrameOwner::Space(a.context.space) =>
            {
                Ok(kind)
            }
            _ => Err(Error::NotOwned),
        }
    }
    pub fn remove_stack_mapping(
        &mut self,
        edit: &crate::node::StackEdit<'_>,
        address: u64,
    ) -> Result<FrameId> {
        let a = edit.active();
        let m = self.mapping_for(a.context.space, address)?;
        let kind = self.stack_frame_valid(edit, m.frame)?;
        let base = match kind {
            StackKind::User => thread_user_stack(a.thread.index()),
            StackKind::Kernel => KSTACK_BASE + a.thread.index() as u64 * KSTACK_STRIDE + PAGE,
        };
        let size = match kind {
            StackKind::User => PAGE,
            StackKind::Kernel => KSTACK_PAGES as u64 * PAGE,
        };
        if !(base..base + size).contains(&address) {
            return Err(Error::Address);
        }
        let slot = self
            .mappings
            .iter()
            .position(|record| {
                record.live && record.space == a.context.space && record.address == address
            })
            .ok_or(Error::NotMapped)?;
        self.mappings[slot].live = false;
        Ok(m.frame)
    }
    pub fn releasable_stack_frame(
        &self,
        edit: &crate::node::StackEdit<'_>,
        id: FrameId,
    ) -> Result<u64> {
        self.stack_frame_valid(edit, id)?;
        if self.mappings.iter().any(|m| m.live && m.frame == id) {
            return Err(Error::InUse);
        }
        Ok(self.frame(id)?.physical)
    }
    pub fn retire_stack_frame(
        &mut self,
        edit: &crate::node::StackEdit<'_>,
        id: FrameId,
    ) -> Result<()> {
        self.releasable_stack_frame(edit, id)?;
        self.frames[id.slot].live = false;
        Ok(())
    }
    pub fn finish_stack_edit(&self, edit: &crate::node::StackEdit<'_>) -> Result<StackRelease> {
        self.stack_edit_valid(edit)?;
        let a = edit.active();
        let user = thread_user_stack(a.thread.index());
        let kernel = KSTACK_BASE + a.thread.index() as u64 * KSTACK_STRIDE + PAGE;
        if self
            .frames
            .iter()
            .any(|f| f.live && f.stack_owner.is_some_and(|(owner, _)| owner == a.thread))
            || self.mappings.iter().any(|m| {
                m.live
                    && m.space == a.context.space
                    && (m.address == user
                        || (kernel..kernel + KSTACK_PAGES as u64 * PAGE).contains(&m.address))
            })
        {
            return Err(Error::InUse);
        }
        Ok(StackRelease { active: a })
    }
    pub fn bind_root(&mut self, id: SpaceId, root: u64) -> Result<()> {
        self.space(id)?;
        physical(root, 52)?;
        if root == 0
            || self.spaces[id.slot].root != 0
            || self.spaces.iter().any(|s| s.live && s.root == root)
        {
            return Err(Error::InUse);
        }
        self.spaces[id.slot].root = root;
        Ok(())
    }
    pub fn root(&self, id: SpaceId) -> Result<u64> {
        Ok(self.space(id)?.root)
    }
    pub fn activate_space(&mut self, id: SpaceId) -> Result<()> {
        if self.root(id)? == 0 || self.active.is_some() {
            return Err(Error::InUse);
        }
        self.active = Some(id);
        Ok(())
    }
    pub fn deactivate_space(&mut self, id: SpaceId) -> Result<()> {
        self.space(id)?;
        if self.active != Some(id) {
            return Err(Error::NotOwned);
        }
        self.active = None;
        Ok(())
    }
    pub fn clear_root(&mut self, id: SpaceId, root: u64) -> Result<()> {
        if self.root(id)? != root
            || self.space(id)?.object_refs != 0
            || self.space(id)?.pins != 0
            || self.active == Some(id)
            || self
                .frames
                .iter()
                .any(|f| f.live && f.owner == FrameOwner::Space(id))
            || self.mappings.iter().any(|m| m.live && m.space == id)
        {
            return Err(Error::InUse);
        }
        self.spaces[id.slot].root = 0;
        Ok(())
    }
    pub fn retire_space(&mut self, id: SpaceId) -> Result<()> {
        self.space(id)?;
        if self.active == Some(id)
            || self.space(id)?.object_refs != 0
            || self.space(id)?.pins != 0
            || self.spaces[id.slot].root != 0
            || self
                .frames
                .iter()
                .any(|f| f.live && f.owner == FrameOwner::Space(id))
            || self.mappings.iter().any(|m| m.live && m.space == id)
        {
            return Err(Error::InUse);
        }
        self.spaces[id.slot].live = false;
        Ok(())
    }
    pub fn insert(&mut self, physical: u64) -> Result<FrameId> {
        self.insert_for(SpaceId::KERNEL, physical)
    }
    pub fn insert_for(&mut self, owner: SpaceId, address: u64) -> Result<FrameId> {
        self.can_allocate(owner)?;
        physical(address, 52)?;
        if self.frames.iter().any(|f| f.live && f.physical == address) {
            return Err(Error::InUse);
        }
        let slot = self
            .frames
            .iter()
            .position(|f| !f.live && f.generation != u64::MAX)
            .ok_or(Error::Capacity)?;
        let generation = self.frames[slot].generation + 1;
        self.frames[slot] = Frame {
            physical: address,
            generation,
            live: true,
            initialized: false,
            owner: FrameOwner::Space(owner),
            stack_owner: None,
        };
        Ok(FrameId { slot, generation })
    }
    fn frame(&self, id: FrameId) -> Result<&Frame> {
        let frame = self
            .frames
            .get(id.slot)
            .filter(|f| f.live && f.generation == id.generation)
            .ok_or(Error::NotOwned)?;
        match frame.owner {
            FrameOwner::Space(owner) => self.valid_space(owner)?,
            FrameOwner::MemoryObject(id) => {
                self.object(id)?;
            }
        }
        Ok(frame)
    }
    fn object(&self, id: MemoryObjectId) -> Result<&MemoryObject> {
        self.objects
            .get(id.slot)
            .filter(|o| o.live && o.generation == id.generation)
            .ok_or(Error::NotOwned)
    }

    /// Trusted kernel setup transfers exclusively owned, initialized kernel
    /// frames to a new object. On failure no ownership changes. The caller
    /// must allocate and zero these frames and free them if construction fails.
    /// This method is not a userspace object factory or authorization boundary.
    pub fn create_memory_object(&mut self, frames: &[FrameId]) -> Result<MemoryObjectId> {
        self.can_allocate(SpaceId::KERNEL)?;
        if frames.is_empty() || frames.len() > MEMORY_OBJECT_PAGES {
            return Err(Error::Address);
        }
        let slot = self
            .objects
            .iter()
            .position(|o| !o.live && o.generation != u64::MAX)
            .ok_or(Error::Capacity)?;
        for (i, id) in frames.iter().copied().enumerate() {
            let frame = self.frame(id)?;
            if frame.owner != FrameOwner::Space(SpaceId::KERNEL)
                || frame.stack_owner.is_some()
                || frames[..i].contains(&id)
            {
                return Err(Error::NotOwned);
            }
            if !frame.initialized {
                return Err(Error::Uninitialized);
            }
            self.releasable(id)?;
        }
        let id = MemoryObjectId {
            slot,
            generation: self.objects[slot].generation + 1,
        };
        let mut backing = [FrameId::EMPTY; MEMORY_OBJECT_PAGES];
        for (i, frame) in frames.iter().copied().enumerate() {
            backing[i] = frame;
        }
        self.objects[slot] = MemoryObject {
            generation: id.generation,
            live: true,
            creator: true,
            frames: backing,
            pages: frames.len(),
        };
        for frame in frames {
            self.frames[frame.slot].owner = FrameOwner::MemoryObject(id);
        }
        Ok(id)
    }

    /// Trusted bootstrap only: the caller has already decided which object and
    /// destination may be paired. This is not exposed across a syscall boundary.
    pub fn authorize_memory_mapping(
        &self,
        id: MemoryObjectId,
        space: SpaceId,
        writable: bool,
    ) -> Result<MemoryMapAuthority> {
        if !self.object(id)?.creator {
            return Err(Error::NotOwned);
        }
        self.space(space)?;
        Ok(MemoryMapAuthority {
            object: id,
            space,
            writable,
        })
    }
    pub fn memory_object_counts(&self, id: MemoryObjectId) -> Result<(usize, usize, usize)> {
        let object = self.object(id)?;
        Ok((
            usize::from(object.creator),
            self.mappings
                .iter()
                .filter(|m| m.live && m.object == MappingObject::Shared(id))
                .count(),
            object.pages,
        ))
    }
    /// Teardown must detach every object mapping while its tree is still live.
    pub fn has_memory_object_mappings(&self, space: SpaceId) -> Result<bool> {
        self.space(space)?;
        Ok(self
            .mappings
            .iter()
            .any(|m| m.live && m.space == space && matches!(m.object, MappingObject::Shared(_))))
    }
    pub fn memory_object_mappings(
        &self,
        space: SpaceId,
    ) -> Result<impl Iterator<Item = Mapping> + '_> {
        self.space(space)?;
        Ok(self.mappings.iter().copied().filter(move |m| {
            m.live && m.space == space && matches!(m.object, MappingObject::Shared(_))
        }))
    }
    pub fn memory_object_frame(&self, id: MemoryObjectId, index: usize) -> Result<FrameId> {
        let object = self.object(id)?;
        if index >= object.pages {
            return Err(Error::Address);
        }
        Ok(object.frames[index])
    }
    pub fn frame_owner(&self, id: FrameId) -> Result<FrameOwner> {
        Ok(self.frame(id)?.owner)
    }
    pub fn release_memory_object(&mut self, id: MemoryObjectId) -> Result<()> {
        if !self.object(id)?.creator {
            return Err(Error::NotOwned);
        }
        self.objects[id.slot].creator = false;
        Ok(())
    }
    /// Return backing to trusted kernel ownership, without freeing it. The
    /// caller must free every returned frame exactly once before reuse. All
    /// validation occurs before any ownership changes.
    pub fn collect_memory_object(
        &mut self,
        id: MemoryObjectId,
    ) -> Result<[Option<FrameId>; MEMORY_OBJECT_PAGES]> {
        let frames = self.collectible_memory_object(id)?;
        for frame in frames.iter().flatten() {
            self.frames[frame.slot].owner = FrameOwner::Space(SpaceId::KERNEL);
        }
        self.objects[id.slot].live = false;
        Ok(frames)
    }

    /// Non-mutating first stage for a physical allocator batch preflight.
    pub fn collectible_memory_object(
        &self,
        id: MemoryObjectId,
    ) -> Result<[Option<FrameId>; MEMORY_OBJECT_PAGES]> {
        self.can_allocate(SpaceId::KERNEL)?;
        let object = *self.object(id)?;
        if object.creator
            || self
                .mappings
                .iter()
                .any(|m| m.live && m.object == MappingObject::Shared(id))
        {
            return Err(Error::InUse);
        }
        for frame in &object.frames[..object.pages] {
            if self.frame(*frame)?.owner != FrameOwner::MemoryObject(id)
                || self.mappings.iter().any(|m| m.live && m.frame == *frame)
            {
                return Err(Error::Corrupt);
            }
        }
        Ok(core::array::from_fn(|i| {
            if i < object.pages {
                Some(object.frames[i])
            } else {
                None
            }
        }))
    }
    pub fn physical(&self, id: FrameId) -> Result<u64> {
        Ok(self.frame(id)?.physical)
    }
    pub fn owner(&self, id: FrameId) -> Result<SpaceId> {
        match self.frame(id)?.owner {
            FrameOwner::Space(owner) => Ok(owner),
            FrameOwner::MemoryObject(_) => Err(Error::NotOwned),
        }
    }
    pub fn initialize(&mut self, id: FrameId) -> Result<()> {
        self.frame(id)?;
        self.frames[id.slot].initialized = true;
        Ok(())
    }
    pub fn writable(&self, id: FrameId) -> Result<()> {
        self.frame(id)?;
        if self
            .mappings
            .iter()
            .any(|m| m.live && m.frame == id && m.permissions.execute)
        {
            Err(Error::Wx)
        } else {
            Ok(())
        }
    }
    pub fn releasable(&self, id: FrameId) -> Result<u64> {
        let frame = self.frame(id)?;
        let owner = match frame.owner {
            FrameOwner::Space(owner) => owner,
            FrameOwner::MemoryObject(_) => return Err(Error::InUse),
        };
        if self.active == Some(owner)
            || (owner != SpaceId::KERNEL && self.space(owner)?.pins != 0)
            || self.mappings.iter().any(|m| m.live && m.frame == id)
        {
            return Err(Error::InUse);
        }
        Ok(frame.physical)
    }
    pub fn retire(&mut self, id: FrameId) -> Result<()> {
        self.releasable(id)?;
        self.frames[id.slot].live = false;
        Ok(())
    }
    pub fn prepare(&self, base: u64, frames: &[FrameId], permissions: Permissions) -> Result<()> {
        self.prepare_kind(Target::KERNEL, base, frames, permissions)
    }
    pub fn prepare_user(
        &self,
        base: u64,
        frames: &[FrameId],
        permissions: Permissions,
    ) -> Result<()> {
        self.prepare_kind(
            Target {
                space: SpaceId::KERNEL,
                user: true,
                stack: false,
                thread: None,
            },
            base,
            frames,
            permissions,
        )
    }
    fn prepare_kind(
        &self,
        target: Target,
        base: u64,
        frames: &[FrameId],
        permissions: Permissions,
    ) -> Result<()> {
        if let Some(owner) = target.thread {
            self.space(target.space)?;
            if self.active.is_some() || permissions != Permissions::RW {
                return Err(Error::InUse);
            }
            let expected = if target.user {
                thread_user_stack(owner.index())
            } else {
                KSTACK_BASE + owner.index() as u64 * KSTACK_STRIDE + PAGE
            };
            if base != expected || frames.len() != if target.user { 1 } else { KSTACK_PAGES } {
                return Err(Error::Address);
            }
        } else {
            self.can_allocate(target.space)?;
        }
        if target.stack {
            stack_range(base, frames.len())?;
        } else if target.user {
            user_range(base, frames.len())?;
        } else {
            test_range(base, frames.len())?;
        }
        permissions.validate()?;
        if self.mappings.iter().filter(|m| !m.live).count() < frames.len() {
            return Err(Error::Capacity);
        }
        for (index, id) in frames.iter().copied().enumerate() {
            let frame = self.frame(id)?;
            if frame.owner != FrameOwner::Space(target.space)
                || target.thread.is_some_and(|owner| {
                    frame.stack_owner
                        != Some((
                            owner,
                            if target.user {
                                StackKind::User
                            } else {
                                StackKind::Kernel
                            },
                        ))
                        || frames[..index].contains(&id)
                })
            {
                return Err(Error::NotOwned);
            }
            if !frame.initialized {
                return Err(Error::Uninitialized);
            }
            let address = base + index as u64 * PAGE;
            if self.mapping_for(target.space, address).is_ok() {
                return Err(Error::AlreadyMapped);
            }
            if self.mappings.iter().any(|m| {
                m.live
                    && m.frame == id
                    && ((permissions.write && m.permissions.execute)
                        || (permissions.execute && m.permissions.write))
            }) {
                return Err(Error::Wx);
            }
        }
        Ok(())
    }
    pub fn commit(
        &mut self,
        base: u64,
        frames: &[FrameId],
        permissions: Permissions,
    ) -> Result<()> {
        self.commit_kind(Target::KERNEL, base, frames, permissions)
    }
    pub fn commit_user(
        &mut self,
        base: u64,
        frames: &[FrameId],
        permissions: Permissions,
    ) -> Result<()> {
        self.commit_kind(
            Target {
                space: SpaceId::KERNEL,
                user: true,
                stack: false,
                thread: None,
            },
            base,
            frames,
            permissions,
        )
    }
    fn commit_kind(
        &mut self,
        target: Target,
        base: u64,
        frames: &[FrameId],
        permissions: Permissions,
    ) -> Result<()> {
        self.prepare_kind(target, base, frames, permissions)?;
        for (index, frame) in frames.iter().copied().enumerate() {
            let slot = self
                .mappings
                .iter()
                .position(|m| !m.live)
                .ok_or(Error::Corrupt)?;
            self.mappings[slot] = Mapping {
                space: target.space,
                address: base + index as u64 * PAGE,
                frame,
                permissions,
                object: MappingObject::Private,
                live: true,
            };
        }
        Ok(())
    }
    pub fn mapping(&self, address: u64) -> Result<Mapping> {
        self.mapping_for(SpaceId::KERNEL, address)
    }
    pub fn mapping_for(&self, space: SpaceId, address: u64) -> Result<Mapping> {
        self.valid_space(space)?;
        self.mappings
            .iter()
            .copied()
            .find(|m| m.live && m.space == space && m.address == address)
            .ok_or(Error::NotMapped)
    }
    pub fn remove(&mut self, address: u64) -> Result<FrameId> {
        self.remove_for(SpaceId::KERNEL, address)
    }
    pub fn remove_for(&mut self, space: SpaceId, address: u64) -> Result<FrameId> {
        self.valid_space(space)?;
        if self.active == Some(space) || (space != SpaceId::KERNEL && self.space(space)?.pins != 0)
        {
            return Err(Error::InUse);
        }
        let slot = self
            .mappings
            .iter()
            .position(|m| m.live && m.space == space && m.address == address)
            .ok_or(Error::NotMapped)?;
        if matches!(self.mappings[slot].object, MappingObject::Shared(_)) {
            return Err(Error::NotOwned);
        }
        self.mappings[slot].live = false;
        Ok(self.mappings[slot].frame)
    }
}
#[derive(Clone, Copy)]
struct Target {
    space: SpaceId,
    user: bool,
    stack: bool,
    thread: Option<crate::node::ThreadId>,
}
impl Target {
    const KERNEL: Self = Self {
        space: SpaceId::KERNEL,
        user: false,
        stack: false,
        thread: None,
    };
}

/// Default wrappers address one kernel/simulated tree. Runtime user roots must
/// use a registered, generation-checked space key via map_user_frames_for.
pub fn map_frames(
    tree: &PageTree,
    memory: &mut impl TableMemory,
    registry: &mut Registry,
    base: u64,
    frames: &[FrameId],
    permissions: Permissions,
) -> Result<()> {
    map_frames_kind(
        tree,
        memory,
        registry,
        base,
        frames,
        permissions,
        Target::KERNEL,
    )
}
pub fn map_user_frames(
    tree: &PageTree,
    memory: &mut impl TableMemory,
    registry: &mut Registry,
    base: u64,
    frames: &[FrameId],
    permissions: Permissions,
) -> Result<()> {
    map_user_frames_for(
        tree,
        memory,
        registry,
        SpaceId::KERNEL,
        base,
        frames,
        permissions,
    )
}
pub fn map_user_frames_for(
    tree: &PageTree,
    memory: &mut impl TableMemory,
    registry: &mut Registry,
    space: SpaceId,
    base: u64,
    frames: &[FrameId],
    permissions: Permissions,
) -> Result<()> {
    if space != SpaceId::KERNEL && registry.root(space)? != tree.root {
        return Err(Error::NotOwned);
    }
    map_frames_kind(
        tree,
        memory,
        registry,
        base,
        frames,
        permissions,
        Target {
            space,
            user: true,
            stack: false,
            thread: None,
        },
    )
}
pub fn map_stack_frames_for(
    tree: &PageTree,
    memory: &mut impl TableMemory,
    registry: &mut Registry,
    space: SpaceId,
    base: u64,
    frames: &[FrameId],
) -> Result<()> {
    if registry.root(space)? != tree.root {
        return Err(Error::NotOwned);
    }
    map_frames_kind(
        tree,
        memory,
        registry,
        base,
        frames,
        Permissions::RW,
        Target {
            space,
            user: false,
            stack: true,
            thread: None,
        },
    )
}
pub fn map_thread_stack_frames(
    tree: &PageTree,
    memory: &mut impl TableMemory,
    registry: &mut Registry,
    edit: &crate::node::StackEdit<'_>,
    base: u64,
    frames: &[FrameId],
    kind: StackKind,
) -> Result<()> {
    registry.stack_edit_valid(edit)?;
    if edit.terminal() || tree.root != edit.active().context.root {
        return Err(Error::InUse);
    }
    map_frames_kind(
        tree,
        memory,
        registry,
        base,
        frames,
        Permissions::RW,
        Target {
            space: edit.active().context.space,
            user: kind == StackKind::User,
            stack: kind == StackKind::Kernel,
            thread: Some(edit.active().thread),
        },
    )
}
fn map_frames_kind(
    tree: &PageTree,
    memory: &mut impl TableMemory,
    registry: &mut Registry,
    base: u64,
    frames: &[FrameId],
    permissions: Permissions,
    target: Target,
) -> Result<()> {
    registry.prepare_kind(target, base, frames, permissions)?;
    for (index, frame) in frames.iter().copied().enumerate() {
        let va = base + index as u64 * PAGE;
        let pa = registry.physical(frame)?;
        let result = if target.user {
            tree.map_user(memory, va, pa, permissions)
        } else {
            tree.map(memory, va, pa, permissions)
        };
        if let Err(error) = result {
            for previous in (0..index).rev() {
                tree.unmap(memory, base + previous as u64 * PAGE)
                    .expect("multi-page mapping rollback failed");
            }
            return Err(error);
        }
    }
    registry.commit_kind(target, base, frames, permissions)
}

/// Map under a trusted, generation-checked object-and-space grant. Mapping
/// records are published only after every leaf has been installed. In the
/// supported single-CPU non-preemptible transaction nothing observes the
/// intermediate leaves. The caller must use the inactive, unpinned root.
#[allow(clippy::too_many_arguments)] // Explicit backend, authority and mapping bounds.
pub fn map_memory_object(
    tree: &PageTree,
    memory: &mut impl TableMemory,
    registry: &mut Registry,
    authority: &MemoryMapAuthority,
    offset: usize,
    base: u64,
    pages: usize,
    permissions: Permissions,
) -> Result<()> {
    let object = *registry.object(authority.object)?;
    if !object.creator {
        return Err(Error::NotOwned);
    }
    registry.can_allocate(authority.space)?;
    if registry.root(authority.space)? != tree.root {
        return Err(Error::NotOwned);
    }
    permissions.validate()?;
    if permissions.execute || (permissions.write && !authority.writable) {
        return Err(Error::Flags);
    }
    user_range(base, pages)?;
    let end = offset.checked_add(pages).ok_or(Error::Address)?;
    if end > object.pages {
        return Err(Error::Address);
    }
    if registry.mappings.iter().filter(|m| !m.live).count() < pages {
        return Err(Error::Capacity);
    }
    for i in 0..pages {
        let id = object.frames[offset + i];
        let frame = registry.frame(id)?;
        if frame.owner != FrameOwner::MemoryObject(authority.object) {
            return Err(Error::NotOwned);
        }
        if !frame.initialized {
            return Err(Error::Uninitialized);
        }
        if registry
            .mapping_for(authority.space, base + i as u64 * PAGE)
            .is_ok()
        {
            return Err(Error::AlreadyMapped);
        }
        if registry
            .mappings
            .iter()
            .any(|m| m.live && m.frame == id && m.permissions.execute)
        {
            return Err(Error::Wx);
        }
    }
    for i in 0..pages {
        let frame = object.frames[offset + i];
        if let Err(error) = tree.map_user(
            memory,
            base + i as u64 * PAGE,
            registry.physical(frame)?,
            permissions,
        ) {
            for previous in (0..i).rev() {
                tree.unmap(memory, base + previous as u64 * PAGE)
                    .expect("memory-object mapping rollback failed");
            }
            return Err(error);
        }
    }
    for i in 0..pages {
        let slot = registry
            .mappings
            .iter()
            .position(|m| !m.live)
            .ok_or(Error::Corrupt)?;
        registry.mappings[slot] = Mapping {
            space: authority.space,
            address: base + i as u64 * PAGE,
            frame: object.frames[offset + i],
            permissions,
            object: MappingObject::Shared(authority.object),
            live: true,
        };
    }
    Ok(())
}

/// Remove exactly one recorded leaf after validating its tree and backing.
pub fn unmap_memory_object(
    tree: &PageTree,
    memory: &mut impl TableMemory,
    registry: &mut Registry,
    space: SpaceId,
    address: u64,
) -> Result<()> {
    validate_memory_object_mapping(tree, memory, registry, space, address)?;
    tree.unmap(memory, address)?;
    retire_memory_object_mapping(registry, space, address)
}

pub fn validate_memory_object_mapping(
    tree: &PageTree,
    memory: &impl TableMemory,
    registry: &Registry,
    space: SpaceId,
    address: u64,
) -> Result<()> {
    registry.can_allocate(space)?;
    if registry.root(space)? != tree.root {
        return Err(Error::NotOwned);
    }
    let record = registry.mapping_for(space, address)?;
    let MappingObject::Shared(id) = record.object else {
        return Err(Error::NotOwned);
    };
    if registry.frame(record.frame)?.owner != FrameOwner::MemoryObject(id) {
        return Err(Error::Corrupt);
    }
    let actual = tree.translate(memory, address)?.ok_or(Error::NotMapped)?;
    if actual.physical != registry.physical(record.frame)?
        || actual.page_size != PAGE
        || !actual.user
        || actual.permissions != record.permissions
    {
        return Err(Error::Corrupt);
    }
    Ok(())
}

fn retire_memory_object_mapping(
    registry: &mut Registry,
    space: SpaceId,
    address: u64,
) -> Result<()> {
    let slot = registry
        .mappings
        .iter()
        .position(|m| {
            m.live
                && m.space == space
                && m.address == address
                && matches!(m.object, MappingObject::Shared(_))
        })
        .ok_or(Error::Corrupt)?;
    registry.mappings[slot].live = false;
    Ok(())
}

/// Teardown variant: preflight is repeated, then the mapping reference is
/// retired only after its leaf was removed and locally invalidated.
pub fn unmap_memory_object_preserving_tables(
    tree: &PageTree,
    memory: &mut impl TableMemory,
    registry: &mut Registry,
    space: SpaceId,
    address: u64,
) -> Result<()> {
    validate_memory_object_mapping(tree, memory, registry, space, address)?;
    tree.unmap_leaf_preserving_tables(memory, address)?;
    retire_memory_object_mapping(registry, space, address)
}

#[cfg(test)]
mod tests;

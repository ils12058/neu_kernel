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
/// borrow an unowned table. flush() completes before any detached table release.
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
        self.map_kind(memory, va, pa, permissions, false)
    }

    pub fn map_user(
        &self,
        memory: &mut impl TableMemory,
        va: u64,
        pa: u64,
        permissions: Permissions,
    ) -> Result<()> {
        user_range(va, 1)?;
        self.map_kind(memory, va, pa, permissions, true)
    }

    fn map_kind(
        &self,
        memory: &mut impl TableMemory,
        va: u64,
        pa: u64,
        permissions: Permissions,
        user: bool,
    ) -> Result<()> {
        page_address(va)?;
        let user_bit = if user { 4 } else { 0 };
        let leaf = leaf_entry(pa, permissions, self.bits)? | user_bit;
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
                            memory.release(child)?;
                            return Err(error);
                        }
                    };
                    if let Err(error) = memory.write(table, index, encoded) {
                        memory.release(child)?;
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
                memory.write(parent, index, 0)?;
                memory.flush();
                memory.release(child)?;
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
        Ok(mapping)
    }
}

pub const DATA_FRAMES: usize = 16;
pub const MAPPINGS: usize = 64;
pub const USER_SPACES: usize = 2;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
#[derive(Clone, Copy)]
struct Space {
    generation: u64,
    live: bool,
    root: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameId {
    slot: usize,
    generation: u64,
}
#[derive(Clone, Copy)]
struct Frame {
    physical: u64,
    generation: u64,
    live: bool,
    initialized: bool,
    owner: SpaceId,
}
#[derive(Clone, Copy)]
pub struct Mapping {
    pub space: SpaceId,
    pub address: u64,
    pub frame: FrameId,
    pub permissions: Permissions,
    live: bool,
}
pub struct Registry {
    frames: [Frame; DATA_FRAMES],
    mappings: [Mapping; MAPPINGS],
    spaces: [Space; USER_SPACES],
    active: Option<SpaceId>,
}
impl Default for Registry {
    fn default() -> Self {
        Self::new()
    }
}
impl Registry {
    pub const fn new() -> Self {
        Self {
            frames: [Frame {
                physical: 0,
                generation: 0,
                live: false,
                initialized: false,
                owner: SpaceId::KERNEL,
            }; DATA_FRAMES],
            mappings: [Mapping {
                space: SpaceId::KERNEL,
                address: 0,
                frame: FrameId {
                    slot: 0,
                    generation: 0,
                },
                permissions: Permissions::R,
                live: false,
            }; MAPPINGS],
            spaces: [Space {
                generation: 0,
                live: false,
                root: 0,
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
        if self.active.is_some() {
            Err(Error::InUse)
        } else {
            Ok(())
        }
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
            || self.active == Some(id)
            || self.frames.iter().any(|f| f.live && f.owner == id)
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
            || self.spaces[id.slot].root != 0
            || self.frames.iter().any(|f| f.live && f.owner == id)
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
            owner,
        };
        Ok(FrameId { slot, generation })
    }
    fn frame(&self, id: FrameId) -> Result<&Frame> {
        let frame = self
            .frames
            .get(id.slot)
            .filter(|f| f.live && f.generation == id.generation)
            .ok_or(Error::NotOwned)?;
        self.valid_space(frame.owner)?;
        Ok(frame)
    }
    pub fn physical(&self, id: FrameId) -> Result<u64> {
        Ok(self.frame(id)?.physical)
    }
    pub fn owner(&self, id: FrameId) -> Result<SpaceId> {
        Ok(self.frame(id)?.owner)
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
        if self.active == Some(frame.owner) || self.mappings.iter().any(|m| m.live && m.frame == id)
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
        self.can_allocate(target.space)?;
        if target.user {
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
            if frame.owner != target.space {
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
        if self.active == Some(space) {
            return Err(Error::InUse);
        }
        let slot = self
            .mappings
            .iter()
            .position(|m| m.live && m.space == space && m.address == address)
            .ok_or(Error::NotMapped)?;
        self.mappings[slot].live = false;
        Ok(self.mappings[slot].frame)
    }
}
#[derive(Clone, Copy)]
struct Target {
    space: SpaceId,
    user: bool,
}
impl Target {
    const KERNEL: Self = Self {
        space: SpaceId::KERNEL,
        user: false,
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
        Target { space, user: true },
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
                tree.unmap(memory, base + previous as u64 * PAGE)?;
            }
            return Err(error);
        }
    }
    registry.commit_kind(target, base, frames, permissions)
}

#[cfg(test)]
mod tests;

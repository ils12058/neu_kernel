//! Pure x86-64 paging logic. CPU access and physical-frame storage are supplied
//! by a backend; host tests exercise the same table transactions as the kernel.

pub const PAGE: u64 = 4096;
pub const TEST_BASE: u64 = 0xffff_a000_0000_0000;
pub const TEST_SIZE: u64 = 4 * 1024 * 1024;
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
        page_address(va)?;
        let leaf = leaf_entry(pa, permissions, self.bits)?;
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
                    if entry & 4 != 0
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
                        Ok(value) => value,
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
}
#[derive(Clone, Copy)]
pub struct Mapping {
    pub address: u64,
    pub frame: FrameId,
    pub permissions: Permissions,
    live: bool,
}
pub struct Registry {
    frames: [Frame; DATA_FRAMES],
    mappings: [Mapping; MAPPINGS],
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
            }; DATA_FRAMES],
            mappings: [Mapping {
                address: 0,
                frame: FrameId {
                    slot: 0,
                    generation: 0,
                },
                permissions: Permissions::R,
                live: false,
            }; MAPPINGS],
        }
    }
    pub fn insert(&mut self, physical: u64) -> Result<FrameId> {
        self::physical(physical, 52)?;
        if self
            .frames
            .iter()
            .any(|frame| frame.live && frame.physical == physical)
        {
            return Err(Error::InUse);
        }
        let slot = self
            .frames
            .iter()
            .position(|f| !f.live && f.generation != u64::MAX)
            .ok_or(Error::Capacity)?;
        let generation = self.frames[slot].generation + 1;
        self.frames[slot] = Frame {
            physical,
            generation,
            live: true,
            initialized: false,
        };
        Ok(FrameId { slot, generation })
    }
    fn frame(&self, id: FrameId) -> Result<&Frame> {
        self.frames
            .get(id.slot)
            .filter(|f| f.live && f.generation == id.generation)
            .ok_or(Error::NotOwned)
    }
    pub fn physical(&self, id: FrameId) -> Result<u64> {
        Ok(self.frame(id)?.physical)
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
            return Err(Error::Wx);
        }
        Ok(())
    }
    pub fn releasable(&self, id: FrameId) -> Result<u64> {
        let address = self.physical(id)?;
        if self.mappings.iter().any(|m| m.live && m.frame == id) {
            return Err(Error::InUse);
        }
        Ok(address)
    }
    pub fn retire(&mut self, id: FrameId) -> Result<()> {
        self.releasable(id)?;
        self.frames[id.slot].live = false;
        Ok(())
    }
    pub fn prepare(&self, base: u64, frames: &[FrameId], permissions: Permissions) -> Result<()> {
        test_range(base, frames.len())?;
        permissions.validate()?;
        if self.mappings.iter().filter(|m| !m.live).count() < frames.len() {
            return Err(Error::Capacity);
        }
        for (index, id) in frames.iter().copied().enumerate() {
            if !self.frame(id)?.initialized {
                return Err(Error::Uninitialized);
            }
            let address = base + index as u64 * PAGE;
            if self.mapping(address).is_ok() {
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
    /// Called only after a successful prepare and completed page-table transaction.
    pub fn commit(
        &mut self,
        base: u64,
        frames: &[FrameId],
        permissions: Permissions,
    ) -> Result<()> {
        self.prepare(base, frames, permissions)?;
        for (index, frame) in frames.iter().copied().enumerate() {
            let slot = self
                .mappings
                .iter()
                .position(|m| !m.live)
                .ok_or(Error::Corrupt)?;
            self.mappings[slot] = Mapping {
                address: base + index as u64 * PAGE,
                frame,
                permissions,
                live: true,
            };
        }
        Ok(())
    }
    pub fn mapping(&self, address: u64) -> Result<Mapping> {
        self.mappings
            .iter()
            .copied()
            .find(|m| m.live && m.address == address)
            .ok_or(Error::NotMapped)
    }
    pub fn remove(&mut self, address: u64) -> Result<FrameId> {
        let slot = self
            .mappings
            .iter()
            .position(|m| m.live && m.address == address)
            .ok_or(Error::NotMapped)?;
        self.mappings[slot].live = false;
        Ok(self.mappings[slot].frame)
    }
}

/// Multi-page rollback is shared with the hardware manager. Backing ownership
/// and mapping references are committed only after every leaf is installed.
pub fn map_frames(
    tree: &PageTree,
    memory: &mut impl TableMemory,
    registry: &mut Registry,
    base: u64,
    frames: &[FrameId],
    permissions: Permissions,
) -> Result<()> {
    registry.prepare(base, frames, permissions)?;
    for (index, frame) in frames.iter().copied().enumerate() {
        if let Err(error) = tree.map(
            memory,
            base + index as u64 * PAGE,
            registry.physical(frame)?,
            permissions,
        ) {
            for previous in (0..index).rev() {
                tree.unmap(memory, base + previous as u64 * PAGE)?;
            }
            return Err(error);
        }
    }
    registry.commit(base, frames, permissions)
}

#[cfg(test)]
mod tests;

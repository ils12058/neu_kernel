//! Physical ownership only. Addresses returned here are never virtual pointers.
//! Exclusive &mut access serializes bookkeeping; the BSP keeps IF=0. No reclaim,
//! frame-content access, zeroing, or mapping operation exists in this module.

pub const PAGE_SIZE: u64 = 4096;
pub const MAX_REGIONS: usize = 4096;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Region {
    pub base: u64,
    pub length: u64,
    pub kind: u64,
}

impl Region {
    pub const EMPTY: Self = Self {
        base: 0,
        length: 0,
        kind: 1,
    };
}

#[derive(Clone, Copy, Debug)]
pub struct Reservation {
    pub start: u64,
    pub end: u64,
}

impl Reservation {
    pub fn new(start: u64, length: u64) -> Result<Self, &'static str> {
        let end = start
            .checked_add(length)
            .ok_or("kernel reservation overflow")?;
        if length == 0 || (start | length) & (PAGE_SIZE - 1) != 0 {
            return Err("kernel reservation is empty or unaligned");
        }
        Ok(Self { start, end })
    }
}

pub struct MemoryMap<'a> {
    regions: &'a [Region],
    kernel: Reservation,
    usable_bytes: u64,
}

impl<'a> MemoryMap<'a> {
    pub fn new(regions: &'a [Region], kernel: Reservation) -> Result<Self, &'static str> {
        if regions.is_empty() || regions.len() > MAX_REGIONS {
            return Err("invalid or excessive memory-map entry count");
        }
        // Reservation fields are public for diagnostics; revalidate at the boundary.
        Reservation::new(
            kernel.start,
            kernel
                .end
                .checked_sub(kernel.start)
                .ok_or("invalid kernel reservation")?,
        )?;
        let (mut previous_base, mut all_end, mut reusable_end, mut usable_bytes) = (0, 0, 0, 0u64);
        for region in regions {
            let end = region
                .base
                .checked_add(region.length)
                .ok_or("memory-map range overflow")?;
            if region.length == 0 || region.base < previous_base {
                return Err("empty or unsorted memory-map region");
            }
            let reusable = region.kind == 0 || region.kind == 5;
            if reusable && (region.base | region.length) & (PAGE_SIZE - 1) != 0 {
                return Err("usable/reclaimable memory is not page aligned");
            }
            // Non-usable entries can overlap each other. Usable/reclaimable
            // entries must not overlap any entry (Limine base 6 contract).
            if region.base < reusable_end || (reusable && region.base < all_end) {
                return Err("usable/reclaimable memory overlaps another region");
            }
            if reusable {
                reusable_end = end;
            }
            if region.kind == 0 {
                usable_bytes = usable_bytes
                    .checked_add(region.length)
                    .ok_or("usable RAM total overflow")?;
            }
            previous_base = region.base;
            all_end = all_end.max(end);
        }
        if usable_bytes == 0 {
            return Err("no usable RAM reported");
        }
        Ok(Self {
            regions,
            kernel,
            usable_bytes,
        })
    }

    pub fn regions(&self) -> &[Region] {
        self.regions
    }
    pub fn kernel(&self) -> Reservation {
        self.kernel
    }
    pub fn usable_bytes(&self) -> u64 {
        self.usable_bytes
    }

    /// Dense ordinals cover usable spans only, even if physical addresses are
    /// sparse/high. Page zero and the actual kernel image are always excluded.
    fn spans(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        self.regions
            .iter()
            .filter(|region| region.kind == 0)
            .flat_map(|region| {
                let start = region.base.max(PAGE_SIZE);
                let end = region.base + region.length; // Validated at construction.
                [
                    (start, end.min(self.kernel.start)),
                    (start.max(self.kernel.end), end),
                ]
                .into_iter()
                .filter(|(start, end)| start < end)
            })
    }

    pub fn allocatable_frames(&self) -> usize {
        self.spans()
            .map(|(start, end)| ((end - start) / PAGE_SIZE) as usize)
            .sum()
    }

    pub fn is_allocatable(&self, address: u64) -> bool {
        address.is_multiple_of(PAGE_SIZE)
            && self
                .spans()
                .any(|(start, end)| (start..end).contains(&address))
    }

    /// Used only by the read-only paging inspection in the #PF test. No generic
    /// direct-map access is authorized by frame allocation or by this predicate.
    pub fn contains_region(&self, start: u64, length: u64, kind: u64) -> bool {
        start.checked_add(length).is_some_and(|end| {
            self.regions
                .iter()
                .any(|r| r.kind == kind && start >= r.base && end <= r.base + r.length)
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum FrameError {
    MetadataCapacity,
    Exhausted,
    Misaligned,
    NotAllocatable,
    NotAllocated,
    Accounting,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameStats {
    pub total: usize,
    pub allocated: usize,
    pub free: usize,
}

pub struct FrameAllocator<'a> {
    map: &'a MemoryMap<'a>,
    allocated: &'a mut [u64],
    total: usize,
    free: usize,
    hint: usize,
}

impl<'a> FrameAllocator<'a> {
    pub fn new(map: &'a MemoryMap<'a>, bitmap: &'a mut [u64]) -> Result<Self, FrameError> {
        let total = map.allocatable_frames();
        if bitmap
            .len()
            .checked_mul(64)
            .is_none_or(|capacity| total > capacity)
        {
            return Err(FrameError::MetadataCapacity);
        }
        bitmap.fill(0);
        Ok(Self {
            map,
            allocated: bitmap,
            total,
            free: total,
            hint: 0,
        })
    }

    pub fn stats(&self) -> FrameStats {
        FrameStats {
            total: self.total,
            allocated: self.total - self.free,
            free: self.free,
        }
    }

    fn address(&self, mut ordinal: usize) -> Option<u64> {
        for (start, end) in self.map.spans() {
            let count = ((end - start) / PAGE_SIZE) as usize;
            if ordinal < count {
                return Some(start + ordinal as u64 * PAGE_SIZE);
            }
            ordinal -= count;
        }
        None
    }

    fn ordinal(&self, address: u64) -> Option<usize> {
        let mut ordinal = 0;
        for (start, end) in self.map.spans() {
            if (start..end).contains(&address) {
                return Some(ordinal + ((address - start) / PAGE_SIZE) as usize);
            }
            ordinal += ((end - start) / PAGE_SIZE) as usize;
        }
        None
    }

    pub fn allocate(&mut self) -> Result<u64, FrameError> {
        if self.free == 0 {
            return Err(FrameError::Exhausted);
        }
        for index in (self.hint..self.total).chain(0..self.hint) {
            let mask = 1u64 << (index % 64);
            if self.allocated[index / 64] & mask == 0 {
                let address = self.address(index).ok_or(FrameError::Accounting)?;
                self.allocated[index / 64] |= mask;
                self.free -= 1;
                self.hint = (index + 1) % self.total;
                return Ok(address);
            }
        }
        Err(FrameError::Accounting)
    }

    pub fn free(&mut self, address: u64) -> Result<(), FrameError> {
        if !address.is_multiple_of(PAGE_SIZE) {
            return Err(FrameError::Misaligned);
        }
        let index = self.ordinal(address).ok_or(FrameError::NotAllocatable)?;
        let mask = 1u64 << (index % 64);
        if self.allocated[index / 64] & mask == 0 {
            return Err(FrameError::NotAllocated);
        }
        self.allocated[index / 64] &= !mask;
        self.free += 1;
        self.hint = self.hint.min(index);
        Ok(())
    }
}

#[cfg(feature = "test-memory")]
pub mod self_test;
#[cfg(test)]
mod tests;

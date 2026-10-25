//! QEMU checks over the actual boot map plus a bounded synthetic exhaustion pool.
use super::*;

pub fn run(map: &MemoryMap<'_>, allocator: &mut FrameAllocator<'_>) -> Result<(), &'static str> {
    let before = allocator.stats();
    let mut held = [0u64; 64];
    for index in 0..held.len() {
        let address = allocator
            .allocate()
            .map_err(|_| "real frame allocation failed")?;
        if !map.is_allocatable(address) || held[..index].contains(&address) {
            return Err("duplicate or reserved real frame allocated");
        }
        held[index] = address;
    }
    if allocator.stats().allocated != before.allocated + held.len() {
        return Err("allocated accounting mismatch");
    }
    let mut retained_loader_checked = false;
    for region in map.regions().iter().filter(|r| r.kind != 0) {
        if map.is_allocatable(region.base) {
            return Err("non-usable region became allocatable");
        }
        if let Some(address) = region
            .base
            .checked_add(PAGE_SIZE - 1)
            .map(|a| a & !(PAGE_SIZE - 1))
            && address
                .checked_add(PAGE_SIZE)
                .is_some_and(|end| end <= region.base + region.length)
        {
            if allocator.free(address) != Err(FrameError::NotAllocatable) {
                return Err("reserved or unreleased loader frame accepted by free");
            }
            retained_loader_checked |= region.kind == 5;
        }
    }
    if !retained_loader_checked {
        return Err("no retained loader frame checked");
    }
    for address in held {
        allocator.free(address).map_err(|_| "valid free rejected")?;
    }
    if allocator.free(held[0]) != Err(FrameError::NotAllocated)
        || allocator.free(held[0] + 1) != Err(FrameError::Misaligned)
        || allocator.free(map.kernel().start) != Err(FrameError::NotAllocatable)
    {
        return Err("invalid/double free was not rejected");
    }
    let reused = allocator.allocate().map_err(|_| "reuse failed")?;
    if reused != held[0] {
        return Err("freed frame was not reused");
    }
    allocator.free(reused).map_err(|_| "reuse cleanup failed")?;
    if allocator.stats() != before {
        return Err("real frame accounting not restored");
    }
    let regions = [
        Region {
            base: 0x1000,
            length: 2 * PAGE_SIZE,
            kind: 0,
        },
        Region {
            base: 1 << 40,
            length: PAGE_SIZE,
            kind: 5,
        },
        Region {
            base: (1 << 40) + PAGE_SIZE,
            length: PAGE_SIZE,
            kind: 0,
        },
    ];
    let tiny = MemoryMap::new(&regions, Reservation::new(0x8000, PAGE_SIZE)?)?;
    let mut bits = [0];
    let mut pool = FrameAllocator::new(&tiny, &mut bits).map_err(|_| "tiny pool init failed")?;
    for expected in [0x1000, 0x2000, (1 << 40) + PAGE_SIZE] {
        if pool.allocate() != Ok(expected) {
            return Err("sparse pool returned wrong frame");
        }
    }
    if pool.allocate() != Err(FrameError::Exhausted)
        || pool.free(1 << 40) != Err(FrameError::NotAllocatable)
    {
        return Err("exhaustion or bootloader reservation failed");
    }
    let malformed = [Region {
        base: 0x1001,
        length: PAGE_SIZE,
        kind: 0,
    }];
    if MemoryMap::new(&malformed, tiny.kernel()).is_ok() {
        return Err("malformed descriptor accepted");
    }
    Ok(())
}

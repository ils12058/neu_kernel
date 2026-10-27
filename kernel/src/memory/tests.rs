use super::*;
use std::{collections::BTreeSet, vec::Vec};

fn r(base: u64, pages: u64, kind: u64) -> Region {
    Region {
        base,
        length: pages * PAGE_SIZE,
        kind,
    }
}
fn outside() -> Reservation {
    Reservation::new(0x8000_0000, PAGE_SIZE).unwrap()
}

#[test]
fn preserves_exact_descriptors_and_excludes_all_reserved_types() {
    let mut regions = Vec::new();
    for kind in 0..=9 {
        regions.push(r((kind + 1) * 0x10000, 2, kind));
    }
    let map = MemoryMap::new(&regions, outside()).unwrap();
    assert_eq!(map.regions(), regions);
    assert_eq!(map.usable_bytes(), 2 * PAGE_SIZE);
    for region in &regions {
        assert_eq!(map.is_allocatable(region.base), region.kind == 0);
    }
    assert!(!map.is_allocatable(0));
    assert!(!map.is_allocatable(0x9000_0000));
}

#[test]
fn rejects_malformed_maps_without_relaxing_overlap_rules() {
    let bad = [
        Vec::new(),
        std::vec![Region {
            base: u64::MAX - 1,
            length: 8,
            kind: 1
        }],
        std::vec![r(0x2000, 1, 0), r(0x1000, 1, 1)],
        std::vec![r(0x1001, 1, 0)],
        std::vec![r(0x1000, 0, 0)],
        std::vec![r(0x1000, 3, 0), r(0x2000, 1, 1)],
        std::vec![r(0x1000, 3, 1), r(0x2000, 1, 0)],
        std::vec![r(0x1000, 1, 0), r(0x2001, 1, 5)],
        std::vec![r(0x1000, 1, 5), r(0x1000, 1, 1)],
        std::vec![r(0x1000, 1, 1)],
        std::vec![r(0x1000, 1, 0); MAX_REGIONS + 1],
    ];
    for regions in bad {
        assert!(MemoryMap::new(&regions, outside()).is_err(), "{regions:?}");
    }
    let valid = [r(0x1000, 1, 0), r(0x3000, 3, 1), r(0x4000, 1, 2)];
    assert!(MemoryMap::new(&valid, outside()).is_ok());
    assert!(Reservation::new(u64::MAX - PAGE_SIZE + 1, PAGE_SIZE).is_err());
}

#[test]
fn kernel_metadata_and_page_zero_are_reserved_even_inside_usable_ram() {
    let regions = [r(0, 10, 0)];
    let map = MemoryMap::new(
        &regions,
        Reservation::new(3 * PAGE_SIZE, 2 * PAGE_SIZE).unwrap(),
    )
    .unwrap();
    let expected = [1, 2, 5, 6, 7, 8, 9].map(|p| p * PAGE_SIZE);
    assert_eq!(map.allocatable_frames(), expected.len());
    let mut bits = [0; 1];
    let mut allocator = FrameAllocator::new(&map, &mut bits).unwrap();
    for address in expected {
        assert_eq!(allocator.allocate(), Ok(address));
    }
    assert_eq!(allocator.allocate(), Err(FrameError::Exhausted));
    for address in [0, 3 * PAGE_SIZE, 4 * PAGE_SIZE] {
        assert_eq!(allocator.free(address), Err(FrameError::NotAllocatable));
    }
}

#[test]
fn sparse_high_addresses_need_bits_for_candidates_not_the_address_space() {
    let regions = [r(0x1000, 1, 0), r(0xfee0_0000, 2, 1), r(1 << 44, 2, 0)];
    let map = MemoryMap::new(&regions, outside()).unwrap();
    let mut bits = [0; 1];
    let mut allocator = FrameAllocator::new(&map, &mut bits).unwrap();
    assert_eq!(allocator.allocate(), Ok(0x1000));
    assert_eq!(allocator.allocate(), Ok(1 << 44));
    assert_eq!(allocator.allocate(), Ok((1 << 44) + PAGE_SIZE));
    assert_eq!(allocator.allocate(), Err(FrameError::Exhausted));
    assert_eq!(allocator.free(0xfee0_0000), Err(FrameError::NotAllocatable));
    assert_eq!(allocator.free(0x2000), Err(FrameError::NotAllocatable));
}

#[test]
fn exhaustion_free_reuse_and_invalid_operations_preserve_accounting() {
    let regions = [r(0x1000, 65, 0)];
    let map = MemoryMap::new(&regions, outside()).unwrap();
    let mut bits = [0; 1];
    assert!(matches!(
        FrameAllocator::new(&map, &mut bits),
        Err(FrameError::MetadataCapacity)
    ));
    let mut bits = [0; 2];
    let mut allocator = FrameAllocator::new(&map, &mut bits).unwrap();
    let mut held = BTreeSet::new();
    for _ in 0..65 {
        assert!(held.insert(allocator.allocate().unwrap()));
    }
    assert_eq!(allocator.allocate(), Err(FrameError::Exhausted));
    assert_eq!(allocator.free(0x1001), Err(FrameError::Misaligned));
    let address = 64 * PAGE_SIZE;
    allocator.free(address).unwrap();
    assert_eq!(allocator.free(address), Err(FrameError::NotAllocated));
    assert_eq!(allocator.allocate(), Ok(address));
    assert_eq!(
        allocator.stats(),
        FrameStats {
            total: 65,
            allocated: 65,
            free: 0
        }
    );
    for address in held {
        allocator.free(address).unwrap();
    }
    assert_eq!(
        allocator.stats(),
        FrameStats {
            total: 65,
            allocated: 0,
            free: 65
        }
    );
}

#[test]
fn mixed_operations_match_an_independent_reference_set() {
    let regions = [r(0, 17, 0), r(0x20000, 5, 5), r(1 << 40, 19, 0)];
    let map = MemoryMap::new(
        &regions,
        Reservation::new(3 * PAGE_SIZE, 2 * PAGE_SIZE).unwrap(),
    )
    .unwrap();
    let candidates: BTreeSet<u64> = (1..17)
        .filter(|p| ![3, 4].contains(p))
        .map(|p| p * PAGE_SIZE)
        .chain((0..19).map(|p| (1 << 40) + p * PAGE_SIZE))
        .collect();
    let mut bits = [0; 1];
    let mut allocator = FrameAllocator::new(&map, &mut bits).unwrap();
    let mut held = BTreeSet::new();
    let mut seed = 7u64;
    for _ in 0..5000 {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        if seed & 3 == 0 && !held.is_empty() {
            let address = *held.iter().nth(seed as usize % held.len()).unwrap();
            allocator.free(address).unwrap();
            held.remove(&address);
        } else {
            match allocator.allocate() {
                Ok(address) => {
                    assert!(candidates.contains(&address));
                    assert!(held.insert(address));
                }
                Err(error) => {
                    assert_eq!(error, FrameError::Exhausted);
                    assert_eq!(held, candidates);
                }
            }
        }
        let stats = allocator.stats();
        assert_eq!(stats.total, candidates.len());
        assert_eq!(stats.allocated, held.len());
        assert_eq!(stats.free + stats.allocated, stats.total);
    }
}

#[test]
fn fully_reserved_usable_span_is_an_empty_pool_not_an_allocatable_frame() {
    let regions = [r(0x1000, 2, 0)];
    let map = MemoryMap::new(&regions, Reservation::new(0x1000, 2 * PAGE_SIZE).unwrap()).unwrap();
    let mut bits = [];
    let mut allocator = FrameAllocator::new(&map, &mut bits).unwrap();
    assert_eq!(
        allocator.stats(),
        FrameStats {
            total: 0,
            allocated: 0,
            free: 0
        }
    );
    assert_eq!(allocator.allocate(), Err(FrameError::Exhausted));
    assert_eq!(allocator.free(0x1000), Err(FrameError::NotAllocatable));
}

#[test]
fn batch_free_preflight_is_atomic_and_rejects_double_free() {
    let regions = [r(0x1000, 8, 0)];
    let map = MemoryMap::new(&regions, outside()).unwrap();
    let mut bitmap = [0; 1];
    let mut allocator = FrameAllocator::new(&map, &mut bitmap).unwrap();
    let a = allocator.allocate().unwrap();
    let b = allocator.allocate().unwrap();
    let before = allocator.stats();
    assert_eq!(allocator.free_batch(&[a, a]), Err(FrameError::NotAllocated));
    assert_eq!(allocator.stats(), before);
    assert_eq!(
        allocator.free_batch(&[a, 0x123]),
        Err(FrameError::Misaligned)
    );
    assert_eq!(allocator.stats(), before);
    allocator.free_batch(&[a, b]).unwrap();
    assert_eq!(allocator.stats().allocated, 0);
    assert_eq!(allocator.free_batch(&[a]), Err(FrameError::NotAllocated));
    assert_eq!(allocator.allocate(), Ok(a));
}

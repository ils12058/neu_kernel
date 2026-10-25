use super::*;
use std::{boxed::Box, collections::BTreeMap, vec::Vec};

struct Model {
    pages: BTreeMap<u64, (u8, Box<[u64; 512]>, bool)>,
    next: u64,
    budget: Option<usize>,
    clock: usize,
    flushed: usize,
    detached: BTreeMap<u64, usize>,
    invalidated: Vec<u64>,
    released: usize,
}
impl Model {
    fn new() -> Self {
        Self {
            pages: BTreeMap::new(),
            next: PAGE,
            budget: None,
            clock: 0,
            flushed: 0,
            detached: BTreeMap::new(),
            invalidated: Vec::new(),
            released: 0,
        }
    }
}
impl TableMemory for Model {
    fn allocate(&mut self, level: u8) -> Result<u64> {
        if self.budget == Some(0) {
            return Err(Error::Exhausted);
        }
        if let Some(budget) = &mut self.budget {
            *budget -= 1;
        }
        let address = self.next;
        self.next += PAGE;
        assert!(
            self.pages
                .insert(address, (level, Box::new([0; 512]), level == 4))
                .is_none()
        );
        Ok(address)
    }
    fn release(&mut self, address: u64) -> Result<()> {
        let page = self.pages.get(&address).ok_or(Error::NotOwned)?;
        if page.2
            || page.1.iter().any(|v| v & 1 != 0)
            || self.pages.values().any(|(level, entries, _)| {
                *level > 1
                    && entries
                        .iter()
                        .any(|v| v & 1 != 0 && v & ADDRESS_FIELD == address)
            })
        {
            return Err(Error::InUse);
        }
        assert!(
            self.flushed >= *self.detached.get(&address).unwrap_or(&0),
            "table reused before walk-cache flush"
        );
        self.pages.remove(&address);
        self.released += 1;
        Ok(())
    }
    fn owned(&self, address: u64, level: u8) -> bool {
        self.pages.get(&address).is_some_and(|p| p.0 == level)
    }
    fn pinned(&self, address: u64) -> bool {
        self.pages.get(&address).is_some_and(|p| p.2)
    }
    fn read(&self, table: u64, index: usize) -> Result<u64> {
        self.pages
            .get(&table)
            .and_then(|p| p.1.get(index))
            .copied()
            .ok_or(Error::NotOwned)
    }
    fn write(&mut self, table: u64, index: usize, value: u64) -> Result<()> {
        let page = self.pages.get_mut(&table).ok_or(Error::NotOwned)?;
        let old = *page.1.get(index).ok_or(Error::Address)?;
        self.clock += 1;
        if page.0 > 1 && old & 1 != 0 && value == 0 {
            self.detached.insert(old & ADDRESS_FIELD, self.clock);
        }
        page.1[index] = value;
        Ok(())
    }
    fn invalidate(&mut self, address: u64) {
        self.invalidated.push(address);
    }
    fn flush(&mut self) {
        self.flushed = self.clock;
    }
}

#[test]
fn canonicality_indices_and_range_overflow() {
    assert_eq!(indices(0xffff_ffff_ffff_f000), Ok([511; 4]));
    assert!(canonical(0x7fff_ffff_ffff));
    assert!(!canonical(0x8000_0000_0000));
    assert!(!canonical(0xffff_0000_0000_0000));
    assert_eq!(test_range(TEST_BASE, 1024), Ok(()));
    for (base, count) in [
        (TEST_BASE + 1, 1),
        (TEST_BASE, 0),
        (TEST_BASE, usize::MAX),
        (TEST_BASE + TEST_SIZE, 1),
        (0xffff_ffff_ffff_f000, 2),
        (0, 1),
    ] {
        assert!(test_range(base, count).is_err());
    }
}

#[test]
fn physical_width_alignment_and_supported_flags() {
    assert_eq!(physical(1 << 40, 40), Err(Error::Physical));
    assert_eq!(physical(0x1001, 48), Err(Error::Alignment));
    assert_eq!(physical(0x1000, 53), Err(Error::Physical));
    assert_eq!(
        leaf_entry(
            0x1000,
            Permissions {
                write: true,
                execute: true
            },
            48
        ),
        Err(Error::Wx)
    );
    let entry = leaf_entry(0x1000, Permissions::RW, 48).unwrap();
    assert_eq!(entry & (4 | 128 | 256), 0); // No user, huge, or global bits.
    assert_ne!(entry & NX, 0);
    assert_eq!(decode(entry | (1 << 62), 1, 48), Err(Error::Flags));
    assert_eq!(decode(0x1000 | 129, 4, 48), Err(Error::Flags));
}

#[test]
fn large_boot_pages_and_effective_parent_permissions() {
    assert_eq!(
        decode(0x4000_0000 | 0x1000 | 131, 3, 48),
        Ok(Some((0x4000_0000, 1 << 30)))
    );
    assert_eq!(
        decode(0x20_0000 | 0x1000 | 131, 2, 48),
        Ok(Some((0x20_0000, 1 << 21)))
    );
    assert!(decode(0x20_2000 | 131, 2, 48).is_err());
    let mut memory = Model::new();
    let tree = PageTree::new(&mut memory, 48).unwrap();
    let child = memory.allocate(3).unwrap();
    let path = indices(TEST_BASE + 0x1234).unwrap();
    memory.write(tree.root, path[0], child | 1).unwrap(); // Parent read-only, supervisor.
    memory
        .write(child, path[1], 0x4000_0000 | 131 | NX)
        .unwrap();
    let result = tree
        .translate(&memory, TEST_BASE + 0x1234)
        .unwrap()
        .unwrap();
    assert_eq!(result.physical, 0x4000_1234);
    assert_eq!(result.permissions, Permissions::R);
    assert!(!result.user);
}

#[test]
fn mapping_conflict_does_not_replace_existing_leaf() {
    let mut memory = Model::new();
    let tree = PageTree::new(&mut memory, 48).unwrap();
    tree.map(&mut memory, TEST_BASE, 0x10_0000, Permissions::RW)
        .unwrap();
    let count = memory.pages.len();
    assert_eq!(
        tree.map(&mut memory, TEST_BASE, 0x20_0000, Permissions::R),
        Err(Error::AlreadyMapped)
    );
    assert_eq!(
        tree.translate(&memory, TEST_BASE + 17)
            .unwrap()
            .unwrap()
            .physical,
        0x10_0011
    );
    assert_eq!(memory.pages.len(), count);
    let child = memory
        .read(tree.root, indices(TEST_BASE).unwrap()[0])
        .unwrap()
        & ADDRESS_FIELD;
    assert_eq!(memory.release(child), Err(Error::InUse));
    assert_eq!(memory.release(tree.root), Err(Error::InUse));
}

#[test]
fn mapping_rejects_restricted_parents_without_weakening_them() {
    let mut memory = Model::new();
    let tree = PageTree::new(&mut memory, 48).unwrap();
    let child = memory.allocate(3).unwrap();
    let slot = indices(TEST_BASE).unwrap()[0];
    memory.write(tree.root, slot, child | 1 | NX).unwrap();
    for permission in [Permissions::RW, Permissions::RX] {
        assert_eq!(
            tree.map(&mut memory, TEST_BASE, 0x10_0000, permission),
            Err(Error::Flags)
        );
        assert_eq!(memory.pages.len(), 2);
        assert_eq!(memory.read(tree.root, slot), Ok(child | 1 | NX));
    }
}

#[test]
fn every_intermediate_allocation_failure_rolls_back() {
    for budget in 0..3 {
        let mut memory = Model::new();
        let tree = PageTree::new(&mut memory, 48).unwrap();
        memory.budget = Some(budget);
        assert_eq!(
            tree.map(&mut memory, TEST_BASE, 0x10_0000, Permissions::RW),
            Err(Error::Exhausted)
        );
        assert_eq!(memory.pages.len(), 1);
        assert_eq!(memory.released, budget);
        assert_eq!(tree.translate(&memory, TEST_BASE).unwrap(), None);
    }
}

#[test]
fn invalid_map_has_no_allocation_side_effects() {
    let mut memory = Model::new();
    let tree = PageTree::new(&mut memory, 40).unwrap();
    for (va, pa, permissions) in [
        (TEST_BASE + 1, 0x1000, Permissions::R),
        (TEST_BASE, 0x1001, Permissions::R),
        (TEST_BASE, 1 << 40, Permissions::R),
        (
            TEST_BASE,
            0x1000,
            Permissions {
                write: true,
                execute: true,
            },
        ),
    ] {
        assert!(tree.map(&mut memory, va, pa, permissions).is_err());
        assert_eq!(memory.pages.len(), 1);
    }
}

#[test]
fn unmapping_flushes_before_table_reuse_and_preserves_pinned_tables() {
    let mut memory = Model::new();
    let tree = PageTree::new(&mut memory, 48).unwrap();
    tree.map(&mut memory, TEST_BASE, 0x10_0000, Permissions::R)
        .unwrap();
    tree.unmap(&mut memory, TEST_BASE).unwrap();
    assert_eq!(memory.pages.len(), 1);
    assert_eq!(memory.released, 3);
    assert_eq!(memory.invalidated.last(), Some(&TEST_BASE));
    assert_eq!(tree.unmap(&mut memory, TEST_BASE), Err(Error::NotMapped));
    tree.map(&mut memory, TEST_BASE, 0x10_0000, Permissions::R)
        .unwrap();
    for page in memory.pages.values_mut() {
        page.2 = true;
    }
    tree.unmap(&mut memory, TEST_BASE).unwrap();
    assert_eq!(memory.pages.len(), 4);
}

#[test]
fn registry_tracks_initialization_aliases_and_stale_ownership() {
    let mut registry = Registry::new();
    let id = registry.insert(0x10_0000).unwrap();
    assert_eq!(
        registry.prepare(TEST_BASE, &[id], Permissions::RW),
        Err(Error::Uninitialized)
    );
    registry.initialize(id).unwrap();
    registry
        .commit(TEST_BASE, &[id, id], Permissions::RW)
        .unwrap();
    assert_eq!(registry.releasable(id), Err(Error::InUse));
    registry.remove(TEST_BASE).unwrap();
    assert_eq!(registry.releasable(id), Err(Error::InUse));
    assert_eq!(
        registry.prepare(TEST_BASE, &[id], Permissions::RX),
        Err(Error::Wx)
    );
    registry.remove(TEST_BASE + PAGE).unwrap();
    registry.commit(TEST_BASE, &[id], Permissions::RX).unwrap();
    assert_eq!(registry.writable(id), Err(Error::Wx));
    registry.remove(TEST_BASE).unwrap();
    registry.retire(id).unwrap();
    let next = registry.insert(0x10_0000).unwrap();
    assert_ne!(next, id);
    assert_eq!(registry.physical(id), Err(Error::NotOwned));
    assert_eq!(
        registry.prepare(TEST_BASE, &[next], Permissions::R),
        Err(Error::Uninitialized)
    );
}

#[test]
fn multi_page_failure_rolls_back_leaves_tables_and_references() {
    let mut memory = Model::new();
    let tree = PageTree::new(&mut memory, 48).unwrap();
    let mut registry = Registry::new();
    let a = registry.insert(0x10_0000).unwrap();
    let b = registry.insert(0x20_0000).unwrap();
    registry.initialize(a).unwrap();
    registry.initialize(b).unwrap();
    let base = TEST_BASE + 511 * PAGE;
    memory.budget = Some(3); // First page succeeds; second page needs a new PT.
    assert_eq!(
        map_frames(
            &tree,
            &mut memory,
            &mut registry,
            base,
            &[a, b],
            Permissions::RW
        ),
        Err(Error::Exhausted)
    );
    assert_eq!(memory.pages.len(), 1);
    assert_eq!(tree.translate(&memory, base).unwrap(), None);
    assert_eq!(registry.releasable(a), Ok(0x10_0000));
    assert_eq!(registry.releasable(b), Ok(0x20_0000));
    memory.budget = None;
    map_frames(
        &tree,
        &mut memory,
        &mut registry,
        base,
        &[a, b],
        Permissions::RW,
    )
    .unwrap();
    for (offset, id) in [(0, a), (PAGE, b)] {
        tree.unmap(&mut memory, base + offset).unwrap();
        registry.remove(base + offset).unwrap();
        assert!(registry.releasable(id).is_ok());
    }
    assert_eq!(memory.pages.len(), 1);
}

#[test]
fn registry_capacity_failure_is_atomic() {
    let mut registry = Registry::new();
    let id = registry.insert(0x1000).unwrap();
    registry.initialize(id).unwrap();
    assert_eq!(
        registry.prepare(TEST_BASE, &[id; MAPPINGS + 1], Permissions::RW),
        Err(Error::Capacity)
    );
    assert!(registry.releasable(id).is_ok());
    registry
        .commit(TEST_BASE, &[id; MAPPINGS], Permissions::R)
        .unwrap();
    assert_eq!(
        registry.prepare(TEST_BASE + MAPPINGS as u64 * PAGE, &[id], Permissions::R),
        Err(Error::Capacity)
    );
    for index in 0..MAPPINGS {
        registry.remove(TEST_BASE + index as u64 * PAGE).unwrap();
    }
    assert!(registry.releasable(id).is_ok());
}

#[test]
fn duplicate_physical_ownership_cannot_bypass_alias_permissions() {
    let mut registry = Registry::new();
    let frame = registry.insert(0x1000).unwrap();
    registry.initialize(frame).unwrap();
    registry
        .commit(TEST_BASE, &[frame], Permissions::RW)
        .unwrap();
    assert_eq!(registry.insert(0x1000), Err(Error::InUse));
    assert_eq!(registry.insert(0x1001), Err(Error::Alignment));
    assert_eq!(registry.retire(frame), Err(Error::InUse));
    registry.remove(TEST_BASE).unwrap();
    registry.retire(frame).unwrap();
    let next = registry.insert(0x1000).unwrap();
    assert_ne!(next, frame);
    assert_eq!(registry.physical(frame), Err(Error::NotOwned));
}

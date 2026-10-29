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
    fail_clear: bool,
    reject_leaf_index: Option<usize>,
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
            fail_clear: false,
            reject_leaf_index: None,
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
        if value & 1 != 0 && self.owned(table, 1) && self.reject_leaf_index == Some(index) {
            self.reject_leaf_index = None;
            return Err(Error::Exhausted);
        }
        if value == 0 && self.fail_clear {
            return Err(Error::Corrupt);
        }
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

#[test]
fn user_mappings_require_lower_range_and_user_bits_at_every_level() {
    let mut memory = Model::new();
    let tree = PageTree::new(&mut memory, 48).unwrap();
    for address in [
        0,
        USER_BASE - PAGE,
        USER_BASE + USER_SIZE,
        TEST_BASE,
        0x8000_0000_0000,
    ] {
        assert!(
            tree.map_user(&mut memory, address, 0x100000, Permissions::RX)
                .is_err()
        );
        assert_eq!(memory.pages.len(), 1);
    }
    assert_eq!(user_range(USER_BASE, usize::MAX), Err(Error::Address));
    tree.map_user(&mut memory, USER_BASE, 0x100000, Permissions::RX)
        .unwrap();
    let mut table = tree.root;
    for (depth, index) in indices(USER_BASE).unwrap().into_iter().enumerate() {
        let entry = memory.read(table, index).unwrap();
        assert_ne!(entry & 4, 0);
        if depth < 3 {
            table = entry & ADDRESS_FIELD;
        } else {
            assert_eq!(entry & (2 | NX), 0);
        }
    }
    let mapping = tree.translate(&memory, USER_BASE).unwrap().unwrap();
    assert!(mapping.user);
    assert_eq!(mapping.permissions, Permissions::RX);
    let parent_index = indices(USER_BASE).unwrap()[0];
    let parent = memory.read(tree.root, parent_index).unwrap();
    memory.write(tree.root, parent_index, parent & !4).unwrap();
    assert!(!tree.translate(&memory, USER_BASE).unwrap().unwrap().user);
    assert_eq!(
        tree.map_user(&mut memory, USER_BASE + PAGE, 0x101000, Permissions::RW),
        Err(Error::Flags)
    );
    assert_eq!(memory.read(tree.root, parent_index), Ok(parent & !4));
}

#[test]
fn supervisor_and_user_branches_cannot_escalate_each_other() {
    let mut memory = Model::new();
    let tree = PageTree::new(&mut memory, 48).unwrap();
    tree.map(&mut memory, USER_BASE, 0x100000, Permissions::R)
        .unwrap();
    let count = memory.pages.len();
    assert_eq!(
        tree.map_user(&mut memory, USER_BASE + PAGE, 0x101000, Permissions::RW),
        Err(Error::Flags)
    );
    assert_eq!(memory.pages.len(), count);
    tree.unmap(&mut memory, USER_BASE).unwrap();
    tree.map_user(&mut memory, USER_BASE, 0x100000, Permissions::R)
        .unwrap();
    assert_eq!(
        tree.map(&mut memory, USER_BASE + PAGE, 0x101000, Permissions::RW),
        Err(Error::Flags)
    );
    assert!(tree.translate(&memory, USER_BASE).unwrap().unwrap().user);
}

#[test]
fn user_transactions_rollback_and_keep_alias_ownership_across_domains() {
    let mut memory = Model::new();
    let tree = PageTree::new(&mut memory, 48).unwrap();
    let mut registry = Registry::new();
    let id = registry.insert(0x100000).unwrap();
    registry.initialize(id).unwrap();
    let base = USER_BASE + 511 * PAGE;
    memory.budget = Some(3);
    assert_eq!(
        map_user_frames(
            &tree,
            &mut memory,
            &mut registry,
            base,
            &[id, id],
            Permissions::RX
        ),
        Err(Error::Exhausted)
    );
    assert_eq!(memory.pages.len(), 1);
    assert!(registry.releasable(id).is_ok());
    assert_eq!(registry.mapping(base).err(), Some(Error::NotMapped));
    memory.budget = None;
    map_user_frames(
        &tree,
        &mut memory,
        &mut registry,
        USER_BASE,
        &[id],
        Permissions::RX,
    )
    .unwrap();
    assert_eq!(
        registry.prepare(TEST_BASE, &[id], Permissions::RW),
        Err(Error::Wx)
    );
    assert_eq!(registry.writable(id), Err(Error::Wx));
    assert_eq!(registry.retire(id), Err(Error::InUse));
    // An R/NX kernel alias is compatible, and still prevents backing reuse.
    map_frames(
        &tree,
        &mut memory,
        &mut registry,
        TEST_BASE,
        &[id],
        Permissions::R,
    )
    .unwrap();
    tree.unmap(&mut memory, USER_BASE).unwrap();
    registry.remove(USER_BASE).unwrap();
    assert_eq!(registry.retire(id), Err(Error::InUse));
    tree.unmap(&mut memory, TEST_BASE).unwrap();
    registry.remove(TEST_BASE).unwrap();
    registry.retire(id).unwrap();
    assert_eq!(memory.pages.len(), 1);
}

#[test]
fn private_roots_share_backing_without_sharing_or_reclaiming_child_tables() {
    let mut memory = Model::new();
    let kernel = PageTree::new(&mut memory, 48).unwrap();
    kernel
        .map(&mut memory, TEST_BASE, 0x100000, Permissions::RX)
        .unwrap();
    let before = memory.pages.keys().copied().collect::<Vec<_>>();
    let user = PageTree::new(&mut memory, 48).unwrap();
    user.map(&mut memory, TEST_BASE, 0x100000, Permissions::RX)
        .unwrap();
    user.map_user(&mut memory, USER_BASE, 0x200000, Permissions::RX)
        .unwrap();
    assert_ne!(kernel.root, user.root);
    let index = indices(TEST_BASE).unwrap()[0];
    assert_ne!(
        memory.read(kernel.root, index).unwrap() & ADDRESS_FIELD,
        memory.read(user.root, index).unwrap() & ADDRESS_FIELD
    );
    assert!(kernel.translate(&memory, USER_BASE).unwrap().is_none());
    assert!(!user.translate(&memory, TEST_BASE).unwrap().unwrap().user);
    user.unmap(&mut memory, USER_BASE).unwrap();
    user.unmap(&mut memory, TEST_BASE).unwrap();
    memory.pages.get_mut(&user.root).unwrap().2 = false; // Inactive, no root owner remains.
    memory.flush();
    memory.release(user.root).unwrap();
    assert_eq!(memory.pages.keys().copied().collect::<Vec<_>>(), before);
    assert_eq!(
        kernel
            .translate(&memory, TEST_BASE)
            .unwrap()
            .unwrap()
            .physical,
        0x100000
    );
}

#[test]
fn simultaneous_spaces_key_same_va_by_identity_and_reject_cross_owner_frames() {
    let mut memory = Model::new();
    let a = PageTree::new(&mut memory, 48).unwrap();
    let b = PageTree::new(&mut memory, 48).unwrap();
    let mut registry = Registry::new();
    let sa = registry.create_space().unwrap();
    let sb = registry.create_space().unwrap();
    registry.bind_root(sa, a.root).unwrap();
    registry.bind_root(sb, b.root).unwrap();
    let fa = registry.insert_for(sa, 0x100000).unwrap();
    let fb = registry.insert_for(sb, 0x200000).unwrap();
    registry.initialize(fa).unwrap();
    registry.initialize(fb).unwrap();
    let count = memory.pages.len();
    assert_eq!(
        map_user_frames_for(
            &b,
            &mut memory,
            &mut registry,
            sa,
            USER_BASE,
            &[fa],
            Permissions::RW
        ),
        Err(Error::NotOwned)
    );
    assert_eq!(memory.pages.len(), count);
    assert!(b.translate(&memory, USER_BASE).unwrap().is_none());
    map_user_frames_for(
        &a,
        &mut memory,
        &mut registry,
        sa,
        USER_BASE,
        &[fa],
        Permissions::RW,
    )
    .unwrap();
    map_user_frames_for(
        &b,
        &mut memory,
        &mut registry,
        sb,
        USER_BASE,
        &[fb],
        Permissions::RW,
    )
    .unwrap();
    assert_eq!(registry.mapping_for(sa, USER_BASE).unwrap().frame, fa);
    assert_eq!(registry.mapping_for(sb, USER_BASE).unwrap().frame, fb);
    assert_eq!(registry.mapping(USER_BASE).err(), Some(Error::NotMapped));
    assert_eq!(
        map_user_frames_for(
            &b,
            &mut memory,
            &mut registry,
            sb,
            USER_BASE + PAGE,
            &[fa],
            Permissions::R
        ),
        Err(Error::NotOwned)
    );
    assert_eq!(
        a.translate(&memory, USER_BASE).unwrap().unwrap().physical,
        0x100000
    );
    assert_eq!(
        b.translate(&memory, USER_BASE).unwrap().unwrap().physical,
        0x200000
    );
    a.unmap(&mut memory, USER_BASE).unwrap();
    registry.remove_for(sa, USER_BASE).unwrap();
    registry.retire(fa).unwrap();
    registry.clear_root(sa, a.root).unwrap();
    registry.retire_space(sa).unwrap();
    assert_eq!(registry.mapping_for(sb, USER_BASE).unwrap().frame, fb);
    assert_eq!(registry.releasable(fb), Err(Error::InUse));
    assert_eq!(
        b.translate(&memory, USER_BASE).unwrap().unwrap().physical,
        0x200000
    );
}

#[test]
fn bounded_space_ids_refuse_active_referenced_and_stale_root_lifetimes() {
    let mut r = Registry::new();
    let a = r.create_space().unwrap();
    let b = r.create_space().unwrap();
    assert_eq!(r.create_space(), Err(Error::Capacity));
    r.bind_root(a, 0x1000).unwrap();
    r.bind_root(b, 0x2000).unwrap();
    let f = r.insert_for(a, 0x100000).unwrap();
    r.initialize(f).unwrap();
    assert_eq!(r.retire_space(a), Err(Error::InUse));
    r.activate_space(a).unwrap();
    assert_eq!(r.releasable(f), Err(Error::InUse));
    assert_eq!(r.clear_root(a, 0x1000), Err(Error::InUse));
    assert_eq!(r.deactivate_space(b), Err(Error::NotOwned));
    assert_eq!(r.create_space(), Err(Error::InUse));
    r.deactivate_space(a).unwrap();
    r.retire(f).unwrap();
    assert_eq!(r.retire_space(a), Err(Error::InUse)); // bound root still owns a lifetime
    r.clear_root(a, 0x1000).unwrap();
    r.retire_space(a).unwrap();
    let c = r.create_space().unwrap();
    assert_ne!(a, c);
    assert_ne!(a.raw(), c.raw());
    r.bind_root(c, 0x1000).unwrap(); // same physical root may be reused, not its identity
    assert_eq!(r.root(a), Err(Error::NotOwned));
    assert_eq!(r.mapping_for(a, USER_BASE).err(), Some(Error::NotOwned));
    assert_eq!(r.root(b), Ok(0x2000));
}

#[test]
fn failed_second_space_mapping_preserves_sibling_refs_tables_and_capacity() {
    let mut memory = Model::new();
    let a = PageTree::new(&mut memory, 48).unwrap();
    let b = PageTree::new(&mut memory, 48).unwrap();
    let mut r = Registry::new();
    let sa = r.create_space().unwrap();
    let sb = r.create_space().unwrap();
    r.bind_root(sa, a.root).unwrap();
    r.bind_root(sb, b.root).unwrap();
    let fa = r.insert_for(sa, 0x100000).unwrap();
    let fb = r.insert_for(sb, 0x200000).unwrap();
    r.initialize(fa).unwrap();
    r.initialize(fb).unwrap();
    map_user_frames_for(
        &a,
        &mut memory,
        &mut r,
        sa,
        USER_BASE,
        &[fa],
        Permissions::RW,
    )
    .unwrap();
    let count = memory.pages.len();
    memory.budget = Some(3);
    assert_eq!(
        map_user_frames_for(
            &b,
            &mut memory,
            &mut r,
            sb,
            USER_BASE + 511 * PAGE,
            &[fb, fb],
            Permissions::R
        ),
        Err(Error::Exhausted)
    );
    assert_eq!(memory.pages.len(), count);
    assert_eq!(r.mapping_for(sa, USER_BASE).unwrap().frame, fa);
    assert!(r.releasable(fb).is_ok());
    assert_eq!(r.releasable(fa), Err(Error::InUse));
    assert_eq!(
        a.translate(&memory, USER_BASE).unwrap().unwrap().physical,
        0x100000
    );
    assert!(
        b.translate(&memory, USER_BASE + 511 * PAGE)
            .unwrap()
            .is_none()
    );
}

#[test]
fn space_generation_and_owned_frame_capacity_exhaust_without_wrap_or_leaks() {
    let mut r = Registry::new();
    let space = r.create_space().unwrap();
    let mut frames = Vec::new();
    for i in 0..DATA_FRAMES {
        frames.push(r.insert_for(space, 0x100000 + i as u64 * PAGE).unwrap());
    }
    assert_eq!(r.insert_for(space, 0x200000), Err(Error::Capacity));
    for frame in frames {
        r.retire(frame).unwrap();
    }
    r.clear_root(space, 0).unwrap();
    r.retire_space(space).unwrap();
    for slot in &mut r.spaces {
        slot.live = false;
        slot.generation = u64::MAX >> 8;
    }
    assert_eq!(r.create_space(), Err(Error::Capacity));
}

#[test]
fn device_leaf_requires_verified_uc_pat_and_never_becomes_user_ram() {
    let mut memory = Model::new();
    let tree = PageTree::new(&mut memory, 48).unwrap();
    let va = 0xffffb00000000000;
    let count = memory.pages.len();
    assert_eq!(
        tree.map_device_uc(&mut memory, va, 0xfee00000, 6 << 24),
        Err(Error::Flags)
    );
    assert_eq!(memory.pages.len(), count);
    tree.map_device_uc(&mut memory, va, 0xfee00000, 0x0007040600070406)
        .unwrap();
    let path = indices(va).unwrap();
    let mut table = tree.root;
    for index in &path[..3] {
        table = memory.read(table, *index).unwrap() & ADDRESS_FIELD;
    }
    let raw = memory.read(table, path[3]).unwrap();
    assert_eq!(raw & (0x9f | NX), 0x1b | NX);
    let actual = tree.translate(&memory, va).unwrap().unwrap();
    assert_eq!(actual.physical, 0xfee00000);
    assert_eq!(actual.permissions, Permissions::RW);
    assert!(!actual.user);
    tree.unmap(&mut memory, va).unwrap();
    assert_eq!(memory.pages.len(), count);
}

#[test]
fn context_pins_preserve_owned_mappings_frames_and_root_lifetimes() {
    let mut memory = Model::new();
    let tree = PageTree::new(&mut memory, 48).unwrap();
    let mut r = Registry::new();
    let space = r.create_space().unwrap();
    r.bind_root(space, tree.root).unwrap();
    let frame = r.insert_for(space, 0x100000).unwrap();
    let unmapped = r.insert_for(space, 0x101000).unwrap();
    r.initialize(frame).unwrap();
    r.initialize(unmapped).unwrap();
    map_user_frames_for(
        &tree,
        &mut memory,
        &mut r,
        space,
        USER_BASE,
        &[frame],
        Permissions::RW,
    )
    .unwrap();
    r.pin_space(space).unwrap();
    let count = memory.pages.len();
    assert_eq!(r.remove_for(space, USER_BASE), Err(Error::InUse));
    assert_eq!(r.releasable(unmapped), Err(Error::InUse));
    assert_eq!(r.clear_root(space, tree.root), Err(Error::InUse));
    assert_eq!(r.retire_space(space), Err(Error::InUse));
    assert_eq!(
        map_user_frames_for(
            &tree,
            &mut memory,
            &mut r,
            space,
            USER_BASE + PAGE,
            &[unmapped],
            Permissions::R
        ),
        Err(Error::InUse)
    );
    assert_eq!(memory.pages.len(), count);
    assert_eq!(r.mapping_for(space, USER_BASE).unwrap().frame, frame);
    r.unpin_space(space).unwrap();
    tree.unmap(&mut memory, USER_BASE).unwrap();
    r.remove_for(space, USER_BASE).unwrap();
    r.retire(frame).unwrap();
    r.retire(unmapped).unwrap();
    r.clear_root(space, tree.root).unwrap();
    r.retire_space(space).unwrap();
}
#[test]
fn context_pin_bounds_refuse_wrap_underflow_active_release_and_stale_ids() {
    let mut r = Registry::new();
    let id = r.create_space().unwrap();
    r.bind_root(id, 0x1000).unwrap();
    assert_eq!(r.unpin_space(id), Err(Error::InUse));
    r.pin_space(id).unwrap();
    r.pin_space(id).unwrap();
    assert_eq!(r.pin_space(id), Err(Error::Capacity));
    r.activate_space(id).unwrap();
    assert_eq!(r.unpin_space(id), Err(Error::InUse));
    r.deactivate_space(id).unwrap();
    r.unpin_space(id).unwrap();
    r.unpin_space(id).unwrap();
    r.clear_root(id, 0x1000).unwrap();
    r.retire_space(id).unwrap();
    let new = r.create_space().unwrap();
    r.bind_root(new, 0x2000).unwrap();
    r.pin_space(new).unwrap();
    assert_eq!(r.unpin_space(id), Err(Error::NotOwned));
    assert_eq!(r.can_allocate(new), Err(Error::InUse));
    r.unpin_space(new).unwrap();
}
#[test]
fn supervisor_stack_arena_guards_owner_checks_and_rollback_are_enforced() {
    let mut memory = Model::new();
    let tree = PageTree::new(&mut memory, 48).unwrap();
    let mut r = Registry::new();
    let space = r.create_space().unwrap();
    r.bind_root(space, tree.root).unwrap();
    let other = r.create_space().unwrap();
    r.bind_root(other, 0x9000).unwrap();
    let a = r.insert_for(space, 0x100000).unwrap();
    let b = r.insert_for(space, 0x101000).unwrap();
    r.initialize(a).unwrap();
    r.initialize(b).unwrap();
    let count = memory.pages.len();
    assert_eq!(
        map_stack_frames_for(&tree, &mut memory, &mut r, space, KSTACK_BASE, &[a, b]),
        Err(Error::Address)
    );
    assert_eq!(
        map_stack_frames_for(
            &tree,
            &mut memory,
            &mut r,
            other,
            KSTACK_BASE + PAGE,
            &[a, b]
        ),
        Err(Error::NotOwned)
    );
    memory.budget = Some(2);
    assert_eq!(
        map_stack_frames_for(
            &tree,
            &mut memory,
            &mut r,
            space,
            KSTACK_BASE + PAGE,
            &[a, b]
        ),
        Err(Error::Exhausted)
    );
    assert_eq!(memory.pages.len(), count);
    assert!(r.releasable(a).is_ok());
    memory.budget = None;
    map_stack_frames_for(
        &tree,
        &mut memory,
        &mut r,
        space,
        KSTACK_BASE + PAGE,
        &[a, b],
    )
    .unwrap();
    for va in [KSTACK_BASE, KSTACK_BASE + 3 * PAGE] {
        assert!(tree.translate(&memory, va).unwrap().is_none());
    }
    for va in [KSTACK_BASE + PAGE, KSTACK_BASE + 2 * PAGE] {
        let t = tree.translate(&memory, va).unwrap().unwrap();
        assert!(!t.user);
        assert_eq!(t.permissions, Permissions::RW);
    }
    for va in [KSTACK_BASE + PAGE, KSTACK_BASE + 2 * PAGE] {
        tree.unmap(&mut memory, va).unwrap();
        r.remove_for(space, va).unwrap();
    }
    r.retire(a).unwrap();
    r.retire(b).unwrap();
    assert_eq!(memory.pages.len(), count);
}

struct MemberFixture {
    nodes: crate::node::Nodes,
    registry: Registry,
    memory: Model,
    tree: PageTree,
    queue: crate::scheduler::Queue,
    active: [crate::node::ActiveContext; 2],
    frames: [[FrameId; 3]; 2],
    data: FrameId,
}
fn member_fixture() -> MemberFixture {
    use crate::{
        context::{SavedContext, StackBinding},
        node::{ActiveContext, Nodes, ThreadContext},
        scheduler::Queue,
    };
    let mut memory = Model::new();
    let tree = PageTree::new(&mut memory, 48).unwrap();
    let mut registry = Registry::new();
    let space = registry.create_space().unwrap();
    registry.bind_root(space, tree.root).unwrap();
    let mut nodes = Nodes::new();
    let node = nodes.create().unwrap();
    nodes.configure(node).unwrap();
    nodes.bind_space(node, space).unwrap();
    let first = nodes
        .initial_thread(
            node,
            ThreadContext {
                space,
                root: tree.root,
                entry: 0x400000,
                rsp: thread_user_stack(0) + PAGE - 8,
            },
        )
        .unwrap();
    nodes.ready(node).unwrap();
    let second = nodes
        .member_thread(
            node,
            ThreadContext {
                space,
                root: tree.root,
                entry: 0x400000,
                rsp: thread_user_stack(1) + PAGE - 8,
            },
        )
        .unwrap();
    let data = registry.insert_for(space, 0x900000).unwrap();
    registry.initialize(data).unwrap();
    map_user_frames_for(
        &tree,
        &mut memory,
        &mut registry,
        space,
        0x402000,
        &[data],
        Permissions::RW,
    )
    .unwrap();
    let mut queue = Queue::new();
    let mut active = Vec::new();
    let mut all_frames = Vec::new();
    for (index, id) in [first, second].into_iter().enumerate() {
        let t = nodes.thread(id).unwrap();
        let a = ActiveContext {
            node,
            thread: id,
            context: t.context,
        };
        let stack = StackBinding::new(a);
        let frames = {
            let edit = nodes.stack_edit(id).unwrap();
            let pa = 0x100000 + index as u64 * 0x10000;
            let u = registry
                .insert_stack_frame(&edit, pa, StackKind::User)
                .unwrap();
            let k0 = registry
                .insert_stack_frame(&edit, pa + PAGE, StackKind::Kernel)
                .unwrap();
            let k1 = registry
                .insert_stack_frame(&edit, pa + 2 * PAGE, StackKind::Kernel)
                .unwrap();
            for f in [u, k0, k1] {
                registry.initialize(f).unwrap();
            }
            map_thread_stack_frames(
                &tree,
                &mut memory,
                &mut registry,
                &edit,
                thread_user_stack(index),
                &[u],
                StackKind::User,
            )
            .unwrap();
            map_thread_stack_frames(
                &tree,
                &mut memory,
                &mut registry,
                &edit,
                stack.bottom,
                &[k0, k1],
                StackKind::Kernel,
            )
            .unwrap();
            [u, k0, k1]
        };
        nodes
            .install_execution(id, SavedContext::initial_preemptible(a, stack))
            .unwrap();
        registry.pin_space(space).unwrap();
        queue.enqueue(&mut nodes, id).unwrap();
        active.push(a);
        all_frames.push(frames);
    }
    MemberFixture {
        nodes,
        registry,
        memory,
        tree,
        queue,
        active: active.try_into().unwrap(),
        frames: all_frames.try_into().unwrap(),
        data,
    }
}
fn retire_fixture_first(f: &mut MemberFixture) {
    let d = f.queue.next(&mut f.nodes).unwrap().unwrap();
    assert_eq!(d.active, f.active[0]);
    f.queue
        .finish(&mut f.nodes, d.active, crate::node::ExitReason::Normal(1))
        .unwrap();
    let edit = f.nodes.stack_edit(d.active.thread).unwrap();
    assert!(matches!(
        f.registry.finish_stack_edit(&edit),
        Err(Error::InUse)
    ));
    f.registry.unpin_space(d.active.context.space).unwrap();
    for (va, id) in [
        (thread_user_stack(0), f.frames[0][0]),
        (KSTACK_BASE + PAGE, f.frames[0][1]),
        (KSTACK_BASE + 2 * PAGE, f.frames[0][2]),
    ] {
        f.tree.unmap(&mut f.memory, va).unwrap();
        assert_eq!(f.registry.remove_stack_mapping(&edit, va).unwrap(), id);
        f.registry.retire_stack_frame(&edit, id).unwrap();
    }
    let receipt = f.registry.finish_stack_edit(&edit).unwrap();
    edit.finish(receipt).unwrap();
}
#[test]
fn scoped_stack_retirement_keeps_sibling_pin_tables_and_mappings_live() {
    let mut f = member_fixture();
    let space = f.active[0].context.space;
    let before_tables = f.memory.pages.len();
    let sibling = f.nodes.thread(f.active[1].thread).unwrap().saved;
    assert_eq!(f.registry.pins(space).unwrap(), 2);
    assert_eq!(
        f.registry.remove_for(space, thread_user_stack(0)),
        Err(Error::InUse)
    );
    assert_eq!(f.registry.releasable(f.frames[0][0]), Err(Error::InUse));
    retire_fixture_first(&mut f);
    assert_eq!(f.registry.pins(space).unwrap(), 1);
    assert_eq!(f.memory.pages.len(), before_tables);
    assert_eq!(f.nodes.thread(f.active[1].thread).unwrap().saved, sibling);
    assert_eq!(
        f.tree
            .translate(&f.memory, thread_user_stack(1))
            .unwrap()
            .unwrap()
            .physical,
        f.registry.physical(f.frames[1][0]).unwrap()
    );
    assert_eq!(f.registry.clear_root(space, f.tree.root), Err(Error::InUse));
    assert_eq!(f.registry.physical(f.frames[0][0]), Err(Error::NotOwned));
    assert_eq!(
        f.nodes.node(f.active[0].node).unwrap().state,
        crate::node::NodeState::Active
    );
}
#[test]
fn stack_edit_cannot_release_sibling_or_node_shared_data() {
    let mut f = member_fixture();
    let d = f.queue.next(&mut f.nodes).unwrap().unwrap();
    f.queue
        .finish(&mut f.nodes, d.active, crate::node::ExitReason::Normal(0))
        .unwrap();
    let edit = f.nodes.stack_edit(d.active.thread).unwrap();
    assert_eq!(
        f.registry.remove_stack_mapping(&edit, thread_user_stack(1)),
        Err(Error::NotOwned)
    );
    assert_eq!(
        f.registry.releasable_stack_frame(&edit, f.frames[1][0]),
        Err(Error::NotOwned)
    );
    assert_eq!(
        f.registry.remove_stack_mapping(&edit, 0x402000),
        Err(Error::NotOwned)
    );
    assert_eq!(
        f.registry.retire_stack_frame(&edit, f.data),
        Err(Error::NotOwned)
    );
    assert_eq!(f.registry.pins(d.active.context.space).unwrap(), 2);
    assert!(f.tree.translate(&f.memory, 0x402000).unwrap().is_some());
}
#[test]
fn scoped_mapping_mid_failure_rolls_back_without_touching_pinned_sibling() {
    let mut f = member_fixture();
    retire_fixture_first(&mut f);
    let context = f.active[0].context;
    let fresh = f.nodes.member_thread(f.active[0].node, context).unwrap();
    let edit = f.nodes.stack_edit(fresh).unwrap();
    let k0 = f
        .registry
        .insert_stack_frame(&edit, 0x100000, StackKind::Kernel)
        .unwrap();
    let k1 = f
        .registry
        .insert_stack_frame(&edit, 0x101000, StackKind::Kernel)
        .unwrap();
    for id in [k0, k1] {
        f.registry.initialize(id).unwrap();
    }
    let base = KSTACK_BASE + PAGE;
    f.tree
        .map(&mut f.memory, base + PAGE, 0x800000, Permissions::RW)
        .unwrap();
    let tables = f.memory.pages.len();
    assert_eq!(
        map_thread_stack_frames(
            &f.tree,
            &mut f.memory,
            &mut f.registry,
            &edit,
            base,
            &[k0, k1],
            StackKind::Kernel
        ),
        Err(Error::AlreadyMapped)
    );
    assert!(f.tree.translate(&f.memory, base).unwrap().is_none());
    assert_eq!(f.memory.pages.len(), tables);
    assert_eq!(
        f.registry.mapping_for(context.space, base).err(),
        Some(Error::NotMapped)
    );
    assert_eq!(f.registry.pins(context.space).unwrap(), 1);
    assert!(
        f.tree
            .translate(&f.memory, thread_user_stack(1))
            .unwrap()
            .is_some()
    );
    f.tree.unmap(&mut f.memory, base + PAGE).unwrap();
    for id in [k0, k1] {
        f.registry.retire_stack_frame(&edit, id).unwrap();
    }
    assert!(f.registry.finish_stack_edit(&edit).is_ok());
}
#[test]
fn thread_stack_frame_class_and_duplicate_backing_are_checked() {
    let mut f = member_fixture();
    retire_fixture_first(&mut f);
    let fresh = f
        .nodes
        .member_thread(f.active[0].node, f.active[0].context)
        .unwrap();
    let edit = f.nodes.stack_edit(fresh).unwrap();
    let k = f
        .registry
        .insert_stack_frame(&edit, 0x100000, StackKind::Kernel)
        .unwrap();
    f.registry.initialize(k).unwrap();
    assert_eq!(
        map_thread_stack_frames(
            &f.tree,
            &mut f.memory,
            &mut f.registry,
            &edit,
            thread_user_stack(0),
            &[k],
            StackKind::User
        ),
        Err(Error::NotOwned)
    );
    assert_eq!(
        map_thread_stack_frames(
            &f.tree,
            &mut f.memory,
            &mut f.registry,
            &edit,
            KSTACK_BASE + PAGE,
            &[k, k],
            StackKind::Kernel
        ),
        Err(Error::NotOwned)
    );
    assert_eq!(
        map_thread_stack_frames(
            &f.tree,
            &mut f.memory,
            &mut f.registry,
            &edit,
            thread_user_stack(1),
            &[f.frames[1][0]],
            StackKind::User
        ),
        Err(Error::Address)
    );
    assert!(
        f.tree
            .translate(&f.memory, thread_user_stack(0))
            .unwrap()
            .is_none()
    );
    assert_eq!(f.registry.pins(f.active[0].context.space).unwrap(), 1);
}
#[test]
fn recycled_thread_and_frame_generations_cannot_authorize_old_stack_resources() {
    let mut f = member_fixture();
    let old = f.active[0].thread;
    let old_frame = f.frames[0][0];
    retire_fixture_first(&mut f);
    let fresh = f
        .nodes
        .member_thread(f.active[0].node, f.active[0].context)
        .unwrap();
    assert_ne!(old, fresh);
    assert_eq!(old.index(), fresh.index());
    assert_eq!(
        f.nodes.stack_edit(old).err(),
        Some(crate::node::Error::Identity)
    );
    let edit = f.nodes.stack_edit(fresh).unwrap();
    let frame = f
        .registry
        .insert_stack_frame(&edit, 0x100000, StackKind::User)
        .unwrap();
    assert_ne!(old_frame, frame);
    assert_eq!(
        f.registry.releasable_stack_frame(&edit, old_frame),
        Err(Error::NotOwned)
    );
    assert_eq!(
        f.registry.releasable_stack_frame(&edit, frame).unwrap(),
        0x100000
    );
    assert!(matches!(
        f.registry.finish_stack_edit(&edit),
        Err(Error::InUse)
    ));
}
#[test]
fn three_thread_stack_arena_has_bounded_distinct_guarded_ranges() {
    for slot in 0..3 {
        let base = KSTACK_BASE + slot as u64 * KSTACK_STRIDE + PAGE;
        assert_eq!(stack_range(base, 2), Ok(()));
        assert_eq!(stack_range(base - PAGE, 2), Err(Error::Address));
        assert_eq!(thread_user_stack(slot), 0x404000 + slot as u64 * 0x4000);
    }
    assert_eq!(
        stack_range(KSTACK_BASE + 3 * KSTACK_STRIDE + PAGE, 2),
        Err(Error::Address)
    );
    assert_eq!((DATA_FRAMES, MAPPINGS), (16, 64));
}

#[test]
fn address_space_refs_are_separate_from_context_pins_and_block_root_destruction() {
    let mut r = Registry::new();
    let s = r.create_space().unwrap();
    r.bind_root(s, 0x1000).unwrap();
    r.pin_space(s).unwrap();
    r.pin_space(s).unwrap();
    let reference = r.retain_space(s).unwrap();
    assert_eq!(r.object_references(s), Ok(1));
    assert_eq!(r.pins(s), Ok(2));
    r.unpin_space(s).unwrap();
    r.unpin_space(s).unwrap();
    assert_eq!(r.can_allocate(s), Ok(()));
    assert_eq!(r.can_destroy_space(s), Err(Error::InUse));
    assert_eq!(r.clear_root(s, 0x1000), Err(Error::InUse));
    r.release_space_ref(reference).unwrap();
    r.clear_root(s, 0x1000).unwrap();
    r.retire_space(s).unwrap();
}
#[test]
fn space_ref_bounds_stale_generations_and_failed_release_preserve_ownership() {
    let mut r = Registry::new();
    let s = r.create_space().unwrap();
    r.bind_root(s, 0x1000).unwrap();
    let a = r.retain_space(s).unwrap();
    let b = r.retain_space(s).unwrap();
    assert!(matches!(r.retain_space(s), Err(Error::Capacity)));
    r.activate_space(s).unwrap();
    let (e, a) = r.release_space_ref(a).unwrap_err();
    assert_eq!(e, Error::InUse);
    assert_eq!(r.object_references(s), Ok(2));
    r.deactivate_space(s).unwrap();
    r.release_space_ref(a).unwrap();
    r.release_space_ref(b).unwrap();
    r.clear_root(s, 0x1000).unwrap();
    r.retire_space(s).unwrap();
    assert!(matches!(r.retain_space(s), Err(Error::NotOwned)));
    let fresh = r.create_space().unwrap();
    assert_ne!(fresh, s);
    r.bind_root(fresh, 0x2000).unwrap();
    assert!(matches!(r.retain_space(s), Err(Error::NotOwned)));
}
#[test]
fn live_inspection_reads_actual_mapping_changes_and_returns_no_physical_address() {
    use crate::inspection;
    let mut m = Model::new();
    let tree = PageTree::new(&mut m, 48).unwrap();
    let mut r = Registry::new();
    let s = r.create_space().unwrap();
    r.bind_root(s, tree.root).unwrap();
    let reference = r.retain_space(s).unwrap();
    tree.map_user(&mut m, USER_BASE, 0x800000, Permissions::RX)
        .unwrap();
    assert_eq!(
        inspection::inspect(&r, &reference, &m, 48, USER_BASE),
        Ok(11)
    );
    tree.unmap(&mut m, USER_BASE).unwrap();
    assert_eq!(
        inspection::inspect(&r, &reference, &m, 48, USER_BASE),
        Ok(0)
    );
    tree.map_user(&mut m, USER_BASE, 0x900000, Permissions::RW)
        .unwrap();
    assert_eq!(
        inspection::inspect(&r, &reference, &m, 48, USER_BASE),
        Ok(7)
    );
    r.release_space_ref(reference).unwrap();
}
#[test]
fn inspection_rejects_unaligned_kernel_noncanonical_and_out_of_arena_addresses() {
    let mut m = Model::new();
    let tree = PageTree::new(&mut m, 48).unwrap();
    let mut r = Registry::new();
    let s = r.create_space().unwrap();
    r.bind_root(s, tree.root).unwrap();
    let reference = r.retain_space(s).unwrap();
    for va in [
        0,
        USER_BASE - 4096,
        USER_BASE + 1,
        USER_BASE + USER_SIZE,
        0x800000000000,
        0xffff800000000000,
        u64::MAX,
    ] {
        assert!(crate::inspection::inspect(&r, &reference, &m, 48, va).is_err());
    }
    assert_eq!(
        crate::inspection::inspect(&r, &reference, &m, 48, USER_BASE),
        Ok(0)
    );
    r.release_space_ref(reference).unwrap();
}
#[test]
fn inspection_rejects_nonpresent_software_entries_huge_leaves_and_reserved_flags() {
    let mut m = Model::new();
    let tree = PageTree::new(&mut m, 48).unwrap();
    let mut r = Registry::new();
    let s = r.create_space().unwrap();
    r.bind_root(s, tree.root).unwrap();
    let reference = r.retain_space(s).unwrap();
    let path = indices(USER_BASE).unwrap();
    m.write(tree.root, path[0], 512).unwrap();
    assert_eq!(
        crate::inspection::inspect(&r, &reference, &m, 48, USER_BASE),
        Err(Error::Flags)
    );
    m.write(tree.root, path[0], 0).unwrap();
    tree.map_user(&mut m, USER_BASE, 0x800000, Permissions::RW)
        .unwrap();
    let p3 = m.read(tree.root, path[0]).unwrap() & ADDRESS_FIELD;
    let p2 = m.read(p3, path[1]).unwrap() & ADDRESS_FIELD;
    let p1 = m.read(p2, path[2]).unwrap() & ADDRESS_FIELD;
    let old = m.read(p1, path[3]).unwrap();
    for bad in [old | 512, old | 128, old | 8, old | (1 << 55), old & !NX] {
        m.write(p1, path[3], bad).unwrap();
        assert!(crate::inspection::inspect(&r, &reference, &m, 48, USER_BASE).is_err());
    }
    m.write(p1, path[3], old).unwrap();
    let parent = m.read(p2, path[2]).unwrap();
    m.write(p2, path[2], 0x800000 | 0x87 | NX).unwrap();
    assert_eq!(
        crate::inspection::inspect(&r, &reference, &m, 48, USER_BASE),
        Err(Error::Flags)
    );
    m.write(p2, path[2], parent).unwrap();
    r.release_space_ref(reference).unwrap();
}
#[test]
fn inspection_applies_ancestor_permissions_and_accepts_hardware_accessed_dirty_bits() {
    let mut m = Model::new();
    let tree = PageTree::new(&mut m, 48).unwrap();
    tree.map_user(&mut m, USER_BASE, 0x800000, Permissions::RW)
        .unwrap();
    let mut r = Registry::new();
    let s = r.create_space().unwrap();
    r.bind_root(s, tree.root).unwrap();
    let reference = r.retain_space(s).unwrap();
    let path = indices(USER_BASE).unwrap();
    let parent = m.read(tree.root, path[0]).unwrap();
    m.write(tree.root, path[0], (parent | NX | 32) & !2)
        .unwrap();
    assert_eq!(
        crate::inspection::inspect(&r, &reference, &m, 48, USER_BASE),
        Ok(3)
    );
    m.write(tree.root, path[0], parent | 32).unwrap();
    let p3 = m.read(tree.root, path[0]).unwrap() & ADDRESS_FIELD;
    let p2 = m.read(p3, path[1]).unwrap() & ADDRESS_FIELD;
    let p1 = m.read(p2, path[2]).unwrap() & ADDRESS_FIELD;
    let leaf = m.read(p1, path[3]).unwrap();
    m.write(p1, path[3], leaf | 32 | 64).unwrap();
    assert_eq!(
        crate::inspection::inspect(&r, &reference, &m, 48, USER_BASE),
        Ok(7)
    );
    r.release_space_ref(reference).unwrap();
}

fn object_fixture() -> (
    Registry,
    Model,
    PageTree,
    PageTree,
    SpaceId,
    SpaceId,
    [FrameId; 2],
    MemoryObjectId,
) {
    let mut registry = Registry::new();
    let mut memory = Model::new();
    let a = PageTree::new(&mut memory, 48).unwrap();
    let b = PageTree::new(&mut memory, 48).unwrap();
    let sa = registry.create_space().unwrap();
    let sb = registry.create_space().unwrap();
    registry.bind_root(sa, a.root).unwrap();
    registry.bind_root(sb, b.root).unwrap();
    let frames = [
        registry.insert(0x100000).unwrap(),
        registry.insert(0x101000).unwrap(),
    ];
    for id in frames {
        registry.initialize(id).unwrap();
    }
    let object = registry.create_memory_object(&frames).unwrap();
    (registry, memory, a, b, sa, sb, frames, object)
}

#[test]
fn memory_object_shared_mapping_lifetime_is_independent_of_space() {
    for reverse in [false, true] {
        let (mut r, mut m, a, b, sa, sb, frames, obj) = object_fixture();
        let aa = r.authorize_memory_mapping(obj, sa, true).unwrap();
        let ab = r.authorize_memory_mapping(obj, sb, false).unwrap();
        map_memory_object(&a, &mut m, &mut r, &aa, 0, 0x410000, 2, Permissions::RW).unwrap();
        map_memory_object(&b, &mut m, &mut r, &ab, 0, 0x420000, 2, Permissions::R).unwrap();
        assert_eq!(r.memory_object_counts(obj), Ok((1, 4, 2)));
        assert_eq!(r.mapping_for(sb, 0x420000).unwrap().frame, frames[0]);
        assert_eq!(
            a.translate(&m, 0x410000).unwrap().unwrap().physical,
            b.translate(&m, 0x420000).unwrap().unwrap().physical
        );
        assert_eq!(
            b.translate(&m, 0x420000).unwrap().unwrap().permissions,
            Permissions::R
        );
        r.release_memory_object(obj).unwrap();
        assert_eq!(r.collect_memory_object(obj), Err(Error::InUse));
        let (first, fs, fv, second, ss, sv) = if reverse {
            (&b, sb, 0x420000, &a, sa, 0x410000)
        } else {
            (&a, sa, 0x410000, &b, sb, 0x420000)
        };
        for i in 0..2 {
            unmap_memory_object(first, &mut m, &mut r, fs, fv + i * PAGE).unwrap();
        }
        assert_eq!(r.memory_object_counts(obj), Ok((0, 2, 2)));
        assert!(second.translate(&m, sv).unwrap().is_some());
        assert_eq!(r.releasable(frames[0]), Err(Error::InUse));
        for i in 0..2 {
            unmap_memory_object(second, &mut m, &mut r, ss, sv + i * PAGE).unwrap();
        }
        assert_eq!(
            unmap_memory_object(second, &mut m, &mut r, ss, sv),
            Err(Error::NotMapped)
        );
        assert_eq!(
            r.collect_memory_object(obj),
            Ok([Some(frames[0]), Some(frames[1])])
        );
        assert_eq!(r.collect_memory_object(obj), Err(Error::NotOwned));
        for frame in frames {
            r.retire(frame).unwrap();
            assert_eq!(r.retire(frame), Err(Error::NotOwned));
        }
    }
}

#[test]
fn memory_object_live_contract_shares_pages_with_distinct_rights() {
    let (mut r, mut m, a, b, sa, sb, frames, obj) = object_fixture();
    let aa = r.authorize_memory_mapping(obj, sa, true).unwrap();
    let ab = r.authorize_memory_mapping(obj, sb, false).unwrap();
    map_memory_object(&a, &mut m, &mut r, &aa, 0, 0x600000, 2, Permissions::RW).unwrap();
    map_memory_object(&b, &mut m, &mut r, &ab, 0, 0x602000, 2, Permissions::R).unwrap();
    for i in 0..2 {
        let left = a
            .translate(&m, 0x600000 + i as u64 * PAGE)
            .unwrap()
            .unwrap();
        let right = b
            .translate(&m, 0x602000 + i as u64 * PAGE)
            .unwrap()
            .unwrap();
        assert_eq!(left.physical, r.physical(frames[i]).unwrap());
        assert_eq!(right.physical, left.physical);
        assert_eq!(left.permissions, Permissions::RW);
        assert_eq!(right.permissions, Permissions::R);
        assert!(!left.permissions.execute && !right.permissions.execute);
        assert!(left.user && right.user);
        assert_eq!((left.page_size, right.page_size), (PAGE, PAGE));
        assert_eq!(
            r.mapping_for(sa, 0x600000 + i as u64 * PAGE)
                .unwrap()
                .object,
            MappingObject::Shared(obj)
        );
        assert_eq!(
            r.mapping_for(sb, 0x602000 + i as u64 * PAGE)
                .unwrap()
                .object,
            MappingObject::Shared(obj)
        );
        assert_eq!(
            r.mapping_for(sa, 0x600000 + i as u64 * PAGE).unwrap().frame,
            frames[i]
        );
        assert_eq!(
            r.mapping_for(sb, 0x602000 + i as u64 * PAGE).unwrap().frame,
            frames[i]
        );
    }
    assert_eq!(a.translate(&m, 0x602000), Ok(None));
    assert_eq!(b.translate(&m, 0x600000), Ok(None));
    assert_eq!(r.memory_object_counts(obj), Ok((1, 4, 2)));
    // The read-only grant cannot be re-authorized as writable, and one Node's
    // authority cannot act on the sibling root.
    assert_eq!(
        map_memory_object(&b, &mut m, &mut r, &ab, 0, 0x604000, 2, Permissions::RW),
        Err(Error::Flags)
    );
    assert_eq!(
        map_memory_object(&b, &mut m, &mut r, &aa, 0, 0x604000, 1, Permissions::R),
        Err(Error::NotOwned)
    );
    assert_eq!(r.memory_object_counts(obj), Ok((1, 4, 2)));
}

#[test]
fn memory_object_live_teardown_orders_preserve_the_survivor_and_collect_once() {
    for reverse in [false, true] {
        let (mut r, mut m, a, b, sa, sb, frames, obj) = object_fixture();
        let baseline = m.pages.len();
        let aa = r.authorize_memory_mapping(obj, sa, true).unwrap();
        let ab = r.authorize_memory_mapping(obj, sb, false).unwrap();
        map_memory_object(&a, &mut m, &mut r, &aa, 0, 0x600000, 2, Permissions::RW).unwrap();
        map_memory_object(&b, &mut m, &mut r, &ab, 0, 0x602000, 2, Permissions::R).unwrap();
        assert_eq!(r.memory_object_counts(obj), Ok((1, 4, 2)));
        r.release_memory_object(obj).unwrap();
        let (first, fs, fv, second, ss, sv) = if reverse {
            (&b, sb, 0x602000, &a, sa, 0x600000)
        } else {
            (&a, sa, 0x600000, &b, sb, 0x602000)
        };
        for i in 0..2usize {
            unmap_memory_object(first, &mut m, &mut r, fs, fv + i as u64 * PAGE).unwrap();
        }
        assert_eq!(r.memory_object_counts(obj), Ok((0, 2, 2)));
        for i in 0..2usize {
            assert_eq!(
                second
                    .translate(&m, sv + i as u64 * PAGE)
                    .unwrap()
                    .unwrap()
                    .physical,
                r.physical(frames[i]).unwrap()
            );
        }
        assert_eq!(r.releasable(frames[0]), Err(Error::InUse));
        for i in 0..2usize {
            unmap_memory_object(second, &mut m, &mut r, ss, sv + i as u64 * PAGE).unwrap();
        }
        assert_eq!(
            unmap_memory_object(second, &mut m, &mut r, ss, sv),
            Err(Error::NotMapped)
        );
        assert_eq!(m.pages.len(), baseline);
        assert_eq!(
            r.collect_memory_object(obj),
            Ok([Some(frames[0]), Some(frames[1])])
        );
        assert_eq!(r.collect_memory_object(obj), Err(Error::NotOwned));
        for frame in frames {
            r.retire(frame).unwrap();
            assert_eq!(r.retire(frame), Err(Error::NotOwned));
        }
    }
}

#[test]
fn memory_object_live_private_thread_leaves_never_join_shared_backing() {
    let (mut r, mut m, a, b, sa, sb, frames, obj) = object_fixture();
    let a1 = r.insert_for(sa, 0x102000).unwrap();
    let a2 = r.insert_for(sa, 0x103000).unwrap();
    r.initialize(a1).unwrap();
    r.initialize(a2).unwrap();
    map_user_frames_for(&a, &mut m, &mut r, sa, 0x410000, &[a1], Permissions::RW).unwrap();
    map_user_frames_for(&a, &mut m, &mut r, sa, 0x411000, &[a2], Permissions::RW).unwrap();
    assert_eq!(r.frame_owner(a1), Ok(FrameOwner::Space(sa)));
    assert_eq!(r.frame_owner(a2), Ok(FrameOwner::Space(sa)));
    assert_eq!(r.create_memory_object(&[a1]), Err(Error::NotOwned));
    assert_eq!(
        map_user_frames_for(&b, &mut m, &mut r, sb, 0x602000, &[a1], Permissions::R),
        Err(Error::NotOwned)
    );
    assert_eq!(
        map_user_frames_for(&a, &mut m, &mut r, sb, 0x602000, &[a1], Permissions::R),
        Err(Error::NotOwned)
    );
    assert_eq!(r.releasable(a1), Err(Error::InUse));
    assert_eq!(r.releasable(a2), Err(Error::InUse));
    assert!(a.translate(&m, 0x410000).unwrap().is_some());
    assert!(a.translate(&m, 0x411000).unwrap().is_some());
    assert_eq!(r.memory_object_counts(obj), Ok((1, 0, 2)));
    // The object still owns exactly its two untouched pages and is collectible
    // while unrelated private Thread leaves remain mapped in the same Node.
    for frame in frames {
        assert_eq!(r.frame_owner(frame), Ok(FrameOwner::MemoryObject(obj)));
    }
    r.release_memory_object(obj).unwrap();
    assert_eq!(
        r.collect_memory_object(obj),
        Ok([Some(frames[0]), Some(frames[1])])
    );
}

#[test]
fn memory_object_authority_and_private_isolation_are_enforced() {
    let (mut r, mut m, a, b, sa, sb, frames, obj) = object_fixture();
    let aa = r.authorize_memory_mapping(obj, sa, true).unwrap();
    let ab = r.authorize_memory_mapping(obj, sb, false).unwrap();
    assert_eq!(
        map_user_frames_for(&b, &mut m, &mut r, sb, 0x410000, &frames, Permissions::RW),
        Err(Error::NotOwned)
    );
    assert_eq!(
        map_memory_object(&b, &mut m, &mut r, &aa, 0, 0x410000, 1, Permissions::R),
        Err(Error::NotOwned)
    );
    assert_eq!(
        map_memory_object(&b, &mut m, &mut r, &ab, 0, 0x410000, 1, Permissions::RW),
        Err(Error::Flags)
    );
    assert_eq!(
        map_memory_object(&a, &mut m, &mut r, &aa, 0, 0x410000, 1, Permissions::RX),
        Err(Error::Flags)
    );
    assert_eq!(
        map_memory_object(&a, &mut m, &mut r, &aa, 0, 0x410001, 1, Permissions::R),
        Err(Error::Alignment)
    );
    assert_eq!(
        map_memory_object(
            &a,
            &mut m,
            &mut r,
            &aa,
            usize::MAX,
            0x410000,
            1,
            Permissions::R
        ),
        Err(Error::Address)
    );
    map_memory_object(&a, &mut m, &mut r, &aa, 1, 0x410000, 1, Permissions::R).unwrap();
    assert_eq!(
        map_memory_object(&a, &mut m, &mut r, &aa, 0, 0x410000, 1, Permissions::R),
        Err(Error::AlreadyMapped)
    );
    assert_eq!(r.remove_for(sa, 0x410000), Err(Error::NotOwned));
    let private = r.insert_for(sa, 0x102000).unwrap();
    r.initialize(private).unwrap();
    assert_eq!(
        map_user_frames_for(&b, &mut m, &mut r, sb, 0x420000, &[private], Permissions::R),
        Err(Error::NotOwned)
    );
    assert_eq!(r.memory_object_counts(obj), Ok((1, 1, 2)));
}

#[test]
fn memory_object_second_page_failure_rolls_back_tables_and_records() {
    let (mut r, mut m, a, _, sa, _, _, obj) = object_fixture();
    let authority = r.authorize_memory_mapping(obj, sa, true).unwrap();
    let before = m.pages.len();
    m.budget = Some(3);
    assert_eq!(
        map_memory_object(
            &a,
            &mut m,
            &mut r,
            &authority,
            0,
            0x5ff000,
            2,
            Permissions::RW
        ),
        Err(Error::Exhausted)
    );
    assert_eq!(m.pages.len(), before);
    assert_eq!(a.translate(&m, 0x5ff000), Ok(None));
    assert_eq!(a.translate(&m, 0x600000), Ok(None));
    assert_eq!(r.memory_object_counts(obj), Ok((1, 0, 2)));
}

#[test]
fn memory_object_generation_and_construction_failures_preserve_ownership() {
    let (mut r, _, _, _, _, _, frames, obj) = object_fixture();
    assert_eq!(r.create_memory_object(&[]), Err(Error::Address));
    assert_eq!(r.create_memory_object(&frames), Err(Error::NotOwned));
    assert_eq!(r.frame_owner(frames[0]), Ok(FrameOwner::MemoryObject(obj)));
    r.release_memory_object(obj).unwrap();
    assert_eq!(r.release_memory_object(obj), Err(Error::NotOwned));
    r.collect_memory_object(obj).unwrap();
    let next = r.create_memory_object(&frames[..1]).unwrap();
    assert_ne!(obj, next);
    assert_eq!(r.memory_object_counts(obj), Err(Error::NotOwned));
    assert_eq!(r.memory_object_frame(next, 1), Err(Error::Address));
}

#[test]
fn memory_object_uses_the_actual_frame_allocator_and_reclaims_once() {
    use crate::memory::{FrameAllocator, MemoryMap, Region, Reservation};
    let regions = [Region {
        base: 0x1000,
        length: 16 * PAGE,
        kind: 0,
    }];
    let map = MemoryMap::new(&regions, Reservation::new(0x100000, PAGE).unwrap()).unwrap();
    let mut bitmap = [0u64; 1];
    let mut allocator = FrameAllocator::new(&map, &mut bitmap).unwrap();
    let mut r = Registry::new();
    let pa = allocator.allocate().unwrap();
    let pb = allocator.allocate().unwrap();
    assert_ne!(pa, pb);
    let a = r.insert(pa).unwrap();
    let b = r.insert(pb).unwrap();
    r.initialize(a).unwrap();
    r.initialize(b).unwrap();
    let obj = r.create_memory_object(&[a, b]).unwrap();
    assert_eq!(allocator.stats().allocated, 2);
    r.release_memory_object(obj).unwrap();
    for id in r.collect_memory_object(obj).unwrap().into_iter().flatten() {
        let physical = r.releasable(id).unwrap();
        allocator.free(physical).unwrap();
        r.retire(id).unwrap();
        assert!(allocator.free(physical).is_err());
    }
    assert_eq!(allocator.stats().allocated, 0);
}

#[test]
fn memory_object_capacity_rejection_is_atomic() {
    let (mut r, mut m, a, _, sa, _, _, obj) = object_fixture();
    let grant = r.authorize_memory_mapping(obj, sa, true).unwrap();
    for i in 0..MAPPINGS {
        let frame = r.memory_object_frame(obj, 0).unwrap();
        r.mappings[i] = Mapping {
            space: sa,
            address: 0x1000000 + i as u64 * PAGE,
            frame,
            permissions: Permissions::R,
            object: MappingObject::Shared(obj),
            live: true,
        };
    }
    let before = m.pages.len();
    assert_eq!(
        map_memory_object(&a, &mut m, &mut r, &grant, 1, 0x410000, 1, Permissions::R),
        Err(Error::Capacity)
    );
    assert_eq!(m.pages.len(), before);
    assert_eq!(r.memory_object_counts(obj), Ok((1, MAPPINGS, 2)));
}

#[test]
fn memory_object_repeated_reuse_and_pool_exhaustion() {
    let mut r = Registry::new();
    let mut frames = Vec::new();
    for i in 0..=MEMORY_OBJECTS {
        let f = r.insert(0x100000 + i as u64 * PAGE).unwrap();
        r.initialize(f).unwrap();
        frames.push(f);
    }
    let mut objects = Vec::new();
    for f in &frames[..MEMORY_OBJECTS] {
        objects.push(r.create_memory_object(&[*f]).unwrap());
    }
    assert_eq!(
        r.create_memory_object(&frames[MEMORY_OBJECTS..]),
        Err(Error::Capacity)
    );
    assert_eq!(
        r.frame_owner(frames[MEMORY_OBJECTS]),
        Ok(FrameOwner::Space(SpaceId::KERNEL))
    );
    for obj in objects {
        r.release_memory_object(obj).unwrap();
        r.collect_memory_object(obj).unwrap();
    }
    let mut stale = None;
    for _ in 0..100 {
        let id = r.create_memory_object(&frames[..1]).unwrap();
        if let Some(old) = stale {
            assert_eq!(r.memory_object_counts(old), Err(Error::NotOwned));
        }
        r.release_memory_object(id).unwrap();
        r.collect_memory_object(id).unwrap();
        stale = Some(id);
    }
}

#[test]
fn memory_object_mapping_prevents_space_retirement_until_detached() {
    let (mut r, mut m, a, _, sa, _, _, obj) = object_fixture();
    let grant = r.authorize_memory_mapping(obj, sa, true).unwrap();
    map_memory_object(&a, &mut m, &mut r, &grant, 0, 0x410000, 1, Permissions::RW).unwrap();
    assert_eq!(r.has_memory_object_mappings(sa), Ok(true));
    assert_eq!(r.clear_root(sa, a.root), Err(Error::InUse));
    unmap_memory_object(&a, &mut m, &mut r, sa, 0x410000).unwrap();
    assert_eq!(r.has_memory_object_mappings(sa), Ok(false));
    r.clear_root(sa, a.root).unwrap();
    r.retire_space(sa).unwrap();
    assert_eq!(r.memory_object_counts(obj), Ok((1, 0, 2)));
    assert_eq!(
        map_memory_object(&a, &mut m, &mut r, &grant, 0, 0x410000, 1, Permissions::R),
        Err(Error::NotOwned)
    );
}

#[test]
fn memory_object_teardown_leaf_removal_retains_table_inventory() {
    let (mut r, mut m, a, b, sa, sb, frames, obj) = object_fixture();
    let aa = r.authorize_memory_mapping(obj, sa, true).unwrap();
    let ab = r.authorize_memory_mapping(obj, sb, false).unwrap();
    map_memory_object(&a, &mut m, &mut r, &aa, 0, 0x410000, 2, Permissions::RW).unwrap();
    map_memory_object(&b, &mut m, &mut r, &ab, 0, 0x420000, 2, Permissions::R).unwrap();
    let tables = m.pages.len();
    r.release_memory_object(obj).unwrap();
    for i in 0..2 {
        validate_memory_object_mapping(&a, &m, &r, sa, 0x410000 + i * PAGE).unwrap();
    }
    for i in 0..2 {
        unmap_memory_object_preserving_tables(&a, &mut m, &mut r, sa, 0x410000 + i * PAGE).unwrap();
    }
    assert_eq!(m.pages.len(), tables);
    assert_eq!(r.memory_object_counts(obj), Ok((0, 2, 2)));
    assert_eq!(
        b.translate(&m, 0x420000).unwrap().unwrap().physical,
        r.physical(frames[0]).unwrap()
    );
    assert_eq!(
        unmap_memory_object_preserving_tables(&a, &mut m, &mut r, sa, 0x410000),
        Err(Error::NotMapped)
    );
    assert_eq!(r.collectible_memory_object(obj), Err(Error::InUse));
    for i in 0..2 {
        unmap_memory_object_preserving_tables(&b, &mut m, &mut r, sb, 0x420000 + i * PAGE).unwrap();
    }
    assert_eq!(
        r.collectible_memory_object(obj),
        Ok([Some(frames[0]), Some(frames[1])])
    );
    r.collect_memory_object(obj).unwrap();
    assert_eq!(r.collectible_memory_object(obj), Err(Error::NotOwned));
}

#[test]
fn memory_object_teardown_preflight_rejects_pins_and_wrong_root_without_mutation() {
    let (mut r, mut m, a, b, sa, _, _, obj) = object_fixture();
    let grant = r.authorize_memory_mapping(obj, sa, true).unwrap();
    map_memory_object(&a, &mut m, &mut r, &grant, 0, 0x410000, 1, Permissions::RW).unwrap();
    assert_eq!(
        validate_memory_object_mapping(&b, &m, &r, sa, 0x410000),
        Err(Error::NotOwned)
    );
    r.pin_space(sa).unwrap();
    assert_eq!(
        unmap_memory_object_preserving_tables(&a, &mut m, &mut r, sa, 0x410000),
        Err(Error::InUse)
    );
    assert_eq!(r.memory_object_counts(obj), Ok((1, 1, 2)));
    assert!(a.translate(&m, 0x410000).unwrap().is_some());
    r.unpin_space(sa).unwrap();
    unmap_memory_object_preserving_tables(&a, &mut m, &mut r, sa, 0x410000).unwrap();
}

#[test]
fn memory_object_rollback_backend_failure_is_fail_stop() {
    let (mut r, mut m, a, _, sa, _, _, obj) = object_fixture();
    let authority = r.authorize_memory_mapping(obj, sa, true).unwrap();
    m.budget = Some(3);
    m.fail_clear = true;
    let failure = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = map_memory_object(
            &a,
            &mut m,
            &mut r,
            &authority,
            0,
            0x5ff000,
            2,
            Permissions::RW,
        );
    }));
    assert!(failure.is_err());
    // The exposed host model is deliberately poisoned, not reusable. No
    // mapping metadata was published before the fail-stop.
    assert_eq!(r.memory_object_counts(obj), Ok((1, 0, 2)));
}

#[test]
fn memory_object_table_requirement_deduplicates_intermediate_paths() {
    let mut m = Model::new();
    let tree = PageTree::new(&mut m, 48).unwrap();
    assert_eq!(tree.required_user_tables(&m, 0x410000, 2), Ok(3));
    assert_eq!(tree.required_user_tables(&m, 0x5ff000, 2), Ok(4));
    tree.map_user(&mut m, 0x410000, 0x100000, Permissions::RW)
        .unwrap();
    let before = m.pages.len();
    assert_eq!(tree.required_user_tables(&m, 0x420000, 2), Ok(0));
    assert_eq!(tree.required_user_tables(&m, 0x600000, 2), Ok(1));
    assert_eq!(tree.required_user_tables(&m, 0x5ff000, 2), Ok(1));
    assert_eq!(m.pages.len(), before);
    assert!(tree.required_user_tables(&m, 0x800000, 1).is_err());
}

#[test]
fn memory_object_new_region_failure_and_final_unmap_conserve_tables() {
    let (mut r, mut m, a, _, sa, _, frames, obj) = object_fixture();
    let grant = r.authorize_memory_mapping(obj, sa, true).unwrap();
    // Populate the original user leaf table with an unrelated private page.
    let private = r.insert_for(sa, 0x102000).unwrap();
    r.initialize(private).unwrap();
    map_user_frames_for(
        &a,
        &mut m,
        &mut r,
        sa,
        0x410000,
        &[private],
        Permissions::RW,
    )
    .unwrap();
    let before = m.pages.len();
    m.budget = Some(0);
    assert_eq!(
        map_memory_object(&a, &mut m, &mut r, &grant, 0, 0x600000, 2, Permissions::RW),
        Err(Error::Exhausted)
    );
    assert_eq!(m.pages.len(), before);
    assert_eq!(r.memory_object_counts(obj), Ok((1, 0, 2)));
    m.budget = None;
    map_memory_object(&a, &mut m, &mut r, &grant, 0, 0x600000, 2, Permissions::RW).unwrap();
    assert_eq!(m.pages.len(), before + 1);
    assert_eq!(
        a.translate(&m, 0x600000).unwrap().unwrap().physical,
        r.physical(frames[0]).unwrap()
    );
    unmap_memory_object(&a, &mut m, &mut r, sa, 0x600000).unwrap();
    assert_eq!(m.pages.len(), before + 1);
    unmap_memory_object(&a, &mut m, &mut r, sa, 0x601000).unwrap();
    assert_eq!(m.pages.len(), before);
    assert_eq!(r.mapping_for(sa, 0x410000).unwrap().frame, private);
}

#[test]
fn memory_object_stale_grant_cannot_map_a_recycled_object() {
    let (mut r, mut m, a, _, sa, _, frames, obj) = object_fixture();
    let grant = r.authorize_memory_mapping(obj, sa, true).unwrap();
    r.release_memory_object(obj).unwrap();
    r.collect_memory_object(obj).unwrap();
    let next = r.create_memory_object(&frames).unwrap();
    assert_ne!(obj, next);
    let before = m.pages.len();
    assert_eq!(
        map_memory_object(&a, &mut m, &mut r, &grant, 0, 0x600000, 2, Permissions::RW),
        Err(Error::NotOwned)
    );
    assert_eq!(m.pages.len(), before);
    assert_eq!(r.memory_object_counts(next), Ok((1, 0, 2)));
}

#[test]
fn memory_object_wrong_provenance_rejects_unmap_before_leaf_mutation() {
    let (mut r, mut m, a, _, sa, _, _, obj) = object_fixture();
    let grant = r.authorize_memory_mapping(obj, sa, true).unwrap();
    map_memory_object(&a, &mut m, &mut r, &grant, 0, 0x600000, 1, Permissions::RW).unwrap();
    let record = r
        .mappings
        .iter_mut()
        .find(|m| m.live && m.address == 0x600000)
        .unwrap();
    record.object = MappingObject::Shared(MemoryObjectId {
        slot: obj.slot,
        generation: obj.generation + 1,
    });
    assert_eq!(
        unmap_memory_object(&a, &mut m, &mut r, sa, 0x600000),
        Err(Error::Corrupt)
    );
    assert!(a.translate(&m, 0x600000).unwrap().is_some());
}

#[test]
fn memory_object_second_leaf_write_failure_releases_new_table() {
    let (mut r, mut m, a, _, sa, _, _, obj) = object_fixture();
    let private = r.insert_for(sa, 0x102000).unwrap();
    r.initialize(private).unwrap();
    map_user_frames_for(
        &a,
        &mut m,
        &mut r,
        sa,
        0x410000,
        &[private],
        Permissions::RW,
    )
    .unwrap();
    let grant = r.authorize_memory_mapping(obj, sa, true).unwrap();
    let before = m.pages.len();
    m.reject_leaf_index = Some(1);
    assert_eq!(
        map_memory_object(&a, &mut m, &mut r, &grant, 0, 0x600000, 2, Permissions::RW),
        Err(Error::Exhausted)
    );
    assert!(m.reject_leaf_index.is_none());
    assert_eq!(m.pages.len(), before);
    assert_eq!(a.translate(&m, 0x600000), Ok(None));
    assert_eq!(a.translate(&m, 0x601000), Ok(None));
    assert_eq!(r.memory_object_counts(obj), Ok((1, 0, 2)));
}

#[test]
fn memory_object_exhausted_generation_is_retired_permanently() {
    let (mut r, _, _, _, _, _, frames, object) = object_fixture();
    r.release_memory_object(object).unwrap();
    r.collect_memory_object(object).unwrap();
    r.objects[object.slot].generation = u64::MAX;
    let next = r.create_memory_object(&frames).unwrap();
    assert_ne!(next.slot, object.slot);
    assert_eq!(r.memory_object_counts(object), Err(Error::NotOwned));
}

//! Architecture half of a tracked mapping transaction. K10 removes only owned
//! leaf PTEs while CPUs run; table/frame release waits for shootdown completion.
use super::*;
use neu_kernel::{capability as cap, shootdown};

impl Paging<'_, '_> {
    pub fn map_authority(
        &mut self,
        space: &mut UserSpace,
        guard: &mut cap::Guard<'_>,
        base: u64,
        pages: usize,
        permissions: Permissions,
    ) -> cap::Result<()> {
        let request = cap::MapRequest {
            space: space.id,
            root: space.root,
            offset: 0,
            base,
            pages,
            permissions,
        };
        guard.map_tracked(request, |permit| {
            let grant = permit.authority(&self.registry)?;
            let r = permit.request();
            self.map_memory_backing(space, &grant, r.offset, r.base, r.pages, r.permissions)
        })
    }
    pub fn authority_memory_frames(&self, object: MemoryObjectId) -> Result<[u64; 2]> {
        let mut frames = [0; 2];
        for (index, out) in frames.iter_mut().enumerate() {
            *out = self
                .registry
                .physical(self.registry.memory_object_frame(object, index)?)?;
        }
        Ok(frames)
    }
    pub fn authority_mapping_offsets(&self) -> [u64; 3] {
        self.registry.authority_mapping_layout()
    }
    pub fn release_memory_creator(&mut self, object: MemoryObjectId) -> Result<()> {
        self.registry.release_memory_object(object)
    }
    pub fn revoke_memory_mapping(
        &mut self,
        space: &UserSpace,
        binding: cap::MappingBinding,
        request: shootdown::Request,
    ) -> Result<(u64, u64)> {
        if current_root() != self.root()
            || space.teardown != UserTeardown::Live
            || binding.space() != space.id
            || binding.root() != space.root
            || binding.address() != request.address
        {
            return Err(Error::NotOwned);
        }
        self.registry
            .validate_mapping_revoke(space.id, binding.grant(), request)?;
        let record = self.registry.mapping_for(space.id, request.address)?;
        let tree = PageTree {
            root: space.root,
            bits: self.tree.bits,
        };
        let actual = tree
            .translate(&self.memory, request.address)?
            .ok_or(Error::NotMapped)?;
        if !actual.user
            || actual.page_size != PAGE
            || actual.permissions != record.permissions
            || actual.physical != self.registry.physical(record.frame)?
        {
            return Err(Error::Corrupt);
        }
        let path = logic::indices(request.address)?;
        let mut table = space.root;
        for index in &path[..3] {
            table = self.memory.read(table, *index)? & ADDRESS_FIELD;
        }
        if !space.tables[self.memory.slot(table)?] {
            return Err(Error::NotOwned);
        }
        let pointer = unsafe { self.memory.pointer(table)?.add(path[3]) };
        // SAFETY: the aligned leaf is owned by this still-pinned root. The
        // transaction serializes software writers; swap cannot restore an A/D
        // update after PRESENT is cleared. No page/table is freed here.
        let leaf = unsafe { &*pointer.cast::<core::sync::atomic::AtomicU64>() };
        let before = leaf.swap(0, core::sync::atomic::Ordering::AcqRel);
        self.registry
            .mark_mapping_revoke(space.id, binding.grant(), request)
            .expect("preflighted tracked PTE removal");
        Ok((table + path[3] as u64 * 8, before))
    }
    pub fn complete_authority_mapping(
        &mut self,
        space: &UserSpace,
        objects: &mut cap::Objects,
        receipt: &shootdown::Completion,
    ) -> cap::Result<()> {
        let request = receipt.request();
        if request.root != space.root {
            return Err(cap::Error::Owner);
        }
        let tree = PageTree {
            root: space.root,
            bits: self.tree.bits,
        };
        if tree
            .translate(&self.memory, request.address)
            .map_err(|_| cap::Error::Object)?
            .is_some()
        {
            return Err(cap::Error::InUse);
        }
        objects.finish_mapping_revoke(&mut self.registry, receipt)
    }
    pub fn cleanup_authority_mappings(
        &mut self,
        space: &mut UserSpace,
        objects: &mut cap::Objects,
    ) -> Result<()> {
        let mut addresses = [None; logic::MAPPINGS];
        for (out, mapping) in addresses
            .iter_mut()
            .zip(self.registry.memory_object_mappings(space.id)?)
        {
            *out = Some(mapping.address);
        }
        for address in addresses.into_iter().flatten() {
            self.unmap_memory_backing(space, address)?;
            objects
                .release_unmapped(&self.registry, space.id, address)
                .map_err(|_| Error::Corrupt)?;
        }
        Ok(())
    }
}

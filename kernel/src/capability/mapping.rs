//! Mapping ownership remains attached to its authorizing grant until a verified
//! unmap. Mapping publication and lineage publication share one transaction.
use super::*;
use crate::{paging, shootdown};

#[derive(Clone, Copy, Debug)]
pub struct MapRequest {
    pub space: paging::SpaceId,
    pub root: u64,
    pub offset: usize,
    pub base: u64,
    pub pages: usize,
    pub permissions: paging::Permissions,
}

/// The adapter receives fixed, validated authority and bounds, never a raw
/// reference that could be reused after this guarded publication transaction.
pub struct MapPermit<'a> {
    reference: &'a paging::MemoryObjectRef,
    grant: GrantId,
    request: MapRequest,
}
/// A borrowed view cannot outlive the guarded publication transaction. The
/// underlying authority is not Copy and cannot be extracted from this view.
pub struct TrackedMapAuthority<'a> {
    authority: paging::MemoryMapAuthority,
    permit: core::marker::PhantomData<&'a MapPermit<'a>>,
}
impl core::ops::Deref for TrackedMapAuthority<'_> {
    type Target = paging::MemoryMapAuthority;
    fn deref(&self) -> &Self::Target {
        &self.authority
    }
}
impl MapPermit<'_> {
    pub fn request(&self) -> MapRequest {
        self.request
    }
    pub fn authority(
        &self,
        registry: &paging::Registry,
    ) -> paging::Result<TrackedMapAuthority<'_>> {
        if registry.root(self.request.space)? != self.request.root {
            return Err(paging::Error::NotOwned);
        }
        let mut authority = registry.authorize_capability_mapping(
            self.reference,
            self.request.space,
            self.request.permissions.write,
        )?;
        authority.grant = self.grant;
        authority.bounds = Some((
            self.request.offset,
            self.request.base,
            self.request.pages,
            self.request.permissions,
        ));
        Ok(TrackedMapAuthority {
            authority,
            permit: core::marker::PhantomData,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct MappingBinding {
    live: bool,
    grant: GrantId,
    object: ObjectId,
    space: paging::SpaceId,
    root: u64,
    address: u64,
    epoch: u64,
    targets: u64,
}
impl MappingBinding {
    pub(super) const EMPTY: Self = Self {
        live: false,
        grant: GrantId::NONE,
        object: ObjectId::EMPTY,
        space: paging::SpaceId::KERNEL,
        root: 0,
        address: 0,
        epoch: 0,
        targets: 0,
    };
    pub fn grant(self) -> GrantId {
        self.grant
    }
    pub fn space(self) -> paging::SpaceId {
        self.space
    }
    pub fn root(self) -> u64 {
        self.root
    }
    pub fn address(self) -> u64 {
        self.address
    }
    pub fn epoch(self) -> u64 {
        self.epoch
    }
}

impl Guard<'_> {
    pub fn map_tracked(
        &mut self,
        request: MapRequest,
        map: impl FnOnce(&MapPermit<'_>) -> paging::Result<()>,
    ) -> Result<()> {
        if self.grant == GrantId::NONE {
            return Err(Error::Rights);
        }
        self.objects
            .lineage
            .validate(self.grant, self.id, self.actual)?;
        let required = Rights::MAP.union(if request.permissions.write {
            Rights::READ_WRITE
        } else {
            Rights::READ
        });
        if request.permissions.execute || !self.actual.contains(required) {
            return Err(Error::Rights);
        }
        paging::user_range(request.base, request.pages).map_err(|_| Error::Rights)?;
        if request.pages > paging::MEMORY_OBJECT_PAGES || request.root == 0 {
            return Err(Error::Capacity);
        }
        if self.objects.mappings.iter().filter(|m| !m.live).count() < request.pages {
            return Err(Error::Capacity);
        }
        for offset in 0..request.pages {
            let address = request.base + offset as u64 * paging::PAGE;
            if self
                .objects
                .mappings
                .iter()
                .any(|m| m.live && m.space == request.space && m.address == address)
            {
                return Err(Error::InUse);
            }
        }
        self.objects
            .lineage
            .can_retain(self.grant, Use::Mapping, request.pages as u32)?;
        self.objects
            .object(self.id)?
            .pins
            .checked_add(request.pages as u32)
            .ok_or(Error::Capacity)?;
        let Value::MemoryObject(reference) = &self.objects.object(self.id)?.value else {
            return Err(Error::Type);
        };
        let permit = MapPermit {
            reference,
            grant: self.grant,
            request,
        };
        map(&permit).map_err(|e| match e {
            paging::Error::Capacity | paging::Error::Exhausted => Error::Capacity,
            paging::Error::AlreadyMapped => Error::InUse,
            _ => Error::Rights,
        })?;
        self.objects
            .lineage
            .retain(self.grant, Use::Mapping, request.pages as u32)
            .expect("preflighted mapping lineage");
        for offset in 0..request.pages {
            self.objects.pin(self.id).expect("preflighted mapping pin");
            let index = self
                .objects
                .mappings
                .iter()
                .position(|m| !m.live)
                .expect("reserved mapping slot");
            self.objects.mappings[index] = MappingBinding {
                live: true,
                grant: self.grant,
                object: self.id,
                space: request.space,
                root: request.root,
                address: request.base + offset as u64 * paging::PAGE,
                epoch: 0,
                targets: 0,
            };
        }
        Ok(())
    }
}

impl Objects {
    pub fn tracked_mappings(&self) -> usize {
        self.mappings.iter().filter(|m| m.live).count()
    }
    pub fn revocation_mappings(
        &self,
        ticket: &Revocation,
    ) -> Result<[Option<MappingBinding>; paging::MAPPINGS]> {
        self.lineage.validate_revoke(ticket)?;
        let mut result = [None; paging::MAPPINGS];
        for (out, mapping) in result.iter_mut().zip(self.mappings) {
            if mapping.live && self.lineage.selected(ticket, mapping.grant)? {
                *out = Some(mapping);
            }
        }
        Ok(result)
    }
    pub fn validate_mapping_revoke(
        &self,
        ticket: &Revocation,
        mapping: MappingBinding,
        request: shootdown::Request,
    ) -> Result<()> {
        if !self.lineage.selected(ticket, mapping.grant)?
            || !self.mappings.contains(&mapping)
            || !mapping.live
            || mapping.epoch != 0
            || request.root != mapping.root
            || request.address != mapping.address
            || request.epoch == 0
            || request.targets == 0
        {
            return Err(Error::Stale);
        }
        Ok(())
    }
    pub fn mark_mapping_revoke(
        &mut self,
        ticket: &Revocation,
        mapping: MappingBinding,
        request: shootdown::Request,
    ) -> Result<()> {
        self.validate_mapping_revoke(ticket, mapping, request)?;
        let stored = self
            .mappings
            .iter_mut()
            .find(|m| **m == mapping)
            .ok_or(Error::Object)?;
        stored.epoch = request.epoch;
        stored.targets = request.targets;
        Ok(())
    }
    pub fn validate_mapping_completion(&self, receipt: &shootdown::Completion) -> Result<()> {
        let request = receipt.request();
        if !self.mappings.iter().any(|m| {
            m.live
                && m.root == request.root
                && m.address == request.address
                && m.epoch == request.epoch
                && m.targets == request.targets
        }) {
            return Err(Error::Stale);
        }
        Ok(())
    }
    pub fn finish_mapping_revoke(
        &mut self,
        registry: &mut paging::Registry,
        receipt: &shootdown::Completion,
    ) -> Result<()> {
        self.validate_mapping_completion(receipt)?;
        registry
            .validate_revoked_mapping_completion(receipt)
            .map_err(|_| Error::Stale)?;
        let request = receipt.request();
        let slot = self
            .mappings
            .iter()
            .position(|m| m.live && m.root == request.root && m.address == request.address)
            .expect("validated mapping completion");
        registry
            .finish_revoked_mapping(receipt)
            .expect("validated paging receipt");
        self.release_mapping(slot);
        Ok(())
    }
    /// Normal teardown has no running CPU on this root. The adapter has already
    /// removed its real PTE, and the registry independently checks no pins/live
    /// translation record before releasing the authority/backing owner.
    pub fn release_unmapped(
        &mut self,
        registry: &paging::Registry,
        space: paging::SpaceId,
        address: u64,
    ) -> Result<()> {
        registry.can_allocate(space).map_err(|_| Error::InUse)?;
        if registry.mapping_for(space, address).is_ok() {
            return Err(Error::InUse);
        }
        let slot = self
            .mappings
            .iter()
            .position(|m| m.live && m.space == space && m.address == address && m.epoch == 0)
            .ok_or(Error::Object)?;
        self.release_mapping(slot);
        Ok(())
    }
    fn release_mapping(&mut self, slot: usize) {
        let mapping = core::mem::replace(&mut self.mappings[slot], MappingBinding::EMPTY);
        self.lineage
            .release(mapping.grant, Use::Mapping)
            .expect("owned mapping lineage");
        self.unpin(mapping.object);
    }
    #[cfg(any(feature = "authority-test", test))]
    pub fn mapping_layout(&self) -> [u64; 13] {
        use core::mem::{offset_of as off, size_of as size};
        [
            1,
            self.mappings.as_ptr() as u64,
            size::<MappingBinding>() as u64,
            paging::MAPPINGS as u64,
            off!(MappingBinding, live) as u64,
            off!(MappingBinding, grant) as u64,
            off!(MappingBinding, object) as u64,
            off!(MappingBinding, space) as u64,
            off!(MappingBinding, root) as u64,
            off!(MappingBinding, address) as u64,
            off!(MappingBinding, epoch) as u64,
            off!(MappingBinding, targets) as u64,
            self.tracked_mappings() as u64,
        ]
    }
}

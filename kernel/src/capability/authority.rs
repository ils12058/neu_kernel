//! Bounded derivation provenance. The caller owns the same exclusive transaction
//! as capability tables, queues and mapping publication. An ID is never authority
//! supplied by a user; only a checked table entry can begin a revoke.
use super::{Error, ObjectId, Result, Rights};

pub const GRANTS: usize = 64;
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(transparent)]
pub struct GrantId(u64);
impl GrantId {
    pub const NONE: Self = Self(0);
    pub fn raw(self) -> u64 {
        self.0
    }
    fn slot(self) -> usize {
        (self.0 as u32).wrapping_sub(1) as usize
    }
    fn generation(self) -> u32 {
        (self.0 >> 32) as u32
    }
    fn new(slot: usize, generation: u32) -> Self {
        Self((generation as u64) << 32 | (slot as u64 + 1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::ObjectType;

    #[test]
    fn authority_lineage_generations_and_revoke_epochs_never_wrap() {
        let mut lineage = Lineage::new();
        let object = ObjectId::new(0, 1, ObjectType::Counter);
        let rights = Rights::READ.union(Rights::DERIVE).union(Rights::REVOKE);
        for grant in &mut lineage.grants {
            grant.generation = u32::MAX;
        }
        assert_eq!(lineage.root(object, rights), Err(Error::Capacity));
        assert_eq!(lineage.live(), 0);
        lineage.grants[0].generation -= 1;
        let root = lineage.root(object, rights).unwrap();
        assert_eq!(root.generation(), u32::MAX);
        assert_eq!(lineage.derive(root, Rights::READ), Err(Error::Capacity));
        lineage.epoch = u64::MAX;
        assert!(matches!(lineage.begin(root), Err(Error::Capacity)));
        assert!(lineage.active.is_none());
        assert_eq!(lineage.validate(root, object, rights), Ok(()));
        lineage.release(root, Use::Handle).unwrap();
        assert_eq!(lineage.root(object, rights), Err(Error::Capacity));
        assert_eq!(lineage.validate(root, object, rights), Err(Error::Stale));
    }

    #[test]
    fn authority_pending_ticket_survives_source_close_and_rejects_foreign_completion() {
        let mut lineage = Lineage::new();
        let object = ObjectId::new(0, 1, ObjectType::Counter);
        let rights = Rights::READ.union(Rights::DERIVE).union(Rights::REVOKE);
        let root = lineage.root(object, rights).unwrap();
        let child = lineage.derive(root, Rights::READ).unwrap();
        lineage.transfer(child, Use::Handle, Use::Mapping).unwrap();
        let ticket = lineage.begin(root).unwrap();
        lineage.release(root, Use::Handle).unwrap();
        assert_eq!(lineage.live(), 2);
        assert_eq!(lineage.finish(&ticket), Err(Error::InUse));
        assert_eq!(
            lineage.finish(&Revocation {
                epoch: ticket.epoch + 1,
                root,
                affected: 1
            }),
            Err(Error::Stale)
        );
        assert_eq!(
            lineage.finish(&Revocation {
                epoch: ticket.epoch,
                root: child,
                affected: 1
            }),
            Err(Error::Stale)
        );
        lineage.release(child, Use::Mapping).unwrap();
        assert_eq!(lineage.live(), 1);
        assert_eq!(lineage.finish(&ticket), Ok(1));
        assert_eq!(lineage.live(), 0);
        assert_eq!(lineage.finish(&ticket), Err(Error::Stale));
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub(super) enum Use {
    Handle,
    Transfer,
    Wait,
    Mapping,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct Grant {
    generation: u32,
    live: bool,
    revoked: bool,
    parent: GrantId,
    object: ObjectId,
    rights: Rights,
    uses: [u32; 4],
}
impl Grant {
    const EMPTY: Self = Self {
        generation: 0,
        live: false,
        revoked: false,
        parent: GrantId::NONE,
        object: ObjectId::EMPTY,
        rights: Rights::NONE,
        uses: [0; 4],
    };
}

/// A retained, non-copyable revocation transaction. Losing this token leaves
/// its scope inaccessible and prevents completion/reuse, never silently aborts.
#[must_use = "finish the revocation only after all descendant owners are retired"]
pub struct Revocation {
    epoch: u64,
    root: GrantId,
    affected: usize,
}
impl Revocation {
    pub fn epoch(&self) -> u64 {
        self.epoch
    }
    pub fn root(&self) -> GrantId {
        self.root
    }
    pub fn affected(&self) -> usize {
        self.affected
    }
}

pub(super) struct Lineage {
    grants: [Grant; GRANTS],
    epoch: u64,
    active: Option<(u64, GrantId)>,
}
impl Lineage {
    pub const fn new() -> Self {
        Self {
            grants: [Grant::EMPTY; GRANTS],
            epoch: 0,
            active: None,
        }
    }
    fn entry(&self, id: GrantId) -> Result<&Grant> {
        self.grants
            .get(id.slot())
            .filter(|g| g.live && g.generation == id.generation())
            .ok_or(Error::Stale)
    }
    pub fn validate(&self, id: GrantId, object: ObjectId, rights: Rights) -> Result<()> {
        if id == GrantId::NONE {
            return Ok(());
        }
        let g = self.entry(id)?;
        if g.revoked {
            return Err(Error::Revoked);
        }
        if g.object != object || g.rights != rights {
            return Err(Error::Object);
        }
        Ok(())
    }
    pub fn revoked(&self, id: GrantId) -> bool {
        id != GrantId::NONE && self.entry(id).map_or(true, |g| g.revoked)
    }
    fn below(&self, mut id: GrantId, root: GrantId) -> bool {
        for _ in 0..GRANTS {
            let Ok(g) = self.entry(id) else {
                return false;
            };
            id = g.parent;
            if id == root {
                return true;
            }
            if id == GrantId::NONE {
                return false;
            }
        }
        false
    }
    fn free(&self) -> Result<usize> {
        self.grants
            .iter()
            .position(|g| !g.live && g.generation != u32::MAX)
            .ok_or(Error::Capacity)
    }
    pub fn can_create(&self) -> Result<()> {
        self.free().map(|_| ())
    }
    pub fn root(&mut self, object: ObjectId, rights: Rights) -> Result<GrantId> {
        self.insert(GrantId::NONE, object, rights)
    }
    fn insert(&mut self, parent: GrantId, object: ObjectId, rights: Rights) -> Result<GrantId> {
        let slot = self.free()?;
        let generation = self.grants[slot].generation + 1;
        self.grants[slot] = Grant {
            generation,
            live: true,
            revoked: false,
            parent,
            object,
            rights,
            uses: [1, 0, 0, 0],
        };
        Ok(GrantId::new(slot, generation))
    }
    pub fn derive(&mut self, parent: GrantId, rights: Rights) -> Result<GrantId> {
        let g = *self.entry(parent)?;
        self.validate(parent, g.object, g.rights)?;
        if let Some((_, root)) = self.active
            && (parent == root || self.below(parent, root))
        {
            return Err(Error::InUse);
        }
        if !g.rights.contains(Rights::DERIVE)
            || !g.rights.contains(rights)
            || !rights.valid(g.object.kind())
        {
            return Err(Error::Rights);
        }
        self.insert(parent, g.object, rights)
    }
    pub fn can_retain(&self, id: GrantId, usage: Use, count: u32) -> Result<()> {
        if id == GrantId::NONE {
            return Ok(());
        }
        let g = self.entry(id)?;
        if g.revoked {
            return Err(Error::Revoked);
        }
        g.uses[usage as usize]
            .checked_add(count)
            .ok_or(Error::Capacity)?;
        Ok(())
    }
    pub fn retain(&mut self, id: GrantId, usage: Use, count: u32) -> Result<()> {
        self.can_retain(id, usage, count)?;
        if id != GrantId::NONE {
            self.grants[id.slot()].uses[usage as usize] += count;
        }
        Ok(())
    }
    pub fn release(&mut self, id: GrantId, usage: Use) -> Result<()> {
        if id == GrantId::NONE {
            return Ok(());
        }
        let remaining = self.entry(id)?.uses[usage as usize]
            .checked_sub(1)
            .ok_or(Error::Object)?;
        self.grants[id.slot()].uses[usage as usize] = remaining;
        self.collect(id);
        Ok(())
    }
    pub fn transfer(&mut self, id: GrantId, from: Use, to: Use) -> Result<()> {
        if id == GrantId::NONE {
            return Ok(());
        }
        self.can_retain(id, to, 1)?;
        if self.entry(id)?.uses[from as usize] == 0 {
            return Err(Error::Object);
        }
        self.grants[id.slot()].uses[from as usize] -= 1;
        self.grants[id.slot()].uses[to as usize] += 1;
        Ok(())
    }
    fn collect(&mut self, mut id: GrantId) {
        for _ in 0..GRANTS {
            let Ok(g) = self.entry(id).copied() else {
                break;
            };
            if g.uses != [0; 4]
                || self.active.is_some_and(|(_, root)| root == id)
                || self
                    .grants
                    .iter()
                    .any(|child| child.live && child.parent == id)
            {
                break;
            }
            self.grants[id.slot()] = Grant {
                generation: g.generation,
                ..Grant::EMPTY
            };
            id = g.parent;
        }
    }
    pub fn begin(&mut self, root: GrantId) -> Result<Revocation> {
        let g = self.entry(root)?;
        self.validate(root, g.object, g.rights)?;
        if !g.rights.contains(Rights::REVOKE) {
            return Err(Error::Rights);
        }
        if self.active.is_some() {
            return Err(Error::InUse);
        }
        let epoch = self.epoch.checked_add(1).ok_or(Error::Capacity)?;
        let mut selected = [false; GRANTS];
        for (slot, g) in self.grants.iter().enumerate() {
            selected[slot] = g.live && self.below(GrantId::new(slot, g.generation), root);
        }
        let affected = selected.iter().filter(|&&v| v).count();
        for (g, selected) in self.grants.iter_mut().zip(selected) {
            if selected {
                g.revoked = true;
            }
        }
        self.epoch = epoch;
        self.active = Some((epoch, root));
        Ok(Revocation {
            epoch,
            root,
            affected,
        })
    }
    pub fn validate_revoke(&self, ticket: &Revocation) -> Result<()> {
        if self.active != Some((ticket.epoch, ticket.root)) {
            return Err(Error::Stale);
        }
        self.entry(ticket.root)?;
        Ok(())
    }
    pub fn selected(&self, ticket: &Revocation, id: GrantId) -> Result<bool> {
        self.validate_revoke(ticket)?;
        Ok(id != GrantId::NONE && self.below(id, ticket.root))
    }
    pub fn finish(&mut self, ticket: &Revocation) -> Result<usize> {
        self.validate_revoke(ticket)?;
        for (slot, g) in self.grants.iter().enumerate() {
            if g.live && self.below(GrantId::new(slot, g.generation), ticket.root) {
                return Err(Error::InUse);
            }
        }
        self.active = None;
        self.collect(ticket.root);
        Ok(ticket.affected)
    }
    pub fn live(&self) -> usize {
        self.grants.iter().filter(|g| g.live).count()
    }
    #[cfg(any(feature = "authority-test", feature = "device-test", test))]
    pub fn diagnostic_layout(&self) -> [u64; 14] {
        use core::mem::{offset_of as off, size_of as size};
        [
            1,
            self.grants.as_ptr() as u64,
            size::<Grant>() as u64,
            GRANTS as u64,
            off!(Grant, generation) as u64,
            off!(Grant, live) as u64,
            off!(Grant, revoked) as u64,
            off!(Grant, parent) as u64,
            off!(Grant, object) as u64,
            off!(Grant, rights) as u64,
            off!(Grant, uses) as u64,
            self.epoch,
            self.active.map_or(0, |v| v.0),
            self.active.map_or(0, |v| v.1.raw()),
        ]
    }
}

//! Fixed-capacity Node-local authority. Handles are scalars; object storage and
//! reference accounting stay in this kernel-owned registry. No public factory.
use crate::node::{ActiveContext, NodeId};
pub const HANDLES: usize = 8;
pub const OBJECTS: usize = 4;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ObjectType {
    Counter = 1,
    Flag = 2,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rights(u8);
impl Rights {
    pub const READ: Self = Self(1);
    pub const WRITE: Self = Self(2);
    pub const READ_WRITE: Self = Self(3);
    pub fn from_bits(bits: u64) -> Result<Self> {
        if bits == 0 || bits & !3 != 0 {
            Err(Error::Rights)
        } else {
            Ok(Self(bits as u8))
        }
    }
    pub fn bits(self) -> u64 {
        self.0 as u64
    }
    fn valid(self, kind: ObjectType) -> bool {
        self.0 != 0
            && self.0
                & !match kind {
                    ObjectType::Counter => 3,
                    ObjectType::Flag => 1,
                }
                == 0
    }
    fn contains(self, required: Self) -> bool {
        self.0 & required.0 == required.0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Handle(u64);
impl Handle {
    pub fn from_raw(raw: u64) -> Self {
        Self(raw)
    }
    pub fn raw(self) -> u64 {
        self.0
    }
    fn parts(self) -> Result<(usize, u32)> {
        let low = self.0 as u32;
        let generation = (self.0 >> 32) as u32;
        if low == 0 || low as usize > HANDLES || generation == 0 {
            return Err(Error::Handle);
        }
        Ok((low as usize - 1, generation))
    }
    fn new(slot: usize, generation: u32) -> Self {
        Self((generation as u64) << 32 | (slot as u64 + 1))
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TableId(NodeId);
impl TableId {
    pub fn owner(self) -> NodeId {
        self.0
    }
    pub fn raw(self) -> u64 {
        self.0.raw()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjectId {
    slot: usize,
    generation: u32,
    kind: ObjectType,
}
impl ObjectId {
    pub fn raw(self) -> u64 {
        (self.kind as u64) << 40 | (self.generation as u64) << 8 | (self.slot as u64 + 1)
    }
    pub fn kind(self) -> ObjectType {
        self.kind
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i64)]
pub enum Error {
    Handle = -16,
    Stale = -17,
    Rights = -18,
    Type = -19,
    Object = -20,
    Owner = -21,
    Capacity = -22,
    InUse = -23,
}
pub type Result<T> = core::result::Result<T, Error>;
#[derive(Clone, Copy)]
enum Value {
    Counter(u64),
    Flag(bool),
}
impl Value {
    fn kind(self) -> ObjectType {
        match self {
            Self::Counter(_) => ObjectType::Counter,
            Self::Flag(_) => ObjectType::Flag,
        }
    }
}
#[derive(Clone, Copy)]
struct Object {
    generation: u32,
    value: Option<Value>,
    references: u32,
    pins: u32,
}
impl Object {
    const EMPTY: Self = Self {
        generation: 0,
        value: None,
        references: 0,
        pins: 0,
    };
}
#[must_use = "release the trusted creator reference after publishing explicit grants"]
pub struct ObjectRef {
    id: ObjectId,
}
impl ObjectRef {
    pub fn id(&self) -> ObjectId {
        self.id
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub live: usize,
    pub references: u32,
    pub pins: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjectInfo {
    pub id: ObjectId,
    pub references: u32,
    pub pins: u32,
}
pub struct Objects {
    slots: [Object; OBJECTS],
}
impl Default for Objects {
    fn default() -> Self {
        Self::new()
    }
}
impl Objects {
    pub const fn new() -> Self {
        Self {
            slots: [Object::EMPTY; OBJECTS],
        }
    }
    pub fn counter(&mut self, value: u64) -> Result<ObjectRef> {
        self.create(Value::Counter(value))
    }
    pub fn flag(&mut self, value: bool) -> Result<ObjectRef> {
        self.create(Value::Flag(value))
    }
    fn create(&mut self, value: Value) -> Result<ObjectRef> {
        let slot = self
            .slots
            .iter()
            .position(|s| s.value.is_none() && s.generation != u32::MAX)
            .ok_or(Error::Capacity)?;
        let generation = self.slots[slot].generation + 1;
        self.slots[slot] = Object {
            generation,
            value: Some(value),
            references: 1,
            pins: 0,
        };
        Ok(ObjectRef {
            id: ObjectId {
                slot,
                generation,
                kind: value.kind(),
            },
        })
    }
    fn object(&self, id: ObjectId) -> Result<&Object> {
        self.slots
            .get(id.slot)
            .filter(|s| s.generation == id.generation && s.value.is_some())
            .ok_or(Error::Object)
            .and_then(|s| {
                if s.value.unwrap().kind() == id.kind {
                    Ok(s)
                } else {
                    Err(Error::Type)
                }
            })
    }
    pub fn info(&self, id: ObjectId) -> Result<ObjectInfo> {
        let s = self.object(id)?;
        Ok(ObjectInfo {
            id,
            references: s.references,
            pins: s.pins,
        })
    }
    #[cfg(feature = "cap-test")]
    pub fn debug_value(&self, id: ObjectId) -> Result<u64> {
        match self.object(id)?.value {
            Some(Value::Counter(v)) => Ok(v),
            Some(Value::Flag(v)) => Ok(v as u64),
            None => Err(Error::Object),
        }
    }
    /// Supervisor-only QMP instrumentation, never returned through the ABI.
    #[cfg(feature = "cap-test")]
    pub fn value_address(&self, id: ObjectId) -> Result<u64> {
        match self.object(id)?.value.as_ref() {
            Some(Value::Counter(value)) => Ok(value as *const u64 as u64),
            Some(Value::Flag(value)) => Ok(value as *const bool as u64),
            None => Err(Error::Object),
        }
    }
    pub fn stats(&self) -> Stats {
        let mut result = Stats::default();
        for s in self.slots.iter().filter(|s| s.value.is_some()) {
            result.live += 1;
            result.references += s.references;
            result.pins += s.pins;
        }
        result
    }
    fn retain(&mut self, id: ObjectId) -> Result<()> {
        let refs = self
            .object(id)?
            .references
            .checked_add(1)
            .ok_or(Error::Capacity)?;
        self.slots[id.slot].references = refs;
        Ok(())
    }
    fn collect(&mut self, slot: usize) {
        if self.slots[slot].references == 0 && self.slots[slot].pins == 0 {
            self.slots[slot].value = None;
        }
    }
    fn release_id(&mut self, id: ObjectId) -> Result<()> {
        let references = self
            .object(id)?
            .references
            .checked_sub(1)
            .ok_or(Error::Object)?;
        self.slots[id.slot].references = references;
        self.collect(id.slot);
        Ok(())
    }
    pub fn release(&mut self, reference: ObjectRef) -> Result<()> {
        self.release_id(reference.id)
    }
    fn pin(&mut self, id: ObjectId) -> Result<()> {
        let pins = self
            .object(id)?
            .pins
            .checked_add(1)
            .ok_or(Error::Capacity)?;
        self.slots[id.slot].pins = pins;
        Ok(())
    }
    fn unpin(&mut self, id: ObjectId) {
        let s = self.object(id).expect("live capability operation");
        let pins = s.pins.checked_sub(1).expect("capability pin ownership");
        self.slots[id.slot].pins = pins;
        self.collect(id.slot);
    }
}
struct Entry {
    target: ObjectId,
    kind: ObjectType,
    rights: Rights,
}
struct Slot {
    generation: u32,
    entry: Option<Entry>,
}
impl Slot {
    const fn new() -> Self {
        Self {
            generation: 0,
            entry: None,
        }
    }
}
/// Not Copy/Clone: moving a table transfers its actual reference ownership.
pub struct Table {
    live: bool,
    id: TableId,
    slots: [Slot; HANDLES],
}
impl Table {
    pub(crate) fn new(owner: NodeId) -> Self {
        Self {
            live: true,
            id: TableId(owner),
            slots: [const { Slot::new() }; HANDLES],
        }
    }
    pub(crate) fn terminate(&mut self) {
        self.live = false;
    }
    pub fn id(&self) -> TableId {
        self.id
    }
    pub fn len(&self) -> usize {
        self.slots.iter().filter(|s| s.entry.is_some()).count()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn owner(&self, caller: NodeId) -> Result<()> {
        if self.id.owner() == caller {
            Ok(())
        } else {
            Err(Error::Owner)
        }
    }
    fn entry(&self, caller: NodeId, handle: Handle) -> Result<&Entry> {
        self.owner(caller)?;
        let (slot, generation) = handle.parts()?;
        let s = &self.slots[slot];
        if s.generation == 0 {
            return Err(Error::Handle);
        }
        if s.generation != generation {
            return Err(Error::Stale);
        }
        s.entry.as_ref().ok_or(Error::Stale)
    }
    /// Trusted grant only: no user request accepts ObjectRef or ObjectId.
    pub fn grant(
        &mut self,
        objects: &mut Objects,
        reference: &ObjectRef,
        rights: Rights,
    ) -> Result<Handle> {
        if !self.live {
            return Err(Error::Owner);
        }
        let id = reference.id;
        objects.object(id)?;
        if !rights.valid(id.kind) {
            return Err(Error::Rights);
        }
        let slot = self
            .slots
            .iter()
            .position(|s| s.entry.is_none() && s.generation != u32::MAX)
            .ok_or(Error::Capacity)?;
        let generation = self.slots[slot].generation + 1;
        objects.retain(id)?;
        self.slots[slot] = Slot {
            generation,
            entry: Some(Entry {
                target: id,
                kind: id.kind,
                rights,
            }),
        };
        Ok(Handle::new(slot, generation))
    }
    pub fn describe(&self, caller: NodeId, handle: Handle) -> Result<(ObjectId, Rights)> {
        let e = self.entry(caller, handle)?;
        Ok((e.target, e.rights))
    }
    pub fn lookup<'a>(
        &self,
        objects: &'a mut Objects,
        caller: NodeId,
        handle: Handle,
        kind: ObjectType,
        required: Rights,
    ) -> Result<Guard<'a>> {
        if !self.live {
            return Err(Error::Owner);
        }
        let e = self.entry(caller, handle)?;
        objects.object(e.target)?;
        if e.kind != kind || e.target.kind != kind {
            return Err(Error::Type);
        }
        if !required.valid(kind) || !e.rights.contains(required) {
            return Err(Error::Rights);
        }
        objects.pin(e.target)?;
        Ok(Guard {
            objects,
            id: e.target,
            caller,
            rights: required,
        })
    }
    pub fn close(&mut self, objects: &mut Objects, caller: NodeId, handle: Handle) -> Result<()> {
        let id = self.entry(caller, handle)?.target;
        objects.object(id)?;
        objects.release_id(id)?;
        let (slot, _) = handle.parts()?;
        self.slots[slot].entry = None;
        Ok(())
    }
    pub fn drain(&mut self, objects: &mut Objects, owner: NodeId) -> Result<usize> {
        self.owner(owner)?;
        let mut released = 0;
        for slot in 0..HANDLES {
            if self.slots[slot].entry.is_some() {
                self.close(
                    objects,
                    owner,
                    Handle::new(slot, self.slots[slot].generation),
                )?;
                released += 1;
            }
        }
        Ok(released)
    }
}
/// Exclusive registry borrow prevents storage escape. The explicit pin also
/// retains the object when this already authorized guard closes the last handle.
pub struct Guard<'a> {
    objects: &'a mut Objects,
    id: ObjectId,
    caller: NodeId,
    rights: Rights,
}
impl Guard<'_> {
    #[cfg(feature = "cap-test")]
    pub fn value_address(&self) -> u64 {
        self.objects.value_address(self.id).expect("guarded value")
    }
    pub fn info(&self) -> ObjectInfo {
        self.objects
            .info(self.id)
            .expect("guard pins exact generation")
    }
    pub fn read(&self) -> Result<u64> {
        if !self.rights.contains(Rights::READ) {
            return Err(Error::Rights);
        }
        match self.objects.object(self.id)?.value {
            Some(Value::Counter(v)) => Ok(v),
            Some(Value::Flag(v)) => Ok(v as u64),
            None => Err(Error::Object),
        }
    }
    pub fn write(&mut self, value: u64) -> Result<()> {
        if self.id.kind != ObjectType::Counter {
            return Err(Error::Type);
        }
        if !self.rights.contains(Rights::WRITE) {
            return Err(Error::Rights);
        }
        self.objects.slots[self.id.slot].value = Some(Value::Counter(value));
        Ok(())
    }
    pub fn exchange(&mut self, value: u64) -> Result<u64> {
        if !self.rights.contains(Rights::READ_WRITE) {
            return Err(Error::Rights);
        }
        let previous = self.read()?;
        self.write(value)?;
        Ok(previous)
    }
    pub fn close_handle(&mut self, table: &mut Table, handle: Handle) -> Result<()> {
        table.close(self.objects, self.caller, handle)
    }
}
impl Drop for Guard<'_> {
    fn drop(&mut self) {
        self.objects.unpin(self.id);
    }
}
/// Created only by a valid active model binding; never cloned or user-selected.
#[must_use = "return the Node table before changing execution ownership"]
pub struct Lease {
    active: ActiveContext,
    table: Table,
}
impl Lease {
    pub(crate) fn new(active: ActiveContext, table: Table) -> Self {
        Self { active, table }
    }
    pub fn active(&self) -> ActiveContext {
        self.active
    }
    pub fn table(&self) -> &Table {
        &self.table
    }
    pub fn table_mut(&mut self) -> &mut Table {
        &mut self.table
    }
    pub(crate) fn into_table(self) -> Table {
        self.table
    }
}

#[cfg(test)]
mod tests;

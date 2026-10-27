//! Fixed-capacity Node-local authority. Handles are scalars; object storage and
//! reference accounting stay in this kernel-owned registry. No public factory.
use crate::node::{ActiveContext, NodeId};
use crate::wait::{Kind, WAITERS, Waiter, Waits};
pub const HANDLES: usize = 8;
pub const OBJECTS: usize = 4;
pub const CHANNEL_OBJECTS: usize = 6;
pub const BOOTSTRAP: usize = 8;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ObjectType {
    Counter = 1,
    Flag = 2,
    AddressSpace = 3,
    ChannelEndpoint = 4,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rights(u8);
impl Rights {
    pub const READ: Self = Self(1);
    pub const WRITE: Self = Self(2);
    pub const READ_WRITE: Self = Self(3);
    pub const INSPECT: Self = Self(16);
    pub const SEND: Self = Self(32);
    pub const RECEIVE: Self = Self(64);
    pub const SEND_RECEIVE: Self = Self(96);
    pub const SEND_CAPS: Self = Self(128);
    pub const NONE: Self = Self(0);
    pub fn from_bits(bits: u64) -> Result<Self> {
        if bits == 0 || bits & !0x73 != 0 {
            Err(Error::Rights)
        } else {
            Ok(Self(bits as u8))
        }
    }
    pub fn bits(self) -> u64 {
        self.0 as u64
    }
    fn valid(self, kind: ObjectType) -> bool {
        match kind {
            ObjectType::Counter => self.0 != 0 && self.0 & !3 == 0,
            ObjectType::Flag => self.0 == 1,
            ObjectType::AddressSpace => self.0 & !16 == 0,
            ObjectType::ChannelEndpoint => self.0 != 0 && self.0 & !224 == 0,
        }
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
#[repr(C)]
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
    Empty = -32,
    Full = -33,
    PeerClosed = -34,
    Terminal = -35,
    MessageKind = -36,
    Transfer = -37,
}
pub type Result<T> = core::result::Result<T, Error>;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WaitStart {
    Committed(u64),
    Blocked,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Settle {
    Committed(u64),
    Retry,
    Failed(Error),
}
enum Value {
    Endpoint(crate::channel::EndpointRef),
    Counter(u64),
    Flag(bool),
    AddressSpace {
        owner: NodeId,
        reference: crate::paging::SpaceRef,
    },
}
impl Value {
    fn kind(&self) -> ObjectType {
        match self {
            Self::Endpoint(_) => ObjectType::ChannelEndpoint,
            Self::Counter(_) => ObjectType::Counter,
            Self::Flag(_) => ObjectType::Flag,
            Self::AddressSpace { .. } => ObjectType::AddressSpace,
        }
    }
}
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
/// Exactly one existing reference, moved from a table into one FIFO slot.
/// No Clone/Copy, borrow, pointer, or user-selected object identity.
#[must_use = "install the queued reference in a table or release it on discard"]
pub(crate) struct OwnedTransfer {
    reference: ObjectRef,
    rights: Rights,
}
impl OwnedTransfer {
    pub(crate) fn description(&self) -> (ObjectId, Rights) {
        (self.reference.id, self.rights)
    }
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
    slots: [Object; CHANNEL_OBJECTS],
    limit: usize,
    channels: crate::channel::Channels,
    waits: Waits,
}
impl Default for Objects {
    fn default() -> Self {
        Self::new()
    }
}
impl Objects {
    pub const fn new() -> Self {
        Self {
            slots: [const { Object::EMPTY }; CHANNEL_OBJECTS],
            limit: OBJECTS,
            channels: crate::channel::Channels::new(),
            waits: Waits::new(),
        }
    }
    pub const fn with_channels() -> Self {
        Self {
            limit: CHANNEL_OBJECTS,
            ..Self::new()
        }
    }
    pub fn channel_state(&self) -> &crate::channel::Channels {
        &self.channels
    }
    pub fn queued_references(&self) -> u32 {
        self.channels.queued_capabilities()
    }
    #[cfg(feature = "cap-move-test")]
    pub fn move_objects(&self) -> [u64; 18] {
        let mut words = [0; 18];
        for (i, o) in self.slots.iter().enumerate() {
            if let Some(value) = o.value.as_ref() {
                words[i * 3..i * 3 + 3].copy_from_slice(&[
                    ObjectId {
                        slot: i,
                        generation: o.generation,
                        kind: value.kind(),
                    }
                    .raw(),
                    o.references as u64,
                    o.pins as u64,
                ]);
            }
        }
        words
    }
    #[cfg(feature = "cap-move-test")]
    pub fn move_object_addresses(&self) -> [u64; 18] {
        let mut addresses = [0; 18];
        for (i, o) in self.slots.iter().enumerate() {
            addresses[i * 3..i * 3 + 3].copy_from_slice(&[
                core::ptr::from_ref(&o.generation) as u64,
                core::ptr::from_ref(&o.references) as u64,
                core::ptr::from_ref(&o.pins) as u64,
            ]);
        }
        addresses
    }
    pub fn wait_stats(&self) -> crate::wait::Stats {
        self.waits.stats()
    }
    pub fn node_wait_count(&self, node: NodeId) -> usize {
        self.waits.node_count(node)
    }
    pub fn pins_outside_node(&self, node: NodeId) -> u32 {
        self.waits.pinned_outside(node)
    }
    pub fn wait_snapshot(&self) -> [u64; 64] {
        self.waits.snapshot()
    }
    #[cfg(feature = "channel-wait-test")]
    fn wait_event(
        &self,
        event: u64,
        waiter: Option<Waiter>,
        result: i64,
        saved: crate::wait::witness::Saved,
    ) {
        crate::wait::witness::write(
            event,
            waiter,
            result,
            0,
            self.waits.stats(),
            waiter.map_or(0, |w| self.waits.node_count(w.active.node)),
            self.slots.iter().map(|o| o.pins).sum(),
            saved,
        );
    }
    pub fn wait_waiter(&self, thread: crate::node::ThreadId) -> Option<Waiter> {
        self.waits.by_thread(thread)
    }
    pub fn take_ready_waits(&mut self) -> [Option<Waiter>; WAITERS] {
        self.waits.take_ready()
    }
    pub fn retry_wait(&mut self, token: u64) -> bool {
        self.waits.retry(token)
    }
    fn endpoint_of(&self, id: ObjectId) -> Option<(crate::channel::EndpointInfo, ObjectId)> {
        match self.slots.get(id.slot)?.value.as_ref()? {
            Value::Endpoint(reference) => self
                .channels
                .info(reference.id())
                .ok()
                .map(|info| (info, id)),
            _ => None,
        }
    }
    /// Wakes one receive waiter after a committed send. The notification is
    /// stored by the exclusive registry transaction that committed the enqueue.
    fn wake_receive(&mut self, info: crate::channel::EndpointInfo) {
        self.waits
            .wake(info.channel, 1 - info.side, Kind::Receive, false);
    }
    /// A committed receive created capacity in the peer's send queue.
    fn wake_send(&mut self, info: crate::channel::EndpointInfo) {
        self.waits
            .wake(info.channel, 1 - info.side, Kind::Send, false);
    }
    fn begin_wait(
        &mut self,
        active: ActiveContext,
        object: ObjectId,
        info: crate::channel::EndpointInfo,
        kind: Kind,
        payload: u64,
    ) -> Result<u64> {
        if !self.waits.can_register(active.thread) {
            return Err(if self.waits.by_thread(active.thread).is_some() {
                Error::InUse
            } else {
                Error::Capacity
            });
        }
        self.pin(object)?;
        match self.waits.register(
            active,
            info.id,
            info.channel,
            object,
            info.side,
            info.owner,
            kind,
            payload,
        ) {
            Ok(token) => Ok(token),
            Err(error) => {
                self.unpin(object);
                Err(error)
            }
        }
    }
    /// Rechecks and commits one woken operation inside the same exclusive
    /// transaction that owns handle lookup. Retry keeps the original pin.
    pub fn settle_wait(&mut self, thread: crate::node::ThreadId) -> Settle {
        let Some(waiter) = self.waits.by_thread(thread) else {
            return Settle::Failed(Error::Object);
        };
        if waiter.state != crate::wait::State::Queued {
            return Settle::Failed(Error::Stale);
        }
        let Some(waiter) = self.waits.remove(waiter.token) else {
            return Settle::Failed(Error::Object);
        };
        if let Some(error) = waiter.terminal {
            if waiter.pinned {
                self.unpin(waiter.object);
            }
            #[cfg(feature = "channel-wait-test")]
            self.wait_event(
                crate::wait::witness::FAILED,
                Some(waiter),
                error as i64,
                crate::wait::witness::Saved::NONE,
            );
            return Settle::Failed(error);
        }
        let outcome = match waiter.kind {
            Kind::Send => self
                .channels
                .send(waiter.endpoint, waiter.owner, waiter.payload)
                .map(|()| 0),
            Kind::Receive => self.channels.receive(waiter.endpoint, waiter.owner),
        };
        match outcome {
            Ok(value) => {
                if waiter.pinned {
                    self.unpin(waiter.object);
                }
                let info = self
                    .endpoint_of(waiter.object)
                    .map(|(info, _)| info)
                    .unwrap_or(crate::channel::EndpointInfo {
                        id: waiter.endpoint,
                        channel: waiter.channel,
                        side: waiter.side,
                        owner: waiter.owner,
                    });
                match waiter.kind {
                    Kind::Send => self.wake_receive(info),
                    Kind::Receive => self.wake_send(info),
                }
                #[cfg(feature = "channel-wait-test")]
                self.wait_event(
                    crate::wait::witness::COMMIT,
                    Some(waiter),
                    value as i64,
                    crate::wait::witness::Saved::NONE,
                );
                Settle::Committed(value)
            }
            Err(Error::Full) if waiter.kind == Kind::Send => {
                #[cfg(feature = "channel-wait-test")]
                self.wait_event(
                    crate::wait::witness::RETRY,
                    Some(waiter),
                    Error::Full as i64,
                    crate::wait::witness::Saved::NONE,
                );
                if self.waits.restore(waiter) {
                    Settle::Retry
                } else {
                    Settle::Failed(Error::Capacity)
                }
            }
            Err(Error::Empty) if waiter.kind == Kind::Receive => {
                #[cfg(feature = "channel-wait-test")]
                self.wait_event(
                    crate::wait::witness::RETRY,
                    Some(waiter),
                    Error::Empty as i64,
                    crate::wait::witness::Saved::NONE,
                );
                if self.waits.restore(waiter) {
                    Settle::Retry
                } else {
                    Settle::Failed(Error::Capacity)
                }
            }
            Err(error) => {
                if waiter.pinned {
                    self.unpin(waiter.object);
                }
                #[cfg(feature = "channel-wait-test")]
                self.wait_event(
                    crate::wait::witness::FAILED,
                    Some(waiter),
                    error as i64,
                    crate::wait::witness::Saved::NONE,
                );
                Settle::Failed(error)
            }
        }
    }
    /// Removes every wait owned by a Node before fault/reclamation. The caller
    /// has not yet cleared the owning Threads' saved contexts.
    pub fn cancel_node_waits(&mut self, node: NodeId) -> usize {
        let mut count = 0;
        for token in self.waits.tokens().into_iter().flatten() {
            let Some(waiter) = self.waits.by_token(token) else {
                continue;
            };
            if waiter.node() != node {
                continue;
            }
            self.waits.remove(token);
            if waiter.pinned {
                self.unpin(waiter.object);
            }
            #[cfg(feature = "channel-wait-test")]
            self.wait_event(
                crate::wait::witness::CANCEL,
                Some(waiter),
                0,
                crate::wait::witness::Saved::NONE,
            );
            count += 1;
        }
        count
    }
    fn terminate_endpoint(&mut self, object: ObjectId) {
        let Some((info, _)) = self.endpoint_of(object) else {
            return;
        };
        for token in self.waits.tokens().into_iter().flatten() {
            let Some(waiter) = self.waits.by_token(token) else {
                continue;
            };
            if waiter.object == object {
                if self.waits.mark_terminal(token, Error::PeerClosed)
                    && self.waits.release_pin(token)
                {
                    self.slots[object.slot].pins = self.slots[object.slot].pins.saturating_sub(1);
                }
            } else if waiter.channel == info.channel {
                self.waits.mark_ready(token);
            }
        }
    }
    pub fn counter(&mut self, value: u64) -> Result<ObjectRef> {
        self.create(Value::Counter(value))
    }
    pub fn flag(&mut self, value: bool) -> Result<ObjectRef> {
        self.create(Value::Flag(value))
    }
    fn free_slot(&self) -> Result<usize> {
        self.slots
            .iter()
            .take(self.limit)
            .position(|s| s.value.is_none() && s.generation != u32::MAX)
            .ok_or(Error::Capacity)
    }
    fn create_at(&mut self, slot: usize, value: Value) -> ObjectRef {
        let generation = self.slots[slot].generation + 1;
        let kind = value.kind();
        self.slots[slot] = Object {
            generation,
            value: Some(value),
            references: 1,
            pins: 0,
        };
        ObjectRef {
            id: ObjectId {
                slot,
                generation,
                kind,
            },
        }
    }
    fn create(&mut self, value: Value) -> Result<ObjectRef> {
        let slot = self.free_slot()?;
        Ok(self.create_at(slot, value))
    }
    /// The object owns the real root reference. Failure returns it to the caller.
    pub fn address_space(
        &mut self,
        owner: NodeId,
        reference: crate::paging::SpaceRef,
    ) -> core::result::Result<ObjectRef, (Error, crate::paging::SpaceRef)> {
        let slot = match self.free_slot() {
            Ok(slot) => slot,
            Err(e) => return Err((e, reference)),
        };
        Ok(self.create_at(slot, Value::AddressSpace { owner, reference }))
    }
    /// Trusted atomic two-table construction. All capacity/generation/manifest
    /// checks precede mutation. The exclusive transaction makes commit infallible;
    /// no userspace or IRQ can observe a half-published pair.
    pub fn install_channel(&mut self, a: &mut Table, b: &mut Table) -> Result<()> {
        if self.limit != CHANNEL_OBJECTS {
            return Err(Error::Capacity);
        }
        for t in [&*a, &*b] {
            if !t.live || t.sealed || t.bootstrap[4..].iter().any(Option::is_some) {
                return Err(Error::InUse);
            }
            if t.slots
                .iter()
                .filter(|s| s.entry.is_none() && s.generation != u32::MAX)
                .count()
                < 4
            {
                return Err(Error::Capacity);
            }
        }
        if a.id == b.id {
            return Err(Error::Owner);
        }
        if self
            .slots
            .iter()
            .take(self.limit)
            .filter(|s| s.value.is_none() && s.generation != u32::MAX)
            .count()
            < 2
        {
            return Err(Error::Capacity);
        }
        self.channels.available()?;
        let pair = self.channels.pair([a.id.owner(), b.id.owner()])?;
        for (table, endpoint) in [a, b].into_iter().zip(pair) {
            let creator = self
                .create(Value::Endpoint(endpoint))
                .expect("reserved object slot");
            table.bootstrap_limit = BOOTSTRAP;
            for (index, rights) in [
                (4, Rights::SEND_RECEIVE),
                (5, Rights::SEND_RECEIVE),
                (6, Rights::SEND),
                (7, Rights::RECEIVE),
            ] {
                table
                    .install_bootstrap(self, &creator, rights, index)
                    .expect("preflighted endpoint grant");
            }
            self.release(creator).expect("creator ownership");
        }
        Ok(())
    }
    pub fn reap_space(&mut self) -> Option<crate::paging::SpaceRef> {
        let slot = self.slots.iter().position(|o| {
            o.references == 0 && o.pins == 0 && matches!(o.value, Some(Value::AddressSpace { .. }))
        })?;
        match self.slots[slot].value.take()? {
            Value::AddressSpace { reference, .. } => Some(reference),
            _ => unreachable!(),
        }
    }
    pub fn pending_spaces(&self) -> usize {
        self.slots
            .iter()
            .filter(|o| {
                o.references == 0
                    && o.pins == 0
                    && matches!(o.value, Some(Value::AddressSpace { .. }))
            })
            .count()
    }
    fn object(&self, id: ObjectId) -> Result<&Object> {
        self.slots
            .get(id.slot)
            .filter(|s| {
                s.generation == id.generation
                    && s.value.is_some()
                    && (s.references != 0 || s.pins != 0)
            })
            .ok_or(Error::Object)
            .and_then(|s| {
                if s.value.as_ref().unwrap().kind() == id.kind {
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
        match self.object(id)?.value.as_ref() {
            Some(Value::Counter(v)) => Ok(*v),
            Some(Value::Flag(v)) => Ok(*v as u64),
            Some(Value::AddressSpace { .. } | Value::Endpoint(_)) => Err(Error::Type),
            None => Err(Error::Object),
        }
    }
    /// Supervisor-only QMP instrumentation, never returned through the ABI.
    #[cfg(any(feature = "cap-test", feature = "cap-move-test"))]
    pub fn value_address(&self, id: ObjectId) -> Result<u64> {
        match self.object(id)?.value.as_ref() {
            Some(Value::Counter(value)) => Ok(value as *const u64 as u64),
            Some(Value::Flag(value)) => Ok(value as *const bool as u64),
            Some(Value::AddressSpace { .. } | Value::Endpoint(_)) => Err(Error::Type),
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
        if self.slots[slot].references == 0
            && self.slots[slot].pins == 0
            && !matches!(self.slots[slot].value, Some(Value::AddressSpace { .. }))
        {
            let retired = self.slots[slot].value.take();
            if let Some(Value::Endpoint(reference)) = retired {
                let discarded = self
                    .channels
                    .retire_owned(reference)
                    .expect("last unpinned endpoint reference");
                for transfer in discarded.into_iter().flatten() {
                    self.release(transfer.reference)
                        .expect("queued reference ownership");
                }
            }
        }
    }
    fn release_id(&mut self, id: ObjectId) -> Result<()> {
        let references = self
            .object(id)?
            .references
            .checked_sub(1)
            .ok_or(Error::Object)?;
        self.slots[id.slot].references = references;
        if references == 0 {
            self.terminate_endpoint(id);
        }
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
#[repr(C)]
struct Entry {
    target: ObjectId,
    kind: ObjectType,
    rights: Rights,
    transferable: bool,
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
    bootstrap: [Option<Handle>; BOOTSTRAP],
    bootstrap_limit: usize,
    sealed: bool,
}
impl Table {
    #[cfg(feature = "cap-move-test")]
    pub fn move_slots(&self) -> [u64; 32] {
        let mut words = [0; 32];
        for (i, s) in self.slots.iter().enumerate() {
            if let Some(e) = s.entry.as_ref() {
                words[i * 4..i * 4 + 4].copy_from_slice(&[
                    Handle::new(i, s.generation).raw(),
                    e.target.raw(),
                    e.rights.bits(),
                    e.transferable as u64,
                ]);
            }
        }
        words
    }
    #[cfg(feature = "cap-move-test")]
    pub fn move_slot_addresses(&self) -> [u64; 16] {
        let mut addresses = [0; 16];
        for (i, s) in self.slots.iter().enumerate() {
            addresses[i * 2] = core::ptr::from_ref(&s.generation) as u64;
            addresses[i * 2 + 1] = s
                .entry
                .as_ref()
                .map_or(0, |e| core::ptr::from_ref(e) as u64);
        }
        addresses
    }
    pub(crate) fn new(owner: NodeId) -> Self {
        Self {
            live: true,
            id: TableId(owner),
            slots: [const { Slot::new() }; HANDLES],
            bootstrap: [None; BOOTSTRAP],
            bootstrap_limit: 4,
            sealed: false,
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
        let object = objects.object(id)?;
        if let Some(Value::Endpoint(reference)) = object.value.as_ref()
            && objects.channels.info(reference.id())?.owner != self.id.owner()
        {
            return Err(Error::Owner);
        }
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
                transferable: false,
            }),
        };
        Ok(Handle::new(slot, generation))
    }
    pub fn install_bootstrap(
        &mut self,
        objects: &mut Objects,
        reference: &ObjectRef,
        rights: Rights,
        index: usize,
    ) -> Result<Handle> {
        if self.sealed || index >= self.bootstrap_limit || self.bootstrap[index].is_some() {
            return Err(Error::InUse);
        }
        let handle = self.grant(objects, reference, rights)?;
        self.bootstrap[index] = Some(handle);
        Ok(handle)
    }
    /// Trusted bootstrap grant for the supported move-only object subset.
    pub fn grant_movable(
        &mut self,
        objects: &mut Objects,
        reference: &ObjectRef,
        rights: Rights,
    ) -> Result<Handle> {
        if !matches!(reference.id.kind, ObjectType::Flag | ObjectType::Counter) {
            return Err(Error::Type);
        }
        if self.sealed {
            return Err(Error::InUse);
        }
        let handle = self.grant(objects, reference, rights)?;
        self.slots[handle.parts()?.0]
            .entry
            .as_mut()
            .unwrap()
            .transferable = true;
        Ok(handle)
    }
    pub fn seal_bootstrap(&mut self) {
        self.sealed = true;
    }
    /// Trusted bootstrap configuration only. Index meanings and table capacity
    /// are unchanged. Legacy grants do not acquire these permissions implicitly.
    pub fn configure_moves(&mut self) -> Result<()> {
        if self.sealed || !self.live {
            return Err(Error::InUse);
        }
        let flag = self.bootstrap[3].ok_or(Error::Handle)?;
        let endpoint = self.bootstrap[4].ok_or(Error::Handle)?;
        if self.entry(self.id.owner(), flag)?.kind != ObjectType::Flag
            || self.entry(self.id.owner(), endpoint)?.kind != ObjectType::ChannelEndpoint
        {
            return Err(Error::Type);
        }
        self.slots[flag.parts()?.0]
            .entry
            .as_mut()
            .unwrap()
            .transferable = true;
        self.slots[endpoint.parts()?.0]
            .entry
            .as_mut()
            .unwrap()
            .rights = Rights(224);
        Ok(())
    }
    fn move_endpoint(
        &self,
        objects: &Objects,
        caller: NodeId,
        handle: Handle,
        right: Rights,
    ) -> Result<crate::channel::EndpointInfo> {
        if !self.live {
            return Err(Error::Owner);
        }
        let entry = self.entry(caller, handle)?;
        let object = objects.object(entry.target)?;
        if object.references == 0 {
            return Err(Error::Object);
        }
        if entry.kind != ObjectType::ChannelEndpoint || entry.target.kind != entry.kind {
            return Err(Error::Type);
        }
        if !entry.rights.valid(entry.kind) || !entry.rights.contains(right) {
            return Err(Error::Rights);
        }
        let Some(Value::Endpoint(reference)) = object.value.as_ref() else {
            return Err(Error::Type);
        };
        let info = objects.channels.info(reference.id())?;
        if info.owner != caller {
            return Err(Error::Owner);
        }
        Ok(info)
    }
    /// One exclusive registry/table transaction. Ordinary failures precede
    /// detachment; the source reference is moved, never retained or released.
    pub fn send_cap(
        &mut self,
        objects: &mut Objects,
        caller: NodeId,
        endpoint: Handle,
        source: Handle,
    ) -> Result<()> {
        let info = self.move_endpoint(objects, caller, endpoint, Rights::SEND_CAPS)?;
        let entry = self.entry(caller, source)?;
        if objects.object(entry.target)?.references == 0 {
            return Err(Error::Object);
        }
        if entry.target.kind != entry.kind
            || !matches!(entry.kind, ObjectType::Flag | ObjectType::Counter)
        {
            return Err(Error::Type);
        }
        if !entry.rights.valid(entry.kind) {
            return Err(Error::Rights);
        }
        if !entry.transferable {
            return Err(Error::Transfer);
        }
        objects.channels.can_send(info.id, caller)?;
        let entry = self.slots[source.parts()?.0]
            .entry
            .take()
            .expect("preflighted source");
        let transfer = OwnedTransfer {
            reference: ObjectRef { id: entry.target },
            rights: entry.rights,
        };
        objects.channels.send_transfer(info.id, caller, transfer);
        objects.wake_receive(info);
        Ok(())
    }
    /// Delivery is a separate atomic transaction. No free slot means the owned
    /// token remains at the FIFO head, with counters and generations unchanged.
    pub fn receive_cap(
        &mut self,
        objects: &mut Objects,
        caller: NodeId,
        endpoint: Handle,
    ) -> Result<Handle> {
        let info = self.move_endpoint(objects, caller, endpoint, Rights::RECEIVE)?;
        let (id, _) = objects
            .channels
            .peek_transfer(info.id, caller)?
            .description();
        objects.object(id)?;
        let slot = self
            .slots
            .iter()
            .position(|s| s.entry.is_none() && s.generation != u32::MAX)
            .ok_or(Error::Capacity)?;
        let generation = self.slots[slot].generation + 1;
        let OwnedTransfer {
            reference: ObjectRef { id },
            rights,
        } = objects.channels.receive_transfer(info.id, caller)?;
        self.slots[slot] = Slot {
            generation,
            entry: Some(Entry {
                target: id,
                kind: id.kind,
                rights,
                transferable: true,
            }),
        };
        // The consumed token's reference is now owned by the installed entry.
        objects.wake_send(info);
        if objects.channels.snapshot()[8 + info.side * 5] != 0 {
            objects
                .waits
                .wake(info.channel, info.side, Kind::Receive, false);
        }
        Ok(Handle::new(slot, generation))
    }
    pub fn bootstrap_get(&self, objects: &Objects, caller: NodeId, index: u64) -> Result<Handle> {
        self.owner(caller)?;
        if !self.live {
            return Err(Error::Owner);
        }
        if !self.sealed || index >= self.bootstrap_limit as u64 {
            return Err(Error::Handle);
        }
        let h = self.bootstrap[index as usize].ok_or(Error::Handle)?;
        let entry = self.entry(caller, h)?;
        objects.object(entry.target)?;
        Ok(h)
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
        if required.bits() == 0 || !required.valid(kind) || !e.rights.contains(required) {
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
    #[cfg(any(feature = "cap-test", feature = "cap-move-test"))]
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
        match self.objects.object(self.id)?.value.as_ref() {
            Some(Value::Counter(v)) => Ok(*v),
            Some(Value::Flag(v)) => Ok(*v as u64),
            Some(Value::AddressSpace { .. } | Value::Endpoint(_)) => Err(Error::Type),
            None => Err(Error::Object),
        }
    }
    pub fn address_space(&self) -> Result<(NodeId, &crate::paging::SpaceRef)> {
        if !self.rights.contains(Rights::INSPECT) {
            return Err(Error::Rights);
        }
        match self.objects.object(self.id)?.value.as_ref() {
            Some(Value::AddressSpace { owner, reference }) => Ok((*owner, reference)),
            _ => Err(Error::Type),
        }
    }
    pub fn endpoint_info(&self) -> Result<crate::channel::EndpointInfo> {
        match self.objects.object(self.id)?.value.as_ref() {
            Some(Value::Endpoint(r)) => self.objects.channels.info(r.id()),
            _ => Err(Error::Type),
        }
    }
    pub fn channel_send(&mut self, payload: u64) -> Result<()> {
        if !self.rights.contains(Rights::SEND) {
            return Err(Error::Rights);
        }
        let info = self.endpoint_info()?;
        self.objects.channels.send(info.id, self.caller, payload)?;
        self.objects.wake_receive(info);
        Ok(())
    }
    pub fn channel_receive(&mut self) -> Result<u64> {
        if !self.rights.contains(Rights::RECEIVE) {
            return Err(Error::Rights);
        }
        let info = self.endpoint_info()?;
        let value = self.objects.channels.receive(info.id, self.caller)?;
        self.objects.wake_send(info);
        Ok(value)
    }
    /// Commits immediately when possible; otherwise registers the Thread's one
    /// pending operation and returns `Blocked` without touching the queue count.
    pub fn channel_send_wait(&mut self, active: ActiveContext, payload: u64) -> Result<WaitStart> {
        if !self.rights.contains(Rights::SEND) {
            return Err(Error::Rights);
        }
        let info = self.endpoint_info()?;
        match self.objects.channels.send(info.id, self.caller, payload) {
            Ok(()) => {
                self.objects.wake_receive(info);
                Ok(WaitStart::Committed(0))
            }
            Err(Error::Full) => self
                .objects
                .begin_wait(active, self.id, info, Kind::Send, payload)
                .map(|_| WaitStart::Blocked),
            Err(error) => Err(error),
        }
    }
    pub fn channel_receive_wait(&mut self, active: ActiveContext) -> Result<WaitStart> {
        if !self.rights.contains(Rights::RECEIVE) {
            return Err(Error::Rights);
        }
        let info = self.endpoint_info()?;
        match self.objects.channels.receive(info.id, self.caller) {
            Ok(value) => {
                self.objects.wake_send(info);
                Ok(WaitStart::Committed(value))
            }
            Err(Error::Empty) => self
                .objects
                .begin_wait(active, self.id, info, Kind::Receive, 0)
                .map(|_| WaitStart::Blocked),
            Err(error) => Err(error),
        }
    }
    #[cfg(feature = "channel-test")]
    pub fn registry_stats(&self) -> Stats {
        self.objects.stats()
    }
    #[cfg(feature = "channel-test")]
    pub fn wait_stats(&self) -> crate::wait::Stats {
        self.objects.wait_stats()
    }
    #[cfg(feature = "channel-test")]
    pub fn wait_snapshot(&self) -> [u64; 64] {
        self.objects.wait_snapshot()
    }
    pub fn channel_snapshot(&self) -> [u64; 22] {
        self.objects.channels.snapshot()
    }
    #[cfg(feature = "channel-test")]
    pub fn queue_addresses(&self) -> [u64; 2] {
        self.objects.channels.queue_addresses()
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
mod moves_tests;
#[cfg(test)]
mod tests;

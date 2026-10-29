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
    None = 0,
    Counter = 1,
    Flag = 2,
    AddressSpace = 3,
    ChannelEndpoint = 4,
    MemoryObject = 5,
}
impl ObjectType {
    fn from_raw(value: u8) -> Self {
        match value {
            1 => Self::Counter,
            2 => Self::Flag,
            3 => Self::AddressSpace,
            4 => Self::ChannelEndpoint,
            5 => Self::MemoryObject,
            _ => Self::None,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(transparent)]
pub struct Rights(u8);
impl Rights {
    pub const READ: Self = Self(1);
    pub const WRITE: Self = Self(2);
    pub const READ_WRITE: Self = Self(3);
    pub const MAP: Self = Self(4);
    pub const TRANSFER: Self = Self(8);
    pub const MEMORY_OBJECT: Self = Self(15);
    pub const INSPECT: Self = Self(16);
    pub const SEND: Self = Self(32);
    pub const RECEIVE: Self = Self(64);
    pub const SEND_RECEIVE: Self = Self(96);
    pub const SEND_CAPS: Self = Self(128);
    pub const NONE: Self = Self(0);
    pub fn from_bits(bits: u64) -> Result<Self> {
        if bits == 0 || bits & !0x7f != 0 {
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
            ObjectType::None => false,
            ObjectType::Counter => self.0 != 0 && self.0 & !3 == 0,
            ObjectType::Flag => self.0 == 1,
            ObjectType::AddressSpace => self.0 & !16 == 0,
            ObjectType::ChannelEndpoint => self.0 != 0 && self.0 & !224 == 0,
            ObjectType::MemoryObject => self.0 != 0 && self.0 & !0x0f == 0,
        }
    }
    fn contains(self, required: Self) -> bool {
        self.0 & required.0 == required.0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(transparent)]
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
#[repr(transparent)]
pub struct ObjectId(u64);
impl ObjectId {
    pub(crate) const EMPTY: Self = Self(0);
    fn new(slot: usize, generation: u32, kind: ObjectType) -> Self {
        Self((kind as u64) << 40 | (generation as u64) << 8 | (slot as u64 + 1))
    }
    pub fn raw(self) -> u64 {
        self.0
    }
    pub fn kind(self) -> ObjectType {
        ObjectType::from_raw((self.0 >> 40) as u8)
    }
    fn generation(self) -> u32 {
        (self.0 >> 8) as u32
    }
    fn slot(self) -> usize {
        (self.0 as u8).wrapping_sub(1) as usize
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
    None,
    Endpoint(crate::channel::EndpointRef),
    Counter(u64),
    Flag(bool),
    AddressSpace {
        owner: NodeId,
        reference: crate::paging::SpaceRef,
    },
    MemoryObject(crate::paging::MemoryObjectRef),
}
impl Value {
    fn kind(&self) -> ObjectType {
        match self {
            Self::None => ObjectType::None,
            Self::Endpoint(_) => ObjectType::ChannelEndpoint,
            Self::Counter(_) => ObjectType::Counter,
            Self::Flag(_) => ObjectType::Flag,
            Self::AddressSpace { .. } => ObjectType::AddressSpace,
            Self::MemoryObject(_) => ObjectType::MemoryObject,
        }
    }
    fn target(&self) -> u64 {
        match self {
            Self::Endpoint(reference) => reference.id().raw(),
            Self::AddressSpace { reference, .. } => reference.id().raw(),
            Self::MemoryObject(reference) => reference.id().raw(),
            Self::None | Self::Counter(_) | Self::Flag(_) => 0,
        }
    }
}
/// Field order and explicit live/kind state are a stable diagnostic schema.
/// The values themselves remain private; QMP only reads raw production fields.
#[repr(C)]
struct Object {
    generation: u32,
    references: u32,
    pins: u32,
    live: bool,
    kind: ObjectType,
    target: u64,
    value: Value,
}
impl Object {
    const EMPTY: Self = Self {
        generation: 0,
        references: 0,
        pins: 0,
        live: false,
        kind: ObjectType::None,
        target: 0,
        value: Value::None,
    };
    fn free(&self) -> bool {
        !self.live && self.generation != u32::MAX
    }
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
    /// Dead MemoryObject capability objects whose paging reference cannot be
    /// released without the architecture registry. Bounded so collection can
    /// never grow without limit; a full pending array leaves the dead slot
    /// occupied until the next architecture reap.
    pending: [Option<crate::paging::MemoryObjectRef>; CHANNEL_OBJECTS],
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
            pending: [const { None }; CHANNEL_OBJECTS],
        }
    }
    pub const fn with_channels() -> Self {
        Self {
            limit: CHANNEL_OBJECTS,
            ..Self::new()
        }
    }
    /// Test-only schema for the independent stopped-CPU observer. The
    /// descriptor contains addresses and offsets, never authority.
    #[cfg(feature = "cap-move-test")]
    pub fn diagnostic_layout(&self) -> [u64; 18] {
        use core::mem::{offset_of as off, size_of as size};
        [
            1,
            self as *const Self as u64,
            size::<Self>() as u64,
            CHANNEL_OBJECTS as u64,
            self.slots.as_ptr() as u64,
            size::<Object>() as u64,
            off!(Object, generation) as u64,
            off!(Object, references) as u64,
            off!(Object, pins) as u64,
            off!(Object, live) as u64,
            off!(Object, kind) as u64,
            off!(Object, target) as u64,
            off!(Object, value) as u64,
            size::<ObjectId>() as u64,
            size::<Handle>() as u64,
            size::<ObjectType>() as u64,
            size::<Rights>() as u64,
            size::<Value>() as u64,
        ]
    }
    pub fn pending_memory_objects(&self) -> usize {
        self.pending.iter().flatten().count()
    }
    pub fn drain_memory_object(&mut self) -> Option<crate::paging::MemoryObjectRef> {
        let index = self.pending.iter().position(Option::is_some)?;
        self.pending[index].take()
    }
    /// Retry dead MemoryObject slots that could not be queued while the
    /// architecture-owned pending batch was full. Called only after a drain.
    pub fn retry_pending_memory_objects(&mut self) -> usize {
        let mut moved = 0;
        for slot in 0..self.limit {
            let object = &self.slots[slot];
            if object.live
                && object.references == 0
                && object.pins == 0
                && matches!(object.value, Value::MemoryObject(_))
            {
                let before = self.pending_memory_objects();
                self.collect(slot);
                if self.pending_memory_objects() > before {
                    moved += 1;
                }
            }
        }
        moved
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
            if o.live {
                words[i * 3..i * 3 + 3].copy_from_slice(&[
                    ObjectId::new(i, o.generation, o.kind).raw(),
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
    #[cfg(feature = "test-cap-moves-terminal-order")]
    pub fn debug_wait_addresses(&self) -> [u64; 3] {
        self.waits.debug_addresses()
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
        let object = self.slots.get(id.slot())?;
        if !object.live || object.generation != id.generation() {
            return None;
        }
        match &object.value {
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
                    self.slots[object.slot()].pins =
                        self.slots[object.slot()].pins.saturating_sub(1);
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
            .position(Object::free)
            .ok_or(Error::Capacity)
    }
    fn create_at(&mut self, slot: usize, value: Value) -> ObjectRef {
        let generation = self.slots[slot].generation + 1;
        let kind = value.kind();
        let target = value.target();
        self.slots[slot] = Object {
            generation,
            references: 1,
            pins: 0,
            live: true,
            kind,
            target,
            value,
        };
        ObjectRef {
            id: ObjectId::new(slot, generation, kind),
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
    /// The capability object owns exactly one paging reference. Failure
    /// returns the non-copyable token so the caller can release it.
    pub fn memory_object(
        &mut self,
        reference: crate::paging::MemoryObjectRef,
    ) -> core::result::Result<ObjectRef, (Error, crate::paging::MemoryObjectRef)> {
        let slot = match self.free_slot() {
            Ok(slot) => slot,
            Err(e) => return Err((e, reference)),
        };
        Ok(self.create_at(slot, Value::MemoryObject(reference)))
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
                .filter(|s| !s.live && s.generation != u32::MAX)
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
            .filter(|s| Object::free(s))
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
            o.live
                && o.references == 0
                && o.pins == 0
                && matches!(o.value, Value::AddressSpace { .. })
        })?;
        let retired = core::mem::replace(&mut self.slots[slot].value, Value::None);
        self.slots[slot].live = false;
        self.slots[slot].kind = ObjectType::None;
        self.slots[slot].target = 0;
        match retired {
            Value::AddressSpace { reference, .. } => Some(reference),
            _ => unreachable!(),
        }
    }
    pub fn pending_spaces(&self) -> usize {
        self.slots
            .iter()
            .filter(|o| {
                o.live
                    && o.references == 0
                    && o.pins == 0
                    && matches!(o.value, Value::AddressSpace { .. })
            })
            .count()
    }
    fn object(&self, id: ObjectId) -> Result<&Object> {
        self.slots
            .get(id.slot())
            .filter(|s| {
                s.live
                    && s.generation == id.generation()
                    && s.kind == id.kind()
                    && (s.references != 0 || s.pins != 0)
            })
            .ok_or(Error::Object)
    }
    pub fn info(&self, id: ObjectId) -> Result<ObjectInfo> {
        let s = self.slots.get(id.slot()).ok_or(Error::Object)?;
        if !s.live || s.generation != id.generation() {
            return Err(Error::Object);
        }
        if s.kind != id.kind() {
            return Err(Error::Type);
        }
        if s.references == 0 && s.pins == 0 {
            return Err(Error::Object);
        }
        Ok(ObjectInfo {
            id,
            references: s.references,
            pins: s.pins,
        })
    }
    #[cfg(feature = "cap-test")]
    pub fn debug_value(&self, id: ObjectId) -> Result<u64> {
        match &self.object(id)?.value {
            Value::Counter(v) => Ok(*v),
            Value::Flag(v) => Ok(*v as u64),
            Value::AddressSpace { .. } | Value::Endpoint(_) | Value::MemoryObject(_) => {
                Err(Error::Type)
            }
            Value::None => Err(Error::Object),
        }
    }
    /// Supervisor-only QMP instrumentation, never returned through the ABI.
    #[cfg(any(feature = "cap-test", feature = "cap-move-test"))]
    pub fn value_address(&self, id: ObjectId) -> Result<u64> {
        match &self.object(id)?.value {
            Value::Counter(value) => Ok(value as *const u64 as u64),
            Value::Flag(value) => Ok(value as *const bool as u64),
            Value::AddressSpace { .. } | Value::Endpoint(_) | Value::MemoryObject(_) => {
                Err(Error::Type)
            }
            Value::None => Err(Error::Object),
        }
    }
    pub fn stats(&self) -> Stats {
        let mut result = Stats::default();
        for s in self.slots.iter().filter(|s| s.live) {
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
        self.slots[id.slot()].references = refs;
        Ok(())
    }
    fn collect(&mut self, slot: usize) {
        let object = &self.slots[slot];
        if !object.live
            || object.references != 0
            || object.pins != 0
            || matches!(object.value, Value::AddressSpace { .. })
        {
            return;
        }
        // A dead MemoryObject keeps its slot until the architecture transaction
        // can release the paging reference. This keeps both registries bounded.
        if matches!(object.value, Value::MemoryObject(_))
            && self.pending.iter().all(Option::is_some)
        {
            return;
        }
        let retired = core::mem::replace(&mut self.slots[slot].value, Value::None);
        self.slots[slot].live = false;
        self.slots[slot].kind = ObjectType::None;
        self.slots[slot].target = 0;
        match retired {
            Value::Endpoint(reference) => {
                let discarded = self
                    .channels
                    .retire_owned(reference)
                    .expect("last unpinned endpoint reference");
                for transfer in discarded.into_iter().flatten() {
                    self.release(transfer.reference)
                        .expect("queued reference ownership");
                }
            }
            Value::MemoryObject(reference) => {
                let index = self
                    .pending
                    .iter()
                    .position(Option::is_none)
                    .expect("pending capacity preflight");
                self.pending[index] = Some(reference);
            }
            Value::None | Value::Counter(_) | Value::Flag(_) | Value::AddressSpace { .. } => {}
        }
    }
    fn release_id(&mut self, id: ObjectId) -> Result<()> {
        let references = self
            .object(id)?
            .references
            .checked_sub(1)
            .ok_or(Error::Object)?;
        self.slots[id.slot()].references = references;
        if references == 0 {
            self.terminate_endpoint(id);
        }
        self.collect(id.slot());
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
        self.slots[id.slot()].pins = pins;
        Ok(())
    }
    fn unpin(&mut self, id: ObjectId) {
        let s = self.object(id).expect("live capability operation");
        let pins = s.pins.checked_sub(1).expect("capability pin ownership");
        self.slots[id.slot()].pins = pins;
        self.collect(id.slot());
    }
}
/// Explicit live/kind storage is a stable diagnostic schema. A slot is the
/// sole owner of one object reference while `live` is true.
#[repr(C)]
struct Slot {
    generation: u32,
    live: bool,
    kind: ObjectType,
    target: ObjectId,
    rights: Rights,
    transferable: bool,
}
impl Slot {
    const EMPTY: Self = Self {
        generation: 0,
        live: false,
        kind: ObjectType::None,
        target: ObjectId::EMPTY,
        rights: Rights::NONE,
        transferable: false,
    };
    fn free(&self) -> bool {
        !self.live && self.generation != u32::MAX
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
            if s.live {
                words[i * 4..i * 4 + 4].copy_from_slice(&[
                    Handle::new(i, s.generation).raw(),
                    s.target.raw(),
                    s.rights.bits(),
                    s.transferable as u64,
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
            addresses[i * 2 + 1] = if s.live {
                core::ptr::from_ref(s) as u64
            } else {
                0
            };
        }
        addresses
    }
    /// Test-only schema for the independent stopped-CPU observer.
    #[cfg(feature = "cap-move-test")]
    pub fn diagnostic_layout(&self) -> [u64; 20] {
        use core::mem::{offset_of as off, size_of as size};
        [
            1,
            self as *const Self as u64,
            size::<Self>() as u64,
            self.slots.as_ptr() as u64,
            size::<Slot>() as u64,
            HANDLES as u64,
            off!(Slot, generation) as u64,
            off!(Slot, live) as u64,
            off!(Slot, kind) as u64,
            off!(Slot, target) as u64,
            off!(Slot, rights) as u64,
            off!(Slot, transferable) as u64,
            self.live as u64,
            self.sealed as u64,
            self.bootstrap.as_ptr() as u64,
            size::<Handle>() as u64,
            BOOTSTRAP as u64,
            self.bootstrap_limit as u64,
            self.id.raw(),
            size::<ObjectType>() as u64,
        ]
    }
    pub(crate) fn new(owner: NodeId) -> Self {
        Self {
            live: true,
            id: TableId(owner),
            slots: [const { Slot::EMPTY }; HANDLES],
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
        self.slots.iter().filter(|s| s.live).count()
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
    fn entry(&self, caller: NodeId, handle: Handle) -> Result<&Slot> {
        self.owner(caller)?;
        let (slot, generation) = handle.parts()?;
        let s = &self.slots[slot];
        if s.generation == 0 {
            return Err(Error::Handle);
        }
        if s.generation != generation {
            return Err(Error::Stale);
        }
        if !s.live {
            return Err(Error::Stale);
        }
        Ok(s)
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
        if let Value::Endpoint(reference) = &object.value
            && objects.channels.info(reference.id())?.owner != self.id.owner()
        {
            return Err(Error::Owner);
        }
        if !rights.valid(id.kind()) {
            return Err(Error::Rights);
        }
        let slot = self
            .slots
            .iter()
            .position(Slot::free)
            .ok_or(Error::Capacity)?;
        let generation = self.slots[slot].generation + 1;
        objects.retain(id)?;
        self.slots[slot] = Slot {
            generation,
            live: true,
            kind: id.kind(),
            target: id,
            rights,
            transferable: false,
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
    /// Test-profile bootstrap grant of the supported move-only subset. The
    /// manifest index and capacity checks are identical to `install_bootstrap`.
    #[cfg(feature = "cap-move-test")]
    pub fn install_bootstrap_movable(
        &mut self,
        objects: &mut Objects,
        reference: &ObjectRef,
        rights: Rights,
        index: usize,
    ) -> Result<Handle> {
        if self.sealed || index >= self.bootstrap_limit || self.bootstrap[index].is_some() {
            return Err(Error::InUse);
        }
        let handle = self.grant_movable(objects, reference, rights)?;
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
        if !matches!(
            reference.id.kind(),
            ObjectType::Flag | ObjectType::Counter | ObjectType::MemoryObject
        ) {
            return Err(Error::Type);
        }
        if self.sealed {
            return Err(Error::InUse);
        }
        let handle = self.grant(objects, reference, rights)?;
        self.slots[handle.parts()?.0].transferable = true;
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
        self.slots[flag.parts()?.0].transferable = true;
        self.slots[endpoint.parts()?.0].rights = Rights(224);
        Ok(())
    }
    /// K8B bootstrap: index 3 is a MemoryObject capability, not a Flag. The
    /// capability must itself carry TRANSFER; only the endpoint rights are
    /// widened to SEND|RECEIVE|SEND_CAPS.
    #[cfg(any(feature = "cap-move-test", test))]
    pub fn configure_memory_object_moves(&mut self) -> Result<()> {
        if self.sealed || !self.live {
            return Err(Error::InUse);
        }
        let object = self.bootstrap[3].ok_or(Error::Handle)?;
        let endpoint = self.bootstrap[4].ok_or(Error::Handle)?;
        let object_slot = object.parts()?.0;
        let endpoint_slot = endpoint.parts()?.0;
        let entry = self.entry(self.id.owner(), object)?;
        if entry.kind != ObjectType::MemoryObject
            || self.entry(self.id.owner(), endpoint)?.kind != ObjectType::ChannelEndpoint
        {
            return Err(Error::Type);
        }
        if !entry.rights.contains(Rights::TRANSFER) {
            return Err(Error::Rights);
        }
        self.slots[object_slot].transferable = true;
        self.slots[endpoint_slot].rights = Rights(224);
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
        if entry.kind != ObjectType::ChannelEndpoint || entry.target.kind() != entry.kind {
            return Err(Error::Type);
        }
        if !entry.rights.valid(entry.kind) || !entry.rights.contains(right) {
            return Err(Error::Rights);
        }
        let Value::Endpoint(reference) = &object.value else {
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
        let source_slot = source.parts()?.0;
        let entry = self.entry(caller, source)?;
        if objects.object(entry.target)?.references == 0 {
            return Err(Error::Object);
        }
        if entry.target.kind() != entry.kind
            || !matches!(
                entry.kind,
                ObjectType::Flag | ObjectType::Counter | ObjectType::MemoryObject
            )
        {
            return Err(Error::Type);
        }
        if !entry.rights.valid(entry.kind) {
            return Err(Error::Rights);
        }
        if entry.kind == ObjectType::MemoryObject && !entry.rights.contains(Rights::TRANSFER) {
            return Err(Error::Rights);
        }
        if !entry.transferable {
            return Err(Error::Transfer);
        }
        objects.channels.can_send(info.id, caller)?;
        let target = entry.target;
        let rights = entry.rights;
        self.slots[source_slot].live = false;
        self.slots[source_slot].kind = ObjectType::None;
        self.slots[source_slot].target = ObjectId::EMPTY;
        self.slots[source_slot].rights = Rights::NONE;
        self.slots[source_slot].transferable = false;
        let transfer = OwnedTransfer {
            reference: ObjectRef { id: target },
            rights,
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
        let (id, rights) = objects
            .channels
            .peek_transfer(info.id, caller)?
            .description();
        objects.object(id)?;
        if !rights.valid(id.kind()) {
            return Err(Error::Rights);
        }
        let slot = self
            .slots
            .iter()
            .position(Slot::free)
            .ok_or(Error::Capacity)?;
        let generation = self.slots[slot].generation + 1;
        let OwnedTransfer {
            reference: ObjectRef { id },
            rights,
        } = objects.channels.receive_transfer(info.id, caller)?;
        self.slots[slot] = Slot {
            generation,
            live: true,
            kind: id.kind(),
            target: id,
            rights,
            transferable: true,
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
        if e.kind != kind || e.target.kind() != kind {
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
            requested: required,
            actual: e.rights,
        })
    }
    pub fn close(&mut self, objects: &mut Objects, caller: NodeId, handle: Handle) -> Result<()> {
        let id = self.entry(caller, handle)?.target;
        objects.object(id)?;
        objects.release_id(id)?;
        let (slot, _) = handle.parts()?;
        self.slots[slot].live = false;
        self.slots[slot].kind = ObjectType::None;
        self.slots[slot].target = ObjectId::EMPTY;
        self.slots[slot].rights = Rights::NONE;
        self.slots[slot].transferable = false;
        Ok(())
    }
    pub fn drain(&mut self, objects: &mut Objects, owner: NodeId) -> Result<usize> {
        self.owner(owner)?;
        let mut released = 0;
        for slot in 0..HANDLES {
            if self.slots[slot].live {
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
    /// Rights requested at lookup; existing operations keep the historical
    /// no-amplification checks.
    requested: Rights,
    /// Rights actually held by the table entry. MemoryObject mapping authority
    /// is derived from this, never from the request.
    actual: Rights,
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
        if !self.requested.contains(Rights::READ) {
            return Err(Error::Rights);
        }
        match &self.objects.object(self.id)?.value {
            Value::Counter(v) => Ok(*v),
            Value::Flag(v) => Ok(*v as u64),
            Value::AddressSpace { .. } | Value::Endpoint(_) | Value::MemoryObject(_) => {
                Err(Error::Type)
            }
            Value::None => Err(Error::Object),
        }
    }
    pub fn address_space(&self) -> Result<(NodeId, &crate::paging::SpaceRef)> {
        if !self.requested.contains(Rights::INSPECT) {
            return Err(Error::Rights);
        }
        match &self.objects.object(self.id)?.value {
            Value::AddressSpace { owner, reference } => Ok((*owner, reference)),
            _ => Err(Error::Type),
        }
    }
    /// Mapping authority is derived from the entry's actual rights, not from
    /// the lookup request. A read-only capability can never authorize a
    /// writable mapping, and MAP alone authorizes no data access.
    pub fn memory_object(&self, writable: bool) -> Result<&crate::paging::MemoryObjectRef> {
        let required = if writable {
            Rights(Rights::MAP.bits() as u8 | Rights::READ_WRITE.bits() as u8)
        } else {
            Rights(Rights::MAP.bits() as u8 | Rights::READ.bits() as u8)
        };
        if !self.actual.contains(required) {
            return Err(Error::Rights);
        }
        match &self.objects.object(self.id)?.value {
            Value::MemoryObject(reference) => Ok(reference),
            _ => Err(Error::Type),
        }
    }
    pub fn endpoint_info(&self) -> Result<crate::channel::EndpointInfo> {
        match &self.objects.object(self.id)?.value {
            Value::Endpoint(r) => self.objects.channels.info(r.id()),
            _ => Err(Error::Type),
        }
    }
    pub fn channel_send(&mut self, payload: u64) -> Result<()> {
        if !self.requested.contains(Rights::SEND) {
            return Err(Error::Rights);
        }
        let info = self.endpoint_info()?;
        self.objects.channels.send(info.id, self.caller, payload)?;
        self.objects.wake_receive(info);
        Ok(())
    }
    pub fn channel_receive(&mut self) -> Result<u64> {
        if !self.requested.contains(Rights::RECEIVE) {
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
        if !self.requested.contains(Rights::SEND) {
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
        if !self.requested.contains(Rights::RECEIVE) {
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
        if self.id.kind() != ObjectType::Counter {
            return Err(Error::Type);
        }
        if !self.requested.contains(Rights::WRITE) {
            return Err(Error::Rights);
        }
        self.objects.slots[self.id.slot()].value = Value::Counter(value);
        Ok(())
    }
    pub fn exchange(&mut self, value: u64) -> Result<u64> {
        if !self.requested.contains(Rights::READ_WRITE) {
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

//! One bounded duplex Channel. Mechanism identities are never user handles.
//! Mutation requires the registry's exclusive IF=0 transaction in the kernel.
use crate::{
    capability::{Error, OwnedTransfer, Result},
    node::NodeId,
};
pub const MESSAGES: usize = 2;
pub const ENDPOINTS: usize = 2;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChannelId {
    generation: u32,
}
impl ChannelId {
    pub fn raw(self) -> u64 {
        (self.generation as u64) << 8 | 1
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EndpointId {
    slot: usize,
    generation: u32,
}
impl EndpointId {
    pub fn raw(self) -> u64 {
        (self.generation as u64) << 8 | (self.slot as u64 + 1)
    }
}
/// Non-copyable ownership token held by exactly one endpoint object.
pub struct EndpointRef {
    id: EndpointId,
}
impl EndpointRef {
    pub fn id(&self) -> EndpointId {
        self.id
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EndpointInfo {
    pub id: EndpointId,
    pub channel: ChannelId,
    pub side: usize,
    pub owner: NodeId,
}
#[repr(C)]
struct Fifo {
    head: u64,
    tail: u64,
    len: u64,
    values: [u64; MESSAGES],
    transfers: [Option<OwnedTransfer>; MESSAGES],
}
impl Fifo {
    const EMPTY: Self = Self {
        head: 0,
        tail: 0,
        len: 0,
        values: [0; MESSAGES],
        transfers: [const { None }; MESSAGES],
    };
    fn validate(&self) -> Result<()> {
        let capacity = MESSAGES as u64;
        if self.head >= capacity
            || self.tail >= capacity
            || self.len > capacity
            || (self.head + self.len) % capacity != self.tail
        {
            return Err(Error::Terminal);
        }
        for (slot, transfer) in self.transfers.iter().enumerate() {
            let occupied = (slot as u64 + capacity - self.head) % capacity < self.len;
            if transfer.is_some() && (!occupied || self.values[slot] != 0) {
                return Err(Error::Terminal);
            }
        }
        Ok(())
    }
    fn push(&mut self, value: u64) -> Result<()> {
        self.validate()?;
        if self.len == MESSAGES as u64 {
            return Err(Error::Full);
        }
        self.values[self.tail as usize] = value;
        self.tail = (self.tail + 1) % MESSAGES as u64;
        self.len += 1;
        Ok(())
    }
    fn pop(&mut self) -> Result<Option<u64>> {
        self.validate()?;
        if self.len == 0 {
            return Ok(None);
        }
        let v = self.values[self.head as usize];
        if self.transfers[self.head as usize].is_some() {
            return Err(Error::MessageKind);
        }
        self.values[self.head as usize] = 0;
        self.head = (self.head + 1) % MESSAGES as u64;
        self.len -= 1;
        Ok(Some(v))
    }
    fn words(&self) -> [u64; 5] {
        [
            self.head,
            self.tail,
            self.len,
            self.values[0],
            self.values[1],
        ]
    }
}
#[derive(Clone, Copy)]
struct Endpoint {
    generation: u32,
    info: Option<EndpointInfo>,
}
pub struct Channels {
    generation: u32,
    live: bool,
    ends: [Option<EndpointId>; 2],
    owners: [u64; 2],
    endpoints: [Endpoint; ENDPOINTS],
    queues: [Fifo; 2],
    sent: [u64; 2],
    received: [u64; 2],
    discarded: [u64; 2],
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub channels: usize,
    pub endpoints: usize,
    pub queued: usize,
}
impl Default for Channels {
    fn default() -> Self {
        Self::new()
    }
}
impl Channels {
    pub const fn new() -> Self {
        Self {
            generation: 0,
            live: false,
            ends: [None; 2],
            owners: [0; 2],
            endpoints: [Endpoint {
                generation: 0,
                info: None,
            }; 2],
            queues: [const { Fifo::EMPTY }; 2],
            sent: [0; 2],
            received: [0; 2],
            discarded: [0; 2],
        }
    }
    pub fn available(&self) -> Result<()> {
        if self.live
            || self.generation == u32::MAX
            || self
                .endpoints
                .iter()
                .any(|e| e.info.is_some() || e.generation == u32::MAX)
        {
            Err(Error::Capacity)
        } else {
            if self
                .queues
                .iter()
                .any(|q| q.len != 0 || q.transfers.iter().any(Option::is_some))
            {
                return Err(Error::Terminal);
            }
            Ok(())
        }
    }
    pub(crate) fn pair(&mut self, owners: [NodeId; 2]) -> Result<[EndpointRef; 2]> {
        self.available()?;
        if owners[0] == owners[1] {
            return Err(Error::Owner);
        }
        self.generation += 1;
        self.live = true;
        self.queues = [const { Fifo::EMPTY }; 2];
        self.sent = [0; 2];
        self.received = [0; 2];
        self.discarded = [0; 2];
        let channel = ChannelId {
            generation: self.generation,
        };
        let refs = core::array::from_fn(|side| {
            let slot = side;
            let generation = self.endpoints[slot].generation + 1;
            let id = EndpointId { slot, generation };
            self.endpoints[slot] = Endpoint {
                generation,
                info: Some(EndpointInfo {
                    id,
                    channel,
                    side,
                    owner: owners[side],
                }),
            };
            self.ends[side] = Some(id);
            self.owners[side] = owners[side].raw();
            EndpointRef { id }
        });
        Ok(refs)
    }
    pub fn info(&self, id: EndpointId) -> Result<EndpointInfo> {
        let e = self.endpoints.get(id.slot).ok_or(Error::Object)?;
        if e.generation != id.generation {
            return Err(Error::Object);
        }
        let info = e.info.ok_or(Error::Object)?;
        if info.side >= ENDPOINTS
            || !self.live
            || info.channel.generation != self.generation
            || self.ends[info.side] != Some(id)
        {
            return Err(Error::Terminal);
        }
        Ok(info)
    }
    fn owned(&self, id: EndpointId, owner: NodeId) -> Result<EndpointInfo> {
        let i = self.info(id)?;
        if i.owner != owner {
            Err(Error::Owner)
        } else {
            Ok(i)
        }
    }
    fn peer_live(&self, info: EndpointInfo) -> Result<bool> {
        let side = 1 - info.side;
        match self.ends[side] {
            None => Ok(false),
            Some(id) => {
                let peer = self.info(id)?;
                if peer.channel != info.channel || peer.side != side {
                    Err(Error::Terminal)
                } else {
                    Ok(true)
                }
            }
        }
    }
    pub(crate) fn can_send(&self, id: EndpointId, owner: NodeId) -> Result<EndpointInfo> {
        let i = self.owned(id, owner)?;
        let peer = 1 - i.side;
        if !self.peer_live(i)? {
            return Err(Error::PeerClosed);
        }
        self.sent[i.side].checked_add(1).ok_or(Error::Capacity)?;
        self.queues[peer].validate()?;
        if self.queues[peer].len == MESSAGES as u64 {
            return Err(Error::Full);
        }
        Ok(i)
    }
    pub(crate) fn send(&mut self, id: EndpointId, owner: NodeId, payload: u64) -> Result<()> {
        let i = self.can_send(id, owner)?;
        let peer = 1 - i.side;
        let count = self.sent[i.side] + 1;
        self.queues[peer].push(payload)?; // linearization: scalar becomes queued
        self.sent[i.side] = count;
        Ok(())
    }
    pub(crate) fn send_transfer(&mut self, id: EndpointId, owner: NodeId, transfer: OwnedTransfer) {
        let i = self
            .can_send(id, owner)
            .expect("exclusive send-cap preflight");
        let q = &mut self.queues[1 - i.side];
        q.transfers[q.tail as usize] = Some(transfer);
        q.values[q.tail as usize] = 0;
        q.tail = (q.tail + 1) % MESSAGES as u64;
        q.len += 1;
        self.sent[i.side] += 1;
    }
    pub(crate) fn peek_transfer(&self, id: EndpointId, owner: NodeId) -> Result<&OwnedTransfer> {
        let i = self.owned(id, owner)?;
        let live = self.peer_live(i)?;
        let q = &self.queues[i.side];
        q.validate()?;
        if q.len == 0 {
            return Err(if live {
                Error::Empty
            } else {
                Error::PeerClosed
            });
        }
        self.received[i.side]
            .checked_add(1)
            .ok_or(Error::Capacity)?;
        q.transfers[q.head as usize]
            .as_ref()
            .ok_or(Error::MessageKind)
    }
    pub(crate) fn receive_transfer(
        &mut self,
        id: EndpointId,
        owner: NodeId,
    ) -> Result<OwnedTransfer> {
        self.peek_transfer(id, owner)?;
        let i = self.owned(id, owner)?;
        let q = &mut self.queues[i.side];
        let transfer = q.transfers[q.head as usize]
            .take()
            .expect("checked capability head");
        q.head = (q.head + 1) % MESSAGES as u64;
        q.len -= 1;
        self.received[i.side] += 1;
        Ok(transfer)
    }
    pub(crate) fn receive(&mut self, id: EndpointId, owner: NodeId) -> Result<u64> {
        let i = self.owned(id, owner)?;
        let peer_live = self.peer_live(i)?;
        self.queues[i.side].validate()?;
        if self.queues[i.side].len == 0 {
            return Err(if peer_live {
                Error::Empty
            } else {
                Error::PeerClosed
            });
        }
        let count = self.received[i.side]
            .checked_add(1)
            .ok_or(Error::Capacity)?;
        let value = self.queues[i.side].pop()?.ok_or(Error::Empty)?; // linearization: removes exactly one scalar
        self.received[i.side] = count;
        Ok(value)
    }
    pub(crate) fn retire_owned(
        &mut self,
        reference: EndpointRef,
    ) -> Result<[Option<OwnedTransfer>; MESSAGES]> {
        let i = self.info(reference.id)?;
        self.queues[i.side].validate()?;
        // Retirement cannot publish a partially accounted discard. The sole
        // caller treats failure as a kernel invariant violation (fail-stop).
        let discarded = self.discarded[i.side]
            .checked_add(self.queues[i.side].len)
            .ok_or(Error::Terminal)?;
        self.discarded[i.side] = discarded;
        let transfers = core::mem::replace(
            &mut self.queues[i.side].transfers,
            [const { None }; MESSAGES],
        );
        self.queues[i.side] = Fifo::EMPTY;
        self.ends[i.side] = None;
        self.endpoints[reference.id.slot].info = None;
        if self.ends == [None; 2] {
            self.live = false;
        }
        Ok(transfers)
    }
    #[cfg(test)]
    fn retire(&mut self, reference: EndpointRef) -> Result<()> {
        let transfers = self.retire_owned(reference)?;
        assert!(
            transfers.iter().all(Option::is_none),
            "scalar fixture cannot discard owned authority"
        );
        Ok(())
    }
    /// Supervisor-only per-slot tags and transfer identity/rights for QMP.
    pub fn transfer_snapshot(&self) -> [u64; 12] {
        let mut words = [0; 12];
        for (side, q) in self.queues.iter().enumerate() {
            for (slot, transfer) in q.transfers.iter().enumerate() {
                if let Some(t) = transfer {
                    let (id, rights) = t.description();
                    let offset = side * 6 + slot * 3;
                    words[offset..offset + 3].copy_from_slice(&[1, id.raw(), rights.bits()]);
                }
            }
        }
        words
    }
    pub fn queued_capabilities(&self) -> u32 {
        self.queues
            .iter()
            .map(|q| q.transfers.iter().filter(|t| t.is_some()).count() as u32)
            .sum()
    }
    #[cfg(test)]
    pub(crate) fn test_counters(&mut self, sent: [u64; 2], received: [u64; 2]) {
        // Boundary injection exists only in host tests, never a guest ABI.
        self.sent = sent;
        self.received = received;
    }
    #[cfg(test)]
    pub(crate) fn test_discarded(&mut self, discarded: [u64; 2]) {
        // Boundary injection exists only in host tests, never a guest ABI.
        self.discarded = discarded;
    }
    pub fn stats(&self) -> Stats {
        Stats {
            channels: usize::from(self.live),
            endpoints: self.ends.iter().flatten().count(),
            queued: self.queues.iter().map(|q| q.len as usize).sum(),
        }
    }
    /// Trusted diagnostics, never a userspace result or authority selector.
    pub fn snapshot(&self) -> [u64; 22] {
        let mut w = [0; 22];
        w[..6].copy_from_slice(&[
            if self.generation == 0 {
                0
            } else {
                ChannelId {
                    generation: self.generation,
                }
                .raw()
            },
            self.live as u64,
            self.ends[0].map_or(0, EndpointId::raw),
            self.ends[1].map_or(0, EndpointId::raw),
            self.owners[0],
            self.owners[1],
        ]);
        w[6..11].copy_from_slice(&self.queues[0].words());
        w[11..16].copy_from_slice(&self.queues[1].words());
        w[16..22].copy_from_slice(&[
            self.sent[0],
            self.sent[1],
            self.received[0],
            self.received[1],
            self.discarded[0],
            self.discarded[1],
        ]);
        w
    }
    #[cfg(feature = "channel-test")]
    pub fn queue_addresses(&self) -> [u64; 2] {
        self.queues
            .each_ref()
            .map(|q| core::ptr::from_ref(q) as u64)
    }
}
#[cfg(test)]
mod tests;

//! One bounded duplex Channel. Mechanism identities are never user handles.
//! Mutation requires the registry's exclusive IF=0 transaction in the kernel.
use crate::{
    capability::{Error, Result},
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
#[derive(Clone, Copy)]
struct Fifo {
    head: u64,
    tail: u64,
    len: u64,
    values: [u64; MESSAGES],
}
impl Fifo {
    const EMPTY: Self = Self {
        head: 0,
        tail: 0,
        len: 0,
        values: [0; MESSAGES],
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
        self.values[self.head as usize] = 0;
        self.head = (self.head + 1) % MESSAGES as u64;
        self.len -= 1;
        Ok(Some(v))
    }
    fn words(self) -> [u64; 5] {
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
            queues: [Fifo::EMPTY; 2],
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
        self.queues = [Fifo::EMPTY; 2];
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
    pub(crate) fn send(&mut self, id: EndpointId, owner: NodeId, payload: u64) -> Result<()> {
        let i = self.owned(id, owner)?;
        let peer = 1 - i.side;
        if !self.peer_live(i)? {
            return Err(Error::PeerClosed);
        }
        let count = self.sent[i.side].checked_add(1).ok_or(Error::Capacity)?;
        self.queues[peer].push(payload)?; // linearization: scalar becomes queued
        self.sent[i.side] = count;
        Ok(())
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
    pub(crate) fn retire(&mut self, reference: EndpointRef) -> Result<()> {
        let i = self.info(reference.id)?;
        self.queues[i.side].validate()?;
        // Retirement cannot publish a partially accounted discard. The sole
        // caller treats failure as a kernel invariant violation (fail-stop).
        let discarded = self.discarded[i.side]
            .checked_add(self.queues[i.side].len)
            .ok_or(Error::Terminal)?;
        self.discarded[i.side] = discarded;
        self.queues[i.side] = Fifo::EMPTY;
        self.ends[i.side] = None;
        self.endpoints[reference.id.slot].info = None;
        if self.ends == [None; 2] {
            self.live = false;
        }
        Ok(())
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

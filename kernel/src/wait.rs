//! Bounded pending Channel operations. Registrations are owned scalars only;
//! they never contain kernel pointers or borrowed capability state.
use crate::{
    capability::{Error, ObjectId, Result},
    channel::{ChannelId, EndpointId},
    node::{ActiveContext, NodeId, ThreadId},
};
#[cfg(feature = "channel-wait-test")]
use core::cell::UnsafeCell;
/// The supported topology has three Threads. A Thread owns at most one wait.
pub const WAITERS: usize = 3;
/// Bounded supervisor-only ledger used by the QMP verification profile. It is
/// not an ABI surface and is compiled only for the blocking integration build.
#[cfg(feature = "channel-wait-test")]
pub const WITNESS_RECORDS: usize = 48;
#[cfg(feature = "channel-wait-test")]
pub const WITNESS_WORDS: usize = 28;
#[cfg(feature = "channel-wait-test")]
pub mod witness {
    use super::*;
    pub const REGISTER: u64 = 1;
    pub const WAKE: u64 = 2;
    pub const TERMINAL: u64 = 3;
    pub const TAKE: u64 = 4;
    pub const RETRY: u64 = 5;
    pub const COMMIT: u64 = 6;
    pub const FAILED: u64 = 7;
    pub const CANCEL: u64 = 8;
    pub const PIN: u64 = 9;
    pub const BLOCKED: u64 = 10;
    pub const WOKEN: u64 = 11;
    pub const QUIESCE: u64 = 12;
    pub const REMOVE: u64 = 13;
    #[repr(C)]
    #[derive(Clone, Copy)]
    struct Record {
        words: [u64; WITNESS_WORDS],
    }
    impl Record {
        const EMPTY: Self = Self {
            words: [0; WITNESS_WORDS],
        };
    }
    #[repr(C)]
    struct Ledger {
        next: u64,
        count: u64,
        records: [Record; WITNESS_RECORDS],
    }
    #[repr(transparent)]
    struct Storage(UnsafeCell<Ledger>);
    // SAFETY: the witness is mutated only from the single-CPU supervisor while
    // the caller holds the exclusive scheduler/registry transaction.
    unsafe impl Sync for Storage {}
    #[unsafe(no_mangle)]
    static neu_wait_witness: Storage = Storage(UnsafeCell::new(Ledger {
        next: 0,
        count: 0,
        records: [Record::EMPTY; WITNESS_RECORDS],
    }));
    pub struct Saved {
        pub present: bool,
        pub rip: u64,
        pub rsp: u64,
        pub flags: u64,
    }
    impl Saved {
        pub const NONE: Self = Self {
            present: false,
            rip: 0,
            rsp: 0,
            flags: 0,
        };
    }
    /// Monotonic publication count. Consumers use it to order deferred Channel
    /// commits against the per-syscall diagnostic ledger without embedding
    /// kernel pointers in either record.
    pub fn cursor() -> u64 {
        // SAFETY: single-CPU supervisor reads the ledger while IF=0.
        unsafe { (*neu_wait_witness.0.get()).next }
    }
    #[allow(clippy::too_many_arguments)]
    pub fn write(
        event: u64,
        waiter: Option<Waiter>,
        result: i64,
        queue: usize,
        stats: Stats,
        node_count: usize,
        pins: u32,
        saved: Saved,
    ) {
        // SAFETY: exclusive supervisor transaction; QMP observes stopped CPUs.
        let state = unsafe { &mut *neu_wait_witness.0.get() };
        let slot = (state.next as usize) % WITNESS_RECORDS;
        state.next += 1;
        if state.count < WITNESS_RECORDS as u64 {
            state.count += 1;
        }
        let mut words = [0u64; WITNESS_WORDS];
        words[0] = event;
        words[1] = state.next;
        if let Some(w) = waiter {
            words[2] = w.active.thread.raw();
            words[3] = w.active.node.raw();
            words[4] = w.token;
            words[5] = w.channel.raw();
            words[6] = w.endpoint.raw();
            words[7] = match w.kind {
                Kind::Send => 1,
                Kind::Receive => 2,
            };
            words[8] = match w.state {
                State::Waiting => 1,
                State::Ready => 2,
                State::Queued => 3,
            };
            words[9] = u64::from(w.pinned);
            words[10] = w.terminal.map_or(0, |e| e as i64 as u64);
            words[11] = w.payload;
            words[12] = w.side as u64;
            words[13] = w.sequence;
            words[20] = w.object.raw();
        }
        words[14] = result as u64;
        words[15] = stats.waiting as u64;
        words[16] = stats.ready as u64;
        words[17] = stats.queued as u64;
        words[18] = stats.terminal as u64;
        words[19] = queue as u64;
        words[21] = node_count as u64;
        words[22] = pins as u64;
        words[23] = u64::from(saved.present);
        words[24] = saved.rip;
        words[25] = saved.rsp;
        words[26] = saved.flags;
        state.records[slot] = Record { words };
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Send,
    Receive,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Waiting,
    Ready,
    Queued,
}
#[derive(Clone, Copy, Debug)]
pub struct Waiter {
    pub active: ActiveContext,
    pub endpoint: EndpointId,
    pub channel: ChannelId,
    pub object: ObjectId,
    pub side: usize,
    pub owner: NodeId,
    pub kind: Kind,
    pub payload: u64,
    pub state: State,
    pub sequence: u64,
    pub token: u64,
    /// Set when endpoint retirement/cancellation proves the result without a
    /// live endpoint to recheck. The pin is released at the same instant.
    pub terminal: Option<Error>,
    pub pinned: bool,
}
impl Waiter {
    pub fn thread(self) -> ThreadId {
        self.active.thread
    }
    pub fn node(self) -> NodeId {
        self.active.node
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    pub waiting: usize,
    pub ready: usize,
    pub queued: usize,
    pub terminal: usize,
}
impl Stats {
    pub fn total(self) -> usize {
        self.waiting + self.ready + self.queued + self.terminal
    }
}
pub struct Waits {
    slots: [Option<Waiter>; WAITERS],
    sequence: u64,
    token: u64,
}
impl Default for Waits {
    fn default() -> Self {
        Self::new()
    }
}
impl Waits {
    #[cfg(feature = "channel-wait-test")]
    fn witness(&self, event: u64, waiter: Option<Waiter>, result: i64) {
        witness::write(
            event,
            waiter,
            result,
            0,
            self.stats(),
            waiter.map_or(0, |w| self.node_count(w.active.node)),
            0,
            witness::Saved::NONE,
        );
    }
    pub const fn new() -> Self {
        Self {
            slots: [None; WAITERS],
            sequence: 0,
            token: 0,
        }
    }
    /// Test-only rollback seam: force the next registration to fail after the
    /// caller has already taken its operation pin.
    #[cfg(test)]
    pub(crate) fn force_sequence_exhaustion(&mut self) {
        self.sequence = u64::MAX;
    }
    pub fn is_empty(&self) -> bool {
        self.slots.iter().all(Option::is_none)
    }
    pub fn len(&self) -> usize {
        self.slots.iter().flatten().count()
    }
    pub fn stats(&self) -> Stats {
        let mut stats = Stats::default();
        for waiter in self.slots.iter().flatten() {
            match waiter.state {
                State::Waiting => stats.waiting += 1,
                State::Ready => stats.ready += 1,
                State::Queued => stats.queued += 1,
            }
            stats.terminal += usize::from(waiter.terminal.is_some());
        }
        stats
    }
    pub fn capacity_available(&self) -> bool {
        self.slots.iter().any(Option::is_none)
    }
    pub fn by_thread(&self, thread: ThreadId) -> Option<Waiter> {
        self.slots
            .iter()
            .flatten()
            .copied()
            .find(|waiter| waiter.thread() == thread)
    }
    pub fn by_token(&self, token: u64) -> Option<Waiter> {
        self.slots
            .iter()
            .flatten()
            .copied()
            .find(|waiter| waiter.token == token)
    }
    pub fn node_count(&self, node: NodeId) -> usize {
        self.slots
            .iter()
            .flatten()
            .filter(|waiter| waiter.node() == node)
            .count()
    }
    pub fn pinned_outside(&self, node: NodeId) -> u32 {
        self.slots
            .iter()
            .flatten()
            .filter(|waiter| waiter.pinned && waiter.node() != node)
            .count() as u32
    }
    /// Preflight before taking an object pin. A Thread cannot own two waits.
    pub fn can_register(&self, thread: ThreadId) -> bool {
        self.capacity_available() && self.by_thread(thread).is_none()
    }
    /// Registration commits infallibly after `can_register` and an object pin.
    // The scalar fields mirror the fixed waiter record one-for-one.
    #[allow(clippy::too_many_arguments)]
    pub fn register(
        &mut self,
        active: ActiveContext,
        endpoint: EndpointId,
        channel: ChannelId,
        object: ObjectId,
        side: usize,
        owner: NodeId,
        kind: Kind,
        payload: u64,
    ) -> Result<u64> {
        if !self.can_register(active.thread) {
            return Err(if self.by_thread(active.thread).is_some() {
                Error::InUse
            } else {
                Error::Capacity
            });
        }
        let slot = self
            .slots
            .iter()
            .position(Option::is_none)
            .ok_or(Error::Capacity)?;
        self.sequence = self.sequence.checked_add(1).ok_or(Error::Capacity)?;
        self.token = self.token.checked_add(1).ok_or(Error::Capacity)?;
        let waiter = Waiter {
            active,
            endpoint,
            channel,
            object,
            side,
            owner,
            kind,
            payload,
            state: State::Waiting,
            sequence: self.sequence,
            token: self.token,
            terminal: None,
            pinned: true,
        };
        self.slots[slot] = Some(waiter);
        #[cfg(feature = "channel-wait-test")]
        self.witness(witness::REGISTER, Some(waiter), 0);
        Ok(waiter.token)
    }
    /// Restores a Ready/Queued waiter after a lost-wakeup recheck. The caller
    /// keeps its original pin; no second pin is taken.
    pub fn restore(&mut self, mut waiter: Waiter) -> bool {
        if self.by_token(waiter.token).is_some() {
            return false;
        }
        let Some(slot) = self.slots.iter().position(Option::is_none) else {
            return false;
        };
        waiter.state = State::Waiting;
        self.slots[slot] = Some(waiter);
        #[cfg(feature = "channel-wait-test")]
        self.witness(witness::RETRY, Some(waiter), 0);
        true
    }
    pub fn remove(&mut self, token: u64) -> Option<Waiter> {
        let slot = self
            .slots
            .iter()
            .position(|waiter| waiter.is_some_and(|w| w.token == token))?;
        let waiter = self.slots[slot].take();
        #[cfg(feature = "channel-wait-test")]
        if let Some(waiter) = waiter {
            self.witness(witness::REMOVE, Some(waiter), 0);
        }
        waiter
    }
    pub fn update(&mut self, waiter: Waiter) {
        if let Some(slot) = self
            .slots
            .iter()
            .position(|w| w.is_some_and(|w| w.token == waiter.token))
        {
            self.slots[slot] = Some(waiter);
        }
    }
    pub fn mark_ready(&mut self, token: u64) -> bool {
        let Some(slot) = self
            .slots
            .iter()
            .position(|w| w.is_some_and(|w| w.token == token))
        else {
            return false;
        };
        let waiter = self.slots[slot].as_mut().unwrap();
        if waiter.state != State::Waiting {
            return false;
        }
        waiter.state = State::Ready;
        let copy = *waiter;
        #[cfg(feature = "channel-wait-test")]
        self.witness(witness::WAKE, Some(copy), 0);
        let _ = copy;
        true
    }
    pub fn mark_waiting(&mut self, token: u64) -> bool {
        let Some(slot) = self
            .slots
            .iter()
            .position(|w| w.is_some_and(|w| w.token == token))
        else {
            return false;
        };
        let waiter = self.slots[slot].as_mut().unwrap();
        if !matches!(waiter.state, State::Ready | State::Queued) {
            return false;
        }
        waiter.state = State::Waiting;
        let copy = *waiter;
        #[cfg(feature = "channel-wait-test")]
        self.witness(witness::RETRY, Some(copy), 0);
        let _ = copy;
        true
    }
    /// Marks a waiter terminal and reports whether its held pin must be released
    /// by the caller. Only Waiting/Ready/Queued transitions are accepted.
    pub fn mark_terminal(&mut self, token: u64, error: Error) -> bool {
        let Some(slot) = self
            .slots
            .iter()
            .position(|w| w.is_some_and(|w| w.token == token))
        else {
            return false;
        };
        let waiter = self.slots[slot].as_mut().unwrap();
        if waiter.terminal.is_some() {
            return false;
        }
        waiter.terminal = Some(error);
        // A waiter already admitted to the run queue (Ready/Queued) keeps its
        // state. Downgrading Queued to Ready would let drain_ready admit the
        // same Thread twice and fail the scheduler's InUse check. Only a
        // Waiting registration needs a fresh admission here.
        if waiter.state == State::Waiting {
            waiter.state = State::Ready;
        }
        let copy = *waiter;
        #[cfg(feature = "channel-wait-test")]
        self.witness(witness::TERMINAL, Some(copy), error as i64);
        let _ = copy;
        true
    }
    pub fn release_pin(&mut self, token: u64) -> bool {
        let Some(slot) = self
            .slots
            .iter()
            .position(|w| w.is_some_and(|w| w.token == token))
        else {
            return false;
        };
        let waiter = self.slots[slot].as_mut().unwrap();
        if !waiter.pinned {
            return false;
        }
        waiter.pinned = false;
        let copy = *waiter;
        #[cfg(feature = "channel-wait-test")]
        self.witness(witness::PIN, Some(copy), 0);
        let _ = copy;
        true
    }
    /// Wakes the oldest eligible waiter, or all affected waiters on terminal
    /// peer closure. Wakeup grants scheduling eligibility only.
    pub fn wake(&mut self, channel: ChannelId, side: usize, kind: Kind, all: bool) -> usize {
        let mut count = 0;
        loop {
            let mut choice: Option<(u64, u64)> = None;
            for waiter in self.slots.iter().flatten() {
                if waiter.state == State::Waiting
                    && waiter.terminal.is_none()
                    && waiter.channel == channel
                    && waiter.side == side
                    && waiter.kind == kind
                    && choice.is_none_or(|(sequence, _)| waiter.sequence < sequence)
                {
                    choice = Some((waiter.sequence, waiter.token));
                }
            }
            let Some((_, token)) = choice else {
                break;
            };
            if !self.mark_ready(token) {
                break;
            }
            count += 1;
            if !all {
                break;
            }
        }
        count
    }
    /// Wakes every waiting operation on a Channel after an endpoint becomes
    /// terminal. Used when retirement closes the surviving direction.
    pub fn wake_channel(&mut self, channel: ChannelId, all: bool) -> usize {
        let mut count = 0;
        loop {
            let mut choice: Option<(u64, u64)> = None;
            for waiter in self.slots.iter().flatten() {
                if waiter.state == State::Waiting
                    && waiter.terminal.is_none()
                    && waiter.channel == channel
                    && choice.is_none_or(|(sequence, _)| waiter.sequence < sequence)
                {
                    choice = Some((waiter.sequence, waiter.token));
                }
            }
            let Some((_, token)) = choice else {
                break;
            };
            if !self.mark_ready(token) {
                break;
            }
            count += 1;
            if !all {
                break;
            }
        }
        count
    }
    /// Moves every Ready registration to Queued and returns copies in FIFO
    /// order. Registrations remain owned by the registry until settle/cancel so
    /// a close or fault can still find and terminate them.
    pub fn take_ready(&mut self) -> [Option<Waiter>; WAITERS] {
        let mut result = [None; WAITERS];
        let mut count = 0;
        loop {
            let mut choice: Option<(u64, usize)> = None;
            for (slot, waiter) in self.slots.iter().enumerate() {
                let Some(waiter) = waiter else {
                    continue;
                };
                if waiter.state == State::Ready
                    && choice.is_none_or(|(sequence, _)| waiter.sequence < sequence)
                {
                    choice = Some((waiter.sequence, slot));
                }
            }
            let Some((_, slot)) = choice else {
                break;
            };
            self.slots[slot].as_mut().unwrap().state = State::Queued;
            result[count] = self.slots[slot];
            #[cfg(feature = "channel-wait-test")]
            if let Some(waiter) = result[count] {
                self.witness(witness::TAKE, Some(waiter), 0);
            }
            count += 1;
        }
        result
    }
    /// Restores a Queued registration after a lost-wakeup recheck. The pin is
    /// unchanged; no second pin is taken.
    pub fn retry(&mut self, token: u64) -> bool {
        let Some(slot) = self
            .slots
            .iter()
            .position(|w| w.is_some_and(|w| w.token == token))
        else {
            return false;
        };
        let waiter = self.slots[slot].as_mut().unwrap();
        if waiter.state != State::Queued {
            return false;
        }
        waiter.state = State::Waiting;
        let copy = *waiter;
        #[cfg(feature = "channel-wait-test")]
        self.witness(witness::RETRY, Some(copy), 0);
        let _ = copy;
        true
    }
    pub fn tokens(&self) -> [Option<u64>; WAITERS] {
        let mut result = [None; WAITERS];
        for (slot, waiter) in self.slots.iter().enumerate() {
            result[slot] = waiter.map(|w| w.token);
        }
        result
    }
    pub fn snapshot(&self) -> [u64; 64] {
        let mut words = [0; 64];
        let stats = self.stats();
        words[0] = stats.waiting as u64;
        words[1] = stats.ready as u64;
        words[2] = stats.queued as u64;
        words[3] = stats.terminal as u64;
        let mut count = 0;
        for waiter in self.slots.iter().flatten() {
            let base = 4 + count * 12;
            if base + 12 > words.len() {
                break;
            }
            words[base] = waiter.token;
            words[base + 1] = waiter.active.thread.raw();
            words[base + 2] = waiter.active.node.raw();
            words[base + 3] = waiter.endpoint.raw();
            words[base + 4] = waiter.channel.raw();
            words[base + 5] = match waiter.kind {
                Kind::Send => 1,
                Kind::Receive => 2,
            };
            words[base + 6] = match waiter.state {
                State::Waiting => 1,
                State::Ready => 2,
                State::Queued => 3,
            };
            words[base + 7] = waiter.payload;
            words[base + 8] = waiter.terminal.map_or(0, |e| e as i64 as u64);
            words[base + 9] = u64::from(waiter.pinned);
            words[base + 10] = waiter.side as u64;
            words[base + 11] = waiter.sequence;
            count += 1;
        }
        words[63] = count as u64;
        words
    }
}

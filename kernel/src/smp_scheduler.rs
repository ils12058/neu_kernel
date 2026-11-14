//! Bounded per-CPU FIFO scheduling. All methods require exclusive access to the
//! scheduler and Nodes together. Architecture code provides an IRQ-disabled
//! SpinLock transaction and never retains a borrow across user execution.
use crate::{
    context::{Dispatch, SavedContext},
    cpu::MAX_CPUS,
    node::{self, ActiveContext, ExitReason, MAX_NODES, MAX_THREADS, Nodes, ThreadId, ThreadState},
};

#[derive(Clone, Copy)]
struct Queue {
    entries: [Option<ThreadId>; MAX_THREADS],
    len: usize,
}
impl Queue {
    const fn new() -> Self {
        Self {
            entries: [None; MAX_THREADS],
            len: 0,
        }
    }
    fn contains(&self, id: ThreadId) -> bool {
        self.entries[..self.len].contains(&Some(id))
    }
    fn push(&mut self, id: ThreadId) {
        self.entries[self.len] = Some(id);
        self.len += 1;
    }
    fn remove(&mut self, id: ThreadId) {
        let mut next = Self::new();
        for value in self.entries[..self.len].iter().flatten().copied() {
            if value != id {
                next.push(value);
            }
        }
        *self = next;
    }
}

pub struct Scheduler {
    cpus: usize,
    queues: [Queue; MAX_CPUS],
    owners: [Option<(ThreadId, usize)>; MAX_THREADS],
    affinities: [u64; MAX_THREADS],
    stopping: [Option<ActiveContext>; MAX_NODES],
    pub dispatches: u64,
    pub preemptions: u64,
    pub blocks: u64,
    pub wakes: u64,
    pub migrations: u64,
    pub completions: u64,
}
impl Scheduler {
    #[cfg(feature = "smp-scheduler-test")]
    pub fn diagnostic_layout(&self) -> [u64; 72] {
        let mut words = [0; 72];
        words[..10].copy_from_slice(&[
            1,
            core::ptr::from_ref(&self.cpus) as u64,
            MAX_CPUS as u64,
            MAX_THREADS as u64,
            core::ptr::from_ref(&self.dispatches) as u64,
            core::ptr::from_ref(&self.preemptions) as u64,
            core::ptr::from_ref(&self.blocks) as u64,
            core::ptr::from_ref(&self.wakes) as u64,
            core::ptr::from_ref(&self.migrations) as u64,
            core::ptr::from_ref(&self.completions) as u64,
        ]);
        for (cpu, q) in self.queues.iter().enumerate() {
            let start = 16 + cpu * 7;
            words[start] = core::ptr::from_ref(&q.len) as u64;
            for (slot, id) in q.entries.iter().enumerate() {
                if let Some(id) = id {
                    words[start + 1 + slot * 2..start + 3 + slot * 2]
                        .copy_from_slice(&id.smp_field_addresses());
                }
            }
        }
        for (slot, owner) in self.owners.iter().enumerate() {
            if let Some((id, cpu)) = owner {
                let addresses = id.smp_field_addresses();
                words[44 + slot * 4..48 + slot * 4].copy_from_slice(&[
                    addresses[0],
                    addresses[1],
                    core::ptr::from_ref(cpu) as u64,
                    core::ptr::from_ref(&self.affinities[slot]) as u64,
                ]);
            }
        }
        for (slot, origin) in self.stopping.iter().enumerate() {
            if let Some(origin) = origin {
                let addresses = origin.thread.smp_field_addresses();
                words[56 + slot * 3..59 + slot * 3].copy_from_slice(&[
                    addresses[0],
                    addresses[1],
                    core::ptr::from_ref(&origin.context.root) as u64,
                ]);
            }
        }
        words
    }
    pub const fn empty() -> Self {
        Self {
            cpus: 0,
            queues: [Queue::new(); MAX_CPUS],
            owners: [None; MAX_THREADS],
            affinities: [0; MAX_THREADS],
            stopping: [None; MAX_NODES],
            dispatches: 0,
            preemptions: 0,
            blocks: 0,
            wakes: 0,
            migrations: 0,
            completions: 0,
        }
    }
    pub fn new(cpus: usize) -> node::Result<Self> {
        if cpus == 0 || cpus > MAX_CPUS {
            return Err(node::Error::Capacity);
        }
        Ok(Self {
            cpus,
            ..Self::empty()
        })
    }
    pub fn cpu_count(&self) -> usize {
        self.cpus
    }
    pub fn owner(&self, id: ThreadId) -> Option<usize> {
        self.owners
            .get(id.index())
            .copied()
            .flatten()
            .filter(|(thread, _)| *thread == id)
            .map(|(_, cpu)| cpu)
    }
    pub fn queued(&self, cpu: usize) -> [u64; MAX_THREADS] {
        self.queues.get(cpu).map_or([0; MAX_THREADS], |q| {
            q.entries.map(|id| id.map_or(0, ThreadId::raw))
        })
    }
    pub fn queue_len(&self, cpu: usize) -> usize {
        self.queues.get(cpu).map_or(0, |q| q.len)
    }
    pub fn stopping_for(&self, active: ActiveContext) -> Option<ThreadId> {
        self.stopping
            .iter()
            .flatten()
            .find(|owner| owner.node == active.node)
            .map(|owner| owner.thread)
    }
    pub fn admit(
        &mut self,
        model: &mut Nodes,
        cpu: usize,
        id: ThreadId,
        affinity: u64,
    ) -> node::Result<()> {
        let thread = model.thread(id)?;
        let active = ActiveContext {
            node: thread.node,
            thread: id,
            context: thread.context,
        };
        if cpu >= self.cpus
            || affinity == 0
            || affinity >> self.cpus != 0
            || affinity & (1 << cpu) == 0
        {
            return Err(node::Error::Capacity);
        }
        if self.owner(id).is_some()
            || self.owners[id.index()].is_some()
            || self.queues.iter().any(|q| q.contains(id))
            || self.stopping_for(active).is_some()
        {
            return Err(node::Error::InUse);
        }
        model.enqueue_smp(id)?;
        self.owners[id.index()] = Some((id, cpu));
        self.affinities[id.index()] = affinity;
        self.queues[cpu].push(id);
        Ok(())
    }
    pub fn dispatch(&mut self, model: &mut Nodes, cpu: usize) -> node::Result<Option<Dispatch>> {
        if cpu >= self.cpus {
            return Err(node::Error::Capacity);
        }
        if model.active_on(cpu).is_some() {
            return Err(node::Error::InUse);
        }
        let Some(id) = self.queues[cpu].entries[0] else {
            return Ok(None);
        };
        if self.owner(id) != Some(cpu) {
            return Err(node::Error::Association);
        }
        let t = model.thread(id)?;
        let active = ActiveContext {
            node: t.node,
            thread: id,
            context: t.context,
        };
        if self.stopping_for(active).is_some() {
            return Err(node::Error::InUse);
        }
        let dispatch = model.dispatch_on(cpu, id)?;
        self.queues[cpu].remove(id);
        self.dispatches += 1;
        Ok(Some(dispatch))
    }
    pub fn reschedule(
        &mut self,
        model: &mut Nodes,
        cpu: usize,
        saved: SavedContext,
    ) -> node::Result<()> {
        let id = saved.active.thread;
        if self.owner(id) != Some(cpu)
            || self.queues.iter().any(|q| q.contains(id))
            || self.stopping_for(saved.active).is_some()
        {
            return Err(node::Error::Association);
        }
        model.suspend_on(cpu, saved, false)?;
        model.enqueue_smp(id)?;
        self.queues[cpu].push(id);
        if saved.interrupted {
            self.preemptions += 1;
        }
        Ok(())
    }
    pub fn block(
        &mut self,
        model: &mut Nodes,
        cpu: usize,
        saved: SavedContext,
    ) -> node::Result<()> {
        if self.owner(saved.active.thread) != Some(cpu) || self.stopping_for(saved.active).is_some()
        {
            return Err(node::Error::Association);
        }
        model.suspend_on(cpu, saved, true)?;
        self.blocks += 1;
        Ok(())
    }
    pub fn wake(&mut self, model: &mut Nodes, id: ThreadId) -> node::Result<usize> {
        let cpu = self.owner(id).ok_or(node::Error::Identity)?;
        let t = model.thread(id)?;
        if self.queues.iter().any(|q| q.contains(id))
            || self
                .stopping_for(ActiveContext {
                    node: t.node,
                    thread: id,
                    context: t.context,
                })
                .is_some()
        {
            return Err(node::Error::InUse);
        }
        model.wake_smp(id)?;
        model.enqueue_smp(id)?;
        self.queues[cpu].push(id);
        self.wakes += 1;
        Ok(cpu)
    }
    /// A queued Thread has already left its source CPU and owns a complete
    /// saved context. A running or blocked Thread cannot migrate through here.
    pub fn migrate(
        &mut self,
        model: &Nodes,
        id: ThreadId,
        destination: usize,
    ) -> node::Result<usize> {
        let source = self.owner(id).ok_or(node::Error::Identity)?;
        let t = model.thread(id)?;
        if destination >= self.cpus || self.affinities[id.index()] & (1 << destination) == 0 {
            return Err(node::Error::Capacity);
        }
        if source == destination
            || t.state != ThreadState::Runnable
            || !t.queued
            || t.saved.is_none()
            || !self.queues[source].contains(id)
            || self
                .stopping_for(ActiveContext {
                    node: t.node,
                    thread: id,
                    context: t.context,
                })
                .is_some()
        {
            return Err(node::Error::InUse);
        }
        self.queues[source].remove(id);
        self.queues[destination].push(id);
        self.owners[id.index()] = Some((id, destination));
        self.migrations += 1;
        Ok(source)
    }
    pub fn begin_fault(
        &mut self,
        model: &Nodes,
        cpu: usize,
        active: ActiveContext,
    ) -> node::Result<u64> {
        if model.active_on(cpu) != Some(active) || self.owner(active.thread) != Some(cpu) {
            return Err(node::Error::Association);
        }
        let index = (active.node.raw() as u32)
            .checked_sub(1)
            .ok_or(node::Error::Identity)? as usize;
        if index >= MAX_NODES {
            return Err(node::Error::Identity);
        }
        if let Some(owner) = self.stopping[index] {
            if owner != active {
                return Err(node::Error::InUse);
            }
        } else {
            let mut queues = self.queues;
            // Queue removal precedes any CPU cancellation request. Nodes keeps
            // those Threads non-admissible until the origin commits UserFault.
            for q in &mut queues {
                let ids = q.entries;
                for id in ids.iter().flatten().copied() {
                    if model.thread(id)?.node == active.node {
                        q.remove(id);
                    }
                }
            }
            self.queues = queues;
            self.stopping[index] = Some(active);
        }
        let mut peers = 0;
        for other in 0..self.cpus {
            if model
                .active_on(other)
                .is_some_and(|a| a.node == active.node && a.thread != active.thread)
            {
                peers |= 1 << other;
            }
        }
        Ok(peers)
    }
    pub fn finish(
        &mut self,
        model: &mut Nodes,
        cpu: usize,
        active: ActiveContext,
        outcome: ExitReason,
    ) -> node::Result<()> {
        self.verify(model)?;
        if self.owner(active.thread) != Some(cpu) || model.active_on(cpu) != Some(active) {
            return Err(node::Error::Association);
        }
        // Completion requires the authority lease to have returned to Nodes.
        // Reject an invalid disposition before publishing any stop/queue edit.
        model.authority(active.node)?;
        match outcome {
            ExitReason::Normal(_) if self.stopping_for(active).is_some() => {
                return Err(node::Error::Association);
            }
            ExitReason::Cancelled { by }
                if self.stopping_for(active) != Some(by) || by == active.thread =>
            {
                return Err(node::Error::Association);
            }
            ExitReason::ConstructionFailure | ExitReason::Quiesced => {
                return Err(node::Error::Association);
            }
            _ => {}
        }
        if matches!(outcome, ExitReason::UserFault(_)) && self.begin_fault(model, cpu, active)? != 0
        {
            return Err(node::Error::InUse);
        }
        if let Some(origin) = self.stopping_for(active)
            && origin != active.thread
            && outcome != (ExitReason::Cancelled { by: origin })
        {
            return Err(node::Error::Association);
        }
        model.complete_on(cpu, active, outcome)?;
        self.completions += 1;
        for slot in 0..MAX_THREADS {
            if let Some((id, _)) = self.owners[slot]
                && matches!(
                    model.thread(id)?.state,
                    ThreadState::Exited | ThreadState::Reclaimed
                )
            {
                for q in &mut self.queues {
                    q.remove(id);
                }
                self.owners[slot] = None;
                self.affinities[slot] = 0;
            }
        }
        for stopping in &mut self.stopping {
            if stopping.is_some_and(|origin| {
                model.node(origin.node).is_ok_and(|n| {
                    matches!(
                        n.state,
                        node::NodeState::Exited | node::NodeState::Reclaimed
                    )
                })
            }) {
                *stopping = None;
            }
        }
        Ok(())
    }
    pub fn verify(&self, model: &Nodes) -> node::Result<()> {
        if self.cpus == 0 || self.cpus > MAX_CPUS {
            return Err(node::Error::Capacity);
        }
        let mut queued = 0u64;
        let mut running = 0u64;
        for cpu in 0..self.cpus {
            let q = &self.queues[cpu];
            if q.len > MAX_THREADS
                || q.entries[..q.len].iter().any(Option::is_none)
                || q.entries[q.len..].iter().any(Option::is_some)
            {
                return Err(node::Error::Association);
            }
            for id in self.queues[cpu].entries[..self.queues[cpu].len]
                .iter()
                .flatten()
                .copied()
            {
                let t = model.thread(id)?;
                let bit = 1 << id.index();
                if queued & bit != 0
                    || self.owner(id) != Some(cpu)
                    || t.state != ThreadState::Runnable
                    || !t.queued
                    || t.saved.is_none()
                {
                    return Err(node::Error::Association);
                }
                queued |= bit;
            }
            if let Some(active) = model.active_on(cpu) {
                let bit = 1 << active.thread.index();
                let t = model.thread(active.thread)?;
                if running & bit != 0
                    || queued & bit != 0
                    || self.owner(active.thread) != Some(cpu)
                    || t.state != ThreadState::Running
                    || t.saved.is_some()
                    || t.queued
                    || t.node != active.node
                    || t.context != active.context
                    || !t.stack.is_some_and(|stack| stack.valid(active))
                    || model.node(active.node)?.state != node::NodeState::Active
                    || model.node(active.node)?.space != Some(active.context.space)
                {
                    return Err(node::Error::Association);
                }
                running |= bit;
            }
        }
        if running & queued != 0 {
            return Err(node::Error::Association);
        }
        for cpu in self.cpus..MAX_CPUS {
            if self.queues[cpu].len != 0 || model.active_on(cpu).is_some() {
                return Err(node::Error::Association);
            }
        }
        for slot in 0..MAX_THREADS {
            if let Some((id, cpu)) = self.owners[slot] {
                let t = model.thread(id)?;
                let active = ActiveContext {
                    node: t.node,
                    thread: id,
                    context: t.context,
                };
                let bit = 1 << slot;
                if id.index() != slot
                    || cpu >= self.cpus
                    || self.affinities[slot] == 0
                    || self.affinities[slot] >> self.cpus != 0
                    || self.affinities[slot] & (1 << cpu) == 0
                {
                    return Err(node::Error::Association);
                }
                let saved = t
                    .saved
                    .is_some_and(|s| t.stack.is_some_and(|stack| s.valid(active, stack)));
                let valid = match t.state {
                    ThreadState::Runnable => {
                        saved
                            && t.queued
                            && running & bit == 0
                            && ((queued & bit != 0) == self.stopping_for(active).is_none())
                    }
                    ThreadState::Running => running & bit != 0 && !t.queued && t.saved.is_none(),
                    ThreadState::Blocked => {
                        saved
                            && !t.queued
                            && (queued | running) & bit == 0
                            && t.saved.is_some_and(|s| s.continuation)
                    }
                    _ => false,
                };
                if !valid {
                    return Err(node::Error::Association);
                }
            } else if self.affinities[slot] != 0
                || model.thread_at(slot).is_some_and(|t| {
                    matches!(
                        t.state,
                        ThreadState::Runnable | ThreadState::Running | ThreadState::Blocked
                    )
                })
            {
                return Err(node::Error::Association);
            }
        }
        for (slot, origin) in self.stopping.iter().enumerate() {
            if let Some(origin) = origin
                && ((origin.node.raw() as u32) as usize != slot + 1
                    || !(0..self.cpus).any(|cpu| model.active_on(cpu) == Some(*origin))
                    || self.owner(origin.thread).is_none())
            {
                return Err(node::Error::Association);
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        context::StackBinding,
        node::{Fault, FaultSource, ThreadContext},
        paging::Registry,
    };
    use std::{
        sync::{Arc, Barrier},
        thread,
    };
    fn fault_reason() -> ExitReason {
        ExitReason::UserFault(Fault {
            source: FaultSource::Cpu,
            vector: 14,
            error: 7,
            rip: 0x400020,
            rsp: 0x404ff8,
            cs: 0x3b,
            ss: 0x33,
            flags: 0x202,
            cr2: 0xffff_ffff_80000000,
        })
    }
    fn acquire<T>(lock: &crate::sync::SpinLock<T>, cpu: u64) -> crate::sync::Guard<'_, T> {
        loop {
            if let Some(guard) = lock.lock_bounded(cpu + 1, 1000) {
                return guard;
            }
            thread::yield_now();
        }
    }
    fn setup() -> (Nodes, Scheduler, [ThreadId; 3]) {
        let mut n = Nodes::new();
        let mut r = Registry::new();
        let mut ids = [None; 3];
        for node_slot in 0..2 {
            let space = r.create_space().unwrap();
            let root = (node_slot + 1) as u64 * 0x1000;
            r.bind_root(space, root).unwrap();
            let node = n.create().unwrap();
            n.configure(node).unwrap();
            n.bind_space(node, space).unwrap();
            let ctx = ThreadContext {
                space,
                root,
                entry: 0x400000,
                rsp: 0x404ff8,
            };
            let first = n.initial_thread(node, ctx).unwrap();
            n.ready(node).unwrap();
            let mut members = [Some(first), None];
            if node_slot == 0 {
                members[1] = Some(
                    n.member_thread(
                        node,
                        ThreadContext {
                            entry: 0x408000,
                            rsp: 0x40cff8,
                            ..ctx
                        },
                    )
                    .unwrap(),
                );
            }
            for id in members.into_iter().flatten() {
                let t = n.thread(id).unwrap();
                let active = ActiveContext {
                    node,
                    thread: id,
                    context: t.context,
                };
                n.install_execution(
                    id,
                    SavedContext::initial_preemptible(active, StackBinding::new(active)),
                )
                .unwrap();
                ids[id.index()] = Some(id);
            }
        }
        let ids = ids.map(Option::unwrap);
        let mut s = Scheduler::new(2).unwrap();
        s.admit(&mut n, 0, ids[0], 3).unwrap();
        s.admit(&mut n, 1, ids[1], 3).unwrap();
        s.admit(&mut n, 0, ids[2], 3).unwrap();
        (n, s, ids)
    }
    fn irq(d: &Dispatch) -> SavedContext {
        let mut frame = d.saved.frame;
        frame.context.rip += 16;
        frame.registers.rbx = 0x1122334455667788;
        SavedContext::interrupted(d.active, d.saved.stack, frame)
    }
    fn continuation(d: &Dispatch) -> SavedContext {
        let mut f = d.saved.frame;
        f.context.rip += 32;
        f.registers.rcx = f.context.rip;
        f.registers.r11 = f.context.flags;
        SavedContext::continuation(d.active, d.saved.stack, f)
    }
    #[test]
    fn same_node_threads_run_on_distinct_cpus_and_saved_contexts_round_trip() {
        let (mut n, mut s, ids) = setup();
        let a = s.dispatch(&mut n, 0).unwrap().unwrap();
        let sibling = s.dispatch(&mut n, 1).unwrap().unwrap();
        assert_eq!(a.active.node, sibling.active.node);
        assert_ne!(a.active.thread, sibling.active.thread);
        assert_eq!(n.concurrent_count(), 2);
        assert!(n.dispatch_on(1, ids[0]).is_err());
        let saved = irq(&a);
        s.reschedule(&mut n, 0, saved).unwrap();
        s.verify(&n).unwrap();
        let b = s.dispatch(&mut n, 0).unwrap().unwrap();
        assert_eq!(b.active.thread, ids[2]);
        s.reschedule(&mut n, 0, irq(&b)).unwrap();
        let again = s.dispatch(&mut n, 0).unwrap().unwrap();
        assert_eq!(again.saved, saved);
        s.verify(&n).unwrap();
    }
    #[test]
    fn blocked_thread_wakes_once_while_its_peer_keeps_running() {
        let (mut n, mut s, ids) = setup();
        let a = s.dispatch(&mut n, 0).unwrap().unwrap();
        let sibling = s.dispatch(&mut n, 1).unwrap().unwrap();
        s.block(&mut n, 1, continuation(&sibling)).unwrap();
        assert_eq!(n.active_on(0), Some(a.active));
        assert_eq!(s.wake(&mut n, ids[1]).unwrap(), 1);
        assert!(s.wake(&mut n, ids[1]).is_err());
        assert!(s.admit(&mut n, 0, ids[1], 3).is_err());
        s.verify(&n).unwrap();
    }
    #[test]
    fn migration_rejects_running_work_and_preserves_saved_thread_identity() {
        let (mut n, mut s, ids) = setup();
        let a = s.dispatch(&mut n, 0).unwrap().unwrap();
        assert!(s.migrate(&n, ids[0], 1).is_err());
        let saved = irq(&a);
        s.reschedule(&mut n, 0, saved).unwrap();
        s.migrate(&n, ids[0], 1).unwrap();
        assert_eq!(s.owner(ids[0]), Some(1));
        let sibling = s.dispatch(&mut n, 1).unwrap().unwrap();
        s.reschedule(&mut n, 1, irq(&sibling)).unwrap();
        let migrated = s.dispatch(&mut n, 1).unwrap().unwrap();
        assert_eq!(migrated.saved, saved);
        assert_eq!(migrated.active.thread, ids[0]);
        s.verify(&n).unwrap();
    }
    #[test]
    fn fault_requires_remote_sibling_quiescence_before_node_termination() {
        let (mut n, mut s, ids) = setup();
        let origin = s.dispatch(&mut n, 0).unwrap().unwrap();
        let sibling = s.dispatch(&mut n, 1).unwrap().unwrap();
        let fault = ExitReason::UserFault(Fault {
            source: FaultSource::Cpu,
            vector: 14,
            error: 7,
            rip: 0x400020,
            rsp: 0x404ff8,
            cs: 0x3b,
            ss: 0x33,
            flags: 0x202,
            cr2: 0xffff_ffff_80000000,
        });
        assert_eq!(s.begin_fault(&n, 0, origin.active).unwrap(), 2);
        assert_eq!(
            s.finish(&mut n, 0, origin.active, fault),
            Err(node::Error::InUse)
        );
        assert!(
            n.release_space(origin.active.node, origin.active.context.space)
                .is_err()
        );
        s.finish(
            &mut n,
            1,
            sibling.active,
            ExitReason::Cancelled {
                by: origin.active.thread,
            },
        )
        .unwrap();
        assert_eq!(s.begin_fault(&n, 0, origin.active).unwrap(), 0);
        s.finish(&mut n, 0, origin.active, fault).unwrap();
        assert_eq!(n.node(origin.active.node).unwrap().outcome, Some(fault));
        assert_eq!(
            n.thread(ids[1]).unwrap().outcome,
            Some(ExitReason::Cancelled { by: ids[0] })
        );
        let peer = s.dispatch(&mut n, 0).unwrap().unwrap();
        assert_eq!(peer.active.thread, ids[2]);
        assert!(s.dispatch(&mut n, 1).unwrap().is_none());
        s.verify(&n).unwrap();
    }
    #[test]
    fn shared_capability_table_can_have_only_one_cpu_lease() {
        let (mut n, mut s, _) = setup();
        let a = s.dispatch(&mut n, 0).unwrap().unwrap();
        let b = s.dispatch(&mut n, 1).unwrap().unwrap();
        let lease = n.take_authority_on(0, a.active).unwrap();
        assert!(n.take_authority_on(1, b.active).is_err());
        n.restore_authority_on(0, lease).unwrap();
        let lease = n.take_authority_on(1, b.active).unwrap();
        n.restore_authority_on(1, lease).unwrap();
    }
    #[test]
    fn four_cpu_queues_do_not_require_a_permanent_bootstrap_manager() {
        let (mut n, _, ids) = setup();
        // Rebuild with four CPUs after removing only the bootstrap queue flags.
        let mut s = Scheduler::new(4).unwrap();
        for id in ids {
            // setup already admitted these threads; dispatch/save resets admission.
            let cpu = id.index();
            let d = n.dispatch_on(cpu, id).unwrap();
            n.suspend_on(cpu, irq(&d), false).unwrap();
            s.admit(&mut n, cpu, id, 15).unwrap();
        }
        assert_eq!(s.queue_len(3), 0);
        s.migrate(&n, ids[0], 3).unwrap();
        let d = s.dispatch(&mut n, 3).unwrap().unwrap();
        assert_eq!(d.active.thread, ids[0]);
        assert_eq!(n.active_on(0), None);
        s.verify(&n).unwrap();
    }

    #[test]
    fn real_host_cpus_round_trip_owned_frames_between_lock_transactions() {
        let (n, s, _) = setup();
        let shared = Arc::new(crate::sync::SpinLock::new((n, s)));
        let started = Arc::new(Barrier::new(2));
        let workers: std::vec::Vec<_> = (0..2)
            .map(|cpu| {
                let shared = Arc::clone(&shared);
                let started = Arc::clone(&started);
                thread::spawn(move || {
                    let mut expected = [None; MAX_THREADS];
                    for round in 0..300 {
                        let dispatch = {
                            let mut guard = acquire(&shared, cpu);
                            let (nodes, scheduler) = &mut *guard;
                            let d = scheduler.dispatch(nodes, cpu as usize).unwrap().unwrap();
                            scheduler.verify(nodes).unwrap();
                            d
                        };
                        if let Some(saved) = expected[dispatch.active.thread.index()] {
                            assert_eq!(dispatch.saved, saved);
                        }
                        // Execution owns a value, not a shared registry borrow.
                        // Both first slices stay active while CPU0 verifies them.
                        if round == 0 {
                            started.wait();
                            if cpu == 0 {
                                assert_eq!(acquire(&shared, cpu).0.concurrent_count(), 2);
                            }
                            started.wait();
                        }
                        thread::yield_now();
                        let mut saved = irq(&dispatch);
                        saved.frame.context.rip = dispatch.active.context.entry + 0x80;
                        saved.frame.registers.rbx = dispatch.saved.frame.registers.rbx + 1;
                        expected[dispatch.active.thread.index()] = Some(saved);
                        let mut guard = acquire(&shared, cpu);
                        let (nodes, scheduler) = &mut *guard;
                        scheduler.reschedule(nodes, cpu as usize, saved).unwrap();
                        scheduler.verify(nodes).unwrap();
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        let guard = acquire(&shared, 2);
        guard.1.verify(&guard.0).unwrap();
        assert_eq!(guard.0.concurrent_count(), 0);
        assert_eq!((guard.1.dispatches, guard.1.preemptions), (600, 600));
    }

    #[test]
    fn competing_host_faults_elect_one_origin_and_preserve_the_other_node() {
        let (n, s, ids) = setup();
        let shared = Arc::new(crate::sync::SpinLock::new((n, s)));
        let barrier = Arc::new(Barrier::new(2));
        let workers: std::vec::Vec<_> = (0..2)
            .map(|cpu| {
                let shared = Arc::clone(&shared);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    let d = {
                        let mut guard = acquire(&shared, cpu);
                        let (nodes, scheduler) = &mut *guard;
                        scheduler.dispatch(nodes, cpu as usize).unwrap().unwrap()
                    };
                    barrier.wait();
                    let origin = {
                        let mut guard = acquire(&shared, cpu);
                        let (nodes, scheduler) = &mut *guard;
                        match scheduler.begin_fault(nodes, cpu as usize, d.active) {
                            Ok(mask) => {
                                assert_eq!(mask, 1 << (1 - cpu));
                                scheduler.verify(nodes).unwrap();
                                true
                            }
                            Err(node::Error::InUse) => false,
                            other => panic!("unexpected competing fault result: {other:?}"),
                        }
                    };
                    barrier.wait();
                    if origin {
                        loop {
                            let mut guard = acquire(&shared, cpu);
                            let (nodes, scheduler) = &mut *guard;
                            if scheduler
                                .begin_fault(nodes, cpu as usize, d.active)
                                .unwrap()
                                == 0
                            {
                                scheduler
                                    .finish(nodes, cpu as usize, d.active, fault_reason())
                                    .unwrap();
                                scheduler.verify(nodes).unwrap();
                                break;
                            }
                            drop(guard);
                            thread::yield_now();
                        }
                    } else {
                        let mut guard = acquire(&shared, cpu);
                        let (nodes, scheduler) = &mut *guard;
                        let by = scheduler.stopping_for(d.active).unwrap();
                        assert_eq!(
                            scheduler.reschedule(nodes, cpu as usize, irq(&d)),
                            Err(node::Error::Association)
                        );
                        scheduler
                            .finish(nodes, cpu as usize, d.active, ExitReason::Cancelled { by })
                            .unwrap();
                        scheduler.verify(nodes).unwrap();
                    }
                    (d.active, origin)
                })
            })
            .collect();
        let results: std::vec::Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
        assert_eq!(results.iter().filter(|(_, origin)| *origin).count(), 1);
        let mut guard = acquire(&shared, 2);
        let (nodes, scheduler) = &mut *guard;
        for (active, origin) in results {
            assert_eq!(
                nodes.thread(active.thread).unwrap().state,
                ThreadState::Exited
            );
            assert_eq!(
                matches!(
                    nodes.thread(active.thread).unwrap().outcome,
                    Some(ExitReason::UserFault(_))
                ),
                origin
            );
            assert_eq!(
                scheduler.finish(nodes, 0, active, fault_reason()),
                Err(node::Error::Association)
            );
        }
        assert!(scheduler.stopping.iter().all(Option::is_none));
        let survivor = scheduler.dispatch(nodes, 0).unwrap().unwrap();
        assert_eq!(survivor.active.thread, ids[2]);
        scheduler.verify(nodes).unwrap();
    }

    #[test]
    fn completed_fault_does_not_poison_reused_node_and_thread_generations() {
        let (mut n, mut s, ids) = setup();
        let origin = s.dispatch(&mut n, 0).unwrap().unwrap();
        let old_saved = irq(&origin);
        // Its queued sibling is cancelled by the final fault transition.
        s.finish(&mut n, 0, origin.active, fault_reason()).unwrap();
        for id in &ids[..2] {
            let binding = n.thread(*id).unwrap().stack.unwrap();
            n.release_stack(*id, binding).unwrap();
        }
        n.release_space(origin.active.node, origin.active.context.space)
            .unwrap();
        n.reclaim(origin.active.node).unwrap();
        let node = n.create().unwrap();
        n.configure(node).unwrap();
        n.bind_space(node, origin.active.context.space).unwrap();
        let thread = n.initial_thread(node, origin.active.context).unwrap();
        n.ready(node).unwrap();
        assert_ne!(node, origin.active.node);
        assert_ne!(thread, origin.active.thread);
        assert_eq!(thread.index(), origin.active.thread.index());
        let active = ActiveContext {
            node,
            thread,
            context: origin.active.context,
        };
        n.install_execution(
            thread,
            SavedContext::initial_preemptible(active, StackBinding::new(active)),
        )
        .unwrap();
        s.admit(&mut n, 1, thread, 3).unwrap();
        let fresh = s.dispatch(&mut n, 1).unwrap().unwrap();
        assert_eq!(fresh.active, active);
        assert_eq!(
            s.reschedule(&mut n, 1, old_saved),
            Err(node::Error::Association)
        );
        assert_eq!(s.wake(&mut n, ids[0]), Err(node::Error::Identity));
        assert_eq!(
            s.begin_fault(&n, 0, origin.active),
            Err(node::Error::Association)
        );
        assert_eq!(n.active_on(1), Some(active));
        s.verify(&n).unwrap();
    }

    #[test]
    fn verifier_detects_sparse_queues_bad_affinity_and_orphaned_owners() {
        let (n, mut s, ids) = setup();
        s.queues[0].entries[0] = None;
        assert_eq!(s.verify(&n), Err(node::Error::Association));
        s.queues[0].entries[0] = Some(ids[0]);
        s.affinities[0] = 2;
        assert_eq!(s.verify(&n), Err(node::Error::Association));
        s.affinities[0] = 3;
        s.owners[0] = None;
        s.affinities[0] = 0;
        assert_eq!(s.verify(&n), Err(node::Error::Association));
        s.owners[0] = Some((ids[0], 0));
        s.affinities[0] = 3;
        s.verify(&n).unwrap();
    }

    #[test]
    fn failed_fault_election_leaves_every_queue_and_stop_slot_unchanged() {
        let (mut n, mut s, _) = setup();
        let d = s.dispatch(&mut n, 0).unwrap().unwrap();
        let before = s.queued(0);
        // A stale queue identity found during the staged removal must not
        // publish an origin or discard already validated queue entries.
        let mut other = Nodes::new();
        let mut registry = Registry::new();
        let space = registry.create_space().unwrap();
        registry.bind_root(space, 0x9000).unwrap();
        let mut stale = None;
        for generation in 0..2 {
            let node = other.create().unwrap();
            other.configure(node).unwrap();
            other.bind_space(node, space).unwrap();
            let thread = other
                .initial_thread(
                    node,
                    ThreadContext {
                        space,
                        root: 0x9000,
                        entry: 0x400000,
                        rsp: 0x404ff8,
                    },
                )
                .unwrap();
            other.ready(node).unwrap();
            if generation == 1 {
                stale = Some(thread);
                break;
            }
            let active = other.start(thread).unwrap();
            other.complete(active, ExitReason::Normal(0)).unwrap();
            other.release_space(node, space).unwrap();
            other.reclaim(node).unwrap();
        }
        s.queues[1].entries[0] = stale;
        assert_eq!(s.begin_fault(&n, 0, d.active), Err(node::Error::Identity));
        assert_eq!(s.queued(0), before);
        assert!(s.stopping.iter().all(Option::is_none));
        assert_eq!(n.active_on(0), Some(d.active));
    }

    #[test]
    fn same_node_host_cpus_serialize_capability_close_against_lookup_and_use() {
        use crate::capability::{Error as CapError, ObjectType, Objects, Rights};
        let (mut nodes, scheduler, ids) = setup();
        let node = nodes.thread(ids[0]).unwrap().node;
        let mut objects = Objects::with_channels();
        let creator = objects.flag(true).unwrap();
        let handle = nodes
            .authority_mut(node)
            .unwrap()
            .install_bootstrap(&mut objects, &creator, Rights::READ, 3)
            .unwrap();
        objects.release(creator).unwrap();
        let shared = Arc::new(crate::sync::SpinLock::new((nodes, scheduler, objects)));
        let start = Arc::new(Barrier::new(2));
        let workers: std::vec::Vec<_> = (0..2)
            .map(|cpu| {
                let shared = Arc::clone(&shared);
                let start = Arc::clone(&start);
                thread::spawn(move || {
                    let dispatch = {
                        let mut guard = acquire(&shared, cpu);
                        let (nodes, scheduler, _) = &mut *guard;
                        scheduler.dispatch(nodes, cpu as usize).unwrap().unwrap()
                    };
                    start.wait();
                    let mut guard = acquire(&shared, cpu);
                    let (nodes, scheduler, objects) = &mut *guard;
                    let mut lease = nodes
                        .take_authority_on(cpu as usize, dispatch.active)
                        .unwrap();
                    if cpu == 0 {
                        match lease.table().lookup(
                            objects,
                            node,
                            handle,
                            ObjectType::Flag,
                            Rights::READ,
                        ) {
                            Ok(cap) => assert_eq!(cap.read(), Ok(1)),
                            Err(error) => assert_eq!(error, CapError::Stale),
                        }
                    } else {
                        lease.table_mut().close(objects, node, handle).unwrap();
                    }
                    nodes.restore_authority_on(cpu as usize, lease).unwrap();
                    scheduler.verify(nodes).unwrap();
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        let guard = acquire(&shared, 2);
        assert_eq!(guard.2.stats(), crate::capability::Stats::default());
        assert!(guard.0.authority(node).unwrap().is_empty());
        guard.1.verify(&guard.0).unwrap();
    }

    #[test]
    fn unauthorized_completion_and_leased_authority_reject_before_any_mutation() {
        let (mut n, mut s, ids) = setup();
        let origin = s.dispatch(&mut n, 0).unwrap().unwrap();
        let sibling = s.dispatch(&mut n, 1).unwrap().unwrap();
        let before = s.queued(0);
        assert_eq!(
            s.finish(
                &mut n,
                1,
                sibling.active,
                ExitReason::Cancelled { by: ids[2] }
            ),
            Err(node::Error::Association)
        );
        assert_eq!(
            s.finish(&mut n, 0, origin.active, ExitReason::Quiesced),
            Err(node::Error::Association)
        );
        let lease = n.take_authority_on(0, origin.active).unwrap();
        assert!(s.finish(&mut n, 0, origin.active, fault_reason()).is_err());
        n.restore_authority_on(0, lease).unwrap();
        assert!(s.stopping.iter().all(Option::is_none));
        assert_eq!(s.queued(0), before);
        assert_eq!(n.concurrent_count(), 2);
        s.begin_fault(&n, 0, origin.active).unwrap();
        assert_eq!(
            s.finish(&mut n, 0, origin.active, ExitReason::Normal(0)),
            Err(node::Error::Association)
        );
        assert_eq!(
            s.finish(
                &mut n,
                1,
                sibling.active,
                ExitReason::Cancelled { by: ids[2] }
            ),
            Err(node::Error::Association)
        );
        assert_eq!(n.active_on(0), Some(origin.active));
        assert_eq!(n.active_on(1), Some(sibling.active));
        assert_eq!(s.completions, 0);
        s.verify(&n).unwrap();
    }

    #[test]
    fn malformed_user_rsp_or_flags_cannot_publish_a_saved_context_or_block() {
        let (mut n, mut s, _) = setup();
        let d = s.dispatch(&mut n, 0).unwrap().unwrap();
        let queues = [s.queued(0), s.queued(1)];
        let owners = s.owners;
        let affinities = s.affinities;
        for interrupted in [false, true] {
            for bad_flags in [false, true] {
                let mut saved = if interrupted {
                    irq(&d)
                } else {
                    continuation(&d)
                };
                if bad_flags {
                    saved.frame.context.flags |= 0x400;
                    saved.frame.registers.r11 = saved.frame.context.flags;
                } else {
                    saved.frame.context.rsp += 1;
                }
                assert!(!saved.valid(d.active, d.saved.stack));
                assert_eq!(
                    s.reschedule(&mut n, 0, saved),
                    Err(node::Error::Association)
                );
                assert_eq!(s.block(&mut n, 0, saved), Err(node::Error::Association));
                assert_eq!([s.queued(0), s.queued(1)], queues);
                assert_eq!(s.owners, owners);
                assert_eq!(s.affinities, affinities);
                assert_eq!(n.active_on(0), Some(d.active));
                let t = n.thread(d.active.thread).unwrap();
                assert_eq!(t.state, ThreadState::Running);
                assert_eq!(t.saved, None);
                assert_eq!(t.outcome, None);
                assert_eq!(s.preemptions, 0);
                assert_eq!(s.blocks, 0);
                assert_eq!(s.completions, 0);
                s.verify(&n).unwrap();
            }
        }
    }
}

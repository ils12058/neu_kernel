//! Bounded kernel-internal bootstrap lifecycle, not capabilities or a scheduler.
//! Nodes own domains/resources; separately identified Threads refer to a Node.
use crate::context::{Dispatch, SavedContext, StackBinding};
use crate::paging::{SpaceId, canonical, physical};
pub const MAX_NODES: usize = 2;
pub const MAX_THREADS: usize = 2;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NodeId {
    slot: usize,
    generation: u32,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThreadId {
    slot: usize,
    generation: u32,
}
impl NodeId {
    pub fn raw(self) -> u64 {
        (u64::from(self.generation) << 32) | (self.slot as u64 + 1)
    }
}
impl ThreadId {
    pub fn index(self) -> usize {
        self.slot
    }
    pub fn raw(self) -> u64 {
        (u64::from(self.generation) << 32) | (self.slot as u64 + 1)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodeState {
    Created,
    Configuring,
    Ready,
    Active,
    Exited,
    Reclaimed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThreadState {
    Configuring,
    Ready,
    Runnable,
    Running,
    Exited,
    Reclaimed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultSource {
    Cpu,
    RejectedReturn,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fault {
    pub source: FaultSource,
    pub vector: u64,
    pub error: u64,
    pub rip: u64,
    pub rsp: u64,
    pub cs: u64,
    pub ss: u64,
    pub flags: u64,
    pub cr2: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitReason {
    Normal(u8),
    UserFault(Fault),
    ConstructionFailure,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThreadContext {
    pub space: SpaceId,
    pub root: u64,
    pub entry: u64,
    pub rsp: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActiveContext {
    pub node: NodeId,
    pub thread: ThreadId,
    pub context: ThreadContext,
}
#[derive(Clone, Copy, Debug)]
pub struct Node {
    pub id: NodeId,
    pub state: NodeState,
    pub space: Option<SpaceId>,
    pub outcome: Option<ExitReason>,
}
#[derive(Clone, Copy, Debug)]
pub struct Thread {
    pub id: ThreadId,
    pub node: NodeId,
    pub state: ThreadState,
    pub context: ThreadContext,
    pub outcome: Option<ExitReason>,
    pub stack: Option<StackBinding>,
    pub saved: Option<SavedContext>,
    pub queued: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Identity,
    State,
    InUse,
    Capacity,
    Association,
}
pub type Result<T> = core::result::Result<T, Error>;
pub struct Nodes {
    nodes: [Option<Node>; MAX_NODES],
    threads: [Option<Thread>; MAX_THREADS],
    node_generations: [u32; MAX_NODES],
    thread_generations: [u32; MAX_THREADS],
    active: Option<ActiveContext>,
}
impl Default for Nodes {
    fn default() -> Self {
        Self::new()
    }
}
impl Nodes {
    pub const fn new() -> Self {
        Self {
            nodes: [None; MAX_NODES],
            threads: [None; MAX_THREADS],
            node_generations: [0; MAX_NODES],
            thread_generations: [0; MAX_THREADS],
            active: None,
        }
    }
    pub fn active(&self) -> Option<ActiveContext> {
        self.active
    }
    pub fn node(&self, id: NodeId) -> Result<Node> {
        self.nodes
            .get(id.slot)
            .copied()
            .flatten()
            .filter(|n| n.id == id)
            .ok_or(Error::Identity)
    }
    pub fn thread(&self, id: ThreadId) -> Result<Thread> {
        self.threads
            .get(id.slot)
            .copied()
            .flatten()
            .filter(|t| t.id == id)
            .ok_or(Error::Identity)
    }
    pub fn create(&mut self) -> Result<NodeId> {
        let slot = (0..MAX_NODES)
            .find(|&i| {
                self.nodes[i].is_none_or(|n| n.state == NodeState::Reclaimed)
                    && self.node_generations[i] != u32::MAX
            })
            .ok_or(Error::Capacity)?;
        self.node_generations[slot] += 1;
        let id = NodeId {
            slot,
            generation: self.node_generations[slot],
        };
        self.nodes[slot] = Some(Node {
            id,
            state: NodeState::Created,
            space: None,
            outcome: None,
        });
        Ok(id)
    }
    pub fn configure(&mut self, id: NodeId) -> Result<()> {
        let mut n = self.node(id)?;
        if n.state != NodeState::Created {
            return Err(Error::State);
        }
        n.state = NodeState::Configuring;
        self.nodes[id.slot] = Some(n);
        Ok(())
    }
    pub fn bind_space(&mut self, id: NodeId, space: SpaceId) -> Result<()> {
        let mut n = self.node(id)?;
        if n.state != NodeState::Configuring || n.space.is_some() || space == SpaceId::KERNEL {
            return Err(Error::State);
        }
        if self.nodes.iter().flatten().any(|n| n.space == Some(space)) {
            return Err(Error::Association);
        }
        n.space = Some(space);
        self.nodes[id.slot] = Some(n);
        Ok(())
    }
    pub fn initial_thread(&mut self, node: NodeId, context: ThreadContext) -> Result<ThreadId> {
        let n = self.node(node)?;
        if n.state != NodeState::Configuring || n.space != Some(context.space) {
            return Err(Error::Association);
        }
        // One INITIAL Thread is a bootstrap policy, not an embedded Node field
        // or the future rule for normal Thread exit.
        if self
            .threads
            .iter()
            .flatten()
            .any(|t| t.node == node && t.state != ThreadState::Reclaimed)
        {
            return Err(Error::InUse);
        }
        if physical(context.root, 52).is_err()
            || context.root == 0
            || !canonical(context.entry)
            || !canonical(context.rsp)
            || context.entry >= 0x800000000000
            || context.rsp >= 0x800000000000
            || context.rsp & 15 != 8
        {
            return Err(Error::State);
        }
        let slot = (0..MAX_THREADS)
            .find(|&i| {
                self.threads[i].is_none_or(|t| t.state == ThreadState::Reclaimed)
                    && self.thread_generations[i] != u32::MAX
            })
            .ok_or(Error::Capacity)?;
        self.thread_generations[slot] += 1;
        let id = ThreadId {
            slot,
            generation: self.thread_generations[slot],
        };
        self.threads[slot] = Some(Thread {
            id,
            node,
            state: ThreadState::Configuring,
            context,
            outcome: None,
            stack: None,
            saved: None,
            queued: false,
        });
        Ok(id)
    }
    pub fn ready(&mut self, id: NodeId) -> Result<()> {
        let mut n = self.node(id)?;
        if n.state != NodeState::Configuring || n.space.is_none() {
            return Err(Error::State);
        }
        let mut count = 0;
        for thread in self.threads.iter_mut().flatten().filter(|t| t.node == id) {
            if thread.state != ThreadState::Configuring {
                return Err(Error::State);
            }
            thread.state = ThreadState::Ready;
            count += 1;
        }
        if count == 0 {
            return Err(Error::State);
        }
        n.state = NodeState::Ready;
        self.nodes[id.slot] = Some(n);
        Ok(())
    }
    pub fn start(&mut self, id: ThreadId) -> Result<ActiveContext> {
        let mut t = self.thread(id)?;
        let mut n = self.node(t.node)?;
        if self.active.is_some() {
            return Err(Error::InUse);
        }
        if t.state != ThreadState::Ready
            || n.state != NodeState::Ready
            || n.space != Some(t.context.space)
        {
            return Err(Error::State);
        }
        let active = ActiveContext {
            node: n.id,
            thread: t.id,
            context: t.context,
        };
        n.state = NodeState::Active;
        t.state = ThreadState::Running;
        self.nodes[n.id.slot] = Some(n);
        self.threads[id.slot] = Some(t);
        self.active = Some(active);
        Ok(active)
    }
    pub fn complete(&mut self, active: ActiveContext, outcome: ExitReason) -> Result<()> {
        if self.active != Some(active) {
            return Err(Error::Association);
        }
        let mut t = self.thread(active.thread)?;
        let mut n = self.node(active.node)?;
        if t.node != n.id || t.state != ThreadState::Running || n.state != NodeState::Active {
            return Err(Error::State);
        }
        t.state = ThreadState::Exited;
        t.outcome = Some(outcome);
        t.saved = None;
        t.queued = false;
        self.threads[t.id.slot] = Some(t);
        if matches!(outcome, ExitReason::UserFault(_)) {
            for other in self
                .threads
                .iter_mut()
                .flatten()
                .filter(|t| t.node == n.id && t.state != ThreadState::Reclaimed)
            {
                other.state = ThreadState::Exited;
                other.outcome = Some(outcome);
                other.saved = None;
                other.queued = false;
            }
        }
        let live = self.threads.iter().flatten().any(|t| {
            t.node == n.id
                && matches!(
                    t.state,
                    ThreadState::Configuring
                        | ThreadState::Ready
                        | ThreadState::Runnable
                        | ThreadState::Running
                )
        });
        // A normal Thread exit ends its Node only when no live Threads remain.
        n.state = if live {
            NodeState::Ready
        } else {
            NodeState::Exited
        };
        if !live {
            n.outcome = Some(outcome);
        }
        self.nodes[n.id.slot] = Some(n);
        self.active = None;
        Ok(())
    }
    pub fn install_execution(&mut self, id: ThreadId, saved: SavedContext) -> Result<()> {
        let mut t = self.thread(id)?;
        let n = self.node(t.node)?;
        let active = ActiveContext {
            node: t.node,
            thread: id,
            context: t.context,
        };
        if t.state != ThreadState::Ready
            || n.state != NodeState::Ready
            || t.stack.is_some()
            || t.saved.is_some()
            || t.queued
            || n.space != Some(t.context.space)
            || !saved.fresh
            || !saved.valid(active, saved.stack)
        {
            return Err(Error::Association);
        }
        t.stack = Some(saved.stack);
        t.saved = Some(saved);
        self.threads[id.slot] = Some(t);
        Ok(())
    }
    pub fn enqueue(&mut self, id: ThreadId) -> Result<()> {
        let mut t = self.thread(id)?;
        let n = self.node(t.node)?;
        let active = ActiveContext {
            node: t.node,
            thread: id,
            context: t.context,
        };
        if !matches!(t.state, ThreadState::Ready | ThreadState::Runnable)
            || t.queued
            || n.state != NodeState::Ready
            || n.space != Some(t.context.space)
            || t.stack
                .is_none_or(|stack| t.saved.is_none_or(|s| !s.valid(active, stack)))
        {
            return Err(Error::State);
        }
        t.state = ThreadState::Runnable;
        t.queued = true;
        self.threads[id.slot] = Some(t);
        Ok(())
    }
    pub fn dispatch(&mut self, id: ThreadId) -> Result<Dispatch> {
        let mut t = self.thread(id)?;
        let mut n = self.node(t.node)?;
        if self.active.is_some() {
            return Err(Error::InUse);
        }
        let active = ActiveContext {
            node: t.node,
            thread: id,
            context: t.context,
        };
        let saved = t.saved.ok_or(Error::State)?;
        if t.state != ThreadState::Runnable
            || !t.queued
            || n.state != NodeState::Ready
            || n.space != Some(t.context.space)
            || t.stack.is_none_or(|stack| !saved.valid(active, stack))
        {
            return Err(Error::Association);
        }
        t.saved = None;
        t.queued = false;
        t.state = ThreadState::Running;
        n.state = NodeState::Active;
        self.threads[id.slot] = Some(t);
        self.nodes[n.id.slot] = Some(n);
        self.active = Some(active);
        Ok(Dispatch { active, saved })
    }
    pub fn suspend(&mut self, active: ActiveContext, frame: crate::syscall::Frame) -> Result<()> {
        let stack = self.thread(active.thread)?.stack.ok_or(Error::State)?;
        self.suspend_context(active, SavedContext::yielded(active, stack, frame))
    }
    pub fn suspend_context(&mut self, active: ActiveContext, saved: SavedContext) -> Result<()> {
        if self.active != Some(active) {
            return Err(Error::Association);
        }
        let mut t = self.thread(active.thread)?;
        let mut n = self.node(active.node)?;
        let stack = t.stack.ok_or(Error::State)?;
        if t.state != ThreadState::Running
            || t.queued
            || t.saved.is_some()
            || !saved.valid(active, stack)
        {
            return Err(Error::Association);
        }
        t.state = ThreadState::Runnable;
        t.saved = Some(saved);
        n.state = NodeState::Ready;
        self.threads[t.id.slot] = Some(t);
        self.nodes[n.id.slot] = Some(n);
        self.active = None;
        Ok(())
    }
    pub fn release_stack(&mut self, id: ThreadId, stack: StackBinding) -> Result<()> {
        let mut t = self.thread(id)?;
        if t.state != ThreadState::Exited
            || t.queued
            || t.saved.is_some()
            || t.stack != Some(stack)
            || self.active.is_some_and(|a| a.thread == id)
        {
            return Err(Error::InUse);
        }
        t.stack = None;
        self.threads[id.slot] = Some(t);
        Ok(())
    }
    pub fn construction_failed(&mut self, id: NodeId) -> Result<()> {
        let mut n = self.node(id)?;
        if !matches!(n.state, NodeState::Created | NodeState::Configuring) {
            return Err(Error::State);
        }
        n.state = NodeState::Exited;
        n.outcome = Some(ExitReason::ConstructionFailure);
        for t in self.threads.iter_mut().flatten().filter(|t| t.node == id) {
            t.state = ThreadState::Exited;
            t.outcome = n.outcome;
        }
        self.nodes[id.slot] = Some(n);
        Ok(())
    }
    pub fn release_space(&mut self, id: NodeId, space: SpaceId) -> Result<()> {
        let mut n = self.node(id)?;
        if n.state != NodeState::Exited
            || self
                .threads
                .iter()
                .flatten()
                .any(|t| t.node == id && (t.stack.is_some() || t.saved.is_some() || t.queued))
        {
            return Err(Error::InUse);
        }
        if n.space != Some(space) {
            return Err(Error::Association);
        }
        n.space = None;
        self.nodes[id.slot] = Some(n);
        Ok(())
    }
    pub fn reclaim(&mut self, id: NodeId) -> Result<()> {
        let mut n = self.node(id)?;
        if n.state != NodeState::Exited
            || n.space.is_some()
            || self.active.is_some_and(|a| a.node == id)
        {
            return Err(Error::InUse);
        }
        if self.threads.iter().flatten().any(|t| {
            t.node == id
                && ((t.state != ThreadState::Exited && t.state != ThreadState::Reclaimed)
                    || t.stack.is_some()
                    || t.saved.is_some()
                    || t.queued)
        }) {
            return Err(Error::InUse);
        }
        for t in self.threads.iter_mut().flatten().filter(|t| t.node == id) {
            t.state = ThreadState::Reclaimed;
        }
        n.state = NodeState::Reclaimed;
        self.nodes[id.slot] = Some(n);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paging::Registry;
    fn ready(nodes: &mut Nodes, spaces: &mut Registry, root: u64) -> (NodeId, ThreadId, SpaceId) {
        let space = spaces.create_space().unwrap();
        spaces.bind_root(space, root).unwrap();
        let node = nodes.create().unwrap();
        nodes.configure(node).unwrap();
        nodes.bind_space(node, space).unwrap();
        let thread = nodes
            .initial_thread(
                node,
                ThreadContext {
                    space,
                    root,
                    entry: 0x400000,
                    rsp: 0x404ff8,
                },
            )
            .unwrap();
        nodes.ready(node).unwrap();
        (node, thread, space)
    }
    fn fault() -> Fault {
        Fault {
            source: FaultSource::Cpu,
            vector: 14,
            error: 4,
            rip: 0x400123,
            rsp: 0x404ff8,
            cs: 0x3b,
            ss: 0x33,
            flags: 0x10046,
            cr2: 0x406000,
        }
    }
    #[test]
    fn lifecycle_preserves_normal_result_after_resource_reclamation() {
        let mut n = Nodes::new();
        let mut s = Registry::new();
        let (node, thread, space) = ready(&mut n, &mut s, 0x1000);
        assert_eq!(n.node(node).unwrap().state, NodeState::Ready);
        let active = n.start(thread).unwrap();
        n.complete(active, ExitReason::Normal(7)).unwrap();
        assert_eq!(n.node(node).unwrap().state, NodeState::Exited);
        assert_eq!(n.reclaim(node), Err(Error::InUse));
        n.release_space(node, space).unwrap();
        n.reclaim(node).unwrap();
        assert_eq!(n.node(node).unwrap().state, NodeState::Reclaimed);
        assert_eq!(n.node(node).unwrap().outcome, Some(ExitReason::Normal(7)));
        assert_eq!(n.thread(thread).unwrap().state, ThreadState::Reclaimed);
    }
    #[test]
    fn distinct_nodes_and_threads_validate_their_own_space_association() {
        let mut n = Nodes::new();
        let mut s = Registry::new();
        let (a, ta, sa) = ready(&mut n, &mut s, 0x1000);
        let (b, tb, sb) = ready(&mut n, &mut s, 0x2000);
        assert_ne!(a, b);
        assert_ne!(ta, tb);
        assert_ne!(sa, sb);
        assert_eq!(n.thread(ta).unwrap().node, a);
        assert_eq!(n.thread(tb).unwrap().node, b);
        let active = n.start(ta).unwrap();
        assert_eq!(active.context.space, sa);
        assert_eq!(n.start(tb), Err(Error::InUse));
        let wrong = ActiveContext {
            node: b,
            thread: tb,
            context: n.thread(tb).unwrap().context,
        };
        assert_eq!(
            n.complete(wrong, ExitReason::Normal(0)),
            Err(Error::Association)
        );
        assert_eq!(n.release_space(a, sa), Err(Error::InUse));
        assert_eq!(n.reclaim(a), Err(Error::InUse));
        n.complete(active, ExitReason::Normal(0)).unwrap();
        assert_eq!(n.node(b).unwrap().state, NodeState::Ready);
    }
    #[test]
    fn user_fault_ends_only_the_associated_domain_and_records_hardware_metadata() {
        let mut n = Nodes::new();
        let mut s = Registry::new();
        let (a, ta, _) = ready(&mut n, &mut s, 0x1000);
        let (b, tb, _) = ready(&mut n, &mut s, 0x2000);
        let active = n.start(ta).unwrap();
        n.complete(active, ExitReason::UserFault(fault())).unwrap();
        assert_eq!(
            n.node(a).unwrap().outcome,
            Some(ExitReason::UserFault(fault()))
        );
        assert_eq!(n.thread(ta).unwrap().state, ThreadState::Exited);
        assert_eq!(n.start(ta), Err(Error::State));
        assert_eq!(n.node(b).unwrap().state, NodeState::Ready);
        let active = n.start(tb).unwrap();
        n.complete(active, ExitReason::Normal(0)).unwrap();
        assert_eq!(n.node(b).unwrap().outcome, Some(ExitReason::Normal(0)));
    }
    #[test]
    fn construction_failure_and_identity_reuse_do_not_affect_live_sibling() {
        let mut n = Nodes::new();
        let mut s = Registry::new();
        let (a, ta, sa) = ready(&mut n, &mut s, 0x1000);
        let (b, tb, _) = ready(&mut n, &mut s, 0x2000);
        let active = n.start(ta).unwrap();
        n.complete(active, ExitReason::Normal(0)).unwrap();
        n.release_space(a, sa).unwrap();
        n.reclaim(a).unwrap();
        let new = n.create().unwrap();
        assert_ne!(a, new);
        assert_eq!(n.node(a).err(), Some(Error::Identity));
        n.configure(new).unwrap();
        n.construction_failed(new).unwrap();
        assert_eq!(
            n.node(new).unwrap().outcome,
            Some(ExitReason::ConstructionFailure)
        );
        n.reclaim(new).unwrap();
        assert_eq!(n.node(b).unwrap().state, NodeState::Ready);
        assert_eq!(n.thread(tb).unwrap().state, ThreadState::Ready);
        assert!(n.start(ta).is_err());
    }
    #[test]
    fn invalid_initial_context_and_illegal_transitions_are_rejected() {
        let mut n = Nodes::new();
        let mut s = Registry::new();
        let space = s.create_space().unwrap();
        let wrong = s.create_space().unwrap();
        let a = n.create().unwrap();
        assert_eq!(n.ready(a), Err(Error::State));
        n.configure(a).unwrap();
        n.bind_space(a, space).unwrap();
        let good = ThreadContext {
            space,
            root: 0x1000,
            entry: 0x400000,
            rsp: 0x404ff8,
        };
        assert_eq!(
            n.initial_thread(
                a,
                ThreadContext {
                    space: wrong,
                    ..good
                }
            ),
            Err(Error::Association)
        );
        for bad in [
            ThreadContext {
                root: 0x1001,
                ..good
            },
            ThreadContext {
                entry: 0x800000000000,
                ..good
            },
            ThreadContext {
                rsp: 0x404ff0,
                ..good
            },
        ] {
            assert_eq!(n.initial_thread(a, bad), Err(Error::State));
        }
        let t = n.initial_thread(a, good).unwrap();
        assert_eq!(n.initial_thread(a, good), Err(Error::InUse));
        assert_eq!(n.start(t), Err(Error::State));
        n.ready(a).unwrap();
        assert_eq!(n.configure(a), Err(Error::State));
    }
    #[test]
    fn final_thread_rule_uses_live_associations_not_a_permanent_one_to_one_type() {
        let mut n = Nodes::new();
        let mut s = Registry::new();
        let (a, t, _) = ready(&mut n, &mut s, 0x1000);
        // Model-only future-association fixture: runtime initial_thread still
        // rejects a second initial Thread; no multiple-runnable implementation.
        let second = ThreadId {
            slot: 1,
            generation: 1,
        };
        n.thread_generations[1] = 1;
        n.threads[1] = Some(Thread {
            id: second,
            node: a,
            state: ThreadState::Ready,
            context: n.thread(t).unwrap().context,
            outcome: None,
            stack: None,
            saved: None,
            queued: false,
        });
        let active = n.start(t).unwrap();
        n.complete(active, ExitReason::Normal(0)).unwrap();
        assert_eq!(n.node(a).unwrap().state, NodeState::Ready);
        assert_eq!(n.node(a).unwrap().outcome, None);
        let active = n.start(second).unwrap();
        n.complete(active, ExitReason::Normal(0)).unwrap();
        assert_eq!(n.node(a).unwrap().state, NodeState::Exited);
    }
    #[test]
    fn fixed_node_thread_bounds_and_exhausted_generations_do_not_wrap() {
        let mut n = Nodes::new();
        let mut s = Registry::new();
        let _ = ready(&mut n, &mut s, 0x1000);
        let _ = ready(&mut n, &mut s, 0x2000);
        assert_eq!(n.create(), Err(Error::Capacity));
        let mut exhausted = Nodes::new();
        exhausted.node_generations = [u32::MAX; MAX_NODES];
        assert_eq!(exhausted.create(), Err(Error::Capacity));
        let mut exhausted = Nodes::new();
        exhausted.thread_generations = [u32::MAX; MAX_THREADS];
        let mut spaces = Registry::new();
        let space = spaces.create_space().unwrap();
        let node = exhausted.create().unwrap();
        exhausted.configure(node).unwrap();
        exhausted.bind_space(node, space).unwrap();
        assert_eq!(
            exhausted.initial_thread(
                node,
                ThreadContext {
                    space,
                    root: 0x1000,
                    entry: 0x400000,
                    rsp: 0x404ff8,
                }
            ),
            Err(Error::Capacity)
        );
        assert!(exhausted.threads.iter().all(Option::is_none));
        assert_eq!(exhausted.node(node).unwrap().state, NodeState::Configuring);
    }
}

//! Phase 9C-1 static placement and ownership model for two concurrently
//! executing Ring 3 Threads. Placement is fixed before either CPU is released;
//! a Thread occupies at most one CPU slot, completion and acknowledgement are
//! exactly-once, and reclamation is only allowed after both CPUs are terminal.
//!
//! This module contains no CPU instructions. The x86-64 controller uses the
//! same transitions before it publishes a per-CPU execution descriptor and
//! after it observes the matching Ring 3 exit.

use crate::{
    context::{SavedContext, StackBinding},
    cpu::MAX_CPUS,
    node::{ActiveContext, MAX_THREADS},
    paging::{PAGE, SpaceId},
};

pub const SLOTS: usize = MAX_CPUS;
/// Both CPUs must be placed before either may be released. A one-CPU
/// configuration cannot prove concurrency and is rejected by `ready`.
pub const ROLES: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Capacity,
    Duplicate,
    Identity,
    Stale,
    Descriptor,
    Root,
    State,
    Cpu,
    Barrier,
}

pub type Result<T> = core::result::Result<T, Error>;

/// The exact Ring 3 values a CPU receives. The same values are validated again
/// on the exit frame; the CPU receives no other execution authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Descriptor {
    pub entry: u64,
    pub rsp: u64,
    pub flags: u64,
}

impl Descriptor {
    /// IF/DF/IOPL/TF/NT/AC clear, reserved bit 1 set. User execution is
    /// non-preemptible: no timer or IPI can enter this profile.
    pub const FLAGS: u64 = 2;

    pub fn new(entry: u64, rsp: u64) -> Self {
        Self {
            entry,
            rsp,
            flags: Self::FLAGS,
        }
    }

    pub fn valid(self, context: crate::node::ThreadContext) -> bool {
        self.entry == context.entry
            && self.rsp == context.rsp
            && self.flags == Self::FLAGS
            && self.entry & (PAGE - 1) == 0
            && self.rsp & 7 == 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Phase {
    Prepared = 1,
    Released = 2,
    Entered = 3,
    Exited = 4,
    Completed = 5,
    Acknowledged = 6,
    Reaped = 7,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitKind {
    Return,
    Fault,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Exit {
    pub kind: ExitKind,
    /// Return: test exit vector. Fault: hardware vector.
    pub code: u64,
}

#[derive(Clone, Copy, Debug)]
pub struct Slot {
    pub cpu: usize,
    pub active: ActiveContext,
    pub saved: SavedContext,
    pub descriptor: Descriptor,
    /// Trusted kernel entry stack for this CPU: `[bottom, top)`.
    pub kstack: (u64, u64),
    pub phase: Phase,
    pub exit: Option<Exit>,
    pub completions: u8,
    pub acknowledgements: u8,
    pub entries: u8,
}

#[derive(Debug)]
pub struct Session {
    slots: [Option<Slot>; SLOTS],
    barrier: Option<u64>,
    barrier_word: u64,
}

impl Default for Session {
    fn default() -> Self {
        Self::new()
    }
}

impl Session {
    pub const fn new() -> Self {
        Self {
            slots: [None; SLOTS],
            barrier: None,
            barrier_word: 0,
        }
    }

    /// Fixes one Thread to one CPU. Every identity must be distinct across the
    /// session, the SavedContext must be a fresh owned frame, and `kstack` must
    /// be the CPU's own page-aligned kernel entry arena. The arena is supplied
    /// explicitly because the real CPL3-to-CPL0 handler stacks are reserved in
    /// the kernel image, not derived from the Thread's model stack binding.
    pub fn place(
        &mut self,
        cpu: usize,
        active: ActiveContext,
        saved: SavedContext,
        kstack: (u64, u64),
    ) -> Result<()> {
        if cpu >= SLOTS {
            return Err(Error::Capacity);
        }
        if self.slots[cpu].is_some() {
            return Err(Error::Duplicate);
        }
        if active.thread.index() >= MAX_THREADS || active.thread.index() >= SLOTS {
            return Err(Error::Identity);
        }
        if active.context.space == SpaceId::KERNEL || active.context.root == 0 {
            return Err(Error::Root);
        }
        let binding = StackBinding::new(active);
        if !saved.fresh
            || saved.interrupted
            || saved.continuation
            || saved.active != active
            || saved.stack != binding
            || !saved.valid(active, binding)
        {
            return Err(Error::Descriptor);
        }
        let descriptor = Descriptor::new(active.context.entry, active.context.rsp);
        if !descriptor.valid(active.context) {
            return Err(Error::Descriptor);
        }
        if !arena_valid(kstack) {
            return Err(Error::Descriptor);
        }
        for slot in self.slots.iter().flatten() {
            if slot.active.thread.index() == active.thread.index()
                || slot.active.node == active.node
                || slot.active.context.space == active.context.space
                || slot.active.context.root == active.context.root
                || arenas_overlap(slot.kstack, kstack)
            {
                return Err(Error::Duplicate);
            }
        }
        self.slots[cpu] = Some(Slot {
            cpu,
            active,
            saved,
            descriptor,
            kstack,
            phase: Phase::Prepared,
            exit: None,
            completions: 0,
            acknowledgements: 0,
            entries: 0,
        });
        Ok(())
    }

    /// The shared barrier page must be bound once, page-aligned, before either
    /// CPU is released. Both roots alias the same physical page; no page table
    /// is mutated to coordinate execution.
    pub fn bind_barrier(&mut self, physical: u64) -> Result<()> {
        if physical == 0 || physical & (PAGE - 1) != 0 || self.barrier.is_some() {
            return Err(Error::Barrier);
        }
        if self
            .slots
            .iter()
            .flatten()
            .any(|slot| slot.phase != Phase::Prepared)
        {
            return Err(Error::State);
        }
        self.barrier = Some(physical);
        Ok(())
    }

    pub fn barrier(&self) -> Option<u64> {
        self.barrier
    }

    pub fn placed(&self) -> usize {
        self.slots.iter().flatten().count()
    }

    /// True only when both CPUs are placed, their SavedContexts are still the
    /// executed ones, and the barrier is bound.
    pub fn ready(&self) -> bool {
        self.barrier.is_some()
            && self.placed() == ROLES
            && self
                .slots
                .iter()
                .flatten()
                .all(|slot| slot.phase == Phase::Prepared)
    }

    /// Releasing one CPU requires both roles to be placed already, so a
    /// one-CPU configuration can never start executing.
    pub fn release(&mut self, cpu: usize) -> Result<()> {
        if self.barrier.is_none() {
            return Err(Error::Barrier);
        }
        if self.placed() != ROLES {
            return Err(Error::State);
        }
        let slot = self.slot_mut(cpu)?;
        if slot.phase != Phase::Prepared {
            return Err(Error::State);
        }
        slot.phase = Phase::Released;
        Ok(())
    }

    pub fn enter(&mut self, cpu: usize) -> Result<()> {
        let slot = self.slot_mut(cpu)?;
        if slot.phase != Phase::Released || slot.entries != 0 {
            return Err(Error::State);
        }
        slot.entries = 1;
        slot.phase = Phase::Entered;
        Ok(())
    }

    /// A normal or faulting Ring 3 exit is terminal for this CPU only. The
    /// peer slot keeps its own phase and SavedContext.
    pub fn exit(&mut self, cpu: usize, exit: Exit) -> Result<()> {
        let slot = self.slot_mut(cpu)?;
        if slot.phase != Phase::Entered || slot.exit.is_some() {
            return Err(Error::State);
        }
        slot.exit = Some(exit);
        slot.phase = Phase::Exited;
        Ok(())
    }

    pub fn complete(&mut self, cpu: usize) -> Result<()> {
        let slot = self.slot_mut(cpu)?;
        if slot.phase != Phase::Exited || slot.completions != 0 {
            return Err(Error::State);
        }
        slot.completions = 1;
        slot.phase = Phase::Completed;
        Ok(())
    }

    /// The post-exit hold is only released after both CPUs published their
    /// completion, so a peer can never be reclaimed against a live slot.
    pub fn acknowledge(&mut self, cpu: usize) -> Result<()> {
        if self.count(|slot| slot.phase >= Phase::Completed) != ROLES {
            return Err(Error::State);
        }
        let slot = self.slot_mut(cpu)?;
        if slot.phase != Phase::Completed || slot.acknowledgements != 0 {
            return Err(Error::State);
        }
        slot.acknowledgements = 1;
        slot.phase = Phase::Acknowledged;
        Ok(())
    }

    pub fn reap(&mut self, cpu: usize) -> Result<()> {
        let slot = self.slot_mut(cpu)?;
        if slot.phase != Phase::Acknowledged {
            return Err(Error::State);
        }
        slot.phase = Phase::Reaped;
        Ok(())
    }

    pub fn reaped(&self) -> bool {
        self.placed() == ROLES && self.count(|slot| slot.phase == Phase::Reaped) == ROLES
    }

    /// Records the barrier word observed while both CPUs are at CPL3. A word
    /// that satisfies only one role proves sequential, not concurrent, entry.
    pub fn observe_barrier(&mut self, word: u64, roles: u64) -> Result<()> {
        if roles == 0 || roles >= 64 || self.barrier.is_none() {
            return Err(Error::Barrier);
        }
        if self.count(|slot| slot.phase >= Phase::Entered) != ROLES {
            return Err(Error::State);
        }
        if word & roles != roles {
            return Err(Error::Barrier);
        }
        self.barrier_word = word;
        Ok(())
    }

    pub fn barrier_word(&self) -> u64 {
        self.barrier_word
    }

    /// Proves a handler frame used the assigned CPU's trusted kernel stack.
    pub fn handler_in_stack(&self, cpu: usize, rsp: u64) -> bool {
        self.slots
            .get(cpu)
            .and_then(Option::as_ref)
            .is_some_and(|slot| (slot.kstack.0..slot.kstack.1).contains(&rsp))
    }

    pub fn slot(&self, cpu: usize) -> Option<&Slot> {
        self.slots.get(cpu).and_then(Option::as_ref)
    }

    fn slot_mut(&mut self, cpu: usize) -> Result<&mut Slot> {
        self.slots
            .get_mut(cpu)
            .and_then(Option::as_mut)
            .ok_or(Error::Cpu)
    }

    fn count(&self, predicate: impl Fn(&Slot) -> bool) -> usize {
        self.slots.iter().flatten().filter(|s| predicate(s)).count()
    }
}

/// A trusted entry arena starts on a page boundary and ends 16-byte aligned,
/// so the hardware can push a complete exception frame below its top.
fn arena_valid(arena: (u64, u64)) -> bool {
    let (bottom, top) = arena;
    bottom != 0 && bottom & (PAGE - 1) == 0 && top & 15 == 0 && bottom < top
}

fn arenas_overlap(a: (u64, u64), b: (u64, u64)) -> bool {
    a.0 < b.1 && b.0 < a.1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        node::{Error as NodeError, ExitReason, Nodes, ThreadContext, ThreadState},
        paging::{Registry, SpaceId},
    };

    /// Two separate 16 KiB entry arenas with a guard gap between them, mirroring
    /// the image-reserved arenas the controller publishes.
    const KSTACKS: [(u64, u64); 2] = [(0x7f00_0000, 0x7f00_4000), (0x7f00_8000, 0x7f00_c000)];

    /// Two isolated Nodes with one initial Thread each. The Threads occupy
    /// distinct slots, so their kernel entry stacks are distinct arenas.
    fn fixture() -> (Nodes, Registry, [ActiveContext; 2], [SavedContext; 2]) {
        let mut nodes = Nodes::new();
        let mut registry = Registry::new();
        let mut active = [None; 2];
        let mut saved = [None; 2];
        for (index, root) in [0x1000u64, 0x2000].into_iter().enumerate() {
            let space = registry.create_space().unwrap();
            registry.bind_root(space, root).unwrap();
            let node = nodes.create().unwrap();
            nodes.configure(node).unwrap();
            nodes.bind_space(node, space).unwrap();
            let context = ThreadContext {
                space,
                root,
                entry: 0x400000,
                rsp: 0x404ff8,
            };
            let thread = nodes.initial_thread(node, context).unwrap();
            nodes.ready(node).unwrap();
            let live = ActiveContext {
                node,
                thread,
                context,
            };
            active[index] = Some(live);
            saved[index] = Some(SavedContext::initial(live, StackBinding::new(live)));
        }
        (
            nodes,
            registry,
            [active[0].unwrap(), active[1].unwrap()],
            [saved[0].unwrap(), saved[1].unwrap()],
        )
    }

    fn pair() -> (
        Nodes,
        Registry,
        Session,
        [ActiveContext; 2],
        [SavedContext; 2],
    ) {
        let (nodes, registry, active, saved) = fixture();
        let mut session = Session::new();
        session.bind_barrier(0x9000).unwrap();
        session.place(0, active[0], saved[0], KSTACKS[0]).unwrap();
        session.place(1, active[1], saved[1], KSTACKS[1]).unwrap();
        (nodes, registry, session, active, saved)
    }

    #[test]
    fn placement_requires_fresh_owned_contexts_and_distinct_identities() {
        let (_nodes, _registry, active, saved) = fixture();
        let mut session = Session::new();
        session.bind_barrier(0x9000).unwrap();
        assert_eq!(
            session.place(SLOTS, active[0], saved[0], KSTACKS[0]),
            Err(Error::Capacity)
        );
        let mut forged = saved[0];
        forged.fresh = false;
        assert_eq!(
            session.place(0, active[0], forged, KSTACKS[0]),
            Err(Error::Descriptor)
        );
        let mut mismatched = saved[0];
        mismatched.active.context.entry += PAGE;
        assert_eq!(
            session.place(0, active[0], mismatched, KSTACKS[0]),
            Err(Error::Descriptor)
        );
        let mut wrong_stack = saved[0];
        wrong_stack.stack.top -= PAGE;
        assert_eq!(
            session.place(0, active[0], wrong_stack, KSTACKS[0]),
            Err(Error::Descriptor)
        );
        let mut kernel = active[0];
        kernel.context.space = SpaceId::KERNEL;
        assert_eq!(
            session.place(0, kernel, saved[0], KSTACKS[0]),
            Err(Error::Root)
        );
        let mut unmapped = active[0];
        unmapped.context.root = 0;
        assert_eq!(
            session.place(0, unmapped, saved[0], KSTACKS[0]),
            Err(Error::Root)
        );
        assert!(session.place(0, active[0], saved[0], KSTACKS[0]).is_ok());
        assert_eq!(
            session.place(0, active[0], saved[0], KSTACKS[0]),
            Err(Error::Duplicate)
        );
    }

    #[test]
    fn entry_stacks_must_be_separate_page_aligned_arenas() {
        let (_nodes, _registry, active, saved) = fixture();
        let mut session = Session::new();
        session.bind_barrier(0x9000).unwrap();
        session.place(0, active[0], saved[0], KSTACKS[0]).unwrap();
        for arena in [
            (0x7f00_1800, 0x7f00_4000), // bottom not page aligned
            (0x7f00_8000, 0x7f00_8008), // top not 16-byte aligned
            (0x7f00_8000, 0x7f00_8000), // empty
            (0x7f00_8000, 0x7f00_7fff), // reversed
        ] {
            assert_eq!(
                session.place(1, active[1], saved[1], arena),
                Err(Error::Descriptor)
            );
        }
        // Overlapping and identical arenas are a duplicate identity, not a
        // malformed one.
        for arena in [(0x7f00_2000, 0x7f00_a000), KSTACKS[0]] {
            assert_eq!(
                session.place(1, active[1], saved[1], arena),
                Err(Error::Duplicate)
            );
        }
        session.place(1, active[1], saved[1], KSTACKS[1]).unwrap();
        assert_eq!(session.placed(), ROLES);
    }

    #[test]
    fn one_thread_node_space_or_root_cannot_be_placed_twice() {
        let (mut nodes, _registry, active, saved) = fixture();
        let mut session = Session::new();
        session.bind_barrier(0x9000).unwrap();
        session.place(0, active[0], saved[0], KSTACKS[0]).unwrap();
        assert_eq!(
            session.place(1, active[0], saved[0], KSTACKS[1]),
            Err(Error::Duplicate)
        );
        session.place(1, active[1], saved[1], KSTACKS[1]).unwrap();
        // A second Thread of Node A shares its Node, space and root, so it
        // cannot be given a third CPU even though its identity is fresh.
        let member_context = ThreadContext {
            space: active[0].context.space,
            root: active[0].context.root,
            entry: 0x400000,
            rsp: 0x405ff8,
        };
        let member = nodes.member_thread(active[0].node, member_context).unwrap();
        let member_active = ActiveContext {
            node: active[0].node,
            thread: member,
            context: member_context,
        };
        let member_saved = SavedContext::initial(member_active, StackBinding::new(member_active));
        assert_ne!(member_active.thread.index(), active[0].thread.index());
        assert_eq!(
            session.place(2, member_active, member_saved, (0x7f00_c000, 0x7f00_d000)),
            Err(Error::Duplicate)
        );
        assert_eq!(session.placed(), ROLES);
    }

    #[test]
    fn release_requires_both_slots_and_a_bound_barrier() {
        let (_nodes, _registry, active, saved) = fixture();
        let mut session = Session::new();
        session.place(0, active[0], saved[0], KSTACKS[0]).unwrap();
        session.place(1, active[1], saved[1], KSTACKS[1]).unwrap();
        assert_eq!(session.release(0), Err(Error::Barrier));
        session.bind_barrier(0x9000).unwrap();
        assert!(session.ready());
        assert!(session.release(0).is_ok());
        assert!(!session.ready());
        assert_eq!(session.release(0), Err(Error::State));
        assert_eq!(session.release(1), Ok(()));
        assert_eq!(session.bind_barrier(0xa000), Err(Error::Barrier));
    }

    #[test]
    fn entry_exit_and_completion_are_one_shot_per_cpu() {
        let (_nodes, _registry, mut session, ..) = pair();
        assert_eq!(
            session.exit(
                0,
                Exit {
                    kind: ExitKind::Return,
                    code: 128
                }
            ),
            Err(Error::State)
        );
        session.release(0).unwrap();
        session.release(1).unwrap();
        session.enter(0).unwrap();
        assert_eq!(session.enter(0), Err(Error::State));
        assert_eq!(session.complete(0), Err(Error::State));
        session
            .exit(
                0,
                Exit {
                    kind: ExitKind::Return,
                    code: 128,
                },
            )
            .unwrap();
        assert_eq!(
            session.exit(
                0,
                Exit {
                    kind: ExitKind::Return,
                    code: 128
                }
            ),
            Err(Error::State)
        );
        session.complete(0).unwrap();
        assert_eq!(session.complete(0), Err(Error::State));
    }

    #[test]
    fn acknowledgement_waits_for_both_completions_and_reap_follows_it() {
        let (_nodes, _registry, mut session, ..) = pair();
        for cpu in 0..2 {
            session.release(cpu).unwrap();
            session.enter(cpu).unwrap();
            session
                .exit(
                    cpu,
                    Exit {
                        kind: ExitKind::Return,
                        code: 128,
                    },
                )
                .unwrap();
        }
        session.complete(0).unwrap();
        assert_eq!(session.acknowledge(1), Err(Error::State));
        session.complete(1).unwrap();
        assert_eq!(session.reap(0), Err(Error::State));
        session.acknowledge(0).unwrap();
        session.acknowledge(1).unwrap();
        assert_eq!(session.acknowledge(1), Err(Error::State));
        session.reap(0).unwrap();
        assert!(!session.reaped());
        session.reap(1).unwrap();
        assert!(session.reaped());
    }

    #[test]
    fn a_faulting_cpu_is_terminal_while_the_peer_keeps_executing() {
        let (_nodes, _registry, mut session, ..) = pair();
        for cpu in 0..2 {
            session.release(cpu).unwrap();
            session.enter(cpu).unwrap();
        }
        session
            .exit(
                1,
                Exit {
                    kind: ExitKind::Fault,
                    code: 14,
                },
            )
            .unwrap();
        assert_eq!(session.slot(1).unwrap().phase, Phase::Exited);
        assert_eq!(session.slot(0).unwrap().phase, Phase::Entered);
        assert_eq!(session.slot(0).unwrap().exit, None);
        session
            .exit(
                0,
                Exit {
                    kind: ExitKind::Return,
                    code: 128,
                },
            )
            .unwrap();
        assert_eq!(
            session.slot(0).unwrap().exit.unwrap().kind,
            ExitKind::Return
        );
        assert_eq!(session.slot(1).unwrap().exit.unwrap().kind, ExitKind::Fault);
    }

    #[test]
    fn barrier_and_handler_evidence_is_bound_to_the_assigned_cpu() {
        let (_nodes, _registry, mut session, active, _saved) = pair();
        assert_eq!(session.observe_barrier(3, 3), Err(Error::State));
        for cpu in 0..2 {
            session.release(cpu).unwrap();
            session.enter(cpu).unwrap();
        }
        assert_eq!(session.observe_barrier(1, 3), Err(Error::Barrier));
        assert_eq!(session.observe_barrier(3, 0), Err(Error::Barrier));
        session.observe_barrier(3, 3).unwrap();
        assert_eq!(session.barrier_word(), 3);
        let slot = *session.slot(0).unwrap();
        assert!(session.handler_in_stack(0, slot.kstack.0));
        assert!(session.handler_in_stack(0, slot.kstack.1 - 32));
        assert!(!session.handler_in_stack(0, slot.kstack.1));
        assert!(!session.handler_in_stack(1, slot.kstack.0));
        assert!(session.handler_in_stack(1, session.slot(1).unwrap().kstack.0));
        assert_eq!(slot.saved.stack, StackBinding::new(active[0]));
    }

    #[test]
    fn kernel_entry_stacks_are_private_page_aligned_arenas() {
        let (_nodes, _registry, session, ..) = pair();
        let a = session.slot(0).unwrap().kstack;
        let b = session.slot(1).unwrap().kstack;
        assert_eq!(a, KSTACKS[0]);
        assert_eq!(b, KSTACKS[1]);
        assert_eq!(a.0 & (PAGE - 1), 0);
        assert_eq!(a.1 & 15, 0);
        assert!(a.0 < a.1 && b.0 < b.1);
        assert!(a.1 <= b.0 || b.1 <= a.0);
    }

    #[test]
    fn terminal_thread_cleanup_follows_the_ownership_order() {
        let (mut nodes, registry, active, saved) = fixture();
        let mut slots = [None; 2];
        for (cpu, (live, frame)) in active.iter().copied().zip(saved).enumerate() {
            nodes.install_execution(live.thread, frame).unwrap();
            assert_eq!(nodes.active_on(cpu), None);
            slots[cpu] = Some(nodes.start_on(cpu, live.thread).unwrap());
        }
        assert_eq!(nodes.concurrent_count(), 2);
        assert_eq!(nodes.active(), None);
        for (cpu, slot) in slots.iter().copied().enumerate() {
            assert_eq!(nodes.active_on(cpu), slot);
        }
        // A Thread already placed on a CPU cannot be dispatched again, and the
        // legacy single-CPU path stays closed while CPU slots are occupied.
        assert_eq!(nodes.start_on(2, active[0].thread), Err(NodeError::InUse));
        assert_eq!(nodes.start_on(2, active[1].thread), Err(NodeError::InUse));
        assert_eq!(nodes.start_on(0, active[1].thread), Err(NodeError::InUse));
        assert_eq!(nodes.start(active[0].thread), Err(NodeError::InUse));
        for (cpu, slot) in slots.iter().copied().enumerate() {
            let live = slot.unwrap();
            let stack = StackBinding::new(live);
            // Only the hosting CPU may publish this Thread's outcome.
            assert_eq!(
                nodes.complete_on(1 - cpu, live, ExitReason::Normal(128)),
                Err(NodeError::Association)
            );
            nodes
                .complete_on(cpu, live, ExitReason::Normal(128))
                .unwrap();
            assert_eq!(nodes.active_on(cpu), None);
            assert_eq!(
                nodes.complete_on(cpu, live, ExitReason::Normal(128)),
                Err(NodeError::Association)
            );
            let node = live.node;
            let space = live.context.space;
            // The terminal Thread still owns its stack binding, so neither the
            // Node's space nor the Node itself may be released yet.
            assert_eq!(nodes.release_space(node, space), Err(NodeError::InUse));
            assert_eq!(nodes.reclaim(node), Err(NodeError::InUse));
            nodes.release_stack(live.thread, stack).unwrap();
            assert_eq!(nodes.release_space(node, space), Ok(()));
            assert!(registry.can_destroy_space(space).is_ok());
            nodes.reclaim(node).unwrap();
            assert_eq!(
                nodes.thread(live.thread).map(|t| t.state),
                Ok(ThreadState::Reclaimed)
            );
        }
        assert_eq!(nodes.concurrent_count(), 0);
    }

    #[test]
    fn stale_thread_generations_cannot_be_dispatched() {
        let (mut nodes, _registry, active, saved) = fixture();
        let stale = active[0].thread;
        nodes.install_execution(stale, saved[0]).unwrap();
        let running = nodes.start_on(0, stale).unwrap();
        nodes
            .complete_on(0, running, ExitReason::Normal(128))
            .unwrap();
        nodes
            .release_stack(stale, StackBinding::new(running))
            .unwrap();
        nodes
            .release_space(active[0].node, active[0].context.space)
            .unwrap();
        nodes.reclaim(active[0].node).unwrap();
        // Reclaiming frees the Thread slot; the next creation reuses that slot
        // with a new generation, so the stale ThreadId names nothing.
        let node = nodes.create().unwrap();
        nodes.configure(node).unwrap();
        nodes.bind_space(node, active[0].context.space).unwrap();
        let fresh_context = ThreadContext {
            space: active[0].context.space,
            root: 0x3000,
            entry: 0x400000,
            rsp: 0x404ff8,
        };
        let fresh = nodes.initial_thread(node, fresh_context).unwrap();
        assert_eq!(fresh.index(), stale.index());
        assert_ne!(fresh.raw(), stale.raw());
        nodes.ready(node).unwrap();
        assert!(matches!(nodes.thread(stale), Err(NodeError::Identity)));
        assert!(matches!(
            nodes.install_execution(stale, saved[0]),
            Err(NodeError::Identity)
        ));
        assert!(matches!(nodes.start_on(0, stale), Err(NodeError::Identity)));
        assert!(matches!(nodes.start(stale), Err(NodeError::Identity)));
        assert_eq!(nodes.thread(fresh).map(|t| t.id), Ok(fresh));
    }
}

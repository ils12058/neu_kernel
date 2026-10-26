//! Bounded cooperative FIFO policy. Selection never manipulates CPU registers,
//! roots, TSS, stacks, MSRs or timer state.
use crate::{
    context::Dispatch,
    node::{self, ActiveContext, ExitReason, MAX_THREADS, Nodes, ThreadId, ThreadState},
    syscall::Frame,
};
pub const TEST_YIELD: u64 = 0xffff_ffff_ffff_ff00;
pub struct Queue {
    entries: [Option<ThreadId>; MAX_THREADS],
    len: usize,
}
impl Default for Queue {
    fn default() -> Self {
        Self::new()
    }
}
impl Queue {
    pub const fn new() -> Self {
        Self {
            entries: [None; MAX_THREADS],
            len: 0,
        }
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn contains(&self, id: ThreadId) -> bool {
        self.entries[..self.len].contains(&Some(id))
    }
    pub fn enqueue(&mut self, model: &mut Nodes, id: ThreadId) -> node::Result<()> {
        if self.contains(id) {
            return Err(node::Error::InUse);
        }
        if self.len == MAX_THREADS {
            return Err(node::Error::Capacity);
        }
        model.enqueue(id)?;
        self.entries[self.len] = Some(id);
        self.len += 1;
        Ok(())
    }
    pub fn next(&mut self, model: &mut Nodes) -> node::Result<Option<Dispatch>> {
        if model.active().is_some() {
            return Err(node::Error::InUse);
        }
        if self.len == 0 {
            return Ok(None);
        }
        let id = self.entries[0].ok_or(node::Error::Identity)?;
        let dispatch = model.dispatch(id)?; // Validate before removing FIFO ownership.
        for index in 1..self.len {
            self.entries[index - 1] = self.entries[index];
        }
        self.len -= 1;
        self.entries[self.len] = None;
        Ok(Some(dispatch))
    }
    pub fn yielded(
        &mut self,
        model: &mut Nodes,
        active: ActiveContext,
        frame: Frame,
    ) -> node::Result<()> {
        if self.len == MAX_THREADS || self.contains(active.thread) {
            return Err(node::Error::InUse);
        }
        model.suspend(active, frame)?;
        self.enqueue(model, active.thread)
    }
    pub fn preempted(
        &mut self,
        model: &mut Nodes,
        saved: crate::context::SavedContext,
    ) -> node::Result<()> {
        if !saved.interrupted
            || !saved.preemptible
            || self.len == MAX_THREADS
            || self.contains(saved.active.thread)
        {
            return Err(node::Error::InUse);
        }
        model.suspend_context(saved.active, saved)?;
        self.enqueue(model, saved.active.thread)
    }
    pub fn finish(
        &mut self,
        model: &mut Nodes,
        active: ActiveContext,
        outcome: ExitReason,
    ) -> node::Result<()> {
        model.complete(active, outcome)?;
        let mut retained = [None; MAX_THREADS];
        let mut count = 0;
        for id in self.entries[..self.len].iter().flatten().copied() {
            let thread = model.thread(id)?;
            if !matches!(thread.state, ThreadState::Exited | ThreadState::Reclaimed) {
                retained[count] = Some(id);
                count += 1;
            }
        }
        self.entries = retained;
        self.len = count;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        context::{SavedContext, StackBinding},
        node::{Fault, FaultSource, NodeState, ThreadContext},
        paging::Registry,
        syscall::{self as abi, Registers},
    };
    fn add(model: &mut Nodes, spaces: &mut Registry, root: u64) -> ThreadId {
        let space = spaces.create_space().unwrap();
        spaces.bind_root(space, root).unwrap();
        let node = model.create().unwrap();
        model.configure(node).unwrap();
        model.bind_space(node, space).unwrap();
        let context = ThreadContext {
            space,
            root,
            entry: 0x400000,
            rsp: 0x404ff8,
        };
        let thread = model.initial_thread(node, context).unwrap();
        model.ready(node).unwrap();
        let active = ActiveContext {
            node,
            thread,
            context,
        };
        let stack = StackBinding::new(active);
        model
            .install_execution(thread, SavedContext::initial(active, stack))
            .unwrap();
        thread
    }
    fn pair() -> (Nodes, Registry, Queue, ThreadId, ThreadId) {
        let mut model = Nodes::new();
        let mut spaces = Registry::new();
        let mut queue = Queue::new();
        let a = add(&mut model, &mut spaces, 0x1000);
        let b = add(&mut model, &mut spaces, 0x2000);
        queue.enqueue(&mut model, a).unwrap();
        queue.enqueue(&mut model, b).unwrap();
        (model, spaces, queue, a, b)
    }
    fn yielded(dispatch: &Dispatch) -> Frame {
        let mut frame = dispatch.saved.frame;
        frame.context.rip = 0x400080;
        frame.context.rsp = 0x404fd8;
        frame.context.flags = 0x46;
        frame.registers.rcx = frame.context.rip;
        frame.registers.r11 = frame.context.flags;
        frame.registers.rax = 0;
        frame.registers.rdx = 0;
        frame.registers.rbx = 0x1122334455667788;
        frame.registers.r12 = 0xaabbccddeeff0001;
        frame
    }
    fn release(model: &mut Nodes, spaces: &mut Registry, id: ThreadId) {
        let t = model.thread(id).unwrap();
        model.release_stack(id, t.stack.unwrap()).unwrap();
        model.release_space(t.node, t.context.space).unwrap();
        model.reclaim(t.node).unwrap();
        spaces.clear_root(t.context.space, t.context.root).unwrap();
        spaces.retire_space(t.context.space).unwrap();
    }
    #[test]
    fn fifo_alternation_preserves_complete_saved_context_values() {
        let (mut m, _, mut q, a, b) = pair();
        let mut prior = [None, None];
        for index in 0..8 {
            let dispatch = q.next(&mut m).unwrap().unwrap();
            assert_eq!(dispatch.active.thread, if index % 2 == 0 { a } else { b });
            if let Some(frame) = prior[index % 2] {
                assert_eq!(dispatch.saved.frame, frame);
                assert!(!dispatch.saved.fresh);
            }
            let frame = yielded(&dispatch);
            prior[index % 2] = Some(frame);
            q.yielded(&mut m, dispatch.active, frame).unwrap();
            assert_eq!(q.len(), 2);
        }
    }
    #[test]
    fn one_runnable_thread_yields_to_itself_and_empty_queue_is_idle() {
        let (mut m, _, mut q, a, b) = pair();
        let first = q.next(&mut m).unwrap().unwrap();
        assert_eq!(first.active.thread, a);
        q.finish(&mut m, first.active, ExitReason::Normal(0))
            .unwrap();
        let d = q.next(&mut m).unwrap().unwrap();
        assert_eq!(d.active.thread, b);
        let frame = yielded(&d);
        q.yielded(&mut m, d.active, frame).unwrap();
        let again = q.next(&mut m).unwrap().unwrap();
        assert_eq!(again.active.thread, b);
        assert_eq!(again.saved.frame, frame);
        q.finish(&mut m, again.active, ExitReason::Normal(7))
            .unwrap();
        assert!(q.next(&mut m).unwrap().is_none());
        assert!(q.is_empty());
        assert_eq!(
            m.node(again.active.node).unwrap().outcome,
            Some(ExitReason::Normal(7))
        );
    }
    #[test]
    fn duplicate_running_and_terminal_admission_are_rejected() {
        let (mut m, _, mut q, a, b) = pair();
        assert_eq!(q.enqueue(&mut m, a), Err(node::Error::InUse));
        let d = q.next(&mut m).unwrap().unwrap();
        assert_eq!(q.enqueue(&mut m, a), Err(node::Error::State));
        assert_eq!(q.next(&mut m).err(), Some(node::Error::InUse));
        assert!(q.contains(b));
        assert_eq!(q.len(), 1);
        q.finish(&mut m, d.active, ExitReason::Normal(0)).unwrap();
        assert_eq!(q.enqueue(&mut m, a), Err(node::Error::State));
    }
    #[test]
    fn wrong_active_thread_cannot_suspend_or_finish_another_node() {
        let (mut m, _, mut q, _, b) = pair();
        let d = q.next(&mut m).unwrap().unwrap();
        let bt = m.thread(b).unwrap();
        let wrong = ActiveContext {
            node: bt.node,
            thread: b,
            context: bt.context,
        };
        assert_eq!(
            q.yielded(&mut m, wrong, yielded(&d)),
            Err(node::Error::InUse)
        );
        assert_eq!(
            q.finish(&mut m, wrong, ExitReason::Normal(0)),
            Err(node::Error::Association)
        );
        assert_eq!(
            m.thread(d.active.thread).unwrap().state,
            ThreadState::Running
        );
        assert!(q.contains(b));
    }
    #[test]
    fn fault_terminates_own_thread_and_preserves_suspended_sibling() {
        let (mut m, _, mut q, a, b) = pair();
        for _ in 0..2 {
            let d = q.next(&mut m).unwrap().unwrap();
            q.yielded(&mut m, d.active, yielded(&d)).unwrap();
        }
        let before = m.thread(b).unwrap().saved;
        let d = q.next(&mut m).unwrap().unwrap();
        assert_eq!(d.active.thread, a);
        let f = Fault {
            source: FaultSource::Cpu,
            vector: 14,
            error: 4,
            rip: 0x400123,
            rsp: 0x404fe0,
            cs: 0x3b,
            ss: 0x33,
            flags: 0x46,
            cr2: 0x406000,
        };
        q.finish(&mut m, d.active, ExitReason::UserFault(f))
            .unwrap();
        assert!(m.thread(a).unwrap().saved.is_none());
        assert_eq!(m.thread(b).unwrap().saved, before);
        assert_eq!(
            m.node(m.thread(b).unwrap().node).unwrap().state,
            NodeState::Active
        );
        let resumed = q.next(&mut m).unwrap().unwrap();
        assert_eq!(resumed.active.thread, b);
        assert_eq!(Some(resumed.saved), before);
    }
    #[test]
    fn saved_or_running_stack_references_block_model_reclamation() {
        let (mut m, mut spaces, mut q, a, _) = pair();
        let t = m.thread(a).unwrap();
        assert_eq!(
            m.release_stack(a, t.stack.unwrap()),
            Err(node::Error::InUse)
        );
        assert_eq!(
            m.release_space(t.node, t.context.space),
            Err(node::Error::InUse)
        );
        assert_eq!(m.reclaim(t.node), Err(node::Error::InUse));
        let d = q.next(&mut m).unwrap().unwrap();
        assert_eq!(
            m.release_stack(a, t.stack.unwrap()),
            Err(node::Error::InUse)
        );
        q.yielded(&mut m, d.active, yielded(&d)).unwrap();
        assert_eq!(m.reclaim(t.node), Err(node::Error::InUse));
        let b = q.next(&mut m).unwrap().unwrap();
        q.finish(&mut m, b.active, ExitReason::Normal(0)).unwrap();
        let a = q.next(&mut m).unwrap().unwrap();
        q.finish(&mut m, a.active, ExitReason::Normal(0)).unwrap();
        release(&mut m, &mut spaces, a.active.thread);
        assert_eq!(m.node(t.node).unwrap().state, NodeState::Reclaimed);
    }
    #[test]
    fn stale_thread_ids_and_corrupt_queue_entries_never_dispatch() {
        let (mut m, mut spaces, mut q, a, b) = pair();
        let d = q.next(&mut m).unwrap().unwrap();
        q.finish(&mut m, d.active, ExitReason::Normal(0)).unwrap();
        release(&mut m, &mut spaces, a);
        let replacement = add(&mut m, &mut spaces, 0x3000);
        assert_ne!(a, replacement);
        assert_eq!(q.enqueue(&mut m, a), Err(node::Error::Identity));
        assert!(q.contains(b));
        q.entries = [Some(a), Some(b), None];
        q.len = 2;
        assert_eq!(q.next(&mut m).err(), Some(node::Error::Identity));
        assert_eq!(q.entries, [Some(a), Some(b), None]);
        assert_eq!(m.thread(b).unwrap().state, ThreadState::Runnable);
    }
    #[test]
    fn invalid_initial_context_installation_is_atomic_and_retryable() {
        let mut m = Nodes::new();
        let mut spaces = Registry::new();
        let space = spaces.create_space().unwrap();
        spaces.bind_root(space, 0x1000).unwrap();
        let node = m.create().unwrap();
        m.configure(node).unwrap();
        m.bind_space(node, space).unwrap();
        let context = ThreadContext {
            space,
            root: 0x1000,
            entry: 0x400000,
            rsp: 0x404ff8,
        };
        let id = m.initial_thread(node, context).unwrap();
        m.ready(node).unwrap();
        let active = ActiveContext {
            node,
            thread: id,
            context,
        };
        let good = SavedContext::initial(active, StackBinding::new(active));
        let mut bad = good;
        bad.frame.context.rip = 0x800000000000;
        assert_eq!(m.install_execution(id, bad), Err(node::Error::Association));
        assert!(m.thread(id).unwrap().stack.is_none());
        assert!(m.thread(id).unwrap().saved.is_none());
        m.install_execution(id, good).unwrap();
        assert_eq!(m.thread(id).unwrap().saved, Some(good));
    }
    #[test]
    fn context_ownership_return_state_and_assembly_layout_are_explicit() {
        let (mut m, _, mut q, _, b) = pair();
        let d = q.next(&mut m).unwrap().unwrap();
        let frame = yielded(&d);
        let good = SavedContext::yielded(d.active, d.saved.stack, frame);
        assert!(good.valid(d.active, d.saved.stack));
        let mut cases = [good; 7];
        cases[0].frame.context.cs = 8;
        cases[1].frame.context.rsp = 0xffff800000000000;
        cases[2].frame.context.flags |= 0x200;
        cases[3].frame.registers.rcx += 1;
        cases[4].frame.registers.r11 ^= 1;
        cases[5].active.thread = b;
        cases[6].stack.top += 4096;
        for bad in cases {
            assert!(!bad.valid(d.active, d.saved.stack));
        }
        assert_eq!(core::mem::size_of::<Frame>(), 160);
        assert_eq!(core::mem::offset_of!(Frame, context), 120);
        assert_eq!(core::mem::offset_of!(Registers, r15), 0);
        assert_eq!(core::mem::offset_of!(Registers, rax), 112);
        assert_eq!(
            abi::dispatch(TEST_YIELD, [0; 6]),
            abi::Outcome::Return {
                status: abi::NOT_SUPPORTED,
                scalar: 0
            }
        );
        assert_eq!(abi::ABI_INFO, 0x100000003);
    }
}

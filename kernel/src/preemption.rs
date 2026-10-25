//! Single-owner IRQ handoff values. No hardware, allocation or asynchronous borrows.
use crate::{
    context::{SavedContext, StackBinding},
    node::ActiveContext,
    syscall::Frame,
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Publication {
    pub active: ActiveContext,
    pub stack: StackBinding,
    pub sequence: u64,
    pub reschedule: bool,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Busy,
    Identity,
    Context,
    Empty,
    Overflow,
}
#[derive(Clone, Copy)]
pub struct Mailbox {
    sequence: u64,
    armed: Option<Publication>,
    pending: Option<SavedContext>,
}
impl Default for Mailbox {
    fn default() -> Self {
        Self::new()
    }
}
impl Mailbox {
    pub const fn new() -> Self {
        Self {
            sequence: 0,
            armed: None,
            pending: None,
        }
    }
    pub fn arm(
        &mut self,
        active: ActiveContext,
        stack: StackBinding,
        reschedule: bool,
    ) -> Result<Publication, Error> {
        if self.armed.is_some() || self.pending.is_some() {
            return Err(Error::Busy);
        }
        if !stack.valid(active) {
            return Err(Error::Identity);
        }
        self.sequence = self.sequence.checked_add(1).ok_or(Error::Overflow)?;
        let publication = Publication {
            active,
            stack,
            sequence: self.sequence,
            reschedule,
        };
        self.armed = Some(publication);
        Ok(publication)
    }
    pub fn current(self) -> Option<Publication> {
        self.armed
    }
    pub fn publish(&mut self, owner: Publication, frame: Frame) -> Result<(), Error> {
        if self.armed != Some(owner) {
            return Err(Error::Identity);
        }
        if self.pending.is_some() {
            return Err(Error::Busy);
        }
        let saved = SavedContext::interrupted(owner.active, owner.stack, frame);
        if !saved.valid(owner.active, owner.stack) {
            return Err(Error::Context);
        }
        self.pending = Some(saved);
        Ok(())
    }
    pub fn take(&mut self, owner: Publication) -> Result<SavedContext, Error> {
        if self.armed != Some(owner) {
            return Err(Error::Identity);
        }
        let saved = self.pending.ok_or(Error::Empty)?;
        if !saved.valid(owner.active, owner.stack) {
            return Err(Error::Context);
        }
        self.pending = None;
        self.armed = None;
        Ok(saved)
    }
    pub fn terminal(&mut self, owner: Publication) -> Result<(), Error> {
        if self.armed != Some(owner) {
            return Err(Error::Identity);
        }
        if self.pending.is_some() {
            return Err(Error::Busy);
        }
        self.armed = None;
        Ok(())
    }
    pub fn empty(self) -> bool {
        self.armed.is_none() && self.pending.is_none()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        node::{Nodes, ThreadContext},
        paging::Registry,
        scheduler::Queue,
        syscall::Registers,
    };
    fn setup() -> (Nodes, Publication, SavedContext) {
        let mut nodes = Nodes::new();
        let mut registry = Registry::new();
        let space = registry.create_space().unwrap();
        registry.bind_root(space, 0x1000).unwrap();
        let node = nodes.create().unwrap();
        nodes.configure(node).unwrap();
        nodes.bind_space(node, space).unwrap();
        let context = ThreadContext {
            space,
            root: 0x1000,
            entry: 0x400000,
            rsp: 0x404ff8,
        };
        let thread = nodes.initial_thread(node, context).unwrap();
        nodes.ready(node).unwrap();
        let active = ActiveContext {
            node,
            thread,
            context,
        };
        let stack = StackBinding::new(active);
        let saved = SavedContext::initial_preemptible(active, stack);
        (
            nodes,
            Publication {
                active,
                stack,
                sequence: 1,
                reschedule: true,
            },
            saved,
        )
    }
    fn interrupt(mut saved: SavedContext) -> SavedContext {
        saved = SavedContext::interrupted(saved.active, saved.stack, saved.frame);
        saved.frame.context.rip += 0x77;
        saved.frame.context.flags = 0x297;
        saved.frame.context.rsp -= 24;
        saved.frame.registers = Registers {
            rax: 0x123456789abcdef0,
            rdx: 41,
            rcx: 7,
            r11: 0xaabbccddeeff0011,
            ..Registers::default()
        };
        saved
    }
    #[test]
    fn hardware_context_preserves_non_syscall_registers_and_if() {
        let (_, p, s) = setup();
        let s = interrupt(s);
        assert!(s.valid(p.active, p.stack));
        assert!(!SavedContext::yielded(p.active, p.stack, s.frame).valid(p.active, p.stack));
        assert_eq!(s.frame.registers.r11, 0xaabbccddeeff0011);
    }
    #[test]
    fn irq_flags_reject_privilege_and_unsupported_extended_modes() {
        let (_, p, s) = setup();
        let good = interrupt(s);
        for bit in [8, 10, 12, 13, 14, 17, 18, 63] {
            let mut bad = good;
            bad.frame.context.flags |= 1 << bit;
            assert!(!bad.valid(p.active, p.stack));
        }
        let mut hardware_rf = good;
        hardware_rf.frame.context.flags |= 1 << 16;
        assert!(hardware_rf.valid(p.active, p.stack));
        let mut bad = good;
        bad.frame.context.flags &= !0x200;
        assert!(!bad.valid(p.active, p.stack));
        let mut old = SavedContext::initial(p.active, p.stack);
        old.frame.context.flags |= 0x200;
        assert!(!old.valid(p.active, p.stack));
    }
    #[test]
    fn mailbox_is_consumed_once_and_duplicate_publication_is_rejected() {
        let (_, p, s) = setup();
        let s = interrupt(s);
        let mut m = Mailbox::new();
        let p = m.arm(p.active, p.stack, true).unwrap();
        assert_eq!(m.take(p), Err(Error::Empty));
        m.publish(p, s.frame).unwrap();
        assert_eq!(m.publish(p, s.frame), Err(Error::Busy));
        assert_eq!(m.terminal(p), Err(Error::Busy));
        assert_eq!(m.take(p).unwrap(), s);
        assert_eq!(m.take(p), Err(Error::Identity));
        assert!(m.empty());
    }
    #[test]
    fn stale_sequence_and_wrong_stack_cannot_publish_or_consume() {
        let (_, p, s) = setup();
        let mut m = Mailbox::new();
        let old = m.arm(p.active, p.stack, true).unwrap();
        m.terminal(old).unwrap();
        let current = m.arm(p.active, p.stack, true).unwrap();
        assert_eq!(m.publish(old, interrupt(s).frame), Err(Error::Identity));
        let mut wrong = current;
        wrong.stack.top += 4096;
        assert_eq!(m.take(wrong), Err(Error::Identity));
        m.publish(current, interrupt(s).frame).unwrap();
        assert!(m.take(current).is_ok());
    }
    #[test]
    fn invalid_context_does_not_partially_fill_handoff() {
        let (_, p, s) = setup();
        let mut m = Mailbox::new();
        let p = m.arm(p.active, p.stack, true).unwrap();
        let mut f = interrupt(s).frame;
        f.context.rip = 0x800000000000;
        assert_eq!(m.publish(p, f), Err(Error::Context));
        assert_eq!(m.take(p), Err(Error::Empty));
        m.publish(p, interrupt(s).frame).unwrap();
        assert!(m.take(p).is_ok());
    }
    #[test]
    fn timer_preemption_uses_same_fifo_without_changing_any_gpr() {
        let (mut n, p, s) = setup();
        let mut q = Queue::new();
        n.install_execution(p.active.thread, s).unwrap();
        q.enqueue(&mut n, p.active.thread).unwrap();
        let d = q.next(&mut n).unwrap().unwrap();
        let saved = interrupt(d.saved);
        q.preempted(&mut n, saved).unwrap();
        assert_eq!(q.len(), 1);
        assert!(q.preempted(&mut n, saved).is_err());
        let resumed = q.next(&mut n).unwrap().unwrap();
        assert_eq!(resumed.saved, saved);
        q.finish(&mut n, resumed.active, crate::node::ExitReason::Normal(0))
            .unwrap();
        assert!(q.next(&mut n).unwrap().is_none());
        assert!(n.active().is_none());
    }
    #[test]
    fn terminal_publication_cannot_be_reused_as_a_new_slice() {
        let (_, p, s) = setup();
        let mut m = Mailbox::new();
        let p = m.arm(p.active, p.stack, false).unwrap();
        assert_eq!(m.arm(p.active, p.stack, false), Err(Error::Busy));
        m.terminal(p).unwrap();
        assert_eq!(m.publish(p, interrupt(s).frame), Err(Error::Identity));
        assert!(m.empty());
    }
    #[test]
    fn sequence_exhaustion_refuses_wrap_and_leaves_empty_mailbox() {
        let (_, p, _) = setup();
        let mut m = Mailbox::new();
        m.sequence = u64::MAX;
        assert_eq!(m.arm(p.active, p.stack, true), Err(Error::Overflow));
        assert!(m.empty());
    }
}

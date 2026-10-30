//! Owned x86-64 user execution values. Never a pointer into a kernel stack.
use crate::{
    node::{ActiveContext, ThreadId},
    paging::{self, PAGE, SpaceId},
    syscall::{self as abi, Frame, ReturnContext},
};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StackBinding {
    pub thread: ThreadId,
    pub space: SpaceId,
    pub root: u64,
    pub bottom: u64,
    pub top: u64,
}
impl StackBinding {
    pub fn new(active: ActiveContext) -> Self {
        let bottom =
            paging::KSTACK_BASE + active.thread.index() as u64 * paging::KSTACK_STRIDE + PAGE;
        Self {
            thread: active.thread,
            space: active.context.space,
            root: active.context.root,
            bottom,
            top: bottom + paging::KSTACK_PAGES as u64 * PAGE,
        }
    }
    pub fn valid(self, active: ActiveContext) -> bool {
        self == Self::new(active)
            && active.thread.index() < crate::node::MAX_THREADS
            && self.space != SpaceId::KERNEL
            && self.root != 0
            && paging::physical(self.root, 52).is_ok()
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SavedContext {
    pub active: ActiveContext,
    pub stack: StackBinding,
    pub frame: Frame,
    pub fresh: bool,
    pub preemptible: bool,
    pub interrupted: bool,
    /// A resumed blocking SYSCALL continuation. RCX/R11 still describe the
    /// return site, but RAX/RDX are the operation result rather than zero.
    pub continuation: bool,
}
impl SavedContext {
    pub fn initial(active: ActiveContext, stack: StackBinding) -> Self {
        Self {
            active,
            stack,
            fresh: true,
            preemptible: false,
            interrupted: false,
            continuation: false,
            frame: Frame {
                registers: abi::Registers::default(),
                context: ReturnContext {
                    rip: active.context.entry,
                    cs: abi::USER_CS,
                    flags: 2,
                    rsp: active.context.rsp,
                    ss: abi::USER_SS,
                },
            },
        }
    }
    pub fn yielded(active: ActiveContext, stack: StackBinding, frame: Frame) -> Self {
        Self {
            active,
            stack,
            frame,
            fresh: false,
            preemptible: false,
            interrupted: false,
            continuation: false,
        }
    }
    pub fn continuation(active: ActiveContext, stack: StackBinding, frame: Frame) -> Self {
        let preemptible = frame.context.flags & 0x200 != 0;
        Self {
            active,
            stack,
            frame,
            fresh: false,
            preemptible,
            interrupted: false,
            continuation: true,
        }
    }
    pub fn initial_preemptible(active: ActiveContext, stack: StackBinding) -> Self {
        let mut saved = Self::initial(active, stack);
        saved.preemptible = true;
        saved.frame.context.flags |= 0x200;
        saved
    }
    pub fn interrupted(active: ActiveContext, stack: StackBinding, frame: Frame) -> Self {
        Self {
            active,
            stack,
            frame,
            fresh: false,
            preemptible: true,
            interrupted: true,
            continuation: false,
        }
    }
    pub fn valid(self, active: ActiveContext, stack: StackBinding) -> bool {
        let c = self.frame.context;
        let initial = active.context;
        let Some(code_end) = initial.entry.checked_add(PAGE) else {
            return false;
        };
        if !paging::canonical(initial.entry)
            || initial.entry >= 0x800000000000
            || !paging::canonical(initial.rsp)
            || initial.rsp >= 0x800000000000
        {
            return false;
        }
        if self.active != active
            || self.stack != stack
            || !stack.valid(active)
            || !paging::canonical(c.rip)
            || !paging::canonical(c.rsp)
            || c.rip < initial.entry
            || c.rip >= code_end
            || c.rsp < (initial.rsp & !(PAGE - 1))
            || c.rsp > initial.rsp
            || c.rsp & 7 != 0
            || c.cs != abi::USER_CS
            || c.ss != abi::USER_SS
            || (self.fresh && self.interrupted)
            || (self.interrupted && !self.preemptible)
            || (self.fresh && self.continuation)
            || (self.interrupted && self.continuation)
            || (c.flags & 0x200 != 0) != self.preemptible
            || c.flags
                & !(abi::USER_FLAGS
                    | if self.preemptible { 0x200 } else { 0 }
                    | if self.interrupted { 1 << 16 } else { 0 })
                != 0
            || c.flags & 2 == 0
        {
            return false;
        }
        if self.fresh {
            c.rip == initial.entry
                && c.rsp == initial.rsp
                && self.frame.registers == abi::Registers::default()
        } else if self.interrupted {
            true // Hardware IRQs do not clobber or synthesize any GPR.
        } else {
            self.frame.registers.rcx == c.rip
                && self.frame.registers.r11 == c.flags
                && (self.continuation
                    || (self.frame.registers.rax == 0 && self.frame.registers.rdx == 0))
        }
    }
}
#[derive(Debug)]
pub struct Dispatch {
    pub active: ActiveContext,
    pub saved: SavedContext,
}
#[derive(Clone, Copy, Debug)]
pub enum SliceEvent {
    Yield(Frame),
    Preempted(Frame),
    Blocked(Frame),
    /// A validated mapping request is pending; the controller owns the saving
    /// Thread and its destination root until it resumes or cancels it.
    #[cfg(feature = "memory-object-runtime-test")]
    Requested(Frame),
    Exited(crate::node::ExitReason),
}
const _: () = assert!(core::mem::size_of::<Frame>() == 160);
const _: () = assert!(core::mem::offset_of!(Frame, context) == 120);
const _: () = assert!(core::mem::offset_of!(abi::Registers, rax) == 112);
const _: () = assert!(core::mem::offset_of!(ReturnContext, rsp) == 24);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        node::{Nodes, ThreadContext},
        paging::Registry,
    };
    fn live() -> (ActiveContext, StackBinding) {
        let mut n = Nodes::new();
        let mut r = Registry::new();
        let space = r.create_space().unwrap();
        r.bind_root(space, 0x1000).unwrap();
        let node = n.create().unwrap();
        n.configure(node).unwrap();
        n.bind_space(node, space).unwrap();
        let context = ThreadContext {
            space,
            root: 0x1000,
            entry: 0x400000,
            rsp: 0x404ff8,
        };
        let thread = n.initial_thread(node, context).unwrap();
        n.ready(node).unwrap();
        let active = ActiveContext {
            node,
            thread,
            context,
        };
        (active, StackBinding::new(active))
    }
    #[test]
    fn saved_continuation_accepts_full_syscall_results_but_rejects_forged_frames() {
        let (active, stack) = live();
        let mut frame = SavedContext::initial(active, stack).frame;
        frame.context.rip = 0x400080;
        frame.context.rsp = 0x404fd8;
        frame.context.flags = 0x46;
        frame.registers.rcx = frame.context.rip;
        frame.registers.r11 = frame.context.flags;
        frame.registers.rax = 0x1122334455667788;
        frame.registers.rdx = 0x99;
        let valid = SavedContext::continuation(active, stack, frame);
        assert!(valid.valid(active, stack));
        assert!(valid.continuation);
        assert!(!valid.fresh);
        let mut wrong_rcx = valid;
        wrong_rcx.frame.registers.rcx += 1;
        assert!(!wrong_rcx.valid(active, stack));
        let mut wrong_r11 = valid;
        wrong_r11.frame.registers.r11 ^= 0x40;
        assert!(!wrong_r11.valid(active, stack));
        let mut preempt_mismatch = valid;
        preempt_mismatch.preemptible = true;
        assert!(!preempt_mismatch.valid(active, stack));
        let mut interrupted = valid;
        interrupted.interrupted = true;
        interrupted.preemptible = true;
        assert!(!interrupted.valid(active, stack));
        let mut fabricated_fresh = valid;
        fabricated_fresh.fresh = true;
        assert!(!fabricated_fresh.valid(active, stack));
        let mut yielded = SavedContext::yielded(active, stack, frame);
        yielded.frame.registers.rax = 1;
        assert!(!yielded.valid(active, stack));
        yielded.frame.registers.rax = 0;
        yielded.frame.registers.rdx = 1;
        assert!(!yielded.valid(active, stack));
        yielded.frame.registers.rdx = 0;
        assert!(yielded.valid(active, stack));
    }
}

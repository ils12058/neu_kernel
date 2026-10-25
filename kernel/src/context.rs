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
}
impl SavedContext {
    pub fn initial(active: ActiveContext, stack: StackBinding) -> Self {
        Self {
            active,
            stack,
            fresh: true,
            preemptible: false,
            interrupted: false,
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
                && self.frame.registers.rax == 0
                && self.frame.registers.rdx == 0
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
    Exited(crate::node::ExitReason),
}
const _: () = assert!(core::mem::size_of::<Frame>() == 160);
const _: () = assert!(core::mem::offset_of!(Frame, context) == 120);
const _: () = assert!(core::mem::offset_of!(abi::Registers, rax) == 112);
const _: () = assert!(core::mem::offset_of!(ReturnContext, rsp) == 24);

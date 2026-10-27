//! Trusted cooperative bootstrap controller; every slice finishes its kernel
//! boundary before FIFO selection, and all execution/switching retains IF=0.
use super::{native, serial};
#[cfg(not(feature = "multi-test"))]
use super::{
    paging::{Paging, UserSpace},
    userspace,
};
use core::arch::global_asm;
#[cfg(not(feature = "preempt-test"))]
use core::{arch::asm, ptr};
#[cfg(not(feature = "preempt-test"))]
use neu_kernel::{
    context::SliceEvent,
    node::{Fault, FaultSource},
    syscall::ReturnContract,
};
#[cfg(not(feature = "multi-test"))]
use neu_kernel::{
    context::{SavedContext, StackBinding},
    node::{self, ActiveContext, ExitReason, NodeId, Nodes, ThreadContext},
    scheduler::Queue,
};
use neu_kernel::{
    node::ThreadId,
    paging::{self as logic, Error, PAGE, Result},
    scheduler::TEST_YIELD,
    syscall::{self as abi, Frame},
};
const CANARIES: [u64; 6] = [
    0x1122334455667788,
    0x2233445566778899,
    0x33445566778899aa,
    0x445566778899aabb,
    0x5566778899aabbcc,
    0x66778899aabbccdd,
];
const MARKERS: [u64; 3] = [0x11225566778899aa, 0xaabbccddeeff1234, 0x1234567887654321];
global_asm!(include_str!("cooperative.S"),preemptive=const cfg!(feature="preempt-test") as u8,probe_wait=const if cfg!(feature="multi-test") {0x01000000u64}else{0x04000000u64},query=const abi::ABI_QUERY,exit=const abi::THREAD_EXIT,
    info=const abi::ABI_INFO,yield_op=const TEST_YIELD,private_value=const native::PRIVATE_VALUE,
    stack_bottom=const logic::KSTACK_BASE+PAGE,stack_guard=const logic::KSTACK_BASE,
    canary_bx=const CANARIES[0],canary_bp=const CANARIES[1],canary_12=const CANARIES[2],
    canary_13=const CANARIES[3],canary_14=const CANARIES[4],canary_15=const CANARIES[5],
    marker_0=const MARKERS[0],marker_1=const MARKERS[1],marker_2=const MARKERS[2]);
#[cfg(not(feature = "preempt-test"))]
unsafe extern "C" {
    static neu_switch_payload: u8;
    static neu_switch_payload_end: u8;
    static neu_switch_resume: u8;
    static neu_switch_ud_site: u8;
    static neu_switch_cross_site: u8;
    static neu_switch_stack_user_site: u8;
    static neu_switch_stack_guard_site: u8;
}
#[cfg(not(feature = "preempt-test"))]
#[cfg(not(feature = "multi-test"))]
fn site(symbol: *const u8) -> u64 {
    userspace::CODE + symbol as u64 - ptr::addr_of!(neu_switch_payload) as u64
}
#[cfg(not(feature = "preempt-test"))]
#[cfg(not(feature = "multi-test"))]
fn payload() -> &'static [u8] {
    let length =
        ptr::addr_of!(neu_switch_payload_end) as usize - ptr::addr_of!(neu_switch_payload) as usize;
    assert!(length > 0 && length <= PAGE as usize);
    // SAFETY: Bounded immutable image bytes, copied into new private user RX frames.
    unsafe { core::slice::from_raw_parts(ptr::addr_of!(neu_switch_payload), length) }
}
#[cfg(not(feature = "preempt-test"))]
#[cfg(not(feature = "multi-test"))]
pub fn return_contract(root: u64) -> ReturnContract {
    ReturnContract {
        root,
        code_start: userspace::CODE,
        code_end: site(ptr::addr_of!(neu_switch_payload_end)),
        stack_start: userspace::STACK,
        stack_end: userspace::STACK + PAGE,
        resumes: [site(ptr::addr_of!(neu_switch_resume)); 2],
    }
}
#[cfg(not(feature = "multi-test"))]
fn execution_payload() -> &'static [u8] {
    #[cfg(feature = "preempt-test")]
    {
        super::preemptive::payload()
    }
    #[cfg(not(feature = "preempt-test"))]
    {
        payload()
    }
}
#[cfg(not(feature = "multi-test"))]
fn initial_context(active: ActiveContext, stack: StackBinding) -> SavedContext {
    #[cfg(feature = "preempt-test")]
    {
        SavedContext::initial_preemptible(active, stack)
    }
    #[cfg(not(feature = "preempt-test"))]
    {
        SavedContext::initial(active, stack)
    }
}
#[cfg(not(feature = "multi-test"))]
fn policy<T>(result: node::Result<T>) -> Result<T> {
    result.map_err(|_| Error::Corrupt)
}
#[cfg(not(feature = "multi-test"))]
pub(super) struct Domain {
    pub label: u64,
    pub node: NodeId,
    pub thread: ThreadId,
    pub active: ActiveContext,
    pub space: Option<UserSpace>,
    pub stack: Option<StackBinding>,
    pub last: Option<Frame>,
    #[cfg(not(feature = "preempt-test"))]
    pub slices: u64,
    pub outcome: Option<ExitReason>,
    pub seed: u64,
}
#[cfg(not(feature = "multi-test"))]
pub(super) fn space(domain: &Domain) -> Result<&UserSpace> {
    domain.space.as_ref().ok_or(Error::NotOwned)
}
#[cfg(not(feature = "multi-test"))]
pub(super) fn build(
    paging: &mut Paging<'_, '_>,
    model: &mut Nodes,
    label: u64,
    action: u64,
) -> Result<Domain> {
    let node = policy(model.create())?;
    policy(model.configure(node))?;
    let seed = if label == 1 { 35 } else { 77 };
    let root = match paging.prepare_domain(
        execution_payload(),
        action,
        seed,
        if label == 2 {
            Some(native::PRIVATE_VALUE)
        } else {
            None
        },
        None,
    ) {
        Ok(space) => space,
        Err(error) => {
            policy(model.construction_failed(node))?;
            policy(model.reclaim(node))?;
            return Err(error);
        }
    };
    policy(model.bind_space(node, root.id()))?;
    let context = ThreadContext {
        space: root.id(),
        root: root.root,
        entry: userspace::CODE,
        rsp: userspace::USER_RSP,
    };
    let thread = policy(model.initial_thread(node, context))?;
    policy(model.ready(node))?;
    Ok(Domain {
        label,
        node,
        thread,
        active: ActiveContext {
            node,
            thread,
            context,
        },
        space: Some(root),
        stack: None,
        last: None,
        #[cfg(not(feature = "preempt-test"))]
        slices: 0,
        outcome: None,
        seed,
    })
}
#[cfg(not(feature = "multi-test"))]
pub(super) fn install(
    paging: &mut Paging<'_, '_>,
    model: &mut Nodes,
    queue: &mut Queue,
    domain: &mut Domain,
) -> Result<()> {
    let active = domain.active;
    let stack =
        paging.prepare_thread_stack(domain.space.as_mut().ok_or(Error::NotOwned)?, active, None)?;
    policy(model.install_execution(domain.thread, initial_context(active, stack)))?;
    paging.pin_context(space(domain)?)?;
    policy(queue.enqueue(model, domain.thread))?;
    domain.stack = Some(stack);
    serial::write(format_args!("[NEU] Switch Thread ready: label={} node={} thread={} space={} root={:#x} bottom={:#x} top={:#x} pa0={:#x} pa1={:#x} code_pa={:#x} data_pa={:#x} user_stack_pa={:#x} seed={}\n",domain.label,domain.node.raw(),domain.thread.raw(),active.context.space.raw(),active.context.root,stack.bottom,stack.top,paging.user_physical(space(domain)?,stack.bottom)?,paging.user_physical(space(domain)?,stack.bottom+PAGE)?,paging.user_physical(space(domain)?,userspace::CODE)?,paging.user_physical(space(domain)?,userspace::DATA)?,paging.user_physical(space(domain)?,userspace::STACK)?,domain.seed)).map_err(|_|Error::Corrupt)
}
pub(super) fn frame_log(event: &str, thread: ThreadId, frame: Frame) -> Result<()> {
    let r = frame.registers;
    serial::write(format_args!(
        "[NEU] Switch frame: event={} thread={} RIP={:#x} RSP={:#x} flags={:#x} regs=",
        event,
        thread.raw(),
        frame.context.rip,
        frame.context.rsp,
        frame.context.flags
    ))
    .map_err(|_| Error::Corrupt)?;
    for (index, value) in [
        r.r15, r.r14, r.r13, r.r12, r.r11, r.r10, r.r9, r.r8, r.rdi, r.rsi, r.rbp, r.rdx, r.rcx,
        r.rbx, r.rax,
    ]
    .into_iter()
    .enumerate()
    {
        serial::write(format_args!(
            "{}{:#x}",
            if index == 0 { "" } else { "," },
            value
        ))
        .map_err(|_| Error::Corrupt)?;
    }
    serial::write(format_args!("\n")).map_err(|_| Error::Corrupt)
}
#[cfg(not(feature = "multi-test"))]
pub(super) fn survivor(
    paging: &mut Paging<'_, '_>,
    model: &Nodes,
    queue: &Queue,
    domain: &Domain,
) -> Result<()> {
    let t = policy(model.thread(domain.thread))?;
    let s = space(domain)?;
    if !queue.contains(domain.thread)
        || t.saved.is_none()
        || !t.queued
        || t.stack != domain.stack
        || paging.context_reclaimable(s) != Err(Error::InUse)
    {
        return Err(Error::Corrupt);
    }
    paging.verify_thread_stack(s, domain.stack.ok_or(Error::Corrupt)?)?;
    if domain.label == 2 && paging.private_value(s)? != Some(native::PRIVATE_VALUE) {
        return Err(Error::Corrupt);
    }
    serial::write(format_args!(
        "[NEU] Switch survivor retained: label={} thread={} root={:#x} saved=true pinned=true\n",
        domain.label,
        domain.thread.raw(),
        s.root
    ))
    .map_err(|_| Error::Corrupt)
}
#[cfg(not(feature = "multi-test"))]
pub(super) fn reclaim(
    paging: &mut Paging<'_, '_>,
    model: &mut Nodes,
    domain: &mut Domain,
) -> Result<()> {
    let stack = domain.stack.take().ok_or(Error::NotOwned)?;
    if policy(model.thread(domain.thread))?.saved.is_some()
        || policy(model.thread(domain.thread))?.queued
    {
        return Err(Error::InUse);
    }
    paging.unpin_context(space(domain)?)?;
    paging.destroy_thread_stack(domain.space.as_mut().ok_or(Error::NotOwned)?, domain.thread)?;
    policy(model.release_stack(domain.thread, stack))?;
    let s = domain.space.take().ok_or(Error::NotOwned)?;
    let id = s.id();
    paging.destroy_user(s)?;
    policy(model.release_space(domain.node, id))?;
    policy(model.reclaim(domain.node))?;
    if policy(model.node(domain.node))?.outcome != domain.outcome {
        return Err(Error::Corrupt);
    }
    serial::write(format_args!("[NEU] Switch reclaimed: label={} thread={} space={} saved=false queued=false outcome=retained\n",domain.label,domain.thread.raw(),id.raw())).map_err(|_|Error::Corrupt)
}
#[cfg(not(feature = "multi-test"))]
pub(super) fn stale_audit(
    paging: &mut Paging<'_, '_>,
    model: &mut Nodes,
    queue: &mut Queue,
    old: &Domain,
) -> Result<()> {
    let node = policy(model.create())?;
    policy(model.configure(node))?;
    let s = paging.prepare_domain(execution_payload(), 0, 35, None, None)?;
    policy(model.bind_space(node, s.id()))?;
    let thread = policy(model.initial_thread(
        node,
        ThreadContext {
            space: s.id(),
            root: s.root,
            entry: userspace::CODE,
            rsp: userspace::USER_RSP,
        },
    ))?;
    if thread == old.thread || queue.enqueue(model, old.thread) != Err(node::Error::Identity) {
        return Err(Error::Corrupt);
    }
    policy(model.construction_failed(node))?;
    let id = s.id();
    paging.destroy_user(s)?;
    policy(model.release_space(node, id))?;
    policy(model.reclaim(node))?;
    serial::write(format_args!(
        "[NEU] Switch stale identity rejected: old={} replacement={}\n",
        old.thread.raw(),
        thread.raw()
    ))
    .map_err(|_| Error::Corrupt)
}
#[cfg(not(feature = "preempt-test"))]
#[cfg(not(feature = "multi-test"))]
fn verify_outcome(domain: &Domain, case: u64, reason: ExitReason, words: [u64; 3]) -> Result<()> {
    let faulted = (domain.label == 1 && matches!(case, 1 | 3 | 4 | 5 | 6))
        || (domain.label == 2 && case == 2);
    let expected_steps = if faulted {
        if case == 6 { 1 } else { 2 }
    } else {
        4
    };
    if words
        != [
            domain.seed + 7 * expected_steps,
            expected_steps,
            MARKERS[0] ^ domain.seed,
        ]
    {
        return Err(Error::Corrupt);
    }
    if !faulted {
        if reason != ExitReason::Normal(0) {
            return Err(Error::Corrupt);
        }
    } else if let ExitReason::UserFault(f) = reason {
        if case == 6 {
            if f.source != FaultSource::RejectedReturn
                || f.error != abi::Rejection::Rip as u64
                || f.rip != 0x800000000000
            {
                return Err(Error::Corrupt);
            }
        } else {
            let (vector, error, cr2, rip) = match case {
                1 | 2 => (6, 0, 0, site(ptr::addr_of!(neu_switch_ud_site))),
                3 => (
                    14,
                    4,
                    userspace::UNMAPPED,
                    site(ptr::addr_of!(neu_switch_cross_site)),
                ),
                4 => (
                    14,
                    5,
                    logic::KSTACK_BASE + PAGE,
                    site(ptr::addr_of!(neu_switch_stack_user_site)),
                ),
                _ => (
                    14,
                    4,
                    logic::KSTACK_BASE,
                    site(ptr::addr_of!(neu_switch_stack_guard_site)),
                ),
            };
            if f.source != FaultSource::Cpu
                || f.vector != vector
                || f.error != error
                || f.rip != rip
                || (vector == 14 && f.cr2 != cr2)
                || f.cs != abi::USER_CS
                || f.ss != abi::USER_SS
                || f.rsp != userspace::USER_RSP - 24
            {
                return Err(Error::Corrupt);
            }
        }
    } else {
        return Err(Error::Corrupt);
    }
    Ok(())
}
#[cfg(not(feature = "preempt-test"))]
#[cfg(not(feature = "multi-test"))]
pub fn run(paging: &mut Paging<'_, '_>) -> Result<()> {
    let flags: u64;
    unsafe {
        asm!("pushfq","pop {}",out(reg) flags,options(preserves_flags));
    }
    if flags & 0x600 != 0 {
        return Err(Error::Flags);
    }
    let case = if cfg!(feature = "test-switch-a-fault") {
        1
    } else if cfg!(feature = "test-switch-b-fault") {
        2
    } else if cfg!(feature = "test-switch-cross") {
        3
    } else if cfg!(feature = "test-switch-stack-user") {
        4
    } else if cfg!(feature = "test-switch-stack-guard") {
        5
    } else if cfg!(feature = "test-switch-invalid-context") {
        6
    } else if cfg!(feature = "test-switch-rollback") {
        7
    } else if cfg!(feature = "test-switch-stale") {
        8
    } else {
        0
    };
    let before = paging.stats();
    let tables = paging.table_count();
    serial::write(format_args!(
        "[NEU] Switch accounting baseline: allocated={} free={} tables={}\n",
        before.allocated, before.free, tables
    ))
    .map_err(|_| Error::Corrupt)?;
    let mut model = Nodes::new();
    let mut queue = Queue::new();
    let a_action = match case {
        1 => 1,
        3 => 2,
        4 => 3,
        5 => 4,
        _ => 0,
    };
    let mut a = build(paging, &mut model, 1, a_action)?;
    install(paging, &mut model, &mut queue, &mut a)?;
    let mut b = build(paging, &mut model, 2, if case == 2 { 1 } else { 0 })?;
    if case == 7 {
        let saved = policy(model.thread(a.thread))?.saved;
        let mut failures = 0;
        for budget in 0..5 {
            let stats = paging.stats();
            let count = paging.table_count();
            if paging.prepare_thread_stack(
                b.space.as_mut().ok_or(Error::NotOwned)?,
                b.active,
                Some(budget),
            ) != Err(Error::Exhausted)
                || paging.stats() != stats
                || paging.table_count() != count
                || policy(model.thread(a.thread))?.saved != saved
            {
                return Err(Error::Corrupt);
            }
            failures += 1;
            survivor(paging, &model, &queue, &a)?;
        }
        let stack = paging.prepare_thread_stack(
            b.space.as_mut().ok_or(Error::NotOwned)?,
            b.active,
            None,
        )?;
        let mut invalid = SavedContext::initial(b.active, stack);
        invalid.frame.context.rip = 0x800000000000;
        if model.install_execution(b.thread, invalid) != Err(node::Error::Association) {
            return Err(Error::Corrupt);
        }
        paging.destroy_thread_stack(b.space.as_mut().ok_or(Error::NotOwned)?, b.thread)?;
        survivor(paging, &model, &queue, &a)?;
        serial::write(format_args!("[NEU] Switch construction rollback: stack_failpoints={} context_rejected=true sibling_saved=unchanged\n",failures)).map_err(|_|Error::Corrupt)?;
    }
    install(paging, &mut model, &mut queue, &mut b)?;
    let stats = paging.stats();
    if queue.len() != 2
        || paging.context_reclaimable(space(&a)?) != Err(Error::InUse)
        || paging.destroy_thread_stack(a.space.as_mut().ok_or(Error::NotOwned)?, a.thread)
            != Err(Error::InUse)
        || model.release_stack(a.thread, a.stack.ok_or(Error::NotOwned)?) != Err(node::Error::InUse)
        || model.reclaim(a.node) != Err(node::Error::InUse)
        || paging.stats() != stats
    {
        return Err(Error::Corrupt);
    }
    serial::write(format_args!(
        "[NEU] Switch two runnable: A={} B={} queue=2 pins=verified\n",
        a.thread.raw(),
        b.thread.raw()
    ))
    .map_err(|_| Error::Corrupt)?;
    let mut dispatches = 0;
    while let Some(mut dispatch) = policy(queue.next(&mut model))? {
        dispatches += 1;
        if dispatches > 16 {
            return Err(Error::Corrupt);
        }
        let is_a = dispatch.active.thread == a.thread;
        let domain = if is_a { &mut a } else { &mut b };
        let active = dispatch.active;
        if active != domain.active
            || dispatch.saved.stack != domain.stack.ok_or(Error::NotOwned)?
            || (!dispatch.saved.fresh && domain.last != Some(dispatch.saved.frame))
        {
            return Err(Error::Corrupt);
        }
        let phase = domain.slices;
        domain.slices += 1;
        serial::write(format_args!("[NEU] Switch selected: sequence={} label={} node={} thread={} space={} root={:#x} phase={} fresh={}\n",dispatches,domain.label,domain.node.raw(),domain.thread.raw(),active.context.space.raw(),active.context.root,phase,dispatch.saved.fresh)).map_err(|_|Error::Corrupt)?;
        frame_log("taken", domain.thread, dispatch.saved.frame)?;
        let rejected = case == 6 && domain.label == 1 && phase == 1;
        if rejected {
            dispatch.saved.frame.context.rip = 0x800000000000;
        }
        let rejected_frame = dispatch.saved.frame;
        let event = match native::execute_slice(paging, space(domain)?, dispatch, 32 + case) {
            Ok(event) if !rejected => event,
            Err(Error::Flags) if rejected => {
                serial::write(format_args!("[NEU] Switch rejected before resume: thread={} RIP=0x800000000000 reason=3 root={:#x}\n",domain.thread.raw(),paging.root())).map_err(|_|Error::Corrupt)?;
                SliceEvent::Exited(ExitReason::UserFault(Fault {
                    source: FaultSource::RejectedReturn,
                    vector: 0,
                    error: 3,
                    rip: 0x800000000000,
                    rsp: rejected_frame.context.rsp,
                    cs: rejected_frame.context.cs,
                    ss: rejected_frame.context.ss,
                    flags: rejected_frame.context.flags,
                    cr2: 0,
                }))
            }
            _ => return Err(Error::Corrupt),
        };
        match event {
            SliceEvent::Preempted(_) | SliceEvent::Blocked(_) => return Err(Error::Corrupt),
            SliceEvent::Yield(frame) => {
                frame_log("saved", domain.thread, frame)?;
                domain.last = Some(frame);
                policy(queue.yielded(&mut model, active, frame))?;
                let words = paging.user_words(space(domain)?)?;
                if words
                    != [
                        domain.seed + 7 * (phase + 1),
                        phase + 1,
                        MARKERS[0] ^ domain.seed,
                    ]
                {
                    return Err(Error::Corrupt);
                }
                serial::write(format_args!("[NEU] Switch yielded: label={} thread={} phase={} result={} steps={} queue={}\n",domain.label,domain.thread.raw(),phase,words[0],words[1],queue.len())).map_err(|_|Error::Corrupt)?;
            }
            SliceEvent::Exited(reason) => {
                policy(queue.finish(&mut model, active, reason))?;
                domain.outcome = Some(reason);
                domain.last = None;
                let words = paging.user_words(space(domain)?)?;
                verify_outcome(domain, case, reason, words)?;
                if queue.enqueue(&mut model, domain.thread).is_ok() {
                    return Err(Error::Corrupt);
                }
                serial::write(format_args!("[NEU] Switch outcome: label={} thread={} reason={} result={} steps={} exited_dispatch=rejected\n",domain.label,domain.thread.raw(),if matches!(reason,ExitReason::Normal(_)) {"normal"} else {"fault"},words[0],words[1])).map_err(|_|Error::Corrupt)?;
                reclaim(paging, &mut model, domain)?;
                let other = if is_a { &b } else { &a };
                if other.space.is_some() {
                    survivor(paging, &model, &queue, other)?;
                }
                if case == 8 && is_a {
                    stale_audit(paging, &mut model, &mut queue, &a)?;
                    survivor(paging, &model, &queue, &b)?;
                }
            }
        }
    }
    if a.outcome.is_none()
        || b.outcome.is_none()
        || a.space.is_some()
        || b.space.is_some()
        || !queue.is_empty()
        || paging.stats() != before
        || paging.table_count() != tables
    {
        return Err(Error::Corrupt);
    }
    serial::write(format_args!("[NEU] Switch accounting restored: allocated={} free={} tables={}\n[NEU] Cooperative switch test passed: case={} dispatches={} queue=empty IF=0\n",paging.stats().allocated,paging.stats().free,paging.table_count(),case,dispatches)).map_err(|_|Error::Corrupt)
}

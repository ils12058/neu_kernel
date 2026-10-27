//! Trusted three-Thread bootstrap: Node resources and Thread resources have
//! separate lifetimes. IRQ capture continues to use only the Phase 4C mailbox.
use super::{
    cooperative, native,
    paging::{Paging, UserSpace},
    preemptive, serial, userspace,
};
use core::{arch::global_asm, ptr};
#[cfg(feature = "channel-wait-test")]
use neu_kernel::wait::{self, Waiter};
use neu_kernel::{
    context::{SavedContext, SliceEvent, StackBinding},
    node::{
        self, ActiveContext, ExitReason, Fault, FaultSource, NodeId, NodeState, Nodes,
        ThreadContext, ThreadState,
    },
    paging::{self as logic, Error, PAGE, Result},
    scheduler::Queue,
    syscall::{self as abi, Frame, ReturnContract},
};
global_asm!(include_str!("capabilities.S"),include_str!("address_space.S"),include_str!("ipc.S"),include_str!("moves.S"),include_str!("multithread.S"),capabilities=const cfg!(feature="cap-test") as u8,abi02=const (cfg!(feature="abi-02") && !cfg!(feature="channel-test")) as u8,channels=const cfg!(feature="channel-test") as u8,ipc_stress=const cfg!(feature="test-channels-stress") as u8,ipc_move=const cfg!(feature="cap-move-test") as u8,move_discard=const cfg!(feature="test-cap-moves-discard") as u8,move_mode=const if cfg!(feature="test-cap-moves-sender-fault-before") {7u8} else if cfg!(feature="test-cap-moves-sender-fault") {2u8} else if cfg!(feature="test-cap-moves-receiver-fault-after") {3} else if cfg!(feature="test-cap-moves-double") {4} else if cfg!(feature="test-cap-moves-backpressure-wait") {5} else if cfg!(feature="test-cap-moves-terminal-order") {6} else {0},ipc_wait=const cfg!(feature="channel-wait-test") as u8,ipc_legacy=const cfg!(feature="test-channels-legacy") as u8,wait_mode=const if cfg!(feature="test-channels-wait-send") {1} else if cfg!(feature="test-channels-wait-fifo") {2} else if cfg!(feature="test-channels-wait-close-receive") {3} else if cfg!(feature="test-channels-wait-close-send") {4} else if cfg!(feature="test-channels-wait-fault") {5} else if cfg!(feature="test-channels-wait-deadlock") {6} else {0},ipc_fault_a=const cfg!(feature="test-channels-a-fault") as u8,ipc_fault_b=const cfg!(feature="test-channels-b-fault") as u8,move_disabled=const (cfg!(feature="test-address-space-move-disabled") || cfg!(feature="test-channels-move-disabled") || cfg!(feature="test-channels-wait-move-disabled")) as u8,channel_disabled=const cfg!(feature="test-address-space-channel-disabled") as u8,abi_info=const if cfg!(feature="cap-move-test") {abi::v05::INFO}else if cfg!(feature="channel-wait-test") {abi::v04::INFO}else if cfg!(feature="channel-test") {abi::v03::INFO}else if cfg!(feature="abi-02") {abi::v02::INFO} else {abi::ABI_INFO},cap_rollback=const cfg!(feature="test-capabilities-rollback") as u8);
unsafe extern "C" {
    static neu_multi_payload: u8;
    static neu_multi_payload_end: u8;
    static neu_multi_syscall_resume: u8;
    #[cfg(feature = "cap-test")]
    static neu_cap_syscall_resume: u8;
    #[cfg(all(feature = "abi-02", not(feature = "channel-test")))]
    static neu_as_syscall_resume: u8;
    #[cfg(feature = "channel-test")]
    static neu_ipc_syscall_resume: u8;
    static neu_multi_after_atomic: u8;
    static neu_multi_after_inc: u8;
}
pub fn site(symbol: *const u8) -> u64 {
    userspace::CODE + symbol as u64 - ptr::addr_of!(neu_multi_payload) as u64
}
pub fn payload() -> &'static [u8] {
    let length =
        ptr::addr_of!(neu_multi_payload_end) as usize - ptr::addr_of!(neu_multi_payload) as usize;
    assert!(length > 0 && length <= PAGE as usize);
    // SAFETY: Immutable bounded fixture bytes copied into owned Node RX backing.
    unsafe { core::slice::from_raw_parts(ptr::addr_of!(neu_multi_payload), length) }
}
pub fn return_contract(root: u64, context: ThreadContext) -> ReturnContract {
    let base = context.rsp & !(PAGE - 1);
    ReturnContract {
        root,
        code_start: userspace::CODE,
        code_end: site(ptr::addr_of!(neu_multi_payload_end)),
        stack_start: base,
        stack_end: base + PAGE,
        resumes: [site(ptr::addr_of!(neu_multi_syscall_resume)), {
            #[cfg(feature = "cap-test")]
            {
                site(ptr::addr_of!(neu_cap_syscall_resume))
            }
            #[cfg(all(feature = "abi-02", not(feature = "channel-test")))]
            {
                site(ptr::addr_of!(neu_as_syscall_resume))
            }
            #[cfg(feature = "channel-test")]
            {
                site(ptr::addr_of!(neu_ipc_syscall_resume))
            }
            #[cfg(not(any(feature = "cap-test", feature = "abi-02")))]
            {
                site(ptr::addr_of!(neu_multi_syscall_resume))
            }
        }],
    }
}
fn check<T>(result: node::Result<T>) -> Result<T> {
    result.map_err(|_| Error::Corrupt)
}
struct Domain {
    id: NodeId,
    space: Option<UserSpace>,
}
#[derive(Clone, Copy)]
struct Member {
    label: u64,
    seed: u64,
    active: ActiveContext,
    stack: Option<StackBinding>,
    last: Option<Frame>,
    preemptions: u64,
    outcome: Option<ExitReason>,
    committed: u64,
}
fn space(domain: &Domain) -> Result<&UserSpace> {
    domain.space.as_ref().ok_or(Error::NotOwned)
}
fn space_mut(domain: &mut Domain) -> Result<&mut UserSpace> {
    domain.space.as_mut().ok_or(Error::NotOwned)
}
fn create(
    paging: &mut Paging<'_, '_>,
    nodes: &mut Nodes,
    label: u64,
    slot: usize,
) -> Result<(Domain, Member)> {
    let id = check(nodes.create())?;
    check(nodes.configure(id))?;
    let mut root = paging.prepare_domain(
        payload(),
        0,
        if label == 1 { 35 } else { 77 },
        if label == 3 {
            Some(native::PRIVATE_VALUE)
        } else {
            None
        },
        None,
    )?;
    paging.relocate_user_stack(&mut root, logic::thread_user_stack(slot))?;
    if label == 1 {
        paging.map_shared_probe(&root)?;
    }
    check(nodes.bind_space(id, root.id()))?;
    let context = ThreadContext {
        space: root.id(),
        root: root.root,
        entry: userspace::CODE,
        rsp: logic::thread_user_stack(slot) + PAGE - 8,
    };
    let thread = check(nodes.initial_thread(id, context))?;
    if thread.index() != slot {
        return Err(Error::Corrupt);
    }
    check(nodes.ready(id))?;
    Ok((
        Domain {
            id,
            space: Some(root),
        },
        Member {
            label,
            seed: if label == 1 { 35 } else { 77 },
            active: ActiveContext {
                node: id,
                thread,
                context,
            },
            stack: None,
            last: None,
            preemptions: 0,
            outcome: None,
            committed: 0,
        },
    ))
}
fn add_member(
    nodes: &mut Nodes,
    domain: &Domain,
    label: u64,
    seed: u64,
    slot: usize,
) -> Result<Member> {
    let s = space(domain)?;
    let context = ThreadContext {
        space: s.id(),
        root: s.root,
        entry: userspace::CODE,
        rsp: logic::thread_user_stack(slot) + PAGE - 8,
    };
    let thread = check(nodes.member_thread(domain.id, context))?;
    if thread.index() != slot {
        return Err(Error::Corrupt);
    }
    Ok(Member {
        label,
        seed,
        active: ActiveContext {
            node: domain.id,
            thread,
            context,
        },
        stack: None,
        last: None,
        preemptions: 0,
        outcome: None,
        committed: 0,
    })
}
fn install(
    paging: &mut Paging<'_, '_>,
    nodes: &mut Nodes,
    queue: &mut Queue,
    domain: &mut Domain,
    member: &mut Member,
    action: u64,
) -> Result<()> {
    let a = member.active;
    let binding = {
        let edit = check(nodes.stack_edit(a.thread))?;
        paging.prepare_member(
            space_mut(domain)?,
            &edit,
            [
                member.label,
                member.seed,
                0x402040 + (member.label - 1) * 32,
                action,
            ],
            None,
        )?
    };
    check(nodes.install_execution(a.thread, SavedContext::initial_preemptible(a, binding)))?;
    paging.member_init_state(space(domain)?, member.label)?;
    paging.pin_member(space_mut(domain)?, a.thread)?;
    check(queue.enqueue(nodes, a.thread))?;
    member.stack = Some(binding);
    serial::write(format_args!("[NEU] Multi Thread ready: label={} node={} thread={} space={} root={:#x} user_base={:#x} initial_rsp={:#x} bottom={:#x} top={:#x} data_pa={:#x} user_pa={:#x} k0={:#x} k1={:#x} seed={}\n",member.label,a.node.raw(),a.thread.raw(),a.context.space.raw(),a.context.root,a.context.rsp&!(PAGE-1),a.context.rsp,binding.bottom,binding.top,paging.user_physical(space(domain)?,0x402000)?,paging.user_physical(space(domain)?,a.context.rsp&!(PAGE-1))?,paging.user_physical(space(domain)?,binding.bottom)?,paging.user_physical(space(domain)?,binding.bottom+PAGE)?,member.seed)).map_err(|_|Error::Corrupt)
}
fn committed(stored: u64, frame: Option<Frame>) -> Result<u64> {
    if let Some(frame) = frame {
        if frame.context.rip == site(ptr::addr_of!(neu_multi_after_atomic)) {
            if frame.registers.rax != stored {
                return Err(Error::Corrupt);
            }
            return stored.checked_add(1).ok_or(Error::Corrupt);
        }
        if frame.context.rip == site(ptr::addr_of!(neu_multi_after_inc)) {
            if frame.registers.rax != stored + 1 {
                return Err(Error::Corrupt);
            }
            return Ok(frame.registers.rax);
        }
    }
    Ok(stored)
}
fn audit(paging: &mut Paging<'_, '_>, domain: &Domain, members: &[Member; 3]) -> Result<()> {
    let words = paging.member_words(space(domain)?)?;
    let mut total = 0;
    let mut pending = 0;
    for m in members.iter().filter(|m| m.active.node == domain.id) {
        let stored = words[8 + (m.label as usize - 1) * 4];
        let count = if m.outcome.is_some() {
            m.committed
        } else {
            committed(stored, m.last)?
        };
        if count < stored || count - stored > 1 {
            return Err(Error::Corrupt);
        }
        pending += count - stored;
        total += count;
    }
    if words[0] != total {
        return Err(Error::Corrupt);
    }
    serial::write(format_args!(
        "[NEU] Multi shared audit: node={} total={} committed={} pending={} pins={}\n",
        domain.id.raw(),
        words[0],
        total,
        pending,
        paging.member_pins(space(domain)?)?
    ))
    .map_err(|_| Error::Corrupt)
}
fn survivor(
    paging: &mut Paging<'_, '_>,
    nodes: &Nodes,
    queue: &Queue,
    domain: &Domain,
    members: &[Member; 3],
) -> Result<()> {
    if paging
        .private_value(space(domain)?)?
        .is_some_and(|value| value != native::PRIVATE_VALUE)
    {
        return Err(Error::Corrupt);
    }
    for m in members
        .iter()
        .filter(|m| m.active.node == domain.id && m.outcome.is_none())
    {
        let t = check(nodes.thread(m.active.thread))?;
        let blocked = t.state == ThreadState::Blocked
            && !t.queued
            && t.saved.is_some_and(|saved| saved.continuation);
        let runnable = t.state == ThreadState::Runnable && queue.contains(t.id) && t.queued;
        if (!runnable && !blocked)
            || t.saved.is_none()
            || check(nodes.node(domain.id))?.space != Some(m.active.context.space)
        {
            return Err(Error::Corrupt);
        }
        paging.verify_member(space(domain)?, m.active, m.stack.ok_or(Error::Corrupt)?)?;
    }
    serial::write(format_args!(
        "[NEU] Multi survivor retained: node={} pins={} root={:#x}\n",
        domain.id.raw(),
        paging.member_pins(space(domain)?)?,
        space(domain)?.root
    ))
    .map_err(|_| Error::Corrupt)
}
fn retire(
    paging: &mut Paging<'_, '_>,
    nodes: &mut Nodes,
    domain: &mut Domain,
    member: &mut Member,
) -> Result<()> {
    let before = paging.stats();
    let edit = check(nodes.stack_edit(member.active.thread))?;
    let release = paging.destroy_member(space_mut(domain)?, &edit)?;
    check(edit.finish(release))?;
    member.stack = None;
    member.last = None;
    let after = paging.stats();
    if before.allocated < after.allocated + 3 {
        return Err(Error::Corrupt);
    }
    serial::write(format_args!("[NEU] Multi Thread retired: label={} thread={} freed={} pins={} root={:#x} outcome=retained\n",member.label,member.active.thread.raw(),before.allocated-after.allocated,paging.member_pins(space(domain)?)?,space(domain)?.root)).map_err(|_|Error::Corrupt)
}
fn rollback(
    paging: &mut Paging<'_, '_>,
    nodes: &mut Nodes,
    queue: &Queue,
    a: &mut Domain,
    b: &Domain,
    first: &Member,
    other: &Member,
) -> Result<()> {
    let saved = [
        check(nodes.thread(first.active.thread))?.saved,
        check(nodes.thread(other.active.thread))?.saved,
    ];
    for budget in 0..4 {
        let m = add_member(nodes, a, 2, 53, 2)?;
        let before = paging.stats();
        let tables = paging.table_count();
        let result = {
            let edit = check(nodes.stack_edit(m.active.thread))?;
            paging.prepare_member(space_mut(a)?, &edit, [2, 53, 0x402060, 0], Some(budget))
        };
        if result != Err(Error::Exhausted)
            || paging.stats() != before
            || paging.table_count() != tables
            || paging.member_pins(space(a)?)? != 1
            || paging.member_pins(space(b)?)? != 1
            || check(nodes.thread(first.active.thread))?.saved != saved[0]
            || check(nodes.thread(other.active.thread))?.saved != saved[1]
            || !queue.contains(first.active.thread)
            || !queue.contains(other.active.thread)
        {
            return Err(Error::Corrupt);
        }
        check(nodes.abort_member(m.active.thread))?;
        check(nodes.reclaim_thread(m.active.thread))?;
        serial::write(format_args!("[NEU] Multi rollback: budget={} accounting=restored sibling_contexts=unchanged pins=1,1\n",budget)).map_err(|_|Error::Corrupt)?;
    }
    let m = add_member(nodes, a, 2, 53, 2)?;
    let before = paging.stats();
    let tables = paging.table_count();
    let binding = {
        let edit = check(nodes.stack_edit(m.active.thread))?;
        paging.prepare_member(space_mut(a)?, &edit, [2, 53, 0x402060, 0], None)?
    };
    let mut bad = SavedContext::initial_preemptible(m.active, binding);
    bad.frame.context.rip = 0x800000000000;
    if nodes.install_execution(m.active.thread, bad) != Err(node::Error::Association) {
        return Err(Error::Corrupt);
    }
    {
        let edit = check(nodes.stack_edit(m.active.thread))?;
        paging.destroy_member(space_mut(a)?, &edit)?;
    }
    check(nodes.abort_member(m.active.thread))?;
    check(nodes.reclaim_thread(m.active.thread))?;
    if paging.stats() != before || paging.table_count() != tables {
        return Err(Error::Corrupt);
    }
    serial::write(format_args!(
        "[NEU] Multi context rollback: rejected=Association accounting=restored\n"
    ))
    .map_err(|_| Error::Corrupt)
}
fn stale(
    paging: &mut Paging<'_, '_>,
    nodes: &mut Nodes,
    queue: &mut Queue,
    a: &mut Domain,
    old: &Member,
) -> Result<()> {
    let before = paging.stats();
    let tables = paging.table_count();
    let pins = paging.member_pins(space(a)?)?;
    let replacement = add_member(nodes, a, 4, 99, 0)?;
    if replacement.active.thread == old.active.thread
        || queue.enqueue(nodes, old.active.thread) != Err(node::Error::Identity)
    {
        return Err(Error::Corrupt);
    }
    {
        let edit = check(nodes.stack_edit(replacement.active.thread))?;
        paging.prepare_member(space_mut(a)?, &edit, [4, 99, 0x4020a0, 0], None)?;
    }
    check(nodes.abort_member(replacement.active.thread))?;
    {
        let edit = check(nodes.stack_edit(replacement.active.thread))?;
        let release = paging.destroy_member(space_mut(a)?, &edit)?;
        check(edit.finish(release))?;
    }
    if paging.stats() != before
        || paging.table_count() != tables
        || paging.member_pins(space(a)?)? != pins
    {
        return Err(Error::Corrupt);
    }
    serial::write(format_args!(
        "[NEU] Multi stale rejected: old={} replacement={} root_pins={} accounting=restored\n",
        old.active.thread.raw(),
        replacement.active.thread.raw(),
        pins
    ))
    .map_err(|_| Error::Corrupt)
}
#[cfg(feature = "channel-wait-test")]
fn wait_event(event: u64, waiter: Option<Waiter>, result: i64, queue: usize, saved: Option<Frame>) {
    let stats = super::address_space::wait_stats().unwrap_or_default();
    let saved = saved.map_or(wait::witness::Saved::NONE, |frame| wait::witness::Saved {
        present: true,
        rip: frame.context.rip,
        rsp: frame.context.rsp,
        flags: frame.context.flags,
    });
    wait::witness::write(event, waiter, result, queue, stats, 0, 0, saved);
}
#[cfg(feature = "channel-wait-test")]
fn drain_ready(nodes: &mut Nodes, queue: &mut Queue) -> Result<()> {
    let ready = super::address_space::take_ready_waits().map_err(|_| Error::Corrupt)?;
    for waiter in ready.into_iter().flatten() {
        check(queue.wake(nodes, waiter.active.thread))?;
        let saved = check(nodes.thread(waiter.active.thread))?
            .saved
            .map(|s| s.frame);
        wait_event(wait::witness::WOKEN, Some(waiter), 0, queue.len(), saved);
        serial::write(format_args!(
            "[NEU] Multi woken: thread={} wait={} kind={} RIP={:#x}\n",
            waiter.active.thread.raw(),
            waiter.token,
            match waiter.kind {
                neu_kernel::wait::Kind::Send => 1,
                neu_kernel::wait::Kind::Receive => 2,
            },
            check(nodes.thread(waiter.active.thread))?
                .saved
                .map_or(0, |s| s.frame.context.rip)
        ))
        .map_err(|_| Error::Corrupt)?;
    }
    Ok(())
}

#[cfg(feature = "channel-wait-test")]
fn settle_retry(
    nodes: &mut Nodes,
    queue: &mut Queue,
    active: ActiveContext,
    saved: SavedContext,
) -> Result<()> {
    check(queue.reblock(nodes, active, saved))?;
    Ok(())
}

#[cfg(feature = "channel-wait-test")]
fn quiesce(
    paging: &mut Paging<'_, '_>,
    nodes: &mut Nodes,
    a: &mut Domain,
    b: &mut Domain,
    members: &mut [Member; 3],
) -> Result<()> {
    for domain in [&mut *a, &mut *b] {
        if domain.space.is_none() {
            continue;
        }
        let canceled =
            super::address_space::cancel_node_waits(domain.id).map_err(|_| Error::Corrupt)?;
        wait_event(wait::witness::QUIESCE, None, canceled as i64, 0, None);
        for id in nodes.blocked_threads(domain.id).into_iter().flatten() {
            check(nodes.cancel_blocked(id, ExitReason::Quiesced))?;
        }
        if check(nodes.node(domain.id))?.state == NodeState::Active {
            check(nodes.quiesce(domain.id))?;
        }
        for m in members
            .iter_mut()
            .filter(|m| m.active.node == domain.id && m.outcome.is_none())
        {
            m.outcome = Some(ExitReason::Quiesced);
            m.last = None;
            m.committed = 0;
        }
        let domain_id = domain.id;
        for m in members
            .iter_mut()
            .filter(|m| m.active.node == domain_id && m.stack.is_some())
        {
            retire(paging, nodes, domain, m)?;
        }
        if paging.member_pins(space(domain)?)? != 0 {
            return Err(Error::InUse);
        }
        super::address_space::cleanup(nodes, domain.id, paging).map_err(|_| Error::Corrupt)?;
        let s = domain.space.take().ok_or(Error::NotOwned)?;
        let id = s.id();
        paging.destroy_user(s)?;
        check(nodes.release_space(domain.id, id))?;
        check(nodes.reclaim(domain.id))?;
        serial::write(format_args!(
            "[NEU] Multi Node quiesced: node={} waits_canceled={} reason=no-runnable\n",
            domain.id.raw(),
            canceled
        ))
        .map_err(|_| Error::Corrupt)?;
    }
    Ok(())
}

pub fn run(paging: &mut Paging<'_, '_>) -> Result<()> {
    let before = paging.stats();
    let tables = paging.table_count();
    serial::write(format_args!(
        "[NEU] Multi baseline: allocated={} free={} tables={} root={:#x}\n",
        before.allocated,
        before.free,
        tables,
        paging.root()
    ))
    .map_err(|_| Error::Corrupt)?;
    preemptive::start(paging)?;
    let mut nodes = Nodes::new();
    let mut queue = Queue::new();
    let (mut a, mut a1) = create(paging, &mut nodes, 1, 0)?;
    let (mut b, mut b1) = create(paging, &mut nodes, 3, 1)?;
    let actions = [
        if cfg!(feature = "test-threads-a1-fault")
            || cfg!(feature = "test-capabilities-a-fault")
            || cfg!(feature = "test-address-space-a-fault")
            || cfg!(feature = "test-channels-a-fault")
            || cfg!(feature = "test-cap-moves-sender-fault")
            || cfg!(feature = "test-cap-moves-sender-fault-before")
        {
            1
        } else if cfg!(feature = "test-threads-a-cross") {
            2
        } else if cfg!(feature = "test-threads-stack") {
            4
        } else {
            0
        },
        if cfg!(feature = "test-threads-a2-fault")
            || cfg!(feature = "test-capabilities-a2-fault")
            || cfg!(feature = "test-address-space-a2-fault")
            || cfg!(feature = "test-channels-wait-fault")
        {
            1
        } else if cfg!(feature = "test-threads-user-guard") {
            5
        } else {
            0
        },
        if cfg!(feature = "test-threads-b-fault")
            || cfg!(feature = "test-capabilities-b-fault")
            || cfg!(feature = "test-address-space-b-fault")
            || cfg!(feature = "test-channels-b-fault")
            || cfg!(feature = "test-cap-moves-discard")
            || cfg!(feature = "test-cap-moves-receiver-fault-after")
            || cfg!(feature = "test-cap-moves-terminal-order")
        {
            1
        } else if cfg!(feature = "test-threads-cross") {
            3
        } else {
            0
        },
    ];
    install(paging, &mut nodes, &mut queue, &mut a, &mut a1, actions[0])?;
    install(paging, &mut nodes, &mut queue, &mut b, &mut b1, actions[2])?;
    if cfg!(feature = "test-threads-rollback") {
        rollback(paging, &mut nodes, &queue, &mut a, &b, &a1, &b1)?;
    }
    let mut a2 = add_member(&mut nodes, &a, 2, 53, 2)?;
    install(paging, &mut nodes, &mut queue, &mut a, &mut a2, actions[1])?;
    #[cfg(feature = "cap-test")]
    super::capabilities::bootstrap(&mut nodes, a.id, b.id).map_err(|_| Error::Corrupt)?;
    #[cfg(feature = "cap-test")]
    let mut caps_ready = false;
    #[cfg(feature = "abi-02")]
    super::address_space::bootstrap(&mut nodes, paging, a.id, space(&a)?, b.id, space(&b)?)
        .map_err(|_| Error::Corrupt)?;
    let mut members = [a1, a2, b1];
    let mut limited = [false; 3];
    let mut printed = 0;
    let mut dispatches = 0;
    #[cfg(feature = "abi-02")]
    let mut settled_slices = [0u8; 3];
    if queue.len() != 3
        || paging.member_pins(space(&a)?)? != 2
        || paging.member_pins(space(&b)?)? != 1
    {
        return Err(Error::Corrupt);
    }
    let peak = paging.stats();
    let peak_tables = paging.table_count();
    if peak.allocated != peak_tables + logic::DATA_FRAMES {
        return Err(Error::Corrupt);
    }
    serial::write(format_args!(
        "[NEU] Multi three runnable: order=1,3,2 pins=2,1 data_owners=16 quantum_us=10000 allocated={} free={} tables={}\n",
        peak.allocated, peak.free, peak_tables
    ))
    .map_err(|_| Error::Corrupt)?;
    while let Some(mut dispatch) = {
        #[cfg(feature = "channel-wait-test")]
        drain_ready(&mut nodes, &mut queue)?;
        check(queue.next(&mut nodes))?
    } {
        dispatches += 1;
        if dispatches
            > if cfg!(feature = "channel-test") {
                128
            } else {
                64
            }
        {
            return Err(Error::Corrupt);
        }
        let index = members
            .iter()
            .position(|m| m.active == dispatch.active)
            .ok_or(Error::Corrupt)?;
        let active = dispatch.active;
        let owner_node = if index == 2 { &b } else { &a };
        if members[index].outcome.is_some()
            || (!dispatch.saved.fresh && members[index].last != Some(dispatch.saved.frame))
        {
            return Err(Error::Corrupt);
        }
        serial::write(format_args!("[NEU] Multi selected: sequence={} label={} node={} thread={} space={} root={:#x} fresh={} peer={}\n",dispatches,members[index].label,active.node.raw(),active.thread.raw(),active.context.space.raw(),active.context.root,dispatch.saved.fresh,!queue.is_empty())).map_err(|_|Error::Corrupt)?;
        cooperative::frame_log("multi-taken", active.thread, dispatch.saved.frame)?;
        let binding = members[index].stack.ok_or(Error::Corrupt)?;
        if paging.context_reclaimable(space(owner_node)?) != Err(Error::InUse)
            || nodes.stack_edit(active.thread).err() != Some(node::Error::InUse)
            || nodes.release_stack(active.thread, binding) != Err(node::Error::InUse)
        {
            return Err(Error::Corrupt);
        }
        #[cfg(feature = "test-cap-moves-terminal-order")]
        super::ipc::publish_queue_addresses(&queue);
        let publication = preemptive::arm(active, binding, !queue.is_empty())?;
        let reject = cfg!(feature = "test-threads-return-stack")
            && index == 1
            && members[index].preemptions == 1;
        if reject {
            dispatch.saved.frame.context.rsp = members[0].active.context.rsp - 64;
        }
        #[cfg(feature = "cap-test")]
        super::capabilities::enter(check(nodes.take_authority(active))?)
            .map_err(|_| Error::Corrupt)?;
        #[cfg(feature = "abi-02")]
        super::address_space::enter(check(nodes.take_authority(active))?)
            .map_err(|_| Error::Corrupt)?;
        #[cfg(feature = "channel-wait-test")]
        if dispatch.saved.continuation {
            use neu_kernel::capability::Settle;
            match super::address_space::settle(active).map_err(|_| Error::Corrupt)? {
                Settle::Committed(value) => {
                    dispatch.saved.frame.registers.rax = 0;
                    dispatch.saved.frame.registers.rdx = value;
                    members[index].last = Some(dispatch.saved.frame);
                    serial::write(format_args!(
                        "[NEU] Multi settled: thread={} result={:#x}\n",
                        active.thread.raw(),
                        value
                    ))
                    .map_err(|_| Error::Corrupt)?;
                }
                Settle::Failed(error) => {
                    dispatch.saved.frame.registers.rax = error as i64 as u64;
                    dispatch.saved.frame.registers.rdx = 0;
                    members[index].last = Some(dispatch.saved.frame);
                    serial::write(format_args!(
                        "[NEU] Multi settled: thread={} error={}\n",
                        active.thread.raw(),
                        error as i64
                    ))
                    .map_err(|_| Error::Corrupt)?;
                }
                Settle::Retry => {
                    preemptive::terminal(publication)?;
                    check(nodes.restore_authority(
                        super::address_space::leave(active).map_err(|_| Error::Corrupt)?,
                    ))?;
                    settle_retry(&mut nodes, &mut queue, active, dispatch.saved)?;
                    serial::write(format_args!(
                        "[NEU] Multi retry blocked: thread={} RIP={:#x}\n",
                        active.thread.raw(),
                        dispatch.saved.frame.context.rip
                    ))
                    .map_err(|_| Error::Corrupt)?;
                    continue;
                }
            }
        }
        #[cfg(feature = "channel-test")]
        {
            let gates = super::address_space::channel_gates().map_err(|_| Error::Corrupt)?
                | if members[0].outcome.is_some() { 4 } else { 0 };
            paging.channel_control(space(owner_node)?, dispatches, gates)?;
        }
        let execution = native::execute_slice(paging, space(owner_node)?, dispatch, 96);
        #[cfg(feature = "cap-test")]
        check(
            nodes
                .restore_authority(super::capabilities::leave(active).map_err(|_| Error::Corrupt)?),
        )?;
        #[cfg(feature = "abi-02")]
        {
            check(nodes.restore_authority(
                super::address_space::leave(active).map_err(|_| Error::Corrupt)?,
            ))?;
            super::address_space::reap(paging).map_err(|_| Error::Corrupt)?;
        }
        let event = match execution {
            Ok(event) if !reject => event,
            Err(Error::Flags) if reject => {
                preemptive::terminal(publication)?;
                serial::write(format_args!("[NEU] Multi return stack rejected: thread={} sibling_rsp={:#x} common={:#x} before_entry=true\n",active.thread.raw(),members[0].active.context.rsp-64,paging.root())).map_err(|_|Error::Corrupt)?;
                SliceEvent::Exited(ExitReason::UserFault(Fault {
                    source: FaultSource::RejectedReturn,
                    vector: 0,
                    error: 4,
                    rip: active.context.entry,
                    rsp: members[0].active.context.rsp - 64,
                    cs: abi::USER_CS,
                    ss: abi::USER_SS,
                    flags: 0x202,
                    cr2: 0,
                }))
            }
            _ => return Err(Error::Corrupt),
        };
        preemptive::settle()?;
        preemptive::print_records(&mut printed)?;
        match event {
            SliceEvent::Preempted(frame) => {
                let saved = preemptive::consume(publication, frame)?;
                members[index].last = Some(frame);
                members[index].preemptions += 1;
                cooperative::frame_log("multi-saved", active.thread, frame)?;
                check(queue.preempted(&mut nodes, saved))?;
                let words = paging.member_words(space(owner_node)?)?;
                let offset = 8 + index * 4;
                if words[offset + 2] != 1
                    || words[offset + 3] != (0x11225566778899aa ^ members[index].seed)
                {
                    return Err(Error::Corrupt);
                }
                let count = committed(words[offset], Some(frame))?;
                #[cfg(feature = "cap-move-test")]
                if cfg!(feature = "test-cap-moves-sender-fault") && index == 0 {
                    let gates =
                        super::address_space::channel_gates().map_err(|_| Error::Corrupt)?;
                    if gates & (1 << 26) != 0 {
                        // The capability move has linearized. Let this selected
                        // A1 reach its configured fault action on the next
                        // loop boundary, before B is permitted to consume it.
                        paging.member_limit(
                            space(&a)?,
                            members[index].label,
                            count.checked_add(1).ok_or(Error::Corrupt)?,
                        )?;
                        limited[index] = true;
                    }
                }
                #[cfg(feature = "cap-move-test")]
                if cfg!(feature = "test-cap-moves-sender-fault-before")
                    && index == 0
                    && !limited[index]
                {
                    let gates =
                        super::address_space::channel_gates().map_err(|_| Error::Corrupt)?;
                    if gates & 1 != 0 {
                        // The preflight Full result proves the sender reached
                        // the boundary; its test image is now terminally armed
                        // and cannot issue the later move transaction.
                        paging.member_limit(
                            space(&a)?,
                            members[index].label,
                            count.checked_add(100_000).ok_or(Error::Corrupt)?,
                        )?;
                        limited[index] = true;
                    }
                }
                #[cfg(feature = "cap-move-test")]
                if cfg!(feature = "test-cap-moves-receiver-fault-after") && index == 2 {
                    let gates =
                        super::address_space::channel_gates().map_err(|_| Error::Corrupt)?;
                    if gates & (1 << 30) != 0 {
                        // Keep the delivered handle live until B's configured
                        // user-fault action runs through ordinary Node cleanup.
                        paging.member_limit(
                            space(&b)?,
                            members[index].label,
                            count.checked_add(1).ok_or(Error::Corrupt)?,
                        )?;
                        limited[index] = true;
                    }
                }
                #[cfg(feature = "abi-02")]
                if paging.member_cap_words(space(&a)?)?[0] == 3
                    && (!cfg!(feature = "test-channels-a-fault")
                        && !cfg!(feature = "test-channels-b-fault")
                        || {
                            #[cfg(feature = "channel-test")]
                            {
                                super::address_space::channel_gates().map_err(|_| Error::Corrupt)?
                                    & 8
                                    != 0
                            }
                            #[cfg(not(feature = "channel-test"))]
                            {
                                true
                            }
                        })
                {
                    settled_slices[index] += 1;
                }
                serial::write(format_args!("[NEU] Multi progress: label={} captures={} stored={} committed={} boots={} total={}\n",members[index].label,members[index].preemptions,words[offset],count,words[offset+2],words[0])).map_err(|_|Error::Corrupt)?;
                audit(paging, owner_node, &members)?;
                #[cfg(feature = "cap-test")]
                if !caps_ready {
                    let phase = paging.member_cap_words(space(&a)?)?[0];
                    if phase == 4 {
                        let handle = super::capabilities::regrant(&mut nodes, a.id)
                            .map_err(|_| Error::Corrupt)?;
                        paging.member_cap_write(space(&a)?, 2, handle)?;
                        paging.member_cap_write(space(&a)?, 0, 5)?;
                    } else if phase == 6 {
                        caps_ready = true;
                    }
                }
                let faulting = actions.iter().position(|action| *action != 0);
                let early = if let Some(fault) = faulting {
                    index == fault
                } else {
                    index == 0
                };
                if early && members[index].preemptions >= 4 && !limited[index] && {
                    #[cfg(feature = "cap-test")]
                    {
                        caps_ready
                    }
                    #[cfg(feature = "abi-02")]
                    {
                        settled_slices[index] >= 3
                    }
                    #[cfg(not(any(feature = "cap-test", feature = "abi-02")))]
                    {
                        true
                    }
                } {
                    paging.member_limit(
                        space(owner_node)?,
                        members[index].label,
                        count + 100_000,
                    )?;
                    limited[index] = true;
                }
            }
            SliceEvent::Exited(reason) => {
                if !reject {
                    preemptive::terminal(publication)?;
                }
                let words = paging.member_words(space(owner_node)?)?;
                members[index].committed = if reject {
                    committed(words[8 + index * 4], members[index].last)?
                } else {
                    words[8 + index * 4]
                };
                members[index].last = None;
                members[index].outcome = Some(reason);
                // Snapshot canceled members' partially completed atomic sequences
                // before finish clears their model saved-context references.
                let mut cancels = [None; 3];
                if matches!(reason, ExitReason::UserFault(_)) {
                    for (other, m) in members.iter().enumerate().filter(|(other, m)| {
                        *other != index && m.active.node == active.node && m.outcome.is_none()
                    }) {
                        cancels[other] = Some(committed(words[8 + other * 4], m.last)?);
                    }
                }
                #[cfg(feature = "channel-wait-test")]
                if matches!(reason, ExitReason::UserFault(_)) {
                    let canceled = super::address_space::cancel_node_waits(active.node)
                        .map_err(|_| Error::Corrupt)?;
                    serial::write(format_args!(
                        "[NEU] Multi waits canceled: node={} count={} reason=fault\n",
                        active.node.raw(),
                        canceled
                    ))
                    .map_err(|_| Error::Corrupt)?;
                }
                check(queue.finish(&mut nodes, active, reason))?;
                for (other, count) in cancels.into_iter().enumerate() {
                    if let Some(count) = count {
                        let m = &mut members[other];
                        m.committed = count;
                        m.outcome = Some(ExitReason::Cancelled { by: active.thread });
                        m.last = None;
                        if queue.contains(m.active.thread)
                            || check(nodes.thread(m.active.thread))?.saved.is_some()
                        {
                            return Err(Error::Corrupt);
                        }
                        serial::write(format_args!("[NEU] Multi cancelled: label={} thread={} by={} committed={} saved=false queued=false\n",m.label,m.active.thread.raw(),active.thread.raw(),count)).map_err(|_|Error::Corrupt)?;
                    }
                }
                if queue.enqueue(&mut nodes, active.thread).is_ok() {
                    return Err(Error::Corrupt);
                }
                serial::write(format_args!("[NEU] Multi outcome: label={} thread={} reason={} status={} committed={} node_state={}\n",members[index].label,active.thread.raw(),if matches!(reason,ExitReason::Normal(_)){"normal"}else{"fault"},if let ExitReason::Normal(status)=reason {u64::from(status)}else{0},members[index].committed,if check(nodes.node(active.node))?.state==NodeState::Exited {"Exited"}else{"Active"})).map_err(|_|Error::Corrupt)?;
                audit(paging, owner_node, &members)?;
                if index == 0 && matches!(reason, ExitReason::Normal(_)) {
                    paging.member_done_flag(space(&a)?)?;
                }
                for m in members.iter_mut().filter(|m| {
                    m.active.node == active.node && m.outcome.is_some() && m.stack.is_some()
                }) {
                    retire(
                        paging,
                        &mut nodes,
                        if m.active.node == a.id {
                            &mut a
                        } else {
                            &mut b
                        },
                        m,
                    )?;
                }
                if check(nodes.node(active.node))?.state == NodeState::Exited {
                    let domain = if active.node == a.id { &mut a } else { &mut b };
                    if paging.member_pins(space(domain)?)? != 0 {
                        return Err(Error::InUse);
                    }
                    #[cfg(feature = "cap-test")]
                    super::capabilities::cleanup(&mut nodes, domain.id)
                        .map_err(|_| Error::Corrupt)?;
                    #[cfg(feature = "abi-02")]
                    super::address_space::cleanup(&mut nodes, domain.id, paging)
                        .map_err(|_| Error::Corrupt)?;
                    let s = domain.space.take().ok_or(Error::NotOwned)?;
                    let id = s.id();
                    paging.destroy_user(s)?;
                    check(nodes.release_space(domain.id, id))?;
                    check(nodes.reclaim(domain.id))?;
                    #[cfg(feature = "cap-move-test")]
                    super::ipc::mark_reclaimed(domain.id).map_err(|_| Error::Corrupt)?;
                    serial::write(format_args!(
                        "[NEU] Multi Node reclaimed: node={} final_outcome=retained\n",
                        domain.id.raw()
                    ))
                    .map_err(|_| Error::Corrupt)?;
                }
                if cfg!(feature = "test-threads-stale") && index == 0 {
                    stale(paging, &mut nodes, &mut queue, &mut a, &members[0])?;
                }
                for other in 0..3 {
                    if members[other].outcome.is_none() {
                        let domain = if other == 2 { &b } else { &a };
                        if !limited[other]
                            || (queue.len() == 1
                                && !(cfg!(feature = "test-cap-moves-sender-fault")
                                    || cfg!(feature = "test-cap-moves-receiver-fault-after")))
                        {
                            let words = paging.member_words(space(domain)?)?;
                            let count = committed(words[8 + other * 4], members[other].last)?;
                            paging.member_limit(
                                space(domain)?,
                                members[other].label,
                                count
                                    + if queue.len() == 1 {
                                        8_000_000
                                    } else if other == 0 {
                                        100_000
                                    } else if other == 1 {
                                        3_000_000
                                    } else {
                                        8_000_000
                                    },
                            )?;
                            limited[other] = true;
                        }
                    }
                }
                if a.space.is_some() {
                    survivor(paging, &nodes, &queue, &a, &members)?;
                }
                if b.space.is_some() {
                    survivor(paging, &nodes, &queue, &b, &members)?;
                }
            }
            #[cfg(feature = "channel-wait-test")]
            SliceEvent::Blocked(frame) => {
                preemptive::terminal(publication)?;
                members[index].last = Some(frame);
                let saved = SavedContext::continuation(active, binding, frame);
                check(queue.block(&mut nodes, active, saved))?;
                let waiter =
                    super::address_space::wait_waiter(active.thread).map_err(|_| Error::Corrupt)?;
                wait_event(wait::witness::BLOCKED, waiter, 0, queue.len(), Some(frame));
                cooperative::frame_log("multi-blocked", active.thread, frame)?;
                serial::write(format_args!(
                    "[NEU] Multi blocked: label={} thread={} RIP={:#x} RSP={:#x} queue={}\n",
                    members[index].label,
                    active.thread.raw(),
                    frame.context.rip,
                    frame.context.rsp,
                    queue.len()
                ))
                .map_err(|_| Error::Corrupt)?;
                audit(paging, owner_node, &members)?;
            }
            #[cfg(not(feature = "channel-wait-test"))]
            SliceEvent::Blocked(_) => return Err(Error::Corrupt),
            SliceEvent::Yield(_) => return Err(Error::Corrupt),
        }
    }
    #[cfg(feature = "channel-wait-test")]
    if members.iter().any(|m| m.outcome.is_none()) {
        quiesce(paging, &mut nodes, &mut a, &mut b, &mut members)?;
        serial::write(format_args!(
            "[NEU] Multi no-runnable classification: quiesced={} dispatches={}\n",
            members
                .iter()
                .filter(|m| m.outcome == Some(ExitReason::Quiesced))
                .count(),
            dispatches
        ))
        .map_err(|_| Error::Corrupt)?;
    }
    if nodes.active().is_some()
        || !queue.is_empty()
        || members
            .iter()
            .any(|m| m.outcome.is_none() || m.stack.is_some())
        || a.space.is_some()
        || b.space.is_some()
    {
        return Err(Error::Corrupt);
    }
    #[cfg(feature = "cap-test")]
    super::capabilities::finish().map_err(|_| Error::Corrupt)?;
    #[cfg(feature = "abi-02")]
    super::address_space::finish().map_err(|_| Error::Corrupt)?;
    let captures = preemptive::stop(paging)?;
    if paging.stats() != before || paging.table_count() != tables {
        return Err(Error::Corrupt);
    }
    serial::write(format_args!("[NEU] Multi accounting restored: allocated={} free={} tables={}\n[NEU] Multiple Thread test passed: dispatches={} captures={} queue=empty running=none IF=0\n",before.allocated,before.free,tables,dispatches,captures)).map_err(|_|Error::Corrupt)
}

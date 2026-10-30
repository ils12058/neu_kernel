//! K8C: runtime MemoryObject mapping from a received capability.
//!
//! Node A transfers one MemoryObject capability over the bootstrap Channel and
//! Node B maps the same physical backing at a scheduler safe point. The
//! controller never uses the ordinary mapping API for Node B: the only request
//! path is the private `TEST_MAP` (0xffff_ffff_ffff_fc01) fixture convention
//! documented below. Ring 3 command streams are written into each Node's own
//! DATA page by the controller; the kernel never writes the tested payload.
//!
//! `TEST_MAP(handle, base, pages, writable, offset)` is an exact-argument
//! syscall: it contains no physical address or object id, so it cannot
//! amplify rights. It only validates and records a bounded request; the
//! actual mapping is installed by `address_space::commit_request` after the
//! issuing Thread has been suspended, its capability table is leased, and its
//! root is not active. The safe point therefore guarantees: the CPU is at the
//! kernel root; the saved Ring 3 context is scheduler-owned; the destination
//! root has exactly the issuing Thread's execution pin; authority is
//! re-derived from the received handle slot/generation; and the mapping is
//! committed before the Thread is enqueued again. On one CPU cancellation
//! cannot interleave with the synchronous commit: both run between dispatches
//! with interrupts disabled.
use super::super::{
    address_space, memory_object_runtime as runtime, native,
    paging::{Paging, UserSpace},
    preemptive, serial,
};
use super::{
    Domain, Member, add_member, cap_table, check, create_live, install, live_destroy,
    neu_multi_syscall_resume, retire, runtime_fault_sites, site, space, space_mut,
};
use core::ptr;
use neu_kernel::{
    capability::{self as cap, Handle, Rights},
    context::{SavedContext, SliceEvent, StackBinding},
    node::{self, ExitReason, NodeState, Nodes},
    paging::{Error, MemoryObjectId, PAGE, Permissions, Result},
    scheduler::Queue,
    syscall::{Frame, v02, v05},
};

const OP_GATE: u64 = 0xFFFE;
const OP_READ: u64 = 0xFFFD;
const OP_WRITE: u64 = 0xFFFC;
const OP_STORE: u64 = 0xFFFB;
const EXIT: u64 = neu_kernel::syscall::THREAD_EXIT;
const READ_ONLY: u64 = 0;
const WRITABLE: u64 = 1;
const STALE: u64 = -17i64 as u64;
const FORGED: u64 = -16i64 as u64;
const TYPE: u64 = -19i64 as u64;
const INVALID: u64 = -2i64 as u64;
const RIGHTS: u64 = -18i64 as u64;
const IN_USE: u64 = -23i64 as u64;
const BAD_MAPPING: u64 = -24i64 as u64;
const OWNER: u64 = -21i64 as u64;

struct RuntimeState {
    stage: u64,
    mode: u64,
    order: u64,
    deferred: Option<(Frame, StackBinding)>,
    resumed: bool,
    received: u64,
    audits: [u64; 10],
}

struct Peers {
    h_mem: Handle,
    h_send: Handle,
    h_flag: Handle,
    endpoint: Handle,
    h_recv: Handle,
}

fn node_status(result: node::Result<()>) -> u64 {
    match result {
        Ok(()) => u64::MAX,
        Err(error) => error as u64,
    }
}

/// Returns a syscall status word, converting only genuine storage corruption
/// into a fail-stop. A recoverable capability error is data, not a crash.
fn outcome(result: cap::Result<i64>) -> Result<u64> {
    match result {
        Ok(status) => Ok(status as u64),
        Err(cap::Error::Object) => Err(Error::Corrupt),
        Err(error) => Ok(error as i64 as u64),
    }
}

fn unit(result: cap::Result<()>) -> Result<()> {
    result.map_err(|_| Error::Corrupt)
}

fn granted(result: cap::Result<Handle>) -> Result<Handle> {
    result.map_err(|_| Error::Corrupt)
}

fn ledger() -> Result<[u64; 48]> {
    address_space::request_diagnostics().map_err(|_| Error::Corrupt)
}

fn queued_capabilities() -> Result<u64> {
    let (_, _, _, queued, _) =
        address_space::capability_diagnostics().map_err(|_| Error::Corrupt)?;
    Ok(queued as u64)
}

fn record(op: u64, a: u64, b: u64, c: u64, d: u64, e: u64) -> [u64; 6] {
    [op, a, b, c, d, e]
}

fn write_script(
    paging: &mut Paging<'_, '_>,
    space: &UserSpace,
    base: usize,
    index: usize,
    words: [u64; 6],
) -> Result<()> {
    let at = base + index * runtime::RUNTIME_SCRIPT_STRIDE;
    for (i, word) in words.into_iter().enumerate() {
        paging.live_write(space, at + i * 8, word)?;
    }
    paging.live_write(space, at + runtime::RUNTIME_SCRIPT_STATUS, 0)?;
    paging.live_write(space, at + runtime::RUNTIME_SCRIPT_STATUS + 8, 0)
}

fn record_status(
    paging: &mut Paging<'_, '_>,
    space: &UserSpace,
    base: usize,
    index: usize,
) -> Result<u64> {
    paging.live_data_word(
        space,
        base + index * runtime::RUNTIME_SCRIPT_STRIDE + runtime::RUNTIME_SCRIPT_STATUS,
    )
}

fn block_word(
    paging: &mut Paging<'_, '_>,
    space: &UserSpace,
    block: u64,
    offset: usize,
) -> Result<u64> {
    paging.live_data_word(space, block as usize + offset)
}

fn block_write(
    paging: &mut Paging<'_, '_>,
    space: &UserSpace,
    block: u64,
    offset: usize,
    value: u64,
) -> Result<()> {
    paging.live_write(space, block as usize + offset, value)
}

fn p_word(paging: &mut Paging<'_, '_>, space: &UserSpace, offset: usize) -> Result<u64> {
    paging.live_data_word(space, runtime::RUNTIME_P_BASE as usize + offset)
}

fn p_write(
    paging: &mut Paging<'_, '_>,
    space: &UserSpace,
    offset: usize,
    value: u64,
) -> Result<()> {
    paging.live_write(space, runtime::RUNTIME_P_BASE as usize + offset, value)
}

fn expect_map(
    paging: &Paging<'_, '_>,
    space: &UserSpace,
    address: u64,
    object: MemoryObjectId,
    page: usize,
    write: bool,
) -> Result<()> {
    let physical = paging.live_physical(object, page)?;
    let Some(translation) = paging.user_translate(space, address)? else {
        return Err(Error::Corrupt);
    };
    if translation.physical != physical
        || translation.permissions.write != write
        || translation.permissions.execute
        || !translation.user
        || translation.page_size != PAGE
    {
        return Err(Error::Corrupt);
    }
    Ok(())
}

fn expect_unmapped(paging: &Paging<'_, '_>, space: &UserSpace, address: u64) -> Result<()> {
    match paging.user_translate(space, address)? {
        None => Ok(()),
        Some(_) => Err(Error::Corrupt),
    }
}

fn saved_frames(nodes: &Nodes, members: &[Member; 3]) -> Result<[Option<u64>; 3]> {
    Ok([
        nodes
            .live_saved_frame(members[0].active.thread)
            .map_err(|_| Error::Corrupt)?,
        nodes
            .live_saved_frame(members[1].active.thread)
            .map_err(|_| Error::Corrupt)?,
        nodes
            .live_saved_frame(members[2].active.thread)
            .map_err(|_| Error::Corrupt)?,
    ])
}

#[allow(clippy::too_many_arguments)]
fn publish_checkpoint(
    paging: &Paging<'_, '_>,
    nodes: &Nodes,
    state: &RuntimeState,
    object: Option<MemoryObjectId>,
    a: Option<&Domain>,
    b: Option<&Domain>,
    members: &[Member; 3],
    ids: &runtime::RuntimeIds,
) -> Result<()> {
    runtime::checkpoint(
        paging,
        state.stage,
        state.mode,
        object,
        a.and_then(|domain| domain.space.as_ref()),
        b.and_then(|domain| domain.space.as_ref()),
        saved_frames(nodes, members)?,
        ids,
        a.and_then(|domain| cap_table(nodes, domain.id)),
        b.and_then(|domain| cap_table(nodes, domain.id)),
        state.order,
        state.audits,
    )
}

fn retire_finished(
    paging: &mut Paging<'_, '_>,
    nodes: &mut Nodes,
    a: &mut Domain,
    b: &mut Domain,
    members: &mut [Member; 3],
    state: &RuntimeState,
) -> Result<()> {
    if a.space.is_some() {
        for member in members.iter_mut().take(1) {
            if member.stack.is_some() && member.outcome.is_some() {
                retire(paging, nodes, a, member)?;
            }
        }
    }
    if b.space.is_some() {
        for member in members.iter_mut().skip(1) {
            // The canceled requester keeps its stack until the controller has
            // proven that its terminal continuation can no longer be resumed.
            if state.mode == runtime::RUNTIME_CANCEL && member.label == 3 {
                continue;
            }
            if member.stack.is_some() && member.outcome.is_some() {
                retire(paging, nodes, b, member)?;
            }
        }
    }
    Ok(())
}

fn expect_peer_done(paging: &mut Paging<'_, '_>, nodes: &Nodes, domain: &Domain) -> Result<()> {
    let space = space(domain)?;
    if check(nodes.node(domain.id))?.state != NodeState::Exited {
        return Err(Error::Corrupt);
    }
    if block_word(
        paging,
        space,
        runtime::RUNTIME_BLOCK_A,
        runtime::RUNTIME_RESULT_WORD,
    )? != runtime::RUNTIME_RESULT
        || block_word(
            paging,
            space,
            runtime::RUNTIME_BLOCK_A,
            runtime::RUNTIME_READBACK,
        )? != runtime::RUNTIME_VALUE
        || block_word(
            paging,
            space,
            runtime::RUNTIME_BLOCK_A,
            runtime::RUNTIME_SECOND_WORD,
        )? != runtime::RUNTIME_SECOND
    {
        return Err(Error::Corrupt);
    }
    Ok(())
}

/// Cancellation proof. The second Thread of the destination Node faults while
/// the received-capability request is pending; the controller must cancel the
/// ledger entry, refuse every later commit and resume, and only then retire the
/// suspended requester.
#[allow(clippy::too_many_arguments)]
fn cancel_stage7(
    paging: &mut Paging<'_, '_>,
    nodes: &mut Nodes,
    queue: &mut Queue,
    a: &Domain,
    b: &mut Domain,
    members: &mut [Member; 3],
    state: &mut RuntimeState,
    object: MemoryObjectId,
) -> Result<bool> {
    if state.resumed {
        return Ok(true);
    }
    if members[1].outcome.is_none() || members[2].outcome.is_none() {
        return Ok(false);
    }
    if a.space.is_some() || members[1].stack.is_none() {
        return Err(Error::Corrupt);
    }
    let ExitReason::Cancelled { by } = members[1].outcome.ok_or(Error::Corrupt)? else {
        return Err(Error::Corrupt);
    };
    if by != members[2].active.thread || check(nodes.node(b.id))?.state != NodeState::Exited {
        return Err(Error::Corrupt);
    }
    let sites = runtime_fault_sites();
    fault(
        14,
        6,
        runtime::RUNTIME_VA_B2_FAULT,
        sites[1],
        members[2].active.context.rsp - 64,
        members[2].outcome.ok_or(Error::Corrupt)?,
    )?;
    let space_b = space(b)?;
    if record_status(paging, space_b, runtime::RUNTIME_SCRIPT_B2, 1)? != IN_USE
        || block_word(
            paging,
            space_b,
            runtime::RUNTIME_BLOCK_B2,
            runtime::RUNTIME_GATE,
        )? != 0
        || block_word(
            paging,
            space_b,
            runtime::RUNTIME_BLOCK_B2,
            runtime::RUNTIME_READY,
        )? != 1
        || block_word(
            paging,
            space_b,
            runtime::RUNTIME_BLOCK_B,
            runtime::RUNTIME_READY,
        )? != 2
    {
        return Err(Error::Corrupt);
    }
    expect_unmapped(paging, space_b, runtime::RUNTIME_VA_B)?;
    let diagnostics = ledger()?;
    if diagnostics[40] != 1
        || diagnostics[42] != 1
        || diagnostics[0] != 1
        || diagnostics[8] != object.raw()
        || diagnostics[15] != 0
        || diagnostics[16] != 0
        || paging.live_counts(object)? != (0, 0, 2)
        || paging.memory_object_capability_refs(object)? != 1
    {
        return Err(Error::Corrupt);
    }
    unit(address_space::cancel_request(nodes, members[1].active, 1))?;
    let diagnostics = ledger()?;
    if diagnostics[41] != 1
        || diagnostics[42] != 0
        || diagnostics[43] != 1
        || diagnostics[20] != 1
        || diagnostics[35] != 3
        || diagnostics[36] != 0
    {
        return Err(Error::Corrupt);
    }
    state.audits[0] = outcome(address_space::commit_request(
        nodes,
        paging,
        space_mut(b)?,
        members[1].active,
        1,
    ))?;
    state.audits[7] = outcome(address_space::commit_request(
        nodes,
        paging,
        space_mut(b)?,
        members[1].active,
        1,
    ))?;
    let (frame, binding) = state.deferred.ok_or(Error::Corrupt)?;
    let continuation = SavedContext::continuation(members[1].active, binding, frame);
    state.audits[9] = node_status(queue.resume(nodes, members[1].active, continuation));
    if state.audits[0] != OWNER
        || state.audits[7] != OWNER
        || state.audits[9] != 4
        || queue.contains(members[1].active.thread)
    {
        return Err(Error::Corrupt);
    }
    state.deferred = None;
    state.resumed = true;
    retire(paging, nodes, b, &mut members[1])?;
    p_write(
        paging,
        space(b)?,
        runtime::RUNTIME_P_REPLAY,
        state.audits[0],
    )?;
    p_write(paging, space(b)?, runtime::RUNTIME_P_DONE, 1)?;
    destroy(paging, nodes, b)?;
    Ok(true)
}

fn expected_request(mode: u64, sequence: u64) -> Result<(u64, u64, bool, u64)> {
    match (mode, sequence) {
        (runtime::RUNTIME_GOLDEN, 1..=3) => Ok((runtime::RUNTIME_VA_B, 1, false, 0)),
        (runtime::RUNTIME_WRITABLE, 1) => Ok((runtime::RUNTIME_VA_B, 1, false, 0)),
        (runtime::RUNTIME_WRITABLE, 2) => Ok((runtime::RUNTIME_VA_B_WRITE, 1, true, 1)),
        (runtime::RUNTIME_FAULT, 1)
        | (runtime::RUNTIME_PEER_EXIT, 1)
        | (runtime::RUNTIME_CANCEL, 1) => Ok((runtime::RUNTIME_VA_B, 1, false, 0)),
        _ => Err(Error::Corrupt),
    }
}

fn fault(
    expected_vector: u64,
    expected_error: u64,
    expected_cr2: u64,
    expected_rip: u64,
    expected_rsp: u64,
    reason: ExitReason,
) -> Result<()> {
    let ExitReason::UserFault(fault) = reason else {
        return Err(Error::Corrupt);
    };
    if fault.source != node::FaultSource::Cpu
        || fault.vector != expected_vector
        || fault.error != expected_error
        || fault.cr2 != expected_cr2
        || fault.rip != expected_rip
        || fault.rsp != expected_rsp
        || fault.cs != neu_kernel::syscall::USER_CS
        || fault.ss != neu_kernel::syscall::USER_SS
        || fault.flags & 0x200 == 0
    {
        return Err(Error::Corrupt);
    }
    Ok(())
}

fn destroy(paging: &mut Paging<'_, '_>, nodes: &mut Nodes, domain: &mut Domain) -> Result<()> {
    if check(nodes.node(domain.id))?.state != NodeState::Exited {
        return Err(Error::Corrupt);
    }
    unit(address_space::cleanup(nodes, domain.id, paging))?;
    live_destroy(paging, nodes, domain)
}

/// Controller commit at the safe point. The Thread is not in Ring 3, the root
/// is inactive, and the only execution pin belongs to the suspended Thread.
#[allow(clippy::too_many_arguments)]
fn requested(
    paging: &mut Paging<'_, '_>,
    nodes: &mut Nodes,
    queue: &mut Queue,
    a: &mut Domain,
    b: &mut Domain,
    members: &mut [Member; 3],
    state: &mut RuntimeState,
    index: usize,
    frame: Frame,
    binding: StackBinding,
    h_recv: Handle,
    object: MemoryObjectId,
) -> Result<(Frame, bool)> {
    if index != 1
        || state.mode == runtime::RUNTIME_RIGHTS
        || state.mode == runtime::RUNTIME_READ_DENIED
    {
        return Err(Error::Corrupt);
    }
    let diagnostics = ledger()?;
    if diagnostics[42] != 1 {
        return Err(Error::Corrupt);
    }
    let sequence = diagnostics[0];
    let (base, pages, writable, offset) = expected_request(state.mode, sequence)?;
    let table = check(nodes.authority(b.id))?.id().raw();
    let space_b = space(b)?;
    if diagnostics[1] != b.id.raw()
        || diagnostics[2] != members[1].active.thread.raw()
        || diagnostics[3] != table
        || diagnostics[4] != space_b.id().raw()
        || diagnostics[5] != space_b.root
        || diagnostics[6] != h_recv.raw()
        || diagnostics[7] == 0
        || diagnostics[8] != object.raw()
        || diagnostics[9] != base
        || diagnostics[10] != offset
        || diagnostics[11] != pages
        || diagnostics[12] != u64::from(writable)
        || diagnostics[13] != 0
        || diagnostics[14] != u64::from(writable)
        || diagnostics[15] != 0
        || diagnostics[16] != 0
        || frame.context.rip != site(ptr::addr_of!(neu_multi_syscall_resume))
    {
        return Err(Error::Corrupt);
    }
    let active = members[1].active;
    // The Thread is still Running, so the commit gate must refuse: no
    // destination root may be touched while Ring 3 owns it.
    state.audits[5] = outcome(address_space::commit_request(
        nodes,
        paging,
        space_mut(b)?,
        active,
        sequence,
    ))?;
    if state.audits[5] != OWNER {
        return Err(Error::Corrupt);
    }
    check(queue.request(
        nodes,
        active,
        SavedContext::continuation(active, binding, frame),
    ))?;
    if members[1].active != active || members[1].stack != Some(binding) {
        return Err(Error::Corrupt);
    }
    if sequence == 1 {
        state.audits[4] = outcome(address_space::commit_request(
            nodes,
            paging,
            space_mut(b)?,
            active,
            sequence + 1,
        ))?;
        state.audits[2] = outcome(address_space::commit_request(
            nodes,
            paging,
            space_mut(a)?,
            active,
            sequence,
        ))?;
        let root = {
            let space = space_mut(b)?;
            let root = space.root;
            space.root = root + PAGE;
            root
        };
        let wrong_root =
            address_space::commit_request(nodes, paging, space_mut(b)?, active, sequence);
        space_mut(b)?.root = root;
        state.audits[3] = outcome(wrong_root)?;
        state.audits[8] = outcome(address_space::commit_request(
            nodes,
            paging,
            space_mut(b)?,
            members[0].active,
            sequence,
        ))?;
        if state.audits[4] != IN_USE
            || state.audits[2] != OWNER
            || state.audits[3] != OWNER
            || state.audits[8] != OWNER
        {
            return Err(Error::Corrupt);
        }
    }
    if state.mode == runtime::RUNTIME_PEER_EXIT || state.mode == runtime::RUNTIME_CANCEL {
        if state.deferred.is_some() {
            return Err(Error::Corrupt);
        }
        state.deferred = Some((frame, binding));
        return Ok((frame, false));
    }
    if state.mode == runtime::RUNTIME_GOLDEN && sequence == 3 {
        // Close the received handle after admission but before authorization:
        // the commit must re-derive the slot/generation and fail closed.
        unit(address_space::close_handle(nodes, b.id, h_recv))?;
    }
    let status = outcome(address_space::commit_request(
        nodes,
        paging,
        space_mut(b)?,
        active,
        sequence,
    ))?;
    if sequence == 1 && status != 0 {
        return Err(Error::Corrupt);
    }
    state.audits[0] = outcome(address_space::commit_request(
        nodes,
        paging,
        space_mut(b)?,
        active,
        sequence,
    ))?;
    if state.audits[0] != OWNER {
        return Err(Error::Corrupt);
    }
    let mut frame = frame;
    frame.registers.rax = status;
    frame.registers.rdx = 0;
    let continuation = SavedContext::continuation(active, binding, frame);
    check(queue.resume(nodes, active, continuation))?;
    state.audits[1] = node_status(queue.resume(nodes, active, continuation));
    if state.audits[1] != 2 {
        return Err(Error::Corrupt);
    }
    Ok((frame, true))
}

#[allow(clippy::too_many_arguments)]
fn progress(
    paging: &mut Paging<'_, '_>,
    nodes: &mut Nodes,
    queue: &mut Queue,
    a: &mut Domain,
    b: &mut Domain,
    members: &mut [Member; 3],
    state: &mut RuntimeState,
    object: MemoryObjectId,
    ids: &runtime::RuntimeIds,
    peers: &Peers,
) -> Result<()> {
    loop {
        // A fault in the second Node B Thread exits the suspended requester in
        // the node model without a dispatch; adopt that outcome so the
        // cancellation proof observes it before the stack is retired.
        if state.mode == runtime::RUNTIME_CANCEL && members[1].outcome.is_none() {
            let thread = check(nodes.thread(members[1].active.thread))?;
            if let Some(reason) = thread.outcome {
                members[1].outcome = Some(reason);
                members[1].last = None;
            }
        }
        retire_finished(paging, nodes, a, b, members, state)?;
        let stage = state.stage;
        match stage {
            2 => {
                let space_a = space(a)?;
                if block_word(
                    paging,
                    space_a,
                    runtime::RUNTIME_BLOCK_A,
                    runtime::RUNTIME_READY,
                )? != 1
                {
                    break;
                }
                if block_word(
                    paging,
                    space_a,
                    runtime::RUNTIME_BLOCK_A,
                    runtime::RUNTIME_MEM_HANDLE,
                )? != peers.h_mem.raw()
                {
                    return Err(Error::Corrupt);
                }
                expect_map(paging, space_a, runtime::RUNTIME_VA_A, object, 0, true)?;
                expect_map(
                    paging,
                    space_a,
                    runtime::RUNTIME_VA_A + PAGE,
                    object,
                    1,
                    true,
                )?;
                if paging.live_object_word(object, 0, 0)? != runtime::RUNTIME_VALUE
                    || paging.live_object_word(object, 0, 8)? != runtime::RUNTIME_SECOND
                    || paging.live_object_word(object, 1, 0)? != 1
                {
                    return Err(Error::Corrupt);
                }
                expect_unmapped(paging, space(b)?, runtime::RUNTIME_VA_B)?;
                if ledger()?[42] != 0
                    || paging.live_counts(object)? != (0, 2, 2)
                    || paging.memory_object_capability_refs(object)? != 1
                {
                    return Err(Error::Corrupt);
                }
                state.stage = 3;
                publish_checkpoint(
                    paging,
                    nodes,
                    state,
                    Some(object),
                    Some(a),
                    Some(b),
                    members,
                    ids,
                )?;
                block_write(
                    paging,
                    space(a)?,
                    runtime::RUNTIME_BLOCK_A,
                    runtime::RUNTIME_GATE,
                    1,
                )?;
            }
            3 => {
                let space_a = space(a)?;
                if block_word(
                    paging,
                    space_a,
                    runtime::RUNTIME_BLOCK_A,
                    runtime::RUNTIME_READY,
                )? != 2
                    || block_word(
                        paging,
                        space_a,
                        runtime::RUNTIME_BLOCK_A,
                        runtime::RUNTIME_MAIN_STATUS,
                    )? != 0
                    || address_space::handle_status(nodes, a.id, peers.h_mem)
                        .map_err(|_| Error::Corrupt)?
                        != STALE as i64
                {
                    break;
                }
                if queued_capabilities()? != 1
                    || block_word(
                        paging,
                        space(b)?,
                        runtime::RUNTIME_BLOCK_B,
                        runtime::RUNTIME_RECEIVED,
                    )? != 0
                    || ledger()?[40] != 0
                    || ledger()?[42] != 0
                    || paging.live_counts(object)? != (0, 2, 2)
                    || paging.memory_object_capability_refs(object)? != 1
                {
                    return Err(Error::Corrupt);
                }
                expect_unmapped(paging, space(b)?, runtime::RUNTIME_VA_B)?;
                expect_map(paging, space_a, runtime::RUNTIME_VA_A, object, 0, true)?;
                expect_map(
                    paging,
                    space_a,
                    runtime::RUNTIME_VA_A + PAGE,
                    object,
                    1,
                    true,
                )?;
                state.stage = 4;
                publish_checkpoint(
                    paging,
                    nodes,
                    state,
                    Some(object),
                    Some(a),
                    Some(b),
                    members,
                    ids,
                )?;
                block_write(
                    paging,
                    space(b)?,
                    runtime::RUNTIME_BLOCK_B,
                    runtime::RUNTIME_GATE,
                    1,
                )?;
            }
            4 => {
                let space_b = space(b)?;
                if block_word(
                    paging,
                    space_b,
                    runtime::RUNTIME_BLOCK_B,
                    runtime::RUNTIME_READY,
                )? != 2
                {
                    break;
                }
                let received = block_word(
                    paging,
                    space_b,
                    runtime::RUNTIME_BLOCK_B,
                    runtime::RUNTIME_RECEIVED,
                )?;
                if received != peers.h_recv.raw() {
                    return Err(Error::Corrupt);
                }
                state.received = received;
                let base = runtime::RUNTIME_SCRIPT_B;
                let stale = record_status(paging, space_b, base, 1)?;
                let forged = record_status(paging, space_b, base, 3)?;
                let duplicate = record_status(paging, space_b, base, 4)?;
                let object_type = record_status(paging, space_b, base, 5)?;
                let range = record_status(paging, space_b, base, 6)?;
                let unaligned = record_status(paging, space_b, base, 7)?;
                let pages = record_status(paging, space_b, base, 8)?;
                let offset = record_status(paging, space_b, base, 9)?;
                let generation = record_status(paging, space_b, base, 10)?;
                let receive = record_status(paging, space_b, base, 2)?;
                if receive != 0
                    || stale != STALE
                    || forged != FORGED
                    || duplicate != STALE
                    || object_type != TYPE
                    || range != INVALID
                    || unaligned != INVALID
                    || pages != INVALID
                    || offset != INVALID
                    || generation != STALE
                {
                    return Err(Error::Corrupt);
                }
                p_write(paging, space_b, runtime::RUNTIME_P_STALE, stale)?;
                p_write(paging, space_b, runtime::RUNTIME_P_FORGED, forged)?;
                p_write(paging, space_b, runtime::RUNTIME_P_TYPE, object_type)?;
                p_write(paging, space_b, runtime::RUNTIME_P_RANGE, range)?;
                p_write(paging, space_b, runtime::RUNTIME_P_PAGES, pages)?;
                p_write(paging, space_b, runtime::RUNTIME_P_OFFSET, offset)?;
                p_write(paging, space_b, runtime::RUNTIME_P_GENERATION, generation)?;
                p_write(paging, space_b, runtime::RUNTIME_P_UNALIGNED, unaligned)?;
                let diagnostics = ledger()?;
                if diagnostics[40] != 0
                    || diagnostics[42] != 0
                    || diagnostics[43] != 0
                    || queued_capabilities()? != 0
                    || paging.live_counts(object)? != (0, 2, 2)
                    || paging.memory_object_capability_refs(object)? != 1
                {
                    return Err(Error::Corrupt);
                }
                expect_unmapped(paging, space_b, runtime::RUNTIME_VA_B)?;
                expect_unmapped(paging, space_b, runtime::RUNTIME_VA_B_WRITE)?;
                expect_map(paging, space(a)?, runtime::RUNTIME_VA_A, object, 0, true)?;
                expect_map(
                    paging,
                    space(a)?,
                    runtime::RUNTIME_VA_A + PAGE,
                    object,
                    1,
                    true,
                )?;
                state.stage = 5;
                publish_checkpoint(
                    paging,
                    nodes,
                    state,
                    Some(object),
                    Some(a),
                    Some(b),
                    members,
                    ids,
                )?;
                block_write(
                    paging,
                    space(a)?,
                    runtime::RUNTIME_BLOCK_A,
                    runtime::RUNTIME_GATE_SECOND,
                    1,
                )?;
                block_write(
                    paging,
                    space(b)?,
                    runtime::RUNTIME_BLOCK_B,
                    runtime::RUNTIME_GATE_SECOND,
                    1,
                )?;
            }
            5 => {
                let space_b = space(b)?;
                let base = runtime::RUNTIME_SCRIPT_B;
                match state.mode {
                    runtime::RUNTIME_GOLDEN => {
                        if check(nodes.node(a.id))?.state != NodeState::Exited
                            || block_word(
                                paging,
                                space_b,
                                runtime::RUNTIME_BLOCK_B,
                                runtime::RUNTIME_READY,
                            )? != 3
                        {
                            break;
                        }
                        expect_peer_done(paging, nodes, a)?;
                        expect_map(paging, space(a)?, runtime::RUNTIME_VA_A, object, 0, true)?;
                        expect_map(
                            paging,
                            space(a)?,
                            runtime::RUNTIME_VA_A + PAGE,
                            object,
                            1,
                            true,
                        )?;
                        expect_map(paging, space_b, runtime::RUNTIME_VA_B, object, 0, false)?;
                        expect_unmapped(paging, space_b, runtime::RUNTIME_VA_B_WRITE)?;
                        let readback = block_word(
                            paging,
                            space_b,
                            runtime::RUNTIME_BLOCK_B,
                            runtime::RUNTIME_READBACK,
                        )?;
                        let second = block_word(
                            paging,
                            space_b,
                            runtime::RUNTIME_BLOCK_B,
                            runtime::RUNTIME_STALE_STATUS,
                        )?;
                        let main = block_word(
                            paging,
                            space_b,
                            runtime::RUNTIME_BLOCK_B,
                            runtime::RUNTIME_MAIN_STATUS,
                        )?;
                        let close = block_word(
                            paging,
                            space_b,
                            runtime::RUNTIME_BLOCK_B,
                            runtime::RUNTIME_CLOSE_STATUS,
                        )?;
                        let primary = record_status(paging, space_b, base, 12)?;
                        let write = record_status(paging, space_b, base, 15)?;
                        let overlap = record_status(paging, space_b, base, 16)?;
                        let replayed = record_status(paging, space_b, base, 17)?;
                        let closed = record_status(paging, space_b, base, 18)?;
                        let after = record_status(paging, space_b, base, 19)?;
                        if readback != runtime::RUNTIME_VALUE
                            || second != runtime::RUNTIME_SECOND
                            || main != runtime::RUNTIME_VALUE
                            || close != runtime::RUNTIME_SECOND
                            || primary != 0
                            || write != RIGHTS
                            || overlap != BAD_MAPPING
                            || replayed != STALE
                            || closed != STALE
                            || after != STALE
                        {
                            serial::write(format_args!(
                                "[NEU] Runtime golden mismatch: readback={:#x} second={:#x} main={:#x} close={:#x} primary={} write={} overlap={} replayed={} closed={} after={}\n",
                                readback, second, main, close, primary, write, overlap, replayed,
                                closed, after
                            ))
                            .map_err(|_| Error::Corrupt)?;
                            return Err(Error::Corrupt);
                        }
                        p_write(paging, space_b, runtime::RUNTIME_P_OVERLAP, overlap)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_WRITE, write)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_CLOSE, closed)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_AFTER, after)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_REPLAY, replayed)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_MAIN, primary)?;
                        let diagnostics = ledger()?;
                        if diagnostics[40] != 3
                            || diagnostics[42] != 0
                            || diagnostics[43] != 1
                            || diagnostics[20] != 3
                            || diagnostics[28] != object.raw()
                            || diagnostics[35] != 2
                            || diagnostics[36] != STALE
                            || paging.live_counts(object)? != (0, 3, 2)
                            || paging.memory_object_capability_refs(object)? != 0
                        {
                            serial::write(format_args!(
                                "[NEU] Runtime golden ledger mismatch: requests={} pending={} last={} seq={} object={:#x} state={} status={} counts={:?} refs={}\n",
                                diagnostics[40], diagnostics[42], diagnostics[43], diagnostics[20],
                                diagnostics[28], diagnostics[35], diagnostics[36],
                                paging.live_counts(object)?,
                                paging.memory_object_capability_refs(object)?
                            ))
                            .map_err(|_| Error::Corrupt)?;
                            return Err(Error::Corrupt);
                        }
                        state.audits[7] = outcome(address_space::commit_request(
                            nodes,
                            paging,
                            space_mut(b)?,
                            members[1].active,
                            3,
                        ))?;
                        if state.audits[7] != OWNER {
                            return Err(Error::Corrupt);
                        }
                        let space_b = space(b)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_DONE, 1)?;
                        state.stage = 6;
                        publish_checkpoint(
                            paging,
                            nodes,
                            state,
                            Some(object),
                            Some(a),
                            Some(b),
                            members,
                            ids,
                        )?;
                        block_write(
                            paging,
                            space_b,
                            runtime::RUNTIME_BLOCK_B,
                            runtime::RUNTIME_GATE_THIRD,
                            1,
                        )?;
                    }
                    runtime::RUNTIME_WRITABLE => {
                        if check(nodes.node(a.id))?.state != NodeState::Exited
                            || block_word(
                                paging,
                                space_b,
                                runtime::RUNTIME_BLOCK_B,
                                runtime::RUNTIME_READY,
                            )? != 3
                        {
                            break;
                        }
                        expect_peer_done(paging, nodes, a)?;
                        expect_map(paging, space(a)?, runtime::RUNTIME_VA_A, object, 0, true)?;
                        expect_map(
                            paging,
                            space(a)?,
                            runtime::RUNTIME_VA_A + PAGE,
                            object,
                            1,
                            true,
                        )?;
                        expect_map(paging, space_b, runtime::RUNTIME_VA_B, object, 0, false)?;
                        expect_map(
                            paging,
                            space_b,
                            runtime::RUNTIME_VA_B_WRITE,
                            object,
                            1,
                            true,
                        )?;
                        let readback = block_word(
                            paging,
                            space_b,
                            runtime::RUNTIME_BLOCK_B,
                            runtime::RUNTIME_READBACK,
                        )?;
                        let after = block_word(
                            paging,
                            space_b,
                            runtime::RUNTIME_BLOCK_B,
                            runtime::RUNTIME_AFTER_STATUS,
                        )?;
                        let primary = record_status(paging, space_b, base, 12)?;
                        let writable = record_status(paging, space_b, base, 14)?;
                        let store = record_status(paging, space_b, base, 15)?;
                        if readback != runtime::RUNTIME_VALUE
                            || after != runtime::RUNTIME_WRITE
                            || primary != 0
                            || writable != 0
                            || store != 0
                            || paging.live_object_word(object, 1, 0)? != runtime::RUNTIME_WRITE
                        {
                            return Err(Error::Corrupt);
                        }
                        p_write(paging, space_b, runtime::RUNTIME_P_OVERLAP, 0)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_WRITE, writable)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_CLOSE, 0)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_AFTER, 0)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_REPLAY, state.audits[0])?;
                        p_write(paging, space_b, runtime::RUNTIME_P_MAIN, primary)?;
                        let diagnostics = ledger()?;
                        if diagnostics[40] != 2
                            || diagnostics[42] != 0
                            || diagnostics[43] != 1
                            || diagnostics[20] != 2
                            || diagnostics[28] != object.raw()
                            || diagnostics[35] != 1
                            || diagnostics[36] != 0
                            || paging.live_counts(object)? != (0, 4, 2)
                            || paging.memory_object_capability_refs(object)? != 1
                        {
                            return Err(Error::Corrupt);
                        }
                        state.audits[7] = outcome(address_space::commit_request(
                            nodes,
                            paging,
                            space_mut(b)?,
                            members[1].active,
                            2,
                        ))?;
                        if state.audits[7] != OWNER {
                            return Err(Error::Corrupt);
                        }
                        let space_b = space(b)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_DONE, 1)?;
                        state.stage = 6;
                        publish_checkpoint(
                            paging,
                            nodes,
                            state,
                            Some(object),
                            Some(a),
                            Some(b),
                            members,
                            ids,
                        )?;
                        block_write(
                            paging,
                            space_b,
                            runtime::RUNTIME_BLOCK_B,
                            runtime::RUNTIME_GATE_THIRD,
                            1,
                        )?;
                    }
                    runtime::RUNTIME_RIGHTS | runtime::RUNTIME_READ_DENIED => {
                        if check(nodes.node(a.id))?.state != NodeState::Exited
                            || block_word(
                                paging,
                                space_b,
                                runtime::RUNTIME_BLOCK_B,
                                runtime::RUNTIME_READY,
                            )? != 3
                        {
                            break;
                        }
                        expect_peer_done(paging, nodes, a)?;
                        let primary = record_status(paging, space_b, base, 12)?;
                        let writable = record_status(paging, space_b, base, 13)?;
                        if primary != RIGHTS || writable != RIGHTS {
                            return Err(Error::Corrupt);
                        }
                        expect_unmapped(paging, space_b, runtime::RUNTIME_VA_B)?;
                        expect_unmapped(paging, space_b, runtime::RUNTIME_VA_B_WRITE)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_OVERLAP, 0)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_WRITE, primary)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_CLOSE, 0)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_AFTER, 0)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_REPLAY, 0)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_MAIN, primary)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_DENIED_READ, primary)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_DENIED_WRITE, writable)?;
                        let diagnostics = ledger()?;
                        if diagnostics[40] != 0
                            || diagnostics[42] != 0
                            || diagnostics[43] != 0
                            || paging.live_counts(object)? != (0, 2, 2)
                            || paging.memory_object_capability_refs(object)? != 1
                        {
                            return Err(Error::Corrupt);
                        }
                        state.audits[7] = outcome(address_space::commit_request(
                            nodes,
                            paging,
                            space_mut(b)?,
                            members[1].active,
                            1,
                        ))?;
                        if state.audits[7] != OWNER {
                            return Err(Error::Corrupt);
                        }
                        let space_b = space(b)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_DONE, 1)?;
                        state.stage = 6;
                        publish_checkpoint(
                            paging,
                            nodes,
                            state,
                            Some(object),
                            Some(a),
                            Some(b),
                            members,
                            ids,
                        )?;
                        block_write(
                            paging,
                            space_b,
                            runtime::RUNTIME_BLOCK_B,
                            runtime::RUNTIME_GATE_THIRD,
                            1,
                        )?;
                    }
                    runtime::RUNTIME_FAULT => {
                        if members[0].outcome.is_none() || members[1].outcome.is_none() {
                            break;
                        }
                        expect_peer_done(paging, nodes, a)?;
                        let sites = runtime_fault_sites();
                        fault(
                            14,
                            7,
                            runtime::RUNTIME_VA_B,
                            sites[0],
                            members[1].active.context.rsp - 64,
                            members[1].outcome.ok_or(Error::Corrupt)?,
                        )?;
                        if check(nodes.node(b.id))?.state != NodeState::Exited
                            || paging.live_object_word(object, 0, 0)? != runtime::RUNTIME_VALUE
                            || paging.live_object_word(object, 0, 8)? != runtime::RUNTIME_SECOND
                            || paging.live_object_word(object, 1, 0)? != 1
                        {
                            return Err(Error::Corrupt);
                        }
                        expect_map(paging, space(a)?, runtime::RUNTIME_VA_A, object, 0, true)?;
                        expect_map(
                            paging,
                            space(a)?,
                            runtime::RUNTIME_VA_A + PAGE,
                            object,
                            1,
                            true,
                        )?;
                        let readback = block_word(
                            paging,
                            space_b,
                            runtime::RUNTIME_BLOCK_B,
                            runtime::RUNTIME_READBACK,
                        )?;
                        let second = block_word(
                            paging,
                            space_b,
                            runtime::RUNTIME_BLOCK_B,
                            runtime::RUNTIME_STALE_STATUS,
                        )?;
                        let primary = record_status(paging, space_b, base, 12)?;
                        if readback != runtime::RUNTIME_VALUE
                            || second != runtime::RUNTIME_SECOND
                            || primary != 0
                        {
                            return Err(Error::Corrupt);
                        }
                        p_write(paging, space_b, runtime::RUNTIME_P_OVERLAP, 0)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_WRITE, 0)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_CLOSE, 0)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_AFTER, 0)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_REPLAY, state.audits[0])?;
                        p_write(paging, space_b, runtime::RUNTIME_P_MAIN, primary)?;
                        let diagnostics = ledger()?;
                        if diagnostics[40] != 1
                            || diagnostics[42] != 0
                            || diagnostics[43] != 1
                            || diagnostics[20] != 1
                            || diagnostics[35] != 1
                            || diagnostics[36] != 0
                            || paging.live_counts(object)? != (0, 3, 2)
                            || paging.memory_object_capability_refs(object)? != 1
                        {
                            return Err(Error::Corrupt);
                        }
                        state.audits[7] = outcome(address_space::commit_request(
                            nodes,
                            paging,
                            space_mut(b)?,
                            members[1].active,
                            1,
                        ))?;
                        if state.audits[7] != OWNER {
                            return Err(Error::Corrupt);
                        }
                        let space_b = space(b)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_DONE, 1)?;
                        state.stage = 6;
                        publish_checkpoint(
                            paging,
                            nodes,
                            state,
                            Some(object),
                            Some(a),
                            Some(b),
                            members,
                            ids,
                        )?;
                    }
                    runtime::RUNTIME_PEER_EXIT => {
                        if members[0].outcome.is_none() || state.deferred.is_none() {
                            break;
                        }
                        expect_peer_done(paging, nodes, a)?;
                        expect_unmapped(paging, space_b, runtime::RUNTIME_VA_B)?;
                        let diagnostics = ledger()?;
                        if diagnostics[40] != 1
                            || diagnostics[42] != 1
                            || diagnostics[0] != 1
                            || diagnostics[8] != object.raw()
                            || diagnostics[15] != 0
                            || diagnostics[16] != 0
                            || paging.live_counts(object)? != (0, 2, 2)
                            || paging.memory_object_capability_refs(object)? != 1
                        {
                            return Err(Error::Corrupt);
                        }
                        p_write(paging, space_b, runtime::RUNTIME_P_OVERLAP, 0)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_WRITE, 0)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_CLOSE, 0)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_AFTER, 0)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_REPLAY, 0)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_MAIN, 0)?;
                        state.stage = 6;
                        publish_checkpoint(
                            paging,
                            nodes,
                            state,
                            Some(object),
                            Some(a),
                            Some(b),
                            members,
                            ids,
                        )?;
                    }
                    runtime::RUNTIME_CANCEL => {
                        if members[0].outcome.is_none() || state.deferred.is_none() {
                            break;
                        }
                        if members[1].outcome.is_some() || members[2].stack.is_some() {
                            return Err(Error::Corrupt);
                        }
                        expect_peer_done(paging, nodes, a)?;
                        expect_map(paging, space(a)?, runtime::RUNTIME_VA_A, object, 0, true)?;
                        expect_map(
                            paging,
                            space(a)?,
                            runtime::RUNTIME_VA_A + PAGE,
                            object,
                            1,
                            true,
                        )?;
                        expect_unmapped(paging, space_b, runtime::RUNTIME_VA_B)?;
                        let diagnostics = ledger()?;
                        if diagnostics[40] != 1
                            || diagnostics[42] != 1
                            || diagnostics[0] != 1
                            || diagnostics[8] != object.raw()
                            || diagnostics[15] != 0
                            || diagnostics[16] != 0
                            || paging.live_counts(object)? != (0, 2, 2)
                            || paging.memory_object_capability_refs(object)? != 1
                        {
                            return Err(Error::Corrupt);
                        }
                        p_write(paging, space_b, runtime::RUNTIME_P_OVERLAP, 0)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_WRITE, 0)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_CLOSE, 0)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_AFTER, 0)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_REPLAY, 0)?;
                        p_write(paging, space_b, runtime::RUNTIME_P_MAIN, 0)?;
                        state.stage = 6;
                        publish_checkpoint(
                            paging,
                            nodes,
                            state,
                            Some(object),
                            Some(a),
                            Some(b),
                            members,
                            ids,
                        )?;
                    }
                    _ => return Err(Error::Corrupt),
                }
            }
            6 => {
                if state.order == 1 {
                    if check(nodes.node(a.id))?.state != NodeState::Exited {
                        break;
                    }
                    destroy(paging, nodes, a)?;
                    if state.mode == runtime::RUNTIME_CANCEL {
                        if members[2].stack.is_some() || ledger()?[42] != 1 {
                            return Err(Error::Corrupt);
                        }
                        // Node A's space is retired, so the third Thread fits
                        // the fixed data-frame pool. Its fault must arrive
                        // while the received-capability request is pending.
                        install(paging, nodes, queue, b, &mut members[2], 22)?;
                        block_write(
                            paging,
                            space(b)?,
                            runtime::RUNTIME_BLOCK_B2,
                            runtime::RUNTIME_RECEIVED,
                            state.received,
                        )?;
                        block_write(
                            paging,
                            space(b)?,
                            runtime::RUNTIME_BLOCK_B2,
                            runtime::RUNTIME_GATE,
                            1,
                        )?;
                    }
                } else {
                    if check(nodes.node(b.id))?.state != NodeState::Exited {
                        break;
                    }
                    destroy(paging, nodes, b)?;
                }
                state.stage = 7;
                publish_checkpoint(
                    paging,
                    nodes,
                    state,
                    Some(object),
                    Some(a),
                    Some(b),
                    members,
                    ids,
                )?;
            }
            7 => {
                if state.mode == runtime::RUNTIME_CANCEL {
                    if !cancel_stage7(paging, nodes, queue, a, b, members, state, object)? {
                        break;
                    }
                } else if state.mode == runtime::RUNTIME_PEER_EXIT && !state.resumed {
                    if a.space.is_some() {
                        return Err(Error::Corrupt);
                    }
                    let (frame, binding) = state.deferred.ok_or(Error::Corrupt)?;
                    let sequence = ledger()?[0];
                    if sequence != 1 || ledger()?[42] != 1 || ledger()?[15] != 0 {
                        return Err(Error::Corrupt);
                    }
                    let status = outcome(address_space::commit_request(
                        nodes,
                        paging,
                        space_mut(b)?,
                        members[1].active,
                        sequence,
                    ))?;
                    if status != 0 {
                        return Err(Error::Corrupt);
                    }
                    state.audits[0] = outcome(address_space::commit_request(
                        nodes,
                        paging,
                        space_mut(b)?,
                        members[1].active,
                        sequence,
                    ))?;
                    state.audits[7] = outcome(address_space::commit_request(
                        nodes,
                        paging,
                        space_mut(b)?,
                        members[1].active,
                        sequence,
                    ))?;
                    if state.audits[0] != OWNER || state.audits[7] != OWNER {
                        return Err(Error::Corrupt);
                    }
                    let mut frame = frame;
                    frame.registers.rax = 0;
                    frame.registers.rdx = 0;
                    let continuation =
                        SavedContext::continuation(members[1].active, binding, frame);
                    check(queue.resume(nodes, members[1].active, continuation))?;
                    state.audits[1] =
                        node_status(queue.resume(nodes, members[1].active, continuation));
                    if state.audits[1] != 2 {
                        return Err(Error::Corrupt);
                    }
                    members[1].last = Some(frame);
                    state.resumed = true;
                }
                if state.mode == runtime::RUNTIME_PEER_EXIT {
                    if block_word(
                        paging,
                        space(b)?,
                        runtime::RUNTIME_BLOCK_B,
                        runtime::RUNTIME_READY,
                    )? != 3
                    {
                        break;
                    }
                    expect_map(paging, space(b)?, runtime::RUNTIME_VA_B, object, 0, false)?;
                    expect_unmapped(paging, space(b)?, runtime::RUNTIME_VA_B_WRITE)?;
                    let readback = block_word(
                        paging,
                        space(b)?,
                        runtime::RUNTIME_BLOCK_B,
                        runtime::RUNTIME_READBACK,
                    )?;
                    let second = block_word(
                        paging,
                        space(b)?,
                        runtime::RUNTIME_BLOCK_B,
                        runtime::RUNTIME_STALE_STATUS,
                    )?;
                    if readback != runtime::RUNTIME_VALUE || second != runtime::RUNTIME_SECOND {
                        return Err(Error::Corrupt);
                    }
                    let diagnostics = ledger()?;
                    if diagnostics[40] != 1
                        || diagnostics[42] != 0
                        || diagnostics[43] != 1
                        || diagnostics[20] != 1
                        || diagnostics[35] != 1
                        || diagnostics[36] != 0
                        || paging.live_counts(object)? != (0, 1, 2)
                        || paging.memory_object_capability_refs(object)? != 1
                    {
                        return Err(Error::Corrupt);
                    }
                    p_write(paging, space(b)?, runtime::RUNTIME_P_MAIN, 0)?;
                    p_write(
                        paging,
                        space(b)?,
                        runtime::RUNTIME_P_REPLAY,
                        state.audits[0],
                    )?;
                    p_write(paging, space(b)?, runtime::RUNTIME_P_DONE, 1)?;
                    block_write(
                        paging,
                        space(b)?,
                        runtime::RUNTIME_BLOCK_B,
                        runtime::RUNTIME_GATE_THIRD,
                        1,
                    )?;
                    if check(nodes.node(b.id))?.state != NodeState::Exited {
                        break;
                    }
                    if block_word(
                        paging,
                        space(b)?,
                        runtime::RUNTIME_BLOCK_B,
                        runtime::RUNTIME_RESULT_WORD,
                    )? != runtime::RUNTIME_RESULT
                    {
                        return Err(Error::Corrupt);
                    }
                    destroy(paging, nodes, b)?;
                } else if state.mode != runtime::RUNTIME_CANCEL {
                    let second = if state.order == 1 { &mut *b } else { &mut *a };
                    if check(nodes.node(second.id))?.state != NodeState::Exited {
                        break;
                    }
                    destroy(paging, nodes, second)?;
                }
                state.stage = 8;
                publish_checkpoint(
                    paging,
                    nodes,
                    state,
                    Some(object),
                    Some(a),
                    Some(b),
                    members,
                    ids,
                )?;
            }
            8 => {
                if a.space.is_some() || b.space.is_some() {
                    return Err(Error::Corrupt);
                }
                paging.live_collect(object)?;
                state.stage = 9;
                publish_checkpoint(
                    paging,
                    nodes,
                    state,
                    Some(object),
                    Some(a),
                    Some(b),
                    members,
                    ids,
                )?;
            }
            9 => {
                if nodes.active().is_some()
                    || !queue.is_empty()
                    || members
                        .iter()
                        .any(|member| member.outcome.is_none() || member.stack.is_some())
                    || a.space.is_some()
                    || b.space.is_some()
                    || paging.live_counts(object).is_ok()
                    || paging.memory_object_capability_refs(object).is_ok()
                    || paging.live_physical(object, 0).is_ok()
                {
                    return Err(Error::Corrupt);
                }
                state.stage = 10;
                publish_checkpoint(
                    paging,
                    nodes,
                    state,
                    Some(object),
                    Some(a),
                    Some(b),
                    members,
                    ids,
                )?;
            }
            _ => break,
        }
        if state.stage == stage {
            break;
        }
    }
    Ok(())
}

pub(crate) fn run(paging: &mut Paging<'_, '_>) -> Result<()> {
    let mode = runtime::mode();
    let order = if matches!(
        mode,
        runtime::RUNTIME_GOLDEN
            | runtime::RUNTIME_FAULT
            | runtime::RUNTIME_PEER_EXIT
            | runtime::RUNTIME_CANCEL
    ) {
        1
    } else {
        2
    };
    let before = paging.stats();
    let tables = paging.table_count();
    serial::write(format_args!(
        "[NEU] Runtime baseline: mode={} order={} allocated={} free={} tables={} root={:#x}\n",
        mode,
        order,
        before.allocated,
        before.free,
        tables,
        paging.root()
    ))
    .map_err(|_| Error::Corrupt)?;
    preemptive::start(paging)?;
    let mut nodes = Nodes::new();
    let mut queue = Queue::new();
    let mut state = RuntimeState {
        stage: 0,
        mode,
        order,
        deferred: None,
        resumed: false,
        received: 0,
        audits: [0; 10],
    };
    let empty = runtime::RuntimeIds::EMPTY;
    runtime::checkpoint(
        paging,
        0,
        mode,
        None,
        None,
        None,
        [None; 3],
        &empty,
        None,
        None,
        order,
        state.audits,
    )?;
    let object = paging.create_memory_object(2)?;
    runtime::checkpoint(
        paging,
        1,
        mode,
        Some(object),
        None,
        None,
        [None; 3],
        &empty,
        None,
        None,
        order,
        state.audits,
    )?;
    let (mut a, mut a1) = create_live(paging, &mut nodes, 1, 0, 35)?;
    let (mut b, mut b1) = create_live(paging, &mut nodes, 3, 1, 77)?;
    let mut b2 = add_member(&mut nodes, &b, 4, 99, 2)?;
    let ids = runtime::RuntimeIds {
        node_a: a.id.raw(),
        node_b: b.id.raw(),
        space_a: space(&a)?.id().raw(),
        space_b: space(&b)?.id().raw(),
        thread_a1: a1.active.thread.raw(),
        thread_b1: b1.active.thread.raw(),
        thread_b2: b2.active.thread.raw(),
    };
    if mode != runtime::RUNTIME_CANCEL {
        check(nodes.abort_member(b2.active.thread))?;
        check(nodes.reclaim_thread(b2.active.thread))?;
        b2.outcome = Some(ExitReason::ConstructionFailure);
    }
    let rights = match mode {
        runtime::RUNTIME_WRITABLE => 15,
        runtime::RUNTIME_RIGHTS => 11,
        runtime::RUNTIME_READ_DENIED => 14,
        _ => 13,
    };
    address_space::bootstrap_runtime(
        &mut nodes,
        paging,
        a.id,
        space(&a)?,
        b.id,
        space(&b)?,
        Some(object),
        Rights::from_bits(rights).map_err(|_| Error::Corrupt)?,
    )
    .map_err(|_| Error::Corrupt)?;
    let h_mem = granted(address_space::bootstrap_handle(&nodes, a.id, 3))?;
    let h_send = granted(address_space::bootstrap_handle(&nodes, b.id, 6))?;
    let h_flag = granted(address_space::bootstrap_handle(&nodes, b.id, 3))?;
    let endpoint = granted(address_space::bootstrap_handle(&nodes, b.id, 7))?;
    let h_recv = Handle::from_raw(h_send.raw() + (1 << 32));
    unit(address_space::close_handle(&mut nodes, b.id, h_send))?;
    address_space::map_memory_object_creator(
        paging,
        space_mut(&mut a)?,
        object,
        runtime::RUNTIME_VA_A,
        2,
        Permissions::RW,
    )
    .map_err(|_| Error::Corrupt)?;
    paging.live_release_creator(object)?;
    for offset in runtime::RUNTIME_P_OFFSETS.into_iter().skip(1) {
        p_write(paging, space(&b)?, offset, runtime::RUNTIME_P_SENTINEL)?;
    }
    let script_b = runtime::RUNTIME_SCRIPT_B;
    let prefix = [
        record(OP_GATE, 1, runtime::RUNTIME_GATE as u64, 0, 0, 0),
        record(
            v05::TEST_MAP,
            h_send.raw(),
            runtime::RUNTIME_VA_B,
            1,
            READ_ONLY,
            0,
        ),
        record(v05::CHANNEL_RECEIVE_CAP, endpoint.raw(), 0, 0, 0, 0),
        record(v05::TEST_MAP, 0, runtime::RUNTIME_VA_B, 1, READ_ONLY, 0),
        record(
            v05::TEST_MAP,
            h_send.raw(),
            runtime::RUNTIME_VA_B,
            1,
            READ_ONLY,
            0,
        ),
        record(
            v05::TEST_MAP,
            h_flag.raw(),
            runtime::RUNTIME_VA_B,
            1,
            READ_ONLY,
            0,
        ),
        record(
            v05::TEST_MAP,
            u64::MAX,
            runtime::RUNTIME_VA_RESERVED,
            1,
            READ_ONLY,
            0,
        ),
        record(
            v05::TEST_MAP,
            u64::MAX,
            runtime::RUNTIME_VA_B + 1,
            1,
            READ_ONLY,
            0,
        ),
        record(
            v05::TEST_MAP,
            u64::MAX,
            runtime::RUNTIME_VA_B,
            0,
            READ_ONLY,
            0,
        ),
        record(
            v05::TEST_MAP,
            u64::MAX,
            runtime::RUNTIME_VA_B,
            1,
            READ_ONLY,
            2,
        ),
        record(
            v05::TEST_MAP,
            u64::MAX - 1,
            runtime::RUNTIME_VA_B,
            1,
            READ_ONLY,
            0,
        ),
        record(OP_GATE, 2, runtime::RUNTIME_GATE_SECOND as u64, 0, 0, 0),
    ];
    for (index, words) in prefix.into_iter().enumerate() {
        write_script(paging, space(&b)?, script_b, index, words)?;
    }
    let tail: &[[u64; 6]] = match mode {
        runtime::RUNTIME_GOLDEN => &[
            record(
                v05::TEST_MAP,
                u64::MAX,
                runtime::RUNTIME_VA_B,
                1,
                READ_ONLY,
                0,
            ),
            record(
                OP_READ,
                runtime::RUNTIME_VA_B,
                runtime::RUNTIME_READBACK as u64,
                0,
                0,
                0,
            ),
            record(
                OP_READ,
                runtime::RUNTIME_VA_B + 8,
                runtime::RUNTIME_STALE_STATUS as u64,
                0,
                0,
                0,
            ),
            record(
                v05::TEST_MAP,
                u64::MAX,
                runtime::RUNTIME_VA_B,
                1,
                WRITABLE,
                0,
            ),
            record(
                v05::TEST_MAP,
                u64::MAX,
                runtime::RUNTIME_VA_B,
                1,
                READ_ONLY,
                0,
            ),
            record(
                v05::TEST_MAP,
                u64::MAX,
                runtime::RUNTIME_VA_B,
                1,
                READ_ONLY,
                0,
            ),
            record(v02::CAP_CLOSE, u64::MAX, 0, 0, 0, 0),
            record(
                v05::TEST_MAP,
                u64::MAX,
                runtime::RUNTIME_VA_B,
                1,
                READ_ONLY,
                0,
            ),
            record(
                OP_READ,
                runtime::RUNTIME_VA_B,
                runtime::RUNTIME_MAIN_STATUS as u64,
                0,
                0,
                0,
            ),
            record(
                OP_READ,
                runtime::RUNTIME_VA_B + 8,
                runtime::RUNTIME_CLOSE_STATUS as u64,
                0,
                0,
                0,
            ),
            record(OP_GATE, 3, runtime::RUNTIME_GATE_THIRD as u64, 0, 0, 0),
            record(
                OP_STORE,
                runtime::RUNTIME_RESULT_WORD as u64,
                runtime::RUNTIME_RESULT,
                0,
                0,
                0,
            ),
            record(EXIT, 0, 0, 0, 0, 0),
        ],
        runtime::RUNTIME_WRITABLE => &[
            record(
                v05::TEST_MAP,
                u64::MAX,
                runtime::RUNTIME_VA_B,
                1,
                READ_ONLY,
                0,
            ),
            record(
                OP_READ,
                runtime::RUNTIME_VA_B,
                runtime::RUNTIME_READBACK as u64,
                0,
                0,
                0,
            ),
            record(
                v05::TEST_MAP,
                u64::MAX,
                runtime::RUNTIME_VA_B_WRITE,
                1,
                WRITABLE,
                1,
            ),
            record(
                OP_WRITE,
                runtime::RUNTIME_VA_B_WRITE,
                runtime::RUNTIME_WRITE,
                0,
                0,
                0,
            ),
            record(
                OP_READ,
                runtime::RUNTIME_VA_B_WRITE,
                runtime::RUNTIME_AFTER_STATUS as u64,
                0,
                0,
                0,
            ),
            record(OP_GATE, 3, runtime::RUNTIME_GATE_THIRD as u64, 0, 0, 0),
            record(
                OP_STORE,
                runtime::RUNTIME_RESULT_WORD as u64,
                runtime::RUNTIME_RESULT,
                0,
                0,
                0,
            ),
            record(EXIT, 0, 0, 0, 0, 0),
        ],
        runtime::RUNTIME_FAULT => &[
            record(
                v05::TEST_MAP,
                u64::MAX,
                runtime::RUNTIME_VA_B,
                1,
                READ_ONLY,
                0,
            ),
            record(
                OP_READ,
                runtime::RUNTIME_VA_B,
                runtime::RUNTIME_READBACK as u64,
                0,
                0,
                0,
            ),
            record(
                OP_READ,
                runtime::RUNTIME_VA_B + 8,
                runtime::RUNTIME_STALE_STATUS as u64,
                0,
                0,
                0,
            ),
            record(
                OP_WRITE,
                runtime::RUNTIME_VA_B,
                runtime::RUNTIME_WRITE,
                0,
                0,
                0,
            ),
        ],
        runtime::RUNTIME_PEER_EXIT => &[
            record(
                v05::TEST_MAP,
                u64::MAX,
                runtime::RUNTIME_VA_B,
                1,
                READ_ONLY,
                0,
            ),
            record(
                OP_READ,
                runtime::RUNTIME_VA_B,
                runtime::RUNTIME_READBACK as u64,
                0,
                0,
                0,
            ),
            record(
                OP_READ,
                runtime::RUNTIME_VA_B + 8,
                runtime::RUNTIME_STALE_STATUS as u64,
                0,
                0,
                0,
            ),
            record(OP_GATE, 3, runtime::RUNTIME_GATE_THIRD as u64, 0, 0, 0),
            record(
                OP_STORE,
                runtime::RUNTIME_RESULT_WORD as u64,
                runtime::RUNTIME_RESULT,
                0,
                0,
                0,
            ),
            record(EXIT, 0, 0, 0, 0, 0),
        ],
        runtime::RUNTIME_CANCEL => &[record(
            v05::TEST_MAP,
            u64::MAX,
            runtime::RUNTIME_VA_B,
            1,
            READ_ONLY,
            0,
        )],
        runtime::RUNTIME_RIGHTS | runtime::RUNTIME_READ_DENIED => &[
            record(
                v05::TEST_MAP,
                u64::MAX,
                runtime::RUNTIME_VA_B,
                1,
                READ_ONLY,
                0,
            ),
            record(
                v05::TEST_MAP,
                u64::MAX,
                runtime::RUNTIME_VA_B,
                1,
                WRITABLE,
                0,
            ),
            record(OP_GATE, 3, runtime::RUNTIME_GATE_THIRD as u64, 0, 0, 0),
            record(
                OP_STORE,
                runtime::RUNTIME_RESULT_WORD as u64,
                runtime::RUNTIME_RESULT,
                0,
                0,
                0,
            ),
            record(EXIT, 0, 0, 0, 0, 0),
        ],
        _ => return Err(Error::Corrupt),
    };
    for (offset, words) in tail.iter().enumerate() {
        write_script(paging, space(&b)?, script_b, 12 + offset, *words)?;
    }
    if mode == runtime::RUNTIME_CANCEL {
        let script_b2 = runtime::RUNTIME_SCRIPT_B2;
        let b2_script = [
            record(OP_GATE, 1, runtime::RUNTIME_GATE as u64, 0, 0, 0),
            record(
                v05::TEST_MAP,
                u64::MAX,
                runtime::RUNTIME_VA_B,
                1,
                READ_ONLY,
                0,
            ),
            record(
                OP_WRITE,
                runtime::RUNTIME_VA_B2_FAULT,
                runtime::RUNTIME_WRITE,
                0,
                0,
                0,
            ),
        ];
        for (index, words) in b2_script.into_iter().enumerate() {
            write_script(paging, space(&b)?, script_b2, index, words)?;
        }
    }
    install(paging, &mut nodes, &mut queue, &mut a, &mut a1, 16)?;
    install(paging, &mut nodes, &mut queue, &mut b, &mut b1, 17)?;
    if paging.member_pins(space(&a)?)? != 1 || paging.member_pins(space(&b)?)? != 1 {
        return Err(Error::Corrupt);
    }
    let mut members = [a1, b1, b2];
    state.stage = 2;
    publish_checkpoint(
        paging,
        &nodes,
        &state,
        Some(object),
        Some(&a),
        Some(&b),
        &members,
        &ids,
    )?;
    let peers = Peers {
        h_mem,
        h_send,
        h_flag,
        endpoint,
        h_recv,
    };
    let mut dispatches = 0;
    let mut printed = 0;
    while let Some(dispatch) = check(queue.next(&mut nodes))? {
        dispatches += 1;
        if dispatches > 512 {
            return Err(Error::Corrupt);
        }
        let index = members
            .iter()
            .position(|member| member.active == dispatch.active)
            .ok_or(Error::Corrupt)?;
        let active = dispatch.active;
        if members[index].outcome.is_some()
            || (!dispatch.saved.fresh && members[index].last != Some(dispatch.saved.frame))
        {
            return Err(Error::Corrupt);
        }
        serial::write(format_args!(
            "[NEU] Runtime selected: sequence={} label={} node={} thread={} root={:#x} fresh={}\n",
            dispatches,
            members[index].label,
            active.node.raw(),
            active.thread.raw(),
            active.context.root,
            dispatch.saved.fresh
        ))
        .map_err(|_| Error::Corrupt)?;
        let binding = members[index].stack.ok_or(Error::Corrupt)?;
        let publication = preemptive::arm(active, binding, true)?;
        address_space::enter(check(nodes.take_authority(active))?).map_err(|_| Error::Corrupt)?;
        let execution = native::execute_slice(
            paging,
            space(if index == 0 { &a } else { &b })?,
            dispatch,
            96,
        );
        check(nodes.restore_authority(address_space::leave(active).map_err(|_| Error::Corrupt)?))?;
        address_space::reap(paging).map_err(|_| Error::Corrupt)?;
        preemptive::settle()?;
        preemptive::print_records(&mut printed)?;
        let event = execution?;
        match event {
            SliceEvent::Preempted(frame) => {
                let saved = preemptive::consume(publication, frame)?;
                members[index].last = Some(frame);
                members[index].preemptions += 1;
                check(queue.preempted(&mut nodes, saved))?;
            }
            SliceEvent::Requested(frame) => {
                preemptive::terminal(publication)?;
                let (frame, _) = requested(
                    paging,
                    &mut nodes,
                    &mut queue,
                    &mut a,
                    &mut b,
                    &mut members,
                    &mut state,
                    index,
                    frame,
                    binding,
                    h_recv,
                    object,
                )?;
                members[index].last = Some(frame);
            }
            SliceEvent::Exited(reason) => {
                preemptive::terminal(publication)?;
                members[index].last = None;
                members[index].outcome = Some(reason);
                check(queue.finish(&mut nodes, active, reason))?;
                serial::write(format_args!(
                    "[NEU] Runtime outcome: label={} thread={} reason={}\n",
                    members[index].label,
                    active.thread.raw(),
                    match reason {
                        ExitReason::Normal(_) => "normal",
                        ExitReason::UserFault(_) => "fault",
                        ExitReason::Cancelled { .. } => "cancelled",
                        ExitReason::ConstructionFailure => "construction",
                        ExitReason::Quiesced => "quiesced",
                    }
                ))
                .map_err(|_| Error::Corrupt)?;
            }
            _ => return Err(Error::Corrupt),
        }
        let mut rounds = 0;
        loop {
            let stage = state.stage;
            progress(
                paging,
                &mut nodes,
                &mut queue,
                &mut a,
                &mut b,
                &mut members,
                &mut state,
                object,
                &ids,
                &peers,
            )?;
            rounds += 1;
            if state.stage == stage || rounds > 20 {
                break;
            }
        }
        if state.stage >= 10 {
            break;
        }
    }
    if nodes.active().is_some()
        || !queue.is_empty()
        || members
            .iter()
            .any(|member| member.outcome.is_none() || member.stack.is_some())
        || a.space.is_some()
        || b.space.is_some()
        || state.stage != 10
    {
        return Err(Error::Corrupt);
    }
    let captures = preemptive::stop(paging)?;
    if paging.stats() != before || paging.table_count() != tables {
        return Err(Error::Corrupt);
    }
    unit(address_space::finish())?;
    serial::write(format_args!(
        "[NEU] MemoryObject runtime test passed: mode={} order={} dispatches={} captures={} allocations=restored\n",
        mode, order, dispatches, captures
    ))
    .map_err(|_| Error::Corrupt)
}

//! Trusted two-domain bootstrap controller. This is deliberately sequential,
//! with no run queue, public creation syscall, or implemented capability space.
use super::{
    native,
    paging::{Paging, UserSpace},
    serial, userspace,
};
use core::ptr;
use neu_kernel::{
    node::{self, ExitReason, FaultSource, NodeId, NodeState, Nodes, ThreadContext, ThreadId},
    paging::{Error, Result},
};
unsafe extern "C" {
    static neu_native_payload: u8;
    static neu_node_cross_site: u8;
    static neu_node_ud_site: u8;
}
struct Bootstrap {
    node: NodeId,
    thread: ThreadId,
    space: Option<UserSpace>,
    label: u64,
    kind: u64,
    seed: u64,
}
fn lifecycle<T>(result: node::Result<T>) -> Result<T> {
    result.map_err(|_| Error::Corrupt)
}
fn build(
    paging: &mut Paging<'_, '_>,
    model: &mut Nodes,
    label: u64,
    kind: u64,
    budget: Option<usize>,
) -> Result<Bootstrap> {
    let id = lifecycle(model.create())?;
    lifecycle(model.configure(id))?;
    let seed = if label == 1 { 35 } else { 77 };
    let space = match paging.prepare_domain(
        native::payload(),
        kind,
        seed,
        if label == 2 {
            Some(native::PRIVATE_VALUE)
        } else {
            None
        },
        budget,
    ) {
        Ok(space) => space,
        Err(error) => {
            lifecycle(model.construction_failed(id))?;
            if model.node(id).map_err(|_| Error::Corrupt)?.outcome
                != Some(ExitReason::ConstructionFailure)
            {
                return Err(Error::Corrupt);
            }
            lifecycle(model.reclaim(id))?;
            return Err(error);
        }
    };
    lifecycle(model.bind_space(id, space.id()))?;
    let thread = lifecycle(model.initial_thread(
        id,
        ThreadContext {
            space: space.id(),
            root: space.root,
            entry: userspace::CODE,
            rsp: userspace::USER_RSP,
        },
    ))?;
    lifecycle(model.ready(id))?;
    serial::write(format_args!("[NEU] Node ready: label={} node={} thread={} space={} root={:#x} code_pa={:#x} data_pa={:#x} stack_pa={:#x} seed={} private={}\n",label,id.raw(),thread.raw(),space.id().raw(),space.root,paging.user_physical(&space,userspace::CODE)?,paging.user_physical(&space,userspace::DATA)?,paging.user_physical(&space,userspace::STACK)?,seed,label==2)).map_err(|_|Error::Corrupt)?;
    Ok(Bootstrap {
        node: id,
        thread,
        space: Some(space),
        label,
        kind,
        seed,
    })
}
fn space(node: &Bootstrap) -> Result<&UserSpace> {
    node.space.as_ref().ok_or(Error::NotOwned)
}
fn verify_pair(paging: &mut Paging<'_, '_>, a: &Bootstrap, b: &Bootstrap) -> Result<()> {
    let (sa, sb) = (space(a)?, space(b)?);
    paging.verify_user(sa)?;
    paging.verify_user(sb)?;
    if sa.root == sb.root || sa.id() == sb.id() || a.node == b.node || a.thread == b.thread {
        return Err(Error::Corrupt);
    }
    for va in [
        userspace::CODE,
        userspace::CONSTANTS,
        userspace::DATA,
        userspace::STACK,
    ] {
        if paging.user_physical(sa, va)? == paging.user_physical(sb, va)? {
            return Err(Error::Corrupt);
        }
    }
    if paging.user_physical(sa, userspace::UNMAPPED) != Err(Error::NotMapped)
        || paging.private_value(sb)? != Some(native::PRIVATE_VALUE)
    {
        return Err(Error::Corrupt);
    }
    Ok(())
}
fn execute(paging: &mut Paging<'_, '_>, model: &mut Nodes, node: &Bootstrap) -> Result<()> {
    let binding = lifecycle(model.start(node.thread))?;
    let s = space(node)?;
    if binding.node != node.node
        || binding.context.space != s.id()
        || binding.context.root != s.root
    {
        return Err(Error::Corrupt);
    }
    serial::write(format_args!(
        "[NEU] Node start: label={} node={} thread={} space={} root={:#x} kind={}\n",
        node.label,
        node.node.raw(),
        node.thread.raw(),
        s.id().raw(),
        s.root,
        node.kind
    ))
    .map_err(|_| Error::Corrupt)?;
    let outcome = native::execute_node(paging, s, binding, node.kind)?;
    lifecycle(model.complete(binding, outcome))?;
    let data = paging.user_result(s)?;
    if data != node.seed + 7
        || model.node(node.node).map_err(|_| Error::Corrupt)?.state != NodeState::Exited
    {
        return Err(Error::Corrupt);
    }
    match outcome {
        ExitReason::Normal(status) => {
            if node.kind == 10 || node.kind == 11 || status != 0 {
                return Err(Error::Corrupt);
            }
            serial::write(format_args!("[NEU] Node outcome: label={} node={} thread={} reason=normal status={} result={}\n",node.label,node.node.raw(),node.thread.raw(),status,data)).map_err(|_|Error::Corrupt)?;
        }
        ExitReason::UserFault(fault) => {
            let (vector, error, address, site) = if node.kind == 10 {
                (
                    14,
                    4,
                    userspace::UNMAPPED,
                    ptr::addr_of!(neu_node_cross_site),
                )
            } else {
                (6, 0, 0, ptr::addr_of!(neu_node_ud_site))
            };
            let rip = userspace::CODE + (site as u64 - ptr::addr_of!(neu_native_payload) as u64);
            if !matches!(node.kind, 10 | 11)
                || fault.source != FaultSource::Cpu
                || fault.vector != vector
                || fault.error != error
                || fault.rip != rip
                || fault.rsp != userspace::USER_RSP
                || fault.cs != neu_kernel::syscall::USER_CS
                || fault.ss != neu_kernel::syscall::USER_SS
                || (vector == 14 && fault.cr2 != address)
            {
                return Err(Error::Corrupt);
            }
            serial::write(format_args!(
                "[NEU] Node outcome: label={} node={} thread={} reason=fault status=0 result={}\n",
                node.label,
                node.node.raw(),
                node.thread.raw(),
                data
            ))
            .map_err(|_| Error::Corrupt)?;
        }
        ExitReason::ConstructionFailure | ExitReason::Cancelled { .. } | ExitReason::Quiesced => {
            return Err(Error::Corrupt);
        }
    }
    if node.label == 2 && paging.private_value(s)? != Some(native::PRIVATE_VALUE) {
        return Err(Error::Corrupt);
    }
    Ok(())
}
fn reclaim(paging: &mut Paging<'_, '_>, model: &mut Nodes, node: &mut Bootstrap) -> Result<()> {
    let s = node.space.as_mut().ok_or(Error::NotOwned)?;
    let id = s.id();
    let outcome = lifecycle(model.node(node.node))?.outcome;
    paging.destroy_user(s)?;
    node.space = None;
    lifecycle(model.release_space(node.node, id))?;
    lifecycle(model.reclaim(node.node))?;
    let retained = lifecycle(model.node(node.node))?;
    if retained.state != NodeState::Reclaimed || retained.outcome != outcome {
        return Err(Error::Corrupt);
    }
    serial::write(format_args!(
        "[NEU] Node reclaimed: label={} node={} space={} outcome=retained\n",
        node.label,
        node.node.raw(),
        id.raw()
    ))
    .map_err(|_| Error::Corrupt)?;
    Ok(())
}
fn survivor(paging: &mut Paging<'_, '_>, model: &Nodes, node: &Bootstrap) -> Result<()> {
    let s = space(node)?;
    paging.verify_user(s)?;
    if lifecycle(model.node(node.node))?.state != NodeState::Ready || paging.user_result(s)? != 0 {
        return Err(Error::Corrupt);
    }
    if node.label == 2 && paging.private_value(s)? != Some(native::PRIVATE_VALUE) {
        return Err(Error::Corrupt);
    }
    serial::write(format_args!(
        "[NEU] Node survivor verified: label={} node={} space={} root={:#x} data=0\n",
        node.label,
        node.node.raw(),
        s.id().raw(),
        s.root
    ))
    .map_err(|_| Error::Corrupt)?;
    Ok(())
}
pub fn run(paging: &mut Paging<'_, '_>) -> Result<()> {
    let case = if cfg!(feature = "test-nodes-a-fault") {
        1
    } else if cfg!(feature = "test-nodes-b-fault") {
        2
    } else if cfg!(feature = "test-nodes-cross") {
        3
    } else if cfg!(feature = "test-nodes-reclaim") {
        4
    } else if cfg!(feature = "test-nodes-rollback") {
        5
    } else {
        0
    };
    let before = paging.stats();
    let tables = paging.table_count();
    serial::write(format_args!(
        "[NEU] Node accounting baseline: allocated={} free={} tables={}\n",
        before.allocated, before.free, tables
    ))
    .map_err(|_| Error::Corrupt)?;
    let mut model = Nodes::new();
    let a_kind = if case == 1 {
        11
    } else if case == 3 {
        10
    } else {
        8
    };
    let b_kind = if case == 2 { 11 } else { 9 };
    let mut a = build(paging, &mut model, 1, a_kind, None)?;
    let mut b = if case == 5 {
        let mut failures = 0;
        let mut ready = None;
        for budget in 0..64 {
            let stats = paging.stats();
            let count = paging.table_count();
            match build(paging, &mut model, 2, b_kind, Some(budget)) {
                Err(Error::Exhausted) => {
                    failures += 1;
                    if paging.stats() != stats || paging.table_count() != count {
                        return Err(Error::Corrupt);
                    }
                    survivor(paging, &model, &a)?;
                }
                Ok(node) => {
                    ready = Some(node);
                    break;
                }
                Err(error) => return Err(error),
            }
        }
        if failures == 0 || failures == 64 {
            return Err(Error::Corrupt);
        }
        serial::write(format_args!(
            "[NEU] Node B rollback verified: failpoints={} A_retained=true\n",
            failures
        ))
        .map_err(|_| Error::Corrupt)?;
        ready.ok_or(Error::Corrupt)?
    } else {
        build(paging, &mut model, 2, b_kind, None)?
    };
    verify_pair(paging, &a, &b)?;
    let stats = paging.stats();
    if !matches!(
        paging.prepare_user(native::payload(), 8, None),
        Err(Error::Capacity)
    ) || paging.stats() != stats
    {
        return Err(Error::Corrupt);
    }
    verify_pair(paging, &a, &b)?;
    serial::write(format_args!(
        "[NEU] Two Nodes live: A={} B={} A_root={:#x} B_root={:#x} capacity=verified\n",
        a.node.raw(),
        b.node.raw(),
        space(&a)?.root,
        space(&b)?.root
    ))
    .map_err(|_| Error::Corrupt)?;
    if case == 2 {
        execute(paging, &mut model, &b)?;
        reclaim(paging, &mut model, &mut b)?;
        survivor(paging, &model, &a)?;
        execute(paging, &mut model, &a)?;
        reclaim(paging, &mut model, &mut a)?;
    } else {
        execute(paging, &mut model, &a)?;
        if matches!(case, 1 | 3 | 4) {
            reclaim(paging, &mut model, &mut a)?;
            survivor(paging, &model, &b)?;
        }
        execute(paging, &mut model, &b)?;
        if a.space.is_some() {
            reclaim(paging, &mut model, &mut a)?;
        }
        reclaim(paging, &mut model, &mut b)?;
    }
    if paging.stats() != before || paging.table_count() != tables {
        return Err(Error::Corrupt);
    }
    serial::write(format_args!("[NEU] Node accounting restored: allocated={} free={} tables={}\n[NEU] Node isolation test passed: case={}\n",paging.stats().allocated,paging.stats().free,paging.table_count(),case)).map_err(|_|Error::Corrupt)?;
    Ok(())
}

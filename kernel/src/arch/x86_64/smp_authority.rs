//! K10 uses the existing shared scheduler transaction and actual hardware
//! shootdown path. Public inputs are Node-local handles, never mapping IDs.
use super::*;
use cap::Revocation;
use logic::{MemoryObjectId, Permissions};

pub(super) struct Controller {
    memory: Option<MemoryObjectId>,
    request: Option<Pending>,
    busy: bool,
    operations: usize,
    invalidations: usize,
    completed: usize,
}
struct Pending {
    ticket: Revocation,
    active: ActiveContext,
    frame: Frame,
    cpu: usize,
    entry: u64,
}
impl Controller {
    pub const fn new() -> Self {
        Self {
            memory: None,
            request: None,
            busy: false,
            operations: 0,
            invalidations: 0,
            completed: 0,
        }
    }
    pub fn pending(&self) -> bool {
        self.request.is_some() || self.busy
    }
}
#[unsafe(no_mangle)]
static neu_authority_layout: [AtomicU64; 64] = [const { AtomicU64::new(0) }; 64];
#[unsafe(no_mangle)]
static neu_authority_invalidations: [[AtomicU64; 16]; 8] =
    [const { [const { AtomicU64::new(0) }; 16] }; 8];
#[unsafe(no_mangle)]
static neu_authority_operations: [[AtomicU64; 16]; 128] =
    [const { [const { AtomicU64::new(0) }; 16] }; 128];

fn mode() -> u64 {
    u64::from(cfg!(feature = "test-smp-authority-queued"))
        | (u64::from(
            cfg!(feature = "test-smp-authority-close")
                || cfg!(feature = "test-smp-authority-stress")
                || cfg!(feature = "test-smp-authority-wait"),
        ) << 1)
        | (u64::from(cfg!(feature = "test-smp-authority-wait")) << 2)
        | (u64::from(cfg!(feature = "test-smp-authority-stress")) << 3)
        | (u64::from(cfg!(feature = "test-smp-authority-terminate")) << 4)
}
pub(super) fn snapshot(s: &State) {
    for (cell, value) in neu_authority_layout[..14]
        .iter()
        .zip(s.objects.authority_layout())
    {
        cell.store(value, Ordering::Release);
    }
    for (cell, value) in neu_authority_layout[14..27]
        .iter()
        .zip(s.objects.mapping_layout())
    {
        cell.store(value, Ordering::Release);
    }
    neu_authority_layout[27].store(cap::Table::grant_offset(), Ordering::Release);
    neu_authority_layout[28].store(
        neu_kernel::channel::Channels::grant_offset(),
        Ordering::Release,
    );
    neu_authority_layout[40].store(mode(), Ordering::Release);
    neu_authority_layout[41].store(s.authority.completed as u64, Ordering::Release);
    neu_authority_layout[42].store(s.authority.operations as u64, Ordering::Release);
    neu_authority_layout[45].store(
        s.authority.request.as_ref().map_or(0, |r| r.ticket.epoch()),
        Ordering::Release,
    );
    neu_authority_layout[46].store(
        s.authority
            .request
            .as_ref()
            .map_or(0, |r| r.ticket.root().raw()),
        Ordering::Release,
    );
    neu_authority_layout[47].store(s.authority.busy as u64, Ordering::Release);
    neu_authority_layout[48].store(s.authority.invalidations as u64, Ordering::Release);
    for (cell, value) in neu_authority_layout[52..55]
        .iter()
        .zip(s.objects.authority_wait_grants())
    {
        cell.store(value, Ordering::Release);
    }
}
pub(super) fn bootstrap(
    paging: &mut Paging<'_, '_>,
    s: &mut State,
    spaces: [&UserSpace; 2],
) -> Result<(), &'static str> {
    let memory = paging
        .create_memory_object(2)
        .map_err(|_| "authority backing allocation")?;
    let creator = s
        .objects
        .memory_object(
            paging
                .retain_capability_reference(memory)
                .map_err(|_| "authority paging reference")?,
        )
        .map_err(|_| "authority capability object")?;
    s.authority.memory = Some(memory);
    for (index, space) in spaces.into_iter().enumerate() {
        let node = s.node_ids[index].unwrap();
        let address = s
            .objects
            .address_space(
                node,
                paging
                    .retain_object_space(space)
                    .map_err(|_| "authority address-space reference")?,
            )
            .map_err(|_| "authority address-space object")?;
        let table = s
            .nodes
            .authority_mut(node)
            .map_err(|_| "authority bootstrap table")?;
        for (slot, rights) in [
            (0, Rights::INSPECT),
            (1, Rights::INSPECT),
            (2, Rights::NONE),
        ] {
            table
                .install_bootstrap(&mut s.objects, &address, rights, slot)
                .map_err(|_| "authority inspection grant")?;
        }
        let rights = Rights::MEMORY_OBJECT
            .union(Rights::DERIVE)
            .union(Rights::REVOKE);
        table
            .install_bootstrap(&mut s.objects, &creator, rights, 3)
            .map_err(|_| "authority independent root grant")?;
        s.objects
            .release(address)
            .map_err(|_| "authority address creator release")?;
    }
    s.objects
        .release(creator)
        .map_err(|_| "authority creator release")?;
    paging
        .release_memory_creator(memory)
        .map_err(|_| "authority backing creator release")?;
    for (cell, value) in neu_authority_layout[29..32]
        .iter()
        .zip(paging.authority_mapping_offsets())
    {
        cell.store(value, Ordering::Release);
    }
    neu_authority_layout[32].store(memory.raw(), Ordering::Release);
    for (cell, value) in neu_authority_layout[37..39].iter().zip(
        paging
            .authority_memory_frames(memory)
            .map_err(|_| "authority physical backing")?,
    ) {
        cell.store(value, Ordering::Release);
    }
    barrier_word(12).store(mode(), Ordering::Release);
    Ok(())
}
pub(super) fn prepare_mappings(
    paging: &mut Paging<'_, '_>,
    s: &mut State,
    spaces: [&mut UserSpace; 2],
) -> Result<(), &'static str> {
    let [a, b] = s.node_ids.map(Option::unwrap);
    let root = Handle::from_raw((1u64 << 32) | 4);
    let endpoint = Handle::from_raw((1u64 << 32) | 5);
    for node in [a, b] {
        let table = s
            .nodes
            .authority_mut(node)
            .map_err(|_| "authority setup table")?;
        for slot in [1, 2] {
            table
                .close(
                    &mut s.objects,
                    node,
                    Handle::from_raw((1u64 << 32) | (slot + 1)),
                )
                .map_err(|_| "authority setup free handle")?;
        }
    }
    let table = s
        .nodes
        .authority_mut(a)
        .map_err(|_| "authority derivation table")?;
    let child = table
        .derive(
            &mut s.objects,
            a,
            root,
            Rights::MEMORY_OBJECT
                .union(Rights::DERIVE)
                .union(Rights::REVOKE),
        )
        .map_err(|_| "authority child")?;
    let descendant = table
        .derive(
            &mut s.objects,
            a,
            child,
            Rights::READ.union(Rights::MAP).union(Rights::TRANSFER),
        )
        .map_err(|_| "authority grandchild")?;
    for (index, handle) in [(33, root), (34, child), (35, descendant)] {
        neu_authority_layout[index].store(
            table
                .grant_id(&s.objects, a, handle)
                .map_err(|_| "authority provenance")?
                .raw(),
            Ordering::Release,
        );
    }
    let mut guard = table
        .lookup(
            &mut s.objects,
            a,
            child,
            ObjectType::MemoryObject,
            Rights::MAP,
        )
        .map_err(|_| "authority source mapping lookup")?;
    paging
        .map_authority(spaces[0], &mut guard, 0x410000, 2, Permissions::RW)
        .map_err(|_| "authority source mapping")?;
    drop(guard);
    table
        .send_cap(&mut s.objects, a, endpoint, descendant)
        .map_err(|_| "authority descendant move")?;
    table
        .close(&mut s.objects, a, child)
        .map_err(|_| "authority mapped handle close")?;
    let table = s
        .nodes
        .authority_mut(b)
        .map_err(|_| "authority receiver table")?;
    let received = table
        .receive_cap(&mut s.objects, b, endpoint)
        .map_err(|_| "authority descendant receive")?;
    neu_authority_layout[36].store(
        table
            .grant_id(&s.objects, b, root)
            .map_err(|_| "authority independent provenance")?
            .raw(),
        Ordering::Release,
    );
    let mut guard = table
        .lookup(
            &mut s.objects,
            b,
            received,
            ObjectType::MemoryObject,
            Rights::MAP,
        )
        .map_err(|_| "authority received mapping lookup")?;
    paging
        .map_authority(spaces[1], &mut guard, 0x420000, 2, Permissions::R)
        .map_err(|_| "authority received mapping")?;
    drop(guard);
    let mut guard = table
        .lookup(
            &mut s.objects,
            b,
            root,
            ObjectType::MemoryObject,
            Rights::MAP,
        )
        .map_err(|_| "authority independent mapping lookup")?;
    paging
        .map_authority(spaces[1], &mut guard, 0x430000, 2, Permissions::RW)
        .map_err(|_| "authority independent mapping")?;
    Ok(())
}
pub(super) fn record(
    s: &mut State,
    cpu: usize,
    active: ActiveContext,
    f: Frame,
    outcome: Outcome,
    phase: u64,
) {
    let index = s.authority.operations;
    if index >= neu_authority_operations.len() {
        reject(cpu, 50);
    }
    let (status, scalar) = match outcome {
        Outcome::Return { status, scalar } => (status, scalar),
        Outcome::Blocked => (-100, 0),
        Outcome::Exit(_) => unreachable!(),
    };
    publish(
        &neu_authority_operations[index],
        [
            index as u64 + 1,
            phase,
            cpu as u64,
            active.node.raw(),
            active.thread.raw(),
            f.registers.rax,
            f.registers.rdi,
            f.registers.rsi,
            status as u64,
            scalar,
            neu_smp_sched_cpus[cpu].get(field::ENTRIES),
            active.context.root,
            f.registers.rdx,
            f.registers.r10,
            f.registers.r8,
            f.registers.r9,
        ],
    );
    s.authority.operations += 1;
}
pub(super) fn operation(s: &mut State, cpu: usize, active: ActiveContext, f: Frame) -> Outcome {
    if !abi::v06::valid_arguments(f.registers.rax, f.registers.arguments()) {
        let result = Outcome::Return {
            status: abi::INVALID_ARGUMENT,
            scalar: 0,
        };
        record(s, cpu, active, f, result, 0);
        return result;
    }
    let mut lease = s
        .nodes
        .take_authority_on(cpu, active)
        .expect("authority syscall lease");
    let mut revoke = None;
    let result = (|| -> cap::Result<u64> {
        let source = Handle::from_raw(f.registers.rdi);
        if f.registers.rax == abi::v06::CAP_DERIVE {
            let rights = Rights::from_authority_bits(f.registers.rsi)?;
            Ok(lease
                .table_mut()
                .derive(&mut s.objects, active.node, source, rights)?
                .raw())
        } else {
            revoke = Some(
                lease
                    .table()
                    .begin_revoke(&mut s.objects, active.node, source)?,
            );
            Ok(0)
        }
    })();
    s.nodes
        .restore_authority_on(cpu, lease)
        .expect("authority syscall lease restoration");
    let mut outcome = match result {
        Ok(scalar) => Outcome::Return { status: 0, scalar },
        Err(e) => Outcome::Return {
            status: e as i64,
            scalar: 0,
        },
    };
    if let Some(ticket) = revoke {
        s.objects
            .purge_revoked_operations()
            .expect("preflighted revoked operation sweep");
        s.nodes
            .purge_revoked_authority(&mut s.objects)
            .expect("returned revocation leases");
        if s.objects
            .revocation_mappings(&ticket)
            .expect("active revocation")
            .iter()
            .all(Option::is_none)
        {
            let affected = s
                .objects
                .finish_revoke(&ticket)
                .expect("revocation owners settled");
            s.authority.completed += 1;
            outcome = Outcome::Return {
                status: 0,
                scalar: affected as u64,
            };
        } else {
            assert!(s.authority.request.is_none() && !s.authority.busy);
            s.authority.request = Some(Pending {
                ticket,
                active,
                frame: f,
                cpu,
                entry: neu_smp_sched_cpus[cpu].get(field::ENTRIES),
            });
            outcome = Outcome::Blocked;
        }
    }
    record(s, cpu, active, f, outcome, 0);
    outcome
}
pub(super) fn service(paging: &mut Paging<'_, '_>, spaces: [&UserSpace; 2]) {
    let wait_checkpoint = with_state(0, |s| {
        cfg!(feature = "test-smp-authority-wait")
            && s.objects.wait_stats().waiting != 0
            && barrier_word(10).load(Ordering::Acquire) == 0
    });
    if wait_checkpoint {
        neu_authority_layout[51].store(1, Ordering::Release);
        probe_wait();
        barrier_word(10).store(2, Ordering::Release);
    }
    let pending = with_state(0, |s| {
        let request = s.authority.request.as_ref()?;
        let state = s.nodes.thread(request.active.thread).ok()?.state;
        if !matches!(state, ThreadState::Blocked | ThreadState::Exited) {
            return None;
        }
        s.authority.busy = true;
        s.authority.request.take()
    });
    let Some(pending) = pending else {
        return;
    };
    if cfg!(feature = "test-smp-authority-terminate") {
        // Deterministic lifecycle stress: a real sibling fault cancels the
        // blocked requester and closes its Node before the BSP retires PTEs.
        barrier_word(14).store(1, Ordering::Release);
        let mut exited = false;
        for _ in 0..WAIT_LIMIT {
            if with_state(0, |s| {
                s.nodes
                    .thread(pending.active.thread)
                    .is_ok_and(|t| t.state == ThreadState::Exited)
            }) {
                exited = true;
                break;
            }
            core::hint::spin_loop();
        }
        if !exited {
            reject(0, 53);
        }
    }
    let mappings = with_state(0, |s| {
        s.objects
            .revocation_mappings(&pending.ticket)
            .expect("pending revocation mappings")
    });
    for binding in mappings.into_iter().flatten() {
        let space = spaces
            .into_iter()
            .find(|space| space.id() == binding.space())
            .expect("owned revocation destination");
        let (request, slot) = with_state(0, |s| {
            let request = s
                .tlb
                .begin(
                    binding.root(),
                    binding.address(),
                    (1 << smp::cpu_count()) - 1,
                    (1 << smp::cpu_count()) - 1,
                )
                .expect("authority shootdown begin");
            s.objects
                .validate_mapping_revoke(&pending.ticket, binding, request)
                .expect("authority mapping selection");
            let (pte, before) = paging
                .revoke_memory_mapping(space, binding, request)
                .expect("authority atomic PTE removal");
            s.objects
                .mark_mapping_revoke(&pending.ticket, binding, request)
                .expect("authority retained mapping ownership");
            let slot = s.authority.invalidations;
            if slot >= neu_authority_invalidations.len() {
                reject(0, 51);
            }
            s.authority.invalidations += 1;
            publish(
                &neu_authority_invalidations[slot],
                [
                    2,
                    request.epoch,
                    request.root,
                    request.address,
                    request.targets,
                    binding.grant().raw(),
                    binding.space().raw(),
                    pte,
                    before,
                    0,
                    0,
                    0,
                    pending.ticket.epoch(),
                    pending.ticket.root().raw(),
                    0,
                    0,
                ],
            );
            (request, slot)
        });
        TLB_ROOT.store(request.root, Ordering::Relaxed);
        TLB_ADDRESS.store(request.address, Ordering::Relaxed);
        TLB_TARGETS.store(request.targets, Ordering::Relaxed);
        TLB_ACKS.store(0, Ordering::Relaxed);
        TLB_EPOCH.store(request.epoch, Ordering::Release);
        invalidate_local(0);
        notify(0, request.targets & !1);
        let mut complete = false;
        for _ in 0..WAIT_LIMIT {
            if TLB_ACKS.load(Ordering::Acquire) == request.targets {
                complete = true;
                break;
            }
            core::hint::spin_loop();
        }
        if !complete {
            with_state(0, |s| s.tlb.fail());
            reject(0, 52);
        }
        with_state(0, |s| {
            for cpu in 0..smp::cpu_count() {
                s.tlb
                    .acknowledge(cpu, request)
                    .expect("actual authority invalidation");
            }
            let receipt = s
                .tlb
                .complete(request.epoch)
                .expect("complete authority hardware acknowledgement");
            paging
                .complete_authority_mapping(space, &mut s.objects, &receipt)
                .expect("authority mapping retirement");
            neu_authority_invalidations[slot][10].store(request.targets, Ordering::Release);
            neu_authority_invalidations[slot][11]
                .store(s.objects.tracked_mappings() as u64, Ordering::Release);
            neu_authority_invalidations[slot][0].store(4, Ordering::Release);
        });
    }
    let wake = with_state(0, |s| {
        let affected = s
            .objects
            .finish_revoke(&pending.ticket)
            .expect("all revocation owners retired");
        s.authority.completed += 1;
        s.authority.busy = false;
        neu_authority_layout[44].store(affected as u64, Ordering::Release);
        let index = s.authority.operations;
        record(
            s,
            pending.cpu,
            pending.active,
            pending.frame,
            Outcome::Return {
                status: 0,
                scalar: affected as u64,
            },
            1,
        );
        neu_authority_operations[index][10].store(pending.entry, Ordering::Release);
        let state = s
            .nodes
            .thread(pending.active.thread)
            .expect("revocation requester generation")
            .state;
        if state == ThreadState::Blocked {
            s.nodes
                .settle_blocked_smp(pending.active.thread, 0, affected as u64)
                .expect("authority return continuation");
            let cpu = s
                .scheduler
                .wake(&mut s.nodes, pending.active.thread)
                .expect("authority requester wake");
            1 << cpu
        } else {
            if cfg!(feature = "test-smp-authority-terminate") {
                barrier_word(8).store(1, Ordering::Release);
            }
            0
        }
    });
    notify(0, wake);
}
pub(super) fn cleanup(
    paging: &mut Paging<'_, '_>,
    s: &mut State,
    spaces: [&mut UserSpace; 2],
) -> Result<(), &'static str> {
    if s.authority.pending() {
        return Err("pending authority transaction at teardown");
    }
    for space in spaces {
        paging
            .cleanup_authority_mappings(space, &mut s.objects)
            .map_err(|_| "authority independent mapping cleanup")?;
    }
    while let Some(reference) = s.objects.drain_memory_object() {
        paging
            .release_capability_reference(reference)
            .map_err(|_| "authority paging reference release")?;
    }
    paging
        .collect_memory_object(
            s.authority
                .memory
                .take()
                .ok_or("authority memory identity")?,
        )
        .map_err(|_| "authority backing collection")?;
    if s.objects.live_grants() != 0 || s.objects.tracked_mappings() != 0 {
        return Err("authority residual ownership");
    }
    Ok(())
}

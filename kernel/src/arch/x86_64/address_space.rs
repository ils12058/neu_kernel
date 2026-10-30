//! Experimental ABI 0.2 execution adapter. No suspended &mut Paging or Nodes
//! alias is stored. A lifetime-erased immutable reader exists only in with_reader.
use super::{
    interrupts,
    paging::{self, LiveInspection, Paging, UserSpace},
    serial,
};
use core::{
    cell::UnsafeCell,
    ptr,
    sync::atomic::{AtomicBool, Ordering},
};
#[cfg(feature = "memory-object-live-test")]
use neu_kernel::paging::Permissions;
#[cfg(feature = "channel-wait-test")]
use neu_kernel::{
    capability::Settle,
    wait::{WAITERS, Waiter},
};
use neu_kernel::{
    capability::{self as cap, Handle, Lease, ObjectType, Objects, Rights},
    node::{ActiveContext, NodeId, Nodes},
    paging::{MemoryObjectId, SpaceRef},
    syscall::{Frame, Outcome, v02},
};
const LIMIT: usize = 96;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Record {
    words: [u64; 24],
}
impl Record {
    const EMPTY: Self = Self { words: [0; 24] };
}
#[derive(Clone, Copy)]
struct Reader {
    pointer: *const (),
    call: unsafe fn(*const (), ActiveContext, NodeId, &SpaceRef, u64) -> Result<u64, i64>,
}
/// Bounded single-outstanding-request ledger for the K8C safe point. It stores
/// no Guard, borrowed table, or root mapping: authority is re-derived from the
/// live leased capability table at commit.
#[cfg(feature = "memory-object-runtime-test")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
enum RequestState {
    Pending,
    Committed,
    Rejected,
    Cancelled,
}
#[cfg(feature = "memory-object-runtime-test")]
#[derive(Clone, Copy)]
struct MapRequest {
    sequence: u64,
    node: NodeId,
    thread: neu_kernel::node::ThreadId,
    table: u64,
    space: neu_kernel::paging::SpaceId,
    root: u64,
    handle: Handle,
    object: cap::ObjectId,
    memory_object: MemoryObjectId,
    base: u64,
    offset: usize,
    pages: usize,
    permissions: Permissions,
    writable: bool,
    state: RequestState,
    status: i64,
}
#[cfg(feature = "memory-object-runtime-test")]
impl MapRequest {
    fn layout(self) -> [u64; 20] {
        [
            self.sequence,
            self.node.raw(),
            self.thread.raw(),
            self.table,
            self.space.raw(),
            self.root,
            self.handle.raw(),
            self.object.raw(),
            self.memory_object.raw(),
            self.base,
            self.offset as u64,
            self.pages as u64,
            u64::from(self.permissions.write),
            u64::from(self.permissions.execute),
            u64::from(self.writable),
            self.state as u64,
            self.status as u64,
            0,
            0,
            0,
        ]
    }
}
#[cfg(feature = "memory-object-runtime-test")]
pub const MAP_WITNESS_MAGIC: u64 = 0x4e45_554d_4f4d_4150;
struct State {
    objects: Objects,
    lease: Option<Lease>,
    reader: Option<Reader>,
    count: usize,
    probes: u64,
    records: [Record; LIMIT],
    #[cfg(feature = "memory-object-runtime-test")]
    request: Option<MapRequest>,
    #[cfg(feature = "memory-object-runtime-test")]
    last_request: Option<MapRequest>,
    #[cfg(feature = "memory-object-runtime-test")]
    requests: u64,
    #[cfg(feature = "memory-object-runtime-test")]
    cancelled: u64,
}
struct Storage(UnsafeCell<State>);
// SAFETY: One CPU; every access has IF=0/DF=0 and an exclusive non-reentrant
// gate. IRQ/NMI never access this state. Closures cannot return State borrows.
// Reader is installed/cleared within the enclosing shared Paging borrow, and
// cannot be invoked outside it. No mutable paging/controller alias is retained.
unsafe impl Sync for Storage {}
static STORAGE: Storage = Storage(UnsafeCell::new(State {
    objects: if cfg!(feature = "channel-test") {
        Objects::with_channels()
    } else {
        Objects::new()
    },
    lease: None,
    reader: None,
    count: 0,
    probes: 0,
    records: [Record::EMPTY; LIMIT],
    #[cfg(feature = "memory-object-runtime-test")]
    request: None,
    #[cfg(feature = "memory-object-runtime-test")]
    last_request: None,
    #[cfg(feature = "memory-object-runtime-test")]
    requests: 0,
    #[cfg(feature = "memory-object-runtime-test")]
    cancelled: 0,
}));
#[cfg(feature = "memory-object-runtime-test")]
struct MapWitness(UnsafeCell<[u64; 24]>);
// SAFETY: Written only inside the exclusive IF=0 transaction above; a paused
// external QMP reader is the only observer.
#[cfg(feature = "memory-object-runtime-test")]
unsafe impl Sync for MapWitness {}
#[cfg(feature = "memory-object-runtime-test")]
#[unsafe(no_mangle)]
static neu_map_witness: MapWitness = MapWitness(UnsafeCell::new([0; 24]));
/// Publish the current or most recent bounded mapping request. The layout is
/// identity/outcome metadata only; it carries no authority and no pointer.
#[cfg(feature = "memory-object-runtime-test")]
fn witness_write(s: &State) {
    let mut words = [0u64; 24];
    words[0] = MAP_WITNESS_MAGIC;
    if let Some(request) = s.request.or(s.last_request) {
        words[1..21].copy_from_slice(&request.layout());
    }
    words[21] = s.requests;
    words[22] = s.cancelled;
    words[23] = s.count as u64;
    // SAFETY: Exclusive IF=0 transaction; only a paused external QMP reader
    // observes this supervisor-only witness.
    unsafe {
        ptr::write_volatile(neu_map_witness.0.get(), words);
    }
}
struct Witness(UnsafeCell<Record>);
// SAFETY: Same transaction gate; only external paused QMP reads this witness.
unsafe impl Sync for Witness {}
#[unsafe(no_mangle)]
static neu_as_witness: Witness = Witness(UnsafeCell::new(Record::EMPTY));
static BORROW: AtomicBool = AtomicBool::new(false);
struct Borrow;
impl Drop for Borrow {
    fn drop(&mut self) {
        BORROW.store(false, Ordering::Release);
    }
}
fn tx<R>(f: impl for<'a> FnOnce(&'a mut State) -> cap::Result<R>) -> cap::Result<R> {
    if interrupts::flags() & 0x600 != 0
        || BORROW
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
    {
        return Err(cap::Error::InUse);
    }
    let _guard = Borrow;
    // SAFETY: Exclusive concrete storage contract above.
    f(unsafe { &mut *STORAGE.0.get() })
}
fn model<T>(r: neu_kernel::node::Result<T>) -> cap::Result<T> {
    r.map_err(|_| cap::Error::Owner)
}
pub fn bootstrap(
    nodes: &mut Nodes,
    paging: &mut Paging<'_, '_>,
    a: NodeId,
    sa: &UserSpace,
    b: NodeId,
    sb: &UserSpace,
    memory_object: Option<MemoryObjectId>,
) -> cap::Result<()> {
    bootstrap_at(
        nodes,
        paging,
        a,
        sa,
        b,
        sb,
        memory_object,
        Rights::MEMORY_OBJECT,
    )
}
/// Trusted bootstrap with an explicit MemoryObject grant. The K8C profiles use
/// a narrower grant so a received handle can prove exact rights denial.
#[cfg(feature = "memory-object-runtime-test")]
#[allow(clippy::too_many_arguments)]
pub fn bootstrap_runtime(
    nodes: &mut Nodes,
    paging: &mut Paging<'_, '_>,
    a: NodeId,
    sa: &UserSpace,
    b: NodeId,
    sb: &UserSpace,
    memory_object: Option<MemoryObjectId>,
    memory_rights: Rights,
) -> cap::Result<()> {
    bootstrap_at(nodes, paging, a, sa, b, sb, memory_object, memory_rights)
}
#[allow(clippy::too_many_arguments)]
fn bootstrap_at(
    nodes: &mut Nodes,
    paging: &mut Paging<'_, '_>,
    a: NodeId,
    sa: &UserSpace,
    b: NodeId,
    sb: &UserSpace,
    memory_object: Option<MemoryObjectId>,
    memory_rights: Rights,
) -> cap::Result<()> {
    tx(|s| {
        if s.objects.stats() != cap::Stats::default() || s.lease.is_some() || s.reader.is_some() {
            return Err(cap::Error::InUse);
        }
        let mut creators = [None, None, None, None];
        for (n, (owner, space)) in [(a, sa), (b, sb)].into_iter().enumerate() {
            let root_ref = paging
                .retain_object_space(space)
                .map_err(|_| cap::Error::Object)?;
            let object = s.objects.address_space(owner, root_ref).map_err(|(e, r)| {
                paging.release_object_space(r).unwrap();
                e
            })?;
            let t = model(nodes.authority_mut(owner))?;
            for (index, rights) in [(0, Rights::INSPECT), (1, Rights::INSPECT)] {
                t.install_bootstrap(&mut s.objects, &object, rights, index)?;
            }
            let second = if let Some(id) = memory_object.filter(|_| owner == a) {
                t.install_bootstrap(&mut s.objects, &object, Rights::NONE, 2)?;
                let retained = paging
                    .retain_capability_reference(id)
                    .map_err(|_| cap::Error::Object)?;
                let capability = match s.objects.memory_object(retained) {
                    Ok(capability) => capability,
                    Err((error, retained)) => {
                        paging
                            .release_capability_reference(retained)
                            .map_err(|_| cap::Error::Object)?;
                        return Err(error);
                    }
                };
                if let Err(error) =
                    t.install_bootstrap(&mut s.objects, &capability, memory_rights, 3)
                {
                    s.objects.release(capability)?;
                    while let Some(retained) = s.objects.drain_memory_object() {
                        paging
                            .release_capability_reference(retained)
                            .map_err(|_| cap::Error::Object)?;
                    }
                    return Err(error);
                }
                capability
            } else {
                let flag = s.objects.flag(true)?;
                #[cfg(feature = "test-cap-moves-double")]
                {
                    // Same object/reference totals: the third AddressSpace
                    // reference becomes a second movable reference to this
                    // Node's Flag, so two distinct grants can cross one Channel.
                    t.install_bootstrap_movable(&mut s.objects, &flag, Rights::READ, 2)?;
                    t.install_bootstrap_movable(&mut s.objects, &flag, Rights::READ, 3)?;
                }
                #[cfg(not(feature = "test-cap-moves-double"))]
                {
                    t.install_bootstrap(&mut s.objects, &object, Rights::NONE, 2)?;
                    t.install_bootstrap(&mut s.objects, &flag, Rights::READ, 3)?;
                }
                flag
            };
            creators[n * 2] = Some(object);
            creators[n * 2 + 1] = Some(second);
        }
        if cfg!(feature = "test-address-space-rollback") {
            let table = model(nodes.authority_mut(a))?;
            let before = s.objects.stats();
            if s.objects.flag(false).err() != Some(cap::Error::Capacity)
                || s.objects.stats() != before
            {
                return Err(cap::Error::Object);
            }
            // A full registry must return its real retained root token untouched.
            let reference = paging
                .retain_object_space(sa)
                .map_err(|_| cap::Error::Object)?;
            let (error, reference) = s
                .objects
                .address_space(a, reference)
                .err()
                .ok_or(cap::Error::Object)?;
            if error != cap::Error::Capacity {
                return Err(error);
            }
            paging
                .release_object_space(reference)
                .map_err(|_| cap::Error::Object)?;
            // Duplicate manifest index must fail before publication/reference changes.
            let creator = creators[0].as_ref().unwrap();
            let before = s.objects.stats();
            if table
                .install_bootstrap(&mut s.objects, creator, Rights::INSPECT, 0)
                .err()
                != Some(cap::Error::InUse)
                || before != s.objects.stats()
            {
                return Err(cap::Error::Object);
            }
            let mut extra = [Handle::from_raw(0); 4];
            for h in &mut extra {
                *h = table.grant(&mut s.objects, creator, Rights::INSPECT)?;
            }
            let full = s.objects.stats();
            if table.grant(&mut s.objects, creator, Rights::INSPECT).err()
                != Some(cap::Error::Capacity)
                || full != s.objects.stats()
            {
                return Err(cap::Error::Object);
            }
            for h in extra {
                table.close(&mut s.objects, a, h)?;
            }

            serial::write(format_args!("[NEU] AS rollback: objects=full table=full manifest=duplicate references=restored root_refs=2\n")).map_err(|_|cap::Error::Object)?;
        }
        #[cfg(feature = "channel-test")]
        {
            if cfg!(feature = "test-channels-rollback") {
                let table = model(nodes.authority_mut(b))?;
                let creator = creators[2].as_ref().unwrap();
                let extra = table.grant(&mut s.objects, creator, Rights::INSPECT)?;
                let before = s.objects.stats();
                let snapshot = s.objects.channel_state().snapshot();
                if nodes.bootstrap_channel(&mut s.objects, a, b) != Err(cap::Error::Capacity)
                    || s.objects.stats() != before
                    || s.objects.channel_state().snapshot() != snapshot
                {
                    return Err(cap::Error::Object);
                }
                model(nodes.authority_mut(b))?.close(&mut s.objects, b, extra)?;
                serial::write(format_args!("[NEU] IPC rollback: table=full pair=unpublished queues=unchanged references=restored
")).map_err(|_|cap::Error::Object)?;
            }
            nodes.bootstrap_channel(&mut s.objects, a, b)?;
        }
        for r in creators.into_iter().flatten() {
            s.objects.release(r)?;
        }
        for owner in [a, b] {
            #[cfg(feature = "cap-move-test")]
            if memory_object.is_some() && owner == a {
                model(nodes.authority_mut(owner))?.configure_memory_object_moves()?;
            } else {
                model(nodes.authority_mut(owner))?.configure_moves()?;
            }
            model(nodes.authority_mut(owner))?.seal_bootstrap();
        }
        let stats = s.objects.stats();
        if stats.live != if cfg!(feature = "channel-test") { 6 } else { 4 }
            || stats.references
                != if cfg!(feature = "channel-test") {
                    16
                } else {
                    8
                }
            || stats.pins != 0
        {
            return Err(cap::Error::Object);
        }
        if cfg!(feature = "channel-test") {
            #[cfg(feature = "channel-test")]
            super::ipc::bootstrap(a, b, &s.objects)?;
            Ok(())
        } else {
            serial::write(format_args!("[NEU] AS bootstrap: A={} B={} grants=INSPECT,INSPECT,NONE,Flag.READ handles=4,4 objects=4 references=8 root_refs=2 ABI=0.2\n",a.raw(),b.raw())).map_err(|_|cap::Error::Object)
        }
    })
}
/// Trusted controller setup: map an object into an inactive Node root using
/// only the Node's own bootstrap capability. The capability's actual rights
/// authorize the mapping; the creator token is not consulted here.
#[cfg(feature = "memory-object-live-test")]
#[allow(clippy::too_many_arguments)] // Trusted fixture setup: explicit Node, root, handle, range and permissions.
pub fn map_memory_object_capability(
    nodes: &mut Nodes,
    paging: &mut Paging<'_, '_>,
    node: NodeId,
    space: &mut UserSpace,
    index: u64,
    base: u64,
    pages: usize,
    permissions: Permissions,
) -> cap::Result<()> {
    tx(|s| {
        let table = model(nodes.authority(node))?;
        let handle = table.bootstrap_get(&s.objects, node, index)?;
        let guard = table.lookup(
            &mut s.objects,
            node,
            handle,
            ObjectType::MemoryObject,
            Rights::MAP,
        )?;
        let writable = permissions.write;
        let reference = guard.memory_object(writable)?;
        let grant = paging
            .authorize_capability_mapping(reference, space.id(), writable)
            .map_err(|_| cap::Error::Object)?;
        paging
            .map_memory_backing(space, &grant, 0, base, pages, permissions)
            .map_err(|_| cap::Error::Object)
    })
}
/// Trusted creator setup used only by the narrow-grant K8C profile: the
/// kernel creator authority installs the sender's payload mapping before the
/// creator token is released, so a received handle that lacks WRITE can still
/// be proven to deny writable mapping.
#[cfg(feature = "memory-object-runtime-test")]
#[allow(clippy::too_many_arguments)] // Trusted fixture setup: explicit Node, object, range and permissions.
pub fn map_memory_object_creator(
    paging: &mut Paging<'_, '_>,
    space: &mut UserSpace,
    object: MemoryObjectId,
    base: u64,
    pages: usize,
    permissions: Permissions,
) -> cap::Result<()> {
    tx(|_| {
        let grant = paging
            .authorize_memory_mapping(object, space.id(), permissions.write)
            .map_err(|_| cap::Error::Object)?;
        paging
            .map_memory_backing(space, &grant, 0, base, pages, permissions)
            .map_err(|_| cap::Error::Object)
    })
}
/// Drain dead MemoryObject capability objects into paging exactly once. A
/// full bounded pending batch is retried only after the architecture releases
/// earlier references.
#[cfg(feature = "memory-object-live-test")]
fn drain_memory_objects(s: &mut State, paging: &mut Paging<'_, '_>) -> cap::Result<()> {
    loop {
        let mut drained = 0;
        while let Some(reference) = s.objects.drain_memory_object() {
            paging
                .release_capability_reference(reference)
                .map_err(|_| cap::Error::Object)?;
            drained += 1;
        }
        let requeued = s.objects.retry_pending_memory_objects();
        if drained == 0 && requeued == 0 {
            return Ok(());
        }
    }
}
#[cfg(feature = "memory-object-cap-test")]
#[allow(clippy::type_complexity)] // Stable diagnostic schema returned to the stopped-CPU observer.
pub fn capability_diagnostics() -> cap::Result<([u64; 18], [u64; 32], cap::Stats, u32, usize)> {
    tx(|s| {
        Ok((
            s.objects.diagnostic_layout(),
            s.objects.channel_state().diagnostic_layout(),
            s.objects.stats(),
            s.objects.queued_references(),
            s.objects.pending_memory_objects(),
        ))
    })
}
pub fn enter(lease: Lease) -> cap::Result<()> {
    tx(|s| {
        if s.lease.is_some() || s.reader.is_some() {
            return Err(cap::Error::InUse);
        }
        s.lease = Some(lease);
        Ok(())
    })
}
pub fn leave(active: ActiveContext) -> cap::Result<Lease> {
    tx(|s| {
        if s.reader.is_some() || s.lease.as_ref().is_none_or(|l| l.active() != active) {
            return Err(cap::Error::Owner);
        }
        s.lease.take().ok_or(cap::Error::InUse)
    })
}
/// Controller-side wake admission. Registrations stay owned by the registry
/// while the Thread is queued; the pre-entry settle is the single commit point.
#[cfg(feature = "channel-wait-test")]
pub fn take_ready_waits() -> cap::Result<[Option<Waiter>; WAITERS]> {
    tx(|s| Ok(s.objects.take_ready_waits()))
}
#[cfg(feature = "channel-wait-test")]
#[allow(dead_code)]
pub fn settle(active: ActiveContext) -> cap::Result<Settle> {
    tx(|s| {
        if s.reader.is_some() {
            return Err(cap::Error::InUse);
        }
        let lease = s.lease.take().ok_or(cap::Error::Owner)?;
        let result = if lease.active() != active || lease.table().id().owner() != active.node {
            Err(cap::Error::Owner)
        } else {
            let settled = s.objects.settle_wait(active.thread);
            #[cfg(feature = "cap-move-test")]
            if matches!(settled, Settle::Committed(_)) {
                super::ipc::mark_wait_settled()?;
            }
            Ok(settled)
        };
        s.lease = Some(lease);
        result
    })
}
#[cfg(feature = "channel-wait-test")]
pub fn cancel_node_waits(node: NodeId) -> cap::Result<usize> {
    tx(|s| {
        if s.reader.is_some() {
            return Err(cap::Error::InUse);
        }
        Ok(s.objects.cancel_node_waits(node))
    })
}
#[cfg(feature = "channel-wait-test")]
pub fn wait_stats() -> cap::Result<neu_kernel::wait::Stats> {
    tx(|s| Ok(s.objects.wait_stats()))
}
#[cfg(feature = "channel-wait-test")]
#[allow(dead_code)]
pub fn wait_waiter(thread: neu_kernel::node::ThreadId) -> cap::Result<Option<Waiter>> {
    tx(|s| Ok(s.objects.wait_waiter(thread)))
}
unsafe fn read(
    pointer: *const (),
    active: ActiveContext,
    owner: NodeId,
    reference: &SpaceRef,
    va: u64,
) -> Result<u64, i64> {
    // SAFETY: Only with_reader constructs Reader, pointing to its borrowed view.
    // Its guard clears registration before either view or Paging borrow ends.
    // Calls occur synchronously through tx during that closure, never in IRQ.
    unsafe { (&*pointer.cast::<LiveInspection<'_>>()).inspect(active, owner, reference, va) }
}
struct Registration;
impl Drop for Registration {
    fn drop(&mut self) {
        tx(|s| {
            s.reader = None;
            Ok(())
        })
        .unwrap_or_else(|_| super::halt());
    }
}
pub fn with_reader(view: &LiveInspection<'_>, f: impl FnOnce()) -> cap::Result<()> {
    tx(|s| {
        if s.reader.is_some() || s.lease.is_none() {
            return Err(cap::Error::InUse);
        }
        s.reader = Some(Reader {
            pointer: ptr::from_ref(view).cast(),
            call: read,
        });
        Ok(())
    })?;
    let _scope = Registration;
    f();
    Ok(())
}
unsafe extern "C" {
    fn neu_as_request_probe();
}
fn publish(s: &mut State, r: Record, probe: bool) -> cap::Result<()> {
    if s.count == LIMIT {
        return Err(cap::Error::Capacity);
    }
    s.records[s.count] = r;
    s.count += 1;
    // SAFETY: Exclusive transaction, supervisor-only witness.
    unsafe {
        ptr::write_volatile(neu_as_witness.0.get(), r);
    }
    if probe {
        serial::write(format_args!(
            "[NEU] AS probe: sequence={} node={} thread={} op={:#x} status={}\n",
            r.words[0], r.words[1], r.words[2], r.words[6], r.words[9] as i64
        ))
        .map_err(|_| cap::Error::Object)?;
        // SAFETY: Bounded assembly pause, IF=0 on selected trusted stack.
        unsafe {
            neu_as_request_probe();
        }
    }
    Ok(())
}
pub fn request(active: ActiveContext, frame: Frame, address: u64, top: u64) -> Outcome {
    tx(|s| {
        // Bounded diagnostic capacity must not turn an extra scalar request
        // into loss of the live authority lease or a privileged panic.
        if s.count == LIMIT {
            return Ok(Outcome::Return {
                status: cap::Error::Capacity as i64,
                scalar: 0,
            });
        }
        let mut lease = s.lease.take().ok_or(cap::Error::Owner)?;
        if lease.active() != active
            || lease.table().id().owner() != active.node
            || paging::current_root() != active.context.root
        {
            return Err(cap::Error::Owner);
        }
        let op = frame.registers.rax;
        let h = Handle::from_raw(frame.registers.rdi);
        let va = frame.registers.rsi;
        let mut r = Record::EMPTY;
        r.words[..9].copy_from_slice(&[
            s.count as u64 + 1,
            active.node.raw(),
            active.thread.raw(),
            active.context.space.raw(),
            active.context.root,
            lease.table().id().raw(),
            op,
            h.raw(),
            va,
        ]);
        r.words[18..23].copy_from_slice(&[
            frame.context.rip,
            frame.context.rsp,
            top,
            address,
            interrupts::flags(),
        ]);
        #[cfg(feature = "channel-test")]
        if neu_kernel::syscall::v03::owns(op)
            || cfg!(feature = "cap-move-test")
                && (neu_kernel::syscall::v05::owns(op)
                    || op == neu_kernel::syscall::v05::TEST_READ
                    || op == neu_kernel::syscall::ABI_QUERY)
            || (cfg!(feature = "channel-wait-test") && neu_kernel::syscall::v04::owns(op))
            || (op == v02::CAP_CLOSE && ((h.raw() as u32) >= 5 || cfg!(feature = "cap-move-test")))
        {
            let outcome =
                super::ipc::request(&mut s.objects, &mut lease, active, frame, address, top)?;
            s.lease = Some(lease);
            return Ok(outcome);
        }
        #[cfg(feature = "memory-object-runtime-test")]
        if op == neu_kernel::syscall::v05::TEST_MAP {
            let outcome = mapping_request(s, &lease, active, frame, top)?;
            s.lease = Some(lease);
            return Ok(outcome);
        }
        let result = (|| -> Result<u64, i64> {
            if op == v02::BOOTSTRAP_CAP_GET {
                return lease
                    .table()
                    .bootstrap_get(&s.objects, active.node, h.raw())
                    .map(|h| h.raw())
                    .map_err(|e| e as i64);
            }
            if let Ok((id, rights)) = lease.table().describe(active.node, h) {
                r.words[11] = id.raw();
                r.words[12] = id.kind() as u64;
                r.words[13] = rights.bits();
            }
            if op == v02::CAP_CLOSE {
                return lease
                    .table_mut()
                    .close(&mut s.objects, active.node, h)
                    .map(|_| 0)
                    .map_err(|e| e as i64);
            }
            let guard = lease
                .table()
                .lookup(
                    &mut s.objects,
                    active.node,
                    h,
                    ObjectType::AddressSpace,
                    Rights::INSPECT,
                )
                .map_err(|e| e as i64)?;
            let (owner, reference) = guard.address_space().map_err(|e| e as i64)?;
            r.words[14] = guard.info().references as u64;
            r.words[15] = guard.info().pins as u64;
            r.words[16] = reference.id().raw();
            r.words[17] = reference.root();
            let reader = s.reader.ok_or(cap::Error::Owner as i64)?;
            // SAFETY: with_reader pins immutable view lifetime around this call.
            let scalar = unsafe { (reader.call)(reader.pointer, active, owner, reference, va) }?;
            r.words[10] = scalar;
            // Probe actual successful operation with guard/paging borrow live.
            let bit = match (active.thread.index(), va) {
                (0, 0x400000) => Some(0),
                (0, 0x406000) => Some(1),
                (2, 0x404000) => Some(if scalar == 0 { 3 } else { 2 }),
                (1, 0x406000) => Some(if frame.context.rsp == active.context.rsp - 80 {
                    9
                } else {
                    4
                }),
                _ => None,
            };
            let probe = bit.is_some_and(|b| s.probes & (1 << b) == 0);
            if probe {
                s.probes |= 1 << bit.unwrap();
            }
            r.words[23] = probe as u64;
            // Guard borrows only objects; publish uses disjoint metadata fields.
            if s.count == LIMIT {
                return Err(cap::Error::Capacity as i64);
            }
            s.records[s.count] = r;
            s.count += 1;
            // SAFETY: Same exclusive transaction; guard intentionally remains live.
            unsafe {
                ptr::write_volatile(neu_as_witness.0.get(), r);
            }
            if probe {
                serial::write(format_args!(
                    "[NEU] AS probe: sequence={} node={} thread={} op={:#x} status=0\n",
                    r.words[0], r.words[1], r.words[2], op
                ))
                .map_err(|_| cap::Error::Object as i64)?;
                unsafe {
                    neu_as_request_probe();
                }
            }
            Ok(scalar)
        })();
        let (status, scalar) = match result {
            Ok(v) => (0, v),
            Err(e) => (e, 0),
        };
        if op != v02::ADDRESS_SPACE_INSPECT || status != 0 {
            r.words[9] = status as u64;
            r.words[10] = scalar;
            let bit = match status {
                -16 => Some(5),
                -17 => Some(6),
                -18 => Some(7),
                -19 => Some(8),
                _ => None,
            };
            let probe = bit.is_some_and(|b| s.probes & (1 << b) == 0);
            if probe {
                s.probes |= 1 << bit.unwrap();
            }
            r.words[23] = probe as u64;
            publish(s, r, probe)?;
        }
        s.lease = Some(lease);
        Ok(Outcome::Return { status, scalar })
    })
    .unwrap_or_else(|_| {
        serial::emergency(format_args!(
            "[NEU] BOOT FAILURE: AddressSpace execution binding\n"
        ));
        super::halt()
    })
}
#[cfg(feature = "channel-test")]
#[allow(dead_code)]
pub fn channel_gates() -> cap::Result<u64> {
    tx(|s| super::ipc::gates(&s.objects))
}
/// Handle-only authority preflight. The guard is dropped before any State
/// mutation, so a rejection can never retain an object pin.
#[cfg(feature = "memory-object-runtime-test")]
fn request_authority(
    objects: &mut Objects,
    table: &cap::Table,
    active: ActiveContext,
    handle: Handle,
    writable: bool,
) -> cap::Result<(cap::ObjectId, MemoryObjectId, u32, u32)> {
    let guard = table.lookup(
        objects,
        active.node,
        handle,
        ObjectType::MemoryObject,
        Rights::MAP,
    )?;
    let object = guard.memory_object(writable)?.id();
    let info = guard.info();
    Ok((info.id, object, info.references, info.pins))
}
/// Commit-time revalidation of the received handle: exact slot, generation,
/// capability object and live paging object must still be the recorded ones.
#[cfg(feature = "memory-object-runtime-test")]
fn verify_commit_authority(
    objects: &mut Objects,
    table: &cap::Table,
    active: ActiveContext,
    request: &MapRequest,
) -> cap::Result<()> {
    let guard = table.lookup(
        objects,
        active.node,
        request.handle,
        ObjectType::MemoryObject,
        Rights::MAP,
    )?;
    let object = guard.memory_object(request.writable)?.id();
    if guard.info().id != request.object || object != request.memory_object {
        return Err(cap::Error::Stale);
    }
    Ok(())
}
/// One authoritative transaction: the guard pin spans authorization and the
/// complete mapping, so the exact capability generation cannot be collected
/// between validation and install.
#[cfg(feature = "memory-object-runtime-test")]
fn commit_mapping(
    objects: &mut Objects,
    table: &cap::Table,
    paging: &mut Paging<'_, '_>,
    space: &mut UserSpace,
    active: ActiveContext,
    request: &MapRequest,
) -> core::result::Result<(), neu_kernel::paging::Error> {
    let guard = table
        .lookup(
            objects,
            active.node,
            request.handle,
            ObjectType::MemoryObject,
            Rights::MAP,
        )
        .map_err(|_| neu_kernel::paging::Error::NotOwned)?;
    let reference = guard
        .memory_object(request.writable)
        .map_err(|_| neu_kernel::paging::Error::NotOwned)?;
    let grant = paging
        .authorize_capability_mapping(reference, request.space, request.writable)
        .map_err(|_| neu_kernel::paging::Error::NotOwned)?;
    paging.map_memory_backing_suspended(
        space,
        active.thread,
        &grant,
        request.offset,
        request.base,
        request.pages,
        request.permissions,
    )
}
/// Recoverable request rejection: publish bounded evidence and return the
/// status to Ring 3 without touching any capability, mapping or table state.
#[cfg(feature = "memory-object-runtime-test")]
fn reject_request(s: &mut State, mut r: Record, status: i64) -> cap::Result<Outcome> {
    r.words[9] = status as u64;
    publish(s, r, false)?;
    Ok(Outcome::Return { status, scalar: 0 })
}
/// Validate a TEST_MAP request against the requesting Thread's own live
/// capability handle. Nothing is mapped here: the request only becomes
/// admissible for the controller, which re-derives authority at the safe point.
#[cfg(feature = "memory-object-runtime-test")]
fn mapping_request(
    s: &mut State,
    lease: &Lease,
    active: ActiveContext,
    frame: Frame,
    top: u64,
) -> cap::Result<Outcome> {
    use neu_kernel::paging::{MEMORY_OBJECT_PAGES, PAGE, USER_BASE};
    let handle = Handle::from_raw(frame.registers.rdi);
    let base = frame.registers.rsi;
    let pages = frame.registers.rdx;
    let flags = frame.registers.r10;
    let offset = frame.registers.r8;
    let writable = flags == 1;
    let mut r = Record::EMPTY;
    r.words[..9].copy_from_slice(&[
        s.count as u64 + 1,
        active.node.raw(),
        active.thread.raw(),
        active.context.space.raw(),
        active.context.root,
        lease.table().id().raw(),
        neu_kernel::syscall::v05::TEST_MAP,
        handle.raw(),
        base,
    ]);
    r.words[18..23].copy_from_slice(&[
        frame.context.rip,
        frame.context.rsp,
        top,
        0,
        interrupts::flags(),
    ]);
    if let Ok((id, rights)) = lease.table().describe(active.node, handle) {
        r.words[11] = id.raw();
        r.words[12] = id.kind() as u64;
        r.words[13] = rights.bits();
    }
    if flags > 1
        || pages == 0
        || pages > MEMORY_OBJECT_PAGES as u64
        || offset >= MEMORY_OBJECT_PAGES as u64
        || offset + pages > MEMORY_OBJECT_PAGES as u64
        || !base.is_multiple_of(PAGE)
        || neu_kernel::paging::user_range(base, pages as usize).is_err()
        || base < USER_BASE + 0x10000
    {
        return reject_request(s, r, neu_kernel::syscall::INVALID_ARGUMENT);
    }
    if s.request.is_some() {
        return reject_request(s, r, cap::Error::InUse as i64);
    }
    let lookup = request_authority(&mut s.objects, lease.table(), active, handle, writable);
    let (object_id, object, references, pins) = match lookup {
        Ok(authority) => authority,
        Err(error) => return reject_request(s, r, error as i64),
    };
    let sequence = s.requests.checked_add(1).ok_or(cap::Error::Capacity)?;
    s.requests = sequence;
    s.request = Some(MapRequest {
        sequence,
        node: active.node,
        thread: active.thread,
        table: lease.table().id().raw(),
        space: active.context.space,
        root: active.context.root,
        handle,
        object: object_id,
        memory_object: object,
        base,
        offset: offset as usize,
        pages: pages as usize,
        permissions: if writable {
            Permissions::RW
        } else {
            Permissions::R
        },
        writable,
        state: RequestState::Pending,
        status: 0,
    });
    r.words[10] = sequence;
    r.words[14] = references as u64;
    r.words[15] = pins as u64;
    r.words[16] = object.raw();
    r.words[17] = offset;
    witness_write(s);
    publish(s, r, false)?;
    Ok(Outcome::Request)
}
/// Controller commit at the scheduler safe point. The destination Thread is
/// not in Ring 3, the root is not active, and its execution pin must be the
/// only pin. Authority is re-derived from the live received handle; the pin
/// held by this lookup spans authorization and the whole mapping transaction.
#[cfg(feature = "memory-object-runtime-test")]
pub fn commit_request(
    nodes: &mut Nodes,
    paging: &mut Paging<'_, '_>,
    space: &mut UserSpace,
    active: ActiveContext,
    sequence: u64,
) -> cap::Result<i64> {
    tx(|s| {
        if s.lease.is_some() {
            return Err(cap::Error::InUse);
        }
        let mut request = s.request.ok_or(cap::Error::Owner)?;
        if request.sequence != sequence || request.state != RequestState::Pending {
            return Err(cap::Error::InUse);
        }
        let thread = model(nodes.thread(request.thread))?;
        let table = model(nodes.authority_mut(active.node))?;
        if request.node != active.node
            || request.thread != active.thread
            || request.space != active.context.space
            || request.root != active.context.root
            || request.table != table.id().raw()
            || thread.state != neu_kernel::node::ThreadState::Runnable
            || thread.saved.is_none()
            || thread.queued
            || space.id() != request.space
            || space.root != request.root
            || paging::current_root() != paging.root()
            || s.count == LIMIT
        {
            return Err(cap::Error::Owner);
        }
        if let Err(error) = verify_commit_authority(&mut s.objects, table, active, &request) {
            request.state = RequestState::Rejected;
            request.status = error as i64;
            s.request = None;
            s.last_request = Some(request);
            witness_write(s);
            return Ok(error as i64);
        }
        let mapped = commit_mapping(&mut s.objects, table, paging, space, active, &request);
        let status = match mapped {
            Ok(()) => {
                request.state = RequestState::Committed;
                request.status = 0;
                0
            }
            Err(neu_kernel::paging::Error::Corrupt) => return Err(cap::Error::Object),
            Err(_) => {
                request.state = RequestState::Rejected;
                request.status = v02::BAD_MAPPING;
                v02::BAD_MAPPING
            }
        };
        s.request = None;
        s.last_request = Some(request);
        witness_write(s);
        let mut r = Record::EMPTY;
        r.words[..9].copy_from_slice(&[
            s.count as u64 + 1,
            request.node.raw(),
            request.thread.raw(),
            request.space.raw(),
            request.root,
            request.table,
            neu_kernel::syscall::v05::TEST_MAP,
            request.handle.raw(),
            request.base,
        ]);
        r.words[9] = status as u64;
        r.words[10] = request.sequence;
        r.words[11] = request.object.raw();
        r.words[16] = request.memory_object.raw();
        r.words[17] = request.offset as u64;
        r.words[19] = request.pages as u64;
        r.words[20] = u64::from(request.writable);
        r.words[21] = request.state as u64;
        r.words[23] = 1;
        publish(s, r, false)?;
        Ok(status)
    })
}
/// Cancel a pending request before its Thread is torn down. The Thread must
/// already be Exited in the model, so no later resume or dispatch can revive
/// it; the ledger entry becomes terminal and can never commit.
#[cfg(feature = "memory-object-runtime-test")]
pub fn cancel_request(nodes: &mut Nodes, active: ActiveContext, sequence: u64) -> cap::Result<()> {
    tx(|s| {
        if s.lease.is_some() {
            return Err(cap::Error::InUse);
        }
        let table = model(nodes.authority(active.node))?;
        let mut request = s.request.ok_or(cap::Error::Owner)?;
        if request.sequence != sequence
            || request.state != RequestState::Pending
            || request.node != active.node
            || request.thread != active.thread
            || request.table != table.id().raw()
            || model(nodes.thread(active.thread))?.state != neu_kernel::node::ThreadState::Exited
        {
            return Err(cap::Error::Owner);
        }
        request.state = RequestState::Cancelled;
        request.status = 0;
        s.request = None;
        s.last_request = Some(request);
        s.cancelled += 1;
        witness_write(s);
        serial::write(format_args!(
            "[NEU] Map request cancelled: sequence={} node={} thread={} base={:#x} pages={}\n",
            request.sequence,
            request.node.raw(),
            request.thread.raw(),
            request.base,
            request.pages
        ))
        .map_err(|_| cap::Error::Object)
    })
}
/// Trusted controller read of a sealed bootstrap handle. The handle carries no
/// authority by itself; every operation still goes through `lookup`.
#[cfg(feature = "memory-object-runtime-test")]
pub fn bootstrap_handle(nodes: &Nodes, node: NodeId, index: u64) -> cap::Result<Handle> {
    tx(|s| {
        let table = model(nodes.authority(node))?;
        table.bootstrap_get(&s.objects, node, index)
    })
}
/// Trusted controller close used by the K8C fixture to free the exact slot the
/// receive transaction will reuse, and to close the received capability.
#[cfg(feature = "memory-object-runtime-test")]
pub fn close_handle(nodes: &mut Nodes, node: NodeId, handle: Handle) -> cap::Result<()> {
    tx(|s| {
        let table = model(nodes.authority_mut(node))?;
        table.close(&mut s.objects, node, handle)
    })
}
/// Capability slot verdict without retaining a guard or pin: 0 for a live
/// handle, otherwise the exact capability error number.
#[cfg(feature = "memory-object-runtime-test")]
pub fn handle_status(nodes: &Nodes, node: NodeId, handle: Handle) -> cap::Result<i64> {
    tx(|_| {
        let table = model(nodes.authority(node))?;
        Ok(match table.describe(node, handle) {
            Ok(_) => 0,
            Err(error) => error as i64,
        })
    })
}
/// Published address of the bounded request witness. It carries identity and
/// outcome metadata only; QMP reads it while the CPU is stopped.
#[cfg(feature = "memory-object-runtime-test")]
pub fn map_witness_address() -> u64 {
    ptr::addr_of!(neu_map_witness) as u64
}
#[cfg(feature = "memory-object-runtime-test")]
pub fn request_diagnostics() -> cap::Result<[u64; 48]> {
    tx(|s| {
        let mut words = [0u64; 48];
        if let Some(request) = s.request {
            words[..20].copy_from_slice(&request.layout());
        }
        if let Some(request) = s.last_request {
            words[20..40].copy_from_slice(&request.layout());
        }
        words[40] = s.requests;
        words[41] = s.cancelled;
        words[42] = u64::from(s.request.is_some());
        words[43] = u64::from(s.last_request.is_some());
        Ok(words)
    })
}
pub fn reap(paging: &mut Paging<'_, '_>) -> cap::Result<()> {
    tx(|s| {
        if s.reader.is_some() || s.lease.is_some() {
            return Err(cap::Error::InUse);
        }
        while let Some(r) = s.objects.reap_space() {
            paging
                .release_object_space(r)
                .map_err(|_| cap::Error::Object)?;
        }
        #[cfg(feature = "memory-object-live-test")]
        drain_memory_objects(s, paging)?;
        Ok(())
    })
}
pub fn cleanup(nodes: &mut Nodes, node: NodeId, paging: &mut Paging<'_, '_>) -> cap::Result<()> {
    tx(|s| {
        if s.reader.is_some()
            || s.lease.is_some()
            || nodes.active().is_some()
            || !matches!(
                model(nodes.node(node))?.state,
                neu_kernel::node::NodeState::Exited | neu_kernel::node::NodeState::Quiesced
            )
            || s.objects.node_wait_count(node) != 0
        {
            return Err(cap::Error::InUse);
        }
        // A pending mapping request dies with its Node. The model must have
        // already exited the suspended Thread; the ledger can then never
        // commit for a retired root.
        #[cfg(feature = "memory-object-runtime-test")]
        if let Some(request) = s.request.filter(|request| request.node == node) {
            if model(nodes.thread(request.thread))?.state != neu_kernel::node::ThreadState::Exited {
                return Err(cap::Error::InUse);
            }
            let mut request = request;
            request.state = RequestState::Cancelled;
            request.status = 0;
            s.request = None;
            s.last_request = Some(request);
            s.cancelled += 1;
            witness_write(s);
            serial::write(format_args!(
                "[NEU] Map request cancelled: sequence={} node={} thread={} base={:#x} pages={} reason=node-exit\n",
                request.sequence,
                request.node.raw(),
                request.thread.raw(),
                request.base,
                request.pages
            ))
            .map_err(|_| cap::Error::Object)?;
        }
        let before = s.objects.stats();
        #[cfg(feature = "channel-test")]
        let channel_before = s.objects.channel_state().snapshot();
        let queued_before = s.objects.queued_references();
        let count = model(nodes.authority_mut(node))?.drain(&mut s.objects, node)?;
        #[cfg(feature = "channel-test")]
        super::ipc::cleanup(node, channel_before, &s.objects)?;
        while let Some(r) = s.objects.reap_space() {
            paging
                .release_object_space(r)
                .map_err(|_| cap::Error::Object)?;
        }
        #[cfg(feature = "memory-object-live-test")]
        drain_memory_objects(s, paging)?;
        let after = s.objects.stats();
        let node_pins = after.pins.saturating_sub(s.objects.pins_outside_node(node));
        let queued_discarded = queued_before
            .checked_sub(s.objects.queued_references())
            .ok_or(cap::Error::Object)?;
        if before.references.checked_sub(after.references)
            != (count as u32).checked_add(queued_discarded)
            || node_pins != 0
        {
            return Err(cap::Error::Object);
        }
        #[cfg(feature = "cap-move-test")]
        serial::write(format_args!("[NEU] Move cleanup: node={} handles={} queued_discarded={} references_released={} pins=0\n",node.raw(),count,queued_discarded,before.references-after.references)).map_err(|_|cap::Error::Object)?;
        serial::write(format_args!("[NEU] AS cleanup: node={} handles={} objects_before={} after={} references_before={} after={} pins=0 root_ref=released lease=none\n",node.raw(),count,before.live,after.live,before.references,after.references)).map_err(|_|cap::Error::Object)
    })
}
pub fn finish() -> cap::Result<()> {
    tx(|s| {
        if s.reader.is_some()
            || s.lease.is_some()
            || s.objects.stats() != cap::Stats::default()
            || s.objects.pending_memory_objects() != 0
        {
            return Err(cap::Error::Object);
        }
        #[cfg(feature = "channel-test")]
        super::ipc::finish(&s.objects)?;
        for r in &s.records[..s.count] {
            serial::write(format_args!("[NEU] AS record: words="))
                .map_err(|_| cap::Error::Object)?;
            for (i, v) in r.words.iter().enumerate() {
                serial::write(format_args!("{}{:#x}", if i == 0 { "" } else { "," }, v))
                    .map_err(|_| cap::Error::Object)?;
            }
            serial::write(format_args!("\n")).map_err(|_| cap::Error::Object)?;
        }
        serial::write(format_args!("[NEU] AS accounting restored: objects=0 references=0 pins=0 root_refs=0 leases=0 requests={}\n",s.count)).map_err(|_|cap::Error::Object)
    })
}

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
use neu_kernel::{
    capability::{self as cap, Handle, Lease, ObjectType, Objects, Rights},
    node::{ActiveContext, NodeId, Nodes},
    paging::SpaceRef,
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
struct State {
    objects: Objects,
    lease: Option<Lease>,
    reader: Option<Reader>,
    count: usize,
    probes: u64,
    records: [Record; LIMIT],
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
}));
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
            let flag = s.objects.flag(true)?;
            let t = model(nodes.authority_mut(owner))?;
            for (index, rights) in [
                (0, Rights::INSPECT),
                (1, Rights::INSPECT),
                (2, Rights::NONE),
            ] {
                t.install_bootstrap(&mut s.objects, &object, rights, index)?;
            }
            t.install_bootstrap(&mut s.objects, &flag, Rights::READ, 3)?;
            creators[n * 2] = Some(object);
            creators[n * 2 + 1] = Some(flag);
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
        if neu_kernel::syscall::v03::owns(op) || (op == v02::CAP_CLOSE && (h.raw() as u32) >= 5) {
            let outcome =
                super::ipc::request(&mut s.objects, &mut lease, active, frame, address, top)?;
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
pub fn channel_gates() -> cap::Result<u64> {
    tx(|s| super::ipc::gates(&s.objects))
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
        Ok(())
    })
}
pub fn cleanup(nodes: &mut Nodes, node: NodeId, paging: &mut Paging<'_, '_>) -> cap::Result<()> {
    tx(|s| {
        if s.reader.is_some()
            || s.lease.is_some()
            || nodes.active().is_some()
            || model(nodes.node(node))?.state != neu_kernel::node::NodeState::Exited
        {
            return Err(cap::Error::InUse);
        }
        let before = s.objects.stats();
        #[cfg(feature = "channel-test")]
        let channel_before = s.objects.channel_state().snapshot();
        let count = model(nodes.authority_mut(node))?.drain(&mut s.objects, node)?;
        #[cfg(feature = "channel-test")]
        super::ipc::cleanup(node, channel_before, &s.objects)?;
        while let Some(r) = s.objects.reap_space() {
            paging
                .release_object_space(r)
                .map_err(|_| cap::Error::Object)?;
        }
        let after = s.objects.stats();
        if before.references - after.references != count as u32 || after.pins != 0 {
            return Err(cap::Error::Object);
        }
        serial::write(format_args!("[NEU] AS cleanup: node={} handles={} objects_before={} after={} references_before={} after={} pins=0 root_ref=released lease=none\n",node.raw(),count,before.live,after.live,before.references,after.references)).map_err(|_|cap::Error::Object)
    })
}
pub fn finish() -> cap::Result<()> {
    tx(|s| {
        if s.reader.is_some() || s.lease.is_some() || s.objects.stats() != cap::Stats::default() {
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

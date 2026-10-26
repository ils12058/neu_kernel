//! Concrete single-CPU native capability test boundary. The controller moves
//! the Node table here; handlers never borrow its suspended Nodes or Paging.
use super::{interrupts, paging, serial};
use core::{
    cell::UnsafeCell,
    ptr,
    sync::atomic::{AtomicBool, Ordering},
};
use neu_kernel::{
    capability::{self as cap, Handle, Lease, ObjectId, ObjectType, Objects, Rights},
    node::{ActiveContext, NodeId, Nodes},
    syscall::{Frame, Outcome},
};
pub const READ: u64 = 0xffff_ffff_ffff_fd00;
pub const WRITE: u64 = READ + 1;
pub const CLOSE: u64 = READ + 2;
pub const EXCHANGE: u64 = READ + 3;
pub const FLAG: u64 = READ + 4;
pub const READ_CLOSE: u64 = READ + 5;
const MAX_REQUESTS: usize = 96;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Record {
    pub words: [u64; 24],
}
impl Record {
    const EMPTY: Self = Self { words: [0; 24] };
}
struct State {
    objects: Objects,
    lease: Option<Lease>,
    a: Option<NodeId>,
    b: Option<NodeId>,
    counter: Option<ObjectId>,
    other: Option<ObjectId>,
    solo: Option<ObjectId>,
    replacement: Option<ObjectId>,
    regranted: bool,
    probes: u16,
    count: usize,
    records: [Record; MAX_REQUESTS],
}
#[repr(transparent)]
struct Storage(UnsafeCell<State>);
// SAFETY: Exactly one CPU, IF=0 at every access, guarded non-reentrant borrow.
// Requests occur only inside the native session holding the moved table lease.
// No IRQ/NMI handler accesses this storage; critical nesting remains fail-stop.
// The transaction closure cannot return a reference into State. No mutable
// Nodes/Paging reference is stored here or accessed by a syscall handler.
unsafe impl Sync for Storage {}
#[unsafe(no_mangle)]
static neu_cap_storage: Storage = Storage(UnsafeCell::new(State {
    objects: Objects::new(),
    lease: None,
    a: None,
    b: None,
    counter: None,
    other: None,
    solo: None,
    replacement: None,
    regranted: false,
    probes: 0,
    count: 0,
    records: [Record::EMPTY; MAX_REQUESTS],
}));
#[repr(transparent)]
struct Witness(UnsafeCell<Record>);
// SAFETY: Written only within the same exclusive IF=0 transaction as Storage.
// QMP reads paused CPU memory externally; userspace has no mapping permission.
unsafe impl Sync for Witness {}
#[unsafe(no_mangle)]
static neu_cap_witness: Witness = Witness(UnsafeCell::new(Record::EMPTY));
#[unsafe(no_mangle)]
static neu_cap_storage_bytes: u64 = core::mem::size_of::<State>() as u64;
static BORROWED: AtomicBool = AtomicBool::new(false);
struct Borrow;
impl Drop for Borrow {
    fn drop(&mut self) {
        BORROWED.store(false, Ordering::Release);
    }
}
fn transaction<R>(f: impl for<'a> FnOnce(&'a mut State) -> cap::Result<R>) -> cap::Result<R> {
    if interrupts::flags() & 0x600 != 0
        || BORROWED
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
    {
        return Err(cap::Error::InUse);
    }
    let _borrow = Borrow;
    // SAFETY: The concrete access contract and exclusive atomic gate above.
    f(unsafe { &mut *neu_cap_storage.0.get() })
}
fn model<T>(r: neu_kernel::node::Result<T>) -> cap::Result<T> {
    r.map_err(|_| cap::Error::Owner)
}
pub fn bootstrap(nodes: &mut Nodes, a: NodeId, b: NodeId) -> cap::Result<()> {
    transaction(|s| {
        if s.lease.is_some() || s.objects.stats() != cap::Stats::default() {
            return Err(cap::Error::InUse);
        }
        // SAFETY: Own immutable diagnostic size; volatile use retains the QMP symbol.
        let storage_bytes = unsafe { ptr::read_volatile(ptr::addr_of!(neu_cap_storage_bytes)) };
        if storage_bytes != core::mem::size_of::<State>() as u64 {
            return Err(cap::Error::Object);
        }
        let counter = s.objects.counter(10)?;
        let other = s.objects.counter(200)?;
        let flag = s.objects.flag(true)?;
        let solo = s.objects.counter(9)?;
        s.a = Some(a);
        s.b = Some(b);
        s.counter = Some(counter.id());
        s.other = Some(other.id());
        s.solo = Some(solo.id());
        for (rights, expected) in [
            (Rights::READ_WRITE, 1),
            (Rights::READ, 2),
            (Rights::WRITE, 3),
        ] {
            let h = model(nodes.authority_mut(a))?.grant(&mut s.objects, &counter, rights)?;
            if h.raw() != 0x100000000 + expected {
                return Err(cap::Error::Handle);
            }
        }
        if model(nodes.authority_mut(a))?
            .grant(&mut s.objects, &flag, Rights::READ)?
            .raw()
            != 0x100000004
            || model(nodes.authority_mut(a))?
                .grant(&mut s.objects, &solo, Rights::READ)?
                .raw()
                != 0x100000005
            || model(nodes.authority_mut(b))?
                .grant(&mut s.objects, &other, Rights::READ_WRITE)?
                .raw()
                != 0x100000001
        {
            return Err(cap::Error::Handle);
        }
        if cfg!(feature = "test-capabilities-rollback") {
            let t = model(nodes.authority_mut(a))?;
            let mut extra = [Handle::from_raw(0); 3];
            for h in &mut extra {
                *h = t.grant(&mut s.objects, &counter, Rights::READ)?;
            }
            let before = s.objects.stats();
            if t.grant(&mut s.objects, &counter, Rights::READ) != Err(cap::Error::Capacity)
                || s.objects.stats() != before
                || s.objects.counter(999).err() != Some(cap::Error::Capacity)
                || s.objects.stats() != before
                || t.grant(&mut s.objects, &flag, Rights::WRITE) != Err(cap::Error::Rights)
                || s.objects.stats() != before
            {
                return Err(cap::Error::Object);
            }
            for h in extra {
                t.close(&mut s.objects, a, h)?;
            }
            serial::write(format_args!("[NEU] Cap rollback: table=full objects=full invalid_rights=rejected references=restored entries=5,1\n")).map_err(|_|cap::Error::Object)?;
        }
        for reference in [counter, other, flag, solo] {
            s.objects.release(reference)?;
        }
        let stats = s.objects.stats();
        if stats
            != (cap::Stats {
                live: 4,
                references: 6,
                pins: 0,
            })
        {
            return Err(cap::Error::Object);
        }
        serial::write(format_args!("[NEU] Cap bootstrap: A={} table={} B={} table={} counter={} other={} solo={} objects={} references={} handles=5,1 ABI=0.1 test_only=true\n",a.raw(),model(nodes.authority(a))?.id().raw(),b.raw(),model(nodes.authority(b))?.id().raw(),s.counter.unwrap().raw(),s.other.unwrap().raw(),s.solo.unwrap().raw(),stats.live,stats.references)).map_err(|_|cap::Error::Object)
    })
}
pub fn enter(lease: Lease) -> cap::Result<()> {
    transaction(|s| {
        if s.lease.is_some() {
            return Err(cap::Error::InUse);
        }
        s.lease = Some(lease);
        Ok(())
    })
}
pub fn leave(active: ActiveContext) -> cap::Result<Lease> {
    transaction(|s| {
        if s.lease.as_ref().is_none_or(|l| l.active() != active) {
            return Err(cap::Error::Owner);
        }
        s.lease.take().ok_or(cap::Error::InUse)
    })
}
fn publish(
    records: &mut [Record; MAX_REQUESTS],
    count: &mut usize,
    record: Record,
    probe: bool,
) -> cap::Result<()> {
    if *count == MAX_REQUESTS {
        return Err(cap::Error::Capacity);
    }
    records[*count] = record;
    *count += 1;
    // SAFETY: Exclusive transaction, no normal/IRQ reader retains a reference.
    unsafe {
        ptr::write_volatile(neu_cap_witness.0.get(), record);
    }
    if probe {
        serial::write(format_args!(
            "[NEU] Cap probe: sequence={} node={} thread={} op={:#x} status={}\n",
            record.words[0],
            record.words[1],
            record.words[2],
            record.words[6],
            record.words[9] as i64
        ))
        .map_err(|_| cap::Error::Object)?;
        // SAFETY: Bounded IF=0 assembly observation hold on the selected stack.
        unsafe {
            neu_cap_request_probe();
        }
    }
    Ok(())
}
unsafe extern "C" {
    fn neu_cap_request_probe();
}
pub fn request(active: ActiveContext, frame: Frame, frame_address: u64, top: u64) -> Outcome {
    let result = transaction(|s| {
        let mut lease = s.lease.take().ok_or(cap::Error::Owner)?;
        if lease.active() != active
            || lease.table().id().owner() != active.node
            || paging::current_root() != active.context.root
        {
            return Err(cap::Error::Owner);
        }
        let op = frame.registers.rax;
        let handle = Handle::from_raw(frame.registers.rdi);
        let value = frame.registers.rsi;
        let target = lease.table().describe(active.node, handle).ok();
        let mut record = Record::EMPTY;
        record.words[..9].copy_from_slice(&[
            s.count as u64 + 1,
            active.node.raw(),
            active.thread.raw(),
            active.context.space.raw(),
            active.context.root,
            lease.table().id().raw(),
            op,
            handle.raw(),
            value,
        ]);
        record.words[18..23].copy_from_slice(&[
            frame.context.rip,
            frame.context.rsp,
            top,
            frame_address,
            interrupts::flags(),
        ]);
        if let Some((id, rights)) = target {
            record.words[11] = id.raw();
            record.words[12] = id.kind() as u64;
            record.words[13] = rights.bits();
        }
        let probe_bit = match (active.thread.index(), op, handle.raw()) {
            (0, READ, 0x100000001) => Some(0),
            (0, WRITE, 0x100000002) => Some(1),
            (0, READ, 0x100000004) => Some(2),
            (0, READ_CLOSE, _) => Some(3),
            (1, READ, 0x100000001) => Some(4),
            (2, READ, 0x100000001) => Some(if target.is_some() { 5 } else { 6 }),
            (2, READ, 0x200000001) => Some(if frame.context.rsp == active.context.rsp - 80 {
                8
            } else {
                7
            }),
            _ => None,
        };
        let probe = probe_bit.is_some_and(|bit| s.probes & (1 << bit) == 0);
        if probe {
            s.probes |= 1 << probe_bit.unwrap();
        }
        let operation = (|| {
            if op == CLOSE {
                lease
                    .table_mut()
                    .close(&mut s.objects, active.node, handle)?;
                return Ok(0);
            }
            let kind = if op == FLAG {
                ObjectType::Flag
            } else {
                ObjectType::Counter
            };
            let rights = match op {
                WRITE => Rights::WRITE,
                EXCHANGE => Rights::READ_WRITE,
                _ => Rights::READ,
            };
            let mut guard =
                lease
                    .table()
                    .lookup(&mut s.objects, active.node, handle, kind, rights)?;
            let scalar = match op {
                READ | FLAG => guard.read()?,
                WRITE => {
                    guard.write(value)?;
                    0
                }
                EXCHANGE => guard.exchange(value)?,
                READ_CLOSE => {
                    guard.close_handle(lease.table_mut(), handle)?;
                    guard.read()?
                }
                _ => return Err(cap::Error::Handle),
            };
            let info = guard.info();
            record.words[14] = info.references as u64;
            record.words[15] = info.pins as u64;
            record.words[16] = guard.value_address();
            record.words[17] = guard.read().unwrap_or(value);
            record.words[10] = scalar;
            record.words[23] = probe as u64;
            // Publish while the real operation pin is live (including refs=0).
            publish(&mut s.records, &mut s.count, record, probe)?;
            Ok(scalar)
        })();
        match operation {
            Ok(scalar) => {
                if op == CLOSE {
                    if let Some((id, _)) = target
                        && let Ok(info) = s.objects.info(id)
                    {
                        record.words[14] = info.references as u64;
                        record.words[15] = info.pins as u64;
                        record.words[16] = s.objects.value_address(id)?;
                        record.words[17] = s.objects.debug_value(id)?;
                    }
                    record.words[10] = scalar;
                    publish(&mut s.records, &mut s.count, record, false)?;
                }
                s.lease = Some(lease);
                Ok(Outcome::Return { status: 0, scalar })
            }
            Err(error) => {
                if let Some((id, _)) = target
                    && let Ok(info) = s.objects.info(id)
                {
                    record.words[14] = info.references as u64;
                    record.words[15] = info.pins as u64;
                    record.words[16] = s.objects.value_address(id)?;
                    record.words[17] = s.objects.debug_value(id)?;
                }
                record.words[9] = error as i64 as u64;
                record.words[10] = 0;
                record.words[23] = probe as u64;
                publish(&mut s.records, &mut s.count, record, probe)?;
                s.lease = Some(lease);
                Ok(Outcome::Return {
                    status: error as i64,
                    scalar: 0,
                })
            }
        }
    });
    result.unwrap_or_else(|_| {
        serial::emergency(format_args!(
            "[NEU] BOOT FAILURE: capability execution binding\n"
        ));
        super::halt()
    })
}
pub fn private(number: u64) -> bool {
    (READ..=READ_CLOSE).contains(&number)
}
pub fn regrant(nodes: &mut Nodes, a: NodeId) -> cap::Result<u64> {
    transaction(|s| {
        if s.lease.is_some() || s.regranted {
            return Err(cap::Error::InUse);
        }
        // This fixed owner/value/rights grant is preauthorized by bootstrap;
        // userspace phase words provide ordering only. Require the real checked
        // close/lifetime/stale-lookup transactions before executing that plan.
        if s.a != Some(a) {
            return Err(cap::Error::Owner);
        }
        let checked = &s.records[..s.count];
        let closed = checked.iter().any(|r| {
            r.words[1] == a.raw()
                && r.words[6] == CLOSE
                && r.words[7] == 0x100000001
                && r.words[9] == 0
        });
        let last_ref = checked.iter().any(|r| {
            r.words[1] == a.raw()
                && r.words[6] == READ_CLOSE
                && r.words[7] == 0x100000005
                && r.words[9] == 0
                && r.words[14] == 0
                && r.words[15] == 1
        });
        let stale = checked.iter().any(|r| {
            r.words[1] == a.raw()
                && r.words[6] == READ
                && r.words[7] == 0x100000001
                && r.words[9] == cap::Error::Stale as i64 as u64
        });
        if !closed || !last_ref || !stale {
            return Err(cap::Error::InUse);
        }
        let old = s.solo.ok_or(cap::Error::Object)?;
        if s.objects.info(old) != Err(cap::Error::Object) {
            return Err(cap::Error::Object);
        }
        let r = s.objects.counter(77)?;
        let id = r.id();
        let handle =
            model(nodes.authority_mut(a))?.grant(&mut s.objects, &r, Rights::READ_WRITE)?;
        if handle.raw() != 0x200000001 || id.raw() == old.raw() {
            return Err(cap::Error::Stale);
        }
        s.objects.release(r)?;
        s.replacement = Some(id);
        s.regranted = true;
        serial::write(format_args!("[NEU] Cap regrant: node={} old_object={} new_object={} old_handle={:#x} new_handle={:#x} initial=77\n",a.raw(),old.raw(),id.raw(),0x100000001u64,handle.raw())).map_err(|_|cap::Error::Object)?;
        Ok(handle.raw())
    })
}
pub fn cleanup(nodes: &mut Nodes, node: NodeId) -> cap::Result<()> {
    transaction(|s| {
        if s.lease.is_some()
            || nodes.active().is_some()
            || model(nodes.node(node))?.state != neu_kernel::node::NodeState::Exited
        {
            return Err(cap::Error::InUse);
        }
        let before = s.objects.stats();
        let count = model(nodes.authority_mut(node))?.drain(&mut s.objects, node)?;
        let after = s.objects.stats();
        if before.references - after.references != count as u32 || after.pins != 0 {
            return Err(cap::Error::Object);
        }
        serial::write(format_args!("[NEU] Cap Node cleanup: node={} handles={} objects_before={} after={} references_before={} after={} pins=0 lease=none\n",node.raw(),count,before.live,after.live,before.references,after.references)).map_err(|_|cap::Error::Object)
    })
}
pub fn finish() -> cap::Result<()> {
    transaction(|s| {
        if s.lease.is_some() || s.objects.stats() != cap::Stats::default() || !s.regranted {
            return Err(cap::Error::Object);
        }
        for r in &s.records[..s.count] {
            serial::write(format_args!("[NEU] Cap record: words="))
                .map_err(|_| cap::Error::Object)?;
            for (i, v) in r.words.iter().enumerate() {
                serial::write(format_args!("{}{:#x}", if i == 0 { "" } else { "," }, v))
                    .map_err(|_| cap::Error::Object)?;
            }
            serial::write(format_args!("\n")).map_err(|_| cap::Error::Object)?;
        }
        serial::write(format_args!("[NEU] Cap accounting restored: objects=0 references=0 pins=0 leases=0 requests={} public_ABI=unchanged\n",s.count)).map_err(|_|cap::Error::Object)
    })
}

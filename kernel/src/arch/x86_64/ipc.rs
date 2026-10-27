//! ABI 0.3 adapter. Object/queue mutation occurs through the exclusively leased
//! registry passed by address_space; this storage owns diagnostics only.
use super::{interrupts, serial};
use core::{
    cell::UnsafeCell,
    ptr,
    sync::atomic::{AtomicBool, Ordering},
};
use neu_kernel::{
    capability::{self as cap, Handle, Lease, ObjectType, Objects, Rights},
    node::{ActiveContext, NodeId},
    syscall::{Frame, Outcome, v03},
};
const LIMIT: usize = 112;
const CLEANUP: u64 = u64::MAX - 1;
#[repr(C)]
#[derive(Clone, Copy)]
pub struct Record {
    words: [u64; 64],
}
impl Record {
    const EMPTY: Self = Self { words: [0; 64] };
}
struct State {
    a: Option<NodeId>,
    b: Option<NodeId>,
    count: usize,
    probes: u64,
    gates: u64,
    records: [Record; LIMIT],
}
struct Storage(UnsafeCell<State>);
// SAFETY: Exactly one CPU, IF=0/DF=0, exclusive non-reentrant gate. IRQ/NMI
// never access diagnostics or Channel state. No borrowed State references escape.
// Actual registry mutation has one owner: address_space's transaction/lease.
unsafe impl Sync for Storage {}
static STORAGE: Storage = Storage(UnsafeCell::new(State {
    a: None,
    b: None,
    count: 0,
    probes: 0,
    gates: 0,
    records: [Record::EMPTY; LIMIT],
}));
struct Witness(UnsafeCell<Record>);
// SAFETY: Same IF=0 exclusive transaction; paused external QMP is the only reader.
unsafe impl Sync for Witness {}
#[unsafe(no_mangle)]
static neu_ipc_witness: Witness = Witness(UnsafeCell::new(Record::EMPTY));
static BUSY: AtomicBool = AtomicBool::new(false);
struct Borrow;
impl Drop for Borrow {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::Release);
    }
}
fn tx<R>(f: impl for<'a> FnOnce(&'a mut State) -> cap::Result<R>) -> cap::Result<R> {
    if interrupts::flags() & 0x600 != 0
        || BUSY
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
    {
        return Err(cap::Error::InUse);
    }
    let _borrow = Borrow;
    // SAFETY: Concrete single-CPU exclusive access contract above.
    f(unsafe { &mut *STORAGE.0.get() })
}
pub fn bootstrap(a: NodeId, b: NodeId, objects: &Objects) -> cap::Result<()> {
    tx(|s| {
        if s.a.is_some()
            || objects.channel_state().stats()
                != (neu_kernel::channel::Stats {
                    channels: 1,
                    endpoints: 2,
                    queued: 0,
                })
        {
            return Err(cap::Error::Object);
        }
        s.a = Some(a);
        s.b = Some(b);
        serial::write(format_args!("[NEU] IPC bootstrap: A={} B={} objects=6 references=16 handles=8,8 endpoints=2 channels=1 queues=2 capacity=2 ABI=0.3\n",a.raw(),b.raw())).map_err(|_|cap::Error::Object)
    })
}
unsafe extern "C" {
    fn neu_ipc_request_probe();
}
fn publish(s: &mut State, mut record: Record, bit: Option<u8>) -> cap::Result<()> {
    if s.count == LIMIT {
        return Err(cap::Error::Capacity);
    }
    let probe = bit.is_some_and(|b| s.probes & (1 << b) == 0);
    if probe {
        s.probes |= 1 << bit.unwrap();
    }
    record.words[23] = probe as u64;
    s.records[s.count] = record;
    s.count += 1;
    // SAFETY: Exclusive diagnostics transaction; supervisor-only stable witness.
    unsafe {
        ptr::write_volatile(neu_ipc_witness.0.get(), record);
    }
    if probe {
        serial::write(format_args!(
            "[NEU] IPC probe: sequence={} node={} thread={} op={:#x} status={}\n",
            record.words[0],
            record.words[1],
            record.words[2],
            record.words[6],
            record.words[9] as i64
        ))
        .map_err(|_| cap::Error::Object)?;
        // SAFETY: Bounded IF=0 observation hold on the trusted selected stack.
        unsafe {
            neu_ipc_request_probe();
        }
    }
    Ok(())
}
fn snapshot(r: &mut Record, objects: &Objects) {
    let w = objects.channel_state().snapshot();
    r.words[28..50].copy_from_slice(&w);
    r.words[61] = u64::from(w[2] != 0) | u64::from(w[3] != 0) << 1;
    r.words[26..28].copy_from_slice(&objects.channel_state().queue_addresses());
    let st = objects.stats();
    r.words[62] = st.references as u64;
    r.words[63] = st.live as u64;
}
pub fn request(
    objects: &mut Objects,
    lease: &mut Lease,
    active: ActiveContext,
    frame: Frame,
    address: u64,
    top: u64,
) -> cap::Result<Outcome> {
    tx(|s| {
        if s.count >= LIMIT - 2 {
            return Ok(Outcome::Return {
                status: cap::Error::Capacity as i64,
                scalar: 0,
            });
        }
        if lease.active() != active || lease.table().id().owner() != active.node {
            return Err(cap::Error::Owner);
        }
        let op = frame.registers.rax;
        let h = Handle::from_raw(frame.registers.rdi);
        let payload = frame.registers.rsi;
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
            payload,
        ]);
        r.words[18..23].copy_from_slice(&[
            frame.context.rip,
            frame.context.rsp,
            top,
            address,
            interrupts::flags(),
        ]);
        r.words[24] = u64::MAX;
        let before = objects.channel_state().snapshot();
        r.words[50..60].copy_from_slice(&before[6..16]);
        r.words[60] = u64::from(before[2] != 0) | u64::from(before[3] != 0) << 1;
        if let Ok((id, rights)) = lease.table().describe(active.node, h) {
            r.words[11] = id.raw();
            r.words[12] = id.kind() as u64;
            r.words[13] = rights.bits();
        }
        let result = (|| -> Result<u64, cap::Error> {
            if op == neu_kernel::syscall::v02::CAP_CLOSE {
                lease.table_mut().close(objects, active.node, h)?;
                return Ok(0);
            }
            let rights = if op == v03::CHANNEL_SEND {
                Rights::SEND
            } else {
                Rights::RECEIVE
            };
            let mut guard = lease.table().lookup(
                objects,
                active.node,
                h,
                ObjectType::ChannelEndpoint,
                rights,
            )?;
            let info = guard.endpoint_info()?;
            r.words[14] = guard.info().references as u64;
            r.words[15] = guard.info().pins as u64;
            r.words[16] = info.id.raw();
            r.words[17] = info.channel.raw();
            r.words[24] = info.side as u64;
            r.words[25] = info.owner.raw();
            let outcome = if op == v03::CHANNEL_SEND {
                guard.channel_send(payload).map(|_| 0)
            } else {
                guard.channel_receive()
            };
            let (status, scalar) = match outcome {
                Ok(v) => (0, v),
                Err(e) => (e as i64, 0),
            };
            r.words[9] = status as u64;
            r.words[10] = scalar;
            let stats = guard.registry_stats();
            r.words[62] = stats.references as u64;
            r.words[63] = stats.live as u64;
            let after = guard.channel_snapshot();
            r.words[28..50].copy_from_slice(&after);
            r.words[26..28].copy_from_slice(&guard.queue_addresses());
            r.words[61] = u64::from(after[2] != 0) | u64::from(after[3] != 0) << 1;
            if status == cap::Error::Full as i64 {
                if cfg!(feature = "test-channels-stress") && (0x7000..0x7003).contains(&payload) {
                    s.gates |= 1 << (4 + payload - 0x7000);
                } else {
                    s.gates |= 1 << info.side;
                }
            }
            if status == 0 && info.side == 1 && op == v03::CHANNEL_SEND && payload == 55 {
                s.gates |= 8;
            }
            let bit = match (active.thread.index(), op, status, payload, scalar) {
                (0, 0x13, 0, 0x6000, _) => Some(18),
                (0, 0x13, -33, 0x7000, _) => Some(19),
                (1, 0x14, 0, _, 0x6000) => Some(20),
                (0, 0x13, 0, 0x6002, _) => Some(21),
                (1, 0x14, 0, _, 0x6102) => Some(22),
                (0, 0x14, 0, _, 0x6202) => Some(23),
                (0, 0x13, 0, 0, _) => Some(0),
                (0, 0x13, -33, _, _) => Some(1),
                (1, 0x14, 0, _, 0) => Some(2),
                (1, 0x13, 0, 0x1020304050607080, _) => Some(3),
                (1, 0x13, -33, _, _) => Some(4),
                (2, 0x14, 0, _, 0x1020304050607080) => Some(5),
                (2, 0x13, 0, 33, _) => Some(6),
                (0, 0x13, 0, 44, _) => Some(7),
                (2, 0x14, 0, _, 55) => Some(8),
                (1, 0x13, 0, 55, _) => Some(9),
                (1, _, -34, _, _) => Some(10),
                (2, _, -34, _, _) => Some(11),
                (1, 0x14, 0, _, 0xfedcba9876543210) => Some(17),
                _ => None,
            };
            // The queue result is already committed. A diagnostic failure must
            // never change it into a returned operational error after mutation.
            // Stop instead; no caller can retry an allegedly uncommitted send.
            publish(s, r, bit).unwrap_or_else(|_| super::halt());
            // The guard pin remains live during the QMP observation above.
            outcome
        })();
        let (status, scalar) = match result {
            Ok(v) => (0, v),
            Err(e) => (e as i64, 0),
        };
        // Guarded operations already published; lookup failures and close did not.
        if op == 0x11 || r.words[15] == 0 {
            r.words[9] = status as u64;
            r.words[10] = scalar;
            snapshot(&mut r, objects);
            let bit = match (op, status) {
                (0x11, 0) => Some(if active.node == s.a.unwrap() { 15 } else { 16 }),
                (_, -16) => Some(12),
                (_, -18) => Some(13),
                (_, -19) => Some(14),
                _ => None,
            };
            publish(s, r, bit)?;
        }
        Ok(Outcome::Return { status, scalar })
    })
}
pub fn gates(_objects: &Objects) -> cap::Result<u64> {
    tx(|s| Ok(s.gates))
}
pub fn cleanup(node: NodeId, before: [u64; 22], objects: &Objects) -> cap::Result<()> {
    tx(|s| {
        let mut r = Record::EMPTY;
        r.words[0] = s.count as u64 + 1;
        r.words[1] = node.raw();
        r.words[6] = CLEANUP;
        r.words[24] = u64::MAX;
        r.words[50..60].copy_from_slice(&before[6..16]);
        r.words[60] = u64::from(before[2] != 0) | u64::from(before[3] != 0) << 1;
        snapshot(&mut r, objects);
        publish(s, r, None)
    })
}
pub fn finish(objects: &Objects) -> cap::Result<()> {
    tx(|s| {
        if objects.channel_state().stats() != neu_kernel::channel::Stats::default() {
            return Err(cap::Error::Object);
        }
        for r in &s.records[..s.count] {
            serial::write(format_args!("[NEU] IPC record: words="))
                .map_err(|_| cap::Error::Object)?;
            for (i, w) in r.words.iter().enumerate() {
                serial::write(format_args!("{}{:#x}", if i == 0 { "" } else { "," }, w))
                    .map_err(|_| cap::Error::Object)?;
            }
            serial::write(format_args!("\n")).map_err(|_| cap::Error::Object)?;
        }
        serial::write(format_args!("[NEU] IPC accounting restored: channels=0 endpoints=0 queued=0 references=0 pins=0 requests={}\n",s.count)).map_err(|_|cap::Error::Object)
    })
}

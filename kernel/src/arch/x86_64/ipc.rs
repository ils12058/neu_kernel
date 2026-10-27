//! ABI 0.3 adapter. Object/queue mutation occurs through the exclusively leased
//! registry passed by address_space; this storage owns diagnostics only.
use super::{interrupts, serial};
use core::{
    cell::UnsafeCell,
    ptr,
    sync::atomic::{AtomicBool, Ordering},
};
use neu_kernel::{
    capability::{self as cap, Handle, Lease, ObjectType, Objects, Rights, WaitStart},
    node::{ActiveContext, NodeId},
    syscall::{Frame, Outcome, v03, v04},
};
const LIMIT: usize = 112;
const CLEANUP: u64 = u64::MAX - 1;
/// Internal diagnostic marker for a registered blocking request. Not an ABI
/// status: the user frame is resumed only by the pre-entry settle transaction.
const BLOCKED: i64 = 0x100;
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
#[cfg(feature = "cap-move-test")]
struct MoveWitness(UnsafeCell<[u64; 170]>);
// SAFETY: Owned by this module's exclusive IF=0 transaction, just like Record.
// QMP reads only while stopped. No borrowed table/registry reference is stored.
#[cfg(feature = "cap-move-test")]
unsafe impl Sync for MoveWitness {}
#[cfg(feature = "cap-move-test")]
#[unsafe(no_mangle)]
static neu_move_witness: MoveWitness = MoveWitness(UnsafeCell::new([0; 170]));
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
        serial::write(format_args!("[NEU] IPC bootstrap: A={} B={} objects=6 references=16 handles=8,8 endpoints=2 channels=1 queues=2 capacity=2 ABI={}\n",a.raw(),b.raw(),if cfg!(feature = "cap-move-test") {"0.5"} else if cfg!(feature = "channel-wait-test") {"0.4"} else {"0.3"})).map_err(|_|cap::Error::Object)
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
    #[cfg(feature = "channel-wait-test")]
    {
        // Upper bits carry the wait-ledger publication order. The low bit keeps
        // the existing probe-observation contract for both ABI profiles.
        record.words[23] |= neu_kernel::wait::witness::cursor() << 1;
    }
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
        #[cfg(feature = "cap-move-test")]
        return move_request(s, objects, lease, active, &mut r, frame);
        #[cfg(not(feature = "cap-move-test"))]
        {
            let result = (|| -> Result<Outcome, cap::Error> {
                if op == neu_kernel::syscall::v02::CAP_CLOSE {
                    lease.table_mut().close(objects, active.node, h)?;
                    return Ok(Outcome::Return {
                        status: 0,
                        scalar: 0,
                    });
                }
                let blocking = op == v04::CHANNEL_SEND_WAIT || op == v04::CHANNEL_RECEIVE_WAIT;
                let send = op == v03::CHANNEL_SEND || op == v04::CHANNEL_SEND_WAIT;
                let rights = if send { Rights::SEND } else { Rights::RECEIVE };
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
                let outcome = if send {
                    if blocking {
                        guard.channel_send_wait(active, payload)
                    } else {
                        guard.channel_send(payload).map(|_| WaitStart::Committed(0))
                    }
                } else if blocking {
                    guard.channel_receive_wait(active)
                } else {
                    guard.channel_receive().map(WaitStart::Committed)
                };
                let (status, scalar, blocked) = match outcome {
                    Ok(WaitStart::Committed(v)) => (0, v, false),
                    Ok(WaitStart::Blocked) => (BLOCKED, 0, true),
                    Err(e) => (e as i64, 0, false),
                };
                #[cfg(feature = "channel-wait-test")]
                if blocking {
                    let ws = guard.wait_stats();
                    serial::write(format_args!(
                    "[NEU] IPC wait: op={:#x} thread={} status={} blocked={} waiting={} ready={} queued={} terminal={}\n",
                    op,
                    active.thread.raw(),
                    status,
                    blocked,
                    ws.waiting,
                    ws.ready,
                    ws.queued,
                    ws.terminal
                ))
                .map_err(|_| cap::Error::Object)?;
                }
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
                    if cfg!(feature = "test-channels-stress") && (0x7000..0x7003).contains(&payload)
                    {
                        s.gates |= 1 << (4 + payload - 0x7000);
                    } else {
                        s.gates |= 1 << info.side;
                    }
                }
                if blocked {
                    s.gates |= 1 << 25;
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
                Ok(if blocked {
                    Outcome::Blocked
                } else {
                    Outcome::Return { status, scalar }
                })
            })();
            let outcome = match result {
                Ok(outcome) => outcome,
                Err(e) => Outcome::Return {
                    status: e as i64,
                    scalar: 0,
                },
            };
            let (status, scalar) = match outcome {
                Outcome::Return { status, scalar } => (status, scalar),
                _ => (0, 0),
            };
            // Guarded operations already published; lookup failures and close did not.
            if (op == 0x11 || r.words[15] == 0) && !matches!(outcome, Outcome::Blocked) {
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
            Ok(outcome)
        }
    })
}

#[cfg(feature = "cap-move-test")]
fn move_request(
    s: &mut State,
    objects: &mut Objects,
    lease: &mut Lease,
    active: ActiveContext,
    r: &mut Record,
    frame: Frame,
) -> cap::Result<Outcome> {
    use neu_kernel::syscall::{v02, v05};
    let op = frame.registers.rax;
    let h = Handle::from_raw(frame.registers.rdi);
    let mut evidence = [0u64; 170];
    evidence[..6].copy_from_slice(&[
        r.words[0],
        op,
        active.node.raw(),
        active.thread.raw(),
        h.raw(),
        frame.registers.rsi,
    ]);
    evidence[8..40].copy_from_slice(&lease.table().move_slots());
    evidence[72..84].copy_from_slice(&objects.channel_state().transfer_snapshot());
    evidence[96..114].copy_from_slice(&objects.move_objects());
    let result = (|| -> cap::Result<Outcome> {
        let value = match op {
            neu_kernel::syscall::ABI_QUERY => v05::INFO,
            v02::CAP_CLOSE => {
                lease.table_mut().close(objects, active.node, h)?;
                0
            }
            v05::CHANNEL_SEND_CAP => {
                lease.table_mut().send_cap(
                    objects,
                    active.node,
                    h,
                    Handle::from_raw(frame.registers.rsi),
                )?;
                0
            }
            v05::CHANNEL_RECEIVE_CAP => lease
                .table_mut()
                .receive_cap(objects, active.node, h)?
                .raw(),
            v05::TEST_READ => {
                let guard = lease.table().lookup(
                    objects,
                    active.node,
                    h,
                    ObjectType::Flag,
                    Rights::READ,
                )?;
                evidence[132] = guard.value_address();
                evidence[134] = guard.info().id.raw();
                guard.read()?
            }
            _ => {
                let send = op == v03::CHANNEL_SEND || op == v04::CHANNEL_SEND_WAIT;
                let mut guard = lease.table().lookup(
                    objects,
                    active.node,
                    h,
                    ObjectType::ChannelEndpoint,
                    if send { Rights::SEND } else { Rights::RECEIVE },
                )?;
                let info = guard.endpoint_info()?;
                r.words[14] = guard.info().references as u64;
                r.words[16] = info.id.raw();
                r.words[17] = info.channel.raw();
                r.words[24] = info.side as u64;
                r.words[25] = active.node.raw();
                let result = match op {
                    v03::CHANNEL_SEND => guard
                        .channel_send(frame.registers.rsi)
                        .map(|_| WaitStart::Committed(0)),
                    v03::CHANNEL_RECEIVE => guard.channel_receive().map(WaitStart::Committed),
                    v04::CHANNEL_SEND_WAIT => guard.channel_send_wait(active, frame.registers.rsi),
                    v04::CHANNEL_RECEIVE_WAIT => guard.channel_receive_wait(active),
                    _ => return Err(cap::Error::Type),
                }?;
                match result {
                    WaitStart::Committed(v) => v,
                    WaitStart::Blocked => return Ok(Outcome::Blocked),
                }
            }
        };
        Ok(Outcome::Return {
            status: 0,
            scalar: value,
        })
    })();
    let outcome = result.unwrap_or_else(|e| Outcome::Return {
        status: e as i64,
        scalar: 0,
    });
    let (status, scalar) = match outcome {
        Outcome::Return { status, scalar } => (status, scalar),
        _ => (BLOCKED, 0),
    };
    r.words[9] = status as u64;
    r.words[10] = scalar;
    snapshot(r, objects);
    evidence[6] = status as u64;
    evidence[7] = scalar;
    evidence[40..72].copy_from_slice(&lease.table().move_slots());
    evidence[84..96].copy_from_slice(&objects.channel_state().transfer_snapshot());
    evidence[114..132].copy_from_slice(&objects.move_objects());
    evidence[133] = if evidence[132] != 0 { scalar } else { 0 };
    evidence[136..154].copy_from_slice(&objects.move_object_addresses());
    evidence[154..170].copy_from_slice(&lease.table().move_slot_addresses());
    // Reused, bounded 1360-byte witness. All entries are recorded in serial;
    // selected transactions are independently sampled at a stopped native CPU.
    unsafe {
        ptr::write_volatile(neu_move_witness.0.get(), evidence);
    }
    serial::write(format_args!("[NEU] Move record: words=")).unwrap_or_else(|_| super::halt());
    for (i, word) in evidence.iter().enumerate() {
        serial::write(format_args!("{}{:#x}", if i == 0 { "" } else { "," }, word))
            .unwrap_or_else(|_| super::halt());
    }
    serial::write(format_args!("\n")).unwrap_or_else(|_| super::halt());
    if status == cap::Error::Full as i64 {
        s.gates |= 1;
    }
    if status == BLOCKED {
        // Deterministic "a scalar wait is now registered" gate for the
        // backpressure fixture. Scalar waits remain the only blocking ABI.
        s.gates |= 1 << 25;
    }
    if op == v05::CHANNEL_SEND_CAP && status == 0 {
        s.gates |= 1 << 26;
        // Distinguish the two movable Flag grants used by the double-transfer
        // profile without exposing handles to the ABI.
        match (frame.registers.rsi as u32).wrapping_sub(1) {
            2 => s.gates |= 1 << 27,
            3 => s.gates |= 1 << 28,
            _ => {}
        }
    }
    if op == v05::TEST_READ && status == 0 {
        // Controller-only completion gate for profiles that fault a Node after
        // observing use of a freshly delivered Flag.
        s.gates |= if active.node == s.a.unwrap() {
            1 << 29
        } else {
            1 << 30
        };
    }
    let bit = match (op, status) {
        (1, 0) => Some(37),
        (0x17, 0) => Some(26),
        (0x17, -33) => Some(27),
        (0x17, -18) => Some(28),
        (0x18, 0) => Some(29),
        (0x18, -22) => Some(30),
        (0x18, -36) => Some(31),
        (v05::TEST_READ, 0) => Some(if active.node == s.a.unwrap() { 32 } else { 33 }),
        (v05::TEST_READ, -17) => Some(34),
        (0x14, -36) => Some(35),
        (0x16, -36) => Some(36),
        (0x17, -37) => Some(38),
        (0x17, -19) => Some(39),
        (0x18, -32) => Some(40),
        _ => None,
    };
    publish(s, *r, bit).unwrap_or_else(|_| super::halt());
    Ok(outcome)
}
pub fn gates(_objects: &Objects) -> cap::Result<u64> {
    tx(|s| Ok(s.gates))
}
/// Terminal-order gate: published only after the controller has reclaimed the
/// named Node. Move fixtures use it instead of probing a dead peer with
/// repeated syscalls, keeping the bounded transaction ledger deterministic.
#[cfg(feature = "cap-move-test")]
pub fn mark_reclaimed(node: NodeId) -> cap::Result<()> {
    tx(|s| {
        if s.a == Some(node) {
            s.gates |= 1 << 31;
        } else if s.b == Some(node) {
            s.gates |= 1 << 32;
        } else {
            return Err(cap::Error::Owner);
        }
        Ok(())
    })
}
#[cfg(feature = "cap-move-test")]
pub fn mark_wait_settled() -> cap::Result<()> {
    tx(|s| {
        s.gates |= 1 << 34;
        Ok(())
    })
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

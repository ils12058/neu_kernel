//! One BSP, user-only preemption. IRQ code owns only copied publication/frame
//! values and its mailbox; it never accesses the suspended controller's &muts.
#[cfg(not(feature = "multi-test"))]
use super::userspace;
use super::{
    cooperative as bootstrap, exceptions, interrupts, native,
    paging::{self, Paging},
    serial,
};
use core::{
    arch::{asm, global_asm},
    cell::UnsafeCell,
    ptr,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};
use neu_kernel::{
    context::SavedContext,
    paging::{Error, Result},
    preemption::{Mailbox, Publication},
    syscall::{self as abi, Frame, ReturnContract},
};
#[cfg(not(feature = "multi-test"))]
use neu_kernel::{
    context::SliceEvent,
    node::{ExitReason, Fault, FaultSource, Nodes},
    paging::PAGE,
    scheduler::Queue,
};
global_asm!(include_str!("preemptive.S"));
unsafe extern "C" {
    #[cfg(not(feature = "multi-test"))]
    static neu_preempt_payload: u8;
    #[cfg(not(feature = "multi-test"))]
    static neu_preempt_payload_end: u8;
    #[cfg(not(feature = "multi-test"))]
    static neu_preempt_syscall_resume: u8;
    static mut neu_preempt_irq_observed: u64;
    #[cfg(not(feature = "multi-test"))]
    fn neu_preempt_kernel_origin();
    static __stack_bottom: u8;
    static __stack_top: u8;
}
#[cfg(not(feature = "multi-test"))]
pub fn site(symbol: *const u8) -> u64 {
    userspace::CODE + symbol as u64 - ptr::addr_of!(neu_preempt_payload) as u64
}
#[cfg(not(feature = "multi-test"))]
pub fn payload() -> &'static [u8] {
    let len = ptr::addr_of!(neu_preempt_payload_end) as usize
        - ptr::addr_of!(neu_preempt_payload) as usize;
    assert!(len > 0 && len <= PAGE as usize);
    // SAFETY: Bounded immutable image bytes copied into private owned RX backing.
    unsafe { core::slice::from_raw_parts(ptr::addr_of!(neu_preempt_payload), len) }
}
#[cfg(not(feature = "multi-test"))]
pub fn return_contract(root: u64) -> ReturnContract {
    ReturnContract {
        root,
        code_start: userspace::CODE,
        code_end: site(ptr::addr_of!(neu_preempt_payload_end)),
        stack_start: userspace::STACK,
        stack_end: userspace::STACK + PAGE,
        resumes: [site(ptr::addr_of!(neu_preempt_syscall_resume)); 2],
    }
}
#[cfg(feature = "multi-test")]
pub fn return_contract(root: u64) -> ReturnContract {
    let owner = mailbox()
        .current()
        .unwrap_or_else(|| reject(48, "missing member publication"));
    super::multithread::return_contract(root, owner.active.context)
}
pub fn valid_user_frame(frame: Frame, root: u64) -> bool {
    let mut context = frame.context;
    context.flags &= !(1 << 16); // Hardware IRQ frame RF; preserve it in the owned record/IRETQ.
    let c = return_contract(root);
    // IRQ continuation is any checked RX instruction, not a syscall resume site.
    let interrupt = ReturnContract {
        resumes: [frame.context.rip; 2],
        ..c
    };
    abi::validate_preemptible_return(context, &interrupt, root).is_ok()
}
struct Slot(UnsafeCell<Mailbox>);
// SAFETY: IF=0 copies only, one BSP; no reference into this slot crosses IRETQ.
unsafe impl Sync for Slot {}
static MAILBOX: Slot = Slot(UnsafeCell::new(Mailbox::new()));
static ENABLED: AtomicBool = AtomicBool::new(false);
#[unsafe(export_name = "neu_preempt_busy")]
static BUSY: AtomicBool = AtomicBool::new(false);
static COUNT: AtomicU64 = AtomicU64::new(0);
#[derive(Clone, Copy)]
struct Record {
    owner: Publication,
    frame: Frame,
    tick: u64,
    handler_rsp: u64,
    switched: bool,
}
struct Records(UnsafeCell<[Option<Record>; 256]>);
// SAFETY: IRQ writes one checked slot with IF=0; common controller reads copies.
unsafe impl Sync for Records {}
static RECORDS: Records = Records(UnsafeCell::new([None; 256]));
fn mailbox() -> Mailbox {
    unsafe { ptr::read(MAILBOX.0.get()) }
}
fn save(value: Mailbox) {
    unsafe {
        ptr::write(MAILBOX.0.get(), value);
    }
}
pub fn enabled() -> bool {
    ENABLED.load(Ordering::Acquire)
}
fn reject(vector: u64, reason: &str) -> ! {
    interrupts::reject_user(vector, reason)
}
#[unsafe(no_mangle)]
extern "C" fn neu_preempt_arm() {
    let p = mailbox()
        .current()
        .unwrap_or_else(|| reject(48, "missing publication"));
    if !native::preempt_binding_valid(p.active, p.stack)
        || exceptions::rsp0() != p.stack.top
        || paging::current_root() != p.active.context.root
        || interrupts::flags() & 0x200 != 0
    {
        reject(48, "mixed user publication");
    }
    // SAFETY: IF=0, no IRQ is live. A new publication grants one observation hold.
    unsafe {
        ptr::write_volatile(ptr::addr_of_mut!(neu_preempt_irq_observed), 0);
    }
    interrupts::arm_user();
}
pub(super) fn interrupt(pointer: *const interrupts::Frame) {
    if BUSY.swap(true, Ordering::AcqRel) {
        reject(48, "preemption reentry");
    }
    let address = pointer as u64;
    let p = mailbox().current();
    let (bottom, top) = p.map_or(
        (
            ptr::addr_of!(__stack_bottom) as u64,
            ptr::addr_of!(__stack_top) as u64,
        ),
        |p| (p.stack.bottom, p.stack.top),
    );
    if address < bottom || address.checked_add(176).is_none_or(|end| end > top) || address & 7 != 0
    {
        reject(0, "preemption untrusted stack");
    }
    // SAFETY: Dedicated IRQ assembly and verified live supervisor stack bounds.
    let f = unsafe { ptr::read(pointer) };
    if f.cs != abi::USER_CS {
        if f.vector != 48 || !interrupts::user_in_service(48) {
            interrupts::acknowledge_user(f.vector);
        }
        reject(f.vector, "unsupported kernel origin");
    }
    let p = p.unwrap_or_else(|| reject(f.vector, "missing IRQ owner"));
    let handler_rsp: u64;
    unsafe {
        asm!("mov {}, rsp",out(reg) handler_rsp,options(nomem,nostack,preserves_flags));
    }
    let frame = Frame {
        registers: unsafe { ptr::read(f.gpr.as_ptr().cast::<abi::Registers>()) },
        context: abi::ReturnContext {
            rip: f.rip,
            cs: f.cs,
            flags: f.flags,
            rsp: f.rsp,
            ss: f.ss,
        },
    };
    if address != top - 176
        || f.error != 0
        || interrupts::flags() & 0x600 != 0
        || !(bottom..address).contains(&handler_rsp)
        || !native::preempt_binding_valid(p.active, p.stack)
        || exceptions::rsp0() != top
        || paging::current_root() != p.active.context.root
        || !valid_user_frame(frame, p.active.context.root)
        || !SavedContext::interrupted(p.active, p.stack, frame).valid(p.active, p.stack)
    {
        serial::emergency(format_args!(
            "[NEU] IRQ rejected details: frame={:#x} expected={:#x} handler={:#x} bottom={:#x} native={} TSS={:#x} root={:#x} flags={:#x} RIP={:#x} RSP={:#x} CS={:#x} SS={:#x} user={} saved={}\n",
            address,
            top - 176,
            handler_rsp,
            bottom,
            native::preempt_binding_valid(p.active, p.stack),
            exceptions::rsp0(),
            paging::current_root(),
            f.flags,
            f.rip,
            f.rsp,
            f.cs,
            f.ss,
            valid_user_frame(frame, p.active.context.root),
            SavedContext::interrupted(p.active, p.stack, frame).valid(p.active, p.stack)
        ));
        reject(f.vector, "invalid user IRQ context");
    }
    let tick = interrupts::acknowledge_user(f.vector);
    if tick == 0 {
        BUSY.store(false, Ordering::Release);
        return;
    }
    let index = COUNT.load(Ordering::Relaxed);
    if index >= 256 {
        reject(f.vector, "preemption record overflow");
    }
    unsafe {
        ptr::write(
            RECORDS.0.get().cast::<Option<Record>>().add(index as usize),
            Some(Record {
                owner: p,
                frame,
                tick,
                handler_rsp,
                switched: p.reschedule,
            }),
        );
    }
    COUNT.store(index + 1, Ordering::Release);
    if !p.reschedule {
        interrupts::arm_user();
        BUSY.store(false, Ordering::Release);
        return;
    }
    let mut slot = mailbox();
    slot.publish(p, frame)
        .unwrap_or_else(|_| reject(f.vector, "invalid IRQ handoff"));
    save(slot);
    // The active private IRQ call chain is abandoned only through the common
    // stack bridge. IF remains clear; no Rust reference escapes this function.
    native::preempt(frame)
}
pub(super) fn print_records(from: &mut u64) -> Result<()> {
    let end = COUNT.load(Ordering::Acquire);
    while *from < end {
        let r = unsafe { ptr::read(RECORDS.0.get().cast::<Option<Record>>().add(*from as usize)) }
            .ok_or(Error::Corrupt)?;
        serial::write(format_args!("[NEU] Preempt IRQ: tick={} sequence={} node={} thread={} space={} root={:#x} RIP={:#x} RSP={:#x} flags={:#x} CS={:#x} SS={:#x} handler={:#x} vector=48 ISR_before=1 ISR_after=0 switch={}\n",r.tick,r.owner.sequence,r.owner.active.node.raw(),r.owner.active.thread.raw(),r.owner.active.context.space.raw(),r.owner.active.context.root,r.frame.context.rip,r.frame.context.rsp,r.frame.context.flags,r.frame.context.cs,r.frame.context.ss,r.handler_rsp,r.switched)).map_err(|_|Error::Corrupt)?;
        bootstrap::frame_log("irq", r.owner.active.thread, r.frame)?;
        *from += 1;
    }
    Ok(())
}
#[cfg(not(feature = "multi-test"))]
fn check<T>(value: neu_kernel::node::Result<T>) -> Result<T> {
    value.map_err(|_| Error::Corrupt)
}
#[cfg(not(feature = "multi-test"))]
pub fn run(paging: &mut Paging<'_, '_>) -> Result<()> {
    if interrupts::flags() & 0x600 != 0 {
        return Err(Error::Flags);
    }
    let cr4: u64;
    // SAFETY: Read-only control state. No FSGSBASE/TLS or unsaved extended-state
    // mechanism is admitted by this bounded execution contract.
    unsafe {
        asm!("mov {}, cr4",out(reg) cr4,options(nomem,nostack,preserves_flags));
    }
    if cr4 & ((1 << 9) | (1 << 10) | (1 << 16) | (1 << 18)) != 0 {
        return Err(Error::Flags);
    }
    let before = paging.stats();
    let tables = paging.table_count();
    serial::write(format_args!(
        "[NEU] Preempt baseline: allocated={} free={} tables={} root={:#x}\n",
        before.allocated,
        before.free,
        tables,
        paging.root()
    ))
    .map_err(|_| Error::Corrupt)?;
    interrupts::setup_user(paging).map_err(|_| Error::Corrupt)?;
    ENABLED.store(true, Ordering::Release);
    if cfg!(feature = "test-preempt-unexpected") {
        unsafe {
            asm!("int 0x41", options(noreturn));
        }
    }
    if cfg!(feature = "test-preempt-software") {
        unsafe {
            asm!("int 0x30", options(noreturn));
        }
    }
    if cfg!(feature = "test-preempt-origin") {
        interrupts::arm_user();
        unsafe {
            neu_preempt_kernel_origin();
        }
        return Err(Error::Corrupt);
    }
    let mut nodes = Nodes::new();
    let mut queue = Queue::new();
    let a_action = if cfg!(feature = "test-preempt-a-fault") {
        1
    } else if cfg!(feature = "test-preempt-cross") {
        2
    } else if cfg!(feature = "test-preempt-fpu") {
        3
    } else if cfg!(feature = "test-preempt-native") {
        4
    } else {
        0
    };
    let b_action = if cfg!(feature = "test-preempt-b-fault") {
        1
    } else if cfg!(feature = "test-preempt-native") {
        4
    } else {
        0
    };
    let mut a = bootstrap::build(paging, &mut nodes, 1, a_action)?;
    paging.user_limit(bootstrap::space(&a)?, u64::MAX)?;
    bootstrap::install(paging, &mut nodes, &mut queue, &mut a)?;
    let mut b = bootstrap::build(paging, &mut nodes, 2, b_action)?;
    paging.user_limit(bootstrap::space(&b)?, u64::MAX)?;
    bootstrap::install(paging, &mut nodes, &mut queue, &mut b)?;
    serial::write(format_args!(
        "[NEU] Preempt two runnable: A={} B={} queue=2 quantum_us=10000\n",
        a.thread.raw(),
        b.thread.raw()
    ))
    .map_err(|_| Error::Corrupt)?;
    let mut printed = 0;
    let mut counts = [0u64; 2];
    let mut limited = [false; 2];
    let mut last_progress = [0u64; 2];
    let mut dispatches = 0;
    while let Some(mut d) = check(queue.next(&mut nodes))? {
        dispatches += 1;
        if dispatches > 64 {
            return Err(Error::Corrupt);
        }
        let is_a = d.active.thread == a.thread;
        let index = usize::from(!is_a);
        let domain = if is_a { &mut a } else { &mut b };
        let active = d.active;
        if active != domain.active || d.saved.stack != domain.stack.ok_or(Error::NotOwned)? {
            return Err(Error::Corrupt);
        }
        if !d.saved.fresh && domain.last != Some(d.saved.frame) {
            return Err(Error::Corrupt);
        }
        serial::write(format_args!("[NEU] Preempt selected: sequence={} label={} node={} thread={} space={} root={:#x} fresh={} peer={}\n",dispatches,domain.label,domain.node.raw(),domain.thread.raw(),active.context.space.raw(),active.context.root,d.saved.fresh,!queue.is_empty())).map_err(|_|Error::Corrupt)?;
        bootstrap::frame_log("preempt-taken", domain.thread, d.saved.frame)?;
        if paging.context_reclaimable(bootstrap::space(domain)?) != Err(Error::InUse)
            || paging
                .destroy_thread_stack(domain.space.as_mut().ok_or(Error::NotOwned)?, domain.thread)
                != Err(Error::InUse)
            || nodes.release_stack(domain.thread, d.saved.stack)
                != Err(neu_kernel::node::Error::InUse)
        {
            return Err(Error::Corrupt);
        }
        let reject_kind = if is_a && counts[0] == 1 {
            if cfg!(feature = "test-preempt-invalid-context") {
                1
            } else if cfg!(feature = "test-preempt-stack-mismatch") {
                2
            } else if cfg!(feature = "test-preempt-unsafe") {
                3
            } else {
                0
            }
        } else {
            0
        };
        match reject_kind {
            1 => d.saved.frame.context.rip = 0x800000000000,
            2 => d.saved.stack.top += PAGE,
            3 => d.saved.frame.context.flags &= !0x200,
            _ => {}
        }
        let mut slot = mailbox();
        let owner = slot
            .arm(
                active,
                domain.stack.ok_or(Error::NotOwned)?,
                !queue.is_empty(),
            )
            .map_err(|_| Error::Corrupt)?;
        save(slot);
        let event = match native::execute_slice(paging, bootstrap::space(domain)?, d, 64) {
            Ok(event) if reject_kind == 0 => event,
            Err(Error::Flags) if reject_kind != 0 => {
                let mut slot = mailbox();
                slot.terminal(owner).map_err(|_| Error::Corrupt)?;
                save(slot);
                serial::write(format_args!(
                    "[NEU] Preempt rejected before entry: kind={} thread={} root={:#x} IF=0\n",
                    reject_kind,
                    domain.thread.raw(),
                    paging.root()
                ))
                .map_err(|_| Error::Corrupt)?;
                SliceEvent::Exited(ExitReason::UserFault(Fault {
                    source: FaultSource::RejectedReturn,
                    vector: 0,
                    error: reject_kind,
                    rip: active.context.entry,
                    rsp: active.context.rsp,
                    cs: abi::USER_CS,
                    ss: abi::USER_SS,
                    flags: 0x202,
                    cr2: 0,
                }))
            }
            _ => return Err(Error::Corrupt),
        };
        print_records(&mut printed)?;
        match event {
            SliceEvent::Preempted(frame) => {
                let mut slot = mailbox();
                if cfg!(feature = "test-preempt-handoff") && dispatches == 1 {
                    if slot.publish(owner, frame) != Err(neu_kernel::preemption::Error::Busy) {
                        return Err(Error::Corrupt);
                    }
                    let mut wrong = owner;
                    wrong.sequence += 1;
                    if slot.take(wrong) != Err(neu_kernel::preemption::Error::Identity) {
                        return Err(Error::Corrupt);
                    }
                    serial::write(format_args!("[NEU] Preempt handoff negatives: duplicate=Busy wrong_owner=Identity original=retained\n")).map_err(|_|Error::Corrupt)?;
                }
                let saved = slot.take(owner).map_err(|_| Error::Corrupt)?;
                if saved.frame != frame
                    || slot.take(owner) != Err(neu_kernel::preemption::Error::Identity)
                {
                    return Err(Error::Corrupt);
                }
                save(slot);
                domain.last = Some(frame);
                counts[index] += 1;
                bootstrap::frame_log("preempt-saved", domain.thread, frame)?;
                check(queue.preempted(&mut nodes, saved))?;
                let words = paging.user_words(bootstrap::space(domain)?)?;
                if words[0] <= last_progress[index]
                    || words[1] != 1
                    || words[2] != (0x11225566778899aa ^ domain.seed)
                {
                    return Err(Error::Corrupt);
                }
                last_progress[index] = words[0];
                if counts[index] >= 4 && !limited[index] {
                    // B remains CPU-bound after A exits to exercise no-peer IRQ return.
                    paging.user_limit(
                        bootstrap::space(domain)?,
                        words[0]
                            + if is_a != cfg!(feature = "test-preempt-b-fault") {
                                100_000
                            } else {
                                20_000_000
                            },
                    )?;
                    limited[index] = true;
                }
                serial::write(format_args!(
                    "[NEU] Preempt progress: label={} preemptions={} work={} boots={} queue={}\n",
                    domain.label,
                    counts[index],
                    words[0],
                    words[1],
                    queue.len()
                ))
                .map_err(|_| Error::Corrupt)?;
            }
            SliceEvent::Exited(reason) => {
                let mut slot = mailbox();
                if !slot.empty() {
                    slot.terminal(owner).map_err(|_| Error::Corrupt)?;
                }
                save(slot);
                check(queue.finish(&mut nodes, active, reason))?;
                domain.last = None;
                domain.outcome = Some(reason);
                let words = paging.user_words(bootstrap::space(domain)?)?;
                if (reject_kind == 0 && words[0] <= last_progress[index])
                    || words[0] < last_progress[index]
                    || words[1] != 1
                {
                    return Err(Error::Corrupt);
                }
                if reject_kind == 0 {
                    let expects_fault =
                        (is_a && a_action != 0 && a_action != 4) || (!is_a && b_action == 1);
                    if expects_fault != matches!(reason, ExitReason::UserFault(_))
                        || (!expects_fault && reason != ExitReason::Normal(0))
                    {
                        return Err(Error::Corrupt);
                    }
                }
                if queue.enqueue(&mut nodes, domain.thread).is_ok() {
                    return Err(Error::Corrupt);
                }
                serial::write(format_args!("[NEU] Preempt outcome: label={} thread={} work={} boots={} reason={} terminal_dispatch=rejected\n",domain.label,domain.thread.raw(),words[0],words[1],if matches!(reason,ExitReason::Normal(_)){"normal"}else{"fault"})).map_err(|_|Error::Corrupt)?;
                bootstrap::reclaim(paging, &mut nodes, domain)?;
                let other = if is_a { &b } else { &a };
                if other.space.is_some() {
                    bootstrap::survivor(paging, &nodes, &queue, other)?;
                    // Negative preflight tests may terminate a peer before its
                    // normal work-limit instrumentation. Bound the survivor's
                    // remaining computation without requiring another switch.
                    let other_index = 1 - index;
                    if !limited[other_index] {
                        let words = paging.user_words(bootstrap::space(other)?)?;
                        paging.user_limit(bootstrap::space(other)?, words[0] + 20_000_000)?;
                        limited[other_index] = true;
                    }
                }
                if cfg!(feature = "test-preempt-stale") && is_a {
                    bootstrap::stale_audit(paging, &mut nodes, &mut queue, &a)?;
                    bootstrap::survivor(paging, &nodes, &queue, &b)?;
                }
            }
            SliceEvent::Yield(_) => return Err(Error::Corrupt),
        }
    }
    if nodes.active().is_some()
        || !queue.is_empty()
        || !mailbox().empty()
        || BUSY.load(Ordering::Relaxed)
        || a.space.is_some()
        || b.space.is_some()
    {
        return Err(Error::InUse);
    }
    ENABLED.store(false, Ordering::Release);
    interrupts::shutdown_user(paging).map_err(|_| Error::Corrupt)?;
    if paging.stats() != before || paging.table_count() != tables {
        return Err(Error::Corrupt);
    }
    serial::write(format_args!("[NEU] Preempt accounting restored: allocated={} free={} tables={}\n[NEU] Preemption test passed: dispatches={} captures={} queue=empty running=none IF=0\n",before.allocated,before.free,tables,dispatches,COUNT.load(Ordering::Acquire))).map_err(|_|Error::Corrupt)
}

pub(super) fn complete_boundary(root: u64) -> Result<()> {
    let (cs, ss): (u16, u16);
    // SAFETY: Read-only segment selectors on the restored common continuation.
    unsafe {
        asm!("mov {0:x}, cs","mov {1:x}, ss",out(reg) cs,out(reg) ss,options(nomem,nostack,preserves_flags));
    }
    if cs != 8 || ss != 0x10 || paging::current_root() != root || interrupts::flags() & 0x200 != 0 {
        return Err(Error::InUse);
    }
    BUSY.store(false, Ordering::Release);
    Ok(())
}

#[cfg(feature = "multi-test")]
pub(super) fn start(paging: &mut Paging<'_, '_>) -> Result<()> {
    let cr4: u64;
    unsafe {
        asm!("mov {}, cr4",out(reg) cr4,options(nomem,nostack,preserves_flags));
    }
    if cr4 & 0x50600 != 0 || interrupts::flags() & 0x600 != 0 {
        return Err(Error::Flags);
    }
    interrupts::setup_user(paging).map_err(|_| Error::Corrupt)?;
    ENABLED.store(true, Ordering::Release);
    Ok(())
}
#[cfg(feature = "multi-test")]
pub(super) fn arm(
    active: neu_kernel::node::ActiveContext,
    stack: neu_kernel::context::StackBinding,
    peer: bool,
) -> Result<Publication> {
    let mut slot = mailbox();
    let owner = slot.arm(active, stack, peer).map_err(|_| Error::InUse)?;
    save(slot);
    Ok(owner)
}
#[cfg(feature = "multi-test")]
pub(super) fn consume(owner: Publication, frame: Frame) -> Result<SavedContext> {
    let mut slot = mailbox();
    let saved = slot.take(owner).map_err(|_| Error::Corrupt)?;
    if saved.frame != frame || slot.take(owner) != Err(neu_kernel::preemption::Error::Identity) {
        return Err(Error::Corrupt);
    }
    save(slot);
    Ok(saved)
}
#[cfg(feature = "multi-test")]
pub(super) fn terminal(owner: Publication) -> Result<()> {
    let mut slot = mailbox();
    slot.terminal(owner).map_err(|_| Error::Corrupt)?;
    save(slot);
    Ok(())
}
#[cfg(feature = "multi-test")]
pub(super) fn stop(paging: &mut Paging<'_, '_>) -> Result<u64> {
    if !mailbox().empty() || BUSY.load(Ordering::Acquire) {
        return Err(Error::InUse);
    }
    ENABLED.store(false, Ordering::Release);
    interrupts::shutdown_user(paging).map_err(|_| Error::Corrupt)?;
    Ok(COUNT.load(Ordering::Acquire))
}
#[cfg(feature = "multi-test")]
pub(super) fn settle() -> Result<()> {
    // A terminal SYSCALL can leave a real timer pending while FMASK holds IF=0.
    // Route only this bounded common-stack window through the legacy non-
    // scheduling IRQ path; no Node/paging/scheduler borrow is accessed by it.
    ENABLED.store(false, Ordering::Release);
    let result = interrupts::settle_user_pending().map_err(|_| Error::Corrupt);
    ENABLED.store(true, Ordering::Release);
    result
}

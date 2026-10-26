//! BSP-only local clockevent. IF=1 is confined to bounded kernel timer windows
//! and explicitly preemptible userspace. No handler accesses the Node/paging/
//! allocator/controller borrows suspended on the common stack.
use super::{
    exceptions, halt,
    paging::{self, LAPIC_VIEW, Paging},
    serial,
};
#[cfg(feature = "timer-test")]
use core::cell::UnsafeCell;
use core::{
    arch::{asm, global_asm, x86_64::__cpuid},
    ptr,
    sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering},
};
#[cfg(feature = "timer-test")]
use neu_kernel::timer::Session;
use neu_kernel::timer::{self as policy, Capabilities, Configuration, MAX_EVENTS, Mode};
global_asm!(include_str!("interrupts.S"),preemptive=const cfg!(feature="preempt-test") as u8,probe_wait=const if cfg!(feature="cap-test") {0x00400000u64}else if cfg!(feature="multi-test") {0x01000000u64}else{0x04000000u64});
const APIC_BASE: u32 = 0x1b;
const LVT: u32 = 0x320;
const INITIAL: u32 = 0x380;
const CURRENT: u32 = 0x390;
const DIVIDE: u32 = 0x3e0;
const MASK: u32 = 1 << 16;
const VECTOR: u32 = policy::TIMER_VECTOR as u32;
static MODE: AtomicU8 = AtomicU8::new(0);
static ROOT: AtomicU64 = AtomicU64::new(0);
static PHASE: AtomicU8 = AtomicU8::new(0);
static TICKS: AtomicU64 = AtomicU64::new(0);
static EOIS: AtomicU64 = AtomicU64::new(0);
static SPURIOUS: AtomicU64 = AtomicU64::new(0);
static BUSY: AtomicBool = AtomicBool::new(false);
#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct Frame {
    pub gpr: [u64; 15],
    pub vector: u64,
    pub error: u64,
    pub rip: u64,
    pub cs: u64,
    pub flags: u64,
    pub rsp: u64,
    pub ss: u64,
}
const _: () = assert!(core::mem::size_of::<Frame>() == 176);
const _: () = assert!(core::mem::offset_of!(Frame, gpr) == 0);
const _: () = assert!(core::mem::offset_of!(Frame, vector) == 120);
const _: () = assert!(core::mem::offset_of!(Frame, error) == 128);
const _: () = assert!(core::mem::offset_of!(Frame, rip) == 136);
const _: () = assert!(core::mem::offset_of!(Frame, cs) == 144);
const _: () = assert!(core::mem::offset_of!(Frame, flags) == 152);
const _: () = assert!(core::mem::offset_of!(Frame, rsp) == 160);
const _: () = assert!(core::mem::offset_of!(Frame, ss) == 168);
#[derive(Clone, Copy)]
#[cfg(feature = "timer-test")]
struct Trace {
    tick: u64,
    rip: u64,
    rsp: u64,
    cs: u64,
    ss: u64,
    saved_flags: u64,
    handler_flags: u64,
    handler_rsp: u64,
    root: u64,
    phase: u8,
}
#[cfg(feature = "timer-test")]
impl Trace {
    const EMPTY: Self = Self {
        tick: 0,
        rip: 0,
        rsp: 0,
        cs: 0,
        ss: 0,
        saved_flags: 0,
        handler_flags: 0,
        handler_rsp: 0,
        root: 0,
        phase: 0,
    };
}
#[cfg(feature = "timer-test")]
struct Records(UnsafeCell<[Trace; MAX_EVENTS]>);
// SAFETY: One CPU, one non-nested handler writes through raw pointers. No Rust
// trace reference exists during IF=1. The controller reads copies only with IF=0.
#[cfg(feature = "timer-test")]
unsafe impl Sync for Records {}
#[cfg(feature = "timer-test")]
static RECORDS: Records = Records(UnsafeCell::new([Trace::EMPTY; MAX_EVENTS]));
unsafe extern "C" {
    fn neu_timer_wait() -> u64;
    static neu_timer_wait_begin: u8;
    static neu_timer_wait_end: u8;
    static neu_timer_wait_rsp: u64;
    static __stack_bottom: u8;
    static __stack_top: u8;
}
pub fn flags() -> u64 {
    let result: u64;
    // SAFETY: Read-only CPU flags, restoring RSP before returning.
    unsafe {
        asm!("pushfq", "pop {}", out(reg) result, options(preserves_flags));
    }
    result
}
fn rdmsr(msr: u32) -> u64 {
    let (low, high): (u32, u32);
    // SAFETY: All callers validate CPUID/mode before selecting architectural MSRs.
    unsafe {
        asm!("rdmsr", in("ecx") msr, out("eax") low, out("edx") high, options(nomem,nostack,preserves_flags));
    }
    u64::from(low) | (u64::from(high) << 32)
}
fn wrmsr(msr: u32, value: u64) {
    // SAFETY: Privileged, validated architectural register/value; IF=0.
    unsafe {
        asm!("wrmsr", in("ecx") msr, in("eax") value as u32, in("edx") (value >> 32) as u32, options(nostack,preserves_flags));
    }
}
fn input(port: u16) -> u8 {
    let value: u8;
    // SAFETY: BSP owns these PC-platform timer/PIC ports in isolated tests.
    unsafe {
        asm!("in al, dx", in("dx") port, out("al") value, options(nomem,nostack,preserves_flags));
    }
    value
}
fn output(port: u16, value: u8) {
    // SAFETY: Same bounded BSP PC-platform ownership as input().
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") value, options(nostack,preserves_flags));
    }
}
fn read(offset: u32) -> u32 {
    if MODE.load(Ordering::Acquire) == Mode::X2apic as u8 {
        rdmsr(0x800 + offset / 16) as u32
    } else {
        // SAFETY: Mode is published only after the verified supervisor UC mapping.
        unsafe { ptr::read_volatile((LAPIC_VIEW + u64::from(offset)) as *const u32) }
    }
}
fn write(offset: u32, value: u32) {
    if MODE.load(Ordering::Acquire) == Mode::X2apic as u8 {
        wrmsr(0x800 + offset / 16, u64::from(value));
    } else {
        // SAFETY: Same exclusive device mapping; volatile 32-bit aligned access.
        unsafe {
            ptr::write_volatile((LAPIC_VIEW + u64::from(offset)) as *mut u32, value);
        }
    }
}
fn bitmap(base: u32, vector: u8) -> bool {
    read(base + u32::from(vector / 32) * 16) & (1 << (vector % 32)) != 0
}
fn empty(base: u32) -> bool {
    (0..8).all(|i| read(base + i * 16) == 0)
}
fn fatal(vector: u64, reason: &str) -> ! {
    // No EOI for unclassified/spurious/software events. This path never resumes.
    if MODE.load(Ordering::Acquire) != 0 {
        write(LVT, MASK | VECTOR);
        write(INITIAL, 0);
        write(0x80, 0xff);
    }
    serial::emergency(format_args!(
        "[NEU] Interrupt failure: vector={} reason={} ticks={} eois={} IF=0\n",
        vector,
        reason,
        TICKS.load(Ordering::Relaxed),
        EOIS.load(Ordering::Relaxed)
    ));
    halt()
}
#[unsafe(no_mangle)]
extern "C" fn neu_interrupt(pointer: *const Frame) {
    #[cfg(feature = "preempt-test")]
    if super::preemptive::enabled() {
        super::preemptive::interrupt(pointer);
        return;
    }

    if BUSY.swap(true, Ordering::Relaxed) {
        fatal(0, "reentry");
    }
    let address = pointer as u64;
    let bottom = ptr::addr_of!(__stack_bottom) as u64;
    let top = ptr::addr_of!(__stack_top) as u64;
    if address < bottom || address.checked_add(176).is_none_or(|end| end > top) {
        fatal(0, "untrusted stack");
    }
    // SAFETY: Dedicated assembly supplies a complete frame on the guarded stack.
    let f = unsafe { ptr::read(pointer) };
    let current_flags = flags();
    let handler_rsp: u64;
    // SAFETY: Read-only current stack observation; no user values are dereferenced.
    unsafe {
        asm!("mov {}, rsp", out(reg) handler_rsp, options(nomem,nostack,preserves_flags));
    }
    if f.vector > 255
        || current_flags & 0x600 != 0
        || f.cs != 8
        || f.ss != 0x10
        || f.error != 0
        || paging::current_root() != ROOT.load(Ordering::Acquire)
        || !(bottom..address).contains(&handler_rsp)
    {
        fatal(f.vector, "invalid kernel context");
    }
    let vector = f.vector as u8;
    let phase = PHASE.load(Ordering::Acquire);
    let before = TICKS.load(Ordering::Relaxed);
    if MODE.load(Ordering::Acquire) == 0 {
        fatal(f.vector, "inactive controller");
    }
    let in_service = bitmap(0x100, vector);
    let event = policy::event(phase, before, vector, in_service)
        .unwrap_or_else(|_| fatal(f.vector, "unexpected or non-hardware event"));
    if !event.eoi {
        SPURIOUS.store(
            SPURIOUS
                .load(Ordering::Relaxed)
                .checked_add(1)
                .unwrap_or_else(|| fatal(f.vector, "spurious overflow")),
            Ordering::Relaxed,
        );
        BUSY.store(false, Ordering::Relaxed);
        return;
    }
    // SAFETY: Written once by the assembly-only wait routine with IF=0, remains
    // stable throughout its enabled interval. No Rust reference crosses that call.
    let expected_rsp = unsafe { ptr::read_volatile(ptr::addr_of!(neu_timer_wait_rsp)) };
    if f.flags & 0x600 != 0x200
        || !(ptr::addr_of!(neu_timer_wait_begin) as u64..ptr::addr_of!(neu_timer_wait_end) as u64)
            .contains(&f.rip)
        || !(expected_rsp - 16..=expected_rsp).contains(&f.rsp)
        || !f.rsp.is_multiple_of(8)
        || event.ticks > MAX_EVENTS as u64
    {
        fatal(f.vector, "outside safe interrupt window");
    }
    write(0xb0, 0); // Exactly one EOI for this proven in-service timer vector.
    if bitmap(0x100, vector) {
        fatal(f.vector, "EOI did not clear ISR");
    }
    EOIS.store(
        EOIS.load(Ordering::Relaxed)
            .checked_add(1)
            .unwrap_or_else(|| fatal(f.vector, "EOI overflow")),
        Ordering::Relaxed,
    );
    // SAFETY: Bounds checked above, single non-nested writer, no live trace borrow.
    #[cfg(feature = "timer-test")]
    unsafe {
        ptr::write(
            RECORDS.0.get().cast::<Trace>().add(before as usize),
            Trace {
                tick: event.ticks,
                rip: f.rip,
                rsp: f.rsp,
                cs: f.cs,
                ss: f.ss,
                saved_flags: f.flags,
                handler_flags: current_flags,
                handler_rsp,
                root: paging::current_root(),
                phase,
            },
        );
    }
    TICKS.store(event.ticks, Ordering::Release);
    BUSY.store(false, Ordering::Relaxed);
}
fn pit_start(count: u16) -> Result<(), &'static str> {
    let control = input(0x61) & 0x0c; // Preserve parity/NMI control, speaker/gate off.
    output(0x61, control);
    output(0x43, 0xb0); // Channel 2, low/high, binary, mode 0.
    output(0x42, count as u8);
    output(0x42, (count >> 8) as u8);
    if input(0x61) & 0x20 != 0 {
        return Err("PIT output did not reset");
    }
    output(0x61, control | 1);
    Ok(())
}
fn pit_stop() {
    output(0x61, input(0x61) & 0x0c);
}
fn pit_wait() -> Result<(), &'static str> {
    for _ in 0..10_000_000 {
        if input(0x61) & 0x20 != 0 {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err("PIT calibration timeout")
}
fn calibrate(period_us: u64) -> Result<policy::Calibration, &'static str> {
    let mut samples = [0u32; 3];
    write(LVT, MASK | VECTOR);
    write(DIVIDE, 3); // Divide by 16, not a host CPU frequency assumption.
    for sample in &mut samples {
        write(INITIAL, u32::MAX);
        pit_start(policy::REFERENCE_TICKS)?;
        let result = pit_wait();
        *sample = u32::MAX - read(CURRENT);
        write(INITIAL, 0);
        pit_stop();
        result?;
    }
    let calibrated = Configuration::new(period_us, policy::TIMER_VECTOR)
        .and_then(|c| c.calibrate(samples, policy::REFERENCE_TICKS))
        .map_err(|_| "APIC calibration invalid or inconsistent")?;
    serial::write(format_args!("[NEU] Timer calibrated: PIT_Hz={} reference={} samples={},{},{} divided_Hz={} initial={} divide=16 period_us={}\n", policy::PIT_HZ, policy::REFERENCE_TICKS, samples[0],samples[1],samples[2],calibrated.divided_hz,calibrated.initial,period_us)).map_err(|_|"timer serial failure")?;
    Ok(calibrated)
}
fn window(count: u16) -> Result<(), &'static str> {
    pit_start(count)?;
    // SAFETY: All configuration is published, original kernel root/guarded stack
    // are live. Only the bounded assembly wait runs with IF=1; it returns IF=0
    // after checking all 15 GPR canaries and exact RSP. No handler uses our &muts.
    let result = unsafe { neu_timer_wait() };
    pit_stop();
    if result != 0 || flags() & 0x600 != 0 {
        return Err("interrupt return/canary/deadline failure");
    }
    Ok(())
}
fn initialize_controller(paging: &mut Paging<'_, '_>) -> Result<Option<Mode>, &'static str> {
    if flags() & 0x600 != 0 {
        return Err("controller setup flags");
    }
    let before = paging.stats();
    let root = paging.root();
    if __cpuid(0x80000000).eax < 0x80000008 || (0xc0000100..=0xc0000102).any(|msr| rdmsr(msr) != 0)
    {
        return Err("timer needs physical width and zero FS/GS bases");
    }
    let cpuid = __cpuid(1);
    let bits = (__cpuid(0x80000008).eax & 0xff) as u8;
    // Check APIC CPUID before touching its base MSR on unsupported processors.
    if cpuid.edx & (1 << 9) == 0 {
        return Err("local APIC absent");
    }
    let old_base = rdmsr(APIC_BASE);
    let caps = Capabilities {
        apic: true,
        x2apic: cpuid.ecx & (1 << 21) != 0,
        pat: cpuid.edx & (1 << 16) != 0,
        base: old_base,
        bits,
        logical_cpus: policy::logical_cpus(cpuid.ebx, cpuid.edx),
    };
    serial::write(format_args!(
        "[NEU] Timer capabilities: APIC=true x2APIC={} PAT={} base={:#x} bits={} cpus={}\n",
        caps.x2apic, caps.pat, old_base, bits, caps.logical_cpus
    ))
    .map_err(|_| "timer serial failure")?;
    if cfg!(feature = "test-timer-unsupported") {
        if (Capabilities {
            apic: false,
            ..caps
        })
        .select()
            != Err(policy::Error::Capability)
            || rdmsr(APIC_BASE) != old_base
            || paging.stats() != before
            || flags() & 0x200 != 0
        {
            return Err("unsupported controller validation failed");
        }
        serial::write(format_args!("[NEU] Timer unsupported configuration rejected: injected_APIC=false hardware_unchanged=true IF=0\n")).map_err(|_|"timer serial failure")?;
        return Ok(None);
    }
    let mode = caps
        .select()
        .map_err(|_| "unsupported APIC configuration")?;
    let mut leaf = 0;
    let mut pat = 0;
    if mode == Mode::Xapic {
        pat = rdmsr(0x277);
        leaf = paging
            .map_lapic_uc(old_base & neu_kernel::paging::ADDRESS_FIELD, pat)
            .map_err(|_| "invalid APIC UC device mapping")?;
    } else {
        wrmsr(APIC_BASE, old_base | 0x400);
        if rdmsr(APIC_BASE) != old_base | 0x400 {
            return Err("x2APIC enable readback failed");
        }
    }
    ROOT.store(root, Ordering::Release);
    MODE.store(mode as u8, Ordering::Release);
    let version = read(0x30);
    policy::validate_version(version).map_err(|_| "unsupported APIC version")?;
    let id = if mode == Mode::Xapic {
        read(0x20) >> 24
    } else {
        read(0x20)
    };
    if id != cpuid.ebx >> 24 {
        return Err("APIC ID disagrees with CPUID");
    }
    output(0x21, 0xff);
    output(0xa1, 0xff);
    if input(0x21) != 0xff || input(0xa1) != 0xff {
        return Err("PIC mask readback failed");
    }
    write(0x80, 0xff);
    write(0xf0, 0xff);
    for offset in [0x320, 0x330, 0x340, 0x350, 0x360, 0x370] {
        write(offset, MASK);
        if read(offset) & MASK == 0 {
            return Err("APIC LVT mask readback failed");
        }
    }
    if (version >> 16) & 0xff == 6 {
        write(0x2f0, MASK);
    }
    write(INITIAL, 0);
    if !empty(0x100) || !empty(0x200) {
        return Err("firmware left pending APIC events");
    }
    exceptions::configure_interrupts()?;
    write(0xf0, 0x1ff);
    write(0x80, 0);
    if read(0xf0) != 0x1ff || read(0x80) != 0 || read(0x280) != 0 {
        return Err("APIC software enable readback failed");
    }
    serial::write(format_args!("[NEU] Timer controller verified: mode={} base={:#x} id={} version={:#x} vector=48 spurious=255 PIC=masked leaf={:#x} pat={:#x} root={:#x}\n",mode as u8,rdmsr(APIC_BASE),id,version,leaf,pat,root)).map_err(|_|"timer serial failure")?;
    Ok(Some(mode))
}
#[cfg(feature = "timer-test")]
pub fn run(paging: &mut Paging<'_, '_>) -> Result<(), &'static str> {
    if flags() & 0x600 != 0 {
        return Err("timer setup requires IF=0 DF=0");
    }
    let before = paging.stats();
    let tables = paging.table_count();
    let root = paging.root();
    serial::write(format_args!(
        "[NEU] Timer accounting baseline: allocated={} free={} tables={}\n",
        before.allocated, before.free, tables
    ))
    .map_err(|_| "timer serial failure")?;
    let Some(mode) = initialize_controller(paging)? else {
        return Ok(());
    };
    if cfg!(feature = "test-timer-unexpected") {
        // SAFETY: Deliberate isolated DPL0 negative, not hardware timer evidence.
        unsafe {
            asm!("int 0x41", options(noreturn));
        }
    }
    if cfg!(feature = "test-timer-software") {
        // SAFETY: Deliberate timer-vector INT without an ISR bit must fail closed.
        unsafe {
            asm!("int 0x30", options(noreturn));
        }
    }
    if cfg!(feature = "test-timer-spurious") {
        // SAFETY: Exercises the architecturally no-EOI vector at IF=0.
        unsafe {
            asm!("int 0xff");
        }
        if TICKS.load(Ordering::Relaxed) != 0
            || EOIS.load(Ordering::Relaxed) != 0
            || SPURIOUS.load(Ordering::Relaxed) != 1
        {
            return Err("spurious sent an EOI or tick");
        }
        serial::write(format_args!(
            "[NEU] Spurious test returned: ticks=0 eois=0 spurious=1 IF=0\n"
        ))
        .map_err(|_| "timer serial failure")?;
    }
    let calibrated = calibrate(1000)?;
    let mut session = Session::default();
    let runs = if cfg!(feature = "test-timer-repeat") {
        2
    } else {
        1
    };
    for sequence in 1..=runs {
        session.arm().map_err(|_| "timer arm state")?;
        write(LVT, MASK | (1 << 17) | VECTOR);
        write(DIVIDE, 3);
        write(INITIAL, calibrated.initial);
        if read(LVT) != MASK | (1 << 17) | VECTOR
            || read(DIVIDE) != 3
            || read(INITIAL) != calibrated.initial
        {
            return Err("timer configuration readback failed");
        }
        let start = TICKS.load(Ordering::Acquire);
        serial::write(format_args!(
            "[NEU] Timer session armed: sequence={} ticks_before={} root={:#x} reference={}\n",
            sequence,
            start,
            root,
            policy::WINDOW_TICKS
        ))
        .map_err(|_| "timer serial failure")?;
        session.open().map_err(|_| "timer open state")?;
        PHASE.store(policy::RUNNING, Ordering::Release);
        write(LVT, (1 << 17) | VECTOR);
        window(policy::WINDOW_TICKS)?;
        write(LVT, MASK | (1 << 17) | VECTOR);
        write(INITIAL, 0);
        session.drain().map_err(|_| "timer drain state")?;
        PHASE.store(policy::DRAINING, Ordering::Release);
        window(policy::DRAIN_TICKS)?;
        PHASE.store(0, Ordering::Release);
        session
            .stop(!empty(0x200), !empty(0x100))
            .map_err(|_| "timer pending after drain")?;
        let end = TICKS.load(Ordering::Acquire);
        if !(8..=128).contains(&(end - start))
            || EOIS.load(Ordering::Relaxed) != end
            || paging::current_root() != root
        {
            return Err("timer tick/EOI/root accounting");
        }
        pit_start(policy::DRAIN_TICKS)?;
        pit_wait()?;
        pit_stop();
        if TICKS.load(Ordering::Acquire) != end || BUSY.load(Ordering::Relaxed) {
            return Err("ticks changed after stopping");
        }
        serial::write(format_args!("[NEU] Timer session stopped: sequence={} ticks_after={} eois={} ISR=0 IRR=0 IF=0 GPRs=15 RSP=preserved\n",sequence,end,EOIS.load(Ordering::Relaxed))).map_err(|_|"timer serial failure")?;
    }
    write(0x80, 0xff);
    write(0xf0, 0xff);
    if read(LVT) != MASK | (1 << 17) | VECTOR
        || read(INITIAL) != 0
        || read(CURRENT) != 0
        || read(0x80) != 0xff
        || read(0xf0) != 0xff
        || !empty(0x100)
        || !empty(0x200)
    {
        return Err("timer shutdown controller state");
    }
    let total = TICKS.load(Ordering::Acquire);
    for index in 0..total as usize {
        // SAFETY: IF=0, all sources stopped/drained, bounded initialized records.
        let t = unsafe { ptr::read(RECORDS.0.get().cast::<Trace>().add(index)) };
        if t.tick != index as u64 + 1 {
            return Err("nonmonotonic interrupt trace");
        }
        serial::write(format_args!("[NEU] Timer event: tick={} phase={} RIP={:#x} RSP={:#x} CS={:#x} SS={:#x} saved_flags={:#x} handler_flags={:#x} handler_rsp={:#x} root={:#x} ISR_before=1 ISR_after=0\n",t.tick,t.phase,t.rip,t.rsp,t.cs,t.ss,t.saved_flags,t.handler_flags,t.handler_rsp,t.root)).map_err(|_|"timer serial failure")?;
    }
    MODE.store(0, Ordering::Release);
    if mode == Mode::Xapic {
        paging.unmap_lapic().map_err(|_| "APIC mapping teardown")?;
    }
    if paging.stats() != before
        || paging.table_count() != tables
        || paging::current_root() != root
        || flags() & 0x600 != 0
    {
        return Err("timer final ownership/root/flags");
    }
    serial::write(format_args!(
        "[NEU] Timer accounting restored: allocated={} free={} tables={}\n",
        paging.stats().allocated,
        paging.stats().free,
        paging.table_count()
    ))
    .map_err(|_| "timer serial failure")?;
    serial::write(format_args!("[NEU] Timer shutdown verified: ticks={} eois={} spurious={} LVT=masked initial=0 current=0 SVR=disabled TPR=255 PIC=masked IF=0 root={:#x} tables={} accounting=restored\n[NEU] Timer test passed: sessions={}\n",total,EOIS.load(Ordering::Relaxed),SPURIOUS.load(Ordering::Relaxed),root,tables,runs)).map_err(|_|"timer serial failure")?;
    Ok(())
}

#[cfg(feature = "preempt-test")]
static USER_EVENTS: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "preempt-test")]
static USER_INITIAL: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "preempt-test")]
pub(super) fn setup_user(paging: &mut Paging<'_, '_>) -> Result<(), &'static str> {
    initialize_controller(paging)?.ok_or("no user clockevent")?;
    let calibration = calibrate(10_000)?;
    USER_INITIAL.store(u64::from(calibration.initial), Ordering::Release);
    Ok(())
}
#[cfg(feature = "preempt-test")]
pub(super) fn user_device() -> Option<(u64, u64)> {
    (MODE.load(Ordering::Acquire) == Mode::Xapic as u8).then(|| {
        (
            rdmsr(APIC_BASE) & neu_kernel::paging::ADDRESS_FIELD,
            rdmsr(0x277),
        )
    })
}
#[cfg(feature = "preempt-test")]
pub(super) fn arm_user() {
    if flags() & 0x200 != 0 || MODE.load(Ordering::Acquire) == 0 || BUSY.load(Ordering::Relaxed) {
        fatal(48, "unsafe user arm");
    }
    let initial = USER_INITIAL.load(Ordering::Acquire) as u32;
    if initial == 0 || !empty(0x100) || !empty(0x200) {
        fatal(48, "pending user arm");
    }
    PHASE.store(policy::RUNNING, Ordering::Release);
    write(LVT, MASK | VECTOR);
    write(DIVIDE, 3);
    write(INITIAL, initial);
    write(LVT, VECTOR);
    if read(LVT) != VECTOR || read(INITIAL) != initial || read(DIVIDE) != 3 {
        fatal(48, "user timer readback");
    }
}
#[cfg(feature = "preempt-test")]
pub(super) fn stop_user() {
    write(LVT, MASK | VECTOR);
    write(INITIAL, 0);
}
#[cfg(feature = "preempt-test")]
pub(super) fn acknowledge_user(vector: u64) -> u64 {
    if vector > 255 || MODE.load(Ordering::Acquire) == 0 || flags() & 0x200 != 0 {
        fatal(vector, "user controller state");
    }
    let before = TICKS.load(Ordering::Relaxed);
    let event = policy::event(
        PHASE.load(Ordering::Acquire),
        before,
        vector as u8,
        bitmap(0x100, vector as u8),
    )
    .unwrap_or_else(|_| fatal(vector, "unexpected or non-hardware event"));
    if !event.eoi {
        return 0;
    }
    if event.ticks > MAX_EVENTS as u64 {
        fatal(vector, "user event overflow");
    }
    stop_user();
    write(0xb0, 0);
    if bitmap(0x100, vector as u8) {
        fatal(vector, "user EOI did not clear ISR");
    }
    TICKS.store(event.ticks, Ordering::Release);
    EOIS.fetch_add(1, Ordering::Relaxed);
    USER_EVENTS.fetch_add(1, Ordering::Relaxed);
    event.ticks
}
#[cfg(feature = "preempt-test")]
pub(super) fn reject_user(vector: u64, reason: &str) -> ! {
    fatal(vector, reason)
}
#[cfg(feature = "preempt-test")]
pub(super) fn shutdown_user(paging: &mut Paging<'_, '_>) -> Result<(), &'static str> {
    if flags() & 0x200 != 0 || paging::current_root() != ROOT.load(Ordering::Acquire) {
        return Err("user shutdown flags/root");
    }
    stop_user();
    PHASE.store(policy::DRAINING, Ordering::Release);
    window(policy::DRAIN_TICKS)?;
    PHASE.store(0, Ordering::Release);
    if !empty(0x100)
        || !empty(0x200)
        || TICKS.load(Ordering::Relaxed) != EOIS.load(Ordering::Relaxed)
    {
        return Err("user clock not drained");
    }
    let total = TICKS.load(Ordering::Acquire);
    pit_start(policy::DRAIN_TICKS)?;
    pit_wait()?;
    pit_stop();
    if TICKS.load(Ordering::Acquire) != total || BUSY.load(Ordering::Relaxed) {
        return Err("user clock not stable");
    }
    write(0x80, 0xff);
    write(0xf0, 0xff);
    if read(LVT) != MASK | VECTOR
        || read(INITIAL) != 0
        || read(CURRENT) != 0
        || read(0xf0) != 0xff
        || read(0x80) != 0xff
    {
        return Err("user clock shutdown readback");
    }
    let mode = MODE.swap(0, Ordering::AcqRel);
    if mode == Mode::Xapic as u8 {
        paging.unmap_lapic().map_err(|_| "user APIC unmap")?;
    }
    serial::write(format_args!("[NEU] Preempt timer stopped: ticks={} eois={} user={} drain={} ISR=0 IRR=0 initial=0 current=0 SVR=disabled IF=0\n",total,EOIS.load(Ordering::Relaxed),USER_EVENTS.load(Ordering::Relaxed),total-USER_EVENTS.load(Ordering::Relaxed))).map_err(|_|"user timer serial")?;
    Ok(())
}

#[cfg(feature = "preempt-test")]
pub(super) fn user_in_service(vector: u8) -> bool {
    bitmap(0x100, vector)
}

#[cfg(feature = "multi-test")]
pub(super) fn settle_user_pending() -> Result<(), &'static str> {
    if flags() & 0x600 != 0
        || paging::current_root() != ROOT.load(Ordering::Acquire)
        || !empty(0x100)
    {
        return Err("invalid deferred drain boundary");
    }
    if empty(0x200) {
        return Ok(());
    }
    stop_user();
    let before = TICKS.load(Ordering::Acquire);
    PHASE.store(policy::DRAINING, Ordering::Release);
    window(policy::DRAIN_TICKS)?;
    PHASE.store(0, Ordering::Release);
    let after = TICKS.load(Ordering::Acquire);
    if !empty(0x100)
        || !empty(0x200)
        || after != before + 1
        || EOIS.load(Ordering::Acquire) != after
    {
        return Err("deferred clockevent did not drain");
    }
    serial::write(format_args!("[NEU] Preempt deferred drained: before={} after={} eois={} root={:#x} IF=0 kernel_switch=false\n",before,after,EOIS.load(Ordering::Relaxed),paging::current_root())).map_err(|_|"deferred drain serial")?;
    Ok(())
}

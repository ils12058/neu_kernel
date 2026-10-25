//! Experimental single-CPU native boundary. Entry uses only RIP-relative kernel
//! storage before selecting the guarded stack; there is no GS-relative state.
use super::{
    exceptions,
    paging::{self, Paging},
    serial, userspace,
};
use core::{
    arch::{asm, global_asm, x86_64::__cpuid},
    cell::UnsafeCell,
    mem, ptr,
    sync::atomic::{AtomicBool, Ordering},
};
use neu_kernel::{
    paging::{Error, PAGE, Result},
    syscall::{self as abi, Frame, Msrs, Outcome, Rejection, ReturnContract},
};

const CANARIES: [u64; 6] = [
    0x1122334455667788,
    0x2233445566778899,
    0x33445566778899aa,
    0x445566778899aabb,
    0x5566778899aabbcc,
    0x66778899aabbccdd,
];
pub const PRIVATE_VALUE: u64 = 0x5152535461626364;
global_asm!(include_str!("native.S"),private_value=const PRIVATE_VALUE,user_ss=const abi::USER_SS,user_cs=const abi::USER_CS,
    kernel_ss=const abi::KERNEL_SS,star=const abi::STAR,fmask=const abi::ENTRY_MASK,query=const abi::ABI_QUERY,exit=const abi::THREAD_EXIT,
    info=const abi::ABI_INFO,unsupported=const abi::NOT_SUPPORTED,invalid=const abi::INVALID_ARGUMENT,
    canary_bx=const CANARIES[0],canary_bp=const CANARIES[1],canary_12=const CANARIES[2],
    canary_13=const CANARIES[3],canary_14=const CANARIES[4],canary_15=const CANARIES[5]);
unsafe extern "C" {
    fn neu_native_entry();
    static mut neu_native_busy: u64;
    #[cfg(feature = "node-test")]
    static mut neu_native_observed: u64;
    static neu_native_payload: u8;
    static neu_native_payload_end: u8;
    static neu_native_resume: u8;
    static neu_native_syscall_site: u8;
    static neu_native_bad_rsp_resume: u8;
    static __native_guard_low: u8;
    static __native_guard_high: u8;
    static __native_emergency_bottom: u8;
    static __native_emergency_top: u8;
}
pub fn guards() -> [u64; 2] {
    [
        ptr::addr_of!(__native_guard_low) as u64,
        ptr::addr_of!(__native_guard_high) as u64,
    ]
}
pub fn emergency_stack() -> (u64, u64) {
    (
        ptr::addr_of!(__native_emergency_bottom) as u64,
        ptr::addr_of!(__native_emergency_top) as u64,
    )
}
struct Cell<T>(UnsafeCell<T>);
// SAFETY: One BSP, IF=0, one active fixture. Non-reentrant native entry is
// guarded in assembly before saving user RSP. Critical IST handlers are fatal.
unsafe impl<T> Sync for Cell<T> {}
#[derive(Clone, Copy, Default)]
struct Session {
    kind: u64,
    root: u64,
    previous_efer: u64,
    entries: u64,
    returns: u64,
    queries: u64,
    errors: u64,
    terminal: u64,
    valid: bool,
    #[cfg(feature = "node-test")]
    binding: Option<neu_kernel::node::ActiveContext>,
    #[cfg(feature = "node-test")]
    outcome: Option<neu_kernel::node::ExitReason>,
}
static SESSION: Cell<Session> = Cell(UnsafeCell::new(Session {
    kind: 0,
    root: 0,
    previous_efer: 0,
    entries: 0,
    returns: 0,
    queries: 0,
    errors: 0,
    terminal: 99,
    valid: false,
    #[cfg(feature = "node-test")]
    binding: None,
    #[cfg(feature = "node-test")]
    outcome: None,
}));
static ACTIVE: AtomicBool = AtomicBool::new(false);
#[repr(C)]
#[derive(Clone, Copy)]
struct Record {
    frame: Frame,
    kernel_rsp: u64,
    root: u64,
    kernel_cs: u64,
    kernel_ss: u64,
    flags: u64,
}
#[unsafe(no_mangle)]
static NEU_NATIVE_LAST: Cell<Option<Record>> = Cell(UnsafeCell::new(None));
fn session() -> Session {
    // SAFETY: Single active CPU; called only on the trusted kernel path.
    unsafe { ptr::read(SESSION.0.get()) }
}
fn save(value: Session) {
    // SAFETY: No concurrent normal/user execution overlaps this write.
    unsafe {
        ptr::write(SESSION.0.get(), value);
    }
}
pub fn running() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}
fn msr_read(number: u32) -> u64 {
    let (low, high): (u32, u32);
    // SAFETY: Ring 0, supported x86-64 MSRs; native run checks SYSCALL support.
    unsafe {
        asm!("rdmsr",in("ecx") number,out("eax") low,out("edx") high,options(nomem,nostack,preserves_flags));
    }
    (u64::from(high) << 32) | u64::from(low)
}
fn msr_write(number: u32, value: u64) {
    // SAFETY: Called at Ring 0/IF=0 with canonical entry and valid selector/mask
    // values. No user pointer or user-provided MSR number reaches this helper.
    unsafe {
        asm!("wrmsr",in("ecx") number,in("eax") value as u32,in("edx") (value>>32) as u32,options(nostack,preserves_flags));
    }
}
fn read_msrs() -> Msrs {
    Msrs {
        efer: msr_read(0xc0000080),
        star: msr_read(0xc0000081),
        lstar: msr_read(0xc0000082),
        fmask: msr_read(0xc0000084),
    }
}
fn restore(old: Msrs) -> Result<()> {
    msr_write(0xc0000080, msr_read(0xc0000080) & !1);
    msr_write(0xc0000081, old.star);
    msr_write(0xc0000082, old.lstar);
    msr_write(0xc0000084, old.fmask);
    msr_write(0xc0000080, old.efer);
    if read_msrs() != old {
        return Err(Error::Corrupt);
    }
    Ok(())
}
fn configure() -> Result<Msrs> {
    if __cpuid(0x80000000).eax < 0x80000001 || __cpuid(0x80000001).edx & (1 << 11) == 0 {
        return Err(Error::Flags);
    }
    let old = read_msrs();
    if old.efer & 1 != 0 {
        return Err(Error::InUse);
    }
    let entry = neu_native_entry as *const () as u64;
    msr_write(0xc0000081, abi::STAR);
    msr_write(0xc0000082, entry);
    msr_write(0xc0000084, abi::ENTRY_MASK);
    msr_write(0x174, 0); // Legacy SYSENTER remains disabled.
    msr_write(0xc0000080, old.efer | 1);
    let actual = read_msrs();
    if !abi::valid_msrs(actual, old.efer, entry) || msr_read(0x174) != 0 {
        restore(old)?;
        return Err(Error::Flags);
    }
    if serial::write(format_args!(
        "[NEU] Native MSRs verified: EFER={:#x} STAR={:#x} LSTAR={:#x} FMASK={:#x} SYSENTER=0\n",
        actual.efer, actual.star, actual.lstar, actual.fmask
    ))
    .is_err()
    {
        restore(old)?;
        return Err(Error::Corrupt);
    }
    Ok(old)
}
fn site(symbol: *const u8) -> u64 {
    userspace::CODE + (symbol as u64 - ptr::addr_of!(neu_native_payload) as u64)
}
fn return_contract(root: u64) -> ReturnContract {
    ReturnContract {
        root,
        code_start: userspace::CODE,
        code_end: site(ptr::addr_of!(neu_native_payload_end)),
        stack_start: userspace::STACK,
        stack_end: userspace::STACK + PAGE,
        resumes: [
            site(ptr::addr_of!(neu_native_resume)),
            site(ptr::addr_of!(neu_native_bad_rsp_resume)),
        ],
    }
}
fn fatal(reason: &str) -> ! {
    serial::emergency(format_args!("[NEU] BOOT FAILURE: native {reason}\n"));
    super::halt()
}
fn finish(reason: u64, status: u64) -> ! {
    let mut s = session();
    #[cfg(feature = "node-test")]
    if let Some(binding) = s.binding {
        use neu_kernel::node::ExitReason;
        if s.outcome.is_none() && reason == 0 {
            s.outcome = Some(ExitReason::Normal(status as u8));
        }
        s.terminal = reason;
        s.valid = paging::current_root() == binding.context.root
            && s.root == binding.context.root
            && s.outcome.is_some();
        save(s);
        serial::write(format_args!("[NEU] Node terminal bound: node={} thread={} space={} root={:#x} reason={} status={}\n",binding.node.raw(),binding.thread.raw(),binding.context.space.raw(),s.root,reason,status)).unwrap_or_else(|_|super::halt());
        terminal_exit(s, reason, status);
    }
    let expected_reason = match s.kind {
        0..=2 => 0,
        3 => Rejection::Rip as u64,
        4 => Rejection::Rsp as u64,
        5 => Rejection::Flags as u64,
        6 => Rejection::Selectors as u64,
        _ => 6,
    };
    let (entries, returns, queries, errors) = match s.kind {
        0 => (3, 2, 2, 0),
        1 | 2 => (4, 3, 2, 1),
        7 => (0, 0, 0, 0),
        _ => (1, 0, 0, 0),
    };
    s.terminal = reason;
    s.valid = reason == expected_reason
        && status == 0
        && s.entries == entries
        && s.returns == returns
        && s.queries == queries
        && s.errors == errors;
    save(s);
    terminal_exit(s, reason, status)
}
fn terminal_exit(s: Session, reason: u64, status: u64) -> ! {
    ACTIVE.store(false, Ordering::Relaxed);
    // SAFETY: No native instruction can re-enter before the trusted continuation
    // disables SCE; no user execution is resumed by this terminal path.
    unsafe {
        ptr::write_volatile(ptr::addr_of_mut!(neu_native_busy), 0);
    }
    serial::write(format_args!("[NEU] Native terminal: kind={} reason={} status={} entries={} returns={} queries={} errors={} verified={}\n",s.kind,reason,status,s.entries,s.returns,s.queries,s.errors,s.valid)).unwrap_or_else(|_| super::halt());
    userspace::resume_native()
}
#[unsafe(no_mangle)]
extern "C" fn neu_native_nested() -> ! {
    fatal("nested entry; emergency stack selected")
}

#[unsafe(no_mangle)]
extern "C" fn neu_syscall(pointer: *mut Frame) {
    let (bottom, top) = userspace::kernel_stack();
    let address = pointer as u64;
    if !running() || address != top - mem::size_of::<Frame>() as u64 || address & 15 != 0 {
        fatal("invalid entry frame");
    }
    let (kernel_rsp, flags): (u64, u64);
    let (cs, ss): (u16, u16);
    // SAFETY: Assembly established the exact guarded stack before saving this
    // 160-byte frame. All original user addresses are read only as scalars.
    let mut frame = unsafe {
        asm!("mov {}, rsp",out(reg) kernel_rsp,options(nomem,nostack,preserves_flags));
        asm!("pushfq","pop {}",out(reg) flags,options(preserves_flags));
        asm!("mov {:x}, cs",out(reg) cs,options(nomem,nostack,preserves_flags));
        asm!("mov {:x}, ss",out(reg) ss,options(nomem,nostack,preserves_flags));
        ptr::read_volatile(pointer)
    };
    let mut s = session();
    let root = paging::current_root();
    // SAFETY: Kernel-only entry guard, written before user RSP was saved.
    let busy = unsafe { ptr::read_volatile(ptr::addr_of!(neu_native_busy)) };
    if cs as u64 != abi::KERNEL_CS
        || ss as u64 != abi::KERNEL_SS
        || flags & abi::ENTRY_MASK != 0
        || !(bottom..address).contains(&kernel_rsp)
        || root != s.root
        || busy != 1
        || !abi::valid_msrs(
            read_msrs(),
            s.previous_efer,
            neu_native_entry as *const () as u64,
        )
    {
        fatal("CPU, root, MSR or stack invariant");
    }
    s.entries += 1;
    save(s);
    #[cfg(feature = "node-test")]
    if let Some(binding) = s.binding {
        if binding.context.root != root {
            fatal("stale Node context");
        }
        serial::write(format_args!(
            "[NEU] Node syscall bound: node={} thread={} space={} root={:#x} seq={}\n",
            binding.node.raw(),
            binding.thread.raw(),
            binding.context.space.raw(),
            root,
            s.entries
        ))
        .unwrap_or_else(|_| super::halt());
    }
    let before = frame.registers;
    // SAFETY: Diagnostic record is supervisor-only and outlives terminal halt.
    unsafe {
        ptr::write_volatile(
            NEU_NATIVE_LAST.0.get(),
            Some(Record {
                frame,
                kernel_rsp,
                root,
                kernel_cs: cs as u64,
                kernel_ss: ss as u64,
                flags,
            }),
        );
    }
    serial::write(format_args!("[NEU] Native entry: seq={} op={:#x} arg1={:#x} RIP={:#x} RSP={:#x} R11={:#x}\n[NEU] Native kernel: CS={:#x} SS={:#x} RSP={:#x} frame={:#x} RFLAGS={:#x} CR3={:#x}\n[NEU] Native canaries: RBX={:#x} RBP={:#x} R12={:#x} R13={:#x} R14={:#x} R15={:#x}\n",s.entries,before.rax,before.rdi,frame.context.rip,frame.context.rsp,before.r11,cs,ss,kernel_rsp,address,flags,root,before.rbx,before.rbp,before.r12,before.r13,before.r14,before.r15)).unwrap_or_else(|_| super::halt());
    // Isolated corruption hooks never exist in ordinary or Phase 2A images.
    if s.entries == 1 {
        if s.kind == 3 {
            frame.context.rip = 0x800000000000;
        }
        if s.kind == 6 {
            frame.context.cs = abi::KERNEL_CS;
            frame.context.ss = abi::KERNEL_SS;
        }
    }
    let contract = return_contract(root);
    if let Err(reason) = abi::validate_return(frame.context, &contract, root) {
        serial::write(format_args!("[NEU] Native return rejected: reason={} RIP={:#x} RSP={:#x} CS={:#x} SS={:#x} RFLAGS={:#x}\n",reason as u64,frame.context.rip,frame.context.rsp,frame.context.cs,frame.context.ss,frame.context.flags)).unwrap_or_else(|_| super::halt());
        #[cfg(feature = "node-test")]
        if s.binding.is_some() {
            use neu_kernel::node::{ExitReason, Fault, FaultSource};
            s.outcome = Some(ExitReason::UserFault(Fault {
                source: FaultSource::RejectedReturn,
                vector: 0,
                error: reason as u64,
                rip: frame.context.rip,
                rsp: frame.context.rsp,
                cs: frame.context.cs,
                ss: frame.context.ss,
                flags: frame.context.flags,
                cr2: 0,
            }));
            save(s);
        }
        finish(reason as u64, 0);
    }
    match abi::dispatch(before.rax, before.arguments()) {
        Outcome::Exit(status) => {
            serial::write(format_args!(
                "[NEU] Native thread_exit: status={}\n",
                status
            ))
            .unwrap_or_else(|_| super::halt());
            finish(0, u64::from(status));
        }
        Outcome::Return { status, scalar } => {
            if before.rax == abi::ABI_QUERY {
                s.queries += 1;
            }
            if status < 0 {
                s.errors += 1;
            }
            s.returns += 1;
            save(s);
            frame.registers.rax = status as u64;
            frame.registers.rdx = scalar;
        }
    }
    if !before.preserved(frame.registers)
        || abi::validate_return(frame.context, &contract, paging::current_root()).is_err()
    {
        fatal("return contract changed during dispatch");
    }
    serial::write(format_args!("[NEU] Native return prepared: seq={} RAX={:#x} RDX={:#x} RIP={:#x} RSP={:#x} preserved=true\n",s.entries,frame.registers.rax,frame.registers.rdx,frame.context.rip,frame.context.rsp)).unwrap_or_else(|_| super::halt());
    // SAFETY: Only this kernel stack frame is written. Assembly restores all
    // registers then executes IRETQ using the validated immutable-root context.
    unsafe {
        ptr::write_volatile(pointer, frame);
    }
}

pub(super) fn user_fault(frame: exceptions::ExceptionFrame, cr2: u64, handler_rsp: u64) -> ! {
    let s = session();
    let (bottom, top) = userspace::kernel_stack();
    #[cfg(feature = "node-test")]
    if let Some(binding) = s.binding {
        use neu_kernel::node::{ExitReason, Fault, FaultSource};
        if frame.cs != abi::USER_CS
            || frame.ss != abi::USER_SS
            || !(bottom..top).contains(&handler_rsp)
            || paging::current_root() != binding.context.root
            || s.root != binding.context.root
        {
            fatal("Node fault context mismatch");
        }
        let fault = Fault {
            source: FaultSource::Cpu,
            vector: frame.vector,
            error: frame.error,
            rip: frame.rip,
            rsp: frame.rsp,
            cs: frame.cs,
            ss: frame.ss,
            flags: frame.flags,
            cr2,
        };
        let mut s = s;
        s.outcome = Some(ExitReason::UserFault(fault));
        save(s);
        serial::write(format_args!("[NEU] Node user fault: node={} thread={} space={} root={:#x} vector={} error={:#x} RIP={:#x} CR2={:#x} CS={:#x} SS={:#x} RSP={:#x} RFLAGS={:#x} handler_rsp={:#x}\n",binding.node.raw(),binding.thread.raw(),binding.context.space.raw(),s.root,frame.vector,frame.error,frame.rip,cr2,frame.cs,frame.ss,frame.rsp,frame.flags,handler_rsp)).unwrap_or_else(|_|super::halt());
        finish(8, 0);
    }
    let valid = s.kind == 7
        && s.entries == 0
        && frame.vector == 6
        && frame.error == 0
        && frame.cs == abi::USER_CS
        && frame.ss == abi::USER_SS
        && frame.rip == site(ptr::addr_of!(neu_native_syscall_site))
        && frame.rsp == userspace::USER_RSP - 8
        && frame.gpr[14] == abi::ABI_QUERY
        && (bottom..top).contains(&handler_rsp)
        && paging::current_root() == s.root
        && msr_read(0xc0000080) & 1 == 0;
    serial::write(format_args!("[NEU] Native user fault: vector={} error={:#x} RIP={:#x} CR2={:#x} CS={:#x} SS={:#x} RSP={:#x} handler_rsp={:#x} verified={}\n",frame.vector,frame.error,frame.rip,cr2,frame.cs,frame.ss,frame.rsp,handler_rsp,valid)).unwrap_or_else(|_| super::halt());
    if valid {
        finish(6, 0);
    } else {
        finish(99, 0);
    }
}

#[cfg(not(feature = "node-test"))]
pub fn run(paging: &mut Paging<'_, '_>) -> Result<()> {
    let kind = if cfg!(feature = "test-native-unknown") {
        1
    } else if cfg!(feature = "test-native-exit-invalid") {
        2
    } else if cfg!(feature = "test-native-return-rip") {
        3
    } else if cfg!(feature = "test-native-return-rsp") {
        4
    } else if cfg!(feature = "test-native-return-flags") {
        5
    } else if cfg!(feature = "test-native-return-selectors") {
        6
    } else if cfg!(feature = "test-native-disabled") {
        7
    } else {
        0
    };
    let length =
        ptr::addr_of!(neu_native_payload_end) as usize - ptr::addr_of!(neu_native_payload) as usize;
    if length == 0 || length > PAGE as usize {
        return Err(Error::Capacity);
    }
    // SAFETY: Linker-bounded immutable kernel R/NX fixture bytes, never user data.
    let bytes = unsafe { core::slice::from_raw_parts(ptr::addr_of!(neu_native_payload), length) };
    let space = paging.prepare_user(bytes, kind, None)?;
    if let Err(error) = exceptions::configure_native(true) {
        paging.destroy_user(space)?;
        return Err(error);
    }
    let old = match configure() {
        Ok(old) => old,
        Err(error) => {
            exceptions::configure_native(false)?;
            paging.destroy_user(space)?;
            return Err(error);
        }
    };
    save(Session {
        kind,
        root: space.root,
        previous_efer: old.efer,
        terminal: 99,
        ..Session::default()
    });
    ACTIVE.store(true, Ordering::Relaxed);
    if kind == 7 {
        msr_write(0xc0000080, old.efer & !1);
    }
    if serial::write(format_args!(
        "[NEU] Native user space ready: kind={} root={:#x} kernel={:#x} entry={:#x} rsp={:#x}\n",
        kind,
        space.root,
        paging.root(),
        userspace::CODE,
        userspace::USER_RSP
    ))
    .is_err()
    {
        ACTIVE.store(false, Ordering::Relaxed);
        restore(old)?;
        exceptions::configure_native(false)?;
        paging.destroy_user(space)?;
        return Err(Error::Corrupt);
    }
    paging.activate_user(&space)?;
    userspace::enter_native(paging.root());
    restore(old)?;
    exceptions::configure_native(false)?;
    paging.deactivate_user(&space)?;
    let s = session();
    let data = paging.user_result(&space)?;
    paging.destroy_user(space)?;
    serial::write(format_args!(
        "[NEU] Native teardown verified: kernel root restored, MSRs and accounting restored\n"
    ))
    .map_err(|_| Error::Corrupt)?;
    if !s.valid || data != if kind <= 2 { 42 } else { 0 } {
        return Err(Error::Corrupt);
    }
    serial::write(format_args!(
        "[NEU] Native ABI test passed: kind={}\n",
        kind
    ))
    .map_err(|_| Error::Corrupt)?;
    Ok(())
}

#[cfg(feature = "node-test")]
pub fn payload() -> &'static [u8] {
    let length =
        ptr::addr_of!(neu_native_payload_end) as usize - ptr::addr_of!(neu_native_payload) as usize;
    if length == 0 || length > PAGE as usize {
        fatal("Node fixture extent");
    }
    // SAFETY: Bounded immutable bytes in the kernel image, copied before user RX.
    unsafe { core::slice::from_raw_parts(ptr::addr_of!(neu_native_payload), length) }
}
#[cfg(feature = "node-test")]
pub fn execute_node(
    paging: &mut Paging<'_, '_>,
    space: &super::paging::UserSpace,
    binding: neu_kernel::node::ActiveContext,
    kind: u64,
) -> Result<neu_kernel::node::ExitReason> {
    if running()
        || paging::current_root() != paging.root()
        || binding.context.space != space.id()
        || binding.context.root != space.root
        || binding.context.entry != userspace::CODE
        || binding.context.rsp != userspace::USER_RSP
    {
        return Err(Error::NotOwned);
    }
    // SAFETY: No active entry or user context; reset the per-run observation
    // state only after requiring the previous non-reentrant guard to be clear.
    unsafe {
        if ptr::read_volatile(ptr::addr_of!(neu_native_busy)) != 0 {
            return Err(Error::InUse);
        }
        ptr::write_volatile(ptr::addr_of_mut!(neu_native_observed), 0);
    }
    exceptions::configure_native(true)?;
    let old = configure()?;
    save(Session {
        kind,
        root: space.root,
        previous_efer: old.efer,
        terminal: 99,
        binding: Some(binding),
        outcome: None,
        ..Session::default()
    });
    ACTIVE.store(true, Ordering::Relaxed);
    serial::write(format_args!("[NEU] Node execution bound: node={} thread={} space={} root={:#x}\n[NEU] Native user space ready: kind={} root={:#x} kernel={:#x} entry={:#x} rsp={:#x}\n",binding.node.raw(),binding.thread.raw(),space.id().raw(),space.root,kind,space.root,paging.root(),userspace::CODE,userspace::USER_RSP)).map_err(|_|Error::Corrupt)?;
    paging.activate_user(space)?;
    userspace::enter_native(paging.root());
    restore(old)?;
    exceptions::configure_native(false)?;
    paging.deactivate_user(space)?;
    let ended = session();
    if !ended.valid
        || ended.binding != Some(binding)
        || running()
        || paging::current_root() != paging.root()
        // SAFETY: Back on the original kernel stack/root with user entry ended.
        || unsafe { ptr::read_volatile(ptr::addr_of!(neu_native_busy)) } != 0
    {
        return Err(Error::Corrupt);
    }
    serial::write(format_args!(
        "[NEU] Node execution restored: node={} thread={} root={:#x} MSRs=restored guard=clear\n",
        binding.node.raw(),
        binding.thread.raw(),
        paging.root()
    ))
    .map_err(|_| Error::Corrupt)?;
    ended.outcome.ok_or(Error::Corrupt)
}

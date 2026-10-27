//! One isolated, test-only Ring 3 execution. All user addresses remain scalars;
//! the boundary never dereferences a user-supplied pointer or resumes its frame.
#[cfg(not(feature = "native-test"))]
use super::paging::Paging;
use super::{exceptions, paging, serial};
use core::{
    arch::{asm, global_asm},
    cell::UnsafeCell,
    ptr,
    sync::atomic::{AtomicBool, Ordering},
};
#[cfg(not(feature = "native-test"))]
use neu_kernel::paging::{Error, Result};
use neu_kernel::paging::{PAGE, USER_BASE};

global_asm!(include_str!("userspace.S"));
pub const CODE: u64 = USER_BASE;
pub const CONSTANTS: u64 = CODE + PAGE;
pub const DATA: u64 = CODE + 2 * PAGE;
pub const GUARD: u64 = CODE + 3 * PAGE;
pub const STACK: u64 = CODE + 4 * PAGE;
pub const USER_RSP: u64 = STACK + PAGE - 8;
pub const UNMAPPED: u64 = CODE + 6 * PAGE;
pub use neu_kernel::syscall::{USER_CS, USER_SS};
unsafe extern "C" {
    #[cfg(not(feature = "native-test"))]
    static neu_user_payload: u8;
    #[cfg(not(feature = "native-test"))]
    static neu_user_payload_end: u8;
    #[cfg(not(feature = "native-test"))]
    static neu_user_done_resume: u8;
    #[cfg(not(feature = "native-test"))]
    static neu_user_kernel_site: u8;
    #[cfg(not(feature = "native-test"))]
    static neu_user_readonly_site: u8;
    #[cfg(not(feature = "native-test"))]
    static neu_user_unmapped_site: u8;
    #[cfg(not(feature = "native-test"))]
    static neu_user_privileged_site: u8;
    #[cfg(not(feature = "native-test"))]
    static neu_user_gate_site: u8;
    #[cfg(not(feature = "native-test"))]
    static neu_user_guard_site: u8;
    static __user_kernel_bottom: u8;
    static __user_kernel_top: u8;
    static __user_kernel_guard_low: u8;
    static __user_kernel_guard_high: u8;
    #[cfg(not(feature = "native-test"))]
    fn neu_enter_user(rip: u64, rsp: u64, kernel_root: u64);
    fn neu_resume_kernel() -> !;
}
pub fn kernel_guards() -> [u64; 2] {
    [
        ptr::addr_of!(__user_kernel_guard_low) as u64,
        ptr::addr_of!(__user_kernel_guard_high) as u64,
    ]
}
pub fn kernel_stack() -> (u64, u64) {
    (
        ptr::addr_of!(__user_kernel_bottom) as u64,
        ptr::addr_of!(__user_kernel_top) as u64,
    )
}
struct Cell<T>(UnsafeCell<T>);
// SAFETY: One BSP, IF=0. Kernel writes the contract before entry; the boundary
// writes the scalar result before resuming the suspended kernel continuation.
unsafe impl<T> Sync for Cell<T> {}
#[derive(Clone, Copy)]
struct Contract {
    kind: u64,
    root: u64,
    rip: u64,
    vector: u64,
    error: u64,
    cr2: u64,
    rsp: u64,
}
static CONTRACT: Cell<Contract> = Cell(UnsafeCell::new(Contract {
    kind: 0,
    root: 0,
    rip: 0,
    vector: 0,
    error: 0,
    cr2: 0,
    rsp: 0,
}));
static ACTIVE: AtomicBool = AtomicBool::new(false);
static VALID: AtomicBool = AtomicBool::new(false);
#[repr(C)]
#[derive(Clone, Copy)]
struct UserRecord {
    frame: exceptions::ExceptionFrame,
    cr2: u64,
    handler_rsp: u64,
    root: u64,
    kernel_cs: u64,
}
#[unsafe(no_mangle)]
static NEU_USER_RECORD: Cell<Option<UserRecord>> = Cell(UnsafeCell::new(None));

#[cfg(not(feature = "native-test"))]
fn site(symbol: *const u8) -> u64 {
    CODE + symbol as u64 - ptr::addr_of!(neu_user_payload) as u64
}
#[cfg(not(feature = "native-test"))]
fn contract(kind: u64, root: u64) -> Contract {
    let (vector, error, rip, cr2) = match kind {
        0 => (128, 0, site(ptr::addr_of!(neu_user_done_resume)), 0),
        1 => (
            14,
            5,
            site(ptr::addr_of!(neu_user_kernel_site)),
            0xffffffff80000000,
        ),
        2 => (
            14,
            7,
            site(ptr::addr_of!(neu_user_readonly_site)),
            CONSTANTS,
        ),
        3 => (14, 21, DATA + 0x100, DATA + 0x100),
        4 => (14, 4, site(ptr::addr_of!(neu_user_unmapped_site)), UNMAPPED),
        5 => (13, 0, site(ptr::addr_of!(neu_user_privileged_site)), 0),
        6 => (13, 0x40a, site(ptr::addr_of!(neu_user_gate_site)), 0),
        _ => (14, 4, site(ptr::addr_of!(neu_user_guard_site)), GUARD),
    };
    Contract {
        kind,
        root,
        rip,
        vector,
        error,
        cr2,
        rsp: USER_RSP - if kind == 3 { 8 } else { 0 },
    }
}

pub(super) fn boundary(frame: exceptions::ExceptionFrame, cr2: u64, handler_rsp: u64) -> ! {
    if !ACTIVE.swap(false, Ordering::Relaxed) {
        super::halt();
    }
    let current_cs: u16;
    // SAFETY: Interrupt/exception entry has switched to trusted CS and RSP0.
    unsafe {
        asm!("mov {:x}, cs", out(reg) current_cs, options(nomem,nostack,preserves_flags));
    }
    // SAFETY: Initialized before ACTIVE, single CPU, immutable while in user mode.
    let expected = unsafe { ptr::read(CONTRACT.0.get()) };
    let (bottom, top) = kernel_stack();
    let root = paging::current_root();
    let valid = frame.cs == USER_CS
        && frame.ss == USER_SS
        && current_cs == 8
        && frame.vector == expected.vector
        && frame.error == expected.error
        && frame.rip == expected.rip
        && frame.rsp == expected.rsp
        && (frame.vector != 14 || cr2 == expected.cr2)
        && frame.flags
            & ((1 << 8) | (1 << 9) | (1 << 10) | (3 << 12) | (1 << 14) | (1 << 17) | (1 << 18))
            == 0
        && frame.flags & 2 != 0
        && (bottom..top).contains(&handler_rsp)
        && root == expected.root
        && frame.gpr[14] == 42
        && frame.gpr[13] == 42
        && frame.gpr[7] == USER_CS
        && frame.gpr[6] == USER_SS;
    // SAFETY: Kernel-only record, no user pointers; one boundary writer.
    unsafe {
        ptr::write_volatile(
            NEU_USER_RECORD.0.get(),
            Some(UserRecord {
                frame,
                cr2,
                handler_rsp,
                root,
                kernel_cs: u64::from(current_cs),
            }),
        );
    }
    VALID.store(valid, Ordering::Relaxed);
    serial::emergency(format_args!(
        "[NEU] User boundary: kind={} vector={} error={:#x} RIP={:#x} CR2={:#x}\n[NEU] User context: CS={:#x} SS={:#x} RSP={:#x} RFLAGS={:#x} RAX={} kernel_CS={:#x} handler_rsp={:#x} CR3={:#x}\n[NEU] User boundary verified: {}\n",
        expected.kind,
        frame.vector,
        frame.error,
        frame.rip,
        cr2,
        frame.cs,
        frame.ss,
        frame.rsp,
        frame.flags,
        frame.gpr[14],
        current_cs,
        handler_rsp,
        root,
        valid
    ));
    // SAFETY: ACTIVE was set only by run(), with a live assembly continuation.
    // Restore its original CR3/stack/callee-saved registers, never the user frame.
    unsafe { neu_resume_kernel() }
}

#[cfg(not(feature = "native-test"))]
pub fn run(paging: &mut Paging<'_, '_>) -> Result<()> {
    let kind = if cfg!(feature = "test-user-kernel") {
        1
    } else if cfg!(feature = "test-user-readonly") {
        2
    } else if cfg!(feature = "test-user-nx") {
        3
    } else if cfg!(feature = "test-user-unmapped") {
        4
    } else if cfg!(feature = "test-user-privileged") {
        5
    } else if cfg!(feature = "test-user-gate") {
        6
    } else if cfg!(feature = "test-user-guard") {
        7
    } else {
        0
    };
    let length =
        ptr::addr_of!(neu_user_payload_end) as usize - ptr::addr_of!(neu_user_payload) as usize;
    if length == 0 || length > PAGE as usize {
        return Err(Error::Capacity);
    }
    // SAFETY: Linker-owned immutable bytes in R/NX image storage, known bound.
    let bytes = unsafe { core::slice::from_raw_parts(ptr::addr_of!(neu_user_payload), length) };
    if kind == 0 {
        let mut failures = 0;
        for budget in 0..64 {
            match paging.prepare_user(bytes, kind, Some(budget)) {
                Err(Error::Exhausted) => failures += 1,
                Ok(mut space) => {
                    paging.destroy_user(&mut space)?;
                    break;
                }
                Err(error) => return Err(error),
            }
        }
        if failures == 0 || failures == 64 {
            return Err(Error::Corrupt);
        }
        serial::write(format_args!(
            "[NEU] User construction rollback verified: failpoints={}\n",
            failures
        ))
        .map_err(|_| Error::Corrupt)?;
    }
    let mut space = paging.prepare_user(bytes, kind, None)?;
    exceptions::configure_user_test(true)?;
    // SAFETY: No user execution or boundary can run until the subsequent IRETQ.
    unsafe {
        ptr::write(CONTRACT.0.get(), contract(kind, space.root));
    }
    VALID.store(false, Ordering::Relaxed);
    ACTIVE.store(true, Ordering::Relaxed);
    serial::write(format_args!(
        "[NEU] User space ready: kind={} root={:#x} kernel={:#x} entry={:#x} rsp={:#x}\n",
        kind,
        space.root,
        paging.root(),
        CODE,
        USER_RSP
    ))
    .map_err(|_| Error::Corrupt)?;
    paging.activate_user(&space)?;
    // SAFETY: Verified private root, initialized fixture, exact CS/SS/RIP/RSP,
    // RSP0 and test gate. Assembly suspends this kernel stack and returns once.
    unsafe {
        neu_enter_user(CODE, USER_RSP, paging.root());
    }
    exceptions::configure_user_test(false)?;
    paging.deactivate_user(&space)?;
    let valid = VALID.load(Ordering::Relaxed) && paging.user_result(&space)? == 42;
    paging.destroy_user(&mut space)?;
    serial::write(format_args!(
        "[NEU] User teardown verified: kernel root restored, frames and tables restored\n"
    ))
    .map_err(|_| Error::Corrupt)?;
    if !valid {
        return Err(Error::Corrupt);
    }
    serial::write(format_args!(
        "[NEU] User execution test passed: kind={} result=42\n",
        kind
    ))
    .map_err(|_| Error::Corrupt)?;
    Ok(())
}

#[cfg(all(feature = "native-test", not(feature = "switch-test")))]
pub fn enter_native(root: u64) {
    unsafe extern "C" {
        fn neu_enter_native(rip: u64, rsp: u64, kernel_root: u64);
    }
    // SAFETY: Native run validated MSRs, private root, RSP0 and fixture lifetime.
    unsafe {
        neu_enter_native(CODE, USER_RSP, root);
    }
}
#[cfg(all(feature = "native-test", not(feature = "switch-test")))]
pub fn resume_native() -> ! {
    // SAFETY: Called only by the active native session; it owns this continuation.
    unsafe { neu_resume_kernel() }
}

#[cfg(feature = "switch-test")]
pub fn enter_context(frame: &neu_kernel::syscall::Frame, root: u64, top: u64) {
    unsafe extern "C" {
        fn neu_enter_context(frame: *const neu_kernel::syscall::Frame, root: u64, top: u64);
    }
    // SAFETY: Caller validated the copied frame, selected root and stack. The
    // common normal image stack remains mapped in both roots and holds this borrow.
    unsafe {
        neu_enter_context(frame, root, top);
    }
}
#[cfg(feature = "switch-test")]
pub fn resume_context() -> ! {
    unsafe extern "C" {
        fn neu_resume_context() -> !;
    }
    // SAFETY: Active slice owns the single live common-stack continuation.
    unsafe { neu_resume_context() }
}

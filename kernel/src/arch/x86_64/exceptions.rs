//! Fatal Ring 0 exception foundation. Intel SDM vol. 3, sections 6.13–6.14:
//! 64-bit hardware frames contain RIP/CS/RFLAGS/RSP/SS, then an optional error
//! code. Assembly normalizes that prefix and saves every general-purpose register.

use super::{halt, serial};
use core::{
    arch::{asm, global_asm},
    cell::UnsafeCell,
    mem, ptr,
    sync::atomic::{AtomicBool, Ordering},
};

global_asm!(include_str!("exceptions.S"));

struct Table<T>(UnsafeCell<T>);
// SAFETY: One BSP initializes descriptor tables once with IF=0. Thereafter the
// CPU reads them (and marks the TSS busy); only isolated fault-test hooks mutate
// a gate. Fault records are written once before fatal halt; no other CPU exists.
unsafe impl<T> Sync for Table<T> {}

#[repr(C, packed)]
struct Descriptor {
    limit: u16,
    base: u64,
}

#[repr(C, packed)]
struct Tss {
    reserved0: u32,
    rsp: [u64; 3],
    reserved1: u64,
    ist: [u64; 7],
    reserved2: u64,
    reserved3: u16,
    iomap: u16,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Gate {
    low: u16,
    selector: u16,
    ist: u8,
    attributes: u8,
    mid: u16,
    high: u32,
    reserved: u32,
}
impl Gate {
    const EMPTY: Self = Self {
        low: 0,
        selector: 0,
        ist: 0,
        attributes: 0,
        mid: 0,
        high: 0,
        reserved: 0,
    };
    fn new(address: u64, ist: u8) -> Self {
        Self {
            low: address as u16,
            selector: 8,
            ist,
            attributes: 0x8e,
            mid: (address >> 16) as u16,
            high: (address >> 32) as u32,
            reserved: 0,
        }
    }
}

const _: () = assert!(mem::size_of::<Tss>() == 104);
const _: () = assert!(mem::offset_of!(Tss, ist) == 36);
const _: () = assert!(mem::size_of::<Gate>() == 16);
const _: () = assert!(mem::size_of::<Descriptor>() == 10);
static TSS: Table<Tss> = Table(UnsafeCell::new(Tss {
    reserved0: 0,
    rsp: [0; 3],
    reserved1: 0,
    ist: [0; 7],
    reserved2: 0,
    reserved3: 0,
    iomap: 104,
}));
const GDT_ENTRIES: usize = if cfg!(feature = "user-test") { 8 } else { 5 };
static GDT: Table<[u64; GDT_ENTRIES]> = Table(UnsafeCell::new([0; GDT_ENTRIES]));
static IDT: Table<[Gate; 256]> = Table(UnsafeCell::new([Gate::EMPTY; 256]));
static INITIALIZED: AtomicBool = AtomicBool::new(false);
static FAULTING: AtomicBool = AtomicBool::new(false);

unsafe extern "C" {
    static neu_isr_table: [u64; 32];
    fn neu_isr_255();
    fn neu_load_gdt(descriptor: *const Descriptor);
    static __stack_top: u8;
    static __df_stack_bottom: u8;
    static __df_stack_top: u8;
    fn neu_test_ud() -> !;
    fn neu_test_gp() -> !;
    fn neu_test_bp() -> !;
    fn neu_test_unmapped(address: u64) -> u64;
    fn neu_test_readonly(address: u64);
    fn neu_test_nx(address: u64);
    fn neu_test_guard(address: u64) -> u64;
    #[cfg(feature = "test-page-fault")]
    fn neu_test_pf(address: *const u64) -> !;
}

pub fn initialize() -> Result<(), &'static str> {
    if INITIALIZED.swap(true, Ordering::Relaxed) {
        return Err("descriptor tables initialized twice");
    }
    let df_top = ptr::addr_of!(__df_stack_top) as u64;
    if df_top - ptr::addr_of!(__df_stack_bottom) as u64 != 32 * 1024 || df_top & 15 != 0 {
        return Err("invalid double-fault stack");
    }
    // SAFETY: Static kernel-owned storage is mapped RW and never reclaimed.
    // BSP/IF=0 grants exclusive initialization. Packed TSS fields use unaligned
    // writes; descriptors remain live after GDTR/IDTR/TR retain their addresses.
    unsafe {
        ptr::write_unaligned(
            ptr::addr_of_mut!((*TSS.0.get()).rsp).cast::<u64>(),
            ptr::addr_of!(__stack_top) as u64,
        );
        ptr::write_unaligned(ptr::addr_of_mut!((*TSS.0.get()).ist).cast::<u64>(), df_top);
        let base = TSS.0.get() as u64;
        let gdt = GDT.0.get().cast::<u64>();
        ptr::write(gdt.add(1), 0x00af_9a00_0000_ffff); // 64-bit kernel code.
        ptr::write(gdt.add(2), 0x00cf_9200_0000_ffff); // Kernel data.
        ptr::write(
            gdt.add(3),
            103 | ((base & 0xff_ffff) << 16) | (0x89 << 40) | (((base >> 24) & 0xff) << 56),
        );
        ptr::write(gdt.add(4), base >> 32);
        #[cfg(feature = "user-test")]
        {
            // Preserve selector 0x28 as an invalid descriptor for the old #GP
            // test. User data=0x33, long-mode code=0x3b (DPL/RPL=3).
            ptr::write(gdt.add(6), 0x00cf_f300_0000_ffff);
            ptr::write(gdt.add(7), 0x00af_fb00_0000_ffff);
        }
        let descriptor = Descriptor {
            limit: (mem::size_of::<[u64; GDT_ENTRIES]>() - 1) as u16,
            base: gdt as u64,
        };
        neu_load_gdt(&descriptor);
        let idt = IDT.0.get().cast::<Gate>();
        for vector in 0..256 {
            let address = if vector < 32 {
                ptr::read(ptr::addr_of!(neu_isr_table).cast::<u64>().add(vector))
            } else {
                neu_isr_255 as *const () as u64
            };
            ptr::write(idt.add(vector), Gate::new(address, u8::from(vector == 8)));
        }
        let descriptor = Descriptor {
            limit: (mem::size_of::<[Gate; 256]>() - 1) as u16,
            base: idt as u64,
        };
        asm!("lidt [{}]", in(reg) &descriptor, options(readonly, nostack, preserves_flags));
    }
    Ok(())
}

#[repr(C)]
#[derive(Clone, Copy)]
pub(super) struct ExceptionFrame {
    // Assembly order: r15..r8, rdi,rsi,rbp,rdx,rcx,rbx,rax.
    pub(super) gpr: [u64; 15],
    pub(super) vector: u64,
    pub(super) error: u64,
    pub(super) rip: u64,
    pub(super) cs: u64,
    pub(super) flags: u64,
    pub(super) rsp: u64,
    pub(super) ss: u64,
}
const _: () = assert!(mem::size_of::<ExceptionFrame>() == 176);
const _: () = assert!(mem::offset_of!(ExceptionFrame, vector) == 120);
const _: () = assert!(mem::offset_of!(ExceptionFrame, rip) == 136);
#[repr(C)]
#[derive(Clone, Copy)]
struct FaultRecord {
    frame: ExceptionFrame,
    cr2: u64,
    handler_rsp: u64,
}
const _: () = assert!(mem::size_of::<FaultRecord>() == 192);
#[unsafe(no_mangle)]
static NEU_LAST_FAULT: Table<FaultRecord> = Table(UnsafeCell::new(FaultRecord {
    frame: ExceptionFrame {
        gpr: [0; 15],
        vector: 0,
        error: 0,
        rip: 0,
        cs: 0,
        flags: 0,
        rsp: 0,
        ss: 0,
    },
    cr2: 0,
    handler_rsp: 0,
}));

#[unsafe(no_mangle)]
extern "C" fn neu_exception(frame: *const ExceptionFrame, cr2: u64) -> ! {
    if FAULTING.swap(true, Ordering::Relaxed) {
        serial::emergency(format_args!(
            "[NEU] EXCEPTION: nested fatal fault; halting\n"
        ));
        halt();
    }
    let handler_rsp: u64;
    // SAFETY: Only assembly entry passes this complete normalized kernel-stack
    // frame. Original user RSP/RIP are copied as numbers, never dereferenced.
    let frame = unsafe {
        asm!("mov {}, rsp", out(reg) handler_rsp, options(nomem, nostack, preserves_flags));
        ptr::read(frame)
    };
    #[cfg(feature = "user-test")]
    if (frame.cs & 3 == 3 && !matches!(frame.vector, 2 | 8 | 18)) || frame.vector == 128 {
        // The complete trusted frame is copied. User termination is a separate
        // one-shot path; retain the early nested guard for privileged faults.
        FAULTING.store(false, Ordering::Relaxed);
        #[cfg(feature = "native-test")]
        if super::native::running() {
            super::native::user_fault(frame, cr2, handler_rsp);
        }
        super::userspace::boundary(frame, cr2, handler_rsp);
    }
    let ist = (ptr::addr_of!(__df_stack_bottom) as u64..ptr::addr_of!(__df_stack_top) as u64)
        .contains(&handler_rsp);
    // SAFETY: The fault guard permits one writer; this static survives halt.
    unsafe {
        ptr::write_volatile(
            NEU_LAST_FAULT.0.get(),
            FaultRecord {
                frame,
                cr2,
                handler_rsp,
            },
        );
    }
    serial::emergency(format_args!(
        "[NEU] EXCEPTION: vector={} error={:#x} RIP={:#x} CR2={:#x}\n[NEU] EXCEPTION: CS={:#x} RFLAGS={:#x} RSP={:#x} handler_rsp={:#x} IST={}\n[NEU] EXCEPTION: fatal privileged fault; halting\n",
        frame.vector,
        frame.error,
        frame.rip,
        cr2,
        frame.cs,
        frame.flags,
        frame.rsp,
        handler_rsp,
        u8::from(ist)
    ));
    halt()
}

pub fn test_invalid_opcode() -> ! {
    // SAFETY: Intentional #UD after IDT initialization, in an isolated image.
    unsafe { neu_test_ud() }
}
pub fn test_general_protection() -> ! {
    // SAFETY: Selector 0x28 lies beyond our five-entry GDT; produces #GP(0x28).
    unsafe { neu_test_gp() }
}
pub fn test_breakpoint() -> ! {
    // SAFETY: INT3 exercises a no-error-code trap; our #BP policy is fatal.
    unsafe { neu_test_bp() }
}
pub fn test_double_fault() -> ! {
    // SAFETY: Isolated test with IF=0. A null #GP gate selector makes delivery
    // of the subsequent genuine #GP fault raise another contributory exception.
    // The CPU then delivers #DF through intact IST1. This is NOT software INT 8.
    unsafe {
        ptr::write(ptr::addr_of_mut!((*IDT.0.get())[13].selector), 0);
        neu_test_gp()
    }
}

#[cfg(feature = "test-page-fault")]
#[unsafe(no_mangle)]
static NEU_PF_PROBE: u64 = 0x4e45_5550_4654_4553;

#[cfg(feature = "test-page-fault")]
pub fn page_fault_target() -> u64 {
    ptr::addr_of!(NEU_PF_PROBE) as u64
}

#[cfg(feature = "test-page-fault")]
pub fn test_page_fault() -> ! {
    if serial::write(format_args!(
        "[NEU] Test: read-only mapping verified for #PF\n"
    ))
    .is_err()
    {
        halt();
    }
    // SAFETY: Caller inspected the current owned hierarchy and proved this
    // static is present/read-only; CR0.WP was validated before the transition.
    unsafe { neu_test_pf(ptr::addr_of!(NEU_PF_PROBE)) }
}

pub fn test_paging_access(kind: usize, address: u64) {
    // SAFETY: Called only after the manager verifies a deliberately absent,
    // read-only, or NX mapping in a separate test image. If protection silently
    // fails, these bounded probes return and the caller reports a test failure.
    unsafe {
        match kind {
            0 => {
                let _ = neu_test_unmapped(address);
            }
            1 => neu_test_readonly(address),
            2 => neu_test_nx(address),
            _ => {
                let _ = neu_test_guard(address);
            }
        }
    }
}

#[cfg(feature = "user-test")]
pub fn configure_user_test(enabled: bool) -> neu_kernel::paging::Result<()> {
    use neu_kernel::paging::Error;
    unsafe extern "C" {
        fn neu_user_trap();
    }
    let (bottom, top) = super::userspace::kernel_stack();
    if !INITIALIZED.load(Ordering::Relaxed) || top - bottom != 32 * 1024 || top & 15 != 0 {
        return Err(Error::Corrupt);
    }
    // SAFETY: One BSP, IF=0, no active user execution during configuration.
    // RSP0 uses a separate stack so the suspended Rust continuation stays intact.
    // I/O bitmap starts beyond the TSS limit, denying all CPL3 port access.
    unsafe {
        ptr::write_unaligned(
            ptr::addr_of_mut!((*TSS.0.get()).rsp).cast::<u64>(),
            if enabled {
                top
            } else {
                ptr::addr_of!(__stack_top) as u64
            },
        );
        let mut gate = Gate::new(
            if enabled {
                neu_user_trap as *const () as u64
            } else {
                neu_isr_255 as *const () as u64
            },
            0,
        );
        if enabled {
            gate.attributes = 0xee;
        }
        ptr::write(IDT.0.get().cast::<Gate>().add(128), gate);
    }
    Ok(())
}

#[cfg(feature = "native-test")]
pub fn configure_native(enabled: bool) -> neu_kernel::paging::Result<()> {
    configure_user_test(false)?; // the Phase 2A completion gate stays DPL=0 here
    if enabled {
        let (_, top) = super::userspace::kernel_stack();
        let (bottom, emergency) = super::native::emergency_stack();
        if emergency - bottom != 16384 || emergency & 15 != 0 {
            return Err(neu_kernel::paging::Error::Corrupt);
        }
        // SAFETY: Single CPU/IF=0 before native execution. IST2 is separate from
        // #DF's IST1 and remains image-reserved. NMI/#MC paths are terminal.
        unsafe {
            ptr::write_unaligned(ptr::addr_of_mut!((*TSS.0.get()).rsp).cast::<u64>(), top);
            ptr::write_unaligned(
                ptr::addr_of_mut!((*TSS.0.get()).ist).cast::<u64>().add(1),
                emergency,
            );
            // Publish the valid TSS stack pointer before an NMI can observe
            // IST2 in its gate. x86 orders stores; this also orders the compiler.
            core::sync::atomic::compiler_fence(Ordering::SeqCst);
            for vector in [2, 18] {
                ptr::write_volatile(ptr::addr_of_mut!((*IDT.0.get())[vector].ist), 2);
            }
        }
    }
    Ok(())
}

#[cfg(any(feature = "timer-test", feature = "preempt-test"))]
pub fn configure_interrupts() -> Result<(), &'static str> {
    unsafe extern "C" {
        static neu_irq_table: [u64; 224];
    }
    if !INITIALIZED.load(Ordering::Relaxed) || super::interrupts::flags() & 0x200 != 0 {
        return Err("interrupt IDT requires initialized descriptors and IF=0");
    }
    // SAFETY: One BSP, IF=0. No exception/IST gate changes; all IRQ gates remain
    // present DPL0 interrupt gates with IST0, using the proven normal stack.
    unsafe {
        for vector in 32..256 {
            let address = ptr::read(ptr::addr_of!(neu_irq_table).cast::<u64>().add(vector - 32));
            let gate = Gate::new(address, 0);
            ptr::write(IDT.0.get().cast::<Gate>().add(vector), gate);
            let actual = ptr::read(IDT.0.get().cast::<Gate>().add(vector));
            if actual.low != gate.low
                || actual.mid != gate.mid
                || actual.high != gate.high
                || actual.selector != 8
                || actual.attributes != 0x8e
                || actual.ist != 0
            {
                return Err("interrupt IDT readback failed");
            }
        }
    }
    Ok(())
}

#[cfg(feature = "switch-test")]
pub fn configure_thread_stack(
    stack: neu_kernel::context::StackBinding,
) -> neu_kernel::paging::Result<()> {
    let flags: u64;
    // SAFETY: Read-only flags at the non-preemptible dispatch boundary.
    unsafe {
        asm!("pushfq", "pop {}", out(reg) flags, options(preserves_flags));
    }
    if !INITIALIZED.load(Ordering::Relaxed)
        || flags & 0x200 != 0
        || stack.top - stack.bottom != 8192
        || !stack.top.is_multiple_of(4096)
        || stack.bottom < neu_kernel::paging::KSTACK_BASE
        || stack.top > neu_kernel::paging::KSTACK_BASE + 2 * neu_kernel::paging::KSTACK_STRIDE
    {
        return Err(neu_kernel::paging::Error::Flags);
    }
    // SAFETY: Caller verified private supervisor stack mappings under the next
    // root. IF=0, no user context can observe a partially published selection.
    unsafe {
        ptr::write_unaligned(
            ptr::addr_of_mut!((*TSS.0.get()).rsp).cast::<u64>(),
            stack.top,
        );
    }
    if rsp0() != stack.top {
        return Err(neu_kernel::paging::Error::Corrupt);
    }
    Ok(())
}
#[cfg(feature = "switch-test")]
pub fn rsp0() -> u64 {
    // SAFETY: Initialized packed TSS, read at IF=0; copy without an aligned borrow.
    unsafe { ptr::read_unaligned(ptr::addr_of!((*TSS.0.get()).rsp).cast::<u64>()) }
}

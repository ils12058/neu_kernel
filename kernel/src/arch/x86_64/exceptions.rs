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
static GDT: Table<[u64; 5]> = Table(UnsafeCell::new([0; 5]));
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
        let descriptor = Descriptor {
            limit: (mem::size_of::<[u64; 5]>() - 1) as u16,
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
struct ExceptionFrame {
    // Assembly order: r15..r8, rdi,rsi,rbp,rdx,rcx,rbx,rax.
    gpr: [u64; 15],
    vector: u64,
    error: u64,
    rip: u64,
    cs: u64,
    flags: u64,
    rsp: u64,
    ss: u64,
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
    // SAFETY: Assembly passed a complete, aligned normalized frame on the
    // current normal/IST stack. All handlers are fatal, so no return ABI or
    // potentially invalid original stack is used. CR2 was captured in assembly.
    let frame = unsafe {
        asm!("mov {}, rsp", out(reg) handler_rsp, options(nomem, nostack, preserves_flags));
        ptr::read(frame)
    };
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

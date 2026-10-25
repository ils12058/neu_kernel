#![no_std]
#![no_main]
#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
compile_error!("Neu kernel must be built for x86_64-unknown-none");
const _: () = assert!(
    (cfg!(feature = "test-panic") as u8
        + cfg!(feature = "test-missing-memory-map") as u8
        + cfg!(feature = "test-memory") as u8
        + cfg!(feature = "test-invalid-opcode") as u8
        + cfg!(feature = "test-general-protection") as u8
        + cfg!(feature = "test-page-fault") as u8
        + cfg!(feature = "test-double-fault") as u8
        + cfg!(feature = "test-breakpoint") as u8)
        <= 1,
    "build one fault-injection image at a time"
);

#[path = "arch/x86_64/mod.rs"]
mod arch;
mod boot;
use neu_kernel::memory;

use core::{
    cell::UnsafeCell,
    panic::PanicInfo,
    sync::atomic::{AtomicBool, Ordering},
};

const BITMAP_WORDS: usize = 16384; // 128 KiB; at most 1,048,576 candidate frames.
struct Bitmap(UnsafeCell<[u64; BITMAP_WORDS]>);
// SAFETY: Taken once by the BSP with IF=0; the returned mutable slice is unique.
unsafe impl Sync for Bitmap {}
static BITMAP: Bitmap = Bitmap(UnsafeCell::new([0; BITMAP_WORDS]));
static BITMAP_TAKEN: AtomicBool = AtomicBool::new(false);

fn frames<'a>(map: &'a memory::MemoryMap<'a>) -> Result<memory::FrameAllocator<'a>, &'static str> {
    if BITMAP_TAKEN.swap(true, Ordering::Relaxed) {
        return Err("frame allocator initialized twice");
    }
    let pointer = BITMAP.0.get().cast::<u64>();
    let (start, end) = boot::kernel_virtual_range();
    if (pointer as usize) < start
        || (pointer as usize)
            .checked_add(BITMAP_WORDS * 8)
            .is_none_or(|limit| limit > end)
    {
        return Err("frame bitmap is outside the kernel reservation");
    }
    // SAFETY: This array belongs to the explicitly reserved kernel image. One
    // successful take grants exclusive access; no other references are created.
    let bitmap = unsafe { core::slice::from_raw_parts_mut(pointer, BITMAP_WORDS) };
    memory::FrameAllocator::new(map, bitmap).map_err(|_| "frame metadata capacity exceeded")
}

macro_rules! log {
    ($($arg:tt)*) => {
        if arch::serial::write(format_args!($($arg)*)).is_err() {
            arch::halt();
        }
    };
}

fn fail(reason: &str) -> ! {
    arch::serial::emergency(format_args!("[NEU] BOOT FAILURE: {reason}\n"));
    arch::halt()
}

#[unsafe(no_mangle)]
extern "C" fn neu_main() -> ! {
    if let Err(reason) = arch::serial::initialize() {
        fail(reason);
    }
    log!("[NEU] Neu Kernel\n");
    log!("[NEU] Architecture: x86_64\n");
    log!("[NEU] Early serial initialized\n");

    if let Err(reason) = arch::validate_cpu_and_stack() {
        fail(reason);
    }
    log!("[NEU] CPU and kernel stack validated\n");
    if let Err(reason) = arch::exceptions::initialize() {
        fail(reason);
    }
    log!("[NEU] Kernel GDT/TSS/IDT initialized; double-fault IST ready\n");
    let info = match boot::initialize() {
        Ok(info) => info,
        Err(reason) => fail(reason),
    };
    log!("[NEU] Boot protocol initialized: Limine base 6, UEFI64\n");
    log!(
        "[NEU] Memory map validated: {} regions, {} usable bytes\n",
        info.memory_regions,
        info.usable_bytes
    );
    log!(
        "[NEU] Boot memory map copied: {} descriptors in kernel storage\n",
        info.memory_regions
    );
    let mut allocator = match frames(&info.memory_map) {
        Ok(value) => value,
        Err(reason) => fail(reason),
    };
    let stats = allocator.stats();
    log!(
        "[NEU] Physical frame allocator initialized: {} free, {} allocated; loader RAM retained\n",
        stats.free,
        stats.allocated
    );
    #[cfg(feature = "test-memory")]
    {
        if let Err(reason) = memory::self_test::run(&info.memory_map, &mut allocator) {
            fail(reason);
        }
        log!(
            "[NEU] Memory self-test passed: ownership, unique allocation, reuse, invalid frees, sparse exhaustion, accounting\n"
        );
    }
    // Keep the real allocator exclusively owned and alive through bootstrap.
    let _ = &mut allocator;

    if cfg!(feature = "test-panic") {
        panic!("controlled Phase 0 test panic");
    }
    if cfg!(feature = "test-invalid-opcode") {
        arch::exceptions::test_invalid_opcode();
    }
    if cfg!(feature = "test-general-protection") {
        arch::exceptions::test_general_protection();
    }
    if cfg!(feature = "test-breakpoint") {
        arch::exceptions::test_breakpoint();
    }
    if cfg!(feature = "test-double-fault") {
        log!("[NEU] Test: forcing #DF via #GP delivery failure\n");
        arch::exceptions::test_double_fault();
    }
    #[cfg(feature = "test-page-fault")]
    {
        let hhdm = match boot::test_hhdm_offset() {
            Ok(value) => value,
            Err(reason) => fail(reason),
        };
        if let Err(reason) = arch::exceptions::test_page_fault(&info.memory_map, hhdm) {
            fail(reason);
        }
    }

    log!("[NEU] Kernel bootstrap successful\n");
    log!("[NEU] Idle: interrupts disabled; halting\n");
    arch::halt()
}

static PANICKING: AtomicBool = AtomicBool::new(false);

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    if PANICKING.swap(true, Ordering::Relaxed) {
        arch::serial::emergency(format_args!("[NEU] PANIC: recursive panic; halting\n"));
        arch::halt();
    }
    arch::serial::emergency(format_args!("[NEU] PANIC: {info}\n"));
    arch::halt()
}

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
        + cfg!(feature = "test-breakpoint") as u8
        + cfg!(feature = "test-paging") as u8
        + cfg!(feature = "test-paging-bootstrap-failure") as u8
        + cfg!(feature = "test-unmapped") as u8
        + cfg!(feature = "test-readonly") as u8
        + cfg!(feature = "test-nx") as u8
        + cfg!(feature = "test-guard-low") as u8
        + cfg!(feature = "test-guard-high") as u8
        + cfg!(feature = "test-df-guard-low") as u8
        + cfg!(feature = "test-df-guard-high") as u8
        + cfg!(feature = "test-user") as u8
        + cfg!(feature = "test-user-kernel") as u8
        + cfg!(feature = "test-user-readonly") as u8
        + cfg!(feature = "test-user-nx") as u8
        + cfg!(feature = "test-user-unmapped") as u8
        + cfg!(feature = "test-user-privileged") as u8
        + cfg!(feature = "test-user-gate") as u8
        + cfg!(feature = "test-user-guard") as u8
        + cfg!(feature = "test-native") as u8
        + cfg!(feature = "test-native-unknown") as u8
        + cfg!(feature = "test-native-exit-invalid") as u8
        + cfg!(feature = "test-native-return-rip") as u8
        + cfg!(feature = "test-native-return-rsp") as u8
        + cfg!(feature = "test-native-return-flags") as u8
        + cfg!(feature = "test-native-return-selectors") as u8
        + cfg!(feature = "test-native-disabled") as u8
        + cfg!(feature = "test-nodes") as u8
        + cfg!(feature = "test-nodes-a-fault") as u8
        + cfg!(feature = "test-nodes-b-fault") as u8
        + cfg!(feature = "test-nodes-cross") as u8
        + cfg!(feature = "test-nodes-reclaim") as u8
        + cfg!(feature = "test-nodes-rollback") as u8
        + cfg!(feature = "test-timer") as u8
        + cfg!(feature = "test-timer-xapic") as u8
        + cfg!(feature = "test-timer-repeat") as u8
        + cfg!(feature = "test-timer-spurious") as u8
        + cfg!(feature = "test-timer-unsupported") as u8
        + cfg!(feature = "test-timer-unexpected") as u8
        + cfg!(feature = "test-timer-software") as u8
        + cfg!(feature = "test-switch") as u8
        + cfg!(feature = "test-switch-a-fault") as u8
        + cfg!(feature = "test-switch-b-fault") as u8
        + cfg!(feature = "test-switch-cross") as u8
        + cfg!(feature = "test-switch-stack-user") as u8
        + cfg!(feature = "test-switch-stack-guard") as u8
        + cfg!(feature = "test-switch-invalid-context") as u8
        + cfg!(feature = "test-switch-rollback") as u8
        + cfg!(feature = "test-switch-stale") as u8
        + cfg!(feature = "test-preempt") as u8
        + cfg!(feature = "test-preempt-xapic") as u8
        + cfg!(feature = "test-preempt-a-fault") as u8
        + cfg!(feature = "test-preempt-b-fault") as u8
        + cfg!(feature = "test-preempt-cross") as u8
        + cfg!(feature = "test-preempt-native") as u8
        + cfg!(feature = "test-preempt-invalid-context") as u8
        + cfg!(feature = "test-preempt-stack-mismatch") as u8
        + cfg!(feature = "test-preempt-handoff") as u8
        + cfg!(feature = "test-preempt-stale") as u8
        + cfg!(feature = "test-preempt-unsafe") as u8
        + cfg!(feature = "test-preempt-unexpected") as u8
        + cfg!(feature = "test-preempt-software") as u8
        + cfg!(feature = "test-preempt-origin") as u8
        + cfg!(feature = "test-preempt-fpu") as u8
        + cfg!(feature = "test-threads") as u8
        + cfg!(feature = "test-threads-xapic") as u8
        + cfg!(feature = "test-threads-a1-fault") as u8
        + cfg!(feature = "test-threads-a2-fault") as u8
        + cfg!(feature = "test-threads-b-fault") as u8
        + cfg!(feature = "test-threads-cross") as u8
        + cfg!(feature = "test-threads-a-cross") as u8
        + cfg!(feature = "test-threads-rollback") as u8
        + cfg!(feature = "test-threads-stale") as u8
        + cfg!(feature = "test-threads-stack") as u8
        + cfg!(feature = "test-threads-user-guard") as u8
        + cfg!(feature = "test-threads-return-stack") as u8
        + cfg!(feature = "test-capabilities") as u8
        + cfg!(feature = "test-capabilities-xapic") as u8
        + cfg!(feature = "test-capabilities-a-fault") as u8
        + cfg!(feature = "test-capabilities-a2-fault") as u8
        + cfg!(feature = "test-capabilities-b-fault") as u8
        + cfg!(feature = "test-capabilities-rollback") as u8
        + cfg!(feature = "test-native-cap-disabled") as u8
        + cfg!(feature = "test-address-space") as u8
        + cfg!(feature = "test-address-space-xapic") as u8
        + cfg!(feature = "test-address-space-a-fault") as u8
        + cfg!(feature = "test-address-space-a2-fault") as u8
        + cfg!(feature = "test-address-space-b-fault") as u8
        + cfg!(feature = "test-address-space-rollback") as u8
        + cfg!(feature = "test-native-abi02-disabled") as u8
        + cfg!(feature = "test-channels") as u8
        + cfg!(feature = "test-channels-xapic") as u8
        + cfg!(feature = "test-channels-a-fault") as u8
        + cfg!(feature = "test-channels-b-fault") as u8
        + cfg!(feature = "test-channels-rollback") as u8
        + cfg!(feature = "test-native-abi03-disabled") as u8
        + cfg!(feature = "test-address-space-channel-disabled") as u8)
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
    let hhdm = info.hhdm_offset;
    if cfg!(feature = "test-paging-bootstrap-failure") {
        let before = allocator.stats();
        let old_root = arch::paging::current_root();
        {
            let attempt = arch::paging::Paging::initialize(&mut allocator, &info.memory_map, hhdm);
            if !matches!(attempt, Err(neu_kernel::paging::Error::Exhausted)) {
                fail("bootstrap allocation failure was not observed");
            }
        }
        if allocator.stats() != before || arch::paging::current_root() != old_root {
            fail("bootstrap rollback changed the active root or accounting");
        }
        log!(
            "[NEU] Paging bootstrap rollback verified: CR3={:#x} accounting restored\n",
            old_root
        );
        arch::halt();
    }
    let mut paging = match arch::paging::Paging::initialize(&mut allocator, &info.memory_map, hhdm)
    {
        Ok(value) => value,
        Err(error) => {
            arch::serial::emergency(format_args!(
                "[NEU] BOOT FAILURE: paging initialization {error:?}\n"
            ));
            arch::halt();
        }
    };
    log!(
        "[NEU] Kernel paging activated: CR3={:#x} old={:#x} tables={}\n",
        paging.root(),
        paging.old_root(),
        paging.table_count()
    );
    log!("[NEU] Kernel mappings verified: RX text, R rodata, RW/NX data, 4 guards\n");
    if cfg!(feature = "test-paging") {
        if let Err(error) = paging.self_test() {
            arch::serial::emergency(format_args!(
                "[NEU] BOOT FAILURE: paging self-test {error:?}\n"
            ));
            arch::halt();
        }
        log!(
            "[NEU] Paging self-test passed: aliases, mapping, zero/reuse, rollback, TLB, accounting\n"
        );
    }

    #[cfg(feature = "timer-test")]
    if let Err(reason) = arch::interrupts::run(&mut paging) {
        fail(reason);
    }

    #[cfg(all(feature = "switch-test", not(feature = "preempt-test")))]
    if let Err(error) = arch::cooperative::run(&mut paging) {
        arch::serial::emergency(format_args!("[NEU] BOOT FAILURE: switching {error:?}\n"));
        arch::halt();
    }
    #[cfg(all(feature = "preempt-test", not(feature = "multi-test")))]
    if let Err(error) = arch::preemptive::run(&mut paging) {
        arch::serial::emergency(format_args!("[NEU] BOOT FAILURE: preemption {error:?}\n"));
        arch::halt();
    }
    #[cfg(feature = "multi-test")]
    if let Err(error) = arch::multithread::run(&mut paging) {
        arch::serial::emergency(format_args!(
            "[NEU] BOOT FAILURE: multiple Threads {error:?}\n"
        ));
        arch::halt();
    }
    #[cfg(all(feature = "node-test", not(feature = "switch-test")))]
    if let Err(error) = arch::nodes::run(&mut paging) {
        arch::serial::emergency(format_args!("[NEU] BOOT FAILURE: Nodes {error:?}\n"));
        arch::halt();
    }
    #[cfg(all(feature = "native-test", not(feature = "node-test")))]
    if let Err(error) = arch::native::run(&mut paging) {
        arch::serial::emergency(format_args!(
            "[NEU] BOOT FAILURE: native execution {error:?}\n"
        ));
        arch::halt();
    }
    #[cfg(all(feature = "user-test", not(feature = "native-test")))]
    if let Err(error) = arch::userspace::run(&mut paging) {
        arch::serial::emergency(format_args!(
            "[NEU] BOOT FAILURE: user execution {error:?}\n"
        ));
        arch::halt();
    }

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
    if cfg!(feature = "test-page-fault") {
        match paging.translate(arch::exceptions::page_fault_target()) {
            Ok(Some(mapping)) if !mapping.permissions.write && !mapping.user => {
                arch::exceptions::test_page_fault()
            }
            _ => fail("PF test target is not verified read-only"),
        }
    }
    for (enabled, kind) in [
        (cfg!(feature = "test-unmapped"), 0),
        (cfg!(feature = "test-readonly"), 1),
        (cfg!(feature = "test-nx"), 2),
        (cfg!(feature = "test-guard-low"), 3),
        (cfg!(feature = "test-guard-high"), 4),
        (cfg!(feature = "test-df-guard-low"), 5),
        (cfg!(feature = "test-df-guard-high"), 6),
    ] {
        if enabled {
            let address = match paging.prepare_fault(kind) {
                Ok(address) => address,
                Err(error) => {
                    arch::serial::emergency(format_args!(
                        "[NEU] BOOT FAILURE: fault setup {error:?}\n"
                    ));
                    arch::halt();
                }
            };
            log!(
                "[NEU] Paging fault prepared: kind={} address={:#x}\n",
                kind,
                address
            );
            arch::exceptions::test_paging_access(kind, address);
            fail("injected paging access unexpectedly succeeded");
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

#![no_std]
#![no_main]
#![deny(unsafe_op_in_unsafe_fn)]

#[cfg(not(all(target_arch = "x86_64", target_os = "none")))]
compile_error!("Neu Phase 0 must be built for x86_64-unknown-none");
#[cfg(all(feature = "test-panic", feature = "test-missing-memory-map"))]
compile_error!("build each fault-injection image separately");

#[path = "arch/x86_64/mod.rs"]
mod arch;
mod boot;

use core::{
    panic::PanicInfo,
    sync::atomic::{AtomicBool, Ordering},
};

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

    if cfg!(feature = "test-panic") {
        panic!("controlled Phase 0 test panic");
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

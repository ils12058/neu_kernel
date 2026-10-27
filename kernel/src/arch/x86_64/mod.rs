//! Early x86-64 mechanisms. External interrupts require an explicit bounded
//! kernel timer window or the published user-only preemption contract.

use core::arch::{asm, global_asm, x86_64::__cpuid};

#[cfg(feature = "switch-test")]
pub mod cooperative;
pub mod exceptions;
#[cfg(any(feature = "timer-test", feature = "preempt-test"))]
pub mod interrupts;
#[cfg(feature = "multi-test")]
pub mod multithread;
#[cfg(feature = "native-test")]
pub mod native;
#[cfg(all(feature = "node-test", not(feature = "switch-test")))]
pub mod nodes;
#[cfg_attr(feature = "memory-object-test", allow(dead_code))]
pub mod paging;
#[cfg(feature = "preempt-test")]
pub mod preemptive;
pub mod serial;
#[cfg(feature = "user-test")]
#[cfg_attr(feature = "memory-object-test", allow(dead_code))]
pub mod userspace;

global_asm!(include_str!("entry.S"));

unsafe extern "C" {
    static __stack_bottom: u8;
    static __stack_top: u8;
    fn neu_halt_begin() -> !;
}

pub fn halt() -> ! {
    // SAFETY: Ring 0 entry is guaranteed by the loader. This assembly routine
    // disables interrupts and repeatedly halts; it cannot return to its caller.
    unsafe { neu_halt_begin() }
}

pub fn validate_cpu_and_stack() -> Result<(), &'static str> {
    let (cr0, cr3, cr4, flags, stack): (u64, u64, u64, u64, usize);
    let (efer_low, efer_high): (u32, u32);
    // SAFETY: Called only after x86-64 Ring 0 entry. These operations read CPU
    // state without modifying mappings, interrupt state, or the current stack.
    unsafe {
        asm!("mov {}, cr0", out(reg) cr0, options(nomem, nostack, preserves_flags));
        asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags));
        asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack, preserves_flags));
        asm!("pushfq", "pop {}", out(reg) flags, options(preserves_flags));
        asm!("mov {}, rsp", out(reg) stack, options(nomem, nostack, preserves_flags));
        asm!("rdmsr", in("ecx") 0xc000_0080u32,
            out("eax") efer_low, out("edx") efer_high,
            options(nomem, nostack, preserves_flags));
    }
    let efer = (u64::from(efer_high) << 32) | u64::from(efer_low);
    // CPUID is available on x86-64. Check the maximum extended leaf before
    // querying the extended feature bits.
    let extended = __cpuid(0x8000_0000);
    if extended.eax < 0x8000_0001 || __cpuid(0x8000_0001).edx & (1 << 20) == 0 {
        return Err("CPU requires NX/XD support");
    }
    if cr0 & ((1 << 0) | (1 << 16) | (1 << 31)) != ((1 << 0) | (1 << 16) | (1 << 31))
        || cr4 & (1 << 5) == 0
        || efer & ((1 << 8) | (1 << 10) | (1 << 11)) != ((1 << 8) | (1 << 10) | (1 << 11))
        || cr3 & 0x000f_ffff_ffff_f000 == 0
    {
        return Err("invalid long-mode, paging, write-protect, or NX state");
    }
    if cr4 & (1 << 12) != 0 || flags & ((1 << 9) | (1 << 10)) != 0 {
        return Err("Phase 0 requires four-level paging, IF=0, DF=0");
    }
    let bottom = core::ptr::addr_of!(__stack_bottom) as usize;
    let top = core::ptr::addr_of!(__stack_top) as usize;
    if !(bottom..top).contains(&stack) || top - bottom != 64 * 1024 || top & 15 != 0 {
        return Err("execution is outside the aligned kernel-owned stack");
    }
    Ok(())
}

/// Validates the numeric shape of a loader-supplied, higher-half pointer.
/// Mapping validity still relies on the selected Limine handoff contract.
pub fn valid_boot_range(address: usize, length: usize, alignment: usize) -> bool {
    address >= 0xffff_8000_0000_0000
        && address.is_multiple_of(alignment)
        && length != 0
        && address.checked_add(length - 1).is_some()
}

#[cfg(feature = "cap-test")]
pub mod capabilities;

#[cfg(feature = "abi-02")]
pub mod address_space;

#[cfg(feature = "channel-test")]
pub mod ipc;

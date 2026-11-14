//! Phase 9C-1 static two-CPU Ring 3 execution controller.
//!
//! Placement is fixed before either CPU leaves Ring 0: logical CPU 0 runs
//! Node A / Thread A1 / space A and logical CPU 1 runs Node B / Thread B1 /
//! space B. The BSP owns every allocating subsystem and publishes one
//! validated execution descriptor per CPU. The AP only consumes its own
//! descriptor, executes at CPL3, publishes raw exit evidence in its CPU-local
//! block and parks. No AP path reaches `Nodes`, `Paging`, the paging registry,
//! the capability registry or the serial device.

use core::{
    arch::{asm, global_asm},
    cell::UnsafeCell,
    ptr,
    sync::atomic::{AtomicU64, Ordering},
};

use neu_kernel::{
    context::{SavedContext, StackBinding},
    cpu::{self, Bootstrap},
    node::{ActiveContext, ExitReason, Fault, FaultSource, Nodes, ThreadContext},
    paging::{
        PAGE, SpaceId, USER_BARRIER, USER_CODE, USER_CONSTANTS, USER_DATA, USER_RSP, USER_STACK,
    },
    smp_user as model,
};

use super::{
    exceptions::{self, ExceptionFrame},
    paging::{self, Paging, UserSpace},
    serial, smp,
};

global_asm!(include_str!("smp_user.S"));

const FIELDS: usize = 96;
const STRIDE: usize = FIELDS * 8;
const MAGIC: u64 = 0x4e45_555f_5553_5231; // "NEU_USR1"

const PHASE_PREPARED: u64 = model::Phase::Prepared as u64;
const PHASE_RELEASED: u64 = model::Phase::Released as u64;
const PHASE_ENTERED: u64 = model::Phase::Entered as u64;
const PHASE_EXITED: u64 = model::Phase::Exited as u64;
const PHASE_COMPLETED: u64 = model::Phase::Completed as u64;
const PHASE_ACKNOWLEDGED: u64 = model::Phase::Acknowledged as u64;
const PHASE_REAPED: u64 = model::Phase::Reaped as u64;
/// Declared outside the model: a canceled placement never enters Ring 3 and
/// therefore has no `Session` phase at all.
const PHASE_CANCELLED: u64 = 8;
const PHASE_FAILED: u64 = 9;

const EXIT_RETURN: u64 = 1;
const EXIT_FAULT: u64 = 2;

#[allow(dead_code)]
pub mod field {
    pub const MAGIC: usize = 0;
    pub const LOGICAL: usize = 1;
    pub const APIC_ID: usize = 2;
    pub const PHASE: usize = 3;
    pub const ENTRY_COUNT: usize = 4;
    pub const EXIT_COUNT: usize = 5;
    pub const KERNEL_CR3: usize = 6;
    pub const USER_CR3: usize = 7;
    pub const SPACE_ROOT: usize = 8;
    pub const ENTRY_RIP: usize = 9;
    pub const ENTRY_RSP: usize = 10;
    pub const ENTRY_FLAGS: usize = 11;
    pub const SAVED_RIP: usize = 12;
    pub const SAVED_RSP: usize = 13;
    pub const SAVED_FLAGS: usize = 14;
    pub const SAVED_CS: usize = 15;
    pub const SAVED_SS: usize = 16;
    pub const STACK_BOTTOM: usize = 17;
    pub const STACK_TOP: usize = 18;
    pub const ARENA_BOTTOM: usize = 19;
    pub const ARENA_TOP: usize = 20;
    pub const TSS_BASE: usize = 21;
    pub const TSS_RSP0: usize = 22;
    pub const GDT_BASE: usize = 23;
    pub const IDT_BASE: usize = 24;
    pub const NODE_ID: usize = 25;
    pub const THREAD_ID: usize = 26;
    pub const THREAD_GEN: usize = 27;
    pub const SPACE_ID: usize = 28;
    pub const BARRIER_VA: usize = 29;
    pub const BARRIER_PA: usize = 30;
    pub const BARRIER_A: usize = 31;
    pub const BARRIER_B: usize = 32;
    pub const BARRIER_RELEASE: usize = 33;
    pub const BARRIER_SURVIVOR: usize = 34;
    pub const CODE_VA: usize = 35;
    pub const CODE_PA: usize = 36;
    pub const CONSTANTS_VA: usize = 37;
    pub const CONSTANTS_PA: usize = 38;
    pub const DATA_VA: usize = 39;
    pub const DATA_PA: usize = 40;
    pub const STACK_VA: usize = 41;
    pub const STACK_PA: usize = 42;
    pub const EXPECT_VECTOR: usize = 43;
    pub const EXPECT_ERROR: usize = 44;
    pub const EXPECT_RIP: usize = 45;
    pub const EXPECT_RSP: usize = 46;
    pub const EXPECT_CR2: usize = 47;
    pub const EXPECT_AX: usize = 48;
    pub const EXPECT_BX: usize = 49;
    pub const EXPECT_R8: usize = 50;
    pub const EXPECT_R9: usize = 51;
    pub const EXIT_KIND: usize = 52;
    pub const EXIT_CODE: usize = 53;
    pub const EXIT_RIP: usize = 54;
    pub const EXIT_RSP: usize = 55;
    pub const EXIT_FLAGS: usize = 56;
    pub const EXIT_CS: usize = 57;
    pub const EXIT_SS: usize = 58;
    pub const FAULT_VECTOR: usize = 59;
    pub const FAULT_ERROR: usize = 60;
    pub const FAULT_RIP: usize = 61;
    pub const FAULT_CR2: usize = 62;
    pub const FAULT_CS: usize = 63;
    pub const FAULT_SS: usize = 64;
    pub const FAULT_RSP: usize = 65;
    pub const HANDLER_RSP: usize = 66;
    /// Root observed by the hardware on the exit frame. It must be the user
    /// root, not the kernel root, or the transition was not the one published.
    pub const EXIT_CR3: usize = 67;
    pub const RESTORED_CR3: usize = 68;
    pub const RESTORED_GS: usize = 69;
    pub const RESTORED_CR0: usize = 70;
    pub const RESTORED_EFER: usize = 71;
    pub const COMPLETE_COUNT: usize = 72;
    pub const ACK_COUNT: usize = 73;
    pub const COMPLETION: usize = 74;
    pub const FAILURE: usize = 75;
    pub const BASELINE_ALLOCATED: usize = 76;
    pub const BASELINE_FREE: usize = 77;
    pub const BASELINE_TABLES: usize = 78;
    pub const FINAL_ALLOCATED: usize = 79;
    pub const FINAL_FREE: usize = 80;
    pub const FINAL_TABLES: usize = 81;
    pub const DATA0: usize = 82;
    pub const DATA1: usize = 83;
    pub const DATA2: usize = 84;
    pub const DATA3: usize = 85;
    pub const DATA4: usize = 86;
    pub const DATA5: usize = 87;
    pub const DATA6: usize = 88;
    pub const DATA7: usize = 89;
    pub const DATA8: usize = 90;
    pub const CHECKS: usize = 91;
    pub const SESSION_BARRIER: usize = 92;
    pub const ROLE: usize = 93;
    pub const RESERVED_A: usize = 94;
    pub const RESERVED_B: usize = 95;
}

const CHECK_PLACEMENT: u64 = 1 << 0;
const CHECK_IDENTITIES: u64 = 1 << 1;
const CHECK_ROOTS: u64 = 1 << 2;
const CHECK_BARRIER: u64 = 1 << 3;
const CHECK_EXIT: u64 = 1 << 4;
const CHECK_HANDLER: u64 = 1 << 5;
const CHECK_RESULT: u64 = 1 << 6;
const CHECK_OVERLAP: u64 = 1 << 7;
const CHECK_COMPLETE: u64 = 1 << 8;
const CHECK_ACK: u64 = 1 << 9;
const CHECK_REAP: u64 = 1 << 10;
const CHECK_TEARDOWN: u64 = 1 << 11;
const CHECK_BASELINE: u64 = 1 << 12;
const CHECK_FAULT: u64 = 1 << 13;
const CHECK_PEER: u64 = 1 << 14;
const CHECK_FATAL_CLEAR: u64 = 1 << 15;
const CHECK_DISARM: u64 = 1 << 16;
const CHECK_ISOLATION: u64 = 1 << 17;
const CHECK_INVALID: u64 = 1 << 18;
const CHECK_PROFILE: u64 = 1 << 19;

// The AP publishes the same documented codes in its kernel CPU block.
const FAILURE_BLOCK: u64 = smp::FAILURE_USER_BLOCK;
const FAILURE_WAIT: u64 = smp::FAILURE_USER_WAIT;
const FAILURE_DESCRIPTOR: u64 = smp::FAILURE_USER_DESCRIPTOR;
const FAILURE_RESUME: u64 = smp::FAILURE_USER_ENTRY;
const FAILURE_EXIT: u64 = smp::FAILURE_USER_EXIT;
const FAILURE_ACK: u64 = smp::FAILURE_USER_WAIT;

/// One bounded wait covers the whole AP user execution, including the shared
/// concurrency window, so a peer that never appears still fails the image.
const USER_WAIT_LIMIT: u64 = 1_000_000_000;

const BARRIER_SURVIVOR: u64 = 0x4e45_555f_5553_5231;

#[repr(C, align(64))]
pub struct Block {
    words: [AtomicU64; FIELDS],
}

impl Block {
    const fn new() -> Self {
        Self {
            words: [const { AtomicU64::new(0) }; FIELDS],
        }
    }

    fn load(&self, field: usize) -> u64 {
        self.words[field].load(Ordering::Acquire)
    }

    fn store(&self, field: usize, value: u64) {
        self.words[field].store(value, Ordering::Release);
    }
}

/// Exported per-CPU user-execution publication. QMP reads the documented words
/// at `neu_smp_user_blocks + logical * neu_smp_user_layout[3]`.
#[unsafe(no_mangle)]
#[allow(non_upper_case_globals)]
pub static neu_smp_user_blocks: [Block; cpu::MAX_CPUS] = [const { Block::new() }; cpu::MAX_CPUS];

#[used]
#[unsafe(no_mangle)]
#[allow(non_upper_case_globals)]
pub static neu_smp_user_layout: [u64; 4] =
    [MAGIC, cpu::MAX_CPUS as u64, FIELDS as u64, STRIDE as u64];

/// One bridge record per CPU: written only by the owning processor.
#[repr(C, align(64))]
struct BridgeCell(UnsafeCell<[u64; 16]>);
// SAFETY: Each CPU writes only its own record; the BSP never aliases another
// processor's bridge, and the record is read back only on the owning CPU.
unsafe impl Sync for BridgeCell {}

#[unsafe(export_name = "neu_smp_user_bridges")]
static BRIDGES: [BridgeCell; cpu::MAX_CPUS] =
    [const { BridgeCell(UnsafeCell::new([0; 16])) }; cpu::MAX_CPUS];

const USER_RIP: usize = 5;
const USER_RSP_SLOT: usize = 6;
const USER_FLAGS: usize = 7;
const USER_CR3: usize = 8;

/// Physical identity of the shared barrier page. The BSP resolves the image
/// virtual address once, before either private root is prepared; the same
/// value is revalidated by every `verify_user` call.
static BARRIER_PHYSICAL: AtomicU64 = AtomicU64::new(0);

unsafe extern "C" {
    static neu_smp_user_arena0: u8;
    static neu_smp_user_arena1: u8;
    static neu_smp_user_guard0_low: u8;
    static neu_smp_user_guard0_high: u8;
    static neu_smp_user_guard1_low: u8;
    static neu_smp_user_guard1_high: u8;
    static neu_smp_user_barrier: u8;
    static neu_smp_user_payload: u8;
    static neu_smp_user_payload_end: u8;
    static neu_smp_user_done_resume: u8;
    static neu_smp_user_fault_site: u8;
    fn neu_smp_user_prepared_dwell();
    fn neu_smp_user_results_dwell();
    fn neu_smp_user_wait(word: *const u64, target: u64, alternate: u64) -> u64;
    fn neu_smp_user_enter(
        rip: u64,
        rsp: u64,
        user_root: u64,
        kernel_root: u64,
        flags: u64,
        bridge: *mut u64,
    );
    fn neu_smp_user_resume(bridge: *mut u64) -> !;
}

pub(super) fn guards() -> [u64; 4] {
    [
        ptr::addr_of!(neu_smp_user_guard0_low) as u64,
        ptr::addr_of!(neu_smp_user_guard0_high) as u64,
        ptr::addr_of!(neu_smp_user_guard1_low) as u64,
        ptr::addr_of!(neu_smp_user_guard1_high) as u64,
    ]
}

/// Returns the barrier page physical address published by the BSP. Every user
/// root preparation and verification compares against this single value.
pub(super) fn barrier_physical() -> u64 {
    BARRIER_PHYSICAL.load(Ordering::Acquire)
}

fn barrier_address() -> u64 {
    ptr::addr_of!(neu_smp_user_barrier) as u64
}

fn arena(cpu: usize) -> (u64, u64) {
    let bottom = match cpu {
        0 => ptr::addr_of!(neu_smp_user_arena0) as u64,
        _ => ptr::addr_of!(neu_smp_user_arena1) as u64,
    };
    (bottom, bottom + 16 * 1024)
}

fn bridge(cpu: usize) -> *mut u64 {
    BRIDGES[cpu].0.get().cast::<u64>()
}

fn site(symbol: *const u8) -> u64 {
    USER_CODE + symbol as u64 - ptr::addr_of!(neu_smp_user_payload) as u64
}

fn fixture() -> Result<&'static [u8], &'static str> {
    let start = ptr::addr_of!(neu_smp_user_payload);
    let end = ptr::addr_of!(neu_smp_user_payload_end);
    let length = end as usize - start as usize;
    if length == 0 || length > PAGE as usize || start as u64 & 15 != 0 {
        return Err("SMP user fixture length or alignment is invalid");
    }
    // SAFETY: Immutable linker-owned bytes with a proven upper bound.
    Ok(unsafe { core::slice::from_raw_parts(start, length) })
}

fn phase_of(cpu: usize) -> *const u64 {
    // SAFETY: `field::PHASE` is inside this CPU's published block.
    unsafe {
        ptr::addr_of!(neu_smp_user_blocks[cpu])
            .cast::<u64>()
            .add(field::PHASE)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Profile {
    Base,
    Overlap,
    Isolation,
    ApFault,
    Completion,
    Invalid,
}

const PROFILE: Profile = if cfg!(feature = "test-smp-user-overlap") {
    Profile::Overlap
} else if cfg!(feature = "test-smp-user-isolation") {
    Profile::Isolation
} else if cfg!(feature = "test-smp-user-ap-fault") {
    Profile::ApFault
} else if cfg!(feature = "test-smp-user-completion") {
    Profile::Completion
} else if cfg!(feature = "test-smp-user-invalid") {
    Profile::Invalid
} else {
    Profile::Base
};

const _: () = assert!(
    (cfg!(feature = "test-smp-user-2") as u8
        + cfg!(feature = "test-smp-user-overlap") as u8
        + cfg!(feature = "test-smp-user-isolation") as u8
        + cfg!(feature = "test-smp-user-ap-fault") as u8
        + cfg!(feature = "test-smp-user-completion") as u8
        + cfg!(feature = "test-smp-user-invalid") as u8)
        == 1,
    "select exactly one Phase 9C-1 user profile"
);

const BSP_SEED: u64 = 0x4e45_5553_0001;
const BSP_KIND: u64 = 0x51;
const AP_SEED: u64 = 0x4e45_5553_0002;
const AP_KIND: u64 = 0x52;

fn profile_checks() -> u64 {
    match PROFILE {
        Profile::Base => {
            CHECK_PLACEMENT
                | CHECK_IDENTITIES
                | CHECK_ROOTS
                | CHECK_BARRIER
                | CHECK_EXIT
                | CHECK_HANDLER
                | CHECK_RESULT
                | CHECK_COMPLETE
                | CHECK_ACK
                | CHECK_REAP
                | CHECK_TEARDOWN
                | CHECK_BASELINE
                | CHECK_PEER
                | CHECK_FATAL_CLEAR
                | CHECK_DISARM
        }
        Profile::Overlap => profile_base_checks() | CHECK_OVERLAP,
        Profile::Isolation => profile_base_checks() | CHECK_ISOLATION,
        Profile::ApFault => profile_base_checks() | CHECK_FAULT,
        Profile::Completion => profile_base_checks() | CHECK_COMPLETE,
        Profile::Invalid => CHECK_INVALID,
    }
}

fn profile_base_checks() -> u64 {
    CHECK_PLACEMENT
        | CHECK_IDENTITIES
        | CHECK_ROOTS
        | CHECK_BARRIER
        | CHECK_EXIT
        | CHECK_HANDLER
        | CHECK_RESULT
        | CHECK_REAP
        | CHECK_TEARDOWN
        | CHECK_BASELINE
        | CHECK_PEER
        | CHECK_FATAL_CLEAR
        | CHECK_DISARM
}

fn note(cpu: usize, check: u64) {
    let block = &neu_smp_user_blocks[cpu];
    let current = block.load(field::CHECKS);
    block.store(field::CHECKS, current | check);
}

fn failed(cpu: usize, code: u64) {
    let block = &neu_smp_user_blocks[cpu];
    block.store(field::FAILURE, code);
    block.store(field::PHASE, PHASE_FAILED);
}

/// Called from `neu_exception` before the fatal-fault guard on every CPL3
/// exit. The CPU re-derives its own logical identity from CPUID because the
/// entry stub cleared GS, validates the frame against the descriptor the BSP
/// published for this CPU, records the raw outcome and resumes its own kernel
/// continuation. It never touches the other CPU's block.
pub(super) fn boundary(frame: ExceptionFrame, cr2: u64, handler_rsp: u64) -> ! {
    let Some(cpu) = smp::logical_by_apic(smp::apic_id()) else {
        // A CPL3 exit without a published CPU identity is not a profile exit.
        super::halt();
    };
    let block = &neu_smp_user_blocks[cpu];
    let mut kernel_cs: u16 = 0;
    // The exit frame was taken while this CPU was on its private user root.
    let exit_root = paging::current_root();
    let valid = block.load(field::MAGIC) == MAGIC
        && block.load(field::LOGICAL) == cpu as u64
        && block.load(field::PHASE) == PHASE_ENTERED
        && {
            // SAFETY: Interrupt/exception entry has already switched to the
            // kernel code segment.
            unsafe {
                asm!("mov {:x}, cs", out(reg) kernel_cs,
                    options(nomem, nostack, preserves_flags));
            }
            kernel_cs == 0x08
        }
        && frame.cs == 0x3b
        && frame.ss == 0x33
        && frame.vector == block.load(field::EXPECT_VECTOR)
        && frame.error == block.load(field::EXPECT_ERROR)
        && frame.rip == block.load(field::EXPECT_RIP)
        && frame.rsp == block.load(field::EXPECT_RSP)
        && (frame.vector != 14 || cr2 == block.load(field::EXPECT_CR2))
        && frame.flags
            & ((1 << 8) | (1 << 9) | (1 << 10) | (3 << 12) | (1 << 14) | (1 << 17) | (1 << 18))
            == 0
        && frame.flags & 2 != 0
        && (block.load(field::ARENA_BOTTOM)..block.load(field::ARENA_TOP)).contains(&handler_rsp)
        && exit_root == block.load(field::USER_CR3)
        && frame.gpr[14] == block.load(field::EXPECT_AX)
        && frame.gpr[13] == block.load(field::EXPECT_BX)
        && frame.gpr[7] == block.load(field::EXPECT_R8)
        && frame.gpr[6] == block.load(field::EXPECT_R9);
    block.store(field::EXIT_COUNT, 1);
    block.store(field::EXIT_RIP, frame.rip);
    block.store(field::EXIT_RSP, frame.rsp);
    block.store(field::EXIT_FLAGS, frame.flags);
    block.store(field::EXIT_CS, frame.cs);
    block.store(field::EXIT_SS, frame.ss);
    block.store(field::HANDLER_RSP, handler_rsp);
    block.store(field::EXIT_CR3, exit_root);
    if frame.vector == 14 {
        block.store(field::EXIT_KIND, EXIT_FAULT);
        block.store(field::FAULT_VECTOR, frame.vector);
        block.store(field::FAULT_ERROR, frame.error);
        block.store(field::FAULT_RIP, frame.rip);
        block.store(field::FAULT_CR2, cr2);
        block.store(field::FAULT_CS, frame.cs);
        block.store(field::FAULT_SS, frame.ss);
        block.store(field::FAULT_RSP, frame.rsp);
    } else {
        block.store(field::EXIT_KIND, EXIT_RETURN);
    }
    block.store(field::EXIT_CODE, frame.vector);
    if valid {
        note(cpu, CHECK_EXIT);
        block.store(field::PHASE, PHASE_EXITED);
    } else {
        failed(cpu, FAILURE_EXIT);
    }
    // SAFETY: This CPU entered Ring 3 through its own bridge record and the
    // trap already left CPL3; the record's continuation is live.
    unsafe { neu_smp_user_resume(bridge(cpu)) }
}

/// AP-side controller. Runs on the AP's private kernel stack with GS naming its
/// own block. It reads only its own descriptor, executes exactly one Ring 3
/// context and parks after the BSP acknowledges the terminal outcome.
#[cfg(feature = "smp-user-test")]
pub(super) fn ap_main(index: usize, apic: u32) -> ! {
    if index != 1 || index >= cpu::MAX_CPUS {
        smp::ap_fail_index(if index < cpu::MAX_CPUS { index } else { 1 }, FAILURE_BLOCK);
    }
    // The BSP publishes this CPU's descriptor after the AP has already reported
    // Ready, so the descriptor wait precedes any identity validation.
    // SAFETY: The block lives in kernel image storage for the whole image.
    let prepared = unsafe { neu_smp_user_wait(phase_of(index), PHASE_PREPARED, u64::MAX) };
    if prepared != 0 {
        smp::ap_fail_index(index, FAILURE_WAIT);
    }
    let block = &neu_smp_user_blocks[index];
    if block.load(field::MAGIC) != MAGIC || block.load(field::LOGICAL) != index as u64 {
        failed(index, FAILURE_BLOCK);
        smp::ap_fail_index(index, FAILURE_BLOCK);
    }
    if block.load(field::APIC_ID) != u64::from(apic) || block.load(field::ROLE) != 1 {
        failed(index, FAILURE_DESCRIPTOR);
        smp::ap_fail_index(index, FAILURE_DESCRIPTOR);
    }
    // SAFETY: Same block, still owned by this CPU.
    let released = unsafe { neu_smp_user_wait(phase_of(index), PHASE_RELEASED, PHASE_CANCELLED) };
    if released == 1 {
        // The BSP canceled the profile: no Ring 3 context exists for this CPU.
        park(index, false);
    }
    if released != 0 {
        smp::ap_fail_index(index, FAILURE_WAIT);
    }
    let kernel_root = paging::current_root();
    // The published ENTRY_RSP is the user entry stack; the field is repurposed
    // below to record the kernel stack this CPU enters from, so capture it first.
    let user_rsp = block.load(field::ENTRY_RSP);
    block.store(field::KERNEL_CR3, kernel_root);
    block.store(field::ENTRY_COUNT, 1);
    block.store(field::ENTRY_RSP, smp::rsp());
    block.store(field::PHASE, PHASE_ENTERED);
    let record = bridge(index);
    // SAFETY: This CPU owns `record`; the descriptor values were validated by
    // the BSP before it published `PHASE_RELEASED`.
    unsafe {
        ptr::write_volatile(record.add(USER_RIP), block.load(field::ENTRY_RIP));
        ptr::write_volatile(record.add(USER_RSP_SLOT), user_rsp);
        ptr::write_volatile(record.add(USER_FLAGS), block.load(field::ENTRY_FLAGS));
        ptr::write_volatile(record.add(USER_CR3), block.load(field::USER_CR3));
        neu_smp_user_enter(
            block.load(field::ENTRY_RIP),
            user_rsp,
            block.load(field::USER_CR3),
            kernel_root,
            block.load(field::ENTRY_FLAGS),
            record,
        );
    }
    let restored_cr3 = paging::current_root();
    let restored_gs = smp::gs_base();
    block.store(field::RESTORED_CR3, restored_cr3);
    block.store(field::RESTORED_GS, restored_gs);
    block.store(field::RESTORED_CR0, smp::cr0());
    block.store(field::RESTORED_EFER, smp::efer());
    if restored_cr3 != kernel_root
        || restored_gs != smp::block_address(index)
        || block.load(field::PHASE) != PHASE_EXITED
        || block.load(field::CHECKS) & CHECK_EXIT == 0
    {
        failed(index, FAILURE_RESUME);
        smp::ap_fail_index(index, FAILURE_RESUME);
    }
    // The BSP publishes ACK before REAPED, so waiting for the terminal phase
    // can never observe a half-published acknowledgement.
    // SAFETY: The phase word lives in this CPU's published block for the whole
    // image; the BSP alone stores the terminal acknowledgement and reaping.
    let reaped = unsafe { neu_smp_user_wait(phase_of(index), PHASE_REAPED, u64::MAX) };
    if reaped != 0 || block.load(field::ACK_COUNT) != 1 {
        failed(index, FAILURE_ACK);
        smp::ap_fail_index(index, FAILURE_ACK);
    }
    park(index, true);
}

/// Publishes the terminal AP park state. Only reached once the BSP confirmed
/// both Threads are terminal, so no root can still be in use by this CPU.
fn park(index: usize, entered: bool) -> ! {
    let cpu_block = &smp::neu_smp_cpus[index];
    cpu_block.store(cpu::block::PARK_RSP, smp::rsp());
    cpu_block.store(cpu::block::PARK_COUNT, 1);
    cpu_block.store(
        cpu::block::ISOLATION,
        if entered {
            cpu::block::ISOLATION_USER_AP
        } else {
            cpu::block::ISOLATION_AP
        },
    );
    if cpu_block.advance(Bootstrap::Parked).is_err() {
        smp::ap_fail_index(index, FAILURE_RESUME);
    }
    smp::silent_park()
}

fn expected_ax(cpu: usize) -> u64 {
    if cpu == 0 {
        BSP_SEED.wrapping_add(BSP_KIND)
    } else if PROFILE == Profile::ApFault {
        // The faulting instruction runs after the role-1 release observation.
        1
    } else {
        AP_SEED.wrapping_add(AP_KIND)
    }
}

fn expected_bx(cpu: usize) -> u64 {
    if cpu == 0 {
        3
    } else if PROFILE == Profile::ApFault {
        AP_KIND
    } else {
        3
    }
}

fn publish_descriptor(
    paging: &mut Paging<'_, '_>,
    cpu: usize,
    active: ActiveContext,
    saved: SavedContext,
    space: &UserSpace,
    baseline: (u64, u64, u64),
) -> Result<(), &'static str> {
    let block = &neu_smp_user_blocks[cpu];
    let kstack = arena(cpu);
    let info = smp::user_cpu(cpu).ok_or("SMP user CPU resources are missing")?;
    let fault_target = paging::text_range().0;
    let faults = PROFILE == Profile::ApFault && cpu == 1;
    let vector = if faults { 14 } else { 128 };
    let error = if faults { 7 } else { 0 };
    let rip = if faults {
        site(ptr::addr_of!(neu_smp_user_fault_site))
    } else {
        site(ptr::addr_of!(neu_smp_user_done_resume))
    };
    let cr2 = if faults { fault_target } else { 0 };
    block.store(field::MAGIC, MAGIC);
    block.store(field::LOGICAL, cpu as u64);
    block.store(field::APIC_ID, u64::from(info.apic_id));
    block.store(field::ROLE, cpu as u64);
    block.store(field::NODE_ID, active.node.raw());
    block.store(field::THREAD_ID, active.thread.raw());
    block.store(field::THREAD_GEN, active.thread.raw() >> 32);
    block.store(field::SPACE_ID, active.context.space.raw());
    block.store(field::SPACE_ROOT, active.context.root);
    block.store(field::USER_CR3, space.root);
    block.store(field::KERNEL_CR3, paging.root());
    block.store(field::ENTRY_RIP, active.context.entry);
    block.store(field::ENTRY_RSP, active.context.rsp);
    block.store(field::ENTRY_FLAGS, 2);
    block.store(field::SAVED_RIP, saved.frame.context.rip);
    block.store(field::SAVED_RSP, saved.frame.context.rsp);
    block.store(field::SAVED_FLAGS, saved.frame.context.flags);
    block.store(field::SAVED_CS, saved.frame.context.cs);
    block.store(field::SAVED_SS, saved.frame.context.ss);
    block.store(field::STACK_BOTTOM, saved.stack.bottom);
    block.store(field::STACK_TOP, saved.stack.top);
    block.store(field::ARENA_BOTTOM, kstack.0);
    block.store(field::ARENA_TOP, kstack.1);
    block.store(field::TSS_BASE, info.tss);
    block.store(field::TSS_RSP0, kstack.1);
    block.store(field::GDT_BASE, info.gdt);
    block.store(field::IDT_BASE, info.idt);
    block.store(field::BARRIER_VA, USER_BARRIER);
    block.store(field::BARRIER_PA, barrier_physical());
    block.store(field::CODE_VA, USER_CODE);
    block.store(field::CONSTANTS_VA, USER_CONSTANTS);
    block.store(field::DATA_VA, USER_DATA);
    block.store(field::STACK_VA, USER_STACK);
    block.store(
        field::CODE_PA,
        paging
            .user_physical(space, USER_CODE)
            .map_err(|_| "SMP user CODE mapping is missing")?,
    );
    block.store(
        field::CONSTANTS_PA,
        paging
            .user_physical(space, USER_CONSTANTS)
            .map_err(|_| "SMP user CONSTANTS mapping is missing")?,
    );
    block.store(
        field::DATA_PA,
        paging
            .user_physical(space, USER_DATA)
            .map_err(|_| "SMP user DATA mapping is missing")?,
    );
    block.store(
        field::STACK_PA,
        paging
            .user_physical(space, USER_STACK)
            .map_err(|_| "SMP user STACK mapping is missing")?,
    );
    block.store(field::EXPECT_VECTOR, vector);
    block.store(field::EXPECT_ERROR, error);
    block.store(field::EXPECT_RIP, rip);
    block.store(field::EXPECT_RSP, USER_RSP);
    block.store(field::EXPECT_CR2, cr2);
    block.store(field::EXPECT_AX, expected_ax(cpu));
    block.store(field::EXPECT_BX, expected_bx(cpu));
    block.store(field::EXPECT_R8, if faults { 1 } else { 0x3b });
    block.store(field::EXPECT_R9, if faults { 1 } else { 0x33 });
    block.store(field::BASELINE_ALLOCATED, baseline.0);
    block.store(field::BASELINE_FREE, baseline.1);
    block.store(field::BASELINE_TABLES, baseline.2);
    block.store(field::PHASE, PHASE_PREPARED);
    Ok(())
}

fn space_seed(cpu: usize) -> (u64, u64) {
    if cpu == 0 {
        (BSP_SEED, BSP_KIND)
    } else {
        (AP_SEED, AP_KIND)
    }
}

fn build_node(nodes: &mut Nodes, space: SpaceId, root: u64) -> Result<ActiveContext, &'static str> {
    let node = nodes
        .create()
        .map_err(|_| "SMP user Node creation failed")?;
    nodes
        .configure(node)
        .map_err(|_| "SMP user Node configuration failed")?;
    nodes
        .bind_space(node, space)
        .map_err(|_| "SMP user Node space binding failed")?;
    let context = ThreadContext {
        space,
        root,
        entry: USER_CODE,
        rsp: USER_RSP,
    };
    let thread = nodes
        .initial_thread(node, context)
        .map_err(|_| "SMP user initial Thread creation failed")?;
    nodes
        .ready(node)
        .map_err(|_| "SMP user Node readiness failed")?;
    Ok(ActiveContext {
        node,
        thread,
        context,
    })
}

/// Phase 9C-1 entry point. Returns only after both Threads are terminal, both
/// private roots are destroyed, the allocator and table inventory match the
/// pre-execution baseline, and both CPUs are back on the owned kernel root.
pub fn run(paging: &mut Paging<'_, '_>) -> Result<(), &'static str> {
    if smp::cpu_count() != 2 || cpu::MAX_CPUS < 2 {
        return Err("Phase 9C-1 requires exactly two processors");
    }
    let kernel_root = paging.root();
    if paging::current_root() != kernel_root {
        return Err("SMP user controller is not on the owned root");
    }
    let stats = paging.stats();
    let baseline = (
        stats.allocated as u64,
        stats.free as u64,
        paging.table_count() as u64,
    );
    // The barrier page is resolved once, before any private root exists, and
    // every preparation and verification compares against this value.
    let barrier_pa = paging
        .translate(barrier_address())
        .map_err(|_| "SMP user barrier translation failed")?
        .ok_or("SMP user barrier is not mapped in the owned root")?
        .physical;
    if barrier_pa == 0 || barrier_pa & (PAGE - 1) != 0 {
        return Err("SMP user barrier physical page is invalid");
    }
    BARRIER_PHYSICAL.store(barrier_pa, Ordering::Release);
    if PROFILE == Profile::Invalid {
        return invalid(paging, baseline);
    }
    let fault_target = paging::text_range().0;
    let bytes = fixture()?;
    let mut spaces = [
        paging
            .prepare_domain_shared(bytes, BSP_KIND, BSP_SEED, [0, 0, fault_target, 0], None)
            .map_err(|_| "SMP user BSP domain preparation failed")?,
        paging
            .prepare_domain_shared(
                bytes,
                AP_KIND,
                AP_SEED,
                [1, u64::from(PROFILE == Profile::ApFault), fault_target, 0],
                None,
            )
            .map_err(|_| "SMP user AP domain preparation failed")?,
    ];
    if spaces[0].root == spaces[1].root {
        return Err("SMP user private roots are not distinct");
    }
    let mut nodes = Nodes::new();
    let mut live = [None; 2];
    let mut saved = [None; 2];
    for (cpu, space) in spaces.iter().enumerate() {
        let active = build_node(&mut nodes, space.id(), space.root)?;
        live[cpu] = Some(active);
        saved[cpu] = Some(SavedContext::initial(active, StackBinding::new(active)));
    }
    let (live, saved) = (live.map(Option::unwrap), saved.map(Option::unwrap));
    if live[0].node == live[1].node
        || live[0].thread == live[1].thread
        || live[0].context.space == live[1].context.space
        || live[0].context.root == live[1].context.root
    {
        return Err("SMP user placement identities are not distinct");
    }
    for cpu in 0..2 {
        nodes
            .install_execution(live[cpu].thread, saved[cpu])
            .map_err(|_| "SMP user execution installation failed")?;
    }
    let mut slots = [live[0], live[1]];
    for cpu in 0..2 {
        slots[cpu] = nodes
            .start_on(cpu, live[cpu].thread)
            .map_err(|_| "SMP user static dispatch failed")?;
    }
    for (cpu, slot) in slots.iter().enumerate() {
        if nodes.active_on(cpu) != Some(*slot) || nodes.active().is_some() {
            return Err("SMP user static assignment is not exclusive");
        }
    }
    let mut session = model::Session::new();
    for cpu in 0..2 {
        session
            .place(cpu, slots[cpu], saved[cpu], arena(cpu))
            .map_err(|_| "SMP user placement rejected")?;
    }
    session
        .bind_barrier(barrier_pa)
        .map_err(|_| "SMP user barrier binding rejected")?;
    if !session.ready() {
        return Err("SMP user session is not ready to release");
    }
    for cpu in 0..2 {
        note(cpu, CHECK_PLACEMENT | CHECK_IDENTITIES);
    }
    for cpu in 0..2 {
        publish_descriptor(paging, cpu, slots[cpu], saved[cpu], &spaces[cpu], baseline)?;
    }
    // Both handler stacks are published before any CPU can take a CPL3 exit.
    exceptions::arm_smp_user(arena(0).1)?;
    let ap = smp::user_cpu(1).ok_or("SMP user AP resources are missing")?;
    // SAFETY: The AP is spinning on its phase word at CPL0; its private TSS is
    // mapped and cannot be used for a privilege transition before release.
    unsafe {
        exceptions::write_tss_rsp0(ap.tss, arena(1).1)?;
    }
    serial::write(format_args!(
        "[NEU] SMP user prepared: root0={:#x} root1={:#x} thread0={} thread1={} barrier={:#x}\n",
        spaces[0].root,
        spaces[1].root,
        slots[0].thread.raw(),
        slots[1].thread.raw(),
        barrier_pa
    ))
    .map_err(|_| "SMP user serial publication failed")?;
    // Bounded, exported observation window: both CPUs are still at CPL0 and no
    // Root 3 entry has happened. QMP freezes the VM here.
    // SAFETY: Image code with no side effects other than the bounded loop.
    unsafe { neu_smp_user_prepared_dwell() };
    for cpu in 0..2 {
        session
            .release(cpu)
            .map_err(|_| "SMP user release rejected")?;
    }
    for block in neu_smp_user_blocks.iter().take(2) {
        block.store(field::PHASE, PHASE_RELEASED);
    }
    for cpu in 0..2 {
        smp::neu_smp_cpus[cpu].store(
            cpu::block::ISOLATION,
            if cpu == 0 {
                cpu::block::ISOLATION_USER_BSP
            } else {
                cpu::block::ISOLATION_USER_AP
            },
        );
    }
    serial::write(format_args!(
        "[NEU] SMP user released: kernel={:#x} code={:#x} rsp={:#x}\n",
        kernel_root, USER_CODE, USER_RSP
    ))
    .map_err(|_| "SMP user release publication failed")?;
    // BSP Ring 3 entry. The AP enters from its own wait loop as soon as it
    // observes the same release publication.
    session
        .enter(0)
        .map_err(|_| "SMP user BSP entry rejected")?;
    neu_smp_user_blocks[0].store(field::ENTRY_COUNT, 1);
    neu_smp_user_blocks[0].store(field::ENTRY_RSP, smp::rsp());
    neu_smp_user_blocks[0].store(field::KERNEL_CR3, kernel_root);
    neu_smp_user_blocks[0].store(field::PHASE, PHASE_ENTERED);
    let record = bridge(0);
    // SAFETY: This CPU owns `record`; both roots, the barrier alias, the entry
    // arena and the RSP0 publication were verified before the release.
    unsafe {
        ptr::write_volatile(record.add(USER_RIP), slots[0].context.entry);
        ptr::write_volatile(record.add(USER_RSP_SLOT), slots[0].context.rsp);
        ptr::write_volatile(record.add(USER_FLAGS), 2);
        ptr::write_volatile(record.add(USER_CR3), spaces[0].root);
        neu_smp_user_enter(
            slots[0].context.entry,
            slots[0].context.rsp,
            spaces[0].root,
            kernel_root,
            2,
            record,
        );
    }
    let restored_cr3 = paging::current_root();
    let restored_gs = smp::gs_base();
    neu_smp_user_blocks[0].store(field::RESTORED_CR3, restored_cr3);
    neu_smp_user_blocks[0].store(field::RESTORED_GS, restored_gs);
    neu_smp_user_blocks[0].store(field::RESTORED_CR0, smp::cr0());
    neu_smp_user_blocks[0].store(field::RESTORED_EFER, smp::efer());
    if restored_cr3 != kernel_root || restored_gs != smp::block_address(0) {
        failed(0, FAILURE_RESUME);
        return Err("SMP user BSP did not restore its kernel root or GS identity");
    }
    if neu_smp_user_blocks[0].load(field::PHASE) != PHASE_EXITED {
        return Err("SMP user BSP exit was not validated");
    }
    // The AP completes its own exit independently; the BSP only observes.
    let mut spins = 0u64;
    loop {
        let phase = neu_smp_user_blocks[1].load(field::PHASE);
        if phase == PHASE_EXITED {
            break;
        }
        if phase == PHASE_FAILED {
            return Err("SMP user AP exit validation failed");
        }
        if spins >= USER_WAIT_LIMIT {
            return Err("SMP user AP exit was not observed within the bounded window");
        }
        core::hint::spin_loop();
        spins += 1;
    }
    // The AP reached EXITED only through its own published ENTERED transition,
    // so the BSP records that observed entry before accepting the terminal exit.
    session
        .enter(1)
        .map_err(|_| "SMP user AP entry transition rejected")?;
    // The barrier word is shared memory, not a counter: both CPUs had to
    // publish their stage-A bit while the other was still executing.
    let barrier = barrier_words();
    for block in neu_smp_user_blocks.iter().take(2) {
        block.store(field::BARRIER_A, barrier[0]);
        block.store(field::BARRIER_B, barrier[1]);
        block.store(field::BARRIER_RELEASE, barrier[2]);
        block.store(field::BARRIER_SURVIVOR, barrier[3]);
    }
    let exit0 = recorded_exit(0)?;
    let exit1 = recorded_exit(1)?;
    if exit0.0 != EXIT_RETURN || exit0.1 == 0 {
        return Err("SMP user BSP did not publish a normal Ring 3 exit");
    }
    if PROFILE == Profile::ApFault {
        let fault_site = site(ptr::addr_of!(neu_smp_user_fault_site));
        if exit1.0 != EXIT_FAULT
            || exit1.1 != 14
            || exit1.2 != fault_site
            || exit1.3 != fault_target
            || neu_smp_user_blocks[1].load(field::FAULT_ERROR) != 7
        {
            return Err("SMP user AP fault evidence is not the expected one");
        }
        // Session-wide fact: the AP's contained fault was validated by the BSP.
        for cpu in 0..2 {
            note(cpu, CHECK_FAULT);
        }
    } else if exit1.0 != EXIT_RETURN {
        return Err("SMP user AP did not publish a normal Ring 3 exit");
    }
    // Both CPUs resumed to Ring 0 and both raw exits were accepted, so each
    // Thread's peer survived this session whether it returned or faulted.
    for cpu in 0..2 {
        note(cpu, CHECK_PEER);
    }
    session
        .exit(
            0,
            model::Exit {
                kind: model::ExitKind::Return,
                code: exit0.1,
            },
        )
        .map_err(|_| "SMP user BSP exit transition rejected")?;
    session
        .exit(
            1,
            model::Exit {
                kind: if exit1.0 == EXIT_FAULT {
                    model::ExitKind::Fault
                } else {
                    model::ExitKind::Return
                },
                code: exit1.1,
            },
        )
        .map_err(|_| "SMP user AP exit transition rejected")?;
    session
        .observe_barrier(barrier[0], 3)
        .map_err(|_| "SMP user barrier word does not prove concurrency")?;
    let observed = session.barrier_word();
    for block in neu_smp_user_blocks.iter().take(2) {
        block.store(field::SESSION_BARRIER, observed);
    }
    for (cpu, block) in neu_smp_user_blocks.iter().enumerate().take(2) {
        if !session.handler_in_stack(cpu, block.load(field::HANDLER_RSP)) {
            return Err("SMP user exit did not use its own entry arena");
        }
        note(cpu, CHECK_BARRIER | CHECK_HANDLER);
    }
    let words = [
        paging
            .user_data_words(&spaces[0])
            .map_err(|_| "SMP user BSP DATA frame is unreadable")?,
        paging
            .user_data_words(&spaces[1])
            .map_err(|_| "SMP user AP DATA frame is unreadable")?,
    ];
    for cpu in 0..2 {
        let block = &neu_smp_user_blocks[cpu];
        for (index, value) in words[cpu].iter().copied().enumerate() {
            block.store(field::DATA0 + index, value);
        }
        let (seed, kind) = space_seed(cpu);
        if words[cpu][0] != seed.wrapping_add(kind) || words[cpu][1] != 3 || words[cpu][2] != 3 {
            return Err("SMP user payload result is not the expected one");
        }
        // Role 0 publishes the survivor word into the shared page and its own
        // DATA; role 1 only observes the release. A swapped or shared frame
        // would make these two rows indistinguishable.
        if cpu == 0 {
            if words[0][3] != BARRIER_SURVIVOR || words[0][4] != 0 || words[0][5] != 0x5a5a_a5a5 {
                return Err("SMP user BSP role publication is not the expected one");
            }
        } else if words[1][3] != 0
            || words[1][4] != 1
            || (PROFILE != Profile::ApFault && words[1][5] != 0x5a5a_a5a5)
        {
            return Err("SMP user AP role publication is not the expected one");
        }
        note(cpu, CHECK_RESULT);
    }
    // The shared page kept the survivor word written by role 0 while role 1
    // was executing in the other address space.
    if barrier[3] != BARRIER_SURVIVOR {
        return Err("SMP user shared barrier lost the survivor publication");
    }
    if PROFILE == Profile::Overlap {
        let enter = words[0][7].max(words[1][7]);
        let exit = words[0][8].min(words[1][8]);
        if enter == 0 || exit == 0 || enter >= exit {
            return Err("SMP user execution intervals did not overlap");
        }
        for cpu in 0..2 {
            note(cpu, CHECK_OVERLAP);
        }
    }
    if PROFILE == Profile::Isolation {
        // Both private roots must own every user frame they map: no user page
        // is shared, and each root re-verifies its own W^X/NX/guard layout.
        let mut physical = [[0u64; 4]; 2];
        for (cpu, space) in spaces.iter().enumerate() {
            for (index, address) in [USER_CODE, USER_CONSTANTS, USER_DATA, USER_STACK]
                .into_iter()
                .enumerate()
            {
                physical[cpu][index] = paging
                    .user_physical(space, address)
                    .map_err(|_| "SMP user isolation translation failed")?;
            }
        }
        for first in 0..4 {
            for second in 0..4 {
                if physical[0][first] == physical[1][second] {
                    return Err("SMP user address spaces share a physical frame");
                }
            }
        }
        if spaces[0].root == spaces[1].root {
            return Err("SMP user address spaces share a root");
        }
        paging
            .verify_user(&spaces[0])
            .map_err(|_| "SMP user BSP root isolation check failed")?;
        paging
            .verify_user(&spaces[1])
            .map_err(|_| "SMP user AP root isolation check failed")?;
        for cpu in 0..2 {
            note(cpu, CHECK_ISOLATION);
        }
    }
    if paging.verify_user(&spaces[0]).is_err() || paging.verify_user(&spaces[1]).is_err() {
        return Err("SMP user private root no longer verifies after execution");
    }
    for cpu in 0..2 {
        note(cpu, CHECK_ROOTS);
    }
    serial::write(format_args!(
        "[NEU] SMP user exits: kernel={:#x} bsp={:#x} ap={:#x}\n",
        kernel_root,
        neu_smp_user_blocks[0].load(field::EXIT_RIP),
        neu_smp_user_blocks[1].load(field::EXIT_RIP),
    ))
    .map_err(|_| "SMP user exit publication failed")?;
    // SAFETY: The BSP owns this bounded dwell; both Threads are terminal and
    // every user frame is still mapped by a verified private root.
    unsafe { neu_smp_user_results_dwell() };
    // Completion and acknowledgement are exactly-once and BSP-driven.
    session
        .complete(0)
        .map_err(|_| "SMP user BSP completion rejected")?;
    session
        .complete(1)
        .map_err(|_| "SMP user AP completion rejected")?;
    for block in neu_smp_user_blocks.iter().take(2) {
        block.store(field::COMPLETE_COUNT, 1);
        block.store(field::COMPLETION, 1);
        block.store(field::PHASE, PHASE_COMPLETED);
    }
    let outcome1 = if PROFILE == Profile::ApFault {
        ExitReason::UserFault(Fault {
            source: FaultSource::Cpu,
            vector: 14,
            error: 7,
            rip: neu_smp_user_blocks[1].load(field::FAULT_RIP),
            rsp: USER_RSP,
            cs: 0x3b,
            ss: 0x33,
            flags: neu_smp_user_blocks[1].load(field::EXIT_FLAGS),
            cr2: fault_target,
        })
    } else {
        ExitReason::Normal(128)
    };
    nodes
        .complete_on(0, slots[0], ExitReason::Normal(128))
        .map_err(|_| "SMP user BSP Thread completion rejected")?;
    nodes
        .complete_on(1, slots[1], outcome1)
        .map_err(|_| "SMP user AP Thread completion rejected")?;
    if nodes.active_on(0).is_some() || nodes.active_on(1).is_some() {
        return Err("SMP user completed Thread still owns a CPU slot");
    }
    if nodes
        .complete_on(0, slots[0], ExitReason::Normal(128))
        .is_ok()
    {
        return Err("SMP user completion was accepted twice");
    }
    session
        .acknowledge(0)
        .map_err(|_| "SMP user BSP acknowledgement rejected")?;
    session
        .acknowledge(1)
        .map_err(|_| "SMP user AP acknowledgement rejected")?;
    for (cpu, block) in neu_smp_user_blocks.iter().enumerate().take(2) {
        block.store(field::ACK_COUNT, 1);
        block.store(field::PHASE, PHASE_ACKNOWLEDGED);
        note(cpu, CHECK_COMPLETE | CHECK_ACK);
    }
    if PROFILE == Profile::Completion {
        // Exactly-once publication against the live objects: none of these may
        // be accepted a second time once both Threads are terminal.
        if session.complete(0).is_ok()
            || session.complete(1).is_ok()
            || session.acknowledge(0).is_ok()
            || session.acknowledge(1).is_ok()
            || nodes
                .complete_on(0, slots[0], ExitReason::Normal(128))
                .is_ok()
            || nodes
                .complete_on(1, slots[1], ExitReason::Normal(128))
                .is_ok()
        {
            return Err("SMP user completion was accepted twice");
        }
        for cpu in 0..2 {
            note(cpu, CHECK_PROFILE);
        }
    }
    session.reap(0).map_err(|_| "SMP user BSP reap rejected")?;
    session.reap(1).map_err(|_| "SMP user AP reap rejected")?;
    if !session.reaped() {
        return Err("SMP user reclamation precondition was not reached");
    }
    for (cpu, block) in neu_smp_user_blocks.iter().enumerate().take(2) {
        block.store(field::PHASE, PHASE_REAPED);
        note(cpu, CHECK_REAP);
    }
    // The AP must be parked before any root is destroyed, and its test-only
    // Ring 3 doorway is retired at the same boundary: the halted CPU cannot
    // observe the table write and can never be released again.
    let mut spins = 0u64;
    loop {
        let ap_block = &smp::neu_smp_cpus[1];
        if ap_block.load(cpu::block::PARK_COUNT) == 1 && ap_block.load(cpu::block::STATE) == 4 {
            break;
        }
        if ap_block.load(cpu::block::STATE) == Bootstrap::Failed.code() {
            return Err("SMP user AP failed before parking");
        }
        if spins >= USER_WAIT_LIMIT {
            return Err("SMP user AP did not park within the bounded window");
        }
        core::hint::spin_loop();
        spins += 1;
    }
    let ap = smp::user_cpu(1).ok_or("SMP user AP resources are missing")?;
    exceptions::disarm_smp_user_cpu(
        ap.idt,
        ap.tss,
        smp::neu_smp_cpus[1].load(cpu::block::STACK_TOP),
    )?;
    for slot in &slots {
        let thread = slot.thread;
        let stack = StackBinding::new(*slot);
        nodes
            .release_stack(thread, stack)
            .map_err(|_| "SMP user Thread stack release rejected")?;
    }
    for slot in &slots {
        nodes
            .release_space(slot.node, slot.context.space)
            .map_err(|_| "SMP user Node space release rejected")?;
        nodes
            .reclaim(slot.node)
            .map_err(|_| "SMP user Node reclamation rejected")?;
    }
    if nodes.start_on(0, slots[0].thread).is_ok() || nodes.start_on(1, slots[1].thread).is_ok() {
        return Err("SMP user terminal Thread was still dispatchable");
    }
    for space in &mut spaces {
        paging
            .destroy_user(space)
            .map_err(|_| "SMP user private root destruction failed")?;
    }
    let final_stats = paging.stats();
    let final_tables = paging.table_count() as u64;
    if (
        final_stats.allocated as u64,
        final_stats.free as u64,
        final_tables,
    ) != baseline
        || paging::current_root() != kernel_root
    {
        return Err("SMP user teardown did not restore the frame and table baseline");
    }
    for (cpu, block) in neu_smp_user_blocks.iter().enumerate().take(2) {
        block.store(field::FINAL_ALLOCATED, final_stats.allocated as u64);
        block.store(field::FINAL_FREE, final_stats.free as u64);
        block.store(field::FINAL_TABLES, final_tables);
        note(cpu, CHECK_TEARDOWN | CHECK_BASELINE);
    }
    if !exceptions::fatal_record_clear() {
        return Err("SMP user containment reached the fatal fault record");
    }
    exceptions::disarm_smp_user()?;
    for cpu in 0..2 {
        note(cpu, CHECK_FATAL_CLEAR | CHECK_DISARM | CHECK_PROFILE);
    }
    let required = profile_checks();
    for block in neu_smp_user_blocks.iter().take(2) {
        if block.load(field::CHECKS) & required != required {
            return Err("SMP user profile evidence is incomplete");
        }
    }
    let bsp_block = &smp::neu_smp_cpus[0];
    bsp_block.store(cpu::block::ISOLATION, cpu::block::ISOLATION_USER_BSP);
    if bsp_block.advance(Bootstrap::Parked).is_err() {
        return Err("SMP user BSP park transition failed");
    }
    serial::write(format_args!(
        "[NEU] SMP user complete: checks={:#x} profile={:?}\n",
        neu_smp_user_blocks[0].load(field::CHECKS) & neu_smp_user_blocks[1].load(field::CHECKS),
        PROFILE
    ))
    .map_err(|_| "SMP user completion publication failed")?;
    Ok(())
}

fn barrier_words() -> [u64; 4] {
    // SAFETY: The barrier page is image-owned supervisor RW in the owned root.
    unsafe {
        core::array::from_fn(|index| {
            ptr::read_volatile((barrier_address() as *const u64).add(index))
        })
    }
}

/// Returns `(kind, code, rip, cr2)` from a CPU's recorded raw exit.
fn recorded_exit(cpu: usize) -> Result<(u64, u64, u64, u64), &'static str> {
    let block = &neu_smp_user_blocks[cpu];
    if block.load(field::EXIT_COUNT) != 1 || block.load(field::CHECKS) & CHECK_EXIT == 0 {
        return Err("SMP user exit evidence is incomplete");
    }
    Ok((
        block.load(field::EXIT_KIND),
        block.load(field::EXIT_CODE),
        block.load(field::EXIT_RIP),
        block.load(field::FAULT_CR2),
    ))
}

/// Rejection-only profile: proves the placement, identity, generation and
/// release rules against the production model without entering Ring 3.
fn invalid(paging: &mut Paging<'_, '_>, baseline: (u64, u64, u64)) -> Result<(), &'static str> {
    let barrier_pa = barrier_physical();
    if barrier_pa == 0 {
        return Err("SMP user invalid profile has no published barrier");
    }
    for (cpu, block) in neu_smp_user_blocks.iter().enumerate().take(2) {
        let info = smp::user_cpu(cpu).ok_or("SMP user invalid CPU resources are missing")?;
        // Identity words are published before the phase word that releases the
        // AP's first wait.
        block.store(field::MAGIC, MAGIC);
        block.store(field::LOGICAL, cpu as u64);
        block.store(field::APIC_ID, u64::from(info.apic_id));
        block.store(field::ROLE, cpu as u64);
        block.store(field::BASELINE_ALLOCATED, baseline.0);
        block.store(field::BASELINE_FREE, baseline.1);
        block.store(field::BASELINE_TABLES, baseline.2);
        block.store(field::PHASE, PHASE_PREPARED);
    }
    let bytes = fixture()?;
    let fault_target = paging::text_range().0;
    let mut space = paging
        .prepare_domain_shared(bytes, BSP_KIND, BSP_SEED, [0, 0, fault_target, 0], None)
        .map_err(|_| "SMP user invalid domain preparation failed")?;
    let mut nodes = Nodes::new();
    let live = build_node(&mut nodes, space.id(), space.root)?;
    let saved = SavedContext::initial(live, StackBinding::new(live));
    let mut checks = 0u64;
    let mut session = model::Session::new();
    if session.place(0, live, saved, arena(0)).is_ok()
        && session.place(1, live, saved, arena(1)).is_err()
        && session.place(0, live, saved, arena(0)).is_err()
    {
        checks |= 1 << 0; // duplicate Thread/Node/space and duplicate CPU slot
    }
    let mut forged = saved;
    forged.fresh = false;
    if session.place(1, live, forged, arena(1)).is_err() {
        checks |= 1 << 1; // stale/forged saved context
    }
    let mut kernel = live;
    kernel.context.space = neu_kernel::paging::SpaceId::KERNEL;
    if session.place(1, kernel, saved, arena(1)).is_err() {
        checks |= 1 << 2; // kernel address space
    }
    if session.bind_barrier(barrier_pa).is_ok() {
        checks |= 1 << 3; // the real page-aligned barrier is accepted once
    }
    if session.bind_barrier(0x9008).is_err() {
        checks |= 1 << 4; // unaligned barrier rejected
    }
    if session.bind_barrier(barrier_pa).is_err() {
        checks |= 1 << 5; // second binding is rejected
    }
    if session.release(0).is_err() {
        checks |= 1 << 6; // one-CPU release is rejected
    }
    if session.ready() {
        return Err("SMP user invalid session reported readiness");
    }
    if session.place(1, live, saved, arena(1)).is_err() {
        checks |= 1 << 7; // second CPU cannot repeat the same Thread
    }
    nodes
        .install_execution(live.thread, saved)
        .map_err(|_| "SMP user invalid execution installation failed")?;
    let slot = nodes
        .start_on(0, live.thread)
        .map_err(|_| "SMP user invalid dispatch failed")?;
    nodes
        .complete_on(0, slot, ExitReason::Normal(128))
        .map_err(|_| "SMP user invalid completion failed")?;
    nodes
        .release_stack(live.thread, StackBinding::new(slot))
        .map_err(|_| "SMP user invalid stack release failed")?;
    nodes
        .release_space(live.node, live.context.space)
        .map_err(|_| "SMP user invalid space release failed")?;
    nodes
        .reclaim(live.node)
        .map_err(|_| "SMP user invalid Node reclamation failed")?;
    let stale = live.thread;
    let node = nodes
        .create()
        .map_err(|_| "SMP user invalid Node creation failed")?;
    nodes
        .configure(node)
        .map_err(|_| "SMP user invalid Node configuration failed")?;
    nodes
        .bind_space(node, live.context.space)
        .map_err(|_| "SMP user invalid space binding failed")?;
    let fresh = nodes
        .initial_thread(
            node,
            ThreadContext {
                space: live.context.space,
                root: space.root,
                entry: USER_CODE,
                rsp: USER_RSP,
            },
        )
        .map_err(|_| "SMP user invalid Thread creation failed")?;
    nodes
        .ready(node)
        .map_err(|_| "SMP user invalid readiness failed")?;
    if fresh.index() == stale.index()
        && fresh.raw() != stale.raw()
        && nodes.start_on(0, stale).is_err()
        && nodes.start(stale).is_err()
    {
        checks |= 1 << 8; // stale generation rejected after slot reuse
    }
    // This fixture Node was never started, so its construction is aborted
    // rather than quiesced; quiesce only applies to an Active no-runnable Node.
    nodes
        .abort_member(fresh)
        .map_err(|_| "SMP user invalid fresh Thread abort failed")?;
    nodes
        .release_space(node, live.context.space)
        .map_err(|_| "SMP user invalid fresh space release failed")?;
    nodes
        .reclaim(node)
        .map_err(|_| "SMP user invalid fresh Node reclamation failed")?;
    paging
        .destroy_user(&mut space)
        .map_err(|_| "SMP user invalid root destruction failed")?;
    let stats = paging.stats();
    if (
        stats.allocated as u64,
        stats.free as u64,
        paging.table_count() as u64,
    ) != baseline
    {
        return Err("SMP user invalid teardown did not restore the baseline");
    }
    if checks != (1 << 9) - 1 {
        return Err("SMP user invalid rejections were not the documented set");
    }
    for block in neu_smp_user_blocks.iter().take(2) {
        block.store(field::CHECKS, CHECK_INVALID);
        block.store(field::EXIT_COUNT, 0);
        block.store(field::ENTRY_COUNT, 0);
        block.store(field::FINAL_ALLOCATED, stats.allocated as u64);
        block.store(field::FINAL_FREE, stats.free as u64);
        block.store(field::FINAL_TABLES, paging.table_count() as u64);
        block.store(field::PHASE, PHASE_CANCELLED);
    }
    let mut spins = 0u64;
    loop {
        let ap_block = &smp::neu_smp_cpus[1];
        if ap_block.load(cpu::block::PARK_COUNT) == 1 && ap_block.load(cpu::block::STATE) == 4 {
            break;
        }
        if ap_block.load(cpu::block::STATE) == Bootstrap::Failed.code() {
            return Err("SMP user invalid AP failed before parking");
        }
        if spins >= USER_WAIT_LIMIT {
            return Err("SMP user invalid AP did not park within the bounded window");
        }
        core::hint::spin_loop();
        spins += 1;
    }
    // The profile never installed the BSP doorway, but the AP's bootstrap image
    // still carries it. Retire it on the parked CPU before returning.
    let ap = smp::user_cpu(1).ok_or("SMP user invalid AP resources are missing")?;
    exceptions::disarm_smp_user_cpu(
        ap.idt,
        ap.tss,
        smp::neu_smp_cpus[1].load(cpu::block::STACK_TOP),
    )?;
    let bsp_block = &smp::neu_smp_cpus[0];
    bsp_block.store(cpu::block::ISOLATION, cpu::block::ISOLATION_BSP);
    if bsp_block.advance(Bootstrap::Parked).is_err() {
        return Err("SMP user invalid BSP park transition failed");
    }
    serial::write(format_args!(
        "[NEU] SMP user invalid: rejections={:#x} baseline={} tables={}\n",
        checks, baseline.1, baseline.2
    ))
    .map_err(|_| "SMP user invalid publication failed")?;
    Ok(())
}

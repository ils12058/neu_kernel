//! Phase 9A SMP bootstrap: Limine multiprocessor discovery, fixed per-CPU
//! foundations, and an application-processor path that proves kernel entry and
//! then parks. APs never enter the BSP's single-CPU capability, scheduler,
//! paging, preemption, Channel or serial subsystems.
//!
//! Two-phase ordering: `prepare` runs before the owned page tables exist, so
//! every AP starts on the loader root and waits; `finish` publishes the owned
//! root only after the BSP has verified that the per-CPU arenas are mapped
//! there. APs switch roots, self-test, publish `Parked` and halt with IF=0.

use core::{
    arch::{asm, global_asm, x86_64::__cpuid},
    cell::UnsafeCell,
    fmt, mem, ptr,
    sync::atomic::{AtomicU64, Ordering},
};

use neu_kernel::cpu::{self, Bootstrap, Processor, Topology, Wait};

use super::exceptions::{self, ExceptionFrame};

global_asm!(include_str!("smp.S"));

/// The image profile is fixed at build time. The runner must boot exactly this
/// many vCPUs; the reject profile intentionally mismatches to prove detection.
const EXPECTED: usize = if cfg!(feature = "smp-four") {
    4
} else if cfg!(feature = "smp-two") {
    2
} else if cfg!(feature = "smp-one") {
    1
} else {
    0
};

const _: () = assert!(EXPECTED >= 1, "smp-test requires one SMP profile");
const _: () = assert!(
    cfg!(feature = "smp-one") as u8
        + cfg!(feature = "smp-two") as u8
        + cfg!(feature = "smp-four") as u8
        == 1,
    "select exactly one SMP profile"
);

// Phase 9A deliberately isolates SMP images from the single-CPU controllers.
// Those subsystems share BSP-owned registries and are not AP-safe; combining
// them would invalidate the isolation claims made by the QMP observer.
#[cfg(any(
    feature = "user-test",
    feature = "native-test",
    feature = "node-test",
    feature = "timer-test",
    feature = "switch-test",
    feature = "preempt-test",
    feature = "multi-test",
    feature = "cap-test",
    feature = "abi-02",
    feature = "channel-test",
    feature = "channel-wait-test",
    feature = "cap-move-test",
    feature = "memory-object-test",
    feature = "memory-object-live-test",
    feature = "memory-object-cap-test",
    feature = "memory-object-runtime-test",
))]
compile_error!("SMP bootstrap images are isolated from the single-CPU test subsystems");

const IA32_APIC_BASE: u32 = 0x1b;
const IA32_FS_BASE: u32 = 0xc000_0100;
const IA32_GS_BASE: u32 = 0xc000_0101;
const IA32_KERNEL_GS_BASE: u32 = 0xc000_0102;

/// IA32_APIC_BASE architectural bits: the BSP flag is set only on the
/// bootstrap processor, so an AP must observe it clear.
const APIC_BSP_BIT: u64 = 1 << 8;
const APIC_X2APIC: u64 = 1 << 10;
const APIC_GLOBAL_ENABLE: u64 = 1 << 11;

fn apic_mode_valid(value: u64) -> bool {
    value & APIC_X2APIC == 0 && value & APIC_GLOBAL_ENABLE != 0
}

const AP_WAIT_LIMIT: u64 = 1_000_000_000;
const PAGE: u64 = 4096;

const FAILURE_INFO: u64 = 1;
const FAILURE_IDENTITY: u64 = 2;
const FAILURE_RELEASE: u64 = 3;
const FAILURE_PRIVILEGE: u64 = 4;
const FAILURE_STACK: u64 = 5;
const FAILURE_TABLES: u64 = 6;
const FAILURE_GS: u64 = 7;
const FAILURE_ROOT_TIMEOUT: u64 = 8;
const FAILURE_ROOT: u64 = 9;
const FAILURE_CANARY: u64 = 10;
const FAILURE_APIC: u64 = 11;
const FAILURE_STATE: u64 = 12;
const FAILURE_FAULT: u64 = 13;

// Limine MP protocol, base revision 6. Request ID and field offsets follow
// limine-protocol 3a0526b700e356f0eac1b71a77697b3fd1c707a3 (v12.9.4).
#[repr(C)]
#[derive(Clone, Copy)]
struct MpInfo {
    processor_id: u32,
    lapic_id: u32,
    reserved: u64,
    goto_address: u64,
    extra_argument: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MpResponse {
    revision: u64,
    flags: u32,
    bsp_lapic_id: u32,
    cpu_count: u64,
    cpus: *const u64,
}

#[repr(C)]
struct MpRequest {
    id: [u64; 4],
    revision: u64,
    response: *const MpResponse,
    flags: u64,
}

const _: () = assert!(mem::size_of::<MpRequest>() == 56);
const _: () = assert!(mem::offset_of!(MpRequest, response) == 40);
const _: () = assert!(mem::offset_of!(MpRequest, flags) == 48);
const _: () = assert!(mem::size_of::<MpResponse>() == 32);
const _: () = assert!(mem::offset_of!(MpResponse, cpu_count) == 16);
const _: () = assert!(mem::offset_of!(MpResponse, cpus) == 24);
const _: () = assert!(mem::size_of::<MpInfo>() == 32);
const _: () = assert!(mem::offset_of!(MpInfo, goto_address) == 16);

struct MpCell<T>(UnsafeCell<T>);
// SAFETY: The loader writes the response pointer before entry. Afterwards only
// the BSP reads this cell with interrupts disabled and no AP can reach it.
unsafe impl<T> Sync for MpCell<T> {}

#[used]
#[unsafe(link_section = ".limine_requests")]
static MP: MpCell<MpRequest> = MpCell(UnsafeCell::new(MpRequest {
    id: [
        0xc7b1_dd30_df4c_8b88,
        0x0a82_e883_a194_f07b,
        0x95a6_7b81_9a1b_857e,
        0xa0b6_1b72_3b6a_73e0,
    ],
    revision: 0,
    response: ptr::null(),
    flags: 0, // xAPIC request; the response must confirm x2APIC is off.
}));

#[repr(C, align(64))]
pub struct CpuBlock {
    words: [AtomicU64; cpu::BLOCK_FIELDS],
}

impl CpuBlock {
    const fn new() -> Self {
        Self {
            words: [const { AtomicU64::new(0) }; cpu::BLOCK_FIELDS],
        }
    }

    fn store(&self, field: usize, value: u64) {
        self.words[field].store(value, Ordering::Relaxed);
    }

    fn load(&self, field: usize) -> u64 {
        self.words[field].load(Ordering::Relaxed)
    }

    /// One writer per CPU. `history` records every intermediate transition and
    /// becomes visible before the Release publication of `state`.
    fn advance(&self, next: Bootstrap) -> Result<(), &'static str> {
        let state = Bootstrap::from_code(self.load(cpu::block::STATE))
            .ok_or("SMP per-CPU state is not a documented value")?;
        let mut progress = cpu::Progress::from_parts(state, self.load(cpu::block::HISTORY))
            .ok_or("SMP per-CPU transition history is incomplete")?;
        progress
            .advance(next)
            .map_err(|_| "SMP per-CPU state transition is out of order")?;
        self.store(cpu::block::HISTORY, progress.history());
        self.words[cpu::block::STATE].store(progress.state().code(), Ordering::Release);
        Ok(())
    }

    fn publish_initial(&self) {
        self.store(cpu::block::HISTORY, Bootstrap::Uninitialized.bit());
        self.words[cpu::block::STATE].store(0, Ordering::Release);
    }
}

/// Exported per-CPU array. QMP reads the documented words at
/// `neu_smp_cpus + logical * neu_smp_layout[3]`.
#[unsafe(no_mangle)]
#[allow(non_upper_case_globals)]
pub static neu_smp_cpus: [CpuBlock; cpu::MAX_CPUS] = [const { CpuBlock::new() }; cpu::MAX_CPUS];

#[used]
#[unsafe(no_mangle)]
#[allow(non_upper_case_globals)]
pub static neu_smp_layout: [u64; 4] = [
    cpu::BLOCK_MAGIC,
    cpu::MAX_CPUS as u64,
    cpu::BLOCK_FIELDS as u64,
    cpu::BLOCK_STRIDE as u64,
];

#[unsafe(no_mangle)]
#[allow(non_upper_case_globals)]
pub static neu_smp_released: AtomicU64 = AtomicU64::new(0);

static OWNED_ROOT: AtomicU64 = AtomicU64::new(0);
static BSP_APIC: AtomicU64 = AtomicU64::new(u64::MAX);

unsafe extern "C" {
    static __stack_bottom: u8;
    static __stack_top: u8;
    static __df_stack_bottom: u8;
    static __df_stack_top: u8;
    static __kernel_start: u8;
    static __kernel_end: u8;
    fn neu_smp_ap_entry1();
    fn neu_smp_ap_entry2();
    fn neu_smp_ap_entry3();
    fn neu_smp_park_begin() -> !;
    static neu_smp_ap1_guard_low: u8;
    static neu_smp_ap1_guard_mid: u8;
    static neu_smp_ap1_guard_ist: u8;
    static neu_smp_ap1_stack_bottom: u8;
    static neu_smp_ap1_stack_top: u8;
    static neu_smp_ap1_ist_bottom: u8;
    static neu_smp_ap1_ist_top: u8;
    static neu_smp_ap1_gdt: u8;
    static neu_smp_ap1_tss: u8;
    static neu_smp_ap1_idt: u8;
    static neu_smp_ap2_guard_low: u8;
    static neu_smp_ap2_guard_mid: u8;
    static neu_smp_ap2_guard_ist: u8;
    static neu_smp_ap2_stack_bottom: u8;
    static neu_smp_ap2_stack_top: u8;
    static neu_smp_ap2_ist_bottom: u8;
    static neu_smp_ap2_ist_top: u8;
    static neu_smp_ap2_gdt: u8;
    static neu_smp_ap2_tss: u8;
    static neu_smp_ap2_idt: u8;
    static neu_smp_ap3_guard_low: u8;
    static neu_smp_ap3_guard_mid: u8;
    static neu_smp_ap3_guard_ist: u8;
    static neu_smp_ap3_stack_bottom: u8;
    static neu_smp_ap3_stack_top: u8;
    static neu_smp_ap3_ist_bottom: u8;
    static neu_smp_ap3_ist_top: u8;
    static neu_smp_ap3_gdt: u8;
    static neu_smp_ap3_tss: u8;
    static neu_smp_ap3_idt: u8;
}

#[derive(Clone, Copy)]
struct Resources {
    stack_bottom: u64,
    stack_top: u64,
    ist_bottom: u64,
    ist_top: u64,
    guard_low: u64,
    guard_mid: u64,
    guard_ist: u64,
    gdt: u64,
    tss: u64,
    idt: u64,
    entry: u64,
}

macro_rules! resources {
    ($stack_bottom:ident, $stack_top:ident, $ist_bottom:ident, $ist_top:ident,
     $guard_low:ident, $guard_mid:ident, $guard_ist:ident,
     $gdt:ident, $tss:ident, $idt:ident, $entry:ident) => {
        Resources {
            stack_bottom: ptr::addr_of!($stack_bottom) as u64,
            stack_top: ptr::addr_of!($stack_top) as u64,
            ist_bottom: ptr::addr_of!($ist_bottom) as u64,
            ist_top: ptr::addr_of!($ist_top) as u64,
            guard_low: ptr::addr_of!($guard_low) as u64,
            guard_mid: ptr::addr_of!($guard_mid) as u64,
            guard_ist: ptr::addr_of!($guard_ist) as u64,
            gdt: ptr::addr_of!($gdt) as u64,
            tss: ptr::addr_of!($tss) as u64,
            idt: ptr::addr_of!($idt) as u64,
            entry: $entry as *const () as u64,
        }
    };
}

fn resources(logical: usize) -> Option<Resources> {
    Some(match logical {
        1 => resources!(
            neu_smp_ap1_stack_bottom,
            neu_smp_ap1_stack_top,
            neu_smp_ap1_ist_bottom,
            neu_smp_ap1_ist_top,
            neu_smp_ap1_guard_low,
            neu_smp_ap1_guard_mid,
            neu_smp_ap1_guard_ist,
            neu_smp_ap1_gdt,
            neu_smp_ap1_tss,
            neu_smp_ap1_idt,
            neu_smp_ap_entry1
        ),
        2 => resources!(
            neu_smp_ap2_stack_bottom,
            neu_smp_ap2_stack_top,
            neu_smp_ap2_ist_bottom,
            neu_smp_ap2_ist_top,
            neu_smp_ap2_guard_low,
            neu_smp_ap2_guard_mid,
            neu_smp_ap2_guard_ist,
            neu_smp_ap2_gdt,
            neu_smp_ap2_tss,
            neu_smp_ap2_idt,
            neu_smp_ap_entry2
        ),
        3 => resources!(
            neu_smp_ap3_stack_bottom,
            neu_smp_ap3_stack_top,
            neu_smp_ap3_ist_bottom,
            neu_smp_ap3_ist_top,
            neu_smp_ap3_guard_low,
            neu_smp_ap3_guard_mid,
            neu_smp_ap3_guard_ist,
            neu_smp_ap3_gdt,
            neu_smp_ap3_tss,
            neu_smp_ap3_idt,
            neu_smp_ap_entry3
        ),
        _ => return None,
    })
}

/// Guard pages skipped by the owned-root image mapping. QMP independently
/// proves they are unmapped and lie outside every stack.
pub fn guards() -> [u64; 9] {
    [
        ptr::addr_of!(neu_smp_ap1_guard_low) as u64,
        ptr::addr_of!(neu_smp_ap1_guard_mid) as u64,
        ptr::addr_of!(neu_smp_ap1_guard_ist) as u64,
        ptr::addr_of!(neu_smp_ap2_guard_low) as u64,
        ptr::addr_of!(neu_smp_ap2_guard_mid) as u64,
        ptr::addr_of!(neu_smp_ap2_guard_ist) as u64,
        ptr::addr_of!(neu_smp_ap3_guard_low) as u64,
        ptr::addr_of!(neu_smp_ap3_guard_mid) as u64,
        ptr::addr_of!(neu_smp_ap3_guard_ist) as u64,
    ]
}

pub fn is_guard(address: u64) -> bool {
    guards().contains(&address)
}

fn block_address(logical: usize) -> u64 {
    // SAFETY: `logical < MAX_CPUS` is checked by every caller.
    unsafe { ptr::addr_of!(neu_smp_cpus).cast::<CpuBlock>().add(logical) as u64 }
}

/// Returns the logical index only when the GS base names a published block.
/// A fault before the AP publishes its identity falls back to CPUID matching.
fn current_logical() -> Option<usize> {
    let base = rdmsr(IA32_GS_BASE);
    let start = block_address(0);
    let end = start + cpu::BLOCK_STRIDE as u64 * cpu::MAX_CPUS as u64;
    if !(start..end).contains(&base) {
        return None;
    }
    let index = ((base - start) / cpu::BLOCK_STRIDE as u64) as usize;
    (neu_smp_cpus[index].load(cpu::block::MAGIC) == cpu::BLOCK_MAGIC).then_some(index)
}

fn current_ap() -> Option<&'static CpuBlock> {
    if let Some(logical) = current_logical() {
        return (logical != 0).then(|| &neu_smp_cpus[logical]);
    }
    let apic = u64::from(cpuid_apic_id());
    (1..cpu::MAX_CPUS)
        .find(|&logical| {
            neu_smp_cpus[logical].load(cpu::block::MAGIC) == cpu::BLOCK_MAGIC
                && neu_smp_cpus[logical].load(cpu::block::APIC_ID) == apic
        })
        .map(|logical| &neu_smp_cpus[logical])
}

/// The BSP is the only CPU allowed to touch COM1, the panic handler, the fatal
/// fault record and the halt contract. Before the topology is published no AP
/// executes kernel code, so the sentinel safely identifies the BSP.
pub fn is_bsp() -> bool {
    if let Some(logical) = current_logical() {
        return logical == 0;
    }
    let recorded = BSP_APIC.load(Ordering::Acquire);
    recorded == u64::MAX || u64::from(cpuid_apic_id()) == recorded
}

pub fn silent_park() -> ! {
    // SAFETY: CPU-local halt loop; IF=0, no locks, no shared state, no return.
    unsafe { neu_smp_park_begin() }
}

fn cpuid_apic_id() -> u32 {
    __cpuid(1).ebx >> 24
}

fn rdmsr(msr: u32) -> u64 {
    let (low, high): (u32, u32);
    // SAFETY: Ring 0. Reads the named architectural MSR without side effects.
    unsafe {
        asm!("rdmsr", in("ecx") msr, out("eax") low, out("edx") high,
            options(nomem, nostack, preserves_flags));
    }
    (u64::from(high) << 32) | u64::from(low)
}

fn wrmsr(msr: u32, value: u64) {
    // SAFETY: Ring 0, documented MSRs for segment bases only. The BSP/AP that
    // executes this owns the affected per-CPU address space.
    unsafe {
        asm!("wrmsr", in("ecx") msr, in("eax") value as u32, in("edx") (value >> 32) as u32,
            options(nomem, nostack, preserves_flags));
    }
}

fn stack_pointer() -> u64 {
    let value: u64;
    // SAFETY: Reads the current RSP without modifying it.
    unsafe {
        asm!("mov {}, rsp", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

fn read_cpu_state() -> (u64, u64, u64, u64, u64) {
    let (cr0, cr3, cr4, rflags): (u64, u64, u64, u64);
    // SAFETY: Ring 0 read-only control-register/flag inspection.
    unsafe {
        asm!("mov {}, cr0", out(reg) cr0, options(nomem, nostack, preserves_flags));
        asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags));
        asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack, preserves_flags));
        asm!("pushfq", "pop {}", out(reg) rflags, options(preserves_flags));
    }
    (cr0, cr3, cr4, rdmsr(0xc000_0080), rflags)
}

fn privileges_valid(cr0: u64, cr4: u64, efer: u64, rflags: u64) -> bool {
    cr0 & ((1 << 0) | (1 << 16) | (1 << 31)) == ((1 << 0) | (1 << 16) | (1 << 31))
        && cr4 & (1 << 5) != 0
        && cr4 & ((1 << 7) | (1 << 12) | (1 << 17)) == 0
        && efer & ((1 << 8) | (1 << 10) | (1 << 11)) == ((1 << 8) | (1 << 10) | (1 << 11))
        && efer & (1 << 0) == 0
        && rflags & ((1 << 9) | (1 << 10)) == 0
}

fn load_root(root: u64) {
    // SAFETY: Callers verified the owned root maps all live code, stacks and
    // descriptor tables. PCID/PGE are disabled, so this flushes translations.
    unsafe {
        asm!("mov cr3, {}", in(reg) root, options(nostack, preserves_flags));
    }
}

fn current_root() -> u64 {
    let root: u64;
    // SAFETY: Read-only CR3 inspection at Ring 0.
    unsafe {
        asm!("mov {}, cr3", out(reg) root, options(nomem, nostack, preserves_flags));
    }
    root & 0x000f_ffff_ffff_f000
}

fn log(arguments: fmt::Arguments<'_>) -> Result<(), &'static str> {
    super::serial::write(arguments).map_err(|_| "SMP serial output failed")
}

unsafe fn read_loader<T: Copy>(address: u64) -> Result<T, &'static str> {
    if address == 0
        || !super::valid_boot_range(address as usize, mem::size_of::<T>(), mem::align_of::<T>())
    {
        return Err("SMP loader pointer is null, unaligned, or outside the higher half");
    }
    // SAFETY: Caller supplies a Limine-owned pointer that satisfies the same
    // trusted immutable handoff contract as the boot adapter.
    Ok(unsafe { ptr::read_volatile(address as *const T) })
}

fn region(bottom: u64, top: u64, minimum: u64) -> Result<cpu::Region, &'static str> {
    cpu::Region::new(bottom, top, minimum, PAGE)
        .ok_or("SMP per-CPU stack is misaligned or undersized")
}

fn symbol_range() -> (u64, u64) {
    (
        ptr::addr_of!(__kernel_start) as u64,
        ptr::addr_of!(__kernel_end) as u64,
    )
}

fn validate_resources(logical: usize) -> Result<Resources, &'static str> {
    let resources = resources(logical).ok_or("SMP AP resource slot is missing")?;
    let (start, end) = symbol_range();
    // Adjacency is checked with address differences rather than object
    // identity: the linker places stack_top, guard_mid and ist_bottom at the
    // same address, and LLVM may fold `stack_top != guard_mid` to true for
    // distinct extern objects even though the linker merges them.
    if resources.stack_top.wrapping_sub(resources.stack_bottom) != cpu::AP_STACK_MINIMUM
        || resources.ist_top.wrapping_sub(resources.ist_bottom) != cpu::AP_IST_MINIMUM
    {
        return Err("SMP AP stack or IST size changed");
    }
    if resources.stack_bottom.wrapping_sub(resources.guard_low) != PAGE {
        return Err("SMP AP stack does not begin after its low guard");
    }
    if resources.guard_mid.wrapping_sub(resources.stack_bottom) != cpu::AP_STACK_MINIMUM {
        return Err("SMP AP stack top does not meet its middle guard");
    }
    if resources.ist_bottom.wrapping_sub(resources.guard_mid) != PAGE {
        return Err("SMP AP IST does not begin after its middle guard");
    }
    if resources.guard_ist.wrapping_sub(resources.ist_bottom) != cpu::AP_IST_MINIMUM {
        return Err("SMP AP IST top does not meet its IST guard");
    }
    if [
        resources.stack_bottom,
        resources.ist_bottom,
        resources.guard_low,
        resources.guard_mid,
        resources.guard_ist,
        resources.idt,
    ]
    .iter()
    .any(|value| !value.is_multiple_of(PAGE))
    {
        return Err("SMP AP arena or IDT is not page-aligned");
    }
    if resources.gdt & 15 != 0 || resources.tss & 7 != 0 || resources.idt & 15 != 0 {
        return Err("SMP AP descriptor table alignment");
    }
    if resources.gdt < start
        || resources.idt + PAGE > end
        || resources.stack_bottom < start
        || resources.ist_top > end
    {
        return Err("SMP AP resources are outside the kernel image");
    }
    if resources.entry < start || resources.entry >= end {
        return Err("SMP AP entry is outside the kernel image");
    }
    Ok(resources)
}

/// Validates the discovered topology and per-CPU resources, initializes every
/// AP-private table and arena, then releases the APs onto the loader root.
pub fn prepare() -> Result<(), &'static str> {
    let response_pointer = unsafe { ptr::read_volatile(ptr::addr_of!((*MP.0.get()).response)) };
    if response_pointer.is_null() {
        return Err("SMP request returned no processor response");
    }
    let response: MpResponse = unsafe { read_loader(response_pointer as u64)? };
    if response.revision != 0 {
        return Err("SMP response revision is not zero");
    }
    if response.flags & 1 != 0 {
        return Err("SMP x2APIC mode is not supported in Phase 9A");
    }
    if response.flags & !1 != 0 {
        return Err("SMP response has unknown flags");
    }
    let count = usize::try_from(response.cpu_count)
        .map_err(|_| "SMP processor count is not representable")?;
    if count == 0 {
        return Err("SMP response reported no processors");
    }
    if count > cpu::MAX_CPUS {
        return Err("SMP processor count exceeds the fixed per-CPU capacity");
    }
    if count != EXPECTED {
        return Err(match EXPECTED {
            1 => "SMP processor count does not match the configured 1-CPU profile",
            2 => "SMP processor count does not match the configured 2-CPU profile",
            _ => "SMP processor count does not match the configured 4-CPU profile",
        });
    }
    if !super::valid_boot_range(response.cpus as usize, count * 8, 8) {
        return Err("SMP CPU pointer array is invalid");
    }

    let bsp_cpuid = cpuid_apic_id();
    let mut processors = [Processor {
        apic_id: 0,
        processor_id: 0,
    }; cpu::MAX_CPUS];
    let mut infos = [0u64; cpu::MAX_CPUS];
    for index in 0..count {
        let info_address = unsafe { ptr::read_volatile(response.cpus.add(index)) };
        let info: MpInfo = unsafe { read_loader(info_address)? };
        if info.lapic_id != response.bsp_lapic_id && info.goto_address != 0 {
            return Err("SMP loader already released an application processor");
        }
        if info.lapic_id >= cpu::XAPIC_ID_LIMIT {
            return Err("SMP processor APIC identity is outside the xAPIC range");
        }
        if info.lapic_id != response.bsp_lapic_id
            && !super::valid_boot_range(info.reserved as usize, 8, 8)
        {
            return Err("SMP bootstrap stack pointer is invalid");
        }
        processors[index] = Processor {
            apic_id: info.lapic_id,
            processor_id: info.processor_id,
        };
        infos[index] = info_address;
    }
    let topology = Topology::discover(
        response.bsp_lapic_id,
        bsp_cpuid,
        &processors[..count],
        EXPECTED,
    )
    .map_err(|error| match error {
        cpu::TopologyError::Empty => "SMP processor description is empty",
        cpu::TopologyError::Expected => "SMP processor description count is inconsistent",
        cpu::TopologyError::Capacity => "SMP processor description exceeds the fixed capacity",
        cpu::TopologyError::DuplicateApic => "SMP processor descriptions repeat an APIC identity",
        cpu::TopologyError::DuplicateProcessor => {
            "SMP processor descriptions repeat a processor identity"
        }
        cpu::TopologyError::MissingBsp => "SMP processor descriptions omit the bootstrap processor",
        cpu::TopologyError::ApicRange => "SMP processor APIC identity is outside the xAPIC range",
        cpu::TopologyError::BspIdentity => "SMP bootstrap APIC identity disagrees with CPUID",
    })?;

    // Per-CPU stack/IST layout: BSP pair first, then each AP pair.
    let mut regions = [cpu::Region { bottom: 0, top: 0 }; 2 * cpu::MAX_CPUS];
    let mut minimums = [0u64; 2 * cpu::MAX_CPUS];
    let bsp_stack = region(
        ptr::addr_of!(__stack_bottom) as u64,
        ptr::addr_of!(__stack_top) as u64,
        cpu::BSP_STACK_MINIMUM,
    )?;
    let bsp_ist = region(
        ptr::addr_of!(__df_stack_bottom) as u64,
        ptr::addr_of!(__df_stack_top) as u64,
        cpu::BSP_IST_MINIMUM,
    )?;
    regions[0] = bsp_stack;
    minimums[0] = cpu::BSP_STACK_MINIMUM;
    regions[1] = bsp_ist;
    minimums[1] = cpu::BSP_IST_MINIMUM;
    let mut ap_resources = [const { None }; cpu::MAX_CPUS];
    for logical in 1..count {
        let resources = validate_resources(logical)?;
        regions[logical * 2] = region(
            resources.stack_bottom,
            resources.stack_top,
            cpu::AP_STACK_MINIMUM,
        )?;
        minimums[logical * 2] = cpu::AP_STACK_MINIMUM;
        regions[logical * 2 + 1] =
            region(resources.ist_bottom, resources.ist_top, cpu::AP_IST_MINIMUM)?;
        minimums[logical * 2 + 1] = cpu::AP_IST_MINIMUM;
        ap_resources[logical] = Some(resources);
    }
    cpu::validate_cpu_regions(&regions[..count * 2], &minimums[..count * 2])
        .map_err(|_| "SMP per-CPU stacks overlap or violate the documented layout")?;

    // Guards must be disjoint from every stack and from each other.
    let mut all_guards = guards();
    all_guards[..].sort_unstable();
    if all_guards.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err("SMP AP guard pages are not distinct");
    }
    for guard in super::paging::guards().iter().chain(all_guards.iter()) {
        if regions[..count * 2]
            .iter()
            .any(|region| region.contains(*guard))
        {
            return Err("SMP guard page overlaps a per-CPU stack");
        }
    }
    for resources in ap_resources[1..count].iter().copied().flatten() {
        for table in [resources.gdt, resources.tss, resources.idt] {
            if regions[..count * 2]
                .iter()
                .any(|region| region.contains(table))
            {
                return Err("SMP descriptor table overlaps a per-CPU stack");
            }
        }
    }

    let bsp_tables = exceptions::tables();
    let apic_base = rdmsr(IA32_APIC_BASE);
    if !apic_mode_valid(apic_base) || apic_base & APIC_BSP_BIT == 0 {
        return Err("SMP bootstrap requires the xAPIC-enabled bootstrap processor");
    }
    // The BSP flag is set only on this processor; APs must observe it clear.
    let expected_ap_apic_base = apic_base & !APIC_BSP_BIT;

    // Initialize every AP-private descriptor table before publication.
    for resources in ap_resources[1..count].iter().copied().flatten() {
        // SAFETY: Resources were validated inside the kernel image and outside
        // every stack; no AP is running yet and each table is CPU-private.
        unsafe {
            exceptions::build_kernel_gdt(resources.gdt as *mut u64, resources.tss);
            exceptions::build_kernel_tss(
                resources.tss as *mut u8,
                resources.stack_top,
                resources.ist_top,
            );
            exceptions::copy_kernel_idt(resources.idt as *mut u8);
        }
        let code = unsafe { ptr::read_volatile((resources.gdt + 8) as *const u64) };
        if code != 0x00af_9a00_0000_ffff {
            return Err("SMP AP GDT code descriptor readback failed");
        }
        let tss_rsp = unsafe { ptr::read_unaligned((resources.tss + 4) as *const u64) };
        let tss_ist = unsafe { ptr::read_unaligned((resources.tss + 36) as *const u64) };
        let tss_iomap = unsafe { ptr::read_unaligned((resources.tss + 102) as *const u16) };
        if tss_rsp != resources.stack_top || tss_ist != resources.ist_top || tss_iomap != 104 {
            return Err("SMP AP TSS readback failed");
        }
        let bsp_gate = unsafe { ptr::read((bsp_tables.idt_base + 16 * 8) as *const [u64; 2]) };
        let ap_gate = unsafe { ptr::read((resources.idt + 16 * 8) as *const [u64; 2]) };
        if bsp_gate != ap_gate {
            return Err("SMP AP IDT gate readback failed");
        }
        // SAFETY: Stack/IST bottoms are page-aligned and unused before release.
        unsafe {
            ptr::write_volatile(resources.stack_bottom as *mut u64, cpu::block::CANARY_VALUE);
            ptr::write_volatile(resources.ist_bottom as *mut u64, cpu::block::CANARY_VALUE);
        }
    }
    // SAFETY: The BSP's own deep-stack words only change on overflow, which the
    // guard page catches first.
    unsafe {
        ptr::write_volatile(
            ptr::addr_of!(__stack_bottom) as *mut u64,
            cpu::block::CANARY_VALUE,
        );
        ptr::write_volatile(
            ptr::addr_of!(__df_stack_bottom) as *mut u64,
            cpu::block::CANARY_VALUE,
        );
    }

    // Publish the BSP block before any AP can observe the topology.
    let bsp = topology.get(0).ok_or("SMP topology omitted the BSP")?;
    let bsp_block = &neu_smp_cpus[0];
    bsp_block.publish_initial();
    bsp_block.store(cpu::block::MAGIC, cpu::BLOCK_MAGIC);
    bsp_block.store(cpu::block::LOGICAL, 0);
    bsp_block.store(cpu::block::IS_BSP, 1);
    bsp_block.store(cpu::block::APIC_ID, u64::from(bsp.processor.apic_id));
    bsp_block.store(
        cpu::block::PROCESSOR_ID,
        u64::from(bsp.processor.processor_id),
    );
    bsp_block.store(cpu::block::CPUID_APIC, u64::from(bsp_cpuid));
    bsp_block.store(cpu::block::ENTRY_COUNT, 1);
    bsp_block.store(cpu::block::STACK_BOTTOM, bsp_stack.bottom);
    bsp_block.store(cpu::block::STACK_TOP, bsp_stack.top);
    bsp_block.store(cpu::block::ENTRY_RSP, stack_pointer());
    bsp_block.store(cpu::block::IST_BOTTOM, bsp_ist.bottom);
    bsp_block.store(cpu::block::IST_TOP, bsp_ist.top);
    bsp_block.store(cpu::block::GDT_BASE, bsp_tables.gdt_base);
    bsp_block.store(cpu::block::GDT_LIMIT, bsp_tables.gdt_limit);
    bsp_block.store(cpu::block::TSS_BASE, bsp_tables.tss_base);
    bsp_block.store(cpu::block::TSS_LIMIT, bsp_tables.tss_limit);
    bsp_block.store(cpu::block::IDT_BASE, bsp_tables.idt_base);
    bsp_block.store(cpu::block::IDT_LIMIT, bsp_tables.idt_limit);
    let (cr0, cr3, cr4, efer, rflags) = read_cpu_state();
    bsp_block.store(cpu::block::CR0, cr0);
    bsp_block.store(cpu::block::CR3, cr3);
    bsp_block.store(cpu::block::CR4, cr4);
    bsp_block.store(cpu::block::EFER, efer);
    bsp_block.store(cpu::block::RFLAGS, rflags);
    bsp_block.store(cpu::block::APIC_BASE, apic_base);
    bsp_block.store(cpu::block::EXPECTED_APIC_BASE, apic_base);
    bsp_block.store(cpu::block::CANARY, cpu::block::CANARY_VALUE);
    bsp_block.store(cpu::block::CANARY_IST, cpu::block::CANARY_VALUE);
    bsp_block.store(cpu::block::SCHEDULER_LOCAL, 0);
    bsp_block.advance(Bootstrap::Prepared)?;

    for logical in 1..count {
        let assignment = topology.get(logical).ok_or("SMP topology is not dense")?;
        let resources = ap_resources[logical].ok_or("SMP AP resource slot is missing")?;
        let info_address = processors[..count]
            .iter()
            .position(|processor| processor.apic_id == assignment.processor.apic_id)
            .map(|index| infos[index])
            .ok_or("SMP AP descriptor is missing")?;
        let block = &neu_smp_cpus[logical];
        block.publish_initial();
        block.store(cpu::block::MAGIC, cpu::BLOCK_MAGIC);
        block.store(cpu::block::LOGICAL, logical as u64);
        block.store(cpu::block::IS_BSP, 0);
        block.store(cpu::block::APIC_ID, u64::from(assignment.processor.apic_id));
        block.store(
            cpu::block::PROCESSOR_ID,
            u64::from(assignment.processor.processor_id),
        );
        block.store(cpu::block::STACK_BOTTOM, resources.stack_bottom);
        block.store(cpu::block::STACK_TOP, resources.stack_top);
        block.store(cpu::block::IST_BOTTOM, resources.ist_bottom);
        block.store(cpu::block::IST_TOP, resources.ist_top);
        block.store(cpu::block::GDT_BASE, resources.gdt);
        block.store(cpu::block::GDT_LIMIT, bsp_tables.gdt_limit);
        block.store(cpu::block::TSS_BASE, resources.tss);
        block.store(cpu::block::TSS_LIMIT, bsp_tables.tss_limit);
        block.store(cpu::block::IDT_BASE, resources.idt);
        block.store(cpu::block::IDT_LIMIT, bsp_tables.idt_limit);
        block.store(cpu::block::CANARY, cpu::block::CANARY_VALUE);
        block.store(cpu::block::CANARY_IST, cpu::block::CANARY_VALUE);
        block.store(cpu::block::EXPECTED_APIC_BASE, expected_ap_apic_base);
        block.store(cpu::block::MP_INFO, info_address);
        block.store(cpu::block::RELEASE_RIP, resources.entry);
        block.store(cpu::block::SCHEDULER_LOCAL, 0);
        block.advance(Bootstrap::Prepared)?;
    }

    BSP_APIC.store(u64::from(response.bsp_lapic_id), Ordering::Release);
    wrmsr(IA32_GS_BASE, block_address(0));
    bsp_block.store(cpu::block::GS_BASE, rdmsr(IA32_GS_BASE));
    if rdmsr(IA32_GS_BASE) != block_address(0) {
        return Err("SMP BSP GS-base publication failed");
    }

    log(format_args!(
        "[NEU] SMP topology: cpus={count} aps={} bsp_apic={} x2apic=false\n",
        count - 1,
        response.bsp_lapic_id
    ))?;
    for logical in 0..count {
        let assignment = topology.get(logical).ok_or("SMP topology is not dense")?;
        log(format_args!(
            "[NEU] SMP CPU: logical={logical} apic={} processor={} bsp={}\n",
            assignment.processor.apic_id,
            assignment.processor.processor_id,
            logical == 0
        ))?;
    }

    let mut released = 0u64;
    for logical in 1..count {
        let assignment = topology.get(logical).ok_or("SMP topology is not dense")?;
        let resources = ap_resources[logical].ok_or("SMP AP resource slot is missing")?;
        let info_address = neu_smp_cpus[logical].load(cpu::block::MP_INFO);
        let processor_index = processors[..count]
            .iter()
            .position(|processor| processor.apic_id == assignment.processor.apic_id)
            .ok_or("SMP AP descriptor is missing")?;
        if infos[processor_index] != info_address {
            return Err("SMP AP descriptor association changed");
        }
        // SAFETY: The loader-owned descriptor remains mapped on the current
        // root until paging activation. Only this BSP writes goto_address.
        let goto = unsafe {
            AtomicU64::from_ptr(ptr::addr_of_mut!(
                (*(infos[processor_index] as *mut MpInfo)).goto_address
            ))
        };
        if goto.load(Ordering::Acquire) != 0 {
            return Err("SMP loader already released an application processor");
        }
        goto.store(resources.entry, Ordering::Release);
        released += 1;
    }
    neu_smp_released.store(released, Ordering::Release);
    bsp_block.advance(Bootstrap::Entered)?;
    log(format_args!(
        "[NEU] SMP bootstrap armed: expected={EXPECTED} released={released}\n"
    ))?;
    Ok(())
}

fn apic_base_of(logical: usize) -> u64 {
    neu_smp_cpus[logical].load(cpu::block::APIC_BASE)
}

/// Verifies the complete per-CPU publication and parks the BSP end state.
pub fn finish(paging: &super::paging::Paging<'_, '_>) -> Result<(), &'static str> {
    if OWNED_ROOT.load(Ordering::Acquire) != 0 {
        return Err("SMP owned root was published twice");
    }
    let root = paging.root();
    if root == paging.old_root() || current_root() != root {
        return Err("SMP finish is not executing on the owned root");
    }
    let (start, end) = symbol_range();
    // Bounded by the profile constant at runtime; the 1-CPU profile iterates
    // over no application processors.
    let ap_count = EXPECTED;
    for logical in 1..ap_count {
        let resources = resources(logical).ok_or("SMP AP resource slot is missing")?;
        for address in [
            resources.stack_bottom,
            resources.ist_bottom,
            resources.gdt,
            resources.tss,
            resources.idt,
        ] {
            let translation = paging
                .translate(address)
                .map_err(|_| "SMP per-CPU resource translation failed")?
                .ok_or("SMP per-CPU resource is absent from the owned root")?;
            if !translation.permissions.write
                || translation.permissions.execute
                || translation.user
                || translation.page_size != PAGE
            {
                return Err("SMP per-CPU resource has unexpected owned-root permissions");
            }
            if !(start..end).contains(&address) {
                return Err("SMP per-CPU resource is outside the kernel image");
            }
        }
        for guard in [
            resources.guard_low,
            resources.guard_mid,
            resources.guard_ist,
        ] {
            if paging
                .translate(guard)
                .map_err(|_| "SMP guard translation failed")?
                .is_some()
            {
                return Err("SMP AP guard page is mapped in the owned root");
            }
        }
    }
    OWNED_ROOT.store(root, Ordering::Release);

    let bsp_block = &neu_smp_cpus[0];
    bsp_block.advance(Bootstrap::Ready)?;
    let mut parked = 0usize;
    for (offset, block) in neu_smp_cpus[1..ap_count].iter().enumerate() {
        let logical = offset + 1;
        let mut spins = 0u64;
        loop {
            let state = Bootstrap::from_code(block.load(cpu::block::STATE))
                .ok_or("SMP AP published an undocumented state")?;
            match cpu::ap_wait(state, block.load(cpu::block::HISTORY)) {
                Wait::Complete => break,
                Wait::Failed => return Err("SMP AP reported a failed bootstrap"),
                Wait::Pending => {}
            }
            if spins >= AP_WAIT_LIMIT {
                return Err("SMP AP did not park within the bounded observation window");
            }
            core::hint::spin_loop();
            spins += 1;
        }
        verify_ap(logical, root, start, end)?;
        parked += 1;
        log(format_args!(
            "[NEU] SMP AP parked: logical={logical} apic={} entry_rsp={:#x} park_rsp={:#x} cr3={:#x} stack={:#x}\n",
            block.load(cpu::block::APIC_ID),
            block.load(cpu::block::ENTRY_RSP),
            block.load(cpu::block::PARK_RSP),
            block.load(cpu::block::CR3),
            block.load(cpu::block::STACK_BOTTOM)
        ))?;
    }

    // The BSP revalidates its own publication and the untouched canaries.
    let bsp_tables = exceptions::tables();
    if bsp_block.load(cpu::block::MAGIC) != cpu::BLOCK_MAGIC
        || bsp_block.load(cpu::block::LOGICAL) != 0
        || bsp_block.load(cpu::block::IS_BSP) != 1
        || bsp_block.load(cpu::block::APIC_ID) != u64::from(cpuid_apic_id())
        || bsp_block.load(cpu::block::GDT_BASE) != bsp_tables.gdt_base
        || bsp_block.load(cpu::block::TSS_BASE) != bsp_tables.tss_base
        || bsp_block.load(cpu::block::IDT_BASE) != bsp_tables.idt_base
        || bsp_block.load(cpu::block::GS_BASE) != block_address(0)
    {
        return Err("SMP BSP publication does not match its own CPU state");
    }
    let bsp_rsp = stack_pointer();
    if !(bsp_block.load(cpu::block::STACK_BOTTOM)..bsp_block.load(cpu::block::STACK_TOP))
        .contains(&bsp_rsp)
    {
        return Err("SMP BSP is not executing on its own stack");
    }
    if unsafe { ptr::read_volatile(ptr::addr_of!(__stack_bottom) as *const u64) }
        != cpu::block::CANARY_VALUE
        || unsafe { ptr::read_volatile(ptr::addr_of!(__df_stack_bottom) as *const u64) }
            != cpu::block::CANARY_VALUE
    {
        return Err("SMP BSP stack canary changed");
    }
    let (cr0, cr3, cr4, efer, rflags) = read_cpu_state();
    if cr3 != root || !privileges_valid(cr0, cr4, efer, rflags) {
        return Err("SMP BSP architectural state is invalid after AP park");
    }
    let apic_base = rdmsr(IA32_APIC_BASE);
    if apic_base != bsp_block.load(cpu::block::EXPECTED_APIC_BASE) {
        return Err("SMP BSP APIC base changed during bootstrap");
    }
    if !apic_mode_valid(apic_base) || apic_base & APIC_BSP_BIT == 0 {
        return Err("SMP BSP left xAPIC or lost its bootstrap identity");
    }
    bsp_block.store(cpu::block::CR0, cr0);
    bsp_block.store(cpu::block::CR3, cr3);
    bsp_block.store(cpu::block::CR4, cr4);
    bsp_block.store(cpu::block::EFER, efer);
    bsp_block.store(cpu::block::RFLAGS, rflags);
    bsp_block.store(cpu::block::READY_RSP, bsp_rsp);
    bsp_block.store(cpu::block::APIC_BASE, apic_base);
    bsp_block.store(cpu::block::SELF_TEST, cpu::block::SELF_TEST_ALL);
    bsp_block.store(cpu::block::ISOLATION, cpu::block::ISOLATION_BSP);

    let released = neu_smp_released.load(Ordering::Acquire);
    if released != (EXPECTED - 1) as u64 {
        return Err("SMP released processor count does not match the profile");
    }
    log(format_args!(
        "[NEU] SMP bootstrap complete: cpus={EXPECTED} aps={} released={released} parked={parked} root={root:#x}\n",
        EXPECTED - 1
    ))?;
    bsp_block.advance(Bootstrap::Parked)?;
    Ok(())
}

fn verify_ap(logical: usize, root: u64, start: u64, end: u64) -> Result<(), &'static str> {
    let block = &neu_smp_cpus[logical];
    let own = resources(logical).ok_or("SMP AP resource slot is missing")?;
    if block.load(cpu::block::MAGIC) != cpu::BLOCK_MAGIC
        || block.load(cpu::block::LOGICAL) != logical as u64
        || block.load(cpu::block::IS_BSP) != 0
        || block.load(cpu::block::ENTRY_COUNT) != 1
        || block.load(cpu::block::FAILURE) != 0
        || block.load(cpu::block::FAULT_VECTOR) != 0
        || block.load(cpu::block::PARK_COUNT) != 1
        || block.load(cpu::block::SCHEDULER_LOCAL) != 0
        || block.load(cpu::block::RESERVED) != 0
    {
        return Err("SMP AP block does not record exactly one clean kernel entry");
    }
    let apic = block.load(cpu::block::APIC_ID);
    if apic >= u64::from(cpu::XAPIC_ID_LIMIT)
        || block.load(cpu::block::CPUID_APIC) != apic
        || block.load(cpu::block::EXPECTED_APIC_BASE) != block.load(cpu::block::APIC_BASE)
        || block.load(cpu::block::APIC_BASE) != (apic_base_of(0) & !APIC_BSP_BIT)
        || block.load(cpu::block::APIC_BASE) & APIC_BSP_BIT != 0
        || !apic_mode_valid(block.load(cpu::block::APIC_BASE))
    {
        return Err("SMP AP hardware identity or APIC mode is inconsistent");
    }
    if logical > 1 {
        let previous = neu_smp_cpus[logical - 1].load(cpu::block::APIC_ID);
        if previous >= apic || previous == u64::from(cpuid_apic_id()) {
            return Err("SMP AP logical indices are not ordered by distinct APIC identity");
        }
    }
    if apic == u64::from(cpuid_apic_id()) {
        return Err("SMP AP shares the BSP APIC identity");
    }
    if block.load(cpu::block::STACK_BOTTOM) != own.stack_bottom
        || block.load(cpu::block::STACK_TOP) != own.stack_top
        || block.load(cpu::block::IST_BOTTOM) != own.ist_bottom
        || block.load(cpu::block::IST_TOP) != own.ist_top
        || block.load(cpu::block::GDT_BASE) != own.gdt
        || block.load(cpu::block::TSS_BASE) != own.tss
        || block.load(cpu::block::IDT_BASE) != own.idt
        || block.load(cpu::block::GDT_LIMIT) != exceptions::tables().gdt_limit
        || block.load(cpu::block::TSS_LIMIT) != exceptions::tables().tss_limit
        || block.load(cpu::block::IDT_LIMIT) != exceptions::tables().idt_limit
    {
        return Err("SMP AP stack or descriptor ownership does not match the ELF layout");
    }
    let stack = cpu::Region {
        bottom: own.stack_bottom,
        top: own.stack_top,
    };
    for field in [
        cpu::block::ENTRY_RSP,
        cpu::block::READY_RSP,
        cpu::block::PARK_RSP,
    ] {
        if !stack.contains(block.load(field)) {
            return Err("SMP AP reported an RSP outside its own stack");
        }
    }
    if block.load(cpu::block::READY_RSP) < block.load(cpu::block::PARK_RSP) {
        return Err("SMP AP park state was published before readiness");
    }
    let loader_rsp = block.load(cpu::block::LOADER_RSP);
    if loader_rsp == 0 || (start..end).contains(&loader_rsp) {
        return Err("SMP AP did not abandon the loader-owned bootstrap stack");
    }
    let loader_cr3 = block.load(cpu::block::LOADER_CR3);
    if loader_cr3 == 0 || loader_cr3 == root || block.load(cpu::block::CR3) != root {
        return Err("SMP AP did not switch from the loader root to the owned root");
    }
    if block.load(cpu::block::GS_BASE) != block_address(logical) {
        return Err("SMP AP GS-base identity is not its own block");
    }
    if block.load(cpu::block::CANARY) != cpu::block::CANARY_VALUE
        || block.load(cpu::block::OBSERVED_CANARY) != cpu::block::CANARY_VALUE
        || block.load(cpu::block::CANARY_IST) != cpu::block::CANARY_VALUE
        || block.load(cpu::block::OBSERVED_IST_CANARY) != cpu::block::CANARY_VALUE
    {
        return Err("SMP AP stack/IST canary changed");
    }
    if block.load(cpu::block::SELF_TEST) != cpu::block::SELF_TEST_ALL
        || block.load(cpu::block::ISOLATION) != cpu::block::ISOLATION_AP
    {
        return Err("SMP AP self-test or isolation publication is incomplete");
    }
    let mp_info = block.load(cpu::block::MP_INFO);
    if !super::valid_boot_range(mp_info as usize, mem::size_of::<MpInfo>(), 8)
        || block.load(cpu::block::RELEASE_RIP) != own.entry
    {
        return Err("SMP AP descriptor or release entry does not match the preparation");
    }
    let spins = block.load(cpu::block::WAIT_SPINS);
    if spins == 0 || spins >= AP_WAIT_LIMIT {
        return Err("SMP AP root wait was not a bounded completed wait");
    }
    if !privileges_valid(
        block.load(cpu::block::CR0),
        block.load(cpu::block::CR4),
        block.load(cpu::block::EFER),
        block.load(cpu::block::RFLAGS),
    ) {
        return Err("SMP AP privileged state or interrupt flag is invalid");
    }
    let bsp_tables = exceptions::tables();
    if own.gdt == bsp_tables.gdt_base
        || own.tss == bsp_tables.tss_base
        || own.idt == bsp_tables.idt_base
    {
        return Err("SMP AP shares descriptor tables with the BSP");
    }
    for other in 1..logical {
        let previous = resources(other).ok_or("SMP AP resource slot is missing")?;
        if own.gdt == previous.gdt
            || own.tss == previous.tss
            || own.idt == previous.idt
            || own.stack_bottom == previous.stack_bottom
            || own.ist_bottom == previous.ist_bottom
        {
            return Err("SMP APs share a descriptor table or stack");
        }
    }
    Ok(())
}

fn ap_fail(block: &CpuBlock, code: u64) -> ! {
    block.store(cpu::block::FAILURE, code);
    let _ = block.advance(Bootstrap::Failed);
    silent_park()
}

/// Called from the CPL0 exception entry on an application processor. The raw
/// frame is retained in the AP's own block; no BSP global or serial is touched.
pub(super) fn ap_fault(frame: &ExceptionFrame, _cr2: u64, handler_rsp: u64) -> ! {
    if let Some(block) = current_ap() {
        block.store(cpu::block::FAULT_VECTOR, frame.vector);
        block.store(cpu::block::FAULT_ERROR, frame.error);
        block.store(cpu::block::FAULT_RIP, frame.rip);
        block.store(cpu::block::PARK_RSP, handler_rsp);
        block.store(cpu::block::FAILURE, FAILURE_FAULT);
        let _ = block.advance(Bootstrap::Failed);
    }
    silent_park()
}

/// AP entry after the assembly stub replaced the loader stack. Every step is
/// CPU-local; shared state is only the BSP-published block and owned root.
#[unsafe(no_mangle)]
extern "C" fn neu_smp_ap_main(info: *const MpInfo, logical: u64, loader_rsp: u64) -> ! {
    let index = logical as usize;
    if index == 0 || index >= cpu::MAX_CPUS || index >= EXPECTED {
        silent_park();
    }
    let block = &neu_smp_cpus[index];
    if block.load(cpu::block::MAGIC) != cpu::BLOCK_MAGIC || block.load(cpu::block::IS_BSP) != 0 {
        silent_park();
    }
    block.words[cpu::block::ENTRY_COUNT].fetch_add(1, Ordering::Relaxed);
    block.store(cpu::block::ENTRY_RSP, stack_pointer());
    block.store(cpu::block::LOADER_RSP, loader_rsp);
    block.store(cpu::block::MP_INFO, info as u64);
    if info.is_null() {
        ap_fail(block, FAILURE_INFO);
    }
    let apic = cpuid_apic_id();
    block.store(cpu::block::CPUID_APIC, u64::from(apic));
    // SAFETY: Limine passed this HHDM descriptor for this CPU; it stays mapped
    // until the owned-root switch and is never dereferenced afterwards.
    let descriptor = unsafe { ptr::read_volatile(info) };
    let expected_apic = block.load(cpu::block::APIC_ID) as u32;
    if apic != expected_apic
        || descriptor.lapic_id != expected_apic
        || u64::from(descriptor.processor_id) != block.load(cpu::block::PROCESSOR_ID)
    {
        ap_fail(block, FAILURE_IDENTITY);
    }
    if descriptor.goto_address != block.load(cpu::block::RELEASE_RIP) {
        ap_fail(block, FAILURE_RELEASE);
    }

    let mut passed = 0u64;
    let (cr0, cr3, cr4, efer, rflags) = read_cpu_state();
    block.store(cpu::block::LOADER_CR3, cr3);
    block.store(cpu::block::CR0, cr0);
    block.store(cpu::block::CR4, cr4);
    block.store(cpu::block::EFER, efer);
    block.store(cpu::block::RFLAGS, rflags);
    if !privileges_valid(cr0, cr4, efer, rflags) {
        ap_fail(block, FAILURE_PRIVILEGE);
    }
    passed |= cpu::block::SELF_TEST_PRIVILEGE;
    let stack = cpu::Region {
        bottom: block.load(cpu::block::STACK_BOTTOM),
        top: block.load(cpu::block::STACK_TOP),
    };
    if !stack.contains(stack_pointer()) {
        ap_fail(block, FAILURE_STACK);
    }
    passed |= cpu::block::SELF_TEST_STACK;

    if exceptions::load_ap_tables(
        block.load(cpu::block::GDT_BASE),
        block.load(cpu::block::IDT_BASE),
    )
    .is_err()
        || exceptions::verify_ap_tables(
            block.load(cpu::block::GDT_BASE),
            block.load(cpu::block::TSS_BASE),
            block.load(cpu::block::STACK_TOP),
            block.load(cpu::block::IST_TOP),
        )
        .is_err()
    {
        ap_fail(block, FAILURE_TABLES);
    }
    passed |= cpu::block::SELF_TEST_GDT | cpu::block::SELF_TEST_IDT | cpu::block::SELF_TEST_IST;

    // GS is CPU-local identity. `neu_load_gdt` reloads the flat GS selector and
    // clears its base, so the IA32_GS_BASE write must follow the table switch.
    wrmsr(IA32_FS_BASE, 0);
    wrmsr(IA32_KERNEL_GS_BASE, 0);
    let identity = block_address(index);
    wrmsr(IA32_GS_BASE, identity);
    if rdmsr(IA32_GS_BASE) != identity {
        ap_fail(block, FAILURE_GS);
    }
    block.store(cpu::block::GS_BASE, identity);
    passed |= cpu::block::SELF_TEST_GS;

    let apic_base = rdmsr(IA32_APIC_BASE);
    block.store(cpu::block::APIC_BASE, apic_base);
    if apic_base != block.load(cpu::block::EXPECTED_APIC_BASE)
        || apic_base & APIC_BSP_BIT != 0
        || !apic_mode_valid(apic_base)
    {
        ap_fail(block, FAILURE_APIC);
    }
    passed |= cpu::block::SELF_TEST_APIC;
    passed |= cpu::block::SELF_TEST_IDENTITY;

    if block.advance(Bootstrap::Entered).is_err() {
        ap_fail(block, FAILURE_STATE);
    }

    let mut spins = 0u64;
    let root = loop {
        let root = OWNED_ROOT.load(Ordering::Acquire);
        if root != 0 {
            break root;
        }
        if spins >= AP_WAIT_LIMIT {
            ap_fail(block, FAILURE_ROOT_TIMEOUT);
        }
        core::hint::spin_loop();
        spins += 1;
    };
    block.store(cpu::block::WAIT_SPINS, spins);
    load_root(root);
    if current_root() != root {
        ap_fail(block, FAILURE_ROOT);
    }
    block.store(cpu::block::CR3, root);
    passed |= cpu::block::SELF_TEST_ROOT;

    // SAFETY: The canary words are inside this CPU's private stack arenas and
    // remain mapped by the owned root verified by the BSP before publication.
    let stack_canary = unsafe { ptr::read_volatile(stack.bottom as *const u64) };
    let ist_canary =
        unsafe { ptr::read_volatile(block.load(cpu::block::IST_BOTTOM) as *const u64) };
    if stack_canary != cpu::block::CANARY_VALUE || ist_canary != cpu::block::CANARY_VALUE {
        ap_fail(block, FAILURE_CANARY);
    }
    block.store(cpu::block::OBSERVED_CANARY, stack_canary);
    block.store(cpu::block::OBSERVED_IST_CANARY, ist_canary);

    block.store(cpu::block::READY_RSP, stack_pointer());
    block.store(cpu::block::SELF_TEST, passed);
    if block.advance(Bootstrap::Ready).is_err() {
        ap_fail(block, FAILURE_STATE);
    }
    block.store(cpu::block::PARK_RSP, stack_pointer());
    block.store(cpu::block::PARK_COUNT, 1);
    block.store(cpu::block::ISOLATION, cpu::block::ISOLATION_AP);
    if block.advance(Bootstrap::Parked).is_err() {
        ap_fail(block, FAILURE_STATE);
    }
    silent_park()
}

//! CPU-owned FXSAVE transport for the conventional SMP bridge. Every user
//! boundary is captured by assembly before Rust, including returning IRQs.
use super::*;
use neu_kernel::cpu_state::{self as policy, FxImage, Slot};

const MAGIC: u64 = 0x4e45_555f_4650_5431;
#[repr(C, align(16))]
struct ExtendedTrace {
    words: [AtomicU64; 16],
    image: UnsafeCell<FxImage>,
    kernel: UnsafeCell<FxImage>,
}
// SAFETY: One verified CPU owns each row, and a row is published only after
// its two snapshots have been copied. The external observer stops all CPUs.
unsafe impl Sync for ExtendedTrace {}
impl ExtendedTrace {
    const fn new() -> Self {
        Self {
            words: [const { AtomicU64::new(0) }; 16],
            image: UnsafeCell::new(FxImage::zero()),
            kernel: UnsafeCell::new(FxImage::zero()),
        }
    }
}
#[unsafe(no_mangle)]
static neu_cpu_state_traces: [[ExtendedTrace; TRACE_LIMIT]; cpu::MAX_CPUS] =
    [const { [const { ExtendedTrace::new() }; TRACE_LIMIT] }; cpu::MAX_CPUS];
#[unsafe(no_mangle)]
static neu_cpu_state_entries: [[ExtendedTrace; TRACE_LIMIT]; cpu::MAX_CPUS] =
    [const { [const { ExtendedTrace::new() }; TRACE_LIMIT] }; cpu::MAX_CPUS];
#[unsafe(no_mangle)]
static neu_cpu_state_features: [[AtomicU64; 16]; cpu::MAX_CPUS] =
    [const { [const { AtomicU64::new(0) }; 16] }; cpu::MAX_CPUS];
#[unsafe(no_mangle)]
static neu_cpu_state_storage: [AtomicU64; 8] = [const { AtomicU64::new(0) }; 8];
#[used]
#[unsafe(no_mangle)]
static neu_cpu_state_layout: [u64; 26] = [
    MAGIC,
    1,
    policy::BYTES as u64,
    size_of::<Slot>() as u64,
    offset_of!(Slot, identity) as u64,
    offset_of!(Slot, fs_base) as u64,
    offset_of!(Slot, saves) as u64,
    offset_of!(Slot, loads) as u64,
    offset_of!(Slot, live) as u64,
    offset_of!(Slot, cpu) as u64,
    offset_of!(Slot, retired_generation) as u64,
    size_of::<Cpu>() as u64,
    offset_of!(Cpu, bridge) as u64,
    offset_of!(Cpu, kernel_fx) as u64,
    offset_of!(Cpu, user_fx) as u64,
    offset_of!(Cpu, baseline_fx) as u64,
    offset_of!(Cpu, final_fx) as u64,
    size_of::<ExtendedTrace>() as u64,
    offset_of!(ExtendedTrace, image) as u64,
    offset_of!(ExtendedTrace, kernel) as u64,
    TRACE_LIMIT as u64,
    cpu::MAX_CPUS as u64,
    node::MAX_THREADS as u64,
    BRIDGE_WORDS as u64,
    policy::REQUIRED_EDX as u64,
    policy::NO_CPU,
];

fn cr4() -> u64 {
    let value;
    // SAFETY: Read-only architecture state in the privileged bridge.
    unsafe {
        asm!("mov {}, cr4", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}
fn fs_selector() -> u16 {
    let value;
    // SAFETY: Read-only segment selector.
    unsafe {
        asm!("mov {0:x}, fs", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}
fn snapshot_fx(destination: *mut FxImage) {
    let old_cr0 = smp::cr0();
    let old_cr4 = cr4();
    let enabled_cr0 = (old_cr0 & !12) | 34;
    let enabled_cr4 = (old_cr4 & !((1 << 16) | (1 << 18))) | (1 << 9) | (1 << 10);
    // SAFETY: Required CPUID bits were checked, IF=0, destination is an aligned
    // CPU-owned kernel image. FXSAVE changes no FP registers; controls restore.
    unsafe {
        asm!("mov cr0, {enabled0}", "mov cr4, {enabled4}", "fxsave64 [{image}]",
             "mov cr4, {original4}", "mov cr0, {original0}",
             enabled0=in(reg) enabled_cr0, enabled4=in(reg) enabled_cr4,
             image=in(reg) destination, original4=in(reg) old_cr4, original0=in(reg) old_cr0,
             options(nostack, preserves_flags));
    }
}
#[cfg(feature = "test-cpu-state-kernel")]
fn seed_kernel(cpu: usize) {
    let local = &neu_smp_sched_cpus[cpu];
    let mut image = FxImage::initial(policy::FALLBACK_MXCSR_MASK).expect("kernel sentinel mask");
    image.0[4] = 255;
    for i in 0..8 {
        let value = (cpu as u64 + 1) * 1000 + 8 - i as u64;
        let exponent = 63 - value.leading_zeros();
        let offset = 32 + i * 16;
        image.0[offset..offset + 8].copy_from_slice(&(value << (63 - exponent)).to_le_bytes());
        image.0[offset + 8..offset + 10]
            .copy_from_slice(&(0x3fffu16 + exponent as u16).to_le_bytes());
    }
    for i in 0..16 {
        let value = 0xdead_0000_0000_0000 | ((cpu as u64 + 1) << 40) | i as u64;
        image.0[160 + i * 16..168 + i * 16].copy_from_slice(&value.to_le_bytes());
        image.0[168 + i * 16..176 + i * 16].copy_from_slice(&(!value).to_le_bytes());
    }
    let original0 = smp::cr0();
    let original4 = cr4();
    let enabled0 = (original0 & !12) | 34;
    let enabled4 = (original4 & !0x50000) | 0x600;
    let fs_base = local.words.as_ptr() as u64;
    // SAFETY: Dedicated supported-CPU fixture at IF=0 before the baseline;
    // seed CPU-specific kernel FP/FS sentinels without changing its controls.
    unsafe {
        asm!("mov cr0, {enabled0}", "mov cr4, {enabled4}", "fxrstor64 [{image}]",
             "mov cr4, {original4}", "mov cr0, {original0}",
             enabled0=in(reg) enabled0, enabled4=in(reg) enabled4,
             original0=in(reg) original0, original4=in(reg) original4,
             image=in(reg) ptr::from_ref(&image), options(nostack, preserves_flags));
        asm!("mov fs, {selector:x}", "wrmsr", selector=in(reg) 0x10u16,
             in("ecx") 0xc0000100u32, in("eax") fs_base as u32,
             in("edx") (fs_base >> 32) as u32, options(nostack, preserves_flags));
    }
}
pub(super) fn probe_cpu(cpu: usize) {
    let local = &neu_smp_sched_cpus[cpu];
    let feature = core::arch::x86_64::__cpuid(1);
    #[cfg(feature = "test-cpu-state-kernel")]
    if policy::supported(feature.edx) {
        seed_kernel(cpu);
    }
    let mut row = [0; 16];
    row[0] = u64::from(feature.edx);
    row[2] = rdmsr(0xc0000100);
    row[3] = u64::from(fs_selector());
    row[4] = smp::cr0();
    row[5] = cr4();
    row[8] = u64::from(feature.ecx);
    row[9] = u64::from(core::arch::x86_64::__cpuid_count(7, 0).ebx);
    row[10] = local.baseline_fx.get() as u64;
    row[11] = local.final_fx.get() as u64;
    if policy::supported(feature.edx) {
        snapshot_fx(local.baseline_fx.get());
        // SAFETY: This CPU just populated its own aligned FXSAVE image.
        let image = unsafe { ptr::read_volatile(local.baseline_fx.get()) };
        row[7] = u64::from(image.mask());
        match policy::normalized_mask(image.mask()) {
            Ok(mask) => {
                row[1] = u64::from(mask);
                row[6] = 1;
            }
            Err(_) => row[6] = 2,
        }
    } else {
        row[6] = 2;
    }
    let ready = row[6];
    row[6] = 0;
    publish(&neu_cpu_state_features[cpu], row);
    neu_cpu_state_features[cpu][6].store(ready, Ordering::Release);
}
pub(super) fn configure() -> Result<(), &'static str> {
    probe_cpu(0);
    // Publish raw ownership storage even on a CPU refusal, before installation.
    with_state(0, |_| ());
    let mut features = [0; cpu::MAX_CPUS];
    let mut masks = [0; cpu::MAX_CPUS];
    for cpu in 0..smp::cpu_count() {
        for _ in 0..WAIT_LIMIT {
            if neu_cpu_state_features[cpu][6].load(Ordering::Acquire) != 0 {
                break;
            }
            core::hint::spin_loop();
        }
        let row = &neu_cpu_state_features[cpu];
        features[cpu] = row[0].load(Ordering::Acquire) as u32;
        masks[cpu] = row[1].load(Ordering::Acquire) as u32;
        if row[6].load(Ordering::Acquire) != 1 {
            serial::write(format_args!(
                "[NEU] CPU state unsupported: cpu={} actual={:#x} required={:#x}\n",
                cpu,
                features[cpu],
                policy::REQUIRED_EDX
            ))
            .map_err(|_| "CPU state refusal serial")?;
            return Err("unsupported x87/SSE2 CPU state baseline");
        }
    }
    with_state(0, |s| {
        s.cpu_state
            .configure(&masks[..smp::cpu_count()], &features[..smp::cpu_count()])
    })
    .map_err(|_| "incompatible CPU state baseline")?;
    serial::write(format_args!(
        "[NEU] CPU state enabled: x87=1 sse2=1 eager=1 tls=FS_BASE cpus={}\n",
        smp::cpu_count()
    ))
    .map_err(|_| "CPU state configuration serial")
}
pub(super) fn snapshot(s: &State) {
    let values = [
        1,
        ptr::from_ref(&s.cpu_state.slots) as u64,
        ptr::from_ref(&s.cpu_state.mask) as u64,
        ptr::from_ref(&s.cpu_state.cpu_count) as u64,
        ptr::from_ref(&s.cpu_state) as u64,
        u64::from(s.cpu_state.mask),
        s.cpu_state.slots.iter().filter(|v| v.live != 0).count() as u64,
        s.cpu_state.cpu_count as u64,
    ];
    publish(&neu_cpu_state_storage, values);
}
pub(super) fn load(cpu: usize, active: ActiveContext) {
    let (image, fs_base) = with_state(cpu, |s| {
        s.cpu_state
            .load(cpu, active)
            .expect("CPU state dispatch owner")
    });
    let local = &neu_smp_sched_cpus[cpu];
    // SAFETY: Verified CPU owns these buffers and no user slice is active.
    unsafe {
        ptr::write_volatile(local.user_fx.get(), image);
        let bridge = local.bridge.get().cast::<u64>();
        ptr::write_volatile(bridge.add(16), 1);
        ptr::write_volatile(bridge.add(19), fs_base);
        ptr::write_volatile(bridge.add(20), local.kernel_fx.get() as u64);
        ptr::write_volatile(bridge.add(21), local.user_fx.get() as u64);
    }
}
pub(super) fn save(cpu: usize, active: ActiveContext) {
    let local = &neu_smp_sched_cpus[cpu];
    // SAFETY: Assembly captured this CPU's departing slice before Rust and
    // restored its kernel state. No other CPU can write these buffers.
    let image = unsafe { ptr::read_volatile(local.user_fx.get()) };
    let base = unsafe { ptr::read_volatile(local.bridge.get().cast::<u64>().add(19)) };
    with_state(cpu, |s| {
        s.cpu_state
            .save(cpu, active, image, base)
            .expect("CPU state boundary owner")
    });
}
fn record(cpu: usize, index: usize, vector: u64, entry: bool) {
    let local = &neu_smp_sched_cpus[cpu];
    let row = if entry {
        &neu_cpu_state_entries[cpu][index]
    } else {
        &neu_cpu_state_traces[cpu][index]
    };
    // SAFETY: These images and bridge words belong to this logical CPU.
    unsafe {
        ptr::write_volatile(row.image.get(), ptr::read_volatile(local.user_fx.get()));
    }
    if !entry {
        snapshot_fx(row.kernel.get());
    }
    let bridge = local.bridge.get().cast::<u64>();
    let mut words = [0; 16];
    words[..6].copy_from_slice(&[
        local.get(field::ENTRIES),
        local.get(field::THREAD),
        local.get(field::NODE),
        local.get(field::ROOT),
        cpu as u64,
        vector,
    ]);
    unsafe {
        words[6] = ptr::read_volatile(bridge.add(19));
        words[7] = ptr::read_volatile(bridge.add(18));
        words[9] = ptr::read_volatile(bridge.add(4));
        words[11] = ptr::read_volatile(bridge.add(17));
    }
    words[8] = rdmsr(0xc0000100);
    words[10] = smp::cr0();
    words[12] = cr4();
    words[13] = local.user_fx.get() as u64;
    words[14] = if entry { 1 } else { 2 };
    words[15] = 1;
    publish(&row.words, words);
}
pub(super) fn entry(cpu: usize, index: usize) {
    record(cpu, index, 0, true);
}
pub(super) fn trace(cpu: usize, index: usize, vector: u64) {
    record(cpu, index, vector, false);
}
pub(super) fn scrub(cpu: usize) {
    let local = &neu_smp_sched_cpus[cpu];
    snapshot_fx(local.final_fx.get());
    // SAFETY: No user slice remains on this CPU; clear all live transport data.
    unsafe {
        ptr::write_volatile(local.user_fx.get(), FxImage::zero());
        ptr::write_volatile(local.bridge.get().cast::<u64>().add(19), 0);
    }
    assert_eq!(
        rdmsr(0xc0000100),
        neu_cpu_state_features[cpu][2].load(Ordering::Acquire)
    );
    assert_eq!(
        u64::from(fs_selector()),
        neu_cpu_state_features[cpu][3].load(Ordering::Acquire)
    );
    assert_eq!(
        smp::cr0(),
        neu_cpu_state_features[cpu][4].load(Ordering::Acquire)
    );
    assert_eq!(
        cr4(),
        neu_cpu_state_features[cpu][5].load(Ordering::Acquire)
    );
}
#[cfg(not(feature = "device-test"))]
pub(super) fn action(slot: usize) -> u64 {
    if cfg!(feature = "test-cpu-state-x87-fault")
        || cfg!(feature = "test-cpu-state-sse-status")
        || cfg!(feature = "test-cpu-state-avx-denied")
        || cfg!(feature = "test-cpu-state-fsgsbase-denied")
    {
        if slot == 0 {
            return 6;
        }
        if slot == 1 {
            return 5;
        }
        return if cfg!(feature = "test-cpu-state-x87-fault") {
            40
        } else if cfg!(feature = "test-cpu-state-sse-status") {
            41
        } else if cfg!(feature = "test-cpu-state-avx-denied") {
            42
        } else {
            43
        };
    }
    if cfg!(feature = "test-cpu-state-negative") {
        return 50;
    }
    if cfg!(feature = "test-cpu-state-blocked") || cfg!(feature = "test-cpu-state-blocked-4") {
        return match slot {
            0 => 2,
            1 => 3,
            _ => 0,
        };
    }
    0
}

//! Initial conventional SMP scheduler. Each CPU runs the same scheduling loop;
//! a short IRQ-disabled lock protects shared Nodes, queues, capabilities and
//! Channels. CPU-local IRQ code copies frames and acknowledges its own LAPIC;
//! it never borrows shared mutable state or acquires the scheduler lock.
use super::{
    exceptions::{self, ExceptionFrame},
    paging::{self, Paging, SmpInspection, UserSpace},
    serial, smp,
};
use core::{
    arch::{asm, global_asm},
    cell::UnsafeCell,
    mem::{offset_of, size_of},
    ptr,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};
use neu_kernel::{
    capability::{self as cap, Handle, ObjectType, Objects, Rights, Settle, WaitStart},
    context::{Dispatch, SavedContext, StackBinding},
    cpu::{self, Bootstrap},
    node::{
        self, ActiveContext, ExitReason, Fault, FaultSource, NodeId, Nodes, ThreadContext,
        ThreadId, ThreadState,
    },
    paging::{self as logic, PAGE, USER_CODE},
    shootdown::Shootdown,
    smp_scheduler::Scheduler,
    sync::SpinLock,
    syscall::{self as abi, Frame, Outcome},
};
global_asm!(".set NEU_DEVICE, {device}", ".set NEU_CPU_STATE, {extended}", ".set NEU_ABI08_DISABLED, {disabled}", include_str!("smp_scheduler.S"), device = const cfg!(feature = "device-test") as u8, extended = const cfg!(feature = "cpu-state-test") as u8, disabled = const cfg!(feature = "test-smp-dma-disabled") as u8);
#[cfg(feature = "cpu-state-test")]
#[path = "cpu_state.rs"]
mod extended;
#[cfg(all(feature = "cpu-state-test", not(feature = "device-test")))]
global_asm!(include_str!("cpu_state.S"));
#[cfg(all(feature = "device-test", not(feature = "dma-test")))]
global_asm!(".set NEU_DMA_DISABLED, {disabled}", include_str!("smp_device.S"), disabled = const cfg!(feature = "test-smp-device-dma-disabled") as u8);
#[cfg(feature = "dma-test")]
global_asm!(include_str!("smp_dma.S"));
#[cfg(feature = "authority-test")]
#[path = "smp_authority.rs"]
mod authority;
#[cfg(feature = "device-test")]
#[path = "smp_device.rs"]
mod devices;
const MAGIC: u64 = 0x4e45_555f_5343_4831;
const WORDS: usize = 64;
const BRIDGE_WORDS: usize = if cfg!(feature = "cpu-state-test") {
    32
} else {
    16
};
// DMA retains every AP continuation through up to 16 transfers per claim and
// masked scrub/IRQ windows. K11 and earlier fixtures retain their own bounds.
const TRACE_LIMIT: usize = if cfg!(feature = "dma-test") {
    2048
} else if cfg!(feature = "device-test") {
    512
} else {
    256
};
const TIMER: u8 = neu_kernel::timer::SMP_TIMER_VECTOR;
const IPI: u8 = 241;
const WAIT_LIMIT: usize = 100_000_000;
const READY: u64 = 1;
const KERNEL: u64 = 2;
const USER: u64 = 3;
const IDLE: u64 = 4;
const STOPPED: u64 = 5;
const FAILED: u64 = 6;
#[cfg(not(all(feature = "cpu-state-test", not(feature = "device-test"))))]
const FAULT_PROFILE: bool = cfg!(feature = "test-smp-schedule-fault");
#[cfg(not(all(feature = "cpu-state-test", not(feature = "device-test"))))]
const REJECT_PROFILE: bool = cfg!(feature = "test-smp-schedule-reject");
const TLB_PROFILE: bool = cfg!(feature = "test-smp-tlb-2")
    || cfg!(feature = "test-smp-tlb-4")
    || cfg!(feature = "test-smp-tlb-invalid");
#[cfg(not(all(feature = "cpu-state-test", not(feature = "device-test"))))]
const IPC_PROFILE: bool =
    cfg!(feature = "test-smp-schedule-idle") || cfg!(feature = "test-smp-schedule-ipc");
const MIGRATE: bool = cfg!(feature = "test-smp-schedule-migrate")
    || cfg!(feature = "test-smp-schedule-stress")
    || cfg!(feature = "test-cpu-state-migrate")
    || cfg!(feature = "test-cpu-state-stress");
const TICKS: u64 =
    if cfg!(feature = "test-smp-schedule-stress") || cfg!(feature = "test-cpu-state-stress") {
        90
    } else {
        40
    };
const EXECUTION_PROBES: bool =
    !cfg!(feature = "test-smp-schedule-stress") && !cfg!(feature = "test-cpu-state-stress");
pub mod field {
    pub const PHASE: usize = 3;
    pub const THREAD: usize = 4;
    pub const NODE: usize = 5;
    pub const ROOT: usize = 7;
    pub const KERNEL_ROOT: usize = 8;
    pub const BOTTOM: usize = 9;
    pub const TOP: usize = 10;
    pub const ENTRIES: usize = 11;
    pub const TICKS: usize = 12;
    pub const TIMER_EOIS: usize = 13;
    pub const IPIS: usize = 14;
    pub const IPI_EOIS: usize = 15;
    pub const SYSCALLS: usize = 16;
    pub const FAULTS: usize = 17;
    pub const HANDLER: usize = 18;
    pub const VECTOR: usize = 19;
    pub const CR2: usize = 20;
    pub const EXITS: usize = 21;
    pub const SENT: usize = 22;
    pub const GENERATION: usize = 23;
    pub const DRAIN: usize = 24;
    pub const TRACE_COUNT: usize = 25;
    pub const TLB_EPOCH: usize = 32;
    pub const TLB_ADDRESS: usize = 33;
    pub const TLB_ROOT: usize = 34;
    pub const TLB_MODE: usize = 35;
    pub const TLB_ACK: usize = 36;
    pub const FAILURE: usize = 37;
    pub const GS: usize = 38;
    pub const CR0: usize = 39;
    pub const EFER: usize = 40;
    pub const TSS: usize = 45;
    pub const IDT: usize = 46;
    pub const GDT: usize = 47;
    pub const IDLES: usize = 48;
    pub const RESCHED: usize = 49;
    pub const IRQ_PROBE: usize = 51;
    pub const IRQ_FRAME: usize = 52;
    pub const FAULT_PROBE: usize = 53;
    pub const TLB_PROBE: usize = 54;
    pub const REJECTED_RETURNS: usize = 55;
    pub const NATIVE_PROBE: usize = 61;
    pub const NATIVE_FRAME: usize = 62;
}
#[repr(C, align(64))]
struct Cpu {
    words: [AtomicU64; WORDS],
    frame: UnsafeCell<Frame>,
    bridge: UnsafeCell<[u64; BRIDGE_WORDS]>,
    #[cfg(feature = "cpu-state-test")]
    kernel_fx: UnsafeCell<neu_kernel::cpu_state::FxImage>,
    #[cfg(feature = "cpu-state-test")]
    user_fx: UnsafeCell<neu_kernel::cpu_state::FxImage>,
    #[cfg(feature = "cpu-state-test")]
    baseline_fx: UnsafeCell<neu_kernel::cpu_state::FxImage>,
    #[cfg(feature = "cpu-state-test")]
    final_fx: UnsafeCell<neu_kernel::cpu_state::FxImage>,
}
// SAFETY: A verified logical CPU is the sole writer of its frame and bridge.
// IRQ entry and the resumed controller exchange owned copies, never references.
// Cross-CPU readers use atomics or wait for STOPPED; QMP freezes the whole VM.
unsafe impl Sync for Cpu {}
impl Cpu {
    const fn new() -> Self {
        Self {
            words: [const { AtomicU64::new(0) }; WORDS],
            frame: UnsafeCell::new(Frame {
                registers: abi::Registers {
                    r15: 0,
                    r14: 0,
                    r13: 0,
                    r12: 0,
                    r11: 0,
                    r10: 0,
                    r9: 0,
                    r8: 0,
                    rdi: 0,
                    rsi: 0,
                    rbp: 0,
                    rdx: 0,
                    rcx: 0,
                    rbx: 0,
                    rax: 0,
                },
                context: abi::ReturnContext {
                    rip: 0,
                    cs: 0,
                    flags: 0,
                    rsp: 0,
                    ss: 0,
                },
            }),
            bridge: UnsafeCell::new([0; BRIDGE_WORDS]),
            #[cfg(feature = "cpu-state-test")]
            kernel_fx: UnsafeCell::new(neu_kernel::cpu_state::FxImage::zero()),
            #[cfg(feature = "cpu-state-test")]
            user_fx: UnsafeCell::new(neu_kernel::cpu_state::FxImage::zero()),
            #[cfg(feature = "cpu-state-test")]
            baseline_fx: UnsafeCell::new(neu_kernel::cpu_state::FxImage::zero()),
            #[cfg(feature = "cpu-state-test")]
            final_fx: UnsafeCell::new(neu_kernel::cpu_state::FxImage::zero()),
        }
    }
    fn get(&self, index: usize) -> u64 {
        self.words[index].load(Ordering::Acquire)
    }
    fn set(&self, index: usize, value: u64) {
        self.words[index].store(value, Ordering::Release);
    }
    fn inc(&self, index: usize) {
        self.words[index].fetch_add(1, Ordering::Relaxed);
    }
}
#[unsafe(no_mangle)]
static neu_smp_sched_cpus: [Cpu; cpu::MAX_CPUS] = [const { Cpu::new() }; cpu::MAX_CPUS];
#[used]
#[unsafe(no_mangle)]
static neu_smp_sched_layout: [u64; 9] = [
    MAGIC,
    cpu::MAX_CPUS as u64,
    WORDS as u64,
    size_of::<Cpu>() as u64,
    offset_of!(Cpu, frame) as u64,
    offset_of!(Cpu, bridge) as u64,
    TRACE_LIMIT as u64,
    32,
    256,
];
#[used]
#[unsafe(no_mangle)]
static neu_smp_sched_value_layout: [u64; 22] = [
    1,
    size_of::<StackBinding>() as u64,
    offset_of!(StackBinding, thread) as u64,
    offset_of!(StackBinding, space) as u64,
    offset_of!(StackBinding, root) as u64,
    offset_of!(StackBinding, bottom) as u64,
    offset_of!(StackBinding, top) as u64,
    size_of::<SavedContext>() as u64,
    offset_of!(SavedContext, active) as u64,
    offset_of!(SavedContext, stack) as u64,
    offset_of!(SavedContext, frame) as u64,
    offset_of!(SavedContext, fresh) as u64,
    offset_of!(SavedContext, preemptible) as u64,
    offset_of!(SavedContext, interrupted) as u64,
    offset_of!(SavedContext, continuation) as u64,
    offset_of!(ActiveContext, node) as u64,
    offset_of!(ActiveContext, thread) as u64,
    offset_of!(ActiveContext, context) as u64,
    offset_of!(ThreadContext, space) as u64,
    offset_of!(ThreadContext, root) as u64,
    offset_of!(ThreadContext, entry) as u64,
    offset_of!(ThreadContext, rsp) as u64,
];
#[repr(C)]
struct Trace([AtomicU64; 32]);
impl Trace {
    const fn new() -> Self {
        Self([const { AtomicU64::new(0) }; 32])
    }
}
#[unsafe(no_mangle)]
static neu_smp_sched_traces: [[Trace; TRACE_LIMIT]; cpu::MAX_CPUS] =
    [const { [const { Trace::new() }; TRACE_LIMIT] }; cpu::MAX_CPUS];
#[unsafe(no_mangle)]
static neu_smp_sched_entries: [[Trace; TRACE_LIMIT]; cpu::MAX_CPUS] =
    [const { [const { Trace::new() }; TRACE_LIMIT] }; cpu::MAX_CPUS];
#[unsafe(no_mangle)]
static neu_smp_sched_threads: [[AtomicU64; 32]; node::MAX_THREADS] =
    [const { [const { AtomicU64::new(0) }; 32] }; node::MAX_THREADS];
#[unsafe(no_mangle)]
static neu_smp_sched_evidence: [AtomicU64; 64] = [const { AtomicU64::new(0) }; 64];
#[unsafe(no_mangle)]
static neu_smp_sched_inventory: [AtomicU64; 64] = [const { AtomicU64::new(0) }; 64];
#[unsafe(no_mangle)]
static neu_smp_sched_queues: [AtomicU64; 72] = [const { AtomicU64::new(0) }; 72];
#[unsafe(no_mangle)]
static neu_smp_sched_contexts: [AtomicU64; 36] = [const { AtomicU64::new(0) }; 36];
#[unsafe(no_mangle)]
static neu_smp_sched_authority: [AtomicU64; 128] = [const { AtomicU64::new(0) }; 128];
#[unsafe(no_mangle)]
static neu_smp_sched_tlb: [AtomicU64; 16] = [const { AtomicU64::new(0) }; 16];
#[unsafe(no_mangle)]
static neu_smp_sched_negatives: [AtomicU64; 176] = [const { AtomicU64::new(0) }; 176];
static RELEASE: AtomicBool = AtomicBool::new(false);
static DONE: AtomicBool = AtomicBool::new(false);
static QUIESCENT: AtomicU64 = AtomicU64::new(0);
static BAR_PA: AtomicU64 = AtomicU64::new(0);
static LAPIC_PA: AtomicU64 = AtomicU64::new(0);
static LAPIC_PAT: AtomicU64 = AtomicU64::new(0);
static INITIAL: AtomicU64 = AtomicU64::new(0);
static TLB_EPOCH: AtomicU64 = AtomicU64::new(0);
static TLB_ROOT: AtomicU64 = AtomicU64::new(0);
static TLB_ADDRESS: AtomicU64 = AtomicU64::new(0);
static TLB_TARGETS: AtomicU64 = AtomicU64::new(0);
static TLB_ACKS: AtomicU64 = AtomicU64::new(0);
struct State {
    #[cfg(feature = "cpu-state-test")]
    cpu_state: neu_kernel::cpu_state::Contexts<{ node::MAX_THREADS }>,
    nodes: Nodes,
    scheduler: Scheduler,
    objects: Objects,
    threads: [Option<ThreadId>; 3],
    node_ids: [Option<NodeId>; 2],
    inspections: [Option<SmpInspection>; 2],
    data: [u64; 3],
    entries: [u64; 3],
    preemptions: [u64; 3],
    first_cpu: [u64; 3],
    last_cpu: [u64; 3],
    moved: bool,
    concurrent: bool,
    released: bool,
    tlb: Shootdown,
    #[cfg(feature = "authority-test")]
    authority: authority::Controller,
    #[cfg(feature = "device-test")]
    device: devices::Controller,
}
impl State {
    const fn new() -> Self {
        Self {
            #[cfg(feature = "cpu-state-test")]
            cpu_state: neu_kernel::cpu_state::Contexts::new(),
            nodes: Nodes::new(),
            scheduler: Scheduler::empty(),
            objects: {
                #[cfg(feature = "device-test")]
                {
                    Objects::with_devices()
                }
                #[cfg(not(feature = "device-test"))]
                {
                    Objects::with_channels()
                }
            },
            threads: [None; 3],
            node_ids: [None; 2],
            inspections: [const { None }; 2],
            data: [0; 3],
            entries: [0; 3],
            preemptions: [0; 3],
            first_cpu: [u64::MAX; 3],
            last_cpu: [u64::MAX; 3],
            moved: false,
            concurrent: false,
            released: false,
            tlb: Shootdown::new(),
            #[cfg(feature = "authority-test")]
            authority: authority::Controller::new(),
            #[cfg(feature = "device-test")]
            device: devices::Controller::new(),
        }
    }
    fn snapshot(&self) {
        #[cfg(feature = "cpu-state-test")]
        extended::snapshot(self);
        #[cfg(feature = "device-test")]
        devices::snapshot(self);
        #[cfg(feature = "authority-test")]
        authority::snapshot(self);
        publish(&neu_smp_sched_queues, self.scheduler.diagnostic_layout());
        publish(&neu_smp_sched_contexts, self.nodes.smp_cpu_addresses());
        let mut tlb = [0; 16];
        tlb[0] = 1;
        tlb[1..8].copy_from_slice(&self.tlb.diagnostic_layout());
        tlb[8..16].copy_from_slice(&[
            ptr::from_ref(&TLB_EPOCH) as u64,
            ptr::from_ref(&TLB_ROOT) as u64,
            ptr::from_ref(&TLB_ADDRESS) as u64,
            ptr::from_ref(&TLB_TARGETS) as u64,
            ptr::from_ref(&TLB_ACKS) as u64,
            ptr::from_ref(&INITIAL) as u64,
            ptr::from_ref(&LAPIC_PA) as u64,
            ptr::from_ref(&LAPIC_PAT) as u64,
        ]);
        publish(&neu_smp_sched_tlb, tlb);
        let mut authority = [0; 128];
        authority[..18].copy_from_slice(&self.objects.diagnostic_layout());
        for (index, id) in self.node_ids.iter().enumerate() {
            if let Some(id) = id
                && let Ok(table) = self.nodes.authority(*id)
            {
                authority[24 + index * 20..44 + index * 20]
                    .copy_from_slice(&table.diagnostic_layout());
            }
        }
        authority[64..96].copy_from_slice(&self.objects.channel_state().diagnostic_layout());
        authority[96..120].copy_from_slice(&self.objects.smp_wait_addresses());
        authority[120..128].copy_from_slice(&self.nodes.smp_node_addresses());
        publish(&neu_smp_sched_authority, authority);
        for (slot, id) in self.threads.iter().copied().enumerate() {
            if let Some(id) = id {
                let t = self.nodes.thread(id).expect("SMP snapshot identity");
                let addresses = self
                    .nodes
                    .smp_debug_addresses(id)
                    .expect("SMP snapshot addresses");
                let (outcome, value, cr2) = match t.outcome {
                    Some(ExitReason::Normal(v)) => (1, v as u64, 0),
                    Some(ExitReason::UserFault(f)) => (2, f.vector, f.cr2),
                    Some(ExitReason::Cancelled { by }) => (3, by.raw(), 0),
                    _ => (0, 0, 0),
                };
                let mut row = [0u64; 32];
                row[..11].copy_from_slice(&[
                    id.raw(),
                    t.node.raw(),
                    t.context.root,
                    t.context.rsp,
                    self.entries[slot],
                    self.preemptions[slot],
                    u64::from(matches!(
                        t.state,
                        ThreadState::Exited | ThreadState::Reclaimed
                    )),
                    outcome,
                    value,
                    cr2,
                    self.data[slot],
                ]);
                row[11..23].copy_from_slice(&addresses[..12]);
                row[23] = self.first_cpu[slot];
                row[24] = self.last_cpu[slot];
                row[25] = self.scheduler.owner(id).map_or(u64::MAX, |v| v as u64);
                row[26..32].copy_from_slice(&addresses[12..]);
                for (cell, value) in neu_smp_sched_threads[slot].iter().zip(row) {
                    cell.store(value, Ordering::Release);
                }
            }
        }
        neu_smp_sched_evidence[12].store(self.scheduler.dispatches, Ordering::Release);
        neu_smp_sched_evidence[13].store(self.scheduler.preemptions, Ordering::Release);
        neu_smp_sched_evidence[14].store(self.scheduler.blocks, Ordering::Release);
        neu_smp_sched_evidence[15].store(self.scheduler.wakes, Ordering::Release);
        neu_smp_sched_evidence[16].store(self.scheduler.migrations, Ordering::Release);
        neu_smp_sched_evidence[17].store(self.scheduler.completions, Ordering::Release);
        neu_smp_sched_evidence[18].store(self.objects.stats().references as u64, Ordering::Release);
        neu_smp_sched_evidence[19].store(self.objects.stats().pins as u64, Ordering::Release);
        neu_smp_sched_evidence[20]
            .store(self.objects.wait_stats().waiting as u64, Ordering::Release);
        for cpu in 0..self.scheduler.cpu_count() {
            let q = self.scheduler.queued(cpu);
            for (offset, id) in q.into_iter().enumerate() {
                neu_smp_sched_evidence[24 + cpu * 3 + offset].store(id, Ordering::Release);
            }
        }
    }
    fn ready_waits(&mut self) -> u64 {
        let mut mask = 0;
        #[cfg(feature = "device-test")]
        {
            mask |= devices::take_wakes(self);
        }
        for waiter in self.objects.take_ready_waits().into_iter().flatten() {
            let (status, value) = match self.objects.settle_wait(waiter.active.thread) {
                Settle::Committed(value) => (0, value),
                Settle::Failed(error) => (error as i64, 0),
                Settle::Retry => continue,
            };
            self.nodes
                .settle_blocked_smp(waiter.active.thread, status, value)
                .expect("SMP wait result");
            let cpu = self
                .scheduler
                .wake(&mut self.nodes, waiter.active.thread)
                .expect("SMP wait wake");
            mask |= 1 << cpu;
        }
        mask
    }
    fn terminal(&self) -> bool {
        self.threads.iter().flatten().all(|id| {
            self.nodes
                .thread(*id)
                .is_ok_and(|t| t.state == ThreadState::Exited)
        })
    }
    fn finish(
        &mut self,
        cpu: usize,
        active: ActiveContext,
        outcome: ExitReason,
    ) -> node::Result<u64> {
        self.scheduler
            .finish(&mut self.nodes, cpu, active, outcome)?;
        #[cfg(feature = "cpu-state-test")]
        for id in self.threads.into_iter().flatten() {
            let thread = self.nodes.thread(id)?;
            if thread.state == ThreadState::Exited && self.cpu_state.slots[id.index()].live != 0 {
                self.cpu_state
                    .retire(ActiveContext {
                        node: thread.node,
                        thread: id,
                        context: thread.context,
                    })
                    .expect("CPU state retirement after all CPU owners release");
            }
        }
        if self.nodes.node(active.node)?.state == node::NodeState::Exited {
            #[cfg(feature = "device-test")]
            devices::node_exit(self, active.node);
            self.objects.cancel_node_waits(active.node);
            self.nodes
                .close_authority_smp(active.node, &mut self.objects)?;
        }
        let wakes = self.ready_waits();
        if self.terminal() {
            DONE.store(true, Ordering::Release);
        }
        Ok(wakes)
    }
}
#[unsafe(no_mangle)]
static neu_smp_sched_state: SpinLock<State> = SpinLock::new(State::new());
fn publish<const N: usize>(destination: &[AtomicU64; N], values: [u64; N]) {
    for (cell, value) in destination.iter().zip(values) {
        cell.store(value, Ordering::Release);
    }
}
fn inventory(paging: &Paging<'_, '_>) {
    let mut layout = paging.smp_inventory_layout();
    layout[56..59].copy_from_slice(&neu_smp_sched_state.diagnostic_layout());
    publish(&neu_smp_sched_inventory, layout);
}
unsafe extern "C" {
    fn neu_smp_sched_enter(frame: *const Frame, root: u64, bridge: *mut u64, kernel_root: u64);
    fn neu_smp_sched_resume(bridge: *mut u64) -> !;
    fn neu_smp_sched_idle(generation: *const AtomicU64, observed: u64);
    fn neu_smp_sched_drain();
    fn neu_smp_sched_probe_wait(timer: *const u32);
    fn neu_smp_sched_invalidate(address: u64, root: u64, mode: u64, timer: *const u32, epoch: u64);
    static neu_smp_sched_payload: u8;
    static neu_smp_sched_payload_end: u8;
    static neu_smp_sched_barrier: u8;
}
pub(super) fn flags() -> u64 {
    let f: u64;
    unsafe {
        asm!("pushfq","pop {}",out(reg) f,options(preserves_flags));
    }
    f
}
fn rdmsr(msr: u32) -> u64 {
    let (a, d): (u32, u32);
    unsafe {
        asm!("rdmsr",in("ecx") msr,out("eax") a,out("edx") d,options(nomem,nostack,preserves_flags));
    }
    u64::from(a) | (u64::from(d) << 32)
}
fn read(offset: u32) -> u32 {
    unsafe { ptr::read_volatile((paging::LAPIC_VIEW + u64::from(offset)) as *const u32) }
}
fn write(offset: u32, value: u32) {
    unsafe {
        ptr::write_volatile((paging::LAPIC_VIEW + u64::from(offset)) as *mut u32, value);
    }
}
fn bitmap(base: u32, vector: u8) -> bool {
    read(base + u32::from(vector / 32) * 16) & (1 << (vector % 32)) != 0
}
fn empty(base: u32) -> bool {
    (0..8).all(|i| read(base + i * 16) == 0)
}
fn current() -> usize {
    // User entry clears GS; IRQ/native exit identifies the processor from its
    // hardware APIC identity before reading any CPU-local bridge storage.
    smp::logical_by_apic(smp::apic_id()).expect("SMP verified logical CPU")
}
fn with_state<R>(cpu: usize, work: impl FnOnce(&mut State) -> R) -> R {
    if flags() & 0x600 != 0 {
        reject(cpu, 1);
    }
    let mut guard = neu_smp_sched_state
        .lock_bounded(cpu as u64 + 1, WAIT_LIMIT)
        .unwrap_or_else(|| reject(cpu, 2));
    let result = work(&mut guard);
    guard.snapshot();
    result
}
fn reject(cpu: usize, code: u64) -> ! {
    neu_smp_sched_cpus[cpu].set(field::FAILURE, code);
    neu_smp_sched_cpus[cpu].set(field::PHASE, FAILED);
    if LAPIC_PA.load(Ordering::Acquire) != 0 {
        write(0x320, (1 << 16) | TIMER as u32);
        write(0x380, 0);
    }
    if cpu == 0 {
        serial::emergency(format_args!(
            "[NEU] SMP scheduler failure: cpu={cpu} code={code}\n"
        ));
        super::halt();
    }
    smp::ap_fail_index(cpu, code)
}
fn barrier_word(index: usize) -> &'static AtomicU64 {
    // SAFETY: Dedicated image-owned, page-aligned shared fixture storage.
    unsafe {
        &*(ptr::addr_of!(neu_smp_sched_barrier)
            .cast::<AtomicU64>()
            .add(index))
    }
}
pub(super) fn barrier_physical() -> u64 {
    BAR_PA.load(Ordering::Acquire)
}
#[cfg(feature = "device-test")]
pub(super) fn peripheral() -> u64 {
    devices::physical()
}
pub(super) fn device() -> (u64, u64) {
    (
        LAPIC_PA.load(Ordering::Acquire),
        LAPIC_PAT.load(Ordering::Acquire),
    )
}
fn output(port: u16, value: u8) {
    unsafe {
        asm!("out dx, al",in("dx") port,in("al") value,options(nostack,preserves_flags));
    }
}
fn input(port: u16) -> u8 {
    let value: u8;
    unsafe {
        asm!("in al, dx",in("dx") port,out("al") value,options(nomem,nostack,preserves_flags));
    }
    value
}
pub fn map_lapic(paging: &mut Paging<'_, '_>) -> Result<(), &'static str> {
    let physical = rdmsr(0x1b) & logic::ADDRESS_FIELD;
    let pat = rdmsr(0x277);
    paging
        .map_lapic_uc(physical, pat)
        .map_err(|_| "SMP scheduler LAPIC map")?;
    LAPIC_PA.store(physical, Ordering::Release);
    LAPIC_PAT.store(pat, Ordering::Release);
    output(0x21, 0xff);
    output(0xa1, 0xff);
    Ok(())
}
fn calibrate() -> Result<(), &'static str> {
    let mut samples = [0; 3];
    write(0x320, (1 << 16) | TIMER as u32);
    write(0x3e0, 3);
    for sample in &mut samples {
        let control = input(0x61) & 0x0c;
        output(0x61, control);
        output(0x43, 0xb0);
        output(0x42, neu_kernel::timer::REFERENCE_TICKS as u8);
        output(0x42, (neu_kernel::timer::REFERENCE_TICKS >> 8) as u8);
        write(0x380, u32::MAX);
        output(0x61, control | 1);
        let mut complete = false;
        for _ in 0..10_000_000 {
            if input(0x61) & 0x20 != 0 {
                complete = true;
                break;
            }
            core::hint::spin_loop();
        }
        *sample = u32::MAX - read(0x390);
        write(0x380, 0);
        output(0x61, control);
        if !complete {
            return Err("SMP scheduler PIT calibration timeout");
        }
    }
    for (index, sample) in samples.iter().enumerate() {
        neu_smp_sched_evidence[53 + index].store(u64::from(*sample), Ordering::Release);
    }
    let calibration = neu_kernel::timer::Configuration::smp(10_000, TIMER)
        .and_then(|c| c.calibrate(samples, neu_kernel::timer::REFERENCE_TICKS))
        .map_err(|_| "SMP scheduler inconsistent timer calibration")?;
    INITIAL.store(calibration.initial as u64, Ordering::Release);
    neu_smp_sched_evidence[8].store(calibration.divided_hz, Ordering::Release);
    neu_smp_sched_evidence[9].store(calibration.initial as u64, Ordering::Release);
    Ok(())
}
fn probe_wait() {
    // A clock-bounded observation window avoids PAUSE iteration time depending
    // on how many TCG CPUs are parked. The local timer stays masked and IF=0.
    let initial = (INITIAL.load(Ordering::Acquire) as u32)
        .checked_mul(25)
        .expect("SMP observation timer bound");
    write(0x320, (1 << 16) | TIMER as u32);
    write(0x380, initial);
    unsafe {
        neu_smp_sched_probe_wait((paging::LAPIC_VIEW + 0x390) as *const u32);
    }
    write(0x380, 0);
}
fn enable_cpu(cpu: usize) {
    let block = &smp::neu_smp_cpus[cpu];
    if smp::apic_id() as u64 != block.load(cpu::block::APIC_ID) {
        reject(cpu, 3);
    }
    write(0x80, 0xff);
    write(0xf0, 0x1ff);
    for offset in [0x320, 0x330, 0x340, 0x350, 0x360, 0x370] {
        write(offset, read(offset) | (1 << 16));
    }
    write(0x380, 0);
    write(0x3e0, 3);
    write(0x80, 0);
    let local = &neu_smp_sched_cpus[cpu];
    local.set(0, MAGIC);
    local.set(1, cpu as u64);
    local.set(2, smp::apic_id() as u64);
    local.set(field::KERNEL_ROOT, paging::current_root());
    local.set(field::TSS, block.load(cpu::block::TSS_BASE));
    local.set(field::IDT, block.load(cpu::block::IDT_BASE));
    local.set(field::GDT, block.load(cpu::block::GDT_BASE));
    local.set(field::PHASE, KERNEL);
    block.store(cpu::block::SCHEDULER_LOCAL, ptr::from_ref(local) as u64);
    block.store(cpu::block::ISOLATION, cpu::block::ISOLATION_PRIVATE_TABLES);
}
fn send(cpu: usize, target: usize) {
    if flags() & 0x600 != 0 || neu_smp_sched_state.owner() == cpu as u64 + 1 {
        reject(cpu, 4);
    }
    if target >= smp::cpu_count() || target == cpu {
        return;
    }
    write(
        0x310,
        (smp::neu_smp_cpus[target].load(cpu::block::APIC_ID) as u32) << 24,
    );
    write(0x300, IPI as u32);
    for _ in 0..1_000_000 {
        if read(0x300) & (1 << 12) == 0 {
            neu_smp_sched_cpus[cpu].inc(field::SENT);
            return;
        }
        core::hint::spin_loop();
    }
    reject(cpu, 5)
}
fn notify(cpu: usize, mask: u64) {
    for (target, local) in neu_smp_sched_cpus.iter().enumerate().take(smp::cpu_count()) {
        if mask & (1 << target) != 0 {
            local.words[field::GENERATION].fetch_add(1, Ordering::Release);
            send(cpu, target);
        }
    }
}
fn frame(f: &ExceptionFrame) -> Frame {
    Frame {
        registers: unsafe { ptr::read(f.gpr.as_ptr().cast::<abi::Registers>()) },
        context: abi::ReturnContext {
            rip: f.rip,
            cs: f.cs,
            flags: f.flags,
            rsp: f.rsp,
            ss: f.ss,
        },
    }
}
fn trace(cpu: usize, f: ExceptionFrame, cr2: u64, address: u64, before: u64, after: u64) {
    let local = &neu_smp_sched_cpus[cpu];
    let index = local.get(field::TRACE_COUNT) as usize;
    if index >= TRACE_LIMIT {
        reject(cpu, 6);
    }
    let user = frame(&f);
    let mut words = [0; 32];
    words[..10].copy_from_slice(&[
        local.get(field::ENTRIES),
        local.get(field::THREAD),
        local.get(field::NODE),
        paging::current_root(),
        f.vector,
        cr2,
        address,
        cpu as u64,
        rdmsr(0xc0000102),
        f.error,
    ]);
    let raw = unsafe { core::slice::from_raw_parts(ptr::from_ref(&user).cast::<u64>(), 20) };
    words[10..30].copy_from_slice(raw);
    words[30] = before;
    words[31] = after;
    for (cell, value) in neu_smp_sched_traces[cpu][index].0.iter().zip(words) {
        cell.store(value, Ordering::Release);
    }
    #[cfg(feature = "cpu-state-test")]
    extended::trace(cpu, index, f.vector);
    local.set(field::TRACE_COUNT, index as u64 + 1);
}
fn stage(cpu: usize, f: ExceptionFrame, cr2: u64) -> ! {
    let local = &neu_smp_sched_cpus[cpu];
    if local.get(field::PHASE) != USER
        || local.get(field::THREAD) == 0
        || f.cs != abi::USER_CS
        || f.ss != abi::USER_SS
        || paging::current_root() != local.get(field::ROOT)
        || flags() & 0x600 != 0
    {
        reject(cpu, 7);
    }
    let owned = frame(&f);
    unsafe {
        ptr::write(local.frame.get(), owned);
    }
    local.set(field::VECTOR, f.vector);
    local.set(field::CR2, cr2);
    local.set(26, f.rip);
    local.set(27, f.rsp);
    local.set(28, f.flags);
    local.set(29, f.cs);
    local.set(30, f.ss);
    local.inc(field::EXITS);
    local.set(field::PHASE, KERNEL);
    unsafe { neu_smp_sched_resume(local.bridge.get().cast::<u64>()) }
}
#[unsafe(no_mangle)]
extern "C" fn neu_smp_sched_boundary(pointer: *const ExceptionFrame, cr2: u64) -> ! {
    let cpu = current();
    let local = &neu_smp_sched_cpus[cpu];
    let address = pointer as u64;
    if address != local.get(field::TOP) - 176 || address < local.get(field::BOTTOM) {
        reject(cpu, 8);
    }
    let f = unsafe { ptr::read(pointer) };
    local.inc(field::SYSCALLS);
    local.set(field::HANDLER, smp::rsp());
    trace(cpu, f, cr2, address, 0, 0);
    if EXECUTION_PROBES && local.get(field::SYSCALLS) == 1 && cpu < 2 {
        local.set(field::NATIVE_FRAME, address);
        local.set(field::NATIVE_PROBE, 1);
        probe_wait();
        local.set(field::NATIVE_PROBE, 2);
    }
    stage(cpu, f, cr2)
}
pub(super) fn fault(f: ExceptionFrame, cr2: u64, handler: u64) -> ! {
    let cpu = current();
    let local = &neu_smp_sched_cpus[cpu];
    if !(local.get(field::BOTTOM)..local.get(field::TOP)).contains(&handler) {
        reject(cpu, 9);
    }
    local.inc(field::FAULTS);
    local.set(field::HANDLER, handler);
    trace(cpu, f, cr2, handler, 0, 0);
    stage(cpu, f, cr2)
}
fn invalidate_local(cpu: usize) {
    let epoch = TLB_EPOCH.load(Ordering::Acquire);
    if epoch == 0
        || TLB_TARGETS.load(Ordering::Acquire) & (1 << cpu) == 0
        || neu_smp_sched_cpus[cpu].get(field::TLB_ACK) == epoch
    {
        return;
    }
    let root = TLB_ROOT.load(Ordering::Acquire);
    let address = TLB_ADDRESS.load(Ordering::Acquire);
    let actual = paging::current_root();
    let mode = if actual == root { 1 } else { 2 };
    let local = &neu_smp_sched_cpus[cpu];
    local.set(field::TLB_EPOCH, epoch);
    local.set(field::TLB_ADDRESS, address);
    local.set(field::TLB_ROOT, actual);
    local.set(field::TLB_MODE, mode);
    // Keep the ACK unpublished while QMP observes the processor immediately
    // after the actual instruction. An IPI may return to a running user slice,
    // so restore that slice's timer after the masked observation window.
    let timer = (read(0x320), read(0x390));
    write(0x320, (1 << 16) | TIMER as u32);
    write(0x380, (INITIAL.load(Ordering::Acquire) as u32) * 25);
    local.set(field::TLB_PROBE, 1);
    unsafe {
        neu_smp_sched_invalidate(
            address,
            actual,
            mode,
            (paging::LAPIC_VIEW + 0x390) as *const u32,
            epoch,
        );
    }
    write(0x320, timer.0);
    write(0x380, timer.1);
    local.set(field::TLB_PROBE, 2);
    local.set(field::TLB_ACK, epoch);
    TLB_ACKS.fetch_or(1 << cpu, Ordering::Release);
}
#[unsafe(no_mangle)]
extern "C" fn neu_smp_sched_interrupt(pointer: *const ExceptionFrame) {
    let cpu = current();
    let local = &neu_smp_sched_cpus[cpu];
    let address = pointer as u64;
    let f = unsafe { ptr::read(pointer) };
    if flags() & 0x600 != 0 || f.error != 0 {
        reject(cpu, 10);
    }
    if f.vector == 255 {
        if bitmap(0x100, 255) {
            reject(cpu, 11);
        }
        local.inc(50);
        return;
    }
    #[cfg(feature = "device-test")]
    if f.vector == neu_kernel::device::VECTOR as u64 {
        devices::irq(f, address);
        return;
    }
    if !matches!(f.vector, 224 | 241) || !bitmap(0x100, f.vector as u8) {
        reject(cpu, 12);
    }
    write(0xb0, 0);
    if bitmap(0x100, f.vector as u8) {
        reject(cpu, 13);
    }
    if f.vector == IPI as u64 {
        local.inc(field::IPIS);
        local.inc(field::IPI_EOIS);
        invalidate_local(cpu);
        if f.cs == abi::USER_CS {
            trace(cpu, f, 0, address, 1, 0);
            if local.get(field::RESCHED) != 0 {
                stage(cpu, f, 0);
            }
        }
        return;
    }
    local.inc(field::TIMER_EOIS);
    #[cfg(feature = "device-test")]
    devices::tick(cpu);
    if f.cs != abi::USER_CS {
        local.inc(field::DRAIN);
        return;
    }
    if address != local.get(field::TOP) - 176 || f.flags & 0x200 == 0 {
        reject(cpu, 14);
    }
    local.inc(field::TICKS);
    local.set(field::HANDLER, smp::rsp());
    trace(cpu, f, 0, address, 1, 0);
    if EXECUTION_PROBES && local.get(field::TICKS) == 1 && cpu < 2 {
        local.set(field::IRQ_FRAME, address);
        local.set(field::IRQ_PROBE, 1);
        probe_wait();
        local.set(field::IRQ_PROBE, 2);
    }
    stage(cpu, f, 0)
}
fn operation(s: &mut State, cpu: usize, active: ActiveContext, f: Frame) -> Outcome {
    let number = f.registers.rax;
    #[cfg(feature = "cpu-state-test")]
    if abi::v09::owns(number) {
        if !abi::v09::valid_arguments(number, f.registers.arguments()) {
            return Outcome::Return {
                status: abi::INVALID_ARGUMENT,
                scalar: 0,
            };
        }
        let scalar = if number == abi::v09::THREAD_TLS_SET {
            s.cpu_state
                .set_tls(active, f.registers.rdi)
                .expect("validated current Thread TLS");
            0
        } else {
            s.cpu_state
                .tls(active)
                .expect("current Thread TLS identity")
        };
        return Outcome::Return { status: 0, scalar };
    }
    #[cfg(feature = "device-test")]
    if abi::v07::owns(number)
        || abi::v06::owns(number)
        || number == abi::v02::CAP_CLOSE
        || (cfg!(feature = "dma-test") && abi::v08::owns(number))
    {
        return devices::operation(s, cpu, active, f);
    }
    #[cfg(feature = "authority-test")]
    if abi::v06::owns(number) {
        return authority::operation(s, cpu, active, f);
    }
    if number == abi::THREAD_EXIT {
        return abi::dispatch(number, f.registers.arguments());
    }
    if number == neu_kernel::scheduler::TEST_YIELD {
        return Outcome::Return {
            status: 0,
            scalar: 0,
        };
    }
    let mut lease = s
        .nodes
        .take_authority_on(cpu, active)
        .expect("SMP capability lease");
    let handle = Handle::from_raw(f.registers.rdi);
    let result = (|| -> cap::Result<Outcome> {
        let scalar = match number {
            abi::ABI_QUERY => {
                let previous = if cfg!(feature = "device-test") {
                    if cfg!(feature = "dma-test") {
                        abi::v08::INFO
                    } else {
                        abi::v07::INFO
                    }
                } else if cfg!(feature = "authority-test") {
                    abi::v06::INFO
                } else {
                    abi::v05::INFO
                };
                if cfg!(feature = "cpu-state-test") {
                    abi::v09::info(previous)
                } else {
                    previous
                }
            }
            abi::v02::BOOTSTRAP_CAP_GET => lease
                .table()
                .bootstrap_get(&s.objects, active.node, f.registers.rdi)?
                .raw(),
            abi::v02::CAP_CLOSE => {
                lease
                    .table_mut()
                    .close(&mut s.objects, active.node, handle)?;
                0
            }
            abi::v02::ADDRESS_SPACE_INSPECT => {
                let guard = lease.table().lookup(
                    &mut s.objects,
                    active.node,
                    handle,
                    ObjectType::AddressSpace,
                    Rights::INSPECT,
                )?;
                let (owner, reference) = guard.address_space()?;
                let index = s
                    .node_ids
                    .iter()
                    .position(|n| *n == Some(owner))
                    .ok_or(cap::Error::Owner)?;
                return Ok(
                    match s.inspections[index]
                        .as_ref()
                        .ok_or(cap::Error::Owner)?
                        .inspect(active, owner, reference, f.registers.rsi)
                    {
                        Ok(value) => Outcome::Return {
                            status: 0,
                            scalar: value,
                        },
                        Err(status) => Outcome::Return { status, scalar: 0 },
                    },
                );
            }
            abi::v05::CHANNEL_SEND_CAP => {
                lease.table_mut().send_cap(
                    &mut s.objects,
                    active.node,
                    handle,
                    Handle::from_raw(f.registers.rsi),
                )?;
                0
            }
            abi::v05::CHANNEL_RECEIVE_CAP => lease
                .table_mut()
                .receive_cap(&mut s.objects, active.node, handle)?
                .raw(),
            abi::v05::TEST_READ => lease
                .table()
                .lookup(
                    &mut s.objects,
                    active.node,
                    handle,
                    ObjectType::Flag,
                    Rights::READ,
                )?
                .read()?,
            op if abi::v03::owns(op) || abi::v04::owns(op) => {
                let send = op == abi::v03::CHANNEL_SEND || op == abi::v04::CHANNEL_SEND_WAIT;
                let mut guard = lease.table().lookup(
                    &mut s.objects,
                    active.node,
                    handle,
                    ObjectType::ChannelEndpoint,
                    if send { Rights::SEND } else { Rights::RECEIVE },
                )?;
                let result = match op {
                    abi::v03::CHANNEL_SEND => guard
                        .channel_send(f.registers.rsi)
                        .map(|_| WaitStart::Committed(0)),
                    abi::v03::CHANNEL_RECEIVE => guard.channel_receive().map(WaitStart::Committed),
                    abi::v04::CHANNEL_SEND_WAIT => guard.channel_send_wait(active, f.registers.rsi),
                    _ => guard.channel_receive_wait(active),
                }?;
                match result {
                    WaitStart::Committed(v) => v,
                    WaitStart::Blocked => return Ok(Outcome::Blocked),
                }
            }
            _ => {
                return Ok(Outcome::Return {
                    status: abi::NOT_SUPPORTED,
                    scalar: 0,
                });
            }
        };
        Ok(Outcome::Return { status: 0, scalar })
    })()
    .unwrap_or_else(|e| Outcome::Return {
        status: e as i64,
        scalar: 0,
    });
    s.nodes
        .restore_authority_on(cpu, lease)
        .expect("SMP capability lease restoration");
    #[cfg(feature = "authority-test")]
    authority::record(s, cpu, active, f, result, 0);
    result
}
fn enter(cpu: usize, dispatch: &Dispatch) {
    let local = &neu_smp_sched_cpus[cpu];
    let active = dispatch.active;
    let owned = dispatch.saved.frame;
    if !dispatch.saved.valid(active, dispatch.saved.stack)
        || neu_smp_sched_state.owner() == cpu as u64 + 1
        || paging::current_root() != local.get(field::KERNEL_ROOT)
    {
        reject(cpu, 15);
    }
    #[cfg(feature = "cpu-state-test")]
    extended::load(cpu, active);
    local.set(field::THREAD, active.thread.raw());
    local.set(field::NODE, active.node.raw());
    local.set(6, active.context.space.raw());
    local.set(field::ROOT, active.context.root);
    local.set(field::BOTTOM, dispatch.saved.stack.bottom);
    local.set(field::TOP, dispatch.saved.stack.top);
    local.inc(field::ENTRIES);
    let index = local.get(field::ENTRIES) as usize - 1;
    if index >= TRACE_LIMIT {
        reject(cpu, 6);
    }
    let mut entry = [0; 32];
    entry[..10].copy_from_slice(&[
        index as u64 + 1,
        active.thread.raw(),
        active.node.raw(),
        active.context.root,
        cpu as u64,
        u64::from(dispatch.saved.interrupted),
        dispatch.saved.stack.bottom,
        dispatch.saved.stack.top,
        rdmsr(0xc0000101),
        neu_smp_sched_state.owner(),
    ]);
    let raw = unsafe { core::slice::from_raw_parts(ptr::from_ref(&owned).cast::<u64>(), 20) };
    entry[10..30].copy_from_slice(raw);
    publish(&neu_smp_sched_entries[cpu][index].0, entry);
    #[cfg(feature = "cpu-state-test")]
    extended::entry(cpu, index);
    local.set(field::VECTOR, 0);
    unsafe {
        ptr::write(local.frame.get(), owned);
        ptr::write_volatile(
            local.bridge.get().cast::<u64>().add(1),
            dispatch.saved.stack.top,
        );
        ptr::write_unaligned(
            (local.get(field::TSS) + 4) as *mut u64,
            dispatch.saved.stack.top,
        );
    }
    local.set(field::PHASE, USER);
    write(0x320, TIMER as u32);
    write(0x380, INITIAL.load(Ordering::Acquire) as u32);
    unsafe {
        neu_smp_sched_enter(
            ptr::from_ref(&owned),
            active.context.root,
            local.bridge.get().cast::<u64>(),
            local.get(field::KERNEL_ROOT),
        );
    }
    write(0x320, (1 << 16) | TIMER as u32);
    write(0x380, 0);
    if paging::current_root() != local.get(field::KERNEL_ROOT)
        || local.get(field::PHASE) != KERNEL
        || rdmsr(0xc0000101) != smp::block_address(cpu)
        || rdmsr(0xc0000080) & 1 != 0
    {
        reject(cpu, 16);
    }
}
fn finish_fault(
    cpu: usize,
    active: ActiveContext,
    f: Frame,
    source: FaultSource,
    vector: u64,
    cr2: u64,
) {
    let fault = ExitReason::UserFault(Fault {
        source,
        vector,
        error: neu_smp_sched_traces[cpu]
            [neu_smp_sched_cpus[cpu].get(field::TRACE_COUNT) as usize - 1]
            .0[9]
            .load(Ordering::Acquire),
        rip: f.context.rip,
        rsp: f.context.rsp,
        cs: f.context.cs,
        ss: f.context.ss,
        flags: f.context.flags,
        cr2,
    });
    let (peers, cancelled) = with_state(cpu, |s| {
        if let Some(by) = s
            .scheduler
            .stopping_for(active)
            .filter(|by| *by != active.thread)
        {
            return (
                0,
                Some(
                    s.finish(cpu, active, ExitReason::Cancelled { by })
                        .expect("SMP competing fault cancellation"),
                ),
            );
        }
        #[cfg(feature = "device-test")]
        devices::stop_node(s, active.node);
        let peers = s
            .scheduler
            .begin_fault(&s.nodes, cpu, active)
            .expect("SMP fault origin");
        neu_smp_sched_evidence[56].store(peers, Ordering::Release);
        neu_smp_sched_evidence[57].store(active.thread.raw(), Ordering::Release);
        neu_smp_sched_evidence[58].store(cpu as u64, Ordering::Release);
        (peers, None)
    });
    if let Some(wakes) = cancelled {
        notify(cpu, wakes);
        return;
    }
    neu_smp_sched_cpus[cpu].set(field::FAULT_PROBE, 1);
    probe_wait();
    for (peer, local) in neu_smp_sched_cpus.iter().enumerate().take(smp::cpu_count()) {
        if peers & (1 << peer) != 0 {
            local.set(field::RESCHED, 1);
        }
    }
    notify(cpu, peers);
    let mut complete = false;
    for _ in 0..WAIT_LIMIT {
        let mask = with_state(cpu, |s| {
            s.scheduler
                .begin_fault(&s.nodes, cpu, active)
                .expect("SMP fault peers")
        });
        if mask == 0 {
            complete = true;
            break;
        }
        core::hint::spin_loop();
    }
    if !complete {
        reject(cpu, 17);
    }
    neu_smp_sched_cpus[cpu].set(field::FAULT_PROBE, 2);
    probe_wait();
    let wakes = with_state(cpu, |s| {
        neu_smp_sched_evidence[59].store(s.nodes.concurrent_count() as u64, Ordering::Release);
        s.finish(cpu, active, fault).expect("SMP fault completion")
    });
    neu_smp_sched_cpus[cpu].set(field::FAULT_PROBE, 3);
    barrier_word(4).store(1, Ordering::Release);
    notify(cpu, wakes);
}
fn handle(cpu: usize, dispatch: Dispatch) {
    let local = &neu_smp_sched_cpus[cpu];
    #[cfg(feature = "cpu-state-test")]
    extended::save(cpu, dispatch.active);
    let f = unsafe { ptr::read(local.frame.get()) };
    let vector = local.get(field::VECTOR);
    if !(matches!(vector, 128 | 224 | 241) || cfg!(feature = "device-test") && vector == 240) {
        finish_fault(
            cpu,
            dispatch.active,
            f,
            FaultSource::Cpu,
            vector,
            local.get(field::CR2),
        );
        return;
    }
    // Validate the owned user frame before an operation can change authority,
    // Channels or waiters. A user-controlled RSP/flags violation follows the
    // same sibling-stop protocol as a CPU fault; it cannot panic the kernel.
    let candidate = if vector == 128 {
        SavedContext::continuation(dispatch.active, dispatch.saved.stack, f)
    } else {
        SavedContext::interrupted(dispatch.active, dispatch.saved.stack, f)
    };
    if !candidate.valid(dispatch.active, dispatch.saved.stack) {
        local.inc(field::REJECTED_RETURNS);
        finish_fault(
            cpu,
            dispatch.active,
            f,
            FaultSource::RejectedReturn,
            vector,
            local.get(field::CR2),
        );
        return;
    }
    let wakes = with_state(cpu, |s| {
        if let Some(by) = s
            .scheduler
            .stopping_for(dispatch.active)
            .filter(|by| *by != dispatch.active.thread)
        {
            return s
                .finish(cpu, dispatch.active, ExitReason::Cancelled { by })
                .expect("SMP stopped sibling");
        }
        if vector == 128 {
            let result = operation(s, cpu, dispatch.active, f);
            #[cfg(feature = "device-test")]
            devices::record(s, cpu, dispatch.active, f, result);
            match result {
                Outcome::Exit(code) => s
                    .finish(cpu, dispatch.active, ExitReason::Normal(code))
                    .expect("SMP normal completion"),
                Outcome::Return { status, scalar } => {
                    let mut returned = f;
                    returned.registers.rax = status as u64;
                    returned.registers.rdx = scalar;
                    let saved =
                        SavedContext::continuation(dispatch.active, dispatch.saved.stack, returned);
                    s.scheduler
                        .reschedule(&mut s.nodes, cpu, saved)
                        .expect("SMP syscall continuation");
                    s.ready_waits()
                }
                Outcome::Blocked => {
                    let mut blocked = f;
                    blocked.registers.rax = 0;
                    blocked.registers.rdx = 0;
                    let saved =
                        SavedContext::continuation(dispatch.active, dispatch.saved.stack, blocked);
                    s.scheduler
                        .block(&mut s.nodes, cpu, saved)
                        .expect("SMP blocking continuation");
                    s.ready_waits()
                }
            }
        } else {
            s.scheduler
                .reschedule(&mut s.nodes, cpu, candidate)
                .expect("SMP preemption continuation");
            s.preemptions[dispatch.active.thread.index()] += 1;
            0
        }
    });
    notify(cpu, wakes);
}
fn authority_pending(cpu: usize) -> bool {
    #[cfg(any(feature = "authority-test", feature = "device-test"))]
    {
        with_state(cpu, |s| {
            let mut pending = false;
            #[cfg(feature = "authority-test")]
            {
                pending |= s.authority.pending();
            }
            #[cfg(feature = "device-test")]
            {
                pending |= devices::pending(s);
            }
            pending
        })
    }
    #[cfg(not(any(feature = "authority-test", feature = "device-test")))]
    {
        let _ = cpu;
        false
    }
}
fn runtime(cpu: usize, mut bsp_paging: Option<(&mut Paging<'_, '_>, [&UserSpace; 2])>) {
    enable_cpu(cpu);
    while !DONE.load(Ordering::Acquire) || authority_pending(cpu) {
        if cpu == 0 {
            #[cfg(feature = "device-test")]
            devices::service();
            #[cfg(feature = "authority-test")]
            {
                let (paging, spaces) = bsp_paging.as_mut().expect("authority BSP paging");
                authority::service(paging, *spaces);
            }
            let marker = with_state(cpu, |s| {
                if !s.concurrent
                    && neu_smp_sched_cpus[0].get(field::TICKS) >= 4
                    && neu_smp_sched_cpus[1].get(field::TICKS) >= 4
                {
                    s.concurrent = true;
                    true
                } else {
                    false
                }
            });
            if marker {
                neu_smp_sched_evidence[1].store(2, Ordering::Release);
                serial::write(format_args!(
                    "[NEU] SMP schedule concurrent: cpus={}\n",
                    smp::cpu_count()
                ))
                .expect("SMP concurrent serial");
            }
            let release = with_state(cpu, |s| {
                if !s.released && s.concurrent && neu_smp_sched_cpus[0].get(field::TICKS) >= TICKS {
                    s.released = true;
                    true
                } else {
                    false
                }
            });
            if release {
                if TLB_PROFILE {
                    let (paging, spaces) = bsp_paging.as_mut().expect("SMP TLB BSP transaction");
                    shootdown(paging, spaces[0]);
                }
                barrier_word(0).store(1, Ordering::Release);
            }
        }
        let (next, moved) = with_state(cpu, |s| {
            let mut destination = None;
            if MIGRATE && !s.moved && cpu == 1 && neu_smp_sched_cpus[1].get(field::TICKS) >= 10 {
                let id = s.threads[2].unwrap();
                if s.scheduler.migrate(&s.nodes, id, 3).is_ok() {
                    s.moved = true;
                    destination = Some(3);
                }
            }
            let d = s
                .scheduler
                .dispatch(&mut s.nodes, cpu)
                .expect("SMP dispatch");
            if let Some(d) = &d {
                let slot = d.active.thread.index();
                s.entries[slot] += 1;
                s.last_cpu[slot] = cpu as u64;
                if s.first_cpu[slot] == u64::MAX {
                    s.first_cpu[slot] = cpu as u64;
                }
            }
            s.scheduler.verify(&s.nodes).expect("SMP queue invariant");
            (d, destination)
        });
        if let Some(target) = moved {
            notify(cpu, 1 << target);
        }
        if let Some(dispatch) = next {
            enter(cpu, &dispatch);
            handle(cpu, dispatch);
        } else {
            if DONE.load(Ordering::Acquire) && !authority_pending(cpu) {
                break;
            }
            let local = &neu_smp_sched_cpus[cpu];
            let generation = local.get(field::GENERATION);
            let work = with_state(cpu, |s| s.scheduler.queue_len(cpu) != 0 || s.terminal());
            if work {
                continue;
            }
            #[cfg(feature = "device-test")]
            devices::idle_timer(cpu, authority_pending(cpu));
            local.set(field::PHASE, IDLE);
            local.inc(field::IDLES);
            unsafe {
                neu_smp_sched_idle(ptr::from_ref(&local.words[field::GENERATION]), generation);
            }
            local.set(field::PHASE, KERNEL);
            #[cfg(feature = "device-test")]
            devices::idle_timer(cpu, false);
        }
        if DONE.load(Ordering::Acquire) {
            notify(cpu, (1 << smp::cpu_count()) - 1);
        }
    }
    write(0x320, (1 << 16) | TIMER as u32);
    write(0x380, 0);
    QUIESCENT.fetch_or(1 << cpu, Ordering::Release);
    for _ in 0..WAIT_LIMIT {
        if QUIESCENT.load(Ordering::Acquire) == (1 << smp::cpu_count()) - 1 {
            break;
        }
        core::hint::spin_loop();
    }
    if QUIESCENT.load(Ordering::Acquire) != (1 << smp::cpu_count()) - 1 {
        reject(cpu, 18);
    }
    for _ in 0..1000 {
        if empty(0x200) && empty(0x100) {
            break;
        }
        unsafe {
            neu_smp_sched_drain();
        }
    }
    if !empty(0x200) || !empty(0x100) {
        reject(cpu, 19);
    }
    write(0x80, 0xff);
    write(0xf0, 0xff);
    let local = &neu_smp_sched_cpus[cpu];
    local.set(field::THREAD, 0);
    local.set(field::ROOT, paging::current_root());
    local.set(field::GS, rdmsr(0xc0000101));
    local.set(field::CR0, smp::cr0());
    local.set(field::EFER, smp::efer());
    #[cfg(feature = "cpu-state-test")]
    extended::scrub(cpu);
    unsafe {
        ptr::write_unaligned(
            (local.get(field::TSS) + 4) as *mut u64,
            smp::neu_smp_cpus[cpu].load(cpu::block::STACK_TOP),
        );
    }
    local.set(field::PHASE, STOPPED);
}
fn shootdown(paging: &mut Paging<'_, '_>, space: &UserSpace) {
    let request = with_state(0, |s| {
        s.tlb
            .begin(
                space.root,
                0x410000,
                (1 << smp::cpu_count()) - 1,
                (1 << smp::cpu_count()) - 1,
            )
            .expect("SMP TLB begin")
    });
    let changed = with_state(0, |s| {
        s.tlb
            .validate(request)
            .expect("SMP current TLB transaction binding");
        paging
            .smp_downgrade(space, request)
            .expect("SMP atomic PTE downgrade")
    });
    neu_smp_sched_evidence[40].store(changed.0, Ordering::Release);
    neu_smp_sched_evidence[41].store(changed.1, Ordering::Release);
    neu_smp_sched_evidence[42].store(changed.2, Ordering::Release);
    TLB_ROOT.store(request.root, Ordering::Relaxed);
    TLB_ADDRESS.store(request.address, Ordering::Relaxed);
    TLB_TARGETS.store(request.targets, Ordering::Relaxed);
    TLB_ACKS.store(0, Ordering::Relaxed);
    TLB_EPOCH.store(request.epoch, Ordering::Release);
    invalidate_local(0);
    notify(0, request.targets & !1);
    let mut acknowledged = false;
    for _ in 0..WAIT_LIMIT {
        if TLB_ACKS.load(Ordering::Acquire) == request.targets {
            acknowledged = true;
            break;
        }
        core::hint::spin_loop();
    }
    if !acknowledged {
        with_state(0, |s| s.tlb.fail());
        reject(0, 20);
    }
    with_state(0, |s| {
        for cpu in 0..smp::cpu_count() {
            s.tlb
                .acknowledge(cpu, request)
                .expect("SMP actual invalidation acknowledgement");
        }
        s.tlb.commit(request.epoch).expect("SMP TLB commit");
    });
    neu_smp_sched_evidence[43].store(request.epoch, Ordering::Release);
    neu_smp_sched_evidence[44].store(request.targets, Ordering::Release);
    serial::write(format_args!(
        "[NEU] SMP TLB committed: epoch={} targets={:#x}\n",
        request.epoch, request.targets
    ))
    .expect("SMP TLB serial");
}
pub(super) fn ap_main(cpu: usize, _apic: u32) -> ! {
    #[cfg(feature = "cpu-state-test")]
    extended::probe_cpu(cpu);
    neu_smp_sched_cpus[cpu].set(field::PHASE, READY);
    for _ in 0..WAIT_LIMIT {
        if RELEASE.load(Ordering::Acquire) {
            break;
        }
        core::hint::spin_loop();
    }
    if !RELEASE.load(Ordering::Acquire) {
        reject(cpu, 21);
    }
    runtime(cpu, None);
    let block = &smp::neu_smp_cpus[cpu];
    block.store(cpu::block::PARK_RSP, smp::rsp());
    block.store(cpu::block::PARK_COUNT, 1);
    if block.advance(Bootstrap::Parked).is_err() {
        reject(cpu, 22);
    }
    smp::silent_park()
}
fn payload() -> &'static [u8] {
    let len = ptr::addr_of!(neu_smp_sched_payload_end) as usize
        - ptr::addr_of!(neu_smp_sched_payload) as usize;
    assert!(len > 0 && len <= PAGE as usize);
    unsafe { core::slice::from_raw_parts(ptr::addr_of!(neu_smp_sched_payload), len) }
}
fn negative_record(
    index: usize,
    domain: u64,
    expected: u64,
    actual: u64,
    paging: &Paging<'_, '_>,
    space: &UserSpace,
    before: (u64, u64),
) {
    let after = paging
        .smp_probe_pte(space)
        .expect("SMP negative PTE observation");
    assert_eq!(actual, expected, "SMP negative result");
    assert_eq!(after, before, "SMP negative mutated the PTE");
    let row = [
        index as u64 + 1,
        domain,
        expected,
        actual,
        before.0,
        before.1,
        after.1,
        0,
    ];
    for (cell, value) in neu_smp_sched_negatives[index * 8..index * 8 + 8]
        .iter()
        .zip(row)
    {
        cell.store(value, Ordering::Release);
    }
}
fn negatives(paging: &mut Paging<'_, '_>, space: &UserSpace, foreign_root: u64) {
    use neu_kernel::shootdown::{Error as E, Request};
    let before = paging
        .smp_probe_pte(space)
        .expect("SMP negative starting PTE");
    let mut test = Shootdown::new();
    let invalid = [
        (0, 0x410000, 3, 3, E::Address),
        (space.root + 1, 0x410000, 3, 3, E::Address),
        (space.root, 0x410001, 3, 3, E::Address),
        (space.root, 0xffff_ffff_8000_0000, 3, 3, E::Address),
        (space.root, 0x410000, 0, 3, E::Participants),
        (space.root, 0x410000, 4, 3, E::Participants),
        (space.root, 0x410000, 1, 0x11, E::Participants),
    ];
    for (index, (root, address, targets, online, expected)) in invalid.into_iter().enumerate() {
        let actual = test
            .begin(root, address, targets, online)
            .expect_err("SMP invalid TLB request accepted");
        negative_record(
            index,
            1,
            expected as u64,
            actual as u64,
            paging,
            space,
            before,
        );
        assert!(!test.pending());
    }
    let request = test
        .begin(space.root, 0x410000, 3, 3)
        .expect("SMP negative setup");
    let actual = test
        .begin(space.root, 0x410000, 3, 3)
        .expect_err("SMP overlapping TLB request");
    negative_record(7, 1, E::Busy as u64, actual as u64, paging, space, before);
    for (index, cpu, bad, expected) in [
        (
            8,
            0,
            Request {
                epoch: request.epoch + 1,
                ..request
            },
            E::Generation,
        ),
        (
            9,
            0,
            Request {
                root: foreign_root,
                ..request
            },
            E::Identity,
        ),
        (10, 2, request, E::Identity),
    ] {
        let actual = test
            .acknowledge(cpu, bad)
            .expect_err("SMP invalid TLB acknowledgement");
        negative_record(
            index,
            1,
            expected as u64,
            actual as u64,
            paging,
            space,
            before,
        );
        assert_eq!(test.acknowledged(), 0);
    }
    let actual = test
        .commit(request.epoch)
        .expect_err("SMP early TLB commit");
    negative_record(
        11,
        1,
        E::Pending as u64,
        actual as u64,
        paging,
        space,
        before,
    );
    test.acknowledge(0, request).unwrap();
    let actual = test
        .acknowledge(0, request)
        .expect_err("SMP duplicate TLB acknowledgement");
    negative_record(
        12,
        1,
        E::Duplicate as u64,
        actual as u64,
        paging,
        space,
        before,
    );
    test.acknowledge(1, request).unwrap();
    test.commit(request.epoch).unwrap();
    let next = test.begin(space.root, 0x410000, 3, 3).unwrap();
    let actual = test
        .acknowledge(0, request)
        .expect_err("SMP stale TLB acknowledgement");
    negative_record(
        13,
        1,
        E::Generation as u64,
        actual as u64,
        paging,
        space,
        before,
    );
    assert_eq!(test.acknowledged(), 0);
    test.fail();
    let actual = test
        .begin(space.root, 0x410000, 3, 3)
        .expect_err("SMP failed TLB session reused");
    negative_record(
        14,
        1,
        E::Failed as u64,
        actual as u64,
        paging,
        space,
        before,
    );
    let actual = test
        .commit(next.epoch)
        .expect_err("SMP failed TLB session committed");
    negative_record(
        15,
        1,
        E::Failed as u64,
        actual as u64,
        paging,
        space,
        before,
    );
    for (index, bad, permissions, expected) in [
        (
            16,
            Request {
                root: foreign_root,
                ..request
            },
            logic::Permissions::R,
            logic::Error::NotOwned,
        ),
        (
            17,
            Request {
                address: 0x420000,
                ..request
            },
            logic::Permissions::R,
            logic::Error::NotMapped,
        ),
        (18, request, logic::Permissions::RX, logic::Error::Flags),
        (
            19,
            Request {
                targets: 0x10,
                ..request
            },
            logic::Permissions::R,
            logic::Error::NotOwned,
        ),
    ] {
        let actual = paging
            .smp_validate_downgrade(space, bad, permissions)
            .expect_err("SMP invalid registry restriction");
        negative_record(
            index,
            2,
            expected as u64,
            actual as u64,
            paging,
            space,
            before,
        );
    }
    for (index, bad) in [
        (
            20,
            Request {
                root: foreign_root,
                ..request
            },
        ),
        (
            21,
            Request {
                address: 0x410001,
                ..request
            },
        ),
    ] {
        let actual = paging
            .smp_downgrade(space, bad)
            .expect_err("SMP invalid PTE restriction");
        negative_record(
            index,
            2,
            logic::Error::NotOwned as u64,
            actual as u64,
            paging,
            space,
            before,
        );
    }
    neu_smp_sched_evidence[47].store(22, Ordering::Release);
    neu_smp_sched_evidence[1].store(5, Ordering::Release);
    serial::write(format_args!(
        "[NEU] SMP TLB negatives: checks=22 PTE=unchanged\n"
    ))
    .expect("SMP TLB negative serial");
    probe_wait();
}
pub fn run(paging: &mut Paging<'_, '_>) -> Result<(), &'static str> {
    #[cfg(feature = "cpu-state-test")]
    extended::configure()?;
    calibrate()?;
    let before = paging.stats();
    let tables = paging.table_count();
    let root = paging.root();
    inventory(paging);
    neu_smp_sched_evidence[0].store(MAGIC, Ordering::Release);
    neu_smp_sched_evidence[2].store(smp::cpu_count() as u64, Ordering::Release);
    neu_smp_sched_evidence[3].store(root, Ordering::Release);
    neu_smp_sched_evidence[4].store(before.allocated as u64, Ordering::Release);
    neu_smp_sched_evidence[5].store(tables as u64, Ordering::Release);
    serial::write(format_args!(
        "[NEU] SMP schedule baseline: cpus={} root={:#x}\n",
        smp::cpu_count(),
        root
    ))
    .map_err(|_| "SMP baseline serial")?;
    probe_wait();
    #[cfg(feature = "device-test")]
    devices::init(paging)?;
    let barrier = paging
        .translate(ptr::addr_of!(neu_smp_sched_barrier) as u64)
        .map_err(|_| "SMP barrier translation")?
        .ok_or("SMP barrier absent")?;
    BAR_PA.store(barrier.physical, Ordering::Release);
    barrier_word(3).store(paging::text_range().0, Ordering::Release);
    let mut a = paging
        .prepare_domain_shared(payload(), 0, 0, [0; 4], None)
        .map_err(|error| {
            serial::write(format_args!(
                "[NEU] SMP Node A root error: {error:?}, payload_bytes={}, tables={}\n",
                payload().len(),
                paging.table_count(),
            ))
            .expect("SMP Node A root diagnostic");
            "SMP Node A root"
        })?;
    let mut b = paging
        .prepare_domain_shared(payload(), 0, 0, [0; 4], None)
        .map_err(|error| {
            serial::write(format_args!(
                "[NEU] SMP Node B root error: {error:?}, payload_bytes={}, tables={}\n",
                payload().len(),
                paging.table_count(),
            ))
            .expect("SMP Node B root diagnostic");
            "SMP Node B root"
        })?;
    neu_smp_sched_evidence[6].store(a.root, Ordering::Release);
    neu_smp_sched_evidence[7].store(b.root, Ordering::Release);
    paging
        .relocate_user_stack(&mut b, logic::thread_user_stack(1))
        .map_err(|_| "SMP Node B stack")?;
    let kernel_pins = with_state(0, |s| -> Result<(), &'static str> {
        s.scheduler = Scheduler::new(smp::cpu_count()).map_err(|_| "SMP CPU count")?;
        let mut nodes = [None; 2];
        for (index, space) in [&a, &b].into_iter().enumerate() {
            let id = s.nodes.create().map_err(|_| "SMP Node create")?;
            s.nodes.configure(id).map_err(|_| "SMP Node configure")?;
            s.nodes
                .bind_space(id, space.id())
                .map_err(|_| "SMP Node bind")?;
            let context = ThreadContext {
                space: space.id(),
                root: space.root,
                entry: USER_CODE,
                rsp: logic::thread_user_stack(index) + PAGE - 8,
            };
            let tid = s
                .nodes
                .initial_thread(id, context)
                .map_err(|_| "SMP initial Thread")?;
            s.nodes.ready(id).map_err(|_| "SMP Node ready")?;
            nodes[index] = Some(id);
            s.threads[index] = Some(tid);
        }
        s.node_ids = nodes;
        let extra = s
            .nodes
            .member_thread(
                nodes[0].unwrap(),
                ThreadContext {
                    space: a.id(),
                    root: a.root,
                    entry: USER_CODE,
                    rsp: logic::thread_user_stack(2) + PAGE - 8,
                },
            )
            .map_err(|_| "SMP sibling Thread")?;
        s.threads[2] = Some(extra);
        #[cfg(feature = "authority-test")]
        authority::bootstrap(paging, s, [&a, &b])?;
        #[cfg(not(feature = "authority-test"))]
        for (index, space) in [&a, &b].into_iter().enumerate() {
            let node = nodes[index].unwrap();
            let retained = paging
                .retain_object_space(space)
                .map_err(|_| "SMP capability root reference")?;
            let object = s
                .objects
                .address_space(node, retained)
                .map_err(|_| "SMP AddressSpace capability")?;
            let flag = s.objects.flag(true).map_err(|_| "SMP Flag capability")?;
            let table = s
                .nodes
                .authority_mut(node)
                .map_err(|_| "SMP bootstrap table")?;
            for (slot, rights) in [
                (0, Rights::INSPECT),
                (1, Rights::INSPECT),
                (2, Rights::NONE),
            ] {
                table
                    .install_bootstrap(&mut s.objects, &object, rights, slot)
                    .map_err(|_| "SMP inspect bootstrap")?;
            }
            table
                .install_bootstrap(&mut s.objects, &flag, Rights::READ, 3)
                .map_err(|_| "SMP Flag bootstrap")?;
            s.objects
                .release(object)
                .map_err(|_| "SMP creator release")?;
            s.objects.release(flag).map_err(|_| "SMP Flag release")?;
        }
        s.nodes
            .bootstrap_channel(&mut s.objects, nodes[0].unwrap(), nodes[1].unwrap())
            .map_err(|_| "SMP Channel bootstrap")?;
        #[cfg(feature = "device-test")]
        devices::bootstrap(s)?;
        for id in nodes.into_iter().flatten() {
            let table = s.nodes.authority_mut(id).map_err(|_| "SMP manifest")?;
            #[cfg(not(feature = "authority-test"))]
            table.configure_moves().map_err(|_| "SMP move authority")?;
            #[cfg(feature = "authority-test")]
            {
                table
                    .configure_memory_object_moves()
                    .map_err(|_| "authority move rights")?;
                if cfg!(feature = "test-smp-authority-wait") && id == nodes[0].unwrap() {
                    let endpoint = Handle::from_raw((1u64 << 32) | 5);
                    table
                        .configure_delegation(&mut s.objects, endpoint)
                        .map_err(|_| "authority endpoint grant")?;
                }
            }
            table.seal_bootstrap();
        }
        #[cfg(feature = "authority-test")]
        authority::prepare_mappings(paging, s, [&mut a, &mut b])?;
        Ok(())
    });
    kernel_pins?;
    for slot in 0..3 {
        let space = if slot == 1 { &mut b } else { &mut a };
        with_state(0, |s| -> Result<(), &'static str> {
            let id = s.threads[slot].unwrap();
            let t = s.nodes.thread(id).map_err(|_| "SMP member identity")?;
            let endpoint = s
                .nodes
                .authority(t.node)
                .map_err(|_| "SMP endpoint table")?
                .bootstrap_get(&s.objects, t.node, 4)
                .map_err(|_| "SMP endpoint handle")?
                .raw();
            #[cfg(all(feature = "cpu-state-test", not(feature = "device-test")))]
            let action = extended::action(slot);
            #[cfg(not(all(feature = "cpu-state-test", not(feature = "device-test"))))]
            let action = if cfg!(feature = "test-cpu-state-disabled") {
                17
            } else if cfg!(feature = "device-test") {
                30
            } else if cfg!(feature = "test-smp-device-disabled")
                || cfg!(feature = "test-smp-dma-disabled")
            {
                18
            } else if cfg!(feature = "authority-test") {
                20
            } else if cfg!(feature = "test-smp-authority-disabled") {
                19
            } else if slot != 1 && cfg!(feature = "test-smp-schedule-shared") {
                8
            } else if slot == 2 && REJECT_PROFILE {
                10
            } else if slot == 2 && FAULT_PROFILE {
                1
            } else if slot == 2 && TLB_PROFILE {
                4
            } else if slot == 1 && IPC_PROFILE {
                3
            } else if slot == 0 && IPC_PROFILE {
                2
            } else if slot == 1 && (FAULT_PROFILE || TLB_PROFILE || REJECT_PROFILE) {
                5
            } else if slot == 0 && TLB_PROFILE {
                7
            } else if slot == 0 && (FAULT_PROFILE || REJECT_PROFILE) {
                6
            } else {
                0
            };
            let binding = {
                let edit = s.nodes.stack_edit(id).map_err(|_| "SMP stack edit")?;
                paging
                    .prepare_member(
                        space,
                        &edit,
                        [
                            slot as u64 + 1,
                            0x1101 + slot as u64 * 0x1101,
                            endpoint,
                            action,
                        ],
                        None,
                    )
                    .map_err(|_| "SMP guarded stacks")?
            };
            let active = ActiveContext {
                node: t.node,
                thread: id,
                context: t.context,
            };
            s.nodes
                .install_execution(id, SavedContext::initial_preemptible(active, binding))
                .map_err(|_| "SMP saved context")?;
            #[cfg(feature = "cpu-state-test")]
            s.cpu_state
                .install(active)
                .map_err(|_| "CPU state initial ownership")?;
            s.data[slot] = paging
                .smp_data_physical(space)
                .map_err(|_| "SMP DATA backing")?
                + slot as u64 * 128;
            Ok(())
        })?;
    }
    if TLB_PROFILE {
        paging
            .map_shared_probe(&a)
            .map_err(|_| "SMP TLB private alias")?;
    }
    if cfg!(feature = "test-smp-tlb-invalid") {
        negatives(paging, &a, b.root);
    }
    with_state(0, |s| -> Result<(), &'static str> {
        s.inspections = [
            Some(paging.smp_inspection(&a).map_err(|_| "SMP inspection A")?),
            Some(paging.smp_inspection(&b).map_err(|_| "SMP inspection B")?),
        ];
        for slot in 0..3 {
            let space = if slot == 1 { &mut b } else { &mut a };
            let id = s.threads[slot].unwrap();
            paging
                .pin_member(space, id)
                .map_err(|_| "SMP execution pin")?;
            let cpu = if slot == 0 {
                0
            } else if slot == 2 {
                1
            } else if smp::cpu_count() == 4 {
                2
            } else {
                0
            };
            s.scheduler
                .admit(&mut s.nodes, cpu, id, (1 << smp::cpu_count()) - 1)
                .map_err(|_| "SMP queue admission")?;
        }
        s.scheduler
            .verify(&s.nodes)
            .map_err(|_| "SMP initial ownership")?;
        Ok(())
    })?;
    for (cpu, local) in neu_smp_sched_cpus.iter().enumerate().take(smp::cpu_count()) {
        let block = &smp::neu_smp_cpus[cpu];
        let resources = smp::user_cpu(cpu).ok_or("SMP CPU descriptor identity")?;
        local.set(0, MAGIC);
        local.set(1, cpu as u64);
        local.set(2, u64::from(resources.apic_id));
        local.set(field::KERNEL_ROOT, root);
        local.set(field::TSS, resources.tss);
        local.set(field::IDT, resources.idt);
        local.set(field::GDT, resources.gdt);
        local.set(field::PHASE, READY);
        unsafe {
            exceptions::install_smp_scheduler_gates(block.load(cpu::block::IDT_BASE) as *mut u8)
                .map_err(|_| "SMP scheduler gates")?;
        }
    }
    neu_smp_sched_evidence[0].store(MAGIC, Ordering::Release);
    neu_smp_sched_evidence[1].store(1, Ordering::Release);
    neu_smp_sched_evidence[2].store(smp::cpu_count() as u64, Ordering::Release);
    neu_smp_sched_evidence[3].store(root, Ordering::Release);
    neu_smp_sched_evidence[4].store(before.allocated as u64, Ordering::Release);
    neu_smp_sched_evidence[5].store(tables as u64, Ordering::Release);
    neu_smp_sched_evidence[6].store(a.root, Ordering::Release);
    neu_smp_sched_evidence[7].store(b.root, Ordering::Release);
    neu_smp_sched_evidence[10].store(barrier.physical, Ordering::Release);
    serial::write(format_args!(
        "[NEU] SMP schedule prepared: cpus={} roots={:#x},{:#x}\n",
        smp::cpu_count(),
        a.root,
        b.root
    ))
    .map_err(|_| "SMP prepared serial")?;
    probe_wait();
    RELEASE.store(true, Ordering::Release);
    runtime(0, Some((paging, [&a, &b])));
    for (cpu, local) in neu_smp_sched_cpus
        .iter()
        .enumerate()
        .take(smp::cpu_count())
        .skip(1)
    {
        for _ in 0..WAIT_LIMIT {
            if local.get(field::PHASE) == STOPPED
                && smp::neu_smp_cpus[cpu].load(cpu::block::STATE) == Bootstrap::Parked as u64
            {
                break;
            }
            core::hint::spin_loop();
        }
        if local.get(field::PHASE) != STOPPED {
            return Err("SMP AP did not stop");
        }
    }
    if !exceptions::fatal_record_clear() {
        return Err("SMP fatal record was touched");
    }
    neu_smp_sched_evidence[1].store(3, Ordering::Release);
    serial::write(format_args!("[NEU] SMP schedule terminal: threads=3\n"))
        .map_err(|_| "SMP terminal serial")?;
    probe_wait();
    with_state(0, |s| -> Result<(), &'static str> {
        s.inspections = [None, None];
        for slot in 0..3 {
            let space = if slot == 1 { &mut b } else { &mut a };
            let id = s.threads[slot].unwrap();
            let t = s.nodes.thread(id).map_err(|_| "SMP cleanup identity")?;
            if t.stack.is_none() {
                return Err("SMP cleanup stack");
            }
            let edit = s
                .nodes
                .stack_edit(id)
                .map_err(|_| "SMP terminal stack edit")?;
            let receipt = paging
                .destroy_member(space, &edit)
                .map_err(|_| "SMP guarded stack cleanup")?;
            edit.finish(receipt)
                .map_err(|_| "SMP stack release receipt")?;
        }
        while let Some(reference) = s.objects.reap_space() {
            paging
                .release_object_space(reference)
                .map_err(|_| "SMP root reference release")?;
        }
        #[cfg(feature = "authority-test")]
        authority::cleanup(paging, s, [&mut a, &mut b])?;
        for (index, space) in [&mut a, &mut b].into_iter().enumerate() {
            let id = s.node_ids[index].unwrap();
            s.nodes
                .release_space(id, space.id())
                .map_err(|_| "SMP space release")?;
            s.nodes.reclaim(id).map_err(|_| "SMP Node reclaim")?;
            paging.destroy_user(space).map_err(|_| "SMP root cleanup")?;
        }
        if s.objects.stats() != cap::Stats::default() {
            return Err("SMP residual capability state");
        }
        #[cfg(feature = "cpu-state-test")]
        if !s.cpu_state.clean() {
            return Err("CPU state residual ownership or data");
        }
        Ok(())
    })?;
    #[cfg(feature = "device-test")]
    devices::cleanup(paging)?;
    if paging.stats() != before || paging.table_count() != tables || paging::current_root() != root
    {
        return Err("SMP final allocator/table mismatch");
    }
    neu_smp_sched_evidence[45].store(paging.stats().allocated as u64, Ordering::Release);
    neu_smp_sched_evidence[46].store(paging.table_count() as u64, Ordering::Release);
    neu_smp_sched_evidence[1].store(4, Ordering::Release);
    let block = &smp::neu_smp_cpus[0];
    block.store(cpu::block::PARK_RSP, smp::rsp());
    block.store(cpu::block::PARK_COUNT, 1);
    block.advance(Bootstrap::Parked)?;
    serial::write(format_args!(
        "[NEU] SMP schedule complete: cpus={} threads=3 baseline=restored\n",
        smp::cpu_count()
    ))
    .map_err(|_| "SMP complete serial")?;
    Ok(())
}

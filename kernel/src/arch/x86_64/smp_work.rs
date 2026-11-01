//! Phase 9B x86-64 work backend. The BSP publishes one command per AP mailbox
//! at a time, validates the AP's acceptance and completion, and records the
//! final evidence the QMP observer re-reads. Stage A APs wait in a bounded
//! atomic poll loop. Stage B APs additionally receive a dedicated xAPIC IPI
//! and enter their only interrupt-enabled window inside `sti; hlt`.
//!
//! Every address here is fixed-capacity image memory. No allocator, paging
//! mutation, scheduler, capability, Channel, MemoryObject or serial state is
//! touched by an AP.

use core::{
    arch::{asm, global_asm},
    fmt,
    sync::atomic::{AtomicU64, Ordering},
};

#[cfg(feature = "smp-ipi-test")]
use core::ptr;

use neu_kernel::{
    cpu::{self, Bootstrap},
    smp_work::{
        self, Command, CommandKind, Completion, Error, Executor, GATE_STRIDE, GATE_WORDS, Gate,
        GatePhase, MAILBOX_FIELDS, MAILBOX_MAGIC, MAILBOX_STRIDE, Mailbox, Phase, Poll, Publisher,
        State, mailbox,
    },
};

use super::{exceptions::ExceptionFrame, paging, serial, smp};

global_asm!(include_str!("smp_work.S"));

const _: () = assert!(neu_kernel::smp_work::gate::RELEASE * 8 == 32);
const _: () = assert!(MAILBOX_FIELDS * 8 == MAILBOX_STRIDE);

const EVIDENCE_MAGIC: u64 = 0x4e45_555f_574b_4531; // "NEU_WKE1"
const WORK_WAIT_LIMIT: u64 = 1_000_000_000;
const FAILURE_WORK_BASE: u64 = 32;
/// One AP-written record per accepted command. Slots are never reused because
/// every AP mailbox sequences from 1 and stops before `JOURNAL_SLOTS`.
const JOURNAL_SLOTS: usize = 4;

pub mod journal {
    pub const SEQUENCE: usize = 0;
    pub const KIND: usize = 1;
    pub const INPUT: usize = 2;
    pub const ROUNDS: usize = 3;
    pub const RESULT: usize = 4;
    pub const AUX: usize = 5;
    pub const START: usize = 6;
    pub const MARKER: usize = 7;
    pub const ACCEPT_ORDER: usize = 8;
    pub const COMPLETE_ORDER: usize = 9;
    pub const FIELDS: usize = 10;
}

const CPUS: usize = if cfg!(feature = "smp-four") { 4 } else { 2 };

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Profile {
    Work2,
    Work4,
    Overlap,
    Repeat,
    Invalid,
    Ipi2,
    Ipi4,
}

const PROFILE: Profile = if cfg!(feature = "test-smp-work-2") {
    Profile::Work2
} else if cfg!(feature = "test-smp-work-4") {
    Profile::Work4
} else if cfg!(feature = "test-smp-work-overlap") {
    Profile::Overlap
} else if cfg!(feature = "test-smp-work-repeat") {
    Profile::Repeat
} else if cfg!(feature = "test-smp-work-invalid") {
    Profile::Invalid
} else if cfg!(feature = "test-smp-ipi-2") {
    Profile::Ipi2
} else {
    Profile::Ipi4
};

const _: () = assert!(
    (cfg!(feature = "test-smp-work-2") as u8
        + cfg!(feature = "test-smp-work-4") as u8
        + cfg!(feature = "test-smp-work-overlap") as u8
        + cfg!(feature = "test-smp-work-repeat") as u8
        + cfg!(feature = "test-smp-work-invalid") as u8
        + cfg!(feature = "test-smp-ipi-2") as u8
        + cfg!(feature = "test-smp-ipi-4") as u8)
        == 1,
    "select exactly one Phase 9B work profile"
);

const OVERLAP_PROFILE: bool = cfg!(feature = "test-smp-work-overlap");
const PARALLEL_PROFILE: bool =
    cfg!(feature = "test-smp-work-4") || cfg!(feature = "test-smp-ipi-4");

const WORK2: [Command; 2] = [
    Command::new(CommandKind::Transform, 0x9e37_0001, 512),
    Command::new(CommandKind::Checksum, 0x51ed_0002, 384),
];
const WORK4: [Command; 3] = [
    Command::new(CommandKind::Transform, 0x9101, 320),
    Command::new(CommandKind::Checksum, 0x9202, 288),
    Command::new(CommandKind::Transform, 0x9303, 256),
];
const OVERLAP: Command = Command::new(CommandKind::Transform, 0x0bad_c0de, 448);
const REPEAT: [Command; 3] = [
    Command::new(CommandKind::Transform, 0x31, 96),
    Command::new(CommandKind::Checksum, 0x32, 160),
    Command::new(CommandKind::Transform, 0x33, 224),
];
const INVALID: Command = Command::new(CommandKind::Checksum, 0x44, 192);
const IPI2: [Command; 2] = [
    Command::new(CommandKind::Checksum, 0x5151, 512),
    Command::new(CommandKind::Transform, 0x5252, 384),
];
const IPI4: [Command; 3] = [
    Command::new(CommandKind::Checksum, 0x6161, 320),
    Command::new(CommandKind::Transform, 0x6262, 288),
    Command::new(CommandKind::Checksum, 0x6363, 256),
];

pub mod evidence {
    pub const MAGIC: usize = 0;
    pub const ABI: usize = 1;
    pub const MODE: usize = 2;
    pub const CPUS: usize = 3;
    pub const COMMANDS: usize = 4;
    pub const PER_AP: usize = 5;
    pub const AP_STOPPED: usize = 6;
    pub const IPI_SENT: usize = 7;
    pub const IPI_RECEIVED: usize = 8;
    pub const IPI_EOIS: usize = 9;
    pub const SPURIOUS: usize = 10;
    pub const NEGATIVE_CHECKS: usize = 11;
    pub const NEGATIVE_PASSED: usize = 12;
    pub const BSP_RESULT: usize = 13;
    pub const BSP_AUX: usize = 14;
    pub const GATE_EPOCH: usize = 15;
    pub const GATE_OVERLAP: usize = 16;
    pub const RELEASE: usize = 17;
    pub const FAILURE: usize = 18;
    pub const FIELDS: usize = 19;
}

#[unsafe(no_mangle)]
#[allow(non_upper_case_globals)]
pub static neu_smp_work_mailboxes: [Mailbox; cpu::MAX_CPUS] =
    [const { Mailbox::new() }; cpu::MAX_CPUS];

// The gate is only dereferenced by the overlap profile, so keep the published
// object alive for the QMP oracle under linker garbage collection.
#[used]
#[unsafe(no_mangle)]
#[allow(non_upper_case_globals)]
pub static neu_smp_work_gate: Gate = Gate::new();

/// AP-written per-command journal. The BSP never mutates it; QMP recomputes
/// every result from KIND/INPUT/ROUNDS and checks sequence and marker order.
#[unsafe(no_mangle)]
#[allow(non_upper_case_globals)]
pub static neu_smp_work_journal: [AtomicU64; cpu::MAX_CPUS * JOURNAL_SLOTS * journal::FIELDS] =
    [const { AtomicU64::new(0) }; cpu::MAX_CPUS * JOURNAL_SLOTS * journal::FIELDS];

#[used]
#[unsafe(no_mangle)]
#[allow(non_upper_case_globals)]
pub static neu_smp_work_layout: [u64; 9] = [
    smp_work::ABI,
    MAILBOX_MAGIC,
    cpu::MAX_CPUS as u64,
    MAILBOX_FIELDS as u64,
    MAILBOX_STRIDE as u64,
    GATE_WORDS as u64,
    GATE_STRIDE as u64,
    JOURNAL_SLOTS as u64,
    journal::FIELDS as u64,
];

#[unsafe(no_mangle)]
#[allow(non_upper_case_globals)]
pub static neu_smp_work_evidence: [AtomicU64; evidence::FIELDS] =
    [const { AtomicU64::new(0) }; evidence::FIELDS];

unsafe extern "C" {
    fn neu_smp_work_ap_dwell_begin();
    fn neu_smp_work_bsp_dwell_begin();
    #[cfg(feature = "smp-ipi-test")]
    fn neu_smp_work_idle_once();
    fn neu_smp_work_gate_wait_begin(gate: *const Gate, epoch: u64) -> u64;
    #[cfg(feature = "smp-ipi-test")]
    static neu_smp_work_idle_after: u8;
}

fn record(field: usize, value: u64) {
    neu_smp_work_evidence[field].store(value, Ordering::Release);
}

fn add(field: usize, value: u64) {
    neu_smp_work_evidence[field].fetch_add(value, Ordering::Relaxed);
}

fn log(arguments: fmt::Arguments<'_>) -> Result<(), &'static str> {
    serial::write(arguments).map_err(|_| "SMP work serial output failed")
}

fn apic_of(logical: usize) -> Result<u32, &'static str> {
    u32::try_from(smp::neu_smp_cpus[logical].load(cpu::block::APIC_ID))
        .map_err(|_| "SMP work APIC identity is not representable")
}

fn map_error(error: Error) -> &'static str {
    match error {
        Error::Magic => "SMP work mailbox magic is invalid",
        Error::Identity => "SMP work mailbox identity mismatch",
        Error::Logical => "SMP work logical index is out of range",
        Error::Phase => "SMP work mailbox is not in a usable phase",
        Error::State => "SMP work mailbox is not in the required state",
        Error::Sequence => "SMP work sequence or marker is inconsistent",
        Error::SequenceOverflow => "SMP work sequence exhausted",
        Error::InvalidKind => "SMP work command kind is invalid",
        Error::InvalidRounds => "SMP work command rounds are invalid",
        Error::Busy => "SMP work mailbox is busy",
        Error::StaleAck => "SMP work acknowledgement is stale",
        Error::Failed => "SMP work AP is failed",
        Error::NotReady => "SMP work AP did not reach the required phase",
    }
}

fn stack_pointer() -> u64 {
    let value: u64;
    // SAFETY: Read-only stack-pointer inspection at Ring 0.
    unsafe {
        asm!("mov {}, rsp", out(reg) value, options(nomem, nostack, preserves_flags));
    }
    value
}

fn current_root() -> u64 {
    let root: u64;
    // SAFETY: Read-only CR3 inspection at Ring 0.
    unsafe {
        asm!("mov {}, cr3", out(reg) root, options(nomem, nostack, preserves_flags));
    }
    root & 0x000f_ffff_ffff_f000
}

fn interrupt_flag() -> bool {
    let flags: u64;
    // SAFETY: Read-only flag inspection at Ring 0.
    unsafe {
        asm!("pushfq", "pop {}", out(reg) flags, options(preserves_flags));
    }
    flags & 0x200 != 0
}

fn work_commands() -> usize {
    match PROFILE {
        Profile::Work2 | Profile::Ipi2 => 2,
        Profile::Work4 | Profile::Overlap | Profile::Invalid | Profile::Ipi4 => 1,
        Profile::Repeat => 3,
    }
}

/// Bounded BSP wait for the AP publication that places it inside its dwell.
fn wait_executing(logical: usize) -> Result<(), &'static str> {
    let mailbox = &neu_smp_work_mailboxes[logical];
    let mut spins = 0u64;
    loop {
        match mailbox.state() {
            Ok(State::Executing)
                if mailbox.counter(mailbox::ACCEPT_SEQUENCE) == mailbox.sequence() =>
            {
                return Ok(());
            }
            Ok(State::Executing) | Ok(State::Published) => {}
            Ok(State::Failed) => return Err("SMP work AP failed before its checkpoint"),
            Ok(_) => return Err("SMP work AP mailbox left the published state early"),
            Err(error) => return Err(map_error(error)),
        }
        if spins >= WORK_WAIT_LIMIT {
            return Err("SMP work AP did not accept its command within the bounded window");
        }
        core::hint::spin_loop();
        spins += 1;
    }
}

fn execution_mask(sequence: u64) -> u64 {
    if sequence >= 64 {
        u64::MAX
    } else {
        (1u64 << sequence) - 1
    }
}

fn full_bootstrap_history() -> u64 {
    Bootstrap::Prepared.bit()
        | Bootstrap::Entered.bit()
        | Bootstrap::Ready.bit()
        | Bootstrap::Parked.bit()
}

/// Publishes the AP identity before the AP advances to `Ready`. The BSP only
/// attaches after observing that transition.
pub(super) fn ap_initialize(logical: usize, apic_id: u32) {
    if logical == 0 || logical >= cpu::MAX_CPUS {
        return;
    }
    neu_smp_work_mailboxes[logical].initialize(logical, apic_id);
}

pub(super) fn ap_main(logical: usize, apic_id: u32) -> ! {
    let block = &smp::neu_smp_cpus[logical];
    let mailbox = &neu_smp_work_mailboxes[logical];
    let executor = match Executor::attach(mailbox, logical, apic_id) {
        Ok(value) => value,
        Err(error) => ap_terminal(block, mailbox, error),
    };
    #[cfg(feature = "smp-ipi-test")]
    if let Err(error) = initialize_ap_lapic(mailbox, apic_id) {
        ap_terminal(block, mailbox, error);
    }
    loop {
        let command = match wait_command(&executor, mailbox) {
            Ok(command) => command,
            Err(error) => ap_terminal(block, mailbox, error),
        };
        let sequence = mailbox.sequence();
        if mailbox.state() != Ok(State::Executing) || sequence == 0 {
            ap_terminal(block, mailbox, Error::State);
        }
        if command.kind != CommandKind::Stop {
            if OVERLAP_PROFILE {
                let gate = &neu_smp_work_gate;
                let epoch = gate.epoch();
                if gate.arrive(logical, epoch).is_err() {
                    ap_terminal(block, mailbox, Error::Phase);
                }
                // SAFETY: The gate is the fixed image-resident barrier and the
                // epoch was published before this AP arrived.
                let released = unsafe { neu_smp_work_gate_wait_begin(gate as *const Gate, epoch) };
                if released != 0 || gate.depart(logical, epoch).is_err() {
                    ap_terminal(block, mailbox, Error::NotReady);
                }
            }
            if PARALLEL_PROFILE {
                // SAFETY: Bounded CPU-local assembly dwell with no memory
                // effects; it is the deterministic QMP observation window.
                unsafe { neu_smp_work_ap_dwell_begin() };
            }
        }
        let (result, aux) = if command.kind == CommandKind::Stop {
            (0, 0)
        } else {
            match smp_work::execute(command.kind, command.input, command.rounds) {
                Ok(values) => values,
                Err(error) => ap_terminal(block, mailbox, error),
            }
        };
        if executor.complete(sequence, result, aux).is_err() {
            ap_terminal(block, mailbox, Error::State);
        }
        if let Err(error) = journal_record(logical, apic_id, sequence, &command, result, aux) {
            ap_terminal(block, mailbox, error);
        }
        if !executor.acknowledged(sequence, WORK_WAIT_LIMIT) {
            ap_terminal(block, mailbox, Error::NotReady);
        }
        if command.kind == CommandKind::Stop {
            mailbox.set_phase(Phase::Stopped);
            block.store(cpu::block::PARK_RSP, stack_pointer());
            block.store(cpu::block::PARK_COUNT, 1);
            block.store(cpu::block::ISOLATION, cpu::block::ISOLATION_AP);
            if block.advance(Bootstrap::Parked).is_err() {
                ap_terminal(block, mailbox, Error::State);
            }
            smp::silent_park();
        }
    }
}

fn ap_terminal(block: &smp::CpuBlock, mailbox: &Mailbox, error: Error) -> ! {
    mailbox.fail(error);
    block.store(cpu::block::FAILURE, FAILURE_WORK_BASE + error.code());
    let _ = block.advance(Bootstrap::Failed);
    smp::silent_park()
}

fn journal_record(
    logical: usize,
    apic_id: u32,
    sequence: u64,
    command: &Command,
    result: u64,
    aux: u64,
) -> Result<(), Error> {
    if sequence == 0 || sequence > JOURNAL_SLOTS as u64 {
        return Err(Error::SequenceOverflow);
    }
    let mailbox = &neu_smp_work_mailboxes[logical];
    let start = mailbox.counter(mailbox::START_MARKER);
    if start != smp_work::start_marker(sequence, logical, apic_id) {
        return Err(Error::Sequence);
    }
    let marker = smp_work::complete_marker(start, result, aux);
    let base = (logical * JOURNAL_SLOTS + sequence as usize - 1) * journal::FIELDS;
    let slot = &neu_smp_work_journal[base..base + journal::FIELDS];
    slot[journal::KIND].store(command.kind.code(), Ordering::Relaxed);
    slot[journal::INPUT].store(command.input, Ordering::Relaxed);
    slot[journal::ROUNDS].store(command.rounds, Ordering::Relaxed);
    slot[journal::RESULT].store(result, Ordering::Relaxed);
    slot[journal::AUX].store(aux, Ordering::Relaxed);
    slot[journal::START].store(start, Ordering::Relaxed);
    slot[journal::MARKER].store(marker, Ordering::Relaxed);
    slot[journal::ACCEPT_ORDER].store(mailbox.counter(mailbox::ACCEPT_ORDER), Ordering::Relaxed);
    slot[journal::COMPLETE_ORDER]
        .store(mailbox.counter(mailbox::COMPLETE_ORDER), Ordering::Relaxed);
    slot[journal::SEQUENCE].store(sequence, Ordering::Release);
    Ok(())
}

#[cfg(feature = "smp-ipi-test")]
fn wait_command(executor: &Executor<'_>, mailbox: &Mailbox) -> Result<Command, Error> {
    loop {
        let baseline = mailbox.counter(mailbox::IPI_RECEIVED);
        mailbox.set_phase(Phase::IpiIdle);
        // SAFETY: The only AP interrupt window: IF=1 for one HLT instruction.
        // Every LVT is masked and the work/spurious gates use IST1.
        unsafe { neu_smp_work_idle_once() };
        let observed = mailbox.counter(mailbox::IPI_RECEIVED);
        if observed == baseline {
            continue;
        }
        if observed != baseline.wrapping_add(1) {
            return Err(Error::Sequence);
        }
        mailbox.set_phase(Phase::IpiBusy);
        match executor.accept(stack_pointer()) {
            Ok(Some(command)) => return Ok(command),
            Ok(None) => {
                mailbox.set_phase(Phase::IpiIdle);
                continue;
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(not(feature = "smp-ipi-test"))]
fn wait_command(executor: &Executor<'_>, _mailbox: &Mailbox) -> Result<Command, Error> {
    let mut spins = 0u64;
    loop {
        match executor.accept(stack_pointer()) {
            Ok(Some(command)) => return Ok(command),
            Ok(None) => {}
            Err(error) => return Err(error),
        }
        if spins >= WORK_WAIT_LIMIT {
            return Err(Error::NotReady);
        }
        core::hint::spin_loop();
        spins += 1;
    }
}

#[cfg(feature = "smp-ipi-test")]
fn idle_after_address() -> u64 {
    ptr::addr_of!(neu_smp_work_idle_after) as u64
}

#[cfg(feature = "smp-ipi-test")]
fn lapic_read(offset: u32) -> u32 {
    // SAFETY: LAPIC_VIEW is the BSP-mapped, owned-root UC device window; every
    // access is a 32-bit volatile register read of this CPU's local APIC.
    unsafe { ptr::read_volatile((paging::LAPIC_VIEW + u64::from(offset)) as *const u32) }
}

#[cfg(feature = "smp-ipi-test")]
fn lapic_write(offset: u32, value: u32) {
    // SAFETY: Same window; documented xAPIC register write.
    unsafe { ptr::write_volatile((paging::LAPIC_VIEW + u64::from(offset)) as *mut u32, value) }
}

#[cfg(feature = "smp-ipi-test")]
fn rdmsr(msr: u32) -> u64 {
    let (low, high): (u32, u32);
    // SAFETY: Ring 0 read of the named architectural MSR.
    unsafe {
        asm!("rdmsr", in("ecx") msr, out("eax") low, out("edx") high,
            options(nomem, nostack, preserves_flags));
    }
    (u64::from(high) << 32) | u64::from(low)
}

#[cfg(feature = "smp-ipi-test")]
fn lapic_physical() -> Result<u64, Error> {
    let base = rdmsr(0x1b);
    if base & (1 << 10) != 0 || base & (1 << 11) == 0 {
        return Err(Error::NotReady);
    }
    Ok(base & neu_kernel::paging::ADDRESS_FIELD)
}

#[cfg(feature = "smp-ipi-test")]
fn mask_all_lvts() {
    for offset in [0x320, 0x330, 0x340, 0x350, 0x360, 0x370] {
        lapic_write(offset, lapic_read(offset) | (1 << 16));
    }
}

#[cfg(feature = "smp-ipi-test")]
fn controller_idle() -> Result<(), Error> {
    if lapic_read(0x80) != 0 {
        return Err(Error::NotReady);
    }
    for offset in (0x100..=0x170).step_by(0x10) {
        if lapic_read(offset) != 0 {
            return Err(Error::Busy);
        }
    }
    for offset in (0x200..=0x270).step_by(0x10) {
        if lapic_read(offset) != 0 {
            return Err(Error::Busy);
        }
    }
    Ok(())
}

#[cfg(feature = "smp-ipi-test")]
fn initialize_ap_lapic(_mailbox: &Mailbox, apic_id: u32) -> Result<(), Error> {
    lapic_physical()?;
    let version = lapic_read(0x30);
    if version & 0xff < 0x14 || lapic_read(0x20) >> 24 != apic_id {
        return Err(Error::Identity);
    }
    mask_all_lvts();
    lapic_write(0x80, 0);
    lapic_write(0xf0, 0x1ff);
    if lapic_read(0xf0) & 0x1ff != 0x1ff || lapic_read(0xf0) & (1 << 8) == 0 {
        return Err(Error::Phase);
    }
    controller_idle()
}

#[cfg(feature = "smp-ipi-test")]
fn lapic_in_service(vector: u32) -> bool {
    // xAPIC ISR words are 16 bytes apart (0x100..0x170), not packed.
    let offset = 0x100 + 0x10 * (vector / 32);
    lapic_read(offset) & (1 << (vector % 32)) != 0
}

#[cfg(feature = "smp-ipi-test")]
fn lapic_eoi() {
    lapic_write(0xb0, 0);
}

#[cfg(feature = "smp-ipi-test")]
fn wait_isr_clear(vector: u32) -> bool {
    for _ in 0..1_000_000u64 {
        if !lapic_in_service(vector) {
            return true;
        }
        core::hint::spin_loop();
    }
    false
}

#[cfg(feature = "smp-ipi-test")]
fn wait_delivery_idle() -> Result<(), Error> {
    for _ in 0..1_000_000u64 {
        if lapic_read(0x300) & (1 << 12) == 0 {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err(Error::NotReady)
}

#[cfg(feature = "smp-ipi-test")]
fn send_ipi(target: u32) -> Result<(), Error> {
    wait_delivery_idle()?;
    lapic_write(0x310, target << 24);
    lapic_write(0x300, smp_work::WORK_IPI_VECTOR as u32 | (1 << 14));
    wait_delivery_idle()?;
    if lapic_read(0x300) & (1 << 12) != 0 {
        return Err(Error::NotReady);
    }
    Ok(())
}

/// Handler for vectors 0xF0 and 0xFF on an AP. It validates the exact idle
/// window, accounts every interruption, and EOIs exactly once for a work IPI.
#[unsafe(no_mangle)]
extern "C" fn neu_smp_work_ipi_handler(frame: *const ExceptionFrame) {
    // SAFETY: The assembly stub passes the live IST frame it just built.
    let frame = unsafe { &*frame };
    let Some(block) = smp::current_ap() else {
        smp::silent_park();
    };
    let Some(logical) = smp::current_logical() else {
        smp::silent_park();
    };
    if logical == 0 || logical >= cpu::MAX_CPUS {
        smp::silent_park();
    }
    let mailbox = &neu_smp_work_mailboxes[logical];
    if mailbox.logical() != logical as u64 || mailbox.apic_id() != block.load(cpu::block::APIC_ID) {
        ap_terminal(block, mailbox, Error::Identity);
    }
    #[cfg(feature = "smp-ipi-test")]
    {
        let in_service = lapic_in_service(frame.vector as u32);
        match smp_work::classify(frame.vector, in_service) {
            smp_work::Interrupt::WorkIpi => {
                let stack = cpu::Region {
                    bottom: block.load(cpu::block::STACK_BOTTOM),
                    top: block.load(cpu::block::STACK_TOP),
                };
                if frame.cs != 0x08
                    || frame.flags & 0x200 == 0
                    || frame.rip != idle_after_address()
                    || !stack.contains(frame.rsp)
                    || mailbox.phase() != Ok(Phase::IpiIdle)
                {
                    ap_terminal(block, mailbox, Error::Phase);
                }
                mailbox.set_phase(Phase::IpiBusy);
                lapic_eoi();
                if !wait_isr_clear(smp_work::WORK_IPI_VECTOR as u32) {
                    ap_terminal(block, mailbox, Error::NotReady);
                }
                mailbox.add_counter(mailbox::IPI_RECEIVED, 1);
                mailbox.add_counter(mailbox::IPI_EOIS, 1);
            }
            smp_work::Interrupt::Spurious => {
                if mailbox.phase() != Ok(Phase::IpiIdle) {
                    ap_terminal(block, mailbox, Error::Phase);
                }
                // A spurious vector has no in-service bit; never EOI it.
                mailbox.add_counter(mailbox::SPURIOUS, 1);
            }
            smp_work::Interrupt::Unexpected => {
                ap_terminal(block, mailbox, Error::State);
            }
        }
    }
    #[cfg(not(feature = "smp-ipi-test"))]
    {
        let _ = frame;
        ap_terminal(block, mailbox, Error::State);
    }
}

/// Maps the shared LAPIC window and arms the BSP's local APIC for sending.
/// Interrupts stay disabled on the BSP for the entire work profile.
#[cfg(feature = "smp-ipi-test")]
pub fn map_lapic(paging: &mut paging::Paging<'_, '_>) -> Result<(), &'static str> {
    if !smp::is_bsp() || current_root() != paging.root() || interrupt_flag() {
        return Err("SMP work IPI mapping requires the BSP on the owned root with IF=0");
    }
    let physical = lapic_physical().map_err(map_error)?;
    let pat = rdmsr(0x277);
    paging
        .map_lapic_uc(physical, pat)
        .map_err(|_| "SMP work LAPIC device mapping failed")?;
    let bsp_apic = apic_of(0)?;
    let version = lapic_read(0x30);
    if version & 0xff < 0x14 || lapic_read(0x20) >> 24 != bsp_apic || lapic_read(0x20) >> 24 == 0xff
    {
        return Err("SMP work BSP local APIC identity is invalid");
    }
    mask_all_lvts();
    lapic_write(0x80, 0);
    lapic_write(0xf0, 0x1ff);
    if lapic_read(0xf0) != 0x1ff || controller_idle().is_err() {
        return Err("SMP work BSP local APIC did not initialize");
    }
    log(format_args!(
        "[NEU] SMP work IPI armed: vector={:#x} apic={} physical={:#x} x2apic=false\n",
        smp_work::WORK_IPI_VECTOR,
        bsp_apic,
        physical
    ))
}

pub fn run(paging: &paging::Paging<'_, '_>) -> Result<(), &'static str> {
    if !smp::is_bsp() {
        return Err("SMP work driver must execute on the BSP");
    }
    if current_root() != paging.root() {
        return Err("SMP work driver is not executing on the owned root");
    }
    if interrupt_flag() {
        return Err("SMP work driver requires interrupts disabled");
    }
    record(evidence::MAGIC, EVIDENCE_MAGIC);
    record(evidence::ABI, smp_work::ABI);
    record(
        evidence::MODE,
        if cfg!(feature = "smp-ipi-test") { 2 } else { 1 },
    );
    record(evidence::CPUS, CPUS as u64);
    record(evidence::PER_AP, work_commands() as u64);
    #[cfg(feature = "smp-ipi-test")]
    verify_lapic_view(paging)?;
    log(format_args!(
        "[NEU] SMP work armed: mode={} cpus={CPUS} aps={} profile={:?}\n",
        if cfg!(feature = "smp-ipi-test") {
            "ipi"
        } else {
            "poll"
        },
        CPUS - 1,
        PROFILE
    ))?;
    match drive() {
        Ok(()) => {}
        Err(reason) => {
            record(evidence::FAILURE, 1);
            return Err(reason);
        }
    }
    finalize_aps()?;
    record(evidence::AP_STOPPED, (CPUS - 1) as u64);
    log(format_args!(
        "[NEU] SMP work complete: mode={} cpus={CPUS} aps={} per_ap={} commands={} stopped={} ipi_sent={} ipi_received={} eois={} spurious={} overlap={:#x}\n",
        if cfg!(feature = "smp-ipi-test") {
            "ipi"
        } else {
            "poll"
        },
        CPUS - 1,
        work_commands(),
        neu_smp_work_evidence[evidence::COMMANDS].load(Ordering::Relaxed),
        neu_smp_work_evidence[evidence::AP_STOPPED].load(Ordering::Relaxed),
        neu_smp_work_evidence[evidence::IPI_SENT].load(Ordering::Relaxed),
        neu_smp_work_evidence[evidence::IPI_RECEIVED].load(Ordering::Relaxed),
        neu_smp_work_evidence[evidence::IPI_EOIS].load(Ordering::Relaxed),
        neu_smp_work_evidence[evidence::SPURIOUS].load(Ordering::Relaxed),
        neu_smp_work_evidence[evidence::GATE_OVERLAP].load(Ordering::Relaxed),
    ))?;
    let bsp = &smp::neu_smp_cpus[0];
    if bsp.load(cpu::block::STATE) != Bootstrap::Ready.code()
        || bsp.load(cpu::block::HISTORY)
            & (Bootstrap::Prepared.bit() | Bootstrap::Entered.bit() | Bootstrap::Ready.bit())
            != (Bootstrap::Prepared.bit() | Bootstrap::Entered.bit() | Bootstrap::Ready.bit())
    {
        return Err("SMP work BSP is not in the published ready state");
    }
    if bsp.advance(Bootstrap::Parked).is_err() {
        return Err("SMP work BSP could not publish its parked state");
    }
    Ok(())
}

#[cfg(feature = "smp-ipi-test")]
fn verify_lapic_view(paging: &paging::Paging<'_, '_>) -> Result<(), &'static str> {
    let physical = lapic_physical().map_err(map_error)?;
    let view = paging
        .translate(paging::LAPIC_VIEW)
        .map_err(|_| "SMP work LAPIC view translation failed")?
        .ok_or("SMP work LAPIC view is not mapped")?;
    if view.physical != physical
        || view.user
        || view.permissions.execute
        || !view.permissions.write
        || view.page_size != 4096
    {
        return Err("SMP work LAPIC view has unexpected mappings");
    }
    Ok(())
}

fn drive() -> Result<(), &'static str> {
    match PROFILE {
        Profile::Work2 => drive_sequential(&WORK2),
        Profile::Work4 => drive_parallel(&WORK4),
        Profile::Overlap => drive_overlap(),
        Profile::Repeat => drive_sequential(&REPEAT),
        Profile::Invalid => drive_invalid(),
        Profile::Ipi2 => drive_sequential(&IPI2),
        Profile::Ipi4 => drive_parallel(&IPI4),
    }
}

fn drive_sequential(commands: &[Command]) -> Result<(), &'static str> {
    let logical = 1usize;
    let apic = apic_of(logical)?;
    for command in commands {
        let sequence = publish(logical, apic, *command)?;
        wake(logical, apic, sequence)?;
        collect(logical, apic, sequence, command)?;
    }
    stop_ap(logical, apic)
}

fn drive_parallel(commands: &[Command]) -> Result<(), &'static str> {
    let mut sequences = [0u64; cpu::MAX_CPUS];
    for (logical, slot) in sequences.iter_mut().enumerate().take(CPUS).skip(1) {
        *slot = publish(logical, apic_of(logical)?, commands[logical - 1])?;
    }
    for (logical, sequence) in sequences.iter().enumerate().take(CPUS).skip(1) {
        wake(logical, apic_of(logical)?, *sequence)?;
    }
    for logical in 1..CPUS {
        wait_executing(logical)?;
    }
    log(format_args!(
        "[NEU] Work checkpoint: phase=parallel cpus={CPUS} aps={} sequences={} {} {}\n",
        CPUS - 1,
        sequences[1],
        sequences.get(2).copied().unwrap_or(0),
        sequences.get(3).copied().unwrap_or(0)
    ))?;
    // SAFETY: Bounded CPU-local observation window; the APs are inside their
    // own matching dwell and the BSP holds no locks.
    unsafe { neu_smp_work_bsp_dwell_begin() };
    for (logical, sequence) in sequences.iter().enumerate().take(CPUS).skip(1) {
        collect(
            logical,
            apic_of(logical)?,
            *sequence,
            &commands[logical - 1],
        )?;
    }
    for logical in 1..CPUS {
        stop_ap(logical, apic_of(logical)?)?;
    }
    Ok(())
}

fn drive_overlap() -> Result<(), &'static str> {
    let logical = 1usize;
    let apic = apic_of(logical)?;
    let gate = &neu_smp_work_gate;
    let epoch = gate.open(2).map_err(map_error)?;
    record(evidence::GATE_EPOCH, epoch);
    let sequence = publish(logical, apic, OVERLAP)?;
    wake(logical, apic, sequence)?;
    gate.arrive(0, epoch).map_err(map_error)?;
    let mut spins = 0u64;
    while !gate.all_ready() {
        if gate.failure() != 0 || spins >= WORK_WAIT_LIMIT {
            return Err("SMP work gate did not reach the two-phase checkpoint");
        }
        core::hint::spin_loop();
        spins += 1;
    }
    let overlap = gate.release().map_err(map_error)?;
    if overlap != 0b11 {
        return Err("SMP work gate overlap witness is incomplete");
    }
    record(evidence::GATE_OVERLAP, overlap);
    log(format_args!(
        "[NEU] Work checkpoint: phase=gate epoch={epoch} overlap={overlap:#x} sequence={sequence}\n"
    ))?;
    // SAFETY: Bounded CPU-local checkpoint window while the AP waits inside
    // the exported gate loop and the phase is still `Checkpoint`.
    unsafe { neu_smp_work_bsp_dwell_begin() };
    if gate.phase() != Ok(GatePhase::Checkpoint) {
        return Err("SMP work gate changed before the BSP checkpoint finished");
    }
    gate.publish_release().map_err(map_error)?;
    let (result, aux) =
        smp_work::execute(OVERLAP.kind, OVERLAP.input, OVERLAP.rounds).map_err(map_error)?;
    record(evidence::BSP_RESULT, result);
    record(evidence::BSP_AUX, aux);
    let completion = collect(logical, apic, sequence, &OVERLAP)?;
    if (completion.result, completion.aux) != (result, aux) {
        return Err("SMP work AP and BSP results differ for identical input");
    }
    gate.depart(0, epoch).map_err(map_error)?;
    let mut spins = 0u64;
    while !gate.all_done() {
        if spins >= WORK_WAIT_LIMIT {
            return Err("SMP work gate participants did not depart");
        }
        core::hint::spin_loop();
        spins += 1;
    }
    gate.complete().map_err(map_error)?;
    record(evidence::RELEASE, epoch);
    stop_ap(logical, apic)
}

fn drive_invalid() -> Result<(), &'static str> {
    let logical = 1usize;
    let apic = apic_of(logical)?;
    let mailbox = &neu_smp_work_mailboxes[logical];
    let publisher = Publisher::attach(mailbox, logical, apic).map_err(map_error)?;
    let sequence = publisher.publish(INVALID).map_err(map_error)?;
    log(format_args!(
        "[NEU] Work command: logical={logical} apic={apic} sequence={sequence} kind={} input={:#x} rounds={}\n",
        INVALID.kind.code(),
        INVALID.input,
        INVALID.rounds
    ))?;
    let mut checks = 0u64;
    let mut passed = 0u64;
    negative(
        &mut checks,
        &mut passed,
        "duplicate-publish",
        publisher.publish(INVALID),
        Error::Busy,
    )?;
    negative(
        &mut checks,
        &mut passed,
        "invalid-rounds",
        publisher.publish(Command::new(CommandKind::Transform, 1, 0)),
        Error::InvalidRounds,
    )?;
    negative_result(
        &mut checks,
        &mut passed,
        "identity-mismatch",
        Publisher::attach(mailbox, logical, apic + 1).err(),
        Error::Identity,
    )?;
    let completion = wait_completion(&publisher, sequence)?;
    let (result, aux) =
        smp_work::execute(INVALID.kind, INVALID.input, INVALID.rounds).map_err(map_error)?;
    if (completion.result, completion.aux) != (result, aux) {
        return Err("SMP work invalid-profile completion differs from the deterministic result");
    }
    log(format_args!(
        "[NEU] Work result: logical={logical} apic={apic} sequence={sequence} kind={} start={:#x} marker={:#x} result={result:#x} aux={aux:#x} executions={} mask={:#x}\n",
        INVALID.kind.code(),
        completion.start_marker,
        completion.marker,
        completion.executions,
        mailbox.execution_mask()
    ))?;
    negative(
        &mut checks,
        &mut passed,
        "stale-sequence",
        publisher.poll(sequence + 1),
        Error::Sequence,
    )?;
    negative(
        &mut checks,
        &mut passed,
        "stale-ack",
        publisher.acknowledge(sequence + 1),
        Error::StaleAck,
    )?;
    publisher.acknowledge(sequence).map_err(map_error)?;
    negative(
        &mut checks,
        &mut passed,
        "replayed-poll",
        publisher.poll(sequence),
        Error::State,
    )?;
    record(evidence::NEGATIVE_CHECKS, checks);
    record(evidence::NEGATIVE_PASSED, passed);
    if checks != passed || passed != 6 {
        return Err("SMP work negative checks did not all reject the expected error");
    }
    log(format_args!(
        "[NEU] Work negative checks: consecutive={checks} passed={passed} sequence={sequence} result={result:#x}\n"
    ))?;
    add(evidence::COMMANDS, 1);
    stop_ap(logical, apic)
}

fn negative<T>(
    checks: &mut u64,
    passed: &mut u64,
    name: &str,
    result: Result<T, Error>,
    expected: Error,
) -> Result<(), &'static str> {
    *checks += 1;
    match result {
        Err(actual) if actual == expected => {
            *passed += 1;
            log(format_args!(
                "[NEU] Work negative check: name={name} rejected={} expected={}\n",
                actual.code(),
                expected.code()
            ))
        }
        Err(actual) => {
            let _ = log(format_args!(
                "[NEU] Work negative mismatch: name={name} rejected={} expected={}\n",
                actual.code(),
                expected.code()
            ));
            Err("SMP work negative check rejected an unexpected error")
        }
        Ok(_) => {
            let _ = log(format_args!(
                "[NEU] Work negative mismatch: name={name} accepted expected={}\n",
                expected.code()
            ));
            Err("SMP work negative check did not reject the requested operation")
        }
    }
}

fn negative_result(
    checks: &mut u64,
    passed: &mut u64,
    name: &str,
    rejection: Option<Error>,
    expected: Error,
) -> Result<(), &'static str> {
    // `Option<Error>` is the rejection itself: Some means the operation was
    // refused with that error, so it must map to Err, never Ok.
    negative::<()>(
        checks,
        passed,
        name,
        Err(rejection.unwrap_or(Error::NotReady)),
        expected,
    )
}

fn publish(logical: usize, apic: u32, command: Command) -> Result<u64, &'static str> {
    let mailbox = &neu_smp_work_mailboxes[logical];
    #[cfg(feature = "smp-ipi-test")]
    wait_phase(mailbox, Phase::IpiIdle).map_err(map_error)?;
    let publisher = Publisher::attach(mailbox, logical, apic).map_err(map_error)?;
    let sequence = publisher.publish(command).map_err(map_error)?;
    log(format_args!(
        "[NEU] Work command: logical={logical} apic={apic} sequence={sequence} kind={} input={:#x} rounds={}\n",
        command.kind.code(),
        command.input,
        command.rounds
    ))?;
    Ok(sequence)
}

#[cfg(feature = "smp-ipi-test")]
fn wake(logical: usize, apic: u32, sequence: u64) -> Result<(), &'static str> {
    let mailbox = &neu_smp_work_mailboxes[logical];
    wait_phase(mailbox, Phase::IpiIdle).map_err(map_error)?;
    send_ipi(apic).map_err(map_error)?;
    add(evidence::IPI_SENT, 1);
    wait_ipi(mailbox, sequence).map_err(map_error)
}

#[cfg(not(feature = "smp-ipi-test"))]
fn wake(_logical: usize, _apic: u32, _sequence: u64) -> Result<(), &'static str> {
    Ok(())
}

#[cfg(feature = "smp-ipi-test")]
fn wait_phase(mailbox: &Mailbox, phase: Phase) -> Result<(), Error> {
    let mut spins = 0u64;
    loop {
        match mailbox.phase()? {
            Phase::Failed => return Err(Error::Failed),
            current if current == phase => return Ok(()),
            _ => {}
        }
        if spins >= WORK_WAIT_LIMIT {
            return Err(Error::NotReady);
        }
        core::hint::spin_loop();
        spins += 1;
    }
}

#[cfg(feature = "smp-ipi-test")]
fn wait_ipi(mailbox: &Mailbox, expected: u64) -> Result<(), Error> {
    let mut spins = 0u64;
    loop {
        let received = mailbox.counter(mailbox::IPI_RECEIVED);
        if received > expected {
            return Err(Error::Sequence);
        }
        if received == expected {
            return Ok(());
        }
        if mailbox.phase()? == Phase::Failed {
            return Err(Error::Failed);
        }
        if spins >= WORK_WAIT_LIMIT {
            return Err(Error::NotReady);
        }
        core::hint::spin_loop();
        spins += 1;
    }
}

fn wait_completion(publisher: &Publisher<'_>, sequence: u64) -> Result<Completion, &'static str> {
    let mut spins = 0u64;
    loop {
        match publisher.poll(sequence) {
            Ok(Poll::Pending) => {}
            Ok(Poll::Completed(completion)) => return Ok(completion),
            Err(error) => return Err(map_error(error)),
        }
        if spins >= WORK_WAIT_LIMIT {
            return Err("SMP work AP did not complete within the bounded window");
        }
        core::hint::spin_loop();
        spins += 1;
    }
}

fn collect(
    logical: usize,
    apic: u32,
    sequence: u64,
    command: &Command,
) -> Result<Completion, &'static str> {
    let mailbox = &neu_smp_work_mailboxes[logical];
    let publisher = Publisher::attach(mailbox, logical, apic).map_err(map_error)?;
    let completion = wait_completion(&publisher, sequence)?;
    let (result, aux) = match command.kind {
        CommandKind::Stop => (0, 0),
        _ => smp_work::execute(command.kind, command.input, command.rounds).map_err(map_error)?,
    };
    if completion.sequence != sequence
        || completion.logical != logical
        || completion.apic_id != apic
        || completion.executions != sequence
        || (completion.result, completion.aux) != (result, aux)
    {
        return Err("SMP work completion does not match the published command");
    }
    publisher.acknowledge(sequence).map_err(map_error)?;
    #[cfg(feature = "smp-ipi-test")]
    if command.kind != CommandKind::Stop {
        // The stop transition ends in `Stopped`; only work commands return to
        // the documented idle window.
        wait_phase(mailbox, Phase::IpiIdle).map_err(map_error)?;
    }
    if command.kind != CommandKind::Stop {
        add(evidence::COMMANDS, 1);
    }
    log(format_args!(
        "[NEU] Work result: logical={logical} apic={apic} sequence={sequence} kind={} start={:#x} marker={:#x} result={:#x} aux={:#x} executions={} mask={:#x}\n",
        command.kind.code(),
        completion.start_marker,
        completion.marker,
        completion.result,
        completion.aux,
        completion.executions,
        mailbox.execution_mask()
    ))?;
    Ok(completion)
}

fn stop_ap(logical: usize, apic: u32) -> Result<(), &'static str> {
    let mailbox = &neu_smp_work_mailboxes[logical];
    let command = Command::new(CommandKind::Stop, 0, 0);
    let sequence = publish(logical, apic, command)?;
    wake(logical, apic, sequence)?;
    collect(logical, apic, sequence, &command)?;
    let mut spins = 0u64;
    loop {
        let state = Bootstrap::from_code(smp::neu_smp_cpus[logical].load(cpu::block::STATE));
        if state == Some(Bootstrap::Parked) {
            break;
        }
        if state == Some(Bootstrap::Failed) {
            return Err("SMP work AP failed before its final park");
        }
        if spins >= WORK_WAIT_LIMIT {
            return Err("SMP work AP did not park within the bounded window");
        }
        core::hint::spin_loop();
        spins += 1;
    }
    if mailbox.phase() != Ok(Phase::Stopped)
        || mailbox.executions() != sequence
        || mailbox.execution_mask() != execution_mask(sequence)
    {
        return Err("SMP work AP did not publish its final stopped state");
    }
    #[cfg(feature = "smp-ipi-test")]
    if mailbox.counter(mailbox::IPI_RECEIVED) != sequence
        || mailbox.counter(mailbox::IPI_EOIS) != sequence
        || mailbox.counter(mailbox::SPURIOUS) != 0
    {
        return Err("SMP work IPI accounting does not match the published commands");
    }
    add(evidence::AP_STOPPED, 1);
    log(format_args!(
        "[NEU] Work AP parked: logical={logical} apic={apic} sequence={sequence} executions={} mask={:#x} ipi_received={} eois={} spurious={}\n",
        mailbox.executions(),
        mailbox.execution_mask(),
        mailbox.counter(mailbox::IPI_RECEIVED),
        mailbox.counter(mailbox::IPI_EOIS),
        mailbox.counter(mailbox::SPURIOUS)
    ))?;
    Ok(())
}

fn finalize_aps() -> Result<(), &'static str> {
    let expected = (work_commands() + 1) as u64;
    let mut received = 0u64;
    let mut eois = 0u64;
    let mut spurious = 0u64;
    for (logical, mailbox) in neu_smp_work_mailboxes.iter().enumerate().take(CPUS).skip(1) {
        let block = &smp::neu_smp_cpus[logical];
        let stack = cpu::Region {
            bottom: block.load(cpu::block::STACK_BOTTOM),
            top: block.load(cpu::block::STACK_TOP),
        };
        if Bootstrap::from_code(block.load(cpu::block::STATE)) != Some(Bootstrap::Parked)
            || block.load(cpu::block::HISTORY) & full_bootstrap_history()
                != full_bootstrap_history()
            || block.load(cpu::block::PARK_COUNT) != 1
            || block.load(cpu::block::FAILURE) != 0
            || block.load(cpu::block::FAULT_VECTOR) != 0
            || block.load(cpu::block::ISOLATION) != cpu::block::ISOLATION_AP
            || !stack.contains(block.load(cpu::block::READY_RSP))
            || !stack.contains(block.load(cpu::block::PARK_RSP))
        {
            return Err("SMP work AP final CPU block is not cleanly parked");
        }
        if mailbox.phase() != Ok(Phase::Stopped)
            || mailbox.state() != Ok(State::Idle)
            || mailbox.sequence() != expected
            || mailbox.acknowledged() != expected
            || mailbox.executions() != expected
            || mailbox.execution_mask() != execution_mask(expected)
            || mailbox.counter(mailbox::FAILURE) != 0
            || mailbox.counter(mailbox::REJECTIONS) != 0
        {
            return Err("SMP work AP final mailbox is not exactly-once consistent");
        }
        received += mailbox.counter(mailbox::IPI_RECEIVED);
        eois += mailbox.counter(mailbox::IPI_EOIS);
        spurious += mailbox.counter(mailbox::SPURIOUS);
        if !cfg!(feature = "smp-ipi-test")
            && (mailbox.counter(mailbox::IPI_RECEIVED) != 0
                || mailbox.counter(mailbox::IPI_EOIS) != 0
                || mailbox.counter(mailbox::SPURIOUS) != 0)
        {
            return Err("SMP work polling AP recorded an unexpected interrupt");
        }
    }
    record(evidence::IPI_RECEIVED, received);
    record(evidence::IPI_EOIS, eois);
    record(evidence::SPURIOUS, spurious);
    let sent = neu_smp_work_evidence[evidence::IPI_SENT].load(Ordering::Relaxed);
    if cfg!(feature = "smp-ipi-test") && (sent != received || received != eois || spurious != 0) {
        return Err("SMP work IPI evidence is inconsistent across the AP set");
    }
    if !cfg!(feature = "smp-ipi-test") && sent != 0 {
        return Err("SMP work polling profile sent an unexpected IPI");
    }
    Ok(())
}

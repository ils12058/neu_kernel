//! Phase 9B cross-CPU work protocol. One fixed-capacity mailbox per possible
//! application processor carries a single command at a time; the BSP is the
//! only publisher/acknowledger and the owning AP is the only acceptor and
//! completer. Sequence numbers and role checks make publication, acceptance
//! and completion exactly-once, and a two-phase gate supplies the common
//! barrier used by the overlap profile.
//!
//! This module contains no architecture-specific code and no global mutable
//! state. The x86-64 backend embeds the same types in production BSS and the
//! host tests drive the identical methods.

use core::sync::atomic::{AtomicU64, Ordering};

use crate::cpu::MAX_CPUS;

pub const ABI: u64 = 1;

/// Fixed physical-address-free witnesses shared with the QMP observer.
pub const MAILBOX_MAGIC: u64 = 0x4e45_555f_4d42_5831; // "NEU_MBX1"
pub const MAILBOX_FIELDS: usize = 32;
pub const MAILBOX_STRIDE: usize = MAILBOX_FIELDS * 8;

pub mod mailbox {
    pub const MAGIC: usize = 0;
    pub const LOGICAL: usize = 1;
    pub const APIC_ID: usize = 2;
    pub const PHASE: usize = 3;
    pub const STATE: usize = 4;
    pub const SEQUENCE: usize = 5;
    pub const ACKED: usize = 6;
    pub const KIND: usize = 7;
    pub const INPUT: usize = 8;
    pub const ROUNDS: usize = 9;
    pub const PUBLISH_ORDER: usize = 10;
    pub const ACCEPT_SEQUENCE: usize = 11;
    pub const ACCEPT_ORDER: usize = 12;
    pub const ACCEPT_LOGICAL: usize = 13;
    pub const ACCEPT_APIC: usize = 14;
    pub const START_MARKER: usize = 15;
    pub const START_RSP: usize = 16;
    pub const COMPLETE_SEQUENCE: usize = 17;
    pub const COMPLETE_LOGICAL: usize = 18;
    pub const COMPLETE_APIC: usize = 19;
    pub const COMPLETE_MARKER: usize = 20;
    pub const RESULT: usize = 21;
    pub const RESULT_AUX: usize = 22;
    pub const EXECUTIONS: usize = 23;
    pub const EXEC_MASK: usize = 24;
    pub const COMPLETE_ORDER: usize = 25;
    pub const FAILURE: usize = 26;
    pub const REJECTIONS: usize = 27;
    pub const LAST_REJECT: usize = 28;
    pub const IPI_RECEIVED: usize = 29;
    pub const IPI_EOIS: usize = 30;
    pub const SPURIOUS: usize = 31;
}

pub const GATE_MAGIC: u64 = 0x4e45_555f_4741_5431; // "NEU_GAT1"
pub const GATE_HEADER_FIELDS: usize = 8;
pub const GATE_CPU_FIELDS: usize = 4;
pub const GATE_CPU_STRIDE: usize = GATE_CPU_FIELDS;
pub const GATE_WORDS: usize = GATE_HEADER_FIELDS + GATE_CPU_FIELDS * MAX_CPUS;
pub const GATE_STRIDE: usize = GATE_WORDS * 8;

pub mod gate {
    pub const MAGIC: usize = 0;
    pub const EPOCH: usize = 1;
    pub const PARTICIPANTS: usize = 2;
    pub const PHASE: usize = 3;
    pub const RELEASE: usize = 4;
    pub const OVERLAP: usize = 5;
    pub const STOP: usize = 6;
    pub const FAILURE: usize = 7;
    pub const ENTERED: usize = 0;
    pub const READY: usize = 1;
    pub const DONE: usize = 2;
    pub const RESERVED: usize = 3;
}

/// Upper bound on the deterministic work loop. Images use small counts; the
/// protocol rejects anything larger before publication.
pub const MAX_ROUNDS: u64 = 1 << 20;

/// Work IPI and architecturally spurious vectors.
pub const WORK_IPI_VECTOR: u64 = 0xf0;
pub const SPURIOUS_VECTOR: u64 = 0xff;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Offline = 0,
    Ready = 1,
    IpiIdle = 2,
    Stopped = 3,
    Failed = 4,
    IpiBusy = 5,
}

impl Phase {
    pub const fn code(self) -> u64 {
        self as u64
    }

    pub const fn from_code(code: u64) -> Option<Self> {
        Some(match code {
            0 => Self::Offline,
            1 => Self::Ready,
            2 => Self::IpiIdle,
            3 => Self::Stopped,
            4 => Self::Failed,
            5 => Self::IpiBusy,
            _ => return None,
        })
    }

    const fn online(self) -> bool {
        matches!(self, Self::Ready | Self::IpiIdle | Self::IpiBusy)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Idle = 0,
    Published = 1,
    Executing = 2,
    Completed = 3,
    Failed = 4,
}

impl State {
    pub const fn code(self) -> u64 {
        self as u64
    }

    pub const fn from_code(code: u64) -> Option<Self> {
        Some(match code {
            0 => Self::Idle,
            1 => Self::Published,
            2 => Self::Executing,
            3 => Self::Completed,
            4 => Self::Failed,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandKind {
    Transform = 1,
    Checksum = 2,
    Stop = 3,
    FaultProbe = 4,
}

impl CommandKind {
    pub const fn code(self) -> u64 {
        self as u64
    }

    pub const fn from_code(code: u64) -> Option<Self> {
        Some(match code {
            1 => Self::Transform,
            2 => Self::Checksum,
            3 => Self::Stop,
            4 => Self::FaultProbe,
            _ => return None,
        })
    }

    const fn executes_rounds(self) -> bool {
        matches!(self, Self::Transform | Self::Checksum)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Command {
    pub kind: CommandKind,
    pub input: u64,
    pub rounds: u64,
}

impl Command {
    pub const fn new(kind: CommandKind, input: u64, rounds: u64) -> Self {
        Self {
            kind,
            input,
            rounds,
        }
    }

    fn valid(&self) -> bool {
        if self.kind.executes_rounds() {
            (1..=MAX_ROUNDS).contains(&self.rounds)
        } else {
            self.rounds == 0
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Magic,
    Identity,
    Logical,
    Phase,
    State,
    Sequence,
    SequenceOverflow,
    InvalidKind,
    InvalidRounds,
    Busy,
    StaleAck,
    Failed,
    NotReady,
}

impl Error {
    pub const fn code(self) -> u64 {
        self as u64 + 1
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Completion {
    pub sequence: u64,
    pub logical: usize,
    pub apic_id: u32,
    pub start_marker: u64,
    pub marker: u64,
    pub result: u64,
    pub aux: u64,
    pub executions: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Poll {
    Pending,
    Completed(Completion),
}

/// Deterministic 64-bit finalizer. The QMP observer reimplements this exact
/// function, so it must not change without changing the observer and ADR.
pub const fn mix(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9e37_79b9_7f4a_7c15);
    value = (value ^ (value >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    value ^ (value >> 31)
}

pub const fn start_marker(sequence: u64, logical: usize, apic_id: u32) -> u64 {
    mix(sequence ^ ((logical as u64) << 32) ^ apic_id as u64)
}

pub const fn complete_marker(start: u64, result: u64, aux: u64) -> u64 {
    mix(start ^ result ^ aux.rotate_left(17))
}

/// Documented deterministic payload. `Transform` is a mixed LCG/xorshift
/// chain; `Checksum` is a 64-bit FNV-1a chain. Both return the final state and
/// a second accumulator so a result cannot be reproduced by a single constant.
pub fn execute(kind: CommandKind, input: u64, rounds: u64) -> Result<(u64, u64), Error> {
    if !(1..=MAX_ROUNDS).contains(&rounds) {
        return Err(Error::InvalidRounds);
    }
    match kind {
        CommandKind::Transform => {
            let mut state = input ^ 0x243f_6a88_85a3_08d3;
            let mut aux = 0u64;
            for index in 0..rounds {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1_442_695_040_888_963_407);
                state ^= state >> 29;
                aux = aux.wrapping_add(state ^ index);
            }
            Ok((state, aux))
        }
        CommandKind::Checksum => {
            let mut state = 0xcbf2_9ce4_8422_2325u64;
            for index in 0..rounds {
                state ^= input.wrapping_add(index);
                state = state.wrapping_mul(0x0000_0100_0000_01b3);
            }
            Ok((state, rounds))
        }
        CommandKind::Stop | CommandKind::FaultProbe => Err(Error::InvalidKind),
    }
}

fn state_of(code: u64) -> Result<State, Error> {
    State::from_code(code).ok_or(Error::State)
}

fn phase_of(code: u64) -> Result<Phase, Error> {
    Phase::from_code(code).ok_or(Error::Phase)
}

/// One cache-line-separated mailbox. The QMP observer reads the same words
/// through the exported layout descriptor; no pointer-bearing fields exist.
#[repr(C, align(64))]
pub struct Mailbox {
    words: [AtomicU64; MAILBOX_FIELDS],
}

const _: () = assert!(core::mem::size_of::<Mailbox>() == MAILBOX_STRIDE);
const _: () = assert!(core::mem::align_of::<Mailbox>() == 64);

impl Default for Mailbox {
    fn default() -> Self {
        Self::new()
    }
}

impl Mailbox {
    pub const fn new() -> Self {
        Self {
            words: [const { AtomicU64::new(0) }; MAILBOX_FIELDS],
        }
    }

    fn store(&self, field: usize, value: u64) {
        self.words[field].store(value, Ordering::Relaxed);
    }

    fn load(&self, field: usize) -> u64 {
        self.words[field].load(Ordering::Acquire)
    }

    pub fn magic(&self) -> u64 {
        self.load(mailbox::MAGIC)
    }

    pub fn logical(&self) -> u64 {
        self.load(mailbox::LOGICAL)
    }

    pub fn apic_id(&self) -> u64 {
        self.load(mailbox::APIC_ID)
    }

    pub fn phase(&self) -> Result<Phase, Error> {
        phase_of(self.load(mailbox::PHASE))
    }

    pub fn state(&self) -> Result<State, Error> {
        state_of(self.load(mailbox::STATE))
    }

    pub fn sequence(&self) -> u64 {
        self.load(mailbox::SEQUENCE)
    }

    pub fn acknowledged(&self) -> u64 {
        self.load(mailbox::ACKED)
    }

    /// The owning AP publishes its identity before any BSP observation. All
    /// preceding stores become visible through the Release phase write.
    pub fn initialize(&self, logical: usize, apic_id: u32) {
        debug_assert!(logical > 0 && logical < MAX_CPUS);
        debug_assert!(apic_id < crate::cpu::XAPIC_ID_LIMIT);
        self.store(mailbox::LOGICAL, logical as u64);
        self.store(mailbox::APIC_ID, u64::from(apic_id));
        self.store(mailbox::MAGIC, MAILBOX_MAGIC);
        self.words[mailbox::PHASE].store(Phase::Ready.code(), Ordering::Release);
    }

    pub fn set_phase(&self, phase: Phase) {
        self.words[mailbox::PHASE].store(phase.code(), Ordering::Release);
    }

    fn identity(&self, logical: usize, apic_id: u32) -> Result<(), Error> {
        if self.magic() != MAILBOX_MAGIC {
            return Err(Error::Magic);
        }
        if self.logical() != logical as u64 {
            return Err(Error::Logical);
        }
        if self.apic_id() != u64::from(apic_id) {
            return Err(Error::Identity);
        }
        Ok(())
    }

    fn record_rejection(&self, error: Error) {
        self.words[mailbox::REJECTIONS].fetch_add(1, Ordering::Relaxed);
        self.store(mailbox::LAST_REJECT, error.code());
    }

    /// The AP terminalizes its own mailbox after a protocol violation. The
    /// state is observable by the BSP and by QMP, and is never reopened.
    pub fn fail(&self, error: Error) {
        self.store(mailbox::FAILURE, error.code());
        self.words[mailbox::STATE].store(State::Failed.code(), Ordering::Release);
        self.set_phase(Phase::Failed);
    }

    pub fn executions(&self) -> u64 {
        self.load(mailbox::EXECUTIONS)
    }

    pub fn execution_mask(&self) -> u64 {
        self.load(mailbox::EXEC_MASK)
    }

    pub fn result(&self) -> (u64, u64) {
        (self.load(mailbox::RESULT), self.load(mailbox::RESULT_AUX))
    }

    pub fn counter(&self, field: usize) -> u64 {
        self.load(field)
    }

    pub fn add_counter(&self, field: usize, value: u64) {
        self.words[field].fetch_add(value, Ordering::AcqRel);
    }
}

/// BSP-side endpoint. Construction requires the published AP identity, so a
/// stale or misdirected target can never be published to.
pub struct Publisher<'a> {
    mailbox: &'a Mailbox,
    logical: usize,
    apic_id: u32,
}

impl<'a> Publisher<'a> {
    pub fn attach(mailbox: &'a Mailbox, logical: usize, apic_id: u32) -> Result<Self, Error> {
        if logical == 0 || logical >= MAX_CPUS {
            return Err(Error::Logical);
        }
        mailbox.identity(logical, apic_id)?;
        if !mailbox.phase()?.online() {
            return Err(Error::NotReady);
        }
        Ok(Self {
            mailbox,
            logical,
            apic_id,
        })
    }

    pub fn mailbox(&self) -> &Mailbox {
        self.mailbox
    }

    pub fn logical(&self) -> usize {
        self.logical
    }

    pub fn apic_id(&self) -> u32 {
        self.apic_id
    }

    pub fn publish(&self, command: Command) -> Result<u64, Error> {
        self.mailbox.identity(self.logical, self.apic_id)?;
        let phase = self.mailbox.phase()?;
        if !phase.online() {
            return Err(Error::NotReady);
        }
        if !command.valid() {
            return Err(if command.kind.executes_rounds() {
                Error::InvalidRounds
            } else {
                Error::InvalidKind
            });
        }
        match self.mailbox.state()? {
            State::Idle => {}
            State::Published | State::Executing => return Err(Error::Busy),
            State::Completed => return Err(Error::Busy),
            State::Failed => return Err(Error::Failed),
        }
        let sequence = self.mailbox.sequence();
        let acknowledged = self.mailbox.acknowledged();
        if sequence != acknowledged {
            return Err(Error::StaleAck);
        }
        let next = sequence.checked_add(1).ok_or(Error::SequenceOverflow)?;
        let order = self
            .mailbox
            .load(mailbox::PUBLISH_ORDER)
            .checked_add(1)
            .ok_or(Error::SequenceOverflow)?;
        // Every argument is fully published before the Release state change.
        self.mailbox.store(mailbox::KIND, command.kind.code());
        self.mailbox.store(mailbox::INPUT, command.input);
        self.mailbox.store(mailbox::ROUNDS, command.rounds);
        self.mailbox.store(mailbox::SEQUENCE, next);
        self.mailbox.store(mailbox::PUBLISH_ORDER, order);
        self.mailbox.words[mailbox::STATE].store(State::Published.code(), Ordering::Release);
        Ok(next)
    }

    pub fn poll(&self, sequence: u64) -> Result<Poll, Error> {
        self.mailbox.identity(self.logical, self.apic_id)?;
        match self.mailbox.state()? {
            State::Published | State::Executing => Ok(Poll::Pending),
            State::Idle => Err(Error::State),
            State::Failed => Err(Error::Failed),
            State::Completed => {
                let completion = completion_of(self.mailbox)?;
                if completion.sequence != sequence {
                    return Err(Error::Sequence);
                }
                if completion.logical != self.logical || completion.apic_id != self.apic_id {
                    return Err(Error::Identity);
                }
                if completion.executions != sequence {
                    return Err(Error::Sequence);
                }
                Ok(Poll::Completed(completion))
            }
        }
    }

    pub fn acknowledge(&self, sequence: u64) -> Result<(), Error> {
        self.mailbox.identity(self.logical, self.apic_id)?;
        if self.mailbox.state()? != State::Completed {
            return Err(Error::State);
        }
        if self.mailbox.sequence() != sequence {
            return Err(Error::StaleAck);
        }
        if self.mailbox.acknowledged() >= sequence {
            return Err(Error::StaleAck);
        }
        self.mailbox.words[mailbox::ACKED].store(sequence, Ordering::Release);
        self.mailbox.words[mailbox::STATE].store(State::Idle.code(), Ordering::Release);
        Ok(())
    }
}

fn completion_of(mailbox: &Mailbox) -> Result<Completion, Error> {
    let sequence = mailbox.load(mailbox::SEQUENCE);
    if mailbox.load(mailbox::COMPLETE_SEQUENCE) != sequence
        || mailbox.load(mailbox::ACCEPT_SEQUENCE) != sequence
    {
        return Err(Error::Sequence);
    }
    let logical =
        usize::try_from(mailbox.load(mailbox::ACCEPT_LOGICAL)).map_err(|_| Error::Logical)?;
    if logical == 0 || logical >= MAX_CPUS {
        return Err(Error::Logical);
    }
    let apic_u64 = mailbox.load(mailbox::ACCEPT_APIC);
    let apic_id = u32::try_from(apic_u64).map_err(|_| Error::Identity)?;
    if mailbox.load(mailbox::COMPLETE_LOGICAL) != logical as u64
        || mailbox.load(mailbox::COMPLETE_APIC) != apic_u64
    {
        return Err(Error::Identity);
    }
    let start = mailbox.load(mailbox::START_MARKER);
    if start != start_marker(sequence, logical, apic_id) {
        return Err(Error::Sequence);
    }
    let result = mailbox.load(mailbox::RESULT);
    let aux = mailbox.load(mailbox::RESULT_AUX);
    let marker = mailbox.load(mailbox::COMPLETE_MARKER);
    if marker != complete_marker(start, result, aux) {
        return Err(Error::Sequence);
    }
    Ok(Completion {
        sequence,
        logical,
        apic_id,
        start_marker: start,
        marker,
        result,
        aux,
        executions: mailbox.load(mailbox::EXECUTIONS),
    })
}

/// AP-side endpoint. The AP owns acceptance, completion and terminal failure;
/// the BSP never writes these fields.
pub struct Executor<'a> {
    mailbox: &'a Mailbox,
    logical: usize,
    apic_id: u32,
}

impl<'a> Executor<'a> {
    pub fn attach(mailbox: &'a Mailbox, logical: usize, apic_id: u32) -> Result<Self, Error> {
        if logical == 0 || logical >= MAX_CPUS {
            return Err(Error::Logical);
        }
        mailbox.identity(logical, apic_id)?;
        Ok(Self {
            mailbox,
            logical,
            apic_id,
        })
    }

    pub fn mailbox(&self) -> &Mailbox {
        self.mailbox
    }

    pub fn accept(&self, rsp: u64) -> Result<Option<Command>, Error> {
        match self.mailbox.state()? {
            State::Idle => Ok(None),
            State::Published => {
                let sequence = self.mailbox.sequence();
                if sequence == 0 || sequence <= self.mailbox.acknowledged() {
                    return Err(Error::Sequence);
                }
                let order = self.mailbox.load(mailbox::ACCEPT_ORDER);
                if order.checked_add(1) != Some(sequence) {
                    return Err(Error::Sequence);
                }
                if self.mailbox.load(mailbox::COMPLETE_SEQUENCE) >= sequence {
                    return Err(Error::Sequence);
                }
                let kind_code = self.mailbox.load(mailbox::KIND);
                let kind = CommandKind::from_code(kind_code).ok_or(Error::InvalidKind)?;
                let command = Command::new(
                    kind,
                    self.mailbox.load(mailbox::INPUT),
                    self.mailbox.load(mailbox::ROUNDS),
                );
                if !command.valid() {
                    return Err(if kind.executes_rounds() {
                        Error::InvalidRounds
                    } else {
                        Error::InvalidKind
                    });
                }
                self.mailbox.store(mailbox::ACCEPT_SEQUENCE, sequence);
                self.mailbox.store(mailbox::ACCEPT_ORDER, sequence);
                self.mailbox
                    .store(mailbox::ACCEPT_LOGICAL, self.logical as u64);
                self.mailbox
                    .store(mailbox::ACCEPT_APIC, u64::from(self.apic_id));
                self.mailbox.store(
                    mailbox::START_MARKER,
                    start_marker(sequence, self.logical, self.apic_id),
                );
                self.mailbox.store(mailbox::START_RSP, rsp);
                self.mailbox.words[mailbox::STATE]
                    .store(State::Executing.code(), Ordering::Release);
                Ok(Some(command))
            }
            State::Executing => Err(Error::Busy),
            State::Completed => Err(Error::State),
            State::Failed => Err(Error::Failed),
        }
    }

    pub fn complete(&self, sequence: u64, result: u64, aux: u64) -> Result<Completion, Error> {
        if self.mailbox.state()? != State::Executing {
            return Err(Error::State);
        }
        if self.mailbox.sequence() != sequence
            || self.mailbox.load(mailbox::ACCEPT_SEQUENCE) != sequence
        {
            return Err(Error::Sequence);
        }
        if self.mailbox.load(mailbox::ACCEPT_LOGICAL) != self.logical as u64
            || self.mailbox.load(mailbox::ACCEPT_APIC) != u64::from(self.apic_id)
        {
            return Err(Error::Identity);
        }
        let start = self.mailbox.load(mailbox::START_MARKER);
        if start != start_marker(sequence, self.logical, self.apic_id) {
            return Err(Error::Sequence);
        }
        let order = self.mailbox.load(mailbox::COMPLETE_ORDER);
        if order.checked_add(1) != Some(sequence) {
            return Err(Error::Sequence);
        }
        let executions = self.mailbox.executions();
        if executions != sequence - 1 {
            return Err(Error::Sequence);
        }
        let marker = complete_marker(start, result, aux);
        self.mailbox.store(mailbox::COMPLETE_SEQUENCE, sequence);
        self.mailbox
            .store(mailbox::COMPLETE_LOGICAL, self.logical as u64);
        self.mailbox
            .store(mailbox::COMPLETE_APIC, u64::from(self.apic_id));
        self.mailbox.store(mailbox::COMPLETE_MARKER, marker);
        self.mailbox.store(mailbox::RESULT, result);
        self.mailbox.store(mailbox::RESULT_AUX, aux);
        self.mailbox.store(mailbox::EXECUTIONS, sequence);
        self.mailbox.store(mailbox::COMPLETE_ORDER, sequence);
        let bit = 1u64 << ((sequence - 1) % 64);
        self.mailbox
            .store(mailbox::EXEC_MASK, self.mailbox.execution_mask() | bit);
        self.mailbox.words[mailbox::STATE].store(State::Completed.code(), Ordering::Release);
        Ok(Completion {
            sequence,
            logical: self.logical,
            apic_id: self.apic_id,
            start_marker: start,
            marker,
            result,
            aux,
            executions: sequence,
        })
    }

    /// Bounded, AP-side wait for the BSP acknowledgement of one completion.
    /// `Published` with the acknowledgement already advanced proves the BSP
    /// observed the completion and published the next command, so this cannot
    /// deadlock against a publisher that is one command ahead.
    pub fn acknowledged(&self, sequence: u64, limit: u64) -> bool {
        for _ in 0..limit {
            let acked = self.mailbox.acknowledged() == sequence;
            if acked && matches!(self.mailbox.state(), Ok(State::Idle) | Ok(State::Published)) {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }

    pub fn reject(&self, error: Error) {
        self.mailbox.record_rejection(error);
    }

    pub fn fail(&self, error: Error) -> ! {
        self.mailbox.record_rejection(error);
        self.mailbox.fail(error);
        loop {
            core::hint::spin_loop();
        }
    }
}

/// Two-phase gate shared by the overlap profile. `arrive`/`ready` publish each
/// participant's statement before the BSP computes the overlap witness; the
/// release value is the epoch, so a stale arrival or release is detectable.
#[repr(C, align(64))]
pub struct Gate {
    words: [AtomicU64; GATE_WORDS],
}

const _: () = assert!(core::mem::size_of::<Gate>() == GATE_STRIDE);

impl Default for Gate {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GatePhase {
    Closed = 0,
    Open = 1,
    Checkpoint = 2,
    Released = 3,
    Complete = 4,
}

impl GatePhase {
    pub const fn code(self) -> u64 {
        self as u64
    }

    pub const fn from_code(code: u64) -> Option<Self> {
        Some(match code {
            0 => Self::Closed,
            1 => Self::Open,
            2 => Self::Checkpoint,
            3 => Self::Released,
            4 => Self::Complete,
            _ => return None,
        })
    }
}

impl Gate {
    pub const fn new() -> Self {
        Self {
            words: [const { AtomicU64::new(0) }; GATE_WORDS],
        }
    }

    fn store(&self, field: usize, value: u64) {
        self.words[field].store(value, Ordering::Relaxed);
    }

    fn load(&self, field: usize) -> u64 {
        self.words[field].load(Ordering::Acquire)
    }

    fn cpu(&self, logical: usize, field: usize) -> Result<usize, Error> {
        if logical >= MAX_CPUS {
            return Err(Error::Logical);
        }
        Ok(GATE_HEADER_FIELDS + logical * GATE_CPU_STRIDE + field)
    }

    pub fn phase(&self) -> Result<GatePhase, Error> {
        GatePhase::from_code(self.load(gate::PHASE)).ok_or(Error::Phase)
    }

    pub fn epoch(&self) -> u64 {
        self.load(gate::EPOCH)
    }

    pub fn release_value(&self) -> u64 {
        self.load(gate::RELEASE)
    }

    pub fn overlap(&self) -> u64 {
        self.load(gate::OVERLAP)
    }

    pub fn failure(&self) -> u64 {
        self.load(gate::FAILURE)
    }

    pub fn entered(&self, logical: usize) -> u64 {
        self.cpu(logical, gate::ENTERED)
            .map(|field| self.load(field))
            .unwrap_or(0)
    }

    pub fn ready(&self, logical: usize) -> u64 {
        self.cpu(logical, gate::READY)
            .map(|field| self.load(field))
            .unwrap_or(0)
    }

    pub fn done(&self, logical: usize) -> u64 {
        self.cpu(logical, gate::DONE)
            .map(|field| self.load(field))
            .unwrap_or(0)
    }

    /// BSP-only: opens a new epoch after proving the previous one is closed.
    pub fn open(&self, participants: usize) -> Result<u64, Error> {
        if !(2..=MAX_CPUS).contains(&participants) {
            return Err(Error::Phase);
        }
        if !matches!(self.phase()?, GatePhase::Closed | GatePhase::Complete) {
            return Err(Error::Phase);
        }
        let epoch = self
            .load(gate::EPOCH)
            .checked_add(1)
            .ok_or(Error::SequenceOverflow)?;
        for logical in 0..MAX_CPUS {
            for field in [gate::ENTERED, gate::READY, gate::DONE, gate::RESERVED] {
                let index = self.cpu(logical, field)?;
                self.store(index, 0);
            }
        }
        self.store(gate::MAGIC, GATE_MAGIC);
        self.store(gate::PARTICIPANTS, participants as u64);
        self.store(gate::RELEASE, 0);
        self.store(gate::OVERLAP, 0);
        self.store(gate::STOP, 0);
        self.store(gate::FAILURE, 0);
        self.store(gate::EPOCH, epoch);
        self.words[gate::PHASE].store(GatePhase::Open.code(), Ordering::Release);
        Ok(epoch)
    }

    /// Participant-side arrival. `epoch` must be the currently open epoch.
    pub fn arrive(&self, logical: usize, epoch: u64) -> Result<(), Error> {
        if self.phase()? != GatePhase::Open || self.epoch() != epoch || epoch == 0 {
            return Err(Error::Phase);
        }
        let participants = self.load(gate::PARTICIPANTS) as usize;
        if logical >= participants {
            return Err(Error::Logical);
        }
        let entered = self.cpu(logical, gate::ENTERED)?;
        if self.load(entered) != 0 {
            return Err(Error::Busy);
        }
        self.store(entered, epoch);
        let ready = self.cpu(logical, gate::READY)?;
        self.words[ready].store(epoch, Ordering::Release);
        Ok(())
    }

    pub fn all_ready(&self) -> bool {
        let participants = self.load(gate::PARTICIPANTS) as usize;
        if !(2..=MAX_CPUS).contains(&participants) {
            return false;
        }
        let epoch = self.load(gate::EPOCH);
        (0..participants)
            .all(|logical| self.entered(logical) == epoch && self.ready(logical) == epoch)
            && (participants..MAX_CPUS)
                .all(|logical| self.entered(logical) == 0 && self.ready(logical) == 0)
    }

    /// BSP-only: records the participants that were simultaneously inside the
    /// barrier and publishes the release. The overlap witness is a claim QMP
    /// rechecks against ENTERED/READY; it is not timing evidence.
    pub fn release(&self) -> Result<u64, Error> {
        if self.phase()? != GatePhase::Open || !self.all_ready() {
            return Err(Error::Phase);
        }
        let participants = self.load(gate::PARTICIPANTS) as usize;
        let epoch = self.load(gate::EPOCH);
        let mut overlap = 0u64;
        for logical in 0..participants {
            if self.entered(logical) == epoch && self.ready(logical) == epoch {
                overlap |= 1 << logical;
            }
        }
        self.store(gate::OVERLAP, overlap);
        self.words[gate::PHASE].store(GatePhase::Checkpoint.code(), Ordering::Release);
        Ok(overlap)
    }

    pub fn mark_checkpoint(&self) {
        self.words[gate::PHASE].store(GatePhase::Checkpoint.code(), Ordering::Release);
    }

    /// Releases every waiting participant after the BSP checkpoint.
    pub fn publish_release(&self) -> Result<(), Error> {
        if self.phase()? != GatePhase::Checkpoint {
            return Err(Error::Phase);
        }
        let epoch = self.load(gate::EPOCH);
        self.words[gate::RELEASE].store(epoch, Ordering::Release);
        self.words[gate::PHASE].store(GatePhase::Released.code(), Ordering::Release);
        Ok(())
    }

    pub fn wait_released(&self, epoch: u64) -> Result<(), Error> {
        if self.release_value() != epoch {
            return Err(Error::Phase);
        }
        if !matches!(self.phase()?, GatePhase::Released | GatePhase::Complete) {
            return Err(Error::Phase);
        }
        Ok(())
    }

    pub fn depart(&self, logical: usize, epoch: u64) -> Result<(), Error> {
        if self.release_value() != epoch {
            return Err(Error::Phase);
        }
        let done = self.cpu(logical, gate::DONE)?;
        if self.load(done) != 0 {
            return Err(Error::Busy);
        }
        self.words[done].store(epoch, Ordering::Release);
        Ok(())
    }

    pub fn all_done(&self) -> bool {
        let participants = self.load(gate::PARTICIPANTS) as usize;
        let epoch = self.load(gate::EPOCH);
        (0..participants).all(|logical| self.done(logical) == epoch)
            && (participants..MAX_CPUS).all(|logical| self.done(logical) == 0)
    }

    pub fn complete(&self) -> Result<(), Error> {
        if self.phase()? != GatePhase::Released || !self.all_done() {
            return Err(Error::Phase);
        }
        self.words[gate::PHASE].store(GatePhase::Complete.code(), Ordering::Release);
        Ok(())
    }

    pub fn stop(&self, mask: u64) {
        self.store(gate::STOP, mask);
    }

    pub fn fail(&self, code: u64) {
        self.store(gate::FAILURE, code);
    }
}

/// Classification of an interrupt at a CPU that only admits a work IPI inside
/// its documented idle window. A software-invoked vector with a clear ISR bit
/// is never acknowledged; an unexpected vector is terminal for that CPU.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interrupt {
    WorkIpi,
    Spurious,
    Unexpected,
}

pub fn classify(vector: u64, in_service: bool) -> Interrupt {
    match (vector, in_service) {
        (WORK_IPI_VECTOR, true) => Interrupt::WorkIpi,
        (SPURIOUS_VECTOR, false) => Interrupt::Spurious,
        _ => Interrupt::Unexpected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    const RSP: u64 = 0xffff_8000_0001_0000;

    fn command() -> Command {
        Command::new(CommandKind::Transform, 0x1234_5678, 64)
    }

    fn setup() -> (&'static Mailbox, Publisher<'static>, Executor<'static>) {
        // Leak one fixed mailbox per test; the host process is short-lived and
        // this keeps the borrow shapes identical to the production statics.
        let mailbox: &'static Mailbox = std::boxed::Box::leak(std::boxed::Box::new(Mailbox::new()));
        mailbox.initialize(1, 4);
        let publisher = Publisher::attach(mailbox, 1, 4).unwrap();
        let executor = Executor::attach(mailbox, 1, 4).unwrap();
        (mailbox, publisher, executor)
    }

    fn run_one(publisher: &Publisher<'_>, executor: &Executor<'_>, command: Command) -> (u64, u64) {
        let sequence = publisher.publish(command).unwrap();
        assert_eq!(executor.accept(RSP).unwrap(), Some(command));
        let (result, aux) = execute(command.kind, command.input, command.rounds).unwrap();
        let completion = executor.complete(sequence, result, aux).unwrap();
        assert_eq!(completion.sequence, sequence);
        let Poll::Completed(observed) = publisher.poll(sequence).unwrap() else {
            panic!("completion was not published")
        };
        assert_eq!(observed, completion);
        publisher.acknowledge(sequence).unwrap();
        (result, aux)
    }

    #[test]
    fn mailbox_layout_is_the_published_one() {
        assert_eq!(MAILBOX_FIELDS * 8, MAILBOX_STRIDE);
        assert_eq!(MAILBOX_STRIDE, 256);
        assert_eq!(core::mem::size_of::<Mailbox>(), MAILBOX_STRIDE);
        assert_eq!(core::mem::align_of::<Mailbox>(), 64);
        assert_eq!(GATE_WORDS, GATE_HEADER_FIELDS + GATE_CPU_FIELDS * MAX_CPUS);
        assert_eq!(core::mem::size_of::<Gate>(), GATE_STRIDE);
    }

    #[test]
    fn transform_vectors_are_stable() {
        assert_eq!(
            execute(CommandKind::Transform, 0x1234_5678, 64).unwrap(),
            (0x9ec9_ca55_e40d_e822, 0x4411_7265_001f_fa52)
        );
        assert_eq!(
            execute(CommandKind::Transform, 0xdead_beef, 1).unwrap(),
            (0xb5ad_70dc_8b3a_cb12, 0xb5ad_70dc_8b3a_cb12)
        );
    }

    #[test]
    fn checksum_vectors_are_stable() {
        assert_eq!(
            execute(CommandKind::Checksum, 9, 4).unwrap(),
            (0x0380_6eba_2a3c_468d, 4)
        );
        assert!(execute(CommandKind::Checksum, 0, 0).is_err());
        assert!(execute(CommandKind::Stop, 0, 0).is_err());
    }

    #[test]
    fn markers_are_bound_to_identity_and_result() {
        let start = start_marker(3, 2, 7);
        assert_eq!(start, start_marker(3, 2, 7));
        assert_ne!(start, start_marker(3, 1, 7));
        let marker = complete_marker(start, 0x1111, 0x2222);
        assert_eq!(marker, complete_marker(start, 0x1111, 0x2222));
        assert_ne!(marker, complete_marker(start, 0x1111, 0x2223));
    }

    #[test]
    fn happy_path_publishes_accepts_completes_and_acks() {
        let (mailbox, publisher, executor) = setup();
        let (result, aux) = run_one(&publisher, &executor, command());
        assert_eq!(mailbox.phase().unwrap(), Phase::Ready);
        assert_eq!(mailbox.state().unwrap(), State::Idle);
        assert_eq!(mailbox.sequence(), 1);
        assert_eq!(mailbox.acknowledged(), 1);
        assert_eq!(mailbox.executions(), 1);
        assert_eq!(mailbox.execution_mask(), 1);
        assert_eq!(mailbox.result(), (result, aux));
        assert_eq!(mailbox.load(mailbox::PUBLISH_ORDER), 1);
        assert_eq!(mailbox.load(mailbox::ACCEPT_ORDER), 1);
        assert_eq!(mailbox.load(mailbox::COMPLETE_ORDER), 1);
        assert_eq!(mailbox.load(mailbox::START_RSP), RSP);
        assert_eq!(mailbox.load(mailbox::ACCEPT_LOGICAL), 1);
        assert_eq!(mailbox.load(mailbox::ACCEPT_APIC), 4);
        assert_eq!(mailbox.load(mailbox::COMPLETE_LOGICAL), 1);
        assert_eq!(mailbox.load(mailbox::COMPLETE_APIC), 4);
        assert_eq!(mailbox.counter(mailbox::FAILURE), 0);
    }

    #[test]
    fn publication_requires_a_published_matching_identity() {
        let raw = Mailbox::new();
        assert_eq!(Publisher::attach(&raw, 1, 4).err(), Some(Error::Magic));
        raw.initialize(1, 4);
        assert_eq!(Publisher::attach(&raw, 1, 5).err(), Some(Error::Identity));
        assert_eq!(Publisher::attach(&raw, 2, 4).err(), Some(Error::Logical));
        assert_eq!(Publisher::attach(&raw, 0, 4).err(), Some(Error::Logical));
        raw.set_phase(Phase::Stopped);
        assert_eq!(Publisher::attach(&raw, 1, 4).err(), Some(Error::NotReady));
        raw.set_phase(Phase::Failed);
        assert_eq!(Publisher::attach(&raw, 1, 4).err(), Some(Error::NotReady));
    }

    #[test]
    fn invalid_kind_and_rounds_are_rejected_before_publication() {
        let (mailbox, publisher, _) = setup();
        assert_eq!(
            publisher
                .publish(Command::new(CommandKind::Transform, 1, 0))
                .unwrap_err(),
            Error::InvalidRounds
        );
        assert_eq!(
            publisher
                .publish(Command::new(CommandKind::Transform, 1, MAX_ROUNDS + 1))
                .unwrap_err(),
            Error::InvalidRounds
        );
        assert_eq!(mailbox.state().unwrap(), State::Idle);
        assert_eq!(mailbox.sequence(), 0);
        // The raw kind is never trusted from the mailbox either.
        assert_eq!(CommandKind::from_code(0), None);
        assert_eq!(CommandKind::from_code(5), None);
    }

    #[test]
    fn busy_mailbox_never_overwrites_an_unacknowledged_command() {
        let (mailbox, publisher, executor) = setup();
        let sequence = publisher.publish(command()).unwrap();
        assert_eq!(publisher.publish(command()).unwrap_err(), Error::Busy);
        assert_eq!(mailbox.sequence(), sequence);
        assert_eq!(mailbox.state().unwrap(), State::Published);
        assert_eq!(executor.accept(RSP).unwrap(), Some(command()));
        assert_eq!(publisher.publish(command()).unwrap_err(), Error::Busy);
        assert_eq!(mailbox.load(mailbox::KIND), CommandKind::Transform.code());
    }

    #[test]
    fn executor_never_accepts_before_the_bsp_acknowledges() {
        let (mailbox, publisher, executor) = setup();
        let sequence = publisher.publish(command()).unwrap();
        assert_eq!(executor.accept(RSP).unwrap(), Some(command()));
        let (result, aux) = execute(CommandKind::Transform, 0x1234_5678, 64).unwrap();
        executor.complete(sequence, result, aux).unwrap();
        assert_eq!(executor.accept(RSP).unwrap_err(), Error::State);
        assert_eq!(mailbox.state().unwrap(), State::Completed);
        publisher.acknowledge(sequence).unwrap();
        assert_eq!(executor.accept(RSP).unwrap(), None);
    }

    #[test]
    fn stale_and_replayed_acknowledgements_are_rejected() {
        let (mailbox, publisher, executor) = setup();
        let sequence = publisher.publish(command()).unwrap();
        assert_eq!(publisher.poll(sequence).unwrap(), Poll::Pending);
        assert_eq!(publisher.acknowledge(sequence).unwrap_err(), Error::State);
        assert_eq!(executor.accept(RSP).unwrap(), Some(command()));
        let (result, aux) = execute(CommandKind::Transform, 0x1234_5678, 64).unwrap();
        executor.complete(sequence, result, aux).unwrap();
        assert_eq!(
            publisher.acknowledge(sequence + 1).unwrap_err(),
            Error::StaleAck
        );
        assert_eq!(publisher.poll(sequence + 1).unwrap_err(), Error::Sequence);
        publisher.acknowledge(sequence).unwrap();
        assert_eq!(publisher.acknowledge(sequence).unwrap_err(), Error::State);
        assert_eq!(mailbox.acknowledged(), sequence);
    }

    #[test]
    fn duplicate_accept_and_replayed_completion_are_rejected() {
        let (mailbox, publisher, executor) = setup();
        let sequence = publisher.publish(command()).unwrap();
        assert_eq!(executor.accept(RSP).unwrap(), Some(command()));
        assert_eq!(executor.accept(RSP).unwrap_err(), Error::Busy);
        let (result, aux) = execute(CommandKind::Transform, 0x1234_5678, 64).unwrap();
        executor.complete(sequence, result, aux).unwrap();
        assert_eq!(
            executor.complete(sequence, result, aux).unwrap_err(),
            Error::State
        );
        assert_eq!(mailbox.executions(), 1);
        assert_eq!(mailbox.execution_mask(), 1);
    }

    #[test]
    fn completion_requires_the_accepted_sequence() {
        let (_, publisher, executor) = setup();
        let sequence = publisher.publish(command()).unwrap();
        assert_eq!(executor.complete(sequence, 0, 0).unwrap_err(), Error::State);
        executor.accept(RSP).unwrap();
        assert_eq!(
            executor.complete(sequence + 1, 0, 0).unwrap_err(),
            Error::Sequence
        );
        assert_eq!(
            executor.complete(sequence, 0, 0).unwrap().sequence,
            sequence
        );
    }

    #[test]
    fn sequence_exhaustion_is_rejected_without_touching_state() {
        let (mailbox, publisher, _) = setup();
        mailbox.words[mailbox::SEQUENCE].store(u64::MAX, Ordering::Relaxed);
        mailbox.words[mailbox::ACKED].store(u64::MAX, Ordering::Relaxed);
        assert_eq!(
            publisher.publish(command()).unwrap_err(),
            Error::SequenceOverflow
        );
        assert_eq!(mailbox.state().unwrap(), State::Idle);
        assert_eq!(mailbox.sequence(), u64::MAX);
    }

    #[test]
    fn publish_order_exhaustion_is_rejected() {
        let (mailbox, publisher, _) = setup();
        mailbox.words[mailbox::PUBLISH_ORDER].store(u64::MAX, Ordering::Relaxed);
        assert_eq!(
            publisher.publish(command()).unwrap_err(),
            Error::SequenceOverflow
        );
        assert_eq!(mailbox.state().unwrap(), State::Idle);
    }

    #[test]
    fn executor_rejections_are_recorded_and_failure_is_terminal() {
        let (mailbox, publisher, executor) = setup();
        executor.reject(Error::InvalidKind);
        assert_eq!(mailbox.counter(mailbox::REJECTIONS), 1);
        assert_eq!(
            mailbox.counter(mailbox::LAST_REJECT),
            Error::InvalidKind.code()
        );
        // `Executor::fail` never returns; its publication is exercised through
        // the same mailbox method the AP calls before parking.
        mailbox.fail(Error::InvalidRounds);
        assert_eq!(mailbox.state().unwrap(), State::Failed);
        assert_eq!(mailbox.phase().unwrap(), Phase::Failed);
        assert_eq!(
            mailbox.counter(mailbox::FAILURE),
            Error::InvalidRounds.code()
        );
        assert_eq!(publisher.publish(command()).unwrap_err(), Error::NotReady);
    }

    #[test]
    fn completion_marker_is_recomputed_from_published_arguments() {
        let (mailbox, publisher, executor) = setup();
        let sequence = publisher
            .publish(Command::new(CommandKind::Checksum, 7, 16))
            .unwrap();
        executor.accept(RSP).unwrap();
        let (result, aux) = execute(CommandKind::Checksum, 7, 16).unwrap();
        executor.complete(sequence, result, aux).unwrap();
        assert_eq!(
            mailbox.load(mailbox::COMPLETE_MARKER),
            complete_marker(start_marker(sequence, 1, 4), result, aux)
        );
        assert_eq!(mailbox.load(mailbox::RESULT), result);
        assert_eq!(mailbox.load(mailbox::RESULT_AUX), aux);
    }

    #[test]
    fn commands_advance_sequence_order_and_execution_mask() {
        let (mailbox, publisher, executor) = setup();
        for index in 0..8u64 {
            let command = Command::new(
                if index % 2 == 0 {
                    CommandKind::Transform
                } else {
                    CommandKind::Checksum
                },
                index,
                index + 1,
            );
            let (result, aux) = run_one(&publisher, &executor, command);
            assert_eq!(mailbox.result(), (result, aux));
        }
        assert_eq!(mailbox.sequence(), 8);
        assert_eq!(mailbox.acknowledged(), 8);
        assert_eq!(mailbox.load(mailbox::PUBLISH_ORDER), 8);
        assert_eq!(mailbox.executions(), 8);
        assert_eq!(mailbox.execution_mask(), 0xff);
    }

    #[test]
    fn gate_is_two_phase_and_epoch_checked() {
        let gate = Gate::new();
        let epoch = gate.open(2).unwrap();
        assert_eq!(epoch, 1);
        assert_eq!(gate.phase().unwrap(), GatePhase::Open);
        assert!(gate.release().is_err());
        gate.arrive(0, epoch).unwrap();
        assert_eq!(gate.arrive(0, epoch).unwrap_err(), Error::Busy);
        assert!(!gate.all_ready());
        gate.arrive(1, epoch).unwrap();
        assert!(gate.all_ready());
        let overlap = gate.release().unwrap();
        assert_eq!(overlap, 0b11);
        assert_eq!(gate.phase().unwrap(), GatePhase::Checkpoint);
        assert_eq!(gate.ready(0), epoch);
        assert_eq!(gate.ready(1), epoch);
        gate.publish_release().unwrap();
        assert_eq!(gate.release_value(), epoch);
        assert!(gate.depart(0, epoch + 1).is_err());
        gate.depart(0, epoch).unwrap();
        gate.depart(1, epoch).unwrap();
        assert!(gate.all_done());
        gate.complete().unwrap();
        assert_eq!(gate.phase().unwrap(), GatePhase::Complete);
        let next = gate.open(4).unwrap();
        assert_eq!(next, epoch + 1);
        assert_eq!(gate.entered(0), 0);
    }

    #[test]
    fn gate_rejects_stale_arrivals_and_out_of_range_participants() {
        let gate = Gate::new();
        let epoch = gate.open(3).unwrap();
        assert_eq!(gate.arrive(0, epoch + 1).unwrap_err(), Error::Phase);
        assert_eq!(gate.arrive(3, epoch).unwrap_err(), Error::Logical);
        gate.arrive(0, epoch).unwrap();
        gate.arrive(1, epoch).unwrap();
        gate.arrive(2, epoch).unwrap();
        gate.release().unwrap();
        gate.publish_release().unwrap();
        assert_eq!(gate.arrive(0, epoch).unwrap_err(), Error::Phase);
        assert_eq!(gate.open(2).unwrap_err(), Error::Phase);
    }

    #[test]
    fn interrupt_classification_distinguishes_work_spurious_and_unexpected() {
        assert_eq!(classify(WORK_IPI_VECTOR, true), Interrupt::WorkIpi);
        assert_eq!(classify(WORK_IPI_VECTOR, false), Interrupt::Unexpected);
        assert_eq!(classify(SPURIOUS_VECTOR, false), Interrupt::Spurious);
        assert_eq!(classify(SPURIOUS_VECTOR, true), Interrupt::Unexpected);
        assert_eq!(classify(0x41, true), Interrupt::Unexpected);
    }

    #[test]
    fn threaded_publisher_and_executor_keep_exactly_once_state() {
        let mailbox = Arc::new(Mailbox::new());
        mailbox.initialize(2, 9);
        let bsp = Arc::clone(&mailbox);
        let ap = Arc::clone(&mailbox);
        let producer = thread::spawn(move || {
            let publisher = Publisher::attach(&bsp, 2, 9).unwrap();
            for index in 0..64u64 {
                let command = Command::new(CommandKind::Checksum, index, 32);
                let sequence = publisher.publish(command).unwrap();
                loop {
                    match publisher.poll(sequence).unwrap() {
                        Poll::Pending => std::hint::spin_loop(),
                        Poll::Completed(completion) => {
                            let (result, aux) =
                                execute(command.kind, command.input, command.rounds).unwrap();
                            assert_eq!((completion.result, completion.aux), (result, aux));
                            publisher.acknowledge(sequence).unwrap();
                            break;
                        }
                    }
                }
            }
        });
        let consumer = thread::spawn(move || {
            let executor = Executor::attach(&ap, 2, 9).unwrap();
            let mut done = 0u64;
            while done < 64 {
                match executor.accept(RSP) {
                    Ok(Some(command)) => {
                        let sequence = executor.mailbox().sequence();
                        let (result, aux) =
                            execute(command.kind, command.input, command.rounds).unwrap();
                        executor.complete(sequence, result, aux).unwrap();
                        assert!(executor.acknowledged(sequence, 100_000_000));
                        done += 1;
                    }
                    Ok(None) => std::hint::spin_loop(),
                    Err(error) => panic!("executor rejected a valid command: {error:?}"),
                }
            }
        });
        producer.join().unwrap();
        consumer.join().unwrap();
        assert_eq!(mailbox.executions(), 64);
        assert_eq!(mailbox.sequence(), 64);
        assert_eq!(mailbox.execution_mask(), u64::MAX);
        assert_eq!(mailbox.state().unwrap(), State::Idle);
    }
}

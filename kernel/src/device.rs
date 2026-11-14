//! Bounded device lease and interrupt protocol. All callers are trusted kernel
//! transactions; owner/thread scalars come from a validated ActiveContext, never
//! from syscall arguments. Hardware and capability lookups live in the backend.
pub const PCI_ID: u32 = 0x11e8_1234;
pub const IDENT: u32 = 0x0100_00ed;
pub const BAR_SIZE: u64 = 1 << 20;
pub const VECTOR: u8 = 240;
pub const WATCHDOG: u64 = 80;
pub const IO_BUDGET: u64 = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i64)]
pub enum Error {
    Busy = -40,
    Unavailable = -41,
    Lease = -42,
    Sequence = -43,
    NotReady = -44,
    Register = -45,
    Quarantined = -46,
    Timeout = -47,
    Capacity = -48,
}
pub type Result<T> = core::result::Result<T, Error>;
pub mod phase {
    pub const ABSENT: u64 = 0;
    pub const FREE: u64 = 1;
    pub const REARM: u64 = 2;
    pub const ARMED: u64 = 3;
    pub const FLIGHT: u64 = 4;
    pub const PENDING: u64 = 5;
    pub const QUARANTINE: u64 = 6;
}
pub mod reason {
    pub const CLOSE: u64 = 1;
    pub const REVOKE: u64 = 2;
    pub const TERMINATE: u64 = 3;
    pub const TIMEOUT: u64 = 4;
    pub const BUDGET: u64 = 5;
    pub const HARDWARE: u64 = 6;
}

/// PCI information validated before any address reaches the mapper. Bus-zero,
/// single 32-bit memory BAR, exact EDU aperture and the Q35 slot-five INTx route.
pub fn valid_resource(bdf: u64, bar: u64, size: u64, gsi: u64) -> bool {
    bdf == 0x28
        && (BAR_SIZE..(1 << 32)).contains(&bar)
        && bar.is_multiple_of(BAR_SIZE)
        && size == BAR_SIZE
        && gsi == 21
}

/// Returns the expected completion bit for a triggering write, or zero for a
/// synchronous operation. ACK and DMA registers have no general MMIO authority.
pub fn register(offset: u64, width: u64, write: Option<u64>) -> Result<u64> {
    if width != 4 || !offset.is_multiple_of(4) {
        return Err(Error::Register);
    }
    match write {
        None if matches!(offset, 0 | 4 | 8 | 0x20 | 0x24) => Ok(0),
        Some(value) if value <= u32::MAX as u64 => match (offset, value) {
            (4, _) | (0x20, 0x80) => Ok(0),
            (8, 0..=12) => Ok(1),
            (0x60, 2) => Ok(2),
            _ => Err(Error::Register),
        },
        _ => Err(Error::Register),
    }
}

/// The words below are production state, not an acceptance ledger. QMP reads
/// their actual addresses and reconciles them with hardware and other owners.
#[repr(C)]
#[derive(Debug)]
pub struct Device {
    pub phase: u64,
    pub generation: u64,
    pub owner: u64,
    pub waiter: u64,
    pub receiver: u64,
    pub sequence: u64,
    pub status: u64,
    pub flight: u64,
    pub deadline: u64,
    pub claims: u64,
    pub interrupts: u64,
    pub acknowledgements: u64,
    pub quarantines: u64,
    pub releases: u64,
    pub reason: u64,
    pub reads: u64,
    pub writes: u64,
    pub rejected: u64,
    pub remaining: u64,
    pub blocked: u64,
    pub delivered: u64,
    pub cancelled: u64,
    pub late: u64,
    pub retired: u64,
}
impl Default for Device {
    fn default() -> Self {
        Self::new()
    }
}
impl Device {
    pub const fn new() -> Self {
        Self {
            phase: phase::ABSENT,
            generation: 0,
            owner: 0,
            waiter: 0,
            receiver: 0,
            sequence: 0,
            status: 0,
            flight: 0,
            deadline: 0,
            claims: 0,
            interrupts: 0,
            acknowledgements: 0,
            quarantines: 0,
            releases: 0,
            reason: 0,
            reads: 0,
            writes: 0,
            rejected: 0,
            remaining: 0,
            blocked: 0,
            delivered: 0,
            cancelled: 0,
            late: 0,
            retired: 0,
        }
    }
    pub fn discover(&mut self) -> Result<()> {
        if self.phase != phase::ABSENT {
            return Err(Error::Busy);
        }
        self.phase = phase::FREE;
        Ok(())
    }
    pub fn can_claim(&self, owner: u64) -> Result<u64> {
        if owner == 0 {
            return Err(Error::Lease);
        }
        match self.phase {
            phase::ABSENT => return Err(Error::Unavailable),
            phase::FREE => (),
            phase::QUARANTINE => return Err(Error::Quarantined),
            _ => return Err(Error::Busy),
        }
        self.generation.checked_add(1).ok_or(Error::Capacity)
    }
    pub fn claim(&mut self, owner: u64, now: u64) -> Result<u64> {
        let next = self.can_claim(owner)?;
        self.generation = next;
        self.owner = owner;
        self.phase = phase::REARM;
        self.deadline = now.saturating_add(WATCHDOG);
        self.claims += 1;
        self.remaining = IO_BUDGET;
        self.reason = 0;
        Ok(next)
    }
    pub fn authorize(&self, owner: u64, generation: u64) -> Result<()> {
        if owner == 0 || owner != self.owner || generation != self.generation {
            return Err(Error::Lease);
        }
        if self.phase == phase::QUARANTINE {
            return Err(Error::Quarantined);
        }
        if self.phase < phase::REARM {
            return Err(Error::Lease);
        }
        Ok(())
    }
    pub fn armed(&mut self, generation: u64) -> Result<()> {
        if self.phase != phase::REARM || generation != self.generation {
            return Err(Error::Lease);
        }
        self.phase = phase::ARMED;
        Ok(())
    }
    /// Preflight is separate so a denied capability/register cannot consume
    /// budget or reach hardware. The caller commits only after backend access.
    pub fn check_io(
        &self,
        owner: u64,
        generation: u64,
        offset: u64,
        width: u64,
        value: Option<u64>,
    ) -> Result<u64> {
        self.authorize(owner, generation)?;
        let trigger = register(offset, width, value)?;
        if value.is_some() && self.phase != phase::ARMED {
            return Err(Error::NotReady);
        }
        if self.remaining == 0 {
            return Err(Error::Capacity);
        }
        Ok(trigger)
    }
    pub fn commit_io(&mut self, write: bool, trigger: u64, now: u64) {
        self.remaining -= 1;
        if write {
            self.writes += 1;
        } else {
            self.reads += 1;
        }
        if trigger != 0 {
            self.flight = trigger;
            self.phase = phase::FLIGHT;
            self.deadline = now.saturating_add(WATCHDOG);
        }
    }
    pub fn wait(
        &mut self,
        owner: u64,
        generation: u64,
        thread: u64,
        now: u64,
    ) -> Result<Option<u64>> {
        self.authorize(owner, generation)?;
        if thread == 0 || self.waiter != 0 || self.receiver != 0 {
            return Err(Error::Busy);
        }
        if self.phase == phase::PENDING {
            self.receiver = thread;
            self.delivered += 1;
            return Ok(Some(self.sequence));
        }
        self.waiter = thread;
        self.deadline = now.saturating_add(WATCHDOG);
        self.blocked += 1;
        Ok(None)
    }
    /// Hardware event is accepted once and remains pending without a waiter.
    /// IRQ arrival in quarantine can retire the old operation, never wake a new
    /// owner. Status must match the exact submitted operation, not arbitrary bits.
    pub fn interrupt(
        &mut self,
        generation: u64,
        status: u64,
        now: u64,
    ) -> Result<Option<(u64, u64)>> {
        self.interrupts += 1;
        if generation != self.generation || self.owner == 0 {
            self.late += 1;
            return Err(Error::Lease);
        }
        if status == 0 || status != self.flight {
            return Err(Error::Sequence);
        }
        self.flight = 0;
        if self.phase == phase::QUARANTINE {
            self.late += 1;
            return Ok(None);
        }
        if self.phase != phase::FLIGHT {
            return Err(Error::Sequence);
        }
        self.sequence = self.sequence.checked_add(1).ok_or(Error::Capacity)?;
        self.status = status;
        self.phase = phase::PENDING;
        self.deadline = now.saturating_add(WATCHDOG);
        if self.waiter == 0 {
            return Ok(None);
        }
        let thread = core::mem::take(&mut self.waiter);
        self.receiver = thread;
        self.delivered += 1;
        Ok(Some((thread, self.sequence)))
    }
    pub fn check_ack(
        &self,
        owner: u64,
        generation: u64,
        thread: u64,
        sequence: u64,
    ) -> Result<u64> {
        self.authorize(owner, generation)?;
        if self.phase != phase::PENDING
            || self.receiver != thread
            || sequence == 0
            || sequence != self.sequence
        {
            return Err(Error::Sequence);
        }
        Ok(self.status)
    }
    pub fn acknowledge(&mut self, now: u64) {
        self.receiver = 0;
        self.status = 0;
        self.phase = phase::REARM;
        self.deadline = now.saturating_add(WATCHDOG);
        self.acknowledgements += 1;
    }
    /// Returns a blocked Thread to cancel exactly once, before its capability
    /// is closed/revoked or its Node is stopped.
    pub fn quarantine(&mut self, reason: u64) -> Option<u64> {
        if self.phase < phase::REARM || self.phase == phase::QUARANTINE {
            return None;
        }
        self.phase = phase::QUARANTINE;
        self.reason = reason;
        self.quarantines += 1;
        self.receiver = 0;
        let waiter = core::mem::take(&mut self.waiter);
        if waiter == 0 {
            None
        } else {
            self.cancelled += 1;
            Some(waiter)
        }
    }
    pub fn expired(&self, now: u64) -> bool {
        (phase::REARM..phase::QUARANTINE).contains(&self.phase) && now >= self.deadline
    }
    /// COMPUTING=0 alone is insufficient: EDU clears it before raising FACT_IRQ.
    /// Seeing the expected status proves the old command completed its raise.
    pub fn observe_completion(&mut self, computing: bool, status: u64) {
        if self.phase == phase::QUARANTINE
            && !computing
            && self.flight != 0
            && status == self.flight
        {
            self.flight = 0;
        }
    }
    pub fn release(
        &mut self,
        computing: bool,
        status: u64,
        route_enabled: bool,
        lapic_pending: bool,
        mailbox: bool,
    ) -> Result<()> {
        if self.phase != phase::QUARANTINE
            || self.flight != 0
            || computing
            || status != 0
            || route_enabled
            || lapic_pending
            || mailbox
        {
            return Err(Error::NotReady);
        }
        self.retired = self.generation;
        self.phase = phase::FREE;
        self.owner = 0;
        self.waiter = 0;
        self.receiver = 0;
        self.status = 0;
        self.deadline = 0;
        self.remaining = 0;
        self.releases += 1;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn claimed() -> Device {
        let mut d = Device::new();
        d.discover().unwrap();
        assert_eq!(d.claim(11, 0), Ok(1));
        d.armed(1).unwrap();
        d
    }
    fn trigger(d: &mut Device) {
        assert_eq!(d.check_io(11, 1, 0x60, 4, Some(2)), Ok(2));
        d.commit_io(true, 2, 1);
    }
    #[test]
    fn resource_bounds_and_q35_route_fail_closed() {
        assert!(valid_resource(0x28, 0xc0000000, BAR_SIZE, 21));
        for args in [
            (256, 0xc0000000, BAR_SIZE, 21),
            (0x28, 0, BAR_SIZE, 21),
            (0x28, 0xc0001000, BAR_SIZE, 21),
            (0x28, 1 << 32, BAR_SIZE, 21),
            (0x28, 0xc0000000, 4096, 21),
            (0x28, 0xc0000000, BAR_SIZE, 0xf4),
            (0x28, 0xc0000000, BAR_SIZE, 0x41),
        ] {
            assert!(!valid_resource(args.0, args.1, args.2, args.3));
        }
    }
    #[test]
    fn register_policy_never_exposes_dma_config_ack_or_truncation() {
        for offset in 0..0x100 {
            if ![0, 4, 8, 0x20, 0x24].contains(&offset) {
                assert_eq!(register(offset, 4, None), Err(Error::Register));
            }
        }
        for offset in [0x64, 0x80, 0x88, 0x90, 0x98, 0x100000, u64::MAX] {
            for width in [1, 2, 4, 8, 1 << 32] {
                assert!(register(offset, width, Some(1)).is_err());
            }
        }
        for width in [0, 1, 2, 8, 1 << 32] {
            assert!(register(4, width, None).is_err());
        }
        assert!(register(4, 4, Some(1 << 32)).is_err());
        assert!(register(8, 4, Some(13)).is_err());
        assert!(register(0x20, 4, Some(0x81)).is_err());
        assert!(register(0x60, 4, Some(3)).is_err());
        assert_eq!(register(8, 4, Some(12)), Ok(1));
    }
    #[test]
    fn exclusive_claim_and_stale_owner_generation() {
        let mut d = claimed();
        assert_eq!(d.claim(22, 0), Err(Error::Busy));
        assert_eq!(d.claim(11, 0), Err(Error::Busy));
        for (owner, generation) in [(0, 1), (22, 1), (11, 0), (11, 2)] {
            assert_eq!(d.check_io(owner, generation, 4, 4, None), Err(Error::Lease));
        }
        d.quarantine(reason::CLOSE);
        d.release(false, 0, false, false, false).unwrap();
        assert_eq!(d.claim(22, 2), Ok(2));
        assert_eq!(d.authorize(11, 1), Err(Error::Lease));
        assert_eq!(d.authorize(22, 1), Err(Error::Lease));
    }
    #[test]
    fn pending_before_wait_delivers_once_and_ack_is_bound_to_receiver() {
        let mut d = claimed();
        trigger(&mut d);
        assert_eq!(d.interrupt(1, 2, 2), Ok(None));
        assert_eq!(d.wait(11, 1, 33, 3), Ok(Some(1)));
        assert_eq!(d.wait(11, 1, 33, 3), Err(Error::Busy));
        for (thread, seq) in [(44, 1), (33, 0), (33, 2), (33, u64::MAX)] {
            assert_eq!(d.check_ack(11, 1, thread, seq), Err(Error::Sequence));
        }
        assert_eq!(d.check_ack(11, 1, 33, 1), Ok(2));
        d.acknowledge(4);
        assert_eq!(d.check_ack(11, 1, 33, 1), Err(Error::Sequence));
        assert_eq!(d.check_io(11, 1, 4, 4, Some(1)), Err(Error::NotReady));
        d.armed(1).unwrap();
        assert_eq!(d.acknowledgements, 1);
    }
    #[test]
    fn wait_before_irq_blocks_one_thread_and_wakes_once() {
        let mut d = claimed();
        assert_eq!(d.wait(11, 1, 33, 0), Ok(None));
        assert_eq!(d.wait(11, 1, 44, 0), Err(Error::Busy));
        trigger(&mut d);
        assert_eq!(d.interrupt(1, 2, 1), Ok(Some((33, 1))));
        assert_eq!(d.interrupt(1, 2, 2), Err(Error::Sequence));
        assert_eq!((d.blocked, d.delivered, d.waiter), (1, 1, 0));
    }
    #[test]
    fn every_terminal_cause_cancels_once_and_denies_io_before_hardware_cleanup() {
        for why in [
            reason::CLOSE,
            reason::REVOKE,
            reason::TERMINATE,
            reason::TIMEOUT,
            reason::BUDGET,
            reason::HARDWARE,
        ] {
            let mut d = claimed();
            d.wait(11, 1, 33, 0).unwrap();
            assert_eq!(d.quarantine(why), Some(33));
            assert_eq!(d.quarantine(why), None);
            assert_eq!(d.check_io(11, 1, 0, 4, None), Err(Error::Quarantined));
            assert_eq!(d.claim(22, 4), Err(Error::Quarantined));
            assert_eq!((d.cancelled, d.quarantines), (1, 1));
        }
    }
    #[test]
    fn completion_flag_alone_cannot_release_inflight_driver() {
        let mut d = claimed();
        trigger(&mut d);
        d.quarantine(reason::TERMINATE);
        d.observe_completion(false, 0);
        assert_eq!(
            d.release(false, 0, false, false, false),
            Err(Error::NotReady)
        );
        d.observe_completion(true, 2);
        assert_eq!(d.flight, 2);
        d.observe_completion(false, 2);
        for (computing, status, enabled, pending, mailbox) in [
            (true, 0, false, false, false),
            (false, 2, false, false, false),
            (false, 0, true, false, false),
            (false, 0, false, true, false),
            (false, 0, false, false, true),
        ] {
            assert_eq!(
                d.release(computing, status, enabled, pending, mailbox),
                Err(Error::NotReady)
            );
        }
        d.release(false, 0, false, false, false).unwrap();
    }
    #[test]
    fn late_interrupts_cannot_wake_reassigned_owner() {
        let mut d = claimed();
        trigger(&mut d);
        d.wait(11, 1, 33, 1).unwrap();
        assert_eq!(d.quarantine(reason::CLOSE), Some(33));
        assert_eq!(d.interrupt(1, 2, 2), Ok(None));
        d.release(false, 0, false, false, false).unwrap();
        d.claim(22, 3).unwrap();
        d.armed(2).unwrap();
        d.wait(22, 2, 44, 3).unwrap();
        assert_eq!(d.interrupt(1, 2, 4), Err(Error::Lease));
        assert_eq!((d.waiter, d.delivered), (44, 0));
    }
    #[test]
    fn finite_io_budget_and_watchdog_cannot_be_reset_by_polling() {
        let mut d = claimed();
        for _ in 0..IO_BUDGET {
            d.check_io(11, 1, 0, 4, None).unwrap();
            d.commit_io(false, 0, 1);
        }
        assert_eq!(d.check_io(11, 1, 0, 4, None), Err(Error::Capacity));
        assert!(!d.expired(WATCHDOG - 1));
        assert!(d.expired(WATCHDOG));
        d.quarantine(reason::TIMEOUT);
        assert!(!d.expired(u64::MAX));
    }
    #[test]
    fn generations_exhaust_without_wrapping_or_claim_mutation() {
        let mut d = Device::new();
        d.discover().unwrap();
        d.generation = u64::MAX;
        assert_eq!(d.claim(11, 0), Err(Error::Capacity));
        assert_eq!((d.phase, d.owner, d.claims), (phase::FREE, 0, 0));
    }
}

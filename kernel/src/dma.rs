//! Per-claim DMA lease. Reserved buffers remain physically stable; leases and
//! IOMMU mappings still have explicit ownership, permissions and retirement.
pub const IOVA: u64 = 0x100000;
pub const BUFFER_BYTES: u64 = 8192;
pub const TRANSFER_BYTES: u64 = 4096;
pub const SUBMIT_LIMIT: u64 = 16;
pub const IRQ: u64 = 0x100;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i64)]
pub enum Error {
    Bounds = -49,
    Permission = -50,
    Mapping = -51,
    Busy = -52,
    Quota = -53,
    Isolation = -54,
    Lease = -55,
}
pub type Result<T> = core::result::Result<T, Error>;
#[repr(C)]
#[derive(Debug, Default)]
pub struct Lease {
    pub owner: u64,
    pub generation: u64,
    pub rights: u64,
    pub busy: u64,
    pub quarantined: u64,
    pub epoch: u64,
    pub invalidated: u64,
    pub submissions: u64,
    pub completions: u64,
    pub offset: u64,
    pub length: u64,
    pub direction: u64,
}
impl Lease {
    pub const fn new() -> Self {
        Self {
            owner: 0,
            generation: 0,
            rights: 0,
            busy: 0,
            quarantined: 0,
            epoch: 0,
            invalidated: 0,
            submissions: 0,
            completions: 0,
            offset: 0,
            length: 0,
            direction: 0,
        }
    }
    pub fn claim(&mut self, owner: u64, generation: u64) -> Result<()> {
        if owner == 0 || generation == 0 || generation <= self.generation {
            return Err(Error::Lease);
        }
        if self.owner != 0 || self.rights != 0 || self.busy != 0 || self.epoch != self.invalidated {
            return Err(Error::Busy);
        }
        self.owner = owner;
        self.generation = generation;
        self.quarantined = 0;
        self.submissions = 0;
        self.completions = 0;
        Ok(())
    }
    pub fn authorize(&self, owner: u64, generation: u64) -> Result<()> {
        if owner == 0 || owner != self.owner || generation != self.generation {
            return Err(Error::Lease);
        }
        if self.quarantined != 0 {
            return Err(Error::Busy);
        }
        Ok(())
    }
    pub fn map(&mut self, rights: u64) -> Result<u64> {
        if rights == 0 || rights & !3 != 0 {
            return Err(Error::Permission);
        }
        if self.rights != 0
            || self.busy != 0
            || self.quarantined != 0
            || self.owner == 0
            || self.epoch != self.invalidated
        {
            return Err(Error::Mapping);
        }
        self.epoch = self.epoch.checked_add(1).ok_or(Error::Quota)?;
        self.rights = rights;
        Ok(self.epoch)
    }
    pub fn unmap(&mut self) -> Result<u64> {
        if self.busy != 0 {
            return Err(Error::Busy);
        }
        if self.rights == 0 || self.epoch != self.invalidated {
            return Err(Error::Mapping);
        }
        self.epoch = self.epoch.checked_add(1).ok_or(Error::Quota)?;
        self.rights = 0;
        Ok(self.epoch)
    }
    pub fn complete_invalidation(&mut self, epoch: u64) -> Result<()> {
        if epoch != self.epoch || epoch == self.invalidated {
            return Err(Error::Isolation);
        }
        self.invalidated = epoch;
        Ok(())
    }
    pub fn submit(&mut self, offset: u64, length: u64, direction: u64) -> Result<u64> {
        if self.owner == 0 || self.quarantined != 0 {
            return Err(Error::Lease);
        }
        if self.busy != 0 || self.epoch != self.invalidated {
            return Err(Error::Busy);
        }
        if !matches!(direction, 1 | 2) || self.rights & direction == 0 {
            return Err(Error::Permission);
        }
        if length == 0
            || length > TRANSFER_BYTES
            || offset
                .checked_add(length)
                .is_none_or(|end| end > BUFFER_BYTES)
        {
            return Err(Error::Bounds);
        }
        if self.submissions == SUBMIT_LIMIT {
            return Err(Error::Quota);
        }
        self.submissions += 1;
        self.busy = 1;
        self.offset = offset;
        self.length = length;
        self.direction = direction;
        Ok(IOVA + offset)
    }
    pub fn complete(&mut self) -> Result<()> {
        if self.busy == 0 {
            return Err(Error::Busy);
        }
        self.busy = 0;
        self.completions += 1;
        Ok(())
    }
    pub fn word(&self, offset: u64) -> Result<usize> {
        if offset & 7 != 0 || offset >= BUFFER_BYTES {
            return Err(Error::Bounds);
        }
        if self.busy != 0 || self.quarantined != 0 {
            return Err(Error::Busy);
        }
        Ok((offset / 8) as usize)
    }
    pub fn release(&mut self, scrubbed: bool, hardware_idle: bool) -> Result<()> {
        if self.quarantined == 0
            || self.busy != 0
            || self.rights != 0
            || self.epoch != self.invalidated
            || !scrubbed
            || !hardware_idle
        {
            return Err(Error::Isolation);
        }
        self.owner = 0;
        self.offset = 0;
        self.length = 0;
        self.direction = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn mapped() -> Lease {
        let mut d = Lease::new();
        d.claim(1, 1).unwrap();
        let e = d.map(3).unwrap();
        d.complete_invalidation(e).unwrap();
        d
    }
    #[test]
    fn maps_need_actual_invalidation_before_submit() {
        let mut d = Lease::new();
        d.claim(1, 1).unwrap();
        let e = d.map(3).unwrap();
        assert_eq!(d.submit(0, 8, 1), Err(Error::Busy));
        assert_eq!(d.complete_invalidation(e + 1), Err(Error::Isolation));
        d.complete_invalidation(e).unwrap();
        assert_eq!(d.submit(4095, 2, 1), Ok(IOVA + 4095));
    }
    #[test]
    fn buffer_range_direction_and_stale_owner_checks_fail_closed() {
        let mut d = mapped();
        assert_eq!(d.authorize(2, 1), Err(Error::Lease));
        assert_eq!(d.authorize(1, 2), Err(Error::Lease));
        for (o, n) in [(0, 0), (0, 4097), (8192, 1), (8191, 2), (u64::MAX, 1)] {
            assert_eq!(d.submit(o, n, 1), Err(Error::Bounds));
        }
        assert_eq!(d.submit(0, 1, 3), Err(Error::Permission));
        for o in [1, 8192, u64::MAX] {
            assert_eq!(d.word(o), Err(Error::Bounds));
        }
        let e = d.unmap().unwrap();
        d.complete_invalidation(e).unwrap();
        let e = d.map(1).unwrap();
        d.complete_invalidation(e).unwrap();
        assert_eq!(d.submit(0, 1, 2), Err(Error::Permission));
    }
    #[test]
    fn in_flight_and_unacknowledged_invalidation_prevent_reuse() {
        let mut d = mapped();
        d.submit(0, 8, 1).unwrap();
        d.quarantined = 1;
        assert_eq!(d.unmap(), Err(Error::Busy));
        assert_eq!(d.word(0), Err(Error::Busy));
        assert_eq!(d.release(true, true), Err(Error::Isolation));
        d.complete().unwrap();
        let e = d.unmap().unwrap();
        assert_eq!(d.release(true, true), Err(Error::Isolation));
        d.complete_invalidation(e).unwrap();
        assert_eq!(d.release(false, true), Err(Error::Isolation));
        d.release(true, true).unwrap();
        d.claim(2, 2).unwrap();
        assert_eq!(d.authorize(1, 1), Err(Error::Lease));
    }
    #[test]
    fn finite_submission_and_epoch_budgets_never_wrap() {
        let mut d = mapped();
        for _ in 0..SUBMIT_LIMIT {
            d.submit(0, 8, 1).unwrap();
            d.complete().unwrap();
        }
        assert_eq!(d.submit(0, 8, 1), Err(Error::Quota));
        d.epoch = u64::MAX;
        d.invalidated = u64::MAX;
        assert_eq!(d.unmap(), Err(Error::Quota));
        assert_eq!(d.rights, 3);
    }
}

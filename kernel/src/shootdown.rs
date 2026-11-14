//! Generation-checked remote translation invalidation. CPU instructions and
//! PTE mutation belong to the architecture adapter; this model prevents reuse
//! before every targeted CPU has acknowledged the current transaction.
use crate::paging::{PAGE, canonical, physical};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Address,
    Participants,
    Busy,
    Generation,
    Identity,
    Duplicate,
    Pending,
    Failed,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    pub epoch: u64,
    pub root: u64,
    pub address: u64,
    pub targets: u64,
}
pub struct Shootdown {
    request: Request,
    acknowledged: u64,
    busy: bool,
    failed: bool,
}
impl Default for Shootdown {
    fn default() -> Self {
        Self::new()
    }
}
impl Shootdown {
    pub const fn new() -> Self {
        Self {
            request: Request {
                epoch: 0,
                root: 0,
                address: 0,
                targets: 0,
            },
            acknowledged: 0,
            busy: false,
            failed: false,
        }
    }
    pub fn begin(
        &mut self,
        root: u64,
        address: u64,
        targets: u64,
        online: u64,
    ) -> Result<Request, Error> {
        if self.failed {
            return Err(Error::Failed);
        }
        if self.busy {
            return Err(Error::Busy);
        }
        if root == 0
            || physical(root, 52).is_err()
            || !canonical(address)
            || address >= 0x8000_0000_0000
            || !address.is_multiple_of(PAGE)
        {
            return Err(Error::Address);
        }
        if targets == 0
            || targets & !online != 0
            || online == 0
            || online >> crate::cpu::MAX_CPUS != 0
        {
            return Err(Error::Participants);
        }
        let epoch = self.request.epoch.checked_add(1).ok_or(Error::Generation)?;
        self.request = Request {
            epoch,
            root,
            address,
            targets,
        };
        self.acknowledged = 0;
        self.busy = true;
        Ok(self.request)
    }
    pub fn acknowledge(&mut self, cpu: usize, request: Request) -> Result<(), Error> {
        self.validate(request)?;
        if cpu >= crate::cpu::MAX_CPUS || self.request.targets & (1 << cpu) == 0 {
            return Err(Error::Identity);
        }
        if self.acknowledged & (1 << cpu) != 0 {
            return Err(Error::Duplicate);
        }
        self.acknowledged |= 1 << cpu;
        Ok(())
    }
    pub fn validate(&self, request: Request) -> Result<(), Error> {
        if self.failed {
            return Err(Error::Failed);
        }
        if !self.busy || request.epoch != self.request.epoch {
            return Err(Error::Generation);
        }
        if request != self.request {
            return Err(Error::Identity);
        }
        Ok(())
    }
    #[cfg(feature = "smp-scheduler-test")]
    pub fn diagnostic_layout(&self) -> [u64; 7] {
        [
            core::ptr::from_ref(&self.request.epoch) as u64,
            core::ptr::from_ref(&self.request.root) as u64,
            core::ptr::from_ref(&self.request.address) as u64,
            core::ptr::from_ref(&self.request.targets) as u64,
            core::ptr::from_ref(&self.acknowledged) as u64,
            core::ptr::from_ref(&self.busy) as u64,
            core::ptr::from_ref(&self.failed) as u64,
        ]
    }
    pub fn commit(&mut self, epoch: u64) -> Result<(), Error> {
        if self.failed {
            return Err(Error::Failed);
        }
        if !self.busy || self.request.epoch != epoch {
            return Err(Error::Generation);
        }
        if self.acknowledged != self.request.targets {
            return Err(Error::Pending);
        }
        self.busy = false;
        Ok(())
    }
    pub fn fail(&mut self) {
        self.failed = true;
    }
    pub fn acknowledged(&self) -> u64 {
        self.acknowledged
    }
    pub fn pending(&self) -> bool {
        self.busy
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_target_must_acknowledge_before_reclamation() {
        let mut s = Shootdown::new();
        let request = s.begin(0x1000, 0x402000, 0b1011, 0b1111).unwrap();
        for cpu in [3, 0] {
            s.acknowledge(cpu, request).unwrap();
        }
        assert_eq!(s.commit(request.epoch), Err(Error::Pending));
        assert!(s.pending());
        s.acknowledge(1, request).unwrap();
        s.commit(request.epoch).unwrap();
        assert!(!s.pending());
        assert_eq!(s.commit(request.epoch), Err(Error::Generation));
    }
    #[test]
    fn stale_foreign_duplicate_and_overlapping_transactions_are_rejected() {
        let mut s = Shootdown::new();
        let first = s.begin(0x1000, 0x402000, 3, 3).unwrap();
        assert_eq!(s.begin(0x2000, 0x403000, 3, 3), Err(Error::Busy));
        assert_eq!(s.acknowledge(2, first), Err(Error::Identity));
        let mut foreign = first;
        foreign.root = 0x2000;
        assert_eq!(s.acknowledge(0, foreign), Err(Error::Identity));
        s.acknowledge(0, first).unwrap();
        assert_eq!(s.acknowledge(0, first), Err(Error::Duplicate));
        s.acknowledge(1, first).unwrap();
        s.commit(first.epoch).unwrap();
        let second = s.begin(0x1000, 0x402000, 3, 3).unwrap();
        assert_eq!(second.epoch, 2);
        assert_eq!(s.acknowledge(0, first), Err(Error::Generation));
        assert_eq!(s.acknowledged(), 0);
    }
    #[test]
    fn invalid_addresses_participant_sets_and_failed_sessions_never_commit() {
        for (root, address, targets, online, expected) in [
            (0, 0x400000, 1, 1, Error::Address),
            (0x1001, 0x400000, 1, 1, Error::Address),
            (0x1000, 0x400001, 1, 1, Error::Address),
            (0x1000, 0xffff_8000_0000_0000, 1, 1, Error::Address),
            (0x1000, 0x400000, 0, 1, Error::Participants),
            (0x1000, 0x400000, 2, 1, Error::Participants),
            (0x1000, 0x400000, 1, 0x11, Error::Participants),
        ] {
            let mut s = Shootdown::new();
            assert_eq!(s.begin(root, address, targets, online), Err(expected));
            assert!(!s.pending());
        }
        let mut s = Shootdown::new();
        let request = s.begin(0x1000, 0x400000, 1, 1).unwrap();
        s.fail();
        assert_eq!(s.acknowledge(0, request), Err(Error::Failed));
        assert_eq!(s.commit(request.epoch), Err(Error::Failed));
        assert_eq!(s.begin(0x1000, 0x400000, 1, 1), Err(Error::Failed));
    }
}

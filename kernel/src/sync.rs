//! Short shared-kernel transactions. Callers disable local IRQs before locking
//! and release the guard before user entry, IPI delivery or waiting for a CPU.
//! Interrupt handlers never acquire this lock. The only shared mutable borrow
//! is created after Acquire and ends before Release.
use core::{
    cell::UnsafeCell,
    marker::PhantomData,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

pub struct SpinLock<T> {
    locked: AtomicBool,
    owner: AtomicU64,
    contentions: AtomicU64,
    value: UnsafeCell<T>,
}
// SAFETY: All accesses to T require the exclusive Acquire/Release guard. T may
// move between owners only when it is Send. A guard cannot move to another CPU.
unsafe impl<T: Send> Sync for SpinLock<T> {}
unsafe impl<T: Send> Send for SpinLock<T> {}

impl<T> SpinLock<T> {
    pub const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            owner: AtomicU64::new(0),
            contentions: AtomicU64::new(0),
            value: UnsafeCell::new(value),
        }
    }
    pub fn try_lock(&self, owner: u64) -> Option<Guard<'_, T>> {
        if owner == 0 {
            return None;
        }
        if self
            .locked
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            self.contentions.fetch_add(1, Ordering::Relaxed);
            return None;
        }
        self.owner.store(owner, Ordering::Relaxed);
        Some(Guard {
            lock: self,
            not_send: PhantomData,
        })
    }
    pub fn lock_bounded(&self, owner: u64, attempts: usize) -> Option<Guard<'_, T>> {
        for _ in 0..attempts {
            if let Some(guard) = self.try_lock(owner) {
                return Some(guard);
            }
            core::hint::spin_loop();
        }
        None
    }
    pub fn owner(&self) -> u64 {
        self.owner.load(Ordering::Acquire)
    }
    pub fn contentions(&self) -> u64 {
        self.contentions.load(Ordering::Relaxed)
    }
    #[cfg(feature = "smp-scheduler-test")]
    pub fn diagnostic_layout(&self) -> [u64; 3] {
        [
            core::ptr::from_ref(&self.locked) as u64,
            core::ptr::from_ref(&self.owner) as u64,
            core::ptr::from_ref(&self.contentions) as u64,
        ]
    }
}
pub struct Guard<'a, T> {
    lock: &'a SpinLock<T>,
    not_send: PhantomData<*mut ()>,
}
impl<T> Deref for Guard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        // SAFETY: This guard owns the only successful lock acquisition.
        unsafe { &*self.lock.value.get() }
    }
}
impl<T> DerefMut for Guard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: No other guard exists; the mutable borrow ends before unlock.
        unsafe { &mut *self.lock.value.get() }
    }
}
impl<T> Drop for Guard<'_, T> {
    fn drop(&mut self) {
        self.lock.owner.store(0, Ordering::Relaxed);
        self.lock.locked.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::Arc, thread};

    #[test]
    fn duplicate_lock_and_zero_owner_fail_without_changing_protected_state() {
        let lock = SpinLock::new(7);
        assert!(lock.try_lock(0).is_none());
        let mut first = lock.try_lock(1).unwrap();
        assert!(lock.try_lock(1).is_none());
        assert!(lock.try_lock(2).is_none());
        assert_eq!(lock.owner(), 1);
        *first = 11;
        drop(first);
        assert_eq!(lock.owner(), 0);
        assert_eq!(*lock.try_lock(2).unwrap(), 11);
    }

    #[test]
    fn real_host_threads_observe_one_mutator_and_release_published_updates() {
        let lock = Arc::new(SpinLock::new((0u64, [0u64; 4])));
        let workers: std::vec::Vec<_> = (0..4)
            .map(|cpu| {
                let lock = Arc::clone(&lock);
                thread::spawn(move || {
                    for _ in 0..10_000 {
                        let mut guard = loop {
                            if let Some(g) = lock.lock_bounded(cpu + 1, 1000) {
                                break g;
                            }
                            thread::yield_now();
                        };
                        assert_eq!(guard.0, guard.1.iter().sum::<u64>());
                        guard.1[cpu as usize] += 1;
                        guard.0 += 1;
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        let guard = lock.try_lock(5).unwrap();
        assert_eq!(*guard, (40_000, [10_000; 4]));
    }
}

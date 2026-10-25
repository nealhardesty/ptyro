//! Locks that survive a panic elsewhere (ROADMAP H2.1).
//!
//! A `std` mutex is *poisoned* when a thread panics while holding it, and every later `lock()`
//! returns an error. The daemon had hundreds of `lock().expect(..)`, so one bug in one handler,
//! while it held a session's `meta` lock, turned into a panic in every request that touched that
//! session, and from there into a daemon that answered nothing. These take the guard anyway: the
//! data may be half-updated (what the panicking code was doing), but the daemon keeps serving
//! everything else, which is the better failure. A panic is still logged where it happens.

use std::sync::{Mutex, MutexGuard, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

pub trait MutexExt<T> {
    /// Lock, whether or not another thread panicked while holding it.
    fn locked(&self) -> MutexGuard<'_, T>;
}

impl<T> MutexExt<T> for Mutex<T> {
    fn locked(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub trait RwLockExt<T> {
    /// Read-lock, poisoned or not.
    fn reading(&self) -> RwLockReadGuard<'_, T>;
    /// Write-lock, poisoned or not.
    fn writing(&self) -> RwLockWriteGuard<'_, T>;
}

impl<T> RwLockExt<T> for RwLock<T> {
    fn reading(&self) -> RwLockReadGuard<'_, T> {
        self.read().unwrap_or_else(PoisonError::into_inner)
    }

    fn writing(&self) -> RwLockWriteGuard<'_, T> {
        self.write().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, RwLock};

    use super::*;

    #[test]
    fn a_panic_while_holding_a_lock_does_not_break_the_next_user() {
        let m = Arc::new(Mutex::new(1));
        let m2 = Arc::clone(&m);
        let _ = std::thread::spawn(move || {
            let _g = m2.locked();
            panic!("a handler bug");
        })
        .join();
        assert!(m.lock().is_err(), "std says it is poisoned");
        *m.locked() += 1;
        assert_eq!(*m.locked(), 2);

        let r = Arc::new(RwLock::new(vec![1]));
        let r2 = Arc::clone(&r);
        let _ = std::thread::spawn(move || {
            let _g = r2.writing();
            panic!("another");
        })
        .join();
        r.writing().push(2);
        assert_eq!(*r.reading(), vec![1, 2]);
    }
}

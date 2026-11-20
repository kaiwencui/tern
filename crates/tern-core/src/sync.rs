//! The locks of this crate.

use parking_lot::{Mutex, MutexGuard};

pub struct FairMutex<T> {
    data: Mutex<T>,
    next: Mutex<()>,
}

impl<T> FairMutex<T> {
    pub const fn new(value: T) -> Self {
        Self {
            data: Mutex::new(value),
            next: Mutex::new(()),
        }
    }

    #[inline]
    pub fn lease(&self) -> MutexGuard<'_, ()> {
        self.next.lock()
    }

    #[inline]
    pub fn lock_unfair(&self) -> MutexGuard<'_, T> {
        self.data.lock()
    }
}

/// Locks a mutex, ignoring poisoning: every value guarded in this crate stays
/// consistent even if a holder panicked.
pub(crate) fn lock<T>(m: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

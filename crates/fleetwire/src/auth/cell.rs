//! The single-flight cell that guards the token state (spec §0 I1, I2).
//!
//! The lock is held only for the short sections that read and write the state. The refresh
//! (file lock, POST, save) is done outside the lock with the `refreshing` flag raised, and the
//! other callers wait on a `Condvar` until the deadline for the flag to be lowered. After a
//! panic during a refresh or after lock poisoning, the state is not discarded but put on the
//! safe side with `Recover::recover`.

use std::ops::{Deref, DerefMut};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::{Duration, Instant};

/// Returns the state to the safe side after poisoning or a panic during a refresh.
pub(crate) trait Recover {
    fn recover(&mut self);
}

/// The lock was not obtained within the deadline.
#[derive(Debug)]
pub(crate) struct LockTimeout;

struct Inner<S> {
    state: S,
    refreshing: bool,
}

/// Guards one state together with the "refreshing" flag.
pub(crate) struct StateCell<S> {
    inner: Mutex<Inner<S>>,
    cv: Condvar,
}

/// Access to the state while the lock is held.
pub(crate) struct CellGuard<'a, S: Recover> {
    cell: &'a StateCell<S>,
    guard: MutexGuard<'a, Inner<S>>,
}

/// The right to refresh. If it is dropped without `finish` (panics included), it recovers the
/// state and wakes the waiters.
pub(crate) struct RefreshTicket<'a, S: Recover> {
    cell: &'a StateCell<S>,
    done: bool,
}

impl<S: Recover> StateCell<S> {
    pub(crate) fn new(state: S) -> Self {
        Self {
            inner: Mutex::new(Inner {
                state,
                refreshing: false,
            }),
            cv: Condvar::new(),
        }
    }

    fn lock_raw(&self) -> MutexGuard<'_, Inner<S>> {
        match self.inner.lock() {
            Ok(g) => g,
            Err(p) => {
                let mut g = p.into_inner();
                g.state.recover();
                self.inner.clear_poison();
                g
            }
        }
    }

    /// Obtains the lock within the deadline, waiting for another thread's refresh to finish.
    pub(crate) fn lock_within(&self, deadline: Duration) -> Result<CellGuard<'_, S>, LockTimeout> {
        let until = Instant::now().checked_add(deadline).ok_or(LockTimeout)?;
        let mut guard = self.lock_raw();
        while guard.refreshing {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(LockTimeout);
            }
            guard = match self.cv.wait_timeout(guard, left) {
                Ok((g, _)) => g,
                Err(p) => {
                    let (mut g, _) = p.into_inner();
                    g.state.recover();
                    self.inner.clear_poison();
                    g
                }
            };
        }
        Ok(CellGuard { cell: self, guard })
    }
}

impl<'a, S: Recover> CellGuard<'a, S> {
    /// Raises the flag and releases the lock. The refresh is done outside the lock.
    pub(crate) fn begin_refresh(mut self) -> RefreshTicket<'a, S> {
        self.guard.refreshing = true;
        let cell = self.cell;
        drop(self);
        RefreshTicket { cell, done: false }
    }
}

impl<S: Recover> Deref for CellGuard<'_, S> {
    type Target = S;
    fn deref(&self) -> &S {
        &self.guard.state
    }
}

impl<S: Recover> DerefMut for CellGuard<'_, S> {
    fn deref_mut(&mut self) -> &mut S {
        &mut self.guard.state
    }
}

impl<S: Recover> RefreshTicket<'_, S> {
    /// Retakes the lock, applies the result, lowers the flag and then wakes the waiters.
    pub(crate) fn finish<R>(mut self, f: impl FnOnce(&mut S) -> R) -> R {
        let mut g = self.cell.lock_raw();
        let out = f(&mut g.state);
        g.refreshing = false;
        drop(g);
        self.done = true;
        self.cell.cv.notify_all();
        out
    }
}

impl<S: Recover> Drop for RefreshTicket<'_, S> {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        let mut g = self.cell.lock_raw();
        g.state.recover();
        g.refreshing = false;
        drop(g);
        self.cell.cv.notify_all();
    }
}

#[cfg(test)]
mod tests;

//! A fixed-inode file lock — mutual exclusion of refresh between processes.
//!
//! This is one half of REQ-15 (**between** processes). The other half (single-flight within
//! the same process) is handled by [`super::cell::StateCell`].
//!
//! **Why `try_lock()` + retry rather than a blocking `lock()`.**
//! `flock(2)` is a blocking syscall and **has no timeout argument** (the "flock(LOCK_EX) with
//! a timeout" of design §7.3 is, taken literally, an API that does not exist). The only form
//! that can produce a finite timeout is a non-blocking attempt + backoff retry + deadline.
//!
//! A2 Task 7 ([`super::TokenManager`]) calls this module, so every `dead_code` attribute that
//! Task 5 had attached has been removed.

use std::fs::{File, OpenOptions, TryLockError};
use std::os::unix::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::path::Path;
use std::time::{Duration, Instant};

/// The overall deadline for acquiring the lock.
///
/// It is comfortably longer than one refresh by a sibling process (HTTP round trip + save to
/// disk), and short enough to finish before a person feels that it "has hung".
pub(crate) const LOCK_DEADLINE: Duration = Duration::from_secs(10);
/// The first retry interval. Most contention is resolved by this single retry.
pub(crate) const LOCK_RETRY_INITIAL: Duration = Duration::from_millis(5);
/// The upper bound on the retry interval. The exponential growth stops here — without a
/// bound, the caller would sleep for one more last interval (which grows to the order of
/// seconds) even after the sibling has released the lock.
pub(crate) const LOCK_RETRY_MAX: Duration = Duration::from_millis(200);

/// A failure to acquire the lock.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub(crate) enum LockError {
    /// The lock was not obtained within the deadline. This means a sibling process has been
    /// holding it for a long time.
    #[error("timed out acquiring the token lock")]
    Timeout,
    /// An I/O error on the lock file itself.
    ///
    /// The cause is carried in the message as is. EISDIR, EACCES and ENOSPC each call for a
    /// different remedy, and if the cause were discarded all three would reach the user as
    /// the same eight words. This is safe under REQ-18 — `std::io::Error` carries neither a
    /// path nor file contents (we do not wrap the `open` error together with the path).
    /// Because the cause is carried in the message, it is not passed on through `source()`
    /// (`#[from]` is not used; error style rule R4 — the `std::error::Error` docs: only one
    /// of Display and source).
    #[error("token lock io error: {0}")]
    Io(std::io::Error),
}

impl From<std::io::Error> for LockError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

/// A guard that lives for as long as the lock is held. It releases the lock in `Drop`.
///
/// This type **does not delete the lock file** — on the success, failure or panic path
/// alike. Deleting it would give rise to a process holding the existing inode and a process
/// taking a new inode at the same time, breaking mutual exclusion (design §7.3). Cleanup when
/// a process dies is done not by `unlink` but by **the kernel reclaiming the fd** — a flock
/// is released when the open file description is closed.
#[derive(Debug)]
pub(crate) struct LockGuard {
    file: File,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        // Nothing can be done if this fails. If the process dies the kernel releases the
        // lock anyway. This Drop also runs during unwinding — a process that panicked does
        // not stay behind holding the lock
        // (`the_lock_is_released_when_the_guard_drops_during_a_panic`).
        //
        // **In the current structure, behaviour is the same without this one line.** The
        // guard owns the `File`, so even without this line the `File` is dropped right
        // after, `close(2)` reclaims the open file descriptor and the kernel releases the
        // flock. In fact a mutation that deletes this line breaks no test (an equivalent
        // mutation) — not because the tests are weak but because there is no observable
        // moment between `unlock()` and `close()`. The reason it is kept anyway: if this
        // guard is later changed to `try_clone`/share the `File`, close alone will not
        // release the lock, and at that point the moment of release has to be explicit.
        let _ = File::unlock(&self.file);
    }
}

/// Acquires the exclusive lock within `deadline`.
///
/// On success the lock is held for as long as the returned [`LockGuard`] lives. If the lock
/// file does not exist it is created; if it does, it is **opened as is** — not truncated
/// (no contents are written so there is nothing to cut, and truncating would change the file
/// from the point of view of another holder).
///
/// # Threat model — the lock file has no `ino`/`dev` re-check (intentional)
///
/// `TokenStore::load` re-verifies by `ino`/`dev` that the file it opens is the one it has
/// just checked. **That is not done here.** Unlike the tokens, the lock file's contents are
/// neither read nor written, so what is lost when an inode swapped in by someone else is
/// taken is not *confidentiality* but only *mutual exclusion*. And whoever can do that swap
/// is someone who can write to the token directory, and that person can replace
/// `tokens.json` itself — as the doc of `TokenStore::ensure_private_dir` says, that threat
/// **cannot be prevented by file-level checks** and is prevented by the premise that the
/// directory is 0700. Adding a re-check to the lock alone would be an asymmetry that guards
/// the weaker asset more heavily, and the window between the check and `flock` does not
/// close anyway (it is enough to swap after the check).
///
/// That is, there is one accepted premise: **only the owner can write to the token
/// directory.** If that premise breaks, it is not the integrity of the lock but the tokens
/// themselves that have already collapsed.
pub(crate) fn acquire(path: &Path, deadline: Duration) -> Result<LockGuard, LockError> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        // This is `DirBuilder::mode(0o700)`, not `create_dir_all`. The lock file lives in
        // the **same directory** as the tokens (`TokenStore::lock_path`), so creating the
        // directory here as 0755 (what the umask leaves) would make every later
        // `TokenStore::store`/`load` fail permanently with `BadDirPermissions`.
        // The `~/.fleetwire/ 0700` of design §4 is upheld on this path too.
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
    }
    // Fixed inode: open it if it exists, create it if not. `.mode(0o600)` applies **only on
    // creation** (the permissions of an existing file are not changed — not silently fixing
    // the permissions of someone else's file is the same policy as store.rs).
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?;

    retry_until_locked(deadline, || File::try_lock(&file))?;
    Ok(LockGuard { file })
}

/// Retries with exponential backoff until `attempt` succeeds, giving up at `deadline`.
///
/// It is split out of [`acquire`] for two reasons. (1) This function is the whole of the
/// "finite timeout" logic of REQ-15, and (2) the distinction between `TryLockError::Error`
/// (a real failure — stop immediately) and `WouldBlock` (contention — retry) **cannot be
/// tested without injection** — there is no way to make `flock` return an error other than
/// `EWOULDBLOCK` on a local Linux filesystem. Blurring that distinction would mean retrying
/// pointlessly up to the deadline on a disk error, so it is better to have a testable seam.
///
/// The deadline check being at the **end** of the loop is intentional: `deadline == 0` means
/// not "do not attempt" but "do not retry", so there is always exactly one attempt.
fn retry_until_locked<F>(deadline: Duration, mut attempt: F) -> Result<(), LockError>
where
    F: FnMut() -> Result<(), TryLockError>,
{
    let started = Instant::now();
    let mut backoff = LOCK_RETRY_INITIAL;
    loop {
        match attempt() {
            Ok(()) => return Ok(()),
            // Contention — retry.
            Err(TryLockError::WouldBlock) => {}
            // A real I/O error — retrying will not make it better.
            Err(TryLockError::Error(e)) => return Err(LockError::Io(e)),
        }
        let elapsed = started.elapsed();
        if elapsed >= deadline {
            return Err(LockError::Timeout);
        }
        // Never sleep past the remaining time — this keeps the overshoot of the deadline minimal.
        // The check above guarantees `elapsed < deadline`, but instead of relying on that
        // guarantee the subtraction itself is made unable to overflow
        // (clippy `unchecked_time_subtraction`, 10-14).
        std::thread::sleep(backoff.min(deadline.saturating_sub(elapsed)));
        backoff = (backoff * 2).min(LOCK_RETRY_MAX);
    }
}

#[cfg(test)]
mod tests;

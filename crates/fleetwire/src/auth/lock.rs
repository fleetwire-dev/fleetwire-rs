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
        // 실패해도 할 수 있는 일이 없다. 프로세스가 죽으면 커널이 어차피 해제한다.
        // 언와인딩 중에도 이 Drop 은 실행된다 — 패닉한 프로세스가 락을 쥔 채
        // 남지 않는다(`the_lock_is_released_when_the_guard_drops_during_a_panic`).
        //
        // **이 한 줄은 현재 구조에서는 없어도 동작이 같다.** 가드가 `File` 을
        // 소유하므로, 이 줄이 없어도 바로 뒤에서 `File` 이 drop 되며 `close(2)` 가
        // 열린 파일 서술자를 회수하고 커널이 flock 을 푼다. 실제로 이 줄을 지우는
        // 변이는 어떤 테스트도 깨뜨리지 않는다(등가 변이) — 테스트가 부실해서가
        // 아니라 `unlock()` 과 `close()` 사이에 관측 가능한 순간이 없어서다.
        // 그래도 남겨 두는 이유: 나중에 이 가드가 `File` 을 `try_clone`/공유하도록
        // 바뀌면 close 만으로는 안 풀리고, 그때 해제 시점이 명시적이어야 한다.
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
        // `create_dir_all` 이 아니라 `DirBuilder::mode(0o700)` 이다. 락 파일은
        // 토큰과 **같은 디렉터리**에 있으므로(`TokenStore::lock_path`), 여기서
        // umask 를 먹은 0755 로 디렉터리를 만들면 그 뒤의 모든
        // `TokenStore::store`/`load` 가 `BadDirPermissions` 로 영구히 실패한다.
        // 설계 §4 의 `~/.fleetwire/ 0700` 을 이 경로에서도 지킨다.
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
    }
    // 고정 inode: 있으면 열고 없으면 만든다. `.mode(0o600)` 은 **새로 만들 때만**
    // 적용된다(이미 있는 파일의 권한은 바꾸지 않는다 — 남의 파일 권한을 조용히
    // 고치지 않는 것은 store.rs 와 같은 방침이다).
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
            // 경합 — 재시도한다.
            Err(TryLockError::WouldBlock) => {}
            // 실제 입출력 오류 — 재시도해도 나아지지 않는다.
            Err(TryLockError::Error(e)) => return Err(LockError::Io(e)),
        }
        let elapsed = started.elapsed();
        if elapsed >= deadline {
            return Err(LockError::Timeout);
        }
        // 남은 시간을 넘겨 자지 않는다 — deadline 초과폭을 최소화한다.
        // 위 검사가 `elapsed < deadline` 을 보장하지만, 그 보장에 기대지 않고 뺄셈 자체가 넘치지 않게 합니다
        // (clippy `unchecked_time_subtraction`, 10-14).
        std::thread::sleep(backoff.min(deadline.saturating_sub(elapsed)));
        backoff = (backoff * 2).min(LOCK_RETRY_MAX);
    }
}

#[cfg(test)]
mod tests;

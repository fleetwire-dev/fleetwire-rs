use super::*;
use std::cell::Cell;
use std::fs::TryLockError;
use std::os::unix::fs::PermissionsExt as _;
use std::time::{Duration, Instant};

/// A guard that holds together a lock directory dedicated to this process and thread and the
/// lock path inside it.
///
/// The implementation **never deletes** the lock file (REQ-15: fixed inode). So unless the
/// test cleans up, that file and its directory stay behind whole in `/tmp` — this guard
/// removes the whole directory when it is dropped.
///
/// `Deref<Target = Path>` gives the **lock file path** — existing call sites such as `&path`
/// and `path.exists()` compile unchanged. Do not discard it immediately (a form such as
/// `temp_lock_path("x").parent()` deletes the directory on the spot).
struct TempLock {
    dir: std::path::PathBuf,
    file: std::path::PathBuf,
}

impl std::ops::Deref for TempLock {
    type Target = std::path::Path;
    fn deref(&self) -> &std::path::Path {
        &self.file
    }
}

impl AsRef<std::path::Path> for TempLock {
    fn as_ref(&self) -> &std::path::Path {
        &self.file
    }
}

impl Drop for TempLock {
    /// Never panics (a double panic during unwinding = SIGABRT). The permissions are restored
    /// first for the same reason as in the `store.rs` fixture — it prevents `remove_dir_all`
    /// from failing silently on a directory left at 0500.
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700));
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Creates one [`TempLock`].
///
/// The old doc said "the pid and thread id are reused on **the next run**, so the pre-delete
/// clears the previous run's lock file", but that was **wrong.** The pid changes on every
/// `cargo test` run, so the names almost never matched, and so the pre-delete cleared nothing
/// while directories accumulated run after run (the incident of a 76-minute boot delay — the
/// measured figures are written in one place only, the `crate::test_support::temp_dir` doc).
/// What actually upholds the "acquire creates the file" assertion is `Drop`.
///
/// **Bind the return value to a name.** If it is chained directly, as in
/// `temp_lock_path("x").parent()`, the guard is dropped at the end of that statement and the
/// directory disappears, and then `acquire` creates the same tree again, which stays behind
/// as leftovers nobody deletes
/// (`acquire_creates_a_missing_lock_directory_owner_only` really did that).
fn temp_lock_path(tag: &str) -> TempLock {
    let dir = std::env::temp_dir().join(format!(
        "fleetwire-lock-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join("tokens.lock");
    TempLock { dir, file }
}

fn mode_of(path: &std::path::Path) -> u32 {
    std::fs::metadata(path)
        .expect("must exist")
        .permissions()
        .mode()
        & 0o777
}

fn inode_of(path: &std::path::Path) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(path).expect("must exist").ino()
}

/// Checks "nobody but the owner has any permission", regardless of the umask.
///
/// Comparing the whole value with `assert_eq!(mode, 0o700)` would make a **correct**
/// implementation fail for nothing on a machine with umask 0277 — a umask cannot grant more
/// permissions, so only this direction is asserted.
#[track_caller]
fn assert_owner_only(path: &std::path::Path, what: &str) {
    let mode = mode_of(path);
    assert_eq!(
        mode & 0o077,
        0,
        "{what}: permission bits for others than the owner remain (mode {mode:o})"
    );
}

#[test]
fn acquire_succeeds_on_a_fresh_path_and_creates_the_lock_file() {
    let path = temp_lock_path("fresh");
    assert!(
        !path.exists(),
        "the fixture must not create the lock file in advance"
    );
    let guard = acquire(&path, LOCK_DEADLINE).expect("must acquire");
    assert!(path.exists(), "the lock file must be created");
    drop(guard);
    assert!(
        path.exists(),
        "the file must remain even after the lock is released (fixed inode)"
    );
}

#[test]
fn a_second_holder_times_out_while_the_first_holds_it() {
    // A flock is per open file description, so two opens in the same process exclude each
    // other, which reproduces a "sibling process" without a real child process.
    let path = temp_lock_path("contended");
    let first = acquire(&path, LOCK_DEADLINE).expect("first must acquire");

    let started = Instant::now();
    let second = acquire(&path, Duration::from_millis(300));
    let elapsed = started.elapsed();

    assert!(
        matches!(second, Err(LockError::Timeout)),
        "the second must time out"
    );
    assert!(
        elapsed >= Duration::from_millis(300),
        "gave up before the deadline: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "greatly exceeded the deadline: {elapsed:?}"
    );

    drop(first);
}

#[test]
fn the_lock_is_reacquirable_after_the_guard_drops() {
    let path = temp_lock_path("reacquire");
    drop(acquire(&path, LOCK_DEADLINE).expect("first"));
    // If Drop does not unlock, this times out here.
    let second = acquire(&path, Duration::from_millis(500));
    assert!(second.is_ok(), "Drop did not release the lock");
}

#[test]
fn the_lock_file_keeps_its_inode_across_acquire_release_cycles() {
    // The core of REQ-15: the lock file must not be deleted. An assertion that looks only at
    // `path.exists()` lets a "delete and recreate" implementation pass, and that
    // implementation keeps alive **at the same time** a sibling process holding the old inode
    // and a process taking the new inode — mutual exclusion silently disappears. The
    // observable trace is the inode.
    let path = temp_lock_path("inode");
    let first = acquire(&path, LOCK_DEADLINE).expect("first");
    let ino = inode_of(&path);
    drop(first);
    assert_eq!(
        inode_of(&path),
        ino,
        "the lock file changed on the release path"
    );

    let second = acquire(&path, LOCK_DEADLINE).expect("second");
    assert_eq!(
        inode_of(&path),
        ino,
        "reacquiring created the lock file anew"
    );
    drop(second);
    assert_eq!(
        inode_of(&path),
        ino,
        "the lock file changed on the second release"
    );
}

#[test]
fn a_timed_out_acquire_leaves_the_lock_file_in_place() {
    // An assertion that looks only at the success path lets a "clean up on failure"
    // implementation pass. If the side that timed out removes the lock file, **the inode of
    // the side holding the lock** disappears from the directory, and the next process creates
    // a new inode and gets in at the same time.
    let path = temp_lock_path("timeout-keeps");
    let first = acquire(&path, LOCK_DEADLINE).expect("first");
    let ino = inode_of(&path);

    let second = acquire(&path, Duration::from_millis(50));
    assert!(matches!(second, Err(LockError::Timeout)), "must time out");
    assert!(path.exists(), "the timeout deleted the lock file");
    assert_eq!(inode_of(&path), ino, "the timeout replaced the lock file");

    drop(first);
}

#[test]
fn the_lock_is_released_when_the_guard_drops_during_a_panic() {
    // A test that looks only at a normal drop does not see the unwinding path. If this crate
    // is switched to `panic = "abort"` or the release is moved into an explicit `release()`,
    // a process that panicked stays behind still holding the lock and its siblings are
    // blocked until the deadline.
    let path = temp_lock_path("panic-unwind");
    let guard = acquire(&path, LOCK_DEADLINE).expect("first");

    // The panic hook is process-global, so it is not touched (tests run in parallel).
    // The message the standard hook leaves on stderr is intended noise.
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _held = guard;
        panic!("intentional panic: lock must still be released while unwinding");
    }));
    assert!(caught.is_err(), "the panic must be caught");

    let second = acquire(&path, Duration::from_millis(500));
    assert!(
        second.is_ok(),
        "Drop did not release the lock during unwinding"
    );
    assert!(
        path.exists(),
        "the lock file disappeared on the unwinding path"
    );
}

#[test]
fn a_zero_deadline_still_attempts_the_lock_once() {
    // An implementation that checks the deadline **first** and then sleeps (the form with
    // `if elapsed >= deadline` at the head of the loop) yields Timeout immediately even on a
    // free lock. 0 means not "do not attempt" but "do not retry".
    let path = temp_lock_path("zero-free");
    let guard = acquire(&path, Duration::ZERO);
    assert!(
        guard.is_ok(),
        "a free lock must be acquired even with deadline 0: {:?}",
        guard.err()
    );
}

#[test]
fn a_zero_deadline_makes_exactly_one_attempt_before_timing_out() {
    // The other side of the test above. Under contention it attempts exactly once and gives
    // up — this rules out an implementation that ignores 0 and sleeps at least once.
    let attempts = Cell::new(0_u32);
    let started = Instant::now();
    let outcome = retry_until_locked(Duration::ZERO, || {
        attempts.set(attempts.get() + 1);
        Err(TryLockError::WouldBlock)
    });

    assert!(matches!(outcome, Err(LockError::Timeout)), "{outcome:?}");
    assert_eq!(attempts.get(), 1, "retried despite deadline 0");
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "slept despite deadline 0: {:?}",
        started.elapsed()
    );
}

#[test]
fn a_real_lock_error_aborts_immediately_instead_of_retrying() {
    // Blurring `TryLockError::Error` with `WouldBlock` means retrying pointlessly up to the
    // deadline on a disk error (it keeps the finiteness of REQ-15 but throws away 10
    // seconds). The attempt count pins that distinction.
    let attempts = Cell::new(0_u32);
    let started = Instant::now();
    let outcome = retry_until_locked(Duration::from_secs(10), || {
        attempts.set(attempts.get() + 1);
        Err(TryLockError::Error(std::io::Error::other(
            "simulated disk failure",
        )))
    });

    assert!(matches!(outcome, Err(LockError::Io(_))), "{outcome:?}");
    assert_eq!(attempts.get(), 1, "retried on a real I/O error");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "waited until the deadline despite a real error: {:?}",
        started.elapsed()
    );
}

#[test]
fn contention_is_retried_rather_than_treated_as_failure() {
    // The other side: this rules out an implementation that turns `WouldBlock` into an
    // immediate failure.
    let attempts = Cell::new(0_u32);
    let outcome = retry_until_locked(Duration::from_secs(10), || {
        attempts.set(attempts.get() + 1);
        if attempts.get() < 4 {
            Err(TryLockError::WouldBlock)
        } else {
            Ok(())
        }
    });

    assert!(outcome.is_ok(), "contention must be retried: {outcome:?}");
    assert_eq!(attempts.get(), 4);
}

#[test]
fn the_retry_interval_backs_off_instead_of_spinning_at_the_initial_value() {
    // An implementation with the backoff growth deleted (always 5ms) passes every timing
    // assertion — the only visible difference is the **attempt count**. Over 500ms of
    // contention, exponential backoff sleeps 5, 10, 20, 40, 80, 160, 200, which is around 8
    // attempts, while a fixed 5ms is around 100 attempts. An implementation with the two
    // constants swapped (initial 200ms / cap 5ms) is caught here too, at around 62 attempts.
    //
    // Only the upper bound is asserted: under load each sleep only gets **longer**, so the
    // attempts go down and never up, and this direction does not fail for nothing on a slow
    // machine.
    let attempts = Cell::new(0_u32);
    let outcome = retry_until_locked(Duration::from_millis(500), || {
        attempts.set(attempts.get() + 1);
        Err(TryLockError::WouldBlock)
    });

    assert!(matches!(outcome, Err(LockError::Timeout)), "{outcome:?}");
    assert!(
        attempts.get() <= 25,
        "the backoff did not grow, so it woke as many as {} times in 500ms",
        attempts.get()
    );
    // The other side: this rules out an implementation that does not retry at all.
    assert!(
        attempts.get() >= 3,
        "there were almost no retries: {}",
        attempts.get()
    );
}

#[test]
fn a_freed_lock_is_picked_up_long_before_the_deadline() {
    // Checks that the retry interval is actually short. An implementation that makes the
    // first interval as large as the deadline breaks none of the tests above (the timeout
    // tests still pass), yet it makes the caller sleep 10 seconds even after the sibling has
    // released the lock.
    let path = temp_lock_path("prompt");
    let first = acquire(&path, LOCK_DEADLINE).expect("first");
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        drop(first);
    });

    let started = Instant::now();
    acquire(&path, Duration::from_secs(5)).expect("a released lock should be acquired");
    let elapsed = started.elapsed();

    // The expected value is ~40ms. Only a very generous upper bound is asserted, for the
    // sake of loaded machines.
    assert!(
        elapsed < Duration::from_millis(1_500),
        "slept for a long time although the lock was released: {elapsed:?}"
    );
    releaser.join().expect("releaser");
}

#[test]
fn a_lock_held_for_a_while_is_still_picked_up_promptly_after_release() {
    // An implementation with the cap (`LOCK_RETRY_MAX`) deleted — `backoff *= 2` — breaks
    // none of the tests above. A lock released after 30ms is acquired before the backoff
    // grows, the timeout tests still pass thanks to the `min(remaining)` clamp, and the
    // attempt count **only goes down**, so the upper-bound assertion does not catch it
    // either.
    //
    // The only visible difference is "the reaction delay when a sibling held the lock for a
    // long time and then released it". With the cap the sleep boundaries are ...515, 715,
    // 915, 1115, 1315, 1515ms, so a release at 1400ms is picked up at ~1515ms, but without
    // the cap the caller is in the middle of a 2560ms sleep that started at 635ms and cannot
    // pick it up until ~3835ms. The test separates the two.
    let path = temp_lock_path("cap");
    let first = acquire(&path, LOCK_DEADLINE).expect("first");
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1_400));
        drop(first);
    });

    let started = Instant::now();
    acquire(&path, LOCK_DEADLINE).expect("a released lock should be acquired");
    let elapsed = started.elapsed();

    assert!(
        elapsed >= Duration::from_millis(1_400),
        "acquired while the sibling was still holding it: {elapsed:?}"
    );
    // Normal ~1.5 seconds / the implementation without a cap ~3.8 seconds. The line is drawn
    // with ~1 second of margin on each side.
    assert!(
        elapsed < Duration::from_millis(2_500),
        "no backoff cap, so it slept long even after the release: {elapsed:?}"
    );
    releaser.join().expect("releaser");
}

#[test]
fn acquiring_does_not_modify_the_lock_file_contents() {
    // For the same reason as "never delete", the lock file's **contents are not touched
    // either** — using `truncate(true)` when opening silently wipes what a sibling left.
    // Nothing is written now, but the moment something like a diagnostic pid is put in, it
    // becomes a silent loss.
    let path = temp_lock_path("no-truncate");
    std::fs::write(&path, b"KEEP_ME").expect("seed the lock file");

    let guard = acquire(&path, LOCK_DEADLINE).expect("must acquire");
    assert_eq!(
        std::fs::read(&path).expect("read"),
        b"KEEP_ME",
        "acquire truncated the lock file contents"
    );
    drop(guard);
    assert_eq!(
        std::fs::read(&path).expect("read"),
        b"KEEP_ME",
        "the release truncated the lock file contents"
    );
}

#[test]
fn the_retry_interval_grows_from_the_initial_toward_the_cap() {
    // An implementation with the two constants swapped (initial 200ms / cap 5ms) sleeps 5ms
    // forever after the first interval because of the `min`, and timing tests do not catch
    // it.
    assert!(
        LOCK_RETRY_INITIAL < LOCK_RETRY_MAX,
        "the initial interval is larger than the cap"
    );
    assert!(
        LOCK_RETRY_MAX <= LOCK_DEADLINE,
        "the cap is larger than the deadline"
    );
}

#[test]
fn an_unopenable_lock_path_is_reported_as_io_without_waiting() {
    // If there is a directory at the lock path, open fails with EISDIR. This is not
    // contention, so it must not wait until the deadline.
    let path = temp_lock_path("eisdir");
    std::fs::create_dir(&path).expect("occupy the lock path with a dir");

    let started = Instant::now();
    let outcome = acquire(&path, LOCK_DEADLINE);
    assert!(matches!(outcome, Err(LockError::Io(_))), "{outcome:?}");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "waited until the deadline although the path cannot be opened: {:?}",
        started.elapsed()
    );
}

#[test]
fn acquire_creates_a_missing_lock_directory_owner_only() {
    // `create_dir_all` applies the umask to the mode and creates 0755 under umask 022. The
    // lock file is in the **same directory** as the tokens, so if the path that takes the
    // lock first creates the directory as 0755, every later `TokenStore::store` fails
    // permanently with `BadDirPermissions` (design §4 = 0700).
    // The guard is **held by a name.** If it is chained directly, as in
    // `temp_lock_path("mkdir-mode").parent()`, the `TempLock` is dropped at the end of that
    // statement and the directory disappears, and then `acquire` creates the same tree again,
    // which stays behind as **leftovers nobody deletes** (measured: exactly 1 leak per run of
    // the suite — it was this one test).
    let temp = temp_lock_path("mkdir-mode");
    let root = temp.parent().expect("parent").to_path_buf();
    let deep = root.join("nested").join("deep");
    let guard = acquire(&deep.join("tokens.lock"), LOCK_DEADLINE).expect("must acquire");

    assert_owner_only(&root.join("nested"), "lock dir (ancestor)");
    assert_owner_only(&deep, "lock dir");
    drop(guard);
}

#[test]
fn the_lock_file_is_created_owner_only() {
    let path = temp_lock_path("filemode");
    let guard = acquire(&path, LOCK_DEADLINE).expect("must acquire");
    assert_owner_only(&path, "lock file");
    drop(guard);
}

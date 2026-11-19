use super::*;
use crate::clock::FixedClock;
use crate::test_support::{FAR_FUTURE, TempDir, seed_expired_token, temp_dir, write_token};
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Uses the very path the store actually uses — if a test spelled the file name out again, the
/// source of truth would split in two.
fn token_path(dir: &Path) -> PathBuf {
    TokenStore::new(dir.to_path_buf()).token_path()
}

fn lock_path(dir: &Path) -> PathBuf {
    TokenStore::new(dir.to_path_buf()).lock_path()
}

fn disk_json(dir: &Path) -> serde_json::Value {
    let raw = std::fs::read_to_string(token_path(dir)).expect("read token file");
    serde_json::from_str(&raw).expect("token file must be json")
}

/// Checks "nobody but the owner has any permission", independently of the umask.
#[track_caller]
fn assert_owner_only(path: &Path, what: &str) {
    let mode = std::fs::metadata(path)
        .expect("must exist")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode & 0o077,
        0,
        "{what}: permission bits beyond the owner are still set (mode {mode:o})"
    );
}

/// Is it **impossible** to create a file in this directory?
///
/// When the tests run as root, even a 0500 directory is writable, so a "persist failure" cannot
/// be staged. This checks only that fact, without touching any process-global state.
fn writes_are_denied(dir: &Path) -> bool {
    let probe = dir.join(".write-probe");
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            false
        }
        Err(_) => true,
    }
}

/// Waits, sleeping briefly, until `cond` becomes true (with an upper bound).
///
/// Contention tests use it to confirm "the winner is already inside the critical section" **as a
/// fact, not by timing**. Guessing the order with `sleep(n)` gets the order flipped on a loaded
/// machine, and then the test passes without the contention it meant to exercise ever happening.
/// The upper bound exists to turn a failure into a diagnosable panic instead of an endless hang,
/// so it is set generously.
fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let started = std::time::Instant::now();
    while !cond() {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "condition was never met: {what}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// A refresher that counts its calls. It is the main subject of the single-flight assertions.
struct CountingRefresher {
    calls: AtomicUsize,
    new_access: String,
    /// Round-trip delay. How long the critical section is held is exactly the size of the
    /// contention window.
    delay: Duration,
}

impl CountingRefresher {
    fn new(new_access: &str) -> Self {
        Self::with_delay(new_access, Duration::from_millis(50))
    }
    /// A variant that holds the critical section for a long time — used when testing the wait
    /// on the mutex and on the file lock.
    fn with_delay(new_access: &str, delay: Duration) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            new_access: new_access.to_owned(),
            delay,
        }
    }
    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl TokenRefresher for CountingRefresher {
    fn refresh(&self, _refresh_token: &str) -> Result<Refreshed, Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        // Imitates a real network delay to widen the window for concurrent entry.
        std::thread::sleep(self.delay);
        Ok(Refreshed::new(
            self.new_access.clone(),
            "NA_ROTATED".to_owned(),
            28_800,
        ))
    }
}

/// A refresher that always fails. It checks that a failure is not silently swallowed.
///
/// It counts calls the same way `CountingRefresher` does — because "can it be retried after a
/// failure" is half of the property this type is here to check.
struct FailingRefresher {
    calls: AtomicUsize,
}

impl FailingRefresher {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }
    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl TokenRefresher for FailingRefresher {
    fn refresh(&self, _refresh_token: &str) -> Result<Refreshed, Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(Error::Auth {
            message: "upstream refused".to_owned(),
            pricing: None,
        })
    }
}

/// A refresher that sets the expiry time as the server dictates — it tests lifetime values.
struct LifetimeRefresher {
    calls: AtomicUsize,
    expires_in_secs: u64,
}

impl LifetimeRefresher {
    fn new(expires_in_secs: u64) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            expires_in_secs,
        }
    }
    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl TokenRefresher for LifetimeRefresher {
    fn refresh(&self, _refresh_token: &str) -> Result<Refreshed, Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Refreshed {
            access_token: "NEW".to_owned(),
            refresh_token: "NA_ROTATED".to_owned(),
            expires_in_secs: self.expires_in_secs,
        })
    }
}

// --- Generation + minimum rotation interval (A3 Task 5) fixtures -------

/// The reference time used by the tests in this group. The value itself means nothing — it only
/// stays clear of the epoch so that `checked_sub` becomes `None` **only because the clock went
/// backwards**.
const T0: u64 = 1_700_000_000;

/// A manager for the generation and interval tests.
///
/// It assembles the A2 fixtures as they are: `temp_dir` (0700) + `seed_expired_token` (0600),
/// `LifetimeRefresher` (with a built-in call counter) and `FixedClock`. Nothing new is made here
/// — A2 already stepped on the umask trap.
///
/// `expires_in` is the lifetime the refresher will report. Passing 0 produces the "stale on
/// arrival" state A2 documented, where **every call tries to rotate** — exactly the right input
/// for testing the rotation interval gate.
fn interval_manager(
    tag: &str,
    expires_in: u64,
) -> (
    TokenManager,
    Arc<LifetimeRefresher>,
    Arc<FixedClock>,
    TempDir,
) {
    let dir = temp_dir(tag);
    seed_expired_token(&dir);
    let refresher = Arc::new(LifetimeRefresher::new(expires_in));
    let clock = Arc::new(FixedClock::new(T0));
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>)
        .with_clock(Arc::clone(&clock) as Arc<_>)
        // This group does not test lock contention. If the lock is left held for whatever
        // reason, fail fast instead of waiting 10 seconds, so that the misdiagnosis is exposed.
        .with_lock_deadline(Duration::from_secs(3));
    (mgr, refresher, clock, dir)
}

/// A lifetime long enough to outlive the rotation interval, yet short enough that one more
/// rotation is needed after it.
///
/// The gate opens only past `MIN_REFRESH_INTERVAL` (60 seconds), and the rotation path is reached
/// only if the disk token is stale after `advance`. 100 seconds satisfies both at once: `T0+100`
/// is fresh at `T0` but stale at `T0+61` (+ skew 60).
const SHORT_BUT_USABLE: u64 = 100;

/// A refresher with a slow round trip that also yields a token that is **stale on arrival**.
///
/// A2 has no such combination: `CountingRefresher` is slow but its lifetime is normal (28800
/// seconds), so the second call hits the cache and never reaches the gate; `LifetimeRefresher`
/// lets the lifetime be chosen but returns immediately, so it cannot hold the critical section
/// and cannot create contention. `..._not_a_file_lock_timeout` below needs **both**.
struct SlowStaleRefresher {
    calls: AtomicUsize,
    delay: Duration,
}

impl SlowStaleRefresher {
    fn new(delay: Duration) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            delay,
        }
    }
    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl TokenRefresher for SlowStaleRefresher {
    fn refresh(&self, _refresh_token: &str) -> Result<Refreshed, Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(self.delay);
        // Lifetime 0 — already stale the moment it is recorded, so the next call tries to
        // rotate.
        Ok(Refreshed::new("NEW".to_owned(), "NA_ROTATED".to_owned(), 0))
    }
}

// --- Generation + minimum rotation interval (A3 Task 5) ----------------

#[test]
fn the_min_refresh_interval_is_the_expiry_skew_and_is_sixty_seconds() {
    // The sentence the design closes on is "a lifetime that cannot be considered fresh cannot be
    // rotated either". If the two constants diverge, that sentence becomes false — instead of
    // tying them together structurally (a public constant silently following a crate-internal
    // one would be worse), the equality is pinned here so that moving either one is a conscious
    // decision.
    assert_eq!(MIN_REFRESH_INTERVAL, EXPIRY_SKEW);
    // Checking the equality alone would still pass if both were changed to 0. Pin the value too.
    assert_eq!(MIN_REFRESH_INTERVAL, Duration::from_secs(60));
}

#[test]
fn refresh_within_min_interval_errors_and_sends_nothing() {
    // The hole A2 handed to A3 ("There is no lower bound on the other side" in the
    // `MAX_TOKEN_LIFETIME` doc): if the server gives `expires_in = 0`, the new token is stale on
    // arrival and **every call** rotates the chain. That storm is stopped here.
    let (mgr, refresher, _clock, _dir) = interval_manager("gen-interval", 0);

    assert_eq!(mgr.access_token().expect("first"), "NEW");
    let after_first = refresher.call_count();
    assert_eq!(after_first, 1, "the first call should rotate normally");

    let err = mgr
        .access_token()
        .expect_err("inside the interval it should be an error, not the cache");

    // "It returned an error" is not enough — an implementation that sent the POST and then
    // returned an error looks the same. Count the mechanism.
    assert_eq!(
        refresher.call_count(),
        after_first,
        "one more refresh POST was fired inside the interval"
    );
    let msg = err.to_string();
    assert!(msg.contains("refresh"), "should name the cause: {msg}");
    assert!(
        msg.contains("rotation interval"),
        "a failure for another reason (lock, persist, ...) is mistaken for the interval: {msg}"
    );
}

#[test]
fn min_interval_error_is_not_billed() {
    // No endpoint was reached. Building it with `pricing: Some(..)` would make `billing()` count
    // this failure as spend, silently inflating the cost accounting.
    let (mgr, _refresher, _clock, _dir) = interval_manager("gen-notbilled", 0);
    let _first = mgr.access_token().expect("first");
    let err = mgr
        .access_token()
        .expect_err("should be blocked by the interval");

    assert!(
        matches!(&err, Error::Auth { pricing: None, .. }),
        "actual: {err:?}"
    );
    assert_eq!(err.billing(), crate::pricing::Billing::NotBilled);
}

#[test]
fn clock_going_backwards_counts_as_interval_not_met() {
    // When the clock goes backwards, writing `now - last` as a plain subtraction panics in debug
    // and in release wraps around to a huge value that **defeats the interval**. Folding the
    // `None` of `checked_sub` into "interval not met" is the conservative direction.
    let (mgr, refresher, clock, _dir) = interval_manager("gen-backwards", 0);
    assert_eq!(mgr.access_token().expect("first"), "NEW");
    let after_first = refresher.call_count();

    // Why the step back is 30 seconds (to avoid a vacuous pass). The disk token expires at
    // `T0 + 0` and the freshness check is `expires_at > now + 60`. Rewinding one hour makes
    // `T0 > T0 - 3600 + 60` **true**, so that token becomes "fresh" again and the call succeeds
    // through disk adoption before it even reaches the gate — with no rotation the counter
    // assertion passes, but not one line of the code under test runs. At 30 seconds (< skew 60)
    // the disk is still stale, so the gate is the only line of defence left.
    clock.set(T0 - 30);

    let err = mgr
        .access_token()
        .expect_err("a clock going backwards should count as interval not met");
    assert!(
        err.to_string().contains("rotation interval"),
        "the backwards clock was caught somewhere other than the gate (the test is vacuous): {err}"
    );
    assert_eq!(
        refresher.call_count(),
        after_first,
        "a clock going backwards defeated the rotation suppression"
    );
}

#[test]
fn the_minimum_interval_boundary_admits_exactly_sixty_seconds() {
    // The boundary mutation (`<` → `<=`) passes every test above — because it blocks at 59
    // seconds and at 60 seconds alike. Look at **both sides** of the boundary in the same test.
    let (mgr, refresher, clock, _dir) = interval_manager("gen-boundary", SHORT_BUT_USABLE);
    let (g1, _tok) = mgr.access_token_with_generation().expect("first");
    assert_eq!(refresher.call_count(), 1);

    // Empty the cache to send the call down the rotation path (otherwise it is a cache hit and
    // never reaches the gate).
    mgr.invalidate_generation(g1).expect("invalidate");

    // 59 seconds — still blocked.
    clock.advance(MIN_REFRESH_INTERVAL.as_secs() - 1);
    let err = mgr
        .access_token()
        .expect_err("59 seconds should be blocked");
    assert!(err.to_string().contains("rotation interval"), "{err}");
    assert_eq!(refresher.call_count(), 1, "rotated at 59 seconds");

    // Exactly 60 seconds — it opens. If `<` is changed to `<=`, this is where it dies.
    clock.advance(1);
    assert_eq!(
        mgr.access_token()
            .expect("exactly 60 seconds should be allowed"),
        "NEW"
    );
    assert_eq!(
        refresher.call_count(),
        2,
        "the interval was met but no rotation happened — the gate is off by one at the boundary"
    );
}

#[test]
fn generation_increases_on_each_refresh() {
    let (mgr, refresher, clock, _dir) = interval_manager("gen-increases", SHORT_BUT_USABLE);
    let (g1, _t1) = mgr.access_token_with_generation().expect("first");
    assert_eq!(refresher.call_count(), 1);

    mgr.invalidate_generation(g1).expect("invalidate");
    clock.advance(MIN_REFRESH_INTERVAL.as_secs() + 1);

    let (g2, _t2) = mgr.access_token_with_generation().expect("second");
    // Look at the mechanism as well — the generation alone cannot be told apart from "it rose
    // through adoption".
    assert_eq!(
        refresher.call_count(),
        2,
        "if the second call was not a rotation, this test does not live up to its name"
    );
    assert!(g2 > g1, "g1={g1} g2={g2}");
}

#[test]
fn generation_increases_when_a_siblings_rotation_is_adopted() {
    // Adoption **replaces** the cache too. If the generation is not bumped here, a 401 arriving
    // late, right after the adoption, matches the generation and discards the perfectly good
    // token that was just adopted — the accident the generation was meant to prevent would be
    // left in place on the adoption path alone.
    let (mgr, refresher, _clock, dir) = interval_manager("gen-adopt", SHORT_BUT_USABLE);
    let (g1, t1) = mgr.access_token_with_generation().expect("first");
    assert_eq!(refresher.call_count(), 1);

    mgr.invalidate_generation(g1).expect("invalidate");
    // Set up what a sibling process's rotation would leave behind — our refresher is not used.
    write_token(&dir, "FROM_SIBLING", "NA_SIBLING", FAR_FUTURE);

    let (g2, t2) = mgr.access_token_with_generation().expect("adopt");
    assert_eq!(t2, "FROM_SIBLING", "the sibling's token was not adopted");
    assert_ne!(t2, t1);
    assert_eq!(
        refresher.call_count(),
        1,
        "a rotation POST was fired on the adoption path"
    );
    assert!(
        g2 > g1,
        "the cache was replaced but the generation did not change — a stale 401 can kill this \
         token (g1={g1} g2={g2})"
    );
}

#[test]
fn stale_generation_cannot_invalidate_newer_token() {
    // Without this comparison, concurrent 401s kill a token that was just refreshed: the 401 for
    // a request A sent with generation N makes it discard the N+1 that B has already produced.
    let (mgr, refresher, clock, _dir) = interval_manager("gen-stale", SHORT_BUT_USABLE);
    let (g1, _t1) = mgr.access_token_with_generation().expect("first");
    mgr.invalidate_generation(g1).expect("invalidate g1");
    clock.advance(MIN_REFRESH_INTERVAL.as_secs() + 1);

    let (g2, t2) = mgr.access_token_with_generation().expect("second");
    let calls_after_g2 = refresher.call_count();
    let locks_after_g2 = mgr.lock_attempts();
    assert_eq!(calls_after_g2, 2);

    // A late-arriving 401 attempts an invalidation with the **stale** generation.
    mgr.invalidate_generation(g1)
        .expect("invalidating a stale generation is a no-op, not an error");

    let (g3, t3) = mgr.access_token_with_generation().expect("third");
    assert_eq!(
        g3, g2,
        "an invalidation with a stale generation took effect"
    );
    assert_eq!(t3, t2, "a perfectly good token was discarded");
    assert_eq!(
        refresher.call_count(),
        calls_after_g2,
        "the invalidation took effect and one more rotation happened"
    );
    // There is a mutation the generation assertions alone do not catch (unconditional
    // invalidation + the adoption path). Look at the mechanism as well — had the cache been
    // alive, there would be no reason to go to the file lock.
    assert_eq!(
        mgr.lock_attempts(),
        locks_after_g2,
        "the cache was emptied and the third call went as far as the file lock"
    );
}

#[test]
fn a_blocked_second_caller_gets_the_interval_error_not_a_file_lock_timeout() {
    // **What this test is for** (review fix round).
    //
    // It was written that the borrow checker rules out an implementation that "reads the
    // generation after releasing the lock", but that is true only of the **naive form** (reusing
    // a guard that was dropped, E0382). The form that locks again compiles just fine:
    //
    // ```ignore
    // drop(state);
    // let g = self.state.lock().generation;
    // Ok((g, token))
    // ```
    //
    // This form not only reads the generation **outside** the critical section, it also releases
    // the state mutex **before the flock**, inverting the lock order. This is the point where
    // that inversion becomes observable:
    //
    //   - Correct implementation: the winner releases the mutex and the flock **together**. The
    //     loser obtains the mutex and then the flock right away, and since the disk is stale it
    //     is blocked by the step 8 gate and fails **immediately** with `rotation interval`.
    //   - Re-lock mutation: the winner releases only the mutex while still holding the flock.
    //     The loser obtains the mutex and goes for the flock, and the winner waits for that
    //     mutex — they wait on each other until the loser uses up `lock_deadline` and fails with
    //     `token lock:`.
    //
    // So the **kind of error** separates the mutation. The wall clock is not asserted.
    let dir = temp_dir("relock-order");
    seed_expired_token(&dir);
    let refresher = Arc::new(SlowStaleRefresher::new(Duration::from_millis(300)));
    let mgr = Arc::new(
        TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>)
            .with_clock(Arc::new(FixedClock::new(T0)))
            // Shortened only so that, when the mutation is in place, the test does not really
            // wait 10 seconds. It has to be longer than the winner's round trip (300ms), or
            // the correct implementation would trip over this value.
            .with_lock_deadline(Duration::from_millis(800)),
    );

    let winner = {
        let m = Arc::clone(&mgr);
        std::thread::spawn(move || m.access_token())
    };
    // Start the loser only after confirming **as a fact** that the winner has entered the
    // critical section. Guessing the order with a sleep gets it flipped on a loaded machine (the
    // same reason as in A2).
    wait_until("the winner has entered refresh", || {
        refresher.call_count() >= 1
    });
    let loser = {
        let m = Arc::clone(&mgr);
        std::thread::spawn(move || m.access_token())
    };

    assert_eq!(winner.join().expect("join").expect("winner"), "NEW");
    let err = loser
        .join()
        .expect("join")
        .expect_err("the loser should be blocked by the interval");
    let msg = err.to_string();
    assert!(
        msg.contains("rotation interval"),
        "failed for a reason other than the interval: {msg}"
    );
    assert!(
        !msg.contains("token lock:"),
        "the loser starved on the file lock — this means the winner released the state mutex \
         before the flock, which is a side effect of an implementation that reads the \
         generation outside the critical section: {msg}"
    );
    assert_eq!(
        refresher.call_count(),
        1,
        "a second rotation was fired inside the interval"
    );
}

#[test]
fn invalidating_the_current_generation_clears_the_cache() {
    // The other side. Rules out an implementation that **does nothing** when the generation
    // matches (= 401 recovery keeps getting the stale token back forever).
    let (mgr, refresher, clock, _dir) = interval_manager("gen-clears", SHORT_BUT_USABLE);
    let (g1, _t1) = mgr.access_token_with_generation().expect("first");
    let locks_after_g1 = mgr.lock_attempts();

    // The cache is still fresh, so without the invalidation the next call would be a cache hit.
    mgr.invalidate_generation(g1).expect("invalidate");
    clock.advance(MIN_REFRESH_INTERVAL.as_secs() + 1);

    let (g2, _t2) = mgr.access_token_with_generation().expect("second");
    assert!(g2 > g1, "the cache was not emptied (g1={g1} g2={g2})");
    assert_eq!(refresher.call_count(), 2);
    assert!(
        mgr.lock_attempts() > locks_after_g1,
        "the cache was left as is, so the file lock was never tried — the invalidation is a no-op"
    );
}

// --- 401 rejection (reject_generation) — A3 final fix wave -------------

/// A refresher that hands out a **different** token on every rotation.
///
/// `LifetimeRefresher`/`CountingRefresher` always return the same value (`"NEW"`), so they
/// cannot tell whether "a genuinely different token came out after the rejection" — because an
/// implementation whose rejection was defeated returns the same string too.
struct RotatingRefresher {
    calls: AtomicUsize,
}

impl RotatingRefresher {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }
    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl TokenRefresher for RotatingRefresher {
    fn refresh(&self, _refresh_token: &str) -> Result<Refreshed, Error> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(Refreshed {
            access_token: format!("ROT{n}"),
            refresh_token: format!("NA_ROT{n}"),
            // A generous lifetime — this group tests **rejection**, not expiry. With a
            // short lifetime it could not be told whether a rotation happened because of
            // expiry or because of the rejection.
            expires_in_secs: 28_800,
        })
    }
}

fn rotating_manager(
    tag: &str,
) -> (
    TokenManager,
    Arc<RotatingRefresher>,
    Arc<FixedClock>,
    TempDir,
) {
    let dir = temp_dir(tag);
    seed_expired_token(&dir);
    let refresher = Arc::new(RotatingRefresher::new());
    let clock = Arc::new(FixedClock::new(T0));
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>)
        .with_clock(Arc::clone(&clock) as Arc<_>)
        .with_lock_deadline(Duration::from_secs(3));
    (mgr, refresher, clock, dir)
}

/// A rejection forces a rotation **past `is_fresh`** — exactly once.
///
/// What sets this test apart from `invalidating_the_current_generation_clears_the_cache` is the
/// reason this group exists. Invalidation only empties the cache, so in this situation, where
/// the cache and the disk are **both fresh**, the next call adopts the disk as is — that is, it
/// hands out again the very bytes the server just rejected. A 401 does not move `expires_at`, so
/// an implementation that looks only at freshness never recovers.
///
/// So this test checks together (1) that the rotation counter actually went up, (2) that the
/// token that came out is a **different value**, and (3) that the call after that is a cache hit
/// again (= the forcing stops after one rotation). The mutation that reverts `reject_generation`
/// to `invalidate_generation` dies at (1)(2), and the mutation that "reads the rejection mark as
/// a permanent force" dies at (3).
#[test]
fn rejecting_the_current_generation_forces_exactly_one_rotation_past_a_fresh_token() {
    let (mgr, refresher, clock, _dir) = rotating_manager("gen-reject-forces");
    let (g1, t1) = mgr.access_token_with_generation().expect("first");
    assert_eq!(refresher.call_count(), 1);
    assert_eq!(t1, "ROT1");
    // Premise: both the cache and the disk are fresh. In this state invalidation alone does not
    // cause a rotation.

    mgr.reject_generation(g1).expect("reject");
    // Open the rotation suppression gate — the gate itself is covered by
    // `a_rejection_does_not_bypass_the_minimum_rotation_interval`.
    clock.advance(MIN_REFRESH_INTERVAL.as_secs() + 1);

    let (g2, t2) = mgr.access_token_with_generation().expect("second");
    assert_eq!(
        refresher.call_count(),
        2,
        "the rejection was stopped by the freshness check and forced no rotation — this means \
         the retry resends, unchanged, the token that was just rejected"
    );
    assert_ne!(t2, t1, "the same token came out again");
    assert!(g2 > g1, "g1={g1} g2={g2}");

    // The forcing happens **once**. The third call must be a cache hit — checked through the
    // mechanism (the number of file lock attempts). The value alone cannot be told apart from
    // "rotated but got the same value".
    let locks_after_g2 = mgr.lock_attempts();
    clock.advance(MIN_REFRESH_INTERVAL.as_secs() + 1);
    let (g3, t3) = mgr.access_token_with_generation().expect("third");
    assert_eq!(
        refresher.call_count(),
        2,
        "the rejection mark lingered and fired another rotation — the forcing must happen once"
    );
    assert_eq!((g3, t3.as_str()), (g2, t2.as_str()));
    assert_eq!(
        mgr.lock_attempts(),
        locks_after_g2,
        "it was not a cache hit; it went as far as the file lock"
    );
}

/// Even after a rejection, adopting a sibling process's **different** token still works.
///
/// Implementing rejection as "skip disk adoption wholesale" kills this path — we would rotate
/// again (= risk of chain invalidation + a needless POST) even though a perfectly good token the
/// sibling already rotated in is sitting there. It must block **only when the value is the
/// same**.
#[test]
fn a_rejected_token_still_yields_to_a_siblings_newer_token() {
    let (mgr, refresher, _clock, dir) = rotating_manager("gen-reject-adopt");
    let (g1, t1) = mgr.access_token_with_generation().expect("first");
    assert_eq!(refresher.call_count(), 1);

    mgr.reject_generation(g1).expect("reject");
    // What a sibling process's rotation leaves behind. The value is **different** from ours.
    write_token(&dir, "FROM_SIBLING", "NA_SIBLING", FAR_FUTURE);

    let (g2, t2) = mgr.access_token_with_generation().expect("adopt");
    assert_eq!(
        t2, "FROM_SIBLING",
        "the sibling's new token was not adopted"
    );
    assert_ne!(t2, t1);
    assert_eq!(
        refresher.call_count(),
        1,
        "a rotation POST was fired although a different, adoptable token was there"
    );
    assert!(
        g2 > g1,
        "the cache was replaced but the generation did not change (g1={g1} g2={g2})"
    );
}

/// A rejection with a stale generation is a no-op — it does not kill a token that was just
/// refreshed.
///
/// It prevents, on the rejection side, the same accident as `invalidate_generation`'s
/// `stale_generation_cannot_invalidate_newer_token`. If rejection skipped the generation
/// comparison, a late-arriving 401 would put a perfectly good token a sibling just created on
/// the blacklist and kill, in a chain, the requests using that token as well.
#[test]
fn a_stale_generation_cannot_reject_a_newer_token() {
    let (mgr, refresher, clock, _dir) = rotating_manager("gen-reject-stale");
    let (g1, _t1) = mgr.access_token_with_generation().expect("first");
    mgr.reject_generation(g1).expect("reject g1");
    clock.advance(MIN_REFRESH_INTERVAL.as_secs() + 1);
    let (g2, t2) = mgr.access_token_with_generation().expect("second");
    assert_eq!(refresher.call_count(), 2);
    let locks_after_g2 = mgr.lock_attempts();

    // A late-arriving 401 attempts a rejection with the **stale** generation.
    mgr.reject_generation(g1)
        .expect("rejecting a stale generation is a no-op, not an error");

    clock.advance(MIN_REFRESH_INTERVAL.as_secs() + 1);
    let (g3, t3) = mgr.access_token_with_generation().expect("third");
    assert_eq!(g3, g2, "a rejection with a stale generation took effect");
    assert_eq!(t3, t2, "a perfectly good token was discarded");
    assert_eq!(
        refresher.call_count(),
        2,
        "the stale rejection fired one more rotation"
    );
    assert_eq!(
        mgr.lock_attempts(),
        locks_after_g2,
        "the cache was discarded and the file lock was tried — the stale rejection was not a no-op"
    );
}

/// Bytes that were rejected once never go out again, **even if they later come back to life on
/// disk**.
///
/// This pins the decision that [`State::rejected_access_token`] is **not** cleared after a
/// successful rotation. Mutation M7 (clearing the mark on a successful rotation) died nowhere
/// but here — because in every other test the new token right after a rotation has a different
/// value anyway and is not caught by the gate. So an input that fails only when the mark has
/// been cleared is built separately: the situation where the dead token **shows up on disk
/// again** (a restore from backup, or a write-back by a sibling process that was holding an old
/// file).
#[test]
fn a_rejected_token_is_never_adopted_again_even_after_a_later_rotation() {
    let (mgr, refresher, clock, dir) = rotating_manager("gen-reject-sticky");
    let (g1, t1) = mgr.access_token_with_generation().expect("first");
    mgr.reject_generation(g1).expect("reject");
    clock.advance(MIN_REFRESH_INTERVAL.as_secs() + 1);
    let (g2, t2) = mgr.access_token_with_generation().expect("rotated");
    assert_eq!(refresher.call_count(), 2);
    assert_ne!(t2, t1);

    // The dead token comes back to life on disk — and it is fresh.
    write_token(&dir, &t1, "NA_RESURRECTED", FAR_FUTURE);
    // Empty the cache to send the call down the disk path (a cache hit never looks at the disk).
    mgr.invalidate_generation(g2).expect("invalidate");
    clock.advance(MIN_REFRESH_INTERVAL.as_secs() + 1);

    let (_g3, t3) = mgr.access_token_with_generation().expect("third");
    assert_ne!(
        t3, t1,
        "a token the server already rejected was revived from disk and handed out"
    );
    assert_eq!(
        refresher.call_count(),
        3,
        "it should have rotated instead of adopting the dead token"
    );
}

/// A rejection **does not bypass** the rotation suppression gate.
///
/// All that is forced is "it cannot escape through the cache or adoption"; the actual rotation
/// still has to pass `MIN_REFRESH_INTERVAL`. Implementing rejection so that it skips the gate
/// opens a rotation storm for as long as 401s keep arriving in a row.
///
/// What the second half pins as well: even when blocked by the interval, **the rejection mark
/// remains**. In an implementation that clears the mark, the call after the interval has passed
/// adopts the disk and goes back to the dead token.
#[test]
fn a_rejection_does_not_bypass_the_minimum_rotation_interval() {
    let (mgr, refresher, clock, _dir) = rotating_manager("gen-reject-interval");
    let (g1, t1) = mgr.access_token_with_generation().expect("first");
    assert_eq!(refresher.call_count(), 1);

    // Right after a rotation — the interval has not been met yet.
    mgr.reject_generation(g1).expect("reject");
    let err = mgr
        .access_token()
        .expect_err("a forced rotation inside the interval should be refused");
    assert!(
        err.to_string().contains("rotation interval"),
        "failed for a reason other than the gate: {err}"
    );
    assert_eq!(
        refresher.call_count(),
        1,
        "the rejection bypassed the rotation suppression gate"
    );

    // Once the interval has passed, it rotates — the rejection mark must still be alive.
    clock.advance(MIN_REFRESH_INTERVAL.as_secs() + 1);
    let (_g2, t2) = mgr.access_token_with_generation().expect("second");
    assert_eq!(
        refresher.call_count(),
        2,
        "the interval was met but it did not rotate"
    );
    assert_ne!(
        t2, t1,
        "the rejection mark was cleared and it went back to the dead token"
    );
}

// --- single-flight ---------------------------------------------------

#[test]
fn concurrent_callers_collapse_into_exactly_one_refresh() {
    let dir = temp_dir("singleflight");
    seed_expired_token(&dir);
    // Stretch the refresh by 100ms to make concurrent entry certain. Waiters must be released by
    // a wake-up; if they sleep until the deadline (20 seconds), the elapsed-time assertion below
    // (10 seconds) fails (final review I-1). The deadline and the threshold are kept far apart
    // because the whole test suite running together once took 2.27 seconds under load
    // (2026-10-14; had the wake-up been missed, they would have slept until the deadline).
    let refresher = Arc::new(CountingRefresher::with_delay(
        "NEW",
        Duration::from_millis(100),
    ));
    let mgr = Arc::new(
        TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>)
            .with_state_deadline(Duration::from_secs(20)),
    );

    let started = std::time::Instant::now();
    let mut handles = Vec::new();
    for _ in 0..16 {
        let m = Arc::clone(&mgr);
        handles.push(std::thread::spawn(move || m.access_token()));
    }
    for h in handles {
        assert_eq!(h.join().expect("join").expect("access_token"), "NEW");
    }
    assert_eq!(
        refresher.call_count(),
        1,
        "the refreshes did not collapse into a single flight"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "waiters slept until the state deadline instead of being woken: {:?}",
        started.elapsed()
    );
}

#[test]
fn concurrent_callers_are_served_from_memory_rather_than_each_taking_the_file_lock() {
    // The test above alone cannot tell **which mechanism** did the collapsing. The cross-process
    // file lock by itself also collapses concurrent callers in the same process (a late caller
    // waits for the lock and then adopts from disk). So a mutation that releases the state mutex
    // across the refresh passes the test above unchanged.
    //
    // Setting the file lock deadline to 0 exposes the difference. With the in-process
    // single-flight REQ-15 calls for, a late caller gets its answer **from memory** and never
    // touches the file lock at all. Without it, 15 callers try the lock at once and all of them
    // get Timeout.
    //
    // The hole the Task 7 review caught (the M2 misclassification): looking only at the return
    // value and the call count, **an implementation with the cache check deleted passes this
    // test too.** By the time a late caller arrives the winner has already released `_guard`, so
    // the lock is obtained even with deadline 0. The same value was obtained through a different
    // mechanism, which makes the test name a lie. So the mechanism itself is counted — the
    // number of attempts to acquire the lock must be **exactly 1** (= the winner only).
    //
    // The in-process mutex deadline is not what this test is about, so it is set generously (60
    // seconds). With the default 10 seconds, under the load of the full `verify.sh` test run,
    // the winner's refresh and persist (fsync) took more than 10 seconds and one late caller
    // failed with `token state lock:` (2026-10-20, crate tests 15.18 seconds, 20/20 passes when
    // run alone). A defect where the flag is never cleared is still caught after 60 seconds.
    let dir = temp_dir("memoryserved");
    seed_expired_token(&dir);
    let refresher = Arc::new(CountingRefresher::new("NEW"));
    let mgr = Arc::new(
        TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>)
            .with_lock_deadline(Duration::ZERO)
            .with_state_deadline(Duration::from_secs(60)),
    );

    let mut handles = Vec::new();
    for _ in 0..16 {
        let m = Arc::clone(&mgr);
        handles.push(std::thread::spawn(move || m.access_token()));
    }
    for h in handles {
        assert_eq!(
            h.join()
                .expect("join")
                .expect("every caller should get its answer from memory"),
            "NEW"
        );
    }
    assert_eq!(refresher.call_count(), 1);
    assert_eq!(
        mgr.lock_attempts(),
        1,
        "late callers got their answer through the file lock, not from memory"
    );
}

#[test]
fn two_managers_sharing_a_directory_collapse_into_one_refresh() {
    // The **cross-process** half of REQ-15 AC2. Every concurrency test up to here used **one**
    // manager, and then the state mutex serializes everything before the file lock is ever
    // contended — the flock path "re-read the disk under the lock → adopt if a sibling
    // refreshed" was never executed. The only contention test
    // (`a_lock_timeout_never_refreshes`) holds the lock with a bare `lock::acquire`, so the
    // loser **times out** — the exact opposite of the adoption path.
    //
    // So without this test the following two mutations pass all 222 tests.
    //   (a) Swap steps 5 and 6 (read the disk **before** the lock). Under real contention the
    //       loser reads the **stale** disk, waits for the lock, and then refreshes again with a
    //       refresh token that has already been rotated — the very double refresh Tesla answers
    //       with invalidation of the whole chain.
    //   (b) `drop(_guard)` right after re-reading the disk. Same result.
    //
    // The design supplies the tool: a flock is per **open file description**, so two
    // `TokenManager`s in one process are, for this purpose, two sibling processes.
    let dir = temp_dir("twomanagers");
    seed_expired_token(&dir);
    // The refresher is **shared** — seeing whether they collapsed needs the total.
    let refresher = Arc::new(CountingRefresher::new("NEW"));
    let winner = Arc::new(TokenManager::new(
        dir.to_path_buf(),
        Arc::clone(&refresher) as Arc<_>,
    ));
    let loser = Arc::new(TokenManager::new(
        dir.to_path_buf(),
        Arc::clone(&refresher) as Arc<_>,
    ));

    let w = Arc::clone(&winner);
    let first = std::thread::spawn(move || w.access_token());

    // Start the loser only after confirming that the winner has entered refresh **while holding
    // the file lock**. Simply launching both at once leaves it unknown which one takes the lock
    // first, and mixes in orderings under which the mutations above could pass.
    wait_until("the winner has entered refresh", || {
        refresher.call_count() >= 1
    });

    let l = Arc::clone(&loser);
    let second = std::thread::spawn(move || l.access_token());

    assert_eq!(first.join().expect("join").expect("winner"), "NEW");
    assert_eq!(
        second.join().expect("join").expect("loser"),
        "NEW",
        "the loser did not get the sibling's new token"
    );

    assert_eq!(
        refresher.call_count(),
        1,
        "the two managers each rotated — the cross-process single-flight is broken, and the \
         second rotation is reuse of an already invalidated chain"
    );
    // Pin that the loser "went to the lock and adopted", not "answered from the cache". The
    // loser is a manager that was just created, so its cache is empty; had it not gone to the
    // lock, it would have had no way to get an answer.
    assert_eq!(
        winner.lock_attempts(),
        1,
        "the winner did not take the lock"
    );
    assert_eq!(
        loser.lock_attempts(),
        1,
        "the loser never went for the file lock"
    );
    // The disk reflects exactly one rotation.
    let v = disk_json(&dir);
    assert_eq!(v["access_token"], serde_json::json!("NEW"));
    assert_eq!(v["refresh_token"], serde_json::json!("NA_ROTATED"));
}

#[test]
fn a_contended_in_process_mutex_times_out_with_a_message_distinct_from_the_file_lock() {
    // REQ-15 AC3: "acquiring the mutex and the flock has a finite timeout or a deadline over the
    // whole operation". The flock side had a 10-second deadline but the mutex side had none —
    // the wait was (queue depth) × (a refresh that is itself unbounded), so neither disjunct
    // held.
    //
    // **A deadline over the whole operation is deliberately not provided.** It would mean the
    // refresh POST could be cancelled midway, and at that moment the server has committed the
    // rotation while we fail to persist the new refresh token. The mutex wait comes before any
    // POST, so giving up loses no state — that is why the boundary is drawn here.
    let dir = temp_dir("statedeadline");
    seed_expired_token(&dir);
    // Hold the critical section (= the refresh call) for a long time.
    let refresher = Arc::new(CountingRefresher::with_delay("NEW", Duration::from_secs(1)));
    let mgr = Arc::new(
        TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>)
            .with_state_deadline(Duration::from_millis(50)),
    );

    let holder = {
        let m = Arc::clone(&mgr);
        std::thread::spawn(move || m.access_token())
    };
    wait_until("the holder has entered refresh", || {
        refresher.call_count() >= 1
    });

    let err = mgr
        .access_token()
        .expect_err("the mutex wait must not be unbounded");
    let msg = err.to_string();
    assert!(msg.contains("token state lock"), "{msg}");
    // The two waits must be separable by the error text alone — a bug report usually carries
    // only this one line.
    assert!(
        !msg.contains("token lock:"),
        "the in-process mutex wait arrives with the same wording as the file lock wait: {msg}"
    );
    assert_eq!(
        refresher.call_count(),
        1,
        "refreshed without having obtained the mutex"
    );

    // The holder finishes undisturbed — the timeout does not touch the rotation in progress.
    assert_eq!(holder.join().expect("join").expect("holder"), "NEW");
}

// --- REQ-18: Debug of Refreshed --------------------------------------

#[test]
fn refreshed_debug_never_prints_token_material() {
    // C1. This type is re-exported from the crate root, carries **both** the access token and
    // the refresh token, and is created right after the transport layer (A3) parses the refresh
    // response — with `#[derive(Debug)]`, one `tracing::debug!(?refreshed)` line there would
    // stamp a live refresh token into the logs. The same treatment `TokenSet` got
    // (destructuring + REDACTED) is pinned here as well.
    let r = Refreshed::new(
        "AT_LEAKED_ACCESS".to_owned(),
        "NA_LEAKED_REFRESH".to_owned(),
        28_800,
    );
    let rendered = format!("{r:?}");
    assert!(
        !rendered.contains("AT_LEAKED_ACCESS"),
        "access token leaked: {rendered}"
    );
    assert!(
        !rendered.contains("NA_LEAKED_REFRESH"),
        "refresh token leaked: {rendered}"
    );
    // The marker must be the constant of the crate's single redaction layer — spelling the
    // string out again here would split the source of truth in two (the same reason as for
    // `TokenSet`).
    assert!(
        rendered.contains(crate::redaction::REDACTED),
        "marker diverged from redaction::REDACTED: {rendered}"
    );
    // The other side: also rule out an implementation that redacts everything and makes
    // diagnosis impossible. The lifetime is not a secret, and it is the only value in this type
    // one actually wants to see.
    assert!(
        rendered.contains("28800"),
        "even expires_in_secs was redacted: {rendered}"
    );
}

#[test]
fn refreshed_new_assigns_the_fields_in_the_documented_order() {
    // A constructor with the two arguments swapped passes every Debug test (both are redacted).
    // And that mistake stores the access token in the refresh token's place and loses the chain.
    let r = Refreshed::new("ACCESS".to_owned(), "REFRESH".to_owned(), 7);
    assert_eq!(r.access_token, "ACCESS");
    assert_eq!(r.refresh_token, "REFRESH");
    assert_eq!(r.expires_in_secs, 7);
}

#[test]
fn a_fresh_token_on_disk_is_used_without_calling_refresh() {
    let dir = temp_dir("nofresh");
    write_token(&dir, "STILLGOOD", "NA_X", FAR_FUTURE);

    let refresher = Arc::new(CountingRefresher::new("UNUSED"));
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>);
    assert_eq!(mgr.access_token().expect("token"), "STILLGOOD");
    assert_eq!(
        refresher.call_count(),
        0,
        "refresh was called although the token is valid"
    );
}

#[test]
fn a_cached_fresh_token_is_returned_without_touching_the_file_lock() {
    // An implementation with the cache check deleted passes both tests above — the only
    // difference is "does it go for the file lock". Holding the lock from outside makes that
    // difference visible.
    let dir = temp_dir("fastpath");
    write_token(&dir, "STILLGOOD", "NA_X", FAR_FUTURE);
    let refresher = Arc::new(CountingRefresher::new("UNUSED"));
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>)
        .with_lock_deadline(Duration::from_secs(3));

    // The first call takes the lock and adopts from disk (the cache is empty).
    assert_eq!(mgr.access_token().expect("first"), "STILLGOOD");
    assert_eq!(
        mgr.lock_attempts(),
        1,
        "the first call should take the lock"
    );

    let held = lock::acquire(&lock_path(&dir), lock::LOCK_DEADLINE).expect("hold the lock");
    // An implementation with the cache check deleted waits here until the deadline (3 seconds)
    // and then fails with Timeout, so the `expect` catches it. The wall-clock delay is not
    // asserted — the error kind alone already kills the mutation, and asserting time would only
    // add a reason to flake on a loaded machine. Instead look at **deterministic** evidence:
    // the number of attempts to acquire the lock did not grow.
    assert_eq!(mgr.access_token().expect("cached"), "STILLGOOD");
    assert_eq!(
        mgr.lock_attempts(),
        1,
        "went for the file lock again although the cache is valid"
    );
    assert_eq!(refresher.call_count(), 0);
    drop(held);
}

// --- Preserving the rotation ------------------------------------------

#[test]
fn the_rotated_refresh_token_is_persisted_before_the_new_access_token_is_returned() {
    let dir = temp_dir("rotation");
    seed_expired_token(&dir);
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::new(CountingRefresher::new("NEW")));

    assert_eq!(mgr.access_token().expect("token"), "NEW");

    // The disk must already hold the new refresh token — returning before persisting would
    // lose the rotated token if the process died here, and a re-OAuth would be needed.
    let v = disk_json(&dir);
    assert_eq!(v["refresh_token"], serde_json::json!("NA_ROTATED"));
    assert_eq!(v["access_token"], serde_json::json!("NEW"));

    // REQ-18: the manager's Debug does not leak tokens.
    let rendered = format!("{mgr:?}");
    assert!(!rendered.contains("NEW"), "access token leaked: {rendered}");
    assert!(
        !rendered.contains("NA_ROTATED"),
        "refresh token leaked: {rendered}"
    );
}

#[test]
fn expires_at_is_the_refresh_time_plus_the_reported_lifetime() {
    // Rules out an implementation that uses only `now` or only `expires_in`, as well as one
    // that ignores the injected clock and reads the wall clock.
    let dir = temp_dir("expiry");
    seed_expired_token(&dir);
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::new(CountingRefresher::new("NEW")))
        .with_clock(Arc::new(FixedClock::new(1_000_000)));

    assert_eq!(mgr.access_token().expect("token"), "NEW");
    assert_eq!(
        disk_json(&dir)["expires_at"],
        serde_json::json!(1_028_800_u64),
        "expires_at is not now + expires_in"
    );
}

// --- Lock --------------------------------------------------------------

#[test]
fn a_lock_timeout_never_refreshes() {
    // Proceeding after a timeout is exactly a concurrent refresh, and Tesla answers that with
    // invalidation of the whole chain. This test blocks that path.
    let dir = temp_dir("locktimeout");
    seed_expired_token(&dir);
    let held = lock::acquire(&lock_path(&dir), lock::LOCK_DEADLINE).expect("hold the lock");

    let refresher = Arc::new(CountingRefresher::new("NEW"));
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>)
        .with_lock_deadline(Duration::from_millis(200));

    let err = mgr.access_token().expect_err("should time out");
    assert!(err.to_string().contains("token lock"), "{err}");
    assert_eq!(
        refresher.call_count(),
        0,
        "refreshed without having obtained the lock — the concurrent refresh path"
    );
    assert_eq!(
        disk_json(&dir)["refresh_token"],
        serde_json::json!("NA_OLD"),
        "the disk was updated without the lock"
    );
    drop(held);
}

// --- Directory preparation ---------------------------------------------

#[test]
fn a_missing_token_directory_is_created_owner_only_instead_of_only_being_checked() {
    // A check-only guard (`ensure_private_dir` alone) returns NotFound when the directory does
    // not exist and creates nothing — the first-login path would be blocked entirely.
    let base = temp_dir("mkdir");
    let dir = base.join("nested").join("deep");
    let refresher = Arc::new(CountingRefresher::new("UNUSED"));
    let mgr = TokenManager::new(dir.clone(), Arc::clone(&refresher) as Arc<_>)
        .with_lock_deadline(Duration::from_millis(200));

    // There is no token file, so the result is an error either way. What is checked is the
    // **side effect**.
    let err = mgr.access_token().expect_err("there is no token");
    assert!(err.to_string().contains("token store"), "{err}");
    assert!(
        dir.is_dir(),
        "the directory was not created — this is check-only, not create-then-check"
    );
    assert_owner_only(&base.join("nested"), "token dir (ancestor)");
    assert_owner_only(&dir, "token dir");
    assert_eq!(refresher.call_count(), 0);
}

#[test]
fn an_over_permissive_directory_is_rejected_before_the_lock_is_acquired() {
    // An implementation that goes for the lock first yields a 10-second Timeout under
    // contention, and the real cause (directory permissions) is never seen. With the lock held
    // from outside, two things are checked at once: the kind of error, and the fact that no
    // attempt to acquire the lock was made at all.
    let dir = temp_dir("baddir");
    seed_expired_token(&dir);
    let held = lock::acquire(&lock_path(&dir), lock::LOCK_DEADLINE).expect("hold the lock");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).expect("chmod");

    let refresher = Arc::new(CountingRefresher::new("NEW"));
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>)
        .with_lock_deadline(Duration::from_secs(3));

    let err = mgr
        .access_token()
        .expect_err("an over-permissive directory should be refused");

    assert!(
        err.to_string().contains("token directory permissions"),
        "failed for a reason other than directory permissions: {err}"
    );
    // Look at the mechanism, not the wall clock — there must have been **no** attempt to
    // acquire the lock.
    assert_eq!(
        mgr.lock_attempts(),
        0,
        "the lock came before the directory check"
    );
    assert_eq!(refresher.call_count(), 0);

    drop(held);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("restore");
}

#[test]
fn a_broken_clock_is_diagnosed_instead_of_timing_out_on_a_sibling_s_lock() {
    // Constraint 2 (misdiagnosis prevention) applied to the clock as well. If `now == u64::MAX`
    // no token can be fresh, so nothing ahead of this check can change the result — yet if the
    // lock were attempted first, then while a sibling holds it the user would wait 10 seconds,
    // see `token lock: timed out`, and never suspect the clock.
    let dir = temp_dir("clock-before-lock");
    seed_expired_token(&dir);
    let held = lock::acquire(&lock_path(&dir), lock::LOCK_DEADLINE).expect("hold the lock");

    let pre_epoch = std::time::UNIX_EPOCH - Duration::from_secs(1);
    let refresher = Arc::new(CountingRefresher::new("NEW"));
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>)
        .with_lock_deadline(Duration::from_secs(3))
        .with_clock(Arc::new(FixedClock::new(crate::clock::secs_since_epoch(
            pre_epoch,
        ))));

    let err = mgr.access_token().expect_err("should be refused");
    assert!(
        err.to_string().contains("unix epoch"),
        "reported lock contention instead of the clock: {err}"
    );
    assert_eq!(
        mgr.lock_attempts(),
        0,
        "the lock came before the clock check"
    );
    assert_eq!(refresher.call_count(), 0);
    drop(held);
}

// --- Write-side epoch hazard -------------------------------------------

#[test]
fn a_pre_epoch_clock_refuses_to_refresh_rather_than_minting_an_immortal_token() {
    // Simply writing it with `saturating_add` gives expires_at = u64::MAX, stamping a token
    // that "never expires" onto disk. That file keeps being trusted even after the clock is
    // fixed — a silent, permanent failure.
    let dir = temp_dir("preepoch");
    seed_expired_token(&dir);
    let pre_epoch = std::time::UNIX_EPOCH - Duration::from_secs(1);
    let refresher = Arc::new(CountingRefresher::new("NEW"));
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>).with_clock(
        Arc::new(FixedClock::new(crate::clock::secs_since_epoch(pre_epoch))),
    );

    let err = mgr.access_token().expect_err("should be refused");
    assert!(err.to_string().contains("unix epoch"), "{err}");
    assert_eq!(
        refresher.call_count(),
        0,
        "a rotation was fired although the expiry time cannot be computed"
    );
    let v = disk_json(&dir);
    assert_eq!(v["refresh_token"], serde_json::json!("NA_OLD"));
    assert_eq!(v["expires_at"], serde_json::json!(1_u64));
}

#[test]
fn an_unrepresentable_expiry_is_recorded_as_expired_not_immortal() {
    // When the clock is near u64::MAX, `now + expires_in` is unrepresentable. `saturating_add`
    // creates a permanent token here too. The rotation is preserved, but the expiry time is
    // recorded in the direction that does not lie (= already expired).
    //
    // The `expires_at == now` this test pins looks, on the surface, like the very collapse
    // (= rotation on every call) that the `MAX_TOKEN_LIFETIME` cap is meant to stop. The reason
    // no cap on the number of rotations is demanded here all the same is that **this state is
    // unreachable in production**: on 64-bit Unix the seconds since the epoch of a `SystemTime`
    // are bounded by `i64::MAX` (≈ `u64::MAX / 2`), so for any `now` `SystemClock` can produce,
    // `now + 2_592_000` (the maximum after the clamp) does not overflow. Getting here takes
    // injecting the value directly, as with `FixedClock`. In other words this path is not "a
    // condition the server can create" but "a safety net against implementation mistakes", and
    // it differs in kind from the high side (the clamp) and the low side (no lower bound,
    // documented as a hazard), both of which the server can create freely.
    let dir = temp_dir("nearmax");
    seed_expired_token(&dir);
    let now = u64::MAX - 5;
    let refresher = Arc::new(CountingRefresher::new("NEW"));
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>)
        .with_clock(Arc::new(FixedClock::new(now)));

    // An implementation that looks at `is_fresh` again after the refresh and repeats would loop
    // forever here or return an error. It must refresh exactly once and return the new token as
    // is.
    assert_eq!(mgr.access_token().expect("token"), "NEW");
    assert_eq!(refresher.call_count(), 1);

    let v = disk_json(&dir);
    assert_eq!(v["refresh_token"], serde_json::json!("NA_ROTATED"));
    assert_ne!(
        v["expires_at"],
        serde_json::json!(u64::MAX),
        "a token that never expires was stamped onto disk"
    );
    assert_eq!(v["expires_at"], serde_json::json!(now));
}

// --- Persist-failure contract ------------------------------------------

#[test]
fn a_failed_persist_forbids_a_second_refresh_but_still_adopts_a_good_disk_token() {
    // When the persist fails, the on-disk state is unknown (the fsync after the rename can
    // fail). Retrying with the old refresh token is the very behaviour Tesla answers with
    // invalidation of the whole chain.
    let dir = temp_dir("persistfail");
    seed_expired_token(&dir);
    // Create the lock file in advance — it cannot be created in a write-protected directory.
    drop(lock::acquire(&lock_path(&dir), lock::LOCK_DEADLINE).expect("pre-create the lock file"));
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).expect("chmod");
    if !writes_are_denied(&dir) {
        // Running as root — a persist failure cannot be staged, so there is nothing to test.
        //
        // **Always say so.** With a silent return, this test becomes a complete no-op in a
        // root-container CI — and it is the **only** test that exercises the
        // `refresh_forbidden` contract and disk adoption after the poison — while CI stays
        // green. It says so in the same way as the same guard in store.rs.
        eprintln!(
            "skipped a_failed_persist_forbids_a_second_refresh_but_still_adopts_a_good_disk_token: \
             running as root — writes succeed even in a read-only directory"
        );
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("restore");
        return;
    }

    let refresher = Arc::new(CountingRefresher::new("NEW"));
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>)
        .with_lock_deadline(Duration::from_millis(500));

    let err = mgr.access_token().expect_err("the persist should fail");
    assert!(err.to_string().contains("persist"), "{err}");
    for leaked in ["NA_OLD", "NA_ROTATED", "NEW"] {
        assert!(
            !err.to_string().contains(leaked),
            "a token was carried in the error message: {err}"
        );
    }
    assert_eq!(refresher.call_count(), 1);

    // Second call — after a rotation that could not be persisted, it must not retry with the
    // old token.
    let err2 = mgr.access_token().expect_err("should be forbidden");
    assert!(err2.to_string().contains("token chain"), "{err2}");
    // The reason must be the persist failure (so that it is not mixed up with the panic reason,
    // final review M-3).
    assert!(
        err2.to_string().contains("could not be persisted"),
        "{err2}"
    );
    assert_eq!(
        refresher.call_count(),
        1,
        "retried with the old refresh token after a rotation that could not be persisted"
    );

    // But if a sibling process has refreshed successfully, adoption continues — this checks
    // that the ban has not degenerated into "always an error".
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("restore");
    write_token(&dir, "FROM_DISK", "NA_SIBLING", FAR_FUTURE);
    assert_eq!(mgr.access_token().expect("adopt"), "FROM_DISK");
    assert_eq!(refresher.call_count(), 1);
}

#[test]
fn a_missing_token_file_is_reported_without_calling_refresh() {
    // There is no refresh token to refresh with at all. Calling the refresher here would POST
    // an empty string.
    let dir = temp_dir("notokenfile");
    let refresher = Arc::new(CountingRefresher::new("NEW"));
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>)
        .with_lock_deadline(Duration::from_millis(200));

    let err = mgr.access_token().expect_err("there is no token");
    assert!(err.to_string().contains("token store"), "{err}");
    assert_eq!(refresher.call_count(), 0);
}

// --- Refresh failure (not symmetric with persist failure) --------------

#[test]
fn a_failing_refresh_leaves_the_cache_the_disk_and_the_poison_flag_untouched() {
    // The side that must be handled **differently** from a persist failure. When the POST
    // fails, the refresh token we hold was not touched, so a retry is safe. Forbidding here as
    // well would make a single timeout in an underground car park demand a re-OAuth.
    //
    // Four things are pinned at once: the error is propagated / the disk is unchanged / the
    // cache is not polluted / a retry is allowed. The last one in particular fixes the
    // asymmetry with the persist failure.
    let dir = temp_dir("failrefresh");
    seed_expired_token(&dir);
    let refresher = Arc::new(FailingRefresher::new());
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>)
        .with_lock_deadline(Duration::from_millis(200));

    let err = mgr
        .access_token()
        .expect_err("the failure should be propagated");
    assert!(err.to_string().contains("upstream refused"), "{err}");
    assert_eq!(refresher.call_count(), 1);

    // The disk must be left untouched — a partial persist is how a rotation gets lost.
    let v = disk_json(&dir);
    assert_eq!(v["access_token"], serde_json::json!("OLD"));
    assert_eq!(v["refresh_token"], serde_json::json!("NA_OLD"));
    assert_eq!(v["expires_at"], serde_json::json!(1_u64));

    // The second call gives the same error — if a broken token were cached, this would be Ok.
    let err2 = mgr.access_token().expect_err("should still fail");
    assert!(err2.to_string().contains("upstream refused"), "{err2}");
    assert!(
        !err2.to_string().contains("token chain"),
        "a network failure was poisoned like a persist failure: {err2}"
    );
    assert_eq!(
        refresher.call_count(),
        2,
        "a retry was blocked after a transient refresh failure — treated like a persist failure"
    );
}

// --- Server-supplied lifetime ------------------------------------------

#[test]
fn an_absurd_reported_lifetime_is_clamped_instead_of_collapsing_to_expired() {
    // Without the cap, `now + u64::MAX` overflows and collapses to `unwrap_or(now)` = **already
    // expired**. From then on **every** call fires a refresh POST and rotates the chain once —
    // bounded per call but unbounded in the number of calls, and what it runs up against is
    // Tesla's auth rate limit. The second call catches that storm.
    let dir = temp_dir("absurdlifetime");
    seed_expired_token(&dir);
    let now = 1_000_000_u64;
    let refresher = Arc::new(LifetimeRefresher::new(u64::MAX));
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>)
        .with_clock(Arc::new(FixedClock::new(now)));

    assert_eq!(mgr.access_token().expect("token"), "NEW");
    let expires_at = disk_json(&dir)["expires_at"].clone();
    assert_ne!(
        expires_at,
        serde_json::json!(now),
        "the absurd lifetime collapsed to 'already expired' — every call rotates the chain"
    );
    assert_ne!(
        expires_at,
        serde_json::json!(u64::MAX),
        "a token that never expires was stamped onto disk"
    );
    assert_eq!(
        expires_at,
        serde_json::json!(now + MAX_TOKEN_LIFETIME.as_secs()),
        "it was cut down to some value other than the cap"
    );

    // What is really being checked: the second call does not fire a rotation.
    assert_eq!(mgr.access_token().expect("second"), "NEW");
    assert_eq!(
        refresher.call_count(),
        1,
        "the second call rotated again — the cap did not stop the storm"
    );
}

#[test]
fn the_lifetime_cap_is_thirty_days() {
    // The test above asserts against `MAX_TOKEN_LIFETIME` itself, so it passes even if the
    // constant is changed (raise it to 10 years and even the verbatim test passes along with
    // it). The value itself is pinned here so that a change moving the cap is a conscious
    // decision.
    assert_eq!(MAX_TOKEN_LIFETIME, Duration::from_secs(2_592_000));
}

#[test]
fn a_reported_lifetime_inside_the_skew_window_is_stale_on_arrival() {
    // As the name says — a lifetime inside the skew window is **still** stale on arrival. That
    // fact did not change after A3, and this test keeps pinning that boundary.
    //
    // `is_fresh` is `expires_at > now + EXPIRY_SKEW` (60 seconds). So if the server gives
    // `expires_in_secs <= 60`, the new token is already stale the moment it is recorded.
    //
    // # What changed from A2 → A3 (the only change to this test)
    //
    // In the A2 days this test **pinned a known, unresolved hazard**: every call after that
    // fired a refresh POST and rotated the chain — **the same damage** the
    // `MAX_TOKEN_LIFETIME` cap was meant to stop on the high side happened on the low side,
    // without any clock failure — and A2 did not fix it but recorded "rotation on every call"
    // as it was.
    //
    // A3 Task 5 closed that hole with `MIN_REFRESH_INTERVAL`. So only the last block of the
    // loop below changed, from "the second call rotates again" to "the second call is blocked by
    // the interval". Not one line of the rest was touched.
    //
    // **It is not a `max` lower bound** — that direction, which A2 forbade, ignores a short
    // lifetime the server genuinely stated and sends a dead token (the rationale is in the
    // `MAX_TOKEN_LIFETIME` doc, and that argument still holds). This test proves the
    // distinction: the `expires_at == now + expires_in` assertion below **passes unchanged**,
    // so the lifetime value was not manipulated, and the only thing suppressed is the
    // **frequency** of rotation.
    let now = 1_000_000_u64;

    // Above the boundary — 61 seconds is outside the skew, so it is cached normally (control).
    {
        let dir = temp_dir("skewfloor-above");
        seed_expired_token(&dir);
        let refresher = Arc::new(LifetimeRefresher::new(EXPIRY_SKEW.as_secs() + 1));
        let mgr = TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>)
            .with_clock(Arc::new(FixedClock::new(now)));

        assert_eq!(mgr.access_token().expect("first"), "NEW");
        assert_eq!(mgr.access_token().expect("second"), "NEW");
        assert_eq!(
            refresher.call_count(),
            1,
            "the second call rotated again although it is outside the skew (61 seconds)"
        );
    }

    // On and below the boundary — 0 seconds and exactly the skew (60 seconds) are both stale on
    // arrival.
    for expires_in in [0, EXPIRY_SKEW.as_secs()] {
        let dir = temp_dir(&format!("skewfloor-{expires_in}"));
        seed_expired_token(&dir);
        let refresher = Arc::new(LifetimeRefresher::new(expires_in));
        let mgr = TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>)
            .with_clock(Arc::new(FixedClock::new(now)));

        assert_eq!(mgr.access_token().expect("first"), "NEW");
        // The rotation was preserved normally — the value is not manipulated.
        let v = disk_json(&dir);
        assert_eq!(v["refresh_token"], serde_json::json!("NA_ROTATED"));
        assert_eq!(
            v["expires_at"],
            serde_json::json!(now + expires_in),
            "expires_in={expires_in}: reported lifetime should be recorded as is (no lower bound)"
        );

        // After A3 Task 5: the second call **does not rotate again**. The
        // MIN_REFRESH_INTERVAL (60 seconds) gate blocks it and, instead of returning the
        // cache (= already stale), returns an error that names the cause. This is the
        // handover A2 left at this spot (`MAX_TOKEN_LIFETIME` doc: "the safe solution is ...
        // to put a minimum interval on rotation itself, or ...") carried out as written.
        //
        // It matters that the `expires_at == now + expires_in` assertion above passes
        // unchanged — it is the evidence that this is not the forbidden `max` lower bound.
        // The lifetime value is exactly what the server gave, and the only thing suppressed
        // is **the frequency of rotation**. Without that assertion this test could no longer
        // tell the two solutions apart (the safe interval vs the forbidden lower bound) —
        // never remove it.
        let err = mgr
            .access_token()
            .expect_err("expires_in={expires_in}: the interval gate should block it");
        assert!(
            err.to_string().contains("rotation interval"),
            "expires_in={expires_in}: failed for a reason other than the interval: {err}"
        );
        assert_eq!(
            refresher.call_count(),
            1,
            "expires_in={expires_in}: the chain was rotated once more inside the interval — \
             the storm A2 handed to A3 is still there"
        );
    }
}

#[test]
fn a_normal_reported_lifetime_is_recorded_verbatim() {
    // The other side. Rules out an implementation that always uses the cap (= ignores the
    // lifetime argument).
    let dir = temp_dir("normallifetime");
    seed_expired_token(&dir);
    let now = 1_000_000_u64;
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::new(LifetimeRefresher::new(28_800)))
        .with_clock(Arc::new(FixedClock::new(now)));

    assert_eq!(mgr.access_token().expect("token"), "NEW");
    assert_eq!(
        disk_json(&dir)["expires_at"],
        serde_json::json!(now + 28_800),
        "a normal lifetime was changed to the cap or some other value"
    );
}

// --- Lock file immortality (REQ-15) ------------------------------------

#[test]
fn the_lock_file_survives_every_path_through_access_token() {
    // REQ-15: the lock file is never deleted, on success or on error. If it were removed, a
    // process holding the old inode and a process taking the new inode would be alive at the
    // same time and mutual exclusion would silently vanish. The inode itself is checked because
    // `exists()` alone would let a "delete and recreate" implementation pass (lock.rs checks
    // the inode for the same reason).
    use std::os::unix::fs::MetadataExt as _;
    let ino_of = |p: &Path| std::fs::metadata(p).expect("lock file must exist").ino();

    let dir = temp_dir("lockimmortal");
    let path = lock_path(&dir);

    // (a) The success path.
    seed_expired_token(&dir);
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::new(CountingRefresher::new("NEW")));
    mgr.access_token().expect("success path");
    let ino = ino_of(&path);

    // (b) The refresh failure path.
    seed_expired_token(&dir);
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::new(FailingRefresher::new()));
    mgr.access_token().expect_err("the refresh should fail");
    assert_eq!(
        ino_of(&path),
        ino,
        "the refresh failure path replaced the lock file"
    );

    // (c) The path where the token file is gone (disk read failure).
    std::fs::remove_file(token_path(&dir)).expect("remove token file");
    mgr.access_token().expect_err("there is no token");
    assert_eq!(
        ino_of(&path),
        ino,
        "the read failure path replaced the lock file"
    );
}

/// A refresher that panics during a refresh (spec §0 I1).
struct PanickingRefresher;

impl TokenRefresher for PanickingRefresher {
    fn refresh(&self, _refresh_token: &str) -> Result<Refreshed, Error> {
        panic!("refresher blew up mid-rotation");
    }
}

/// Even if the refresher panics, other callers must not get stuck on the `refreshing` flag, and
/// no further rotation may happen.
///
/// It is unknown whether the server completed the rotation at the moment of the panic, so
/// rotating again with the old refresh token would definitely break the chain. So the next call
/// must receive the "rotation forbidden" error within `state_deadline`, and the token file on
/// disk must be left as it was. Negative control: removing `refresh_forbidden = true` from
/// `State::recover` makes the second call enter the refresher again and panic.
#[test]
fn a_panicking_refresher_forbids_refresh_and_does_not_block_others() {
    let dir = temp_dir("panicking-refresher");
    seed_expired_token(&dir);
    let before = disk_json(&dir);
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::new(PanickingRefresher) as Arc<_>)
        .with_clock(Arc::new(FixedClock::new(T0)) as Arc<_>)
        .with_lock_deadline(Duration::from_secs(3))
        .with_state_deadline(Duration::from_secs(2));

    let first = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| mgr.access_token()));
    assert!(
        first.is_err(),
        "the refresher's panic should propagate to its caller"
    );

    let started = std::time::Instant::now();
    let second = mgr.access_token();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "second caller waited out the state deadline: {:?}",
        started.elapsed()
    );
    let msg = second.expect_err("refresh must stay forbidden").to_string();
    // It has to say the cause is the interruption, not a persist failure, so that the operator
    // does not go digging through the disk (final review M-3).
    assert!(msg.contains("was interrupted"), "actual: {msg}");
    assert!(!msg.contains("could not be persisted"), "actual: {msg}");
    assert_eq!(disk_json(&dir), before, "the token file must be untouched");
}

//! Token lifecycle — storage, rotation and cross-process single-flight refresh.
//!
//! This module never touches the network. The actual refresh POST is the job of a
//! [`TokenRefresher`] implementation, which subproject A3 (HTTP transport) provides.

mod cell;
mod jwt;
mod lock;
mod refresh;
mod store;

pub use refresh::{HttpRefresher, REFRESH_TIMEOUT};
// A3 Task 9: the request pipeline (`crate::client::Client::send`) is the
// first non-test caller of these — `jwt` itself stays a private submodule
// (its internals are not part of this crate's public API), but `send` is a
// sibling of `auth`, not a descendant, so it needs a crate-visible path in.
// Crate-private, not `pub`: `UnverifiedScopeObservation`/`observe_scopes`
// are advisory-precheck internals, never meant to leak past this crate's own
// transport layer (design §10).
pub(crate) use jwt::{UnverifiedScopeObservation, observe_forbidden, observe_scopes};

use crate::clock::{Clock, SystemClock};
use crate::error::Error;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use store::{EXPIRY_SKEW, TokenSet, TokenStore};
// Fix round 1 (Minor 1, A3 Task 6 review): re-exported (not just `use`d) so
// `crate::test_support` — a sibling module of `auth`, not a descendant — can
// name it too. Before this, `test_support::write_token` hardcoded the
// literal `"tokens.json"` while this module's own `token_path()` test helper
// derived it from `TokenStore`, the exact split-source-of-truth split A2's
// own comments warned about avoiding. `#[cfg(test)]`-gated: its only current
// consumer is `test_support` (test-only), so an unguarded `pub(crate) use`
// here is `unused_imports` in a non-test build.
#[cfg(test)]
pub(crate) use store::TOKEN_FILE;

/// Upper bound on the length of an error `message`. The child module ([`refresh`]) uses it as
/// is — it lives in one place so that one layer's message budget does not split into two values.
const MSG_MAX: usize = 512;

/// Upper bound on the lifetime accepted for a single token.
///
/// [`Refreshed::expires_in_secs`] is a **server-supplied value**. If an absurdly large value
/// arrives, `now + expires_in` overflows `u64`, and then the `unwrap_or(now)` fallback below
/// records "already expired" — from then on **every call fires one refresh POST and rotates the
/// chain once**. That is bounded per call but unbounded in the number of calls, and what it runs
/// up against is Tesla's auth rate limit.
///
/// So the lifetime is **cut down** here (why clamp rather than reject: the refresh POST has
/// already succeeded and the rotated token must be preserved). The cut always goes toward the
/// **shorter side**, so it is safe — it only refreshes earlier than necessary and never sends a
/// dead token. 30 days is 90 times Tesla's actual access token lifetime (around 8 hours), so it
/// never touches a normal value, yet it is small enough to stop a rotation storm.
///
/// # ⚠️ There is no lower bound on the other side — and **one must not be added**
///
/// **A hole symmetric to the one above is open on the low side.** [`TokenSet::is_fresh`] is
/// `expires_at > now + EXPIRY_SKEW` (60 seconds), so if the server gives `expires_in_secs <= 60`
/// the new token is **already stale the moment it is written**. Every later
/// [`TokenManager::access_token`] call then fires a refresh POST and rotates the chain once —
/// **literally the same damage** the upper bound above was meant to stop, against the same Tesla
/// auth rate limit, and reached without any clock failure. `expires_in: 0` is a value the server
/// can produce just as freely as `u64::MAX`.
///
/// **Yet the floor must not be propped up with `max`.** `min` is safe because that direction
/// **only ever shortens** — it only refreshes earlier than necessary and never sends a dead
/// token. `max` is the exact opposite: if the server genuinely said "30 seconds" and we insist
/// on 120 seconds, we keep sending an already dead access token for more than 60 seconds. That
/// trades avoiding a rotation storm (cost, rate limit) for **authentication failure (loss of
/// function)**, which is not an even exchange.
///
/// ⚠️ **This argument still holds.** The storm having been closed below does **not** make a lower
/// bound safe — it was closed precisely because the way it was closed was not value manipulation.
/// If someone later reads this as "there is an interval now, so surely `max` is fine", that is a
/// sign the paragraph above needs to be read again. The two mechanisms are not substitutes for
/// each other.
///
/// # ✅ The storm was closed by A3 Task 5 — by **frequency**, not by value
///
/// The safe solution was not to manipulate the value but to **put a minimum interval on rotation
/// itself**, and that is [`MIN_REFRESH_INTERVAL`](crate::MIN_REFRESH_INTERVAL) (= 60 seconds, the
/// same as `EXPIRY_SKEW`). The server-supplied lifetime is **still recorded as is** — there is no
/// lower bound even now, and none will be added. The only thing that changed is that the next
/// rotation is **refused** if 60 seconds have not passed since the previous one (the cache is not
/// returned either — that would be sending a dead token, the same failure as the `max` rejected
/// above).
///
/// The boundary and what happens below it remain pinned by
/// `a_reported_lifetime_inside_the_skew_window_is_stale_on_arrival`. That test's
/// `expires_at == now + expires_in` assertion is the evidence that "the value was not touched",
/// so it must not be removed.
///
/// The other half — **raising** `expires_in_secs <= EXPIRY_SKEW` **to a diagnosable condition**
/// (response parser, logging) — is still unimplemented. The storm is blocked, but there is not
/// yet a path that **tells** a person about that server behaviour.
const MAX_TOKEN_LIFETIME: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// The **generation** of a cached token set.
///
/// It increases by 1 **every time** the token [`TokenManager`] holds in memory **is replaced** —
/// equally when we rotated it ourselves and when we adopted from disk one that a sibling process
/// rotated. The generation is an identifier of "what is in the cache right now", not a counter
/// of "how many times we refreshed".
///
/// # What it is for
///
/// 401 recovery uses it to decide "may I invalidate the token **I used**". Get the generation
/// and the token together from [`TokenManager::access_token_with_generation`], and when a 401
/// arrives hand that generation back to [`TokenManager::invalidate_generation`]. Without this
/// pairing, concurrent 401s kill a token that was just refreshed: the 401 for a request A sent
/// with generation N makes it discard the N+1 that B has already produced.
///
/// # ⚠️ Do not change this into a "refresh count"
///
/// That this value also rises on **disk adoption** (the path that takes over a token a sibling
/// process rotated) is not a mistake. The question the generation answers is "is the token I
/// was given still current", and **adoption replaces that token too**.
///
/// Changing it to "bump only on refresh" makes the generation lie about the question it has to
/// answer: if B adopts a sibling's rotation after A sent a request with generation N and the
/// generation stays N, A's late-arriving 401 matches the generation and discards **the perfectly
/// good token that was just adopted** — the very accident the generation exists to prevent would
/// be left in place on the adoption path alone.
/// (Pinned by `generation_increases_when_a_siblings_rotation_is_adopted`.)
///
/// Conversely, the time of the last **rotation** (the reference point of
/// `MIN_REFRESH_INTERVAL`) is **not** advanced on adoption — that one asks "how often did we
/// send a POST". The two fields answer different questions, so their update conditions differ
/// too.
pub type Generation = u64;

/// Minimum interval kept between rotations (= refresh POSTs).
///
/// It is the **same value** as `EXPIRY_SKEW` (crate-internal). The logic closes in one sentence:
/// **"a lifetime that cannot be considered fresh cannot be rotated either."** If the two values
/// diverge that sentence breaks, so `the_min_refresh_interval_is_the_expiry_skew` pins the
/// equality.
///
/// # What it prevents
///
/// **The same damage** as the rotation storm that `MAX_TOKEN_LIFETIME` blocks on the high side
/// was open on the low side as well: if the server gives `expires_in_secs <= 60`, the new token
/// is already stale the moment it is recorded, so **every** later call fires a refresh POST and
/// rotates the chain once. A2 did not fix this hole; it documented it and handed it to A3 (see
/// "There is no lower bound on the other side" on `MAX_TOKEN_LIFETIME`). This is that handover.
///
/// It is **not** a `max` lower bound that inflates the value. That direction ignores a short
/// lifetime the server genuinely stated and sends a dead access token — a trade that buys
/// authentication failure to avoid a rotation storm. Instead it limits **the frequency of
/// rotation itself**. The lifetime value is recorded exactly as the server gave it.
///
/// # ⚠️ A repeated request inside the interval is an **error**, not the cache
///
/// Simply returning the cached (but already stale) token would send a dead token, which is
/// **literally the same failure** as the `max` lower bound rejected above. So it stops with
/// [`Error::Auth`]`{ pricing: None }`, naming the cause. `pricing` is `None` because no endpoint
/// was reached — [`Error::billing`] must count it as [`crate::Billing::NotBilled`].
///
/// # ⚠️ It is process-local
///
/// The time of the last rotation is kept only in process memory (adding a field to the token
/// file would change A2's storage format contract). A new process can refresh immediately
/// without knowing this interval, and that is the **intended** behaviour — the purpose of this
/// mechanism is to suppress a storm within one process, not to be a global rate limiter. The
/// cross-process guarantee is provided by A2's flock single-flight.
pub const MIN_REFRESH_INTERVAL: Duration = Duration::from_secs(60);

/// A new token set returned by a refresh round trip.
///
/// Because it is `#[non_exhaustive]`, external crates cannot construct it with a literal.
/// [`Refreshed::new`] exists instead — without it the public [`TokenRefresher`] **could not be
/// implemented** outside the crate, and we would ship a public trait nobody can implement.
///
/// # ⚠️ `Debug` is written by hand (REQ-18)
///
/// This type carries **both** the access token and the refresh token, is re-exported from the
/// crate root, and is created right after the transport layer (A3) parses the refresh response
/// — exactly the spot where one `tracing::debug!(?refreshed)` line would stamp a live refresh
/// token into the logs. So it does not use `#[derive(Debug)]` and is written by hand in the
/// **same shape** as `TokenSet` (crate-internal): it destructures `Self` so that an added field
/// breaks with `E0027` instead of being silently left out, and the marker is the
/// `redaction::REDACTED` constant of the crate's single redaction layer.
#[derive(Clone)]
#[non_exhaustive]
pub struct Refreshed {
    /// The new access token.
    pub access_token: String,
    /// The new refresh token. On rotation Tesla invalidates the entire previous chain.
    pub refresh_token: String,
    /// Seconds from now until the access token expires.
    ///
    /// A value above 30 days is recorded cut down to 30 days — this prevents the storm in which
    /// an unrepresentable lifetime collapses into "already expired" and every call rotates the
    /// chain.
    ///
    /// ⚠️ **There is no lower bound on the low side.** If 60 seconds or less is reported, the
    /// token is judged about to expire the moment it is recorded (the expiry margin is 60
    /// seconds), and every later call fires a refresh again. Raising the value is a way of
    /// sending a dead token and so is not a safe solution — how this condition is handled is
    /// decided by the transport layer's retry and diagnostics policy.
    pub expires_in_secs: u64,
}

impl core::fmt::Debug for Refreshed {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // 구조분해가 이 impl 의 안전장치다 — `TokenSet` 과 같은 이유·같은 모양.
        // 필드를 추가하면 여기서 `E0027` 로 컴파일이 깨진다. `self.field` 로
        // 접근했다면 새 필드가 조용히 출력에서 빠지고 어떤 테스트도 실패하지 않는다.
        let Self {
            access_token: _,
            refresh_token: _,
            expires_in_secs,
        } = self;
        f.debug_struct("Refreshed")
            .field("access_token", &crate::redaction::REDACTED)
            .field("refresh_token", &crate::redaction::REDACTED)
            // 수명은 비밀이 아니고, 진단에 실제로 필요한 유일한 값이다.
            .field("expires_in_secs", expires_in_secs)
            .finish()
    }
}

impl Refreshed {
    /// Creates a new token set from a refresh response.
    ///
    /// This type is `#[non_exhaustive]`, so external crates cannot build it with a literal;
    /// this is therefore the **only** way to implement [`TokenRefresher`] outside the crate.
    ///
    /// Pass `expires_in_secs` exactly as the server gave it — the upper-bound (30 days) clamp is
    /// done in one place, by [`TokenManager::access_token`]. Touching it here would split the
    /// policy across two places.
    ///
    /// # Examples
    ///
    /// This doctest is compiled **as an external crate** — that is, the very fact that the code
    /// below compiles is the proof that "the public [`TokenRefresher`] can be implemented outside
    /// the crate".
    ///
    /// ```
    /// use fleetwire::{Error, Refreshed, TokenRefresher};
    ///
    /// struct MyRefresher;
    ///
    /// impl TokenRefresher for MyRefresher {
    ///     fn refresh(&self, _refresh_token: &str) -> Result<Refreshed, Error> {
    ///         // A real implementation performs an HTTP POST here, with its own timeout.
    ///         Ok(Refreshed::new(
    ///             "new-access-token".to_owned(),
    ///             "new-refresh-token".to_owned(),
    ///             28_800,
    ///         ))
    ///     }
    /// }
    /// ```
    #[must_use]
    pub fn new(access_token: String, refresh_token: String, expires_in_secs: u64) -> Self {
        Self {
            access_token,
            refresh_token,
            expires_in_secs,
        }
    }
}

/// An implementation of the refresh round trip.
///
/// A2 **only calls** this trait — the actual HTTP POST implementation comes from subproject A3.
/// That is why all of A2 is tested without a network.
///
/// The method is synchronous (stage 1 of removing tokio). It is kept dyn-safe so that it can be
/// held as `Arc<dyn TokenRefresher>`.
pub trait TokenRefresher: Send + Sync {
    /// Fetches a new token set using `refresh_token`.
    ///
    /// # ⚠️ Implementations must not retry this call
    ///
    /// The server may already have completed the rotation with only the response lost — in that
    /// case the `refresh_token` the caller holds is already dead, and the only way to revive it
    /// is a full re-OAuth. Calling this function again because it failed (including wrapping it
    /// in a generic retry decorator) only makes a chain that might still have been alive
    /// definitely invalid — it makes the failure worse. [`crate::HttpRefresher`] is built to
    /// honour this contract, but any other backend that implements this trait directly must
    /// honour it in the same way.
    fn refresh(&self, refresh_token: &str) -> Result<Refreshed, Error>;
}

/// The mutable state of [`TokenManager`].
///
/// The cache and the "refresh forbidden" flag are guarded by the **same** mutex. Kept apart, a
/// persist failure and the cache replacement would happen in different critical sections, and a
/// task caught between them could fire one more rotation without seeing the forbidden flag.
///
/// ⚠️ The [`Generation`] and the last rotation time, added later, are **under the same mutex**
/// too. The generation in particular must be one with the cache — if the critical section that
/// changed the cache and the one that bumped the generation were separate, a caller caught
/// between them would receive a generation that does not match its token and would invalidate a
/// token it never used. Placing the two fields side by side is not a convenience; it is that
/// invariant.
#[derive(Default)]
struct State {
    /// The last token set that was confirmed.
    cached: Option<TokenSet>,
    /// The generation of [`State::cached`]. It is bumped together **every time the cache is
    /// replaced**.
    ///
    /// The initial value 0 means "nothing has been cached yet", and a generation that actually
    /// goes out to a caller is always 1 or more — because there is no path that returns without
    /// the cache having been filled. So a caller never comes back holding 0, and no stray
    /// invalidation happens through a chance match with 0.
    generation: Generation,
    /// Time of the last **successful** refresh (unix seconds). It is the reference point of the
    /// minimum rotation interval.
    ///
    /// ⚠️ It lives only in process memory — it is not written to disk. See "It is process-local"
    /// on [`MIN_REFRESH_INTERVAL`].
    ///
    /// ⚠️ Disk adoption (taking over a sibling process's rotation) does **not touch** this.
    /// Adoption sent no refresh POST, so it never produced the traffic we are trying to
    /// suppress. If the adoption time were recorded here as well, we could not rotate when we
    /// actually need to for as long as a sibling stays busy.
    last_refresh_at: Option<u64>,
    /// A rotated token could not be left on disk — **this manager** no longer refreshes. See the
    /// "Persist-failure contract" on [`TokenManager::access_token`] below.
    ///
    /// ⚠️ **The scope is the manager, not the process.** This flag lives in `State`, so if one
    /// more [`TokenManager`] pointing at the same directory is created in the same process, that
    /// one starts with clean state and may attempt a rotation with the old refresh token (that
    /// configuration is exactly `two_managers_sharing_a_directory_collapse_into_one_refresh`).
    /// A stronger form would be a process-global, per-directory forbidden set, but that is
    /// global state that would entangle both test parallelism and A4's path resolution, so it
    /// is not improvised here. In the production configuration (one manager) the two scopes
    /// coincide.
    ///
    /// A panic during a refresh (`State::recover`) blocks rotation for the same reason. The
    /// reason is kept separately so that the error text points at the real cause (final review
    /// M-3).
    refresh_forbidden: Option<Forbid>,
    /// The access token value (original bytes) that **the server rejected**. It is a one-slot
    /// blacklist.
    ///
    /// [`TokenManager::reject_generation`] fills it, and **both** the cache hit (step 2) and the
    /// disk adoption (step 6) of [`TokenManager::access_token_with_generation`] compare against
    /// this value so that the same token is not handed out again.
    ///
    /// ⚠️ Why "empty the cache" is not enough — a 401 is a server-side event and does not move
    /// the token's `expires_at`. If only the cache is emptied, the next call re-reads **the same
    /// file** at step 6, and since `is_fresh` is true adopts it as is, resending the very bytes
    /// that were just rejected — the retry becomes entirely pointless. So what is remembered is
    /// the **value**, not the generation.
    ///
    /// ⚠️ Because it is a value comparison, a **different** token that a sibling process has
    /// rotated in is still adopted normally. A rejection does not block the adoption path
    /// wholesale.
    ///
    /// ⚠️ It is **not cleared** even after a successful rotation or adoption. There is no reason
    /// to clear it (the next token has a different value and so is not caught by the gate), and
    /// leaving it makes "bytes that were rejected once are never sent out again" hold regardless
    /// of the path. Its size is fixed at one slot — the next rejection overwrites it.
    ///
    /// ⚠️ It is a token value, so it is never carried in any diagnostic string (REQ-18). `State`
    /// does not derive `Debug`.
    rejected_access_token: Option<String>,
}

impl cell::Recover for State {
    /// Called after a panic during a refresh or after lock poisoning (spec §0 I1). Since it is
    /// unknown whether a rotation happened, it discards the cache, bumps the generation so that
    /// an invalidation for an old generation does not touch a new token, and blocks any further
    /// rotation. `last_refresh_at` and `rejected_access_token` are left as they are.
    fn recover(&mut self) {
        self.cached = None;
        self.generation = self.generation.saturating_add(1);
        // 저장 실패가 먼저 있었다면 그 사유를 남깁니다(더 구체적입니다).
        self.refresh_forbidden.get_or_insert(Forbid::Interrupted);
    }
}

/// The reason this manager no longer rotates.
#[derive(Clone, Copy)]
enum Forbid {
    /// A rotated token could not be left on disk.
    PersistFailed,
    /// The refresh was cut off midway (panic or lock poisoning), so it is unknown whether a
    /// rotation happened.
    Interrupted,
}

/// A copy taken from the state when a refresh starts (read by steps 3–9, outside the lock).
struct Snapshot {
    refresh_forbidden: Option<Forbid>,
    last_refresh_at: Option<u64>,
    /// ⚠️ This is a token value. It is not carried in diagnostic strings (REQ-18).
    rejected: Option<String>,
}

/// The outcome when the steps outside the lock succeeded.
enum Fetched {
    /// A fresh token on disk was adopted (step 6).
    Adopted(TokenSet),
    /// The token was rotated and also persisted (step 9).
    Rotated { set: TokenSet, at: u64 },
}

/// The outcome when the steps outside the lock failed. Only a persist failure changes the state
/// (`refresh_forbidden`).
enum Failed {
    PersistFailed(Error),
    Other(Error),
}

impl State {
    /// Returns whether `candidate` is the very token that the server rejected.
    fn is_rejected(&self, candidate: &str) -> bool {
        self.rejected_access_token.as_deref() == Some(candidate)
    }
}

/// The token lifecycle manager.
///
/// It collapses refreshes into a single flight with [`cell::StateCell`] (Condvar + `refreshing`
/// flag) inside the process and with a fixed-inode file lock across processes (REQ-15). Every
/// method is synchronous and needs no runtime.
pub struct TokenManager {
    store: TokenStore,
    refresher: Arc<dyn TokenRefresher>,
    clock: Arc<dyn Clock>,
    /// Deadline for acquiring the file lock. In production it is always
    /// [`lock::LOCK_DEADLINE`]; only tests shorten it — so as not to write tests that really
    /// wait 10 seconds.
    lock_deadline: Duration,
    /// Deadline for acquiring the **in**-process mutex (REQ-15 AC3).
    ///
    /// Its value equals the file lock deadline, but it is a **different seam** and so is kept
    /// separately. If they were merged into one, a test that "shrank the deadline to 0 to
    /// exercise file lock contention" would also make the mutex wait 0 and kill every late
    /// caller of the in-process single-flight with a timeout — the very property under test
    /// would vanish.
    state_deadline: Duration,
    /// The lock is held briefly, and during a refresh the `refreshing` flag makes other callers
    /// wait ([`cell`]).
    /// ⚠️ Tokens rotate, so this is not put in a `OnceCell`/`OnceLock` (REQ-15).
    state: cell::StateCell<State>,
    /// Number of times this manager **went to acquire** the file lock (test-only
    /// instrumentation).
    ///
    /// "Answered from memory" and "went as far as the file lock" cannot be told apart by the
    /// return value or the refresh count — because when the lock is free both paths return the
    /// same value at the same speed (the cause of the M2 misclassification caught by the Task 7
    /// review). Separating them by wall-clock delay would flake on a loaded machine, so the
    /// mechanism itself is counted.
    ///
    /// It is kept **per manager**, not process-global — tests run in parallel.
    #[cfg(test)]
    lock_attempts: std::sync::atomic::AtomicUsize,
}

impl core::fmt::Debug for TokenManager {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // 캐시된 토큰을 찍지 않는다(REQ-18). `TokenSet` 의 Debug 가 이미 리댁션
        // 하지만, 여기서 상태를 통째로 찍지 않는 편이 잠금 없이도 안전하다.
        f.debug_struct("TokenManager").finish_non_exhaustive()
    }
}

impl TokenManager {
    /// Creates a manager from a token directory and a refresh implementation.
    ///
    /// The directory is not created here — the first [`TokenManager::access_token`] call creates
    /// it with mode 0700 when it is needed.
    pub fn new(dir: PathBuf, refresher: Arc<dyn TokenRefresher>) -> Self {
        Self {
            store: TokenStore::new(dir),
            refresher,
            clock: Arc::new(SystemClock),
            lock_deadline: lock::LOCK_DEADLINE,
            state_deadline: lock::LOCK_DEADLINE,
            state: cell::StateCell::new(State::default()),
            #[cfg(test)]
            lock_attempts: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// Returns the number of times this manager went to acquire the file lock (test-only).
    #[cfg(test)]
    fn lock_attempts(&self) -> usize {
        self.lock_attempts.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Replaces the clock.
    ///
    /// Both the expiry check and the `expires_at` computation use this clock. It is the seam
    /// that lets tests exercise the expiry boundary without a real sleep.
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Shortens the file lock deadline (test-only).
    ///
    /// It is not opened up as public API — lowering this value makes the normal situation of
    /// "a sibling process is refreshing" start to look like an error.
    #[cfg(test)]
    #[must_use]
    fn with_lock_deadline(mut self, deadline: Duration) -> Self {
        self.lock_deadline = deadline;
        self
    }

    /// Shortens the in-process mutex deadline (test-only).
    ///
    /// It is not public for the same reason as [`TokenManager::with_lock_deadline`] — lowering
    /// it makes the normal situation of "a sibling task is refreshing" start to look like an
    /// error.
    #[cfg(test)]
    #[must_use]
    fn with_state_deadline(mut self, deadline: Duration) -> Self {
        self.state_deadline = deadline;
        self
    }

    /// Returns a valid access token, refreshing exactly once if necessary.
    ///
    /// # Orchestration (design §7.3)
    ///
    /// 1. Acquire the state cell — under a **finite deadline**. If another caller is refreshing,
    ///    wait for it to finish; if the cache is empty, raise the `refreshing` flag, release the
    ///    lock and do steps 3–9 outside the lock.
    /// 2. Check the cache — if the in-memory token is fresh, return **without touching the file
    ///    lock**.
    /// 3. Clock sanity check — **before the file lock** (rationale below).
    /// 4. Prepare the token directory (create with 0700 + permission check) — **before the file
    ///    lock**.
    /// 5. Acquire `tokens.lock` (cross-process single-flight).
    /// 6. Re-read the disk under the lock — if a sibling **process** has already refreshed,
    ///    adopt it.
    /// 7. Check the persist-failure forbidden flag ("Persist-failure contract" below).
    /// 8. Minimum rotation interval check — if
    ///    [`MIN_REFRESH_INTERVAL`](crate::MIN_REFRESH_INTERVAL) has not passed since the
    ///    previous rotation, **refuse here** (the cache is not returned instead — the rationale
    ///    is in that constant's documentation).
    ///    ⚠️ This comes **after** adoption (step 6) — a token a sibling process has refreshed is
    ///    adopted regardless of the interval. The interval blocks only the POST we send.
    /// 9. Refresh exactly once.
    /// 10. Disk-first persist → replace memory only after it succeeds.
    /// 11. Replace the cache + generation +1 + record the last rotation time (all inside the
    ///     same critical section).
    ///
    /// (The numbers match the step comments in the body. If they disagree, one of the two is
    /// stale.)
    ///
    /// # Why the cache is checked only once (Task 8 decision)
    ///
    /// Task 7 placed one more identical check **in front of** the mutex (a "fast path"). Since
    /// both blocks take **the same exclusive mutex**, it was observationally equivalent — in
    /// fact no test caught the mutation that deletes only the front block — and Task 7 itself
    /// noted that fact in a comment. Task 8 **removed it.** The alternative that kept it was to
    /// turn `state` into a read-write lock and make the front block a real read guard, but this
    /// critical section is "an integer comparison + a `String` clone", an unmeasurable time
    /// compared with an HTTP round trip (milliseconds), and during a refresh the write guard
    /// holds the whole round trip anyway, so parallel reads gain nothing. Only the risk of
    /// changing the lock kind on a security-sensitive single-flight path would remain. Removing
    /// the duplication was chosen.
    ///
    /// # Why the clock check comes before the lock
    ///
    /// If `now == u64::MAX` no token can be fresh (the comparison is `expires_at >
    /// now + skew`), so neither cache adoption nor disk adoption can happen. Yet if we went for
    /// the lock first, then while a sibling holds it the user would wait 10 seconds and then see
    /// `token lock: timed out` — the real cause (a broken clock) is disguised as contention.
    /// This is the **same** misdiagnosis prevention as described below for directory
    /// permissions.
    ///
    /// **Exactly what is invariant**: not one *success* path changes (there is no path that
    /// yields `Ok` under a broken clock in the first place). What changed is **error priority**,
    /// and that is the purpose of this ordering — the clock diagnosis pre-empts three things:
    /// `token directory permissions`, `token store: <NotFound>` and `token chain`. As a side
    /// effect `prepare_dir()` does not run on this path, so **the 0700 directory is not created
    /// either**. In all three the result is still `Err`, and a broken clock is something that
    /// has to be fixed before those three, so this is the intended order.
    ///
    /// # Why directory preparation comes before the lock
    ///
    /// The lock file is in the same directory as the tokens. If we went for the lock first even
    /// though the directory is open to others, on the normal path it would simply be acquired,
    /// and under contention `LockError::Timeout` would occur after 10 seconds — the real cause
    /// (directory permissions) is disguised as "a sibling is holding it for a long time". So it
    /// is refused here first.
    ///
    /// **It creates, not merely checks.** A call to `ensure_private_dir` alone yields `NotFound`
    /// when the directory does not exist, so the path right after the first login, when there is
    /// no directory yet, would be blocked entirely. It uses the **same** pair as
    /// `TokenStore::store` (create with 0700 → permission check).
    ///
    /// # When the clock is before the epoch (write-side epoch hazard)
    ///
    /// `clock::secs_since_epoch` (crate-internal) saturates a pre-epoch time to [`u64::MAX`]. On
    /// the **read** side that direction is safe (`expires_at > u64::MAX` is always false →
    /// always refresh). The **write** side, however, is the exact opposite: if
    /// `now + expires_in` saturates to `u64::MAX`, a token that "never expires" is stamped onto
    /// disk, and that file keeps being trusted even after the clock is fixed. Therefore
    ///
    /// - If `now == u64::MAX`, refuse **before sending the refresh POST**. No rotation is fired,
    ///   so the chain on disk stays alive as it is, and the remedy ends with the single step
    ///   "fix the clock".
    /// - Any other unrepresentable lifetime (`now + expires_in` overflow) is recorded as
    ///   **`now` (= already expired)**, not `u64::MAX`. This direction neither loses the rotated
    ///   token nor leaves a false expiry time.
    /// - After the refresh, `is_fresh` is **not looked at again**. Looking again and repeating
    ///   would become a loop that rotates forever under a broken clock.
    /// - The server-supplied lifetime is cut down to 30 days. If the server could freely create
    ///   the condition under which that fallback (`now`) applies, the result would be a rotation
    ///   storm that is bounded for each single call but **unbounded in the number of calls**.
    ///   See the rationale on `MAX_TOKEN_LIFETIME`.
    ///
    /// `now` is read **before** the refresh POST. The expiry time is set earlier by however long
    /// the round trip took, so the direction is safe — it never claims a longer lifetime than
    /// was received.
    ///
    /// # Why the two failures are handled differently (the asymmetry is intentional)
    ///
    /// | Failure | Behaviour afterwards |
    /// |---|---|
    /// | refresh POST failure (network, 401, 5xx) | Only the error is propagated. The cache, the disk and the forbidden flag are **all left as they are**. The next call retries normally. |
    /// | Failure to **persist** the rotated token | Refresh is **forbidden** in this **manager** (contract below). |
    ///
    /// Both end in "the state is unknown", so why do they differ — **what is unknown** differs.
    /// When the POST fails, what is unknown is *whether the server committed the rotation*, but
    /// the refresh token we hold was not touched and Tesla does not invalidate the chain for a
    /// failed request. Forbidding here would make a single transient timeout in an underground
    /// car park demand a re-OAuth. When the persist fails, what is unknown is *whether the disk
    /// already has the new chain*, and retrying with the **old** refresh token at that point is
    /// reuse of an already invalidated chain, which is certain destruction. Only the
    /// unrecoverable side is blocked.
    ///
    /// # ⚠️ The refresh implementation must set its own timeout
    ///
    /// While calling [`TokenRefresher::refresh`], this function **holds both** the `refreshing`
    /// flag and **the file lock**. Neither has a timeout of its own (exactly the form design
    /// §7.3 required — releasing midway breaks the single-flight). Therefore, if a single
    /// implementation without a request timeout hangs,
    ///
    /// - every caller in this process is blocked **indefinitely**, and
    /// - sibling processes all run out the full `LOCK_DEADLINE` (10 seconds) and fail.
    ///
    /// This is a contract, not a defect. The transport layer of subproject A3 **must** set a
    /// finite timeout on the refresh request.
    ///
    /// # Each of the two waits has a finite deadline (REQ-15 AC3)
    ///
    /// There are two points of waiting, and **both** are finite.
    ///
    /// | Wait | Deadline | Error prefix |
    /// |---|---|---|
    /// | In-process mutex | `state_deadline` (10 seconds) | `token state lock:` |
    /// | Cross-process file lock | `lock_deadline` (10 seconds) | `token lock:` |
    ///
    /// The prefixes are **deliberately different** — the single sentence that arrives in a bug
    /// report has to separate "another caller in the same process is holding it" from "a sibling
    /// process is holding it" for the remedies to separate.
    ///
    /// **A deadline over the whole operation is deliberately not provided.** That would mean
    /// this future could be cancelled in the middle of the refresh POST, and at that moment the
    /// server may already have committed the rotation while we never get to persist the new
    /// refresh token — exactly the state Tesla answers with invalidation of the whole chain. The
    /// two waits above, by contrast, come **before any POST**, so cancelling them loses nothing.
    /// That is why the boundary is drawn at "before the POST".
    ///
    /// # Persist-failure contract (what the caller needs to know)
    ///
    /// If `TokenStore::store` (crate-internal) returns `Err`, **the on-disk state is unknown**:
    /// it can fail in the parent directory `fsync` after `rename` succeeded, so the disk may be
    /// ahead of memory. Therefore
    ///
    /// - After receiving this error, **do not retry with the old refresh token**. Tesla answers
    ///   reuse against an already rotated chain with invalidation of the whole chain.
    /// - So after a persist failure this manager forbids refresh itself **within this manager**.
    ///   It does, however, keep adopting if a valid token appears on disk — there is no reason
    ///   to also block the case where a sibling process refreshed successfully.
    /// - ⚠️ **That the scope is the manager means it is not the process.** The forbidden flag
    ///   lives in this instance's state, so if one more [`TokenManager`] pointing at the same
    ///   directory is created in the same process, that one starts with clean state. The
    ///   stronger form (a process-global, per-directory ban) is global state and is not
    ///   improvised here — the production configuration has one manager, so the two scopes
    ///   coincide.
    /// - The recovery path is to discard this manager, which in practice is a process restart.
    ///   If the rename already went through, the new token is simply read; if not, a re-OAuth is
    ///   needed.
    ///
    /// # Errors
    ///
    /// [`Error::Auth`]`{ pricing: None }` — failure to access the token directory or file,
    /// failure to acquire the mutex or the lock, a clock anomaly, failure to persist the rotated
    /// token, or **the minimum rotation interval not being met** (step 8 —
    /// [`MIN_REFRESH_INTERVAL`](crate::MIN_REFRESH_INTERVAL) has not passed since the previous
    /// rotation, or the clock went backwards. The cached token is **not** returned instead in
    /// that case). In no case is a token value carried in the message (REQ-18). An error
    /// produced by the refresh implementation is propagated as is.
    ///
    pub fn access_token(&self) -> Result<String, Error> {
        // 얇은 래퍼다 — 본문은 하나뿐이어야 한다. 두 경로로 복제하면 한쪽만
        // 고쳐지는 사고가 나고, 그 사고가 나는 곳이 하필 single-flight 다.
        self.access_token_with_generation().map(|(_gen, t)| t)
    }

    /// Returns a valid access token **together with** its [`Generation`].
    ///
    /// 401 recovery must use this form — it has to know which generation of token it sent for
    /// [`TokenManager::invalidate_generation`] to be able to decide "is that token still
    /// current". [`TokenManager::access_token`] is a thin wrapper around this function, so the
    /// orchestration, errors and contracts are **all described in that documentation**.
    ///
    /// ⚠️ The returned generation is a value read **inside the same critical section**. Reading
    /// it after releasing the lock would see a value another task rotated in the meantime, and
    /// the generation and token would no longer be a matching pair.
    ///
    /// # Errors
    ///
    /// The same as [`TokenManager::access_token`]. In addition, if another rotation is needed
    /// although [`MIN_REFRESH_INTERVAL`] has not passed since the previous rotation, it refuses
    /// with [`Error::Auth`]`{ pricing: None }` (the cache is not returned — see the
    /// [`MIN_REFRESH_INTERVAL`] documentation for the reason).
    pub fn access_token_with_generation(&self) -> Result<(Generation, String), Error> {
        // 1. in-process single-flight. REQ-15 AC3: 이 대기에도 유한 deadline 이 있어야 합니다.
        //    다른 호출자가 갱신 중이면(`refreshing` 깃발) 끝나길 기다립니다. 여기는 아직 어떤 POST 도
        //    보내기 전이라 포기해도 잃는 상태가 없습니다. 접두사는 파일 락(`token lock:`)과 **다르게**
        //    둡니다 — 두 대기를 오류 문장만으로 가를 수 있어야 합니다.
        let state = self
            .state
            .lock_within(self.state_deadline)
            .map_err(|_timeout| state_lock_timeout())?;

        // 2. 캐시 확인 — 유효하면 파일 락을 건드리지 않습니다. 기다리는 동안 형제가 이미 갱신해
        //    두었으면 여기서 걸립니다.
        //
        //    ⚠️ `is_rejected` 가 함께 걸린 이유(final fix wave / Important 1): `is_fresh` 만으로는
        //    **서버가 이미 거절한** 토큰을 걸러낼 수 없습니다. 401 은 `expires_at` 을 움직이지 않습니다.
        if let Some(set) = state.cached.as_ref()
            && set.is_fresh(self.clock.as_ref(), EXPIRY_SKEW)
            && !state.is_rejected(set.access_token())
        {
            // 세대는 잠금을 쥔 채로 읽습니다 — 이 토큰과 짝이 맞는 값이어야 합니다.
            return Ok((state.generation, set.access_token().to_owned()));
        }

        // 3~9 는 잠금 밖에서 합니다. 깃발이 서 있는 동안 상태를 바꾸는 것은 이 호출뿐이므로
        // 여기서 읽은 사본은 끝날 때까지 그대로입니다. 갱신 중 panic 이면 티켓의 Drop 이
        // `State::recover` 로 상태를 안전 쪽에 두고 기다리는 쪽을 깨웁니다(스펙 §0 I1).
        let snap = Snapshot {
            refresh_forbidden: state.refresh_forbidden,
            last_refresh_at: state.last_refresh_at,
            rejected: state.rejected_access_token.clone(),
        };
        let ticket = state.begin_refresh();
        match self.refresh_unlocked(&snap) {
            Ok(Fetched::Adopted(set)) => Ok(ticket.finish(|s| {
                s.generation = s.generation.saturating_add(1);
                let token = set.access_token().to_owned();
                s.cached = Some(set);
                (s.generation, token)
            })),
            Ok(Fetched::Rotated { set, at }) => Ok(ticket.finish(|s| {
                s.generation = s.generation.saturating_add(1);
                s.last_refresh_at = Some(at);
                let token = set.access_token().to_owned();
                s.cached = Some(set);
                (s.generation, token)
            })),
            Err(Failed::PersistFailed(e)) => {
                // 7번이 지키는 상태입니다: 회전은 일어났는데 디스크 상태를 모릅니다.
                ticket.finish(|s| s.refresh_forbidden = Some(Forbid::PersistFailed));
                Err(e)
            }
            Err(Failed::Other(e)) => {
                ticket.finish(|_s| ());
                Err(e)
            }
        }
    }

    /// Runs steps 3–9 (clock, directory, file lock, disk adoption, forbidden flag, minimum
    /// interval, rotation, persist). It runs outside the state cell lock.
    fn refresh_unlocked(&self, snap: &Snapshot) -> Result<Fetched, Failed> {
        // 3. 시계 온전성 — 락보다도, POST 보다도 **먼저**입니다.
        let now = self.clock.now_unix();
        if now == u64::MAX {
            return Err(Failed::Other(Error::Auth {
                message: "system clock is at or before the unix epoch; \
                          refusing to refresh because the new token's expiry \
                          cannot be computed"
                    .to_owned(),
                pricing: None,
            }));
        }

        // 4. 디렉터리 준비 — 파일 락보다 **먼저**입니다.
        self.store.prepare_dir().map_err(|e| {
            Failed::Other(Error::Auth {
                message: format!("token directory: {e}"),
                pricing: None,
            })
        })?;

        // 5. 프로세스 간 파일 락(REQ-15 의 나머지 절반).
        #[cfg(test)]
        self.lock_attempts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let _guard = lock::acquire(&self.store.lock_path(), self.lock_deadline).map_err(|e| {
            Failed::Other(Error::Auth {
                message: format!("token lock: {e}"),
                pricing: None,
            })
        })?;

        // 6. 디스크 채택 — 형제 프로세스가 이미 회전시켰으면 그 결과를 씁니다.
        let on_disk = self.store.load().map_err(|e| {
            Failed::Other(Error::Auth {
                message: format!("token store: {e}"),
                pricing: None,
            })
        })?;
        if on_disk.is_fresh(self.clock.as_ref(), EXPIRY_SKEW)
            && snap.rejected.as_deref() != Some(on_disk.access_token())
        {
            return Ok(Fetched::Adopted(on_disk));
        }

        // 7. 저장 실패 뒤의 회전 금지.
        if let Some(why) = snap.refresh_forbidden {
            let cause = match why {
                Forbid::PersistFailed => "a rotated token could not be persisted earlier",
                Forbid::Interrupted => {
                    "an earlier token refresh was interrupted and its outcome is unknown"
                }
            };
            return Err(Failed::Other(Error::Auth {
                message: format!(
                    "{cause}; refreshing again would risk invalidating the token chain \
                     (restart the process, then re-authenticate if needed)"
                ),
                pricing: None,
            }));
        }

        // 8. 회전 최소 간격.
        if let Some(last) = snap.last_refresh_at {
            let elapsed_secs = now.checked_sub(last);
            if elapsed_secs.is_none_or(|e| e < MIN_REFRESH_INTERVAL.as_secs()) {
                return Err(Failed::Other(Error::Auth {
                    message: crate::redaction::body_preview(
                        &format!(
                            "token refresh suppressed: the minimum rotation interval is {}s. \
                             The server may be reporting an expires_in inside the {}s freshness \
                             window, or the clock moved backwards.",
                            MIN_REFRESH_INTERVAL.as_secs(),
                            EXPIRY_SKEW.as_secs(),
                        ),
                        MSG_MAX,
                    ),
                    pricing: None,
                }));
            }
        }

        // 9. 회전 — 재시도하지 않습니다. 회전된 refresh 토큰은 **반환 전에** 저장합니다.
        let refreshed = self
            .refresher
            .refresh(on_disk.refresh_token())
            .map_err(Failed::Other)?;
        let lifetime = refreshed.expires_in_secs.min(MAX_TOKEN_LIFETIME.as_secs());
        let expires_at = now.checked_add(lifetime).unwrap_or(now);
        let new_set = TokenSet::from_refreshed(&refreshed, expires_at);
        if let Err(e) = self.store.store(&new_set) {
            return Err(Failed::PersistFailed(Error::Auth {
                message: format!(
                    "failed to persist the rotated token ({e}); \
                     the on-disk state is now unknown — restart the process, \
                     then re-authenticate if the token file was not updated"
                ),
                pricing: None,
            }));
        }
        Ok(Fetched::Rotated {
            set: new_set,
            at: now,
        })
    }

    /// Makes the cache stale **only while** generation `seen` **is still current**.
    ///
    /// A caller that received a 401 calls this. When it hands back, unchanged, the generation of
    /// the token it sent, the cache is emptied only if that token is still this manager's
    /// current one, and the next [`TokenManager::access_token`] takes the adoption/rotation
    /// path.
    ///
    /// # ⚠️ The generation comparison is all there is to this function
    ///
    /// Emptying unconditionally without the comparison lets concurrent 401s kill a token that
    /// was just refreshed: the 401 for a request A sent with generation N makes it discard the
    /// N+1 that B has already produced. B's resend then draws another 401, and a feedback loop
    /// opens in which that 401 invalidates yet again.
    ///
    /// If the generation has already advanced, it **does nothing and returns `Ok`**. That is not
    /// a failure but "another request has already fixed it", so the caller can simply resend
    /// with the new token.
    ///
    /// # ⚠️ No refresh happens here
    ///
    /// It only empties the cache. Rotating directly here would bypass **both** the single-flight
    /// and the [`MIN_REFRESH_INTERVAL`] gate — those two exist only on the
    /// [`TokenManager::access_token`] path.
    ///
    /// # Errors
    ///
    /// [`Error::Auth`]`{ pricing: None }` — when the in-process mutex could not be obtained
    /// within `state_deadline`. The wording is **the same** as that of
    /// [`TokenManager::access_token`] (it is the same seam, so the diagnostics must not
    /// diverge).
    pub fn invalidate_generation(&self, seen: Generation) -> Result<(), Error> {
        let mut state = self
            .state
            .lock_within(self.state_deadline)
            .map_err(|_timeout| state_lock_timeout())?;
        if state.generation == seen {
            state.cached = None;
        }
        Ok(())
    }

    /// Reports that **the server rejected** the token of generation `seen` (401 recovery only,
    /// design §5.1).
    ///
    /// The generation comparison (compare-and-…) is identical to that of
    /// [`TokenManager::invalidate_generation`], but what it does differs. Invalidation is
    /// "forget the cache"; this is "this token value is dead". That difference is the whole of
    /// 401 recovery:
    ///
    /// - **Invalidation alone does not recover.** A 401 is a server-side event and does not
    ///   bring the token's `expires_at` forward. Even with the cache emptied, the next
    ///   [`TokenManager::access_token_with_generation`] reads **the same file** at disk adoption
    ///   (step 6), adopts it because `is_fresh` is true, and resends the very bytes that were
    ///   just rejected. One whole retry is wasted (in A4, cost and rate limit are wasted too)
    ///   and the caller does not recover until the process is restarted.
    /// - So the value is kept in the manager's internal state (`State::rejected_access_token`).
    ///   **Both** the cache hit and disk adoption avoid that value, so the next call passes by
    ///   `is_fresh` and goes all the way to the refresh path — exactly **once** (after a
    ///   successful rotation the new token value differs and is not caught by the gate).
    ///
    /// # ⚠️ Nothing at all is bypassed
    ///
    /// No refresh is called here. All that is forced is "the next call cannot escape through the
    /// cache or adoption"; the actual rotation still happens only inside the in-process mutex +
    /// file lock single-flight, after passing the
    /// [`MIN_REFRESH_INTERVAL`](crate::MIN_REFRESH_INTERVAL) gate (step 8) and the
    /// persist-failure lockout (`State::refresh_forbidden`, step 7) unchanged. If the interval
    /// blocks it, the rejection mark **remains**, and the next call after the interval has
    /// passed rotates.
    ///
    /// # ⚠️ The generation comparison is here for the same reason as in [`TokenManager::invalidate_generation`]
    ///
    /// If the generation has already advanced (a sibling has already rotated), it **does nothing
    /// and returns `Ok`**. Rejecting without the comparison would let a late-arriving 401 put a
    /// perfectly good, just-created token on the blacklist and kill, in a chain, the sibling
    /// requests using that token as well.
    ///
    /// If the generation matches but the cache is already empty (no path in this crate produces
    /// that combination — the only place that empties the cache is `invalidate_generation`, and
    /// 401 recovery calls only this one), there is no value to keep, so it is a no-op. The
    /// identity of a token to reject cannot be made up.
    ///
    /// # Errors
    ///
    /// [`Error::Auth`]`{ pricing: None }` — when the in-process mutex could not be obtained
    /// within `state_deadline` (the wording is the same as [`TokenManager::access_token`]).
    pub fn reject_generation(&self, seen: Generation) -> Result<(), Error> {
        let mut state = self
            .state
            .lock_within(self.state_deadline)
            .map_err(|_timeout| state_lock_timeout())?;
        if state.generation == seen
            && let Some(set) = state.cached.as_ref()
        {
            let rejected = set.access_token().to_owned();
            state.rejected_access_token = Some(rejected);
        }
        Ok(())
    }
}

/// The error for when the wait on the in-process mutex exceeded `state_deadline`.
///
/// [`TokenManager::access_token`] and [`TokenManager::invalidate_generation`] wait on the
/// **same** seam, so there must be a single wording too — a bug report usually carries only this
/// one line, and if the two places diverged it would be impossible to tell which one it came
/// from. The prefix is **different** from the file lock's (`token lock:`) (REQ-15 AC3).
fn state_lock_timeout() -> Error {
    Error::Auth {
        message: "token state lock: timed out waiting for this process's \
                  refresh mutex (another caller in this process is holding \
                  it across a refresh)"
            .to_owned(),
        pricing: None,
    }
}

#[cfg(test)]
mod tests;

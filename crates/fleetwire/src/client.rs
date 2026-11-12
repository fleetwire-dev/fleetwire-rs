//! The Fleet API HTTP client: request dispatch, tied to a [`crate::TokenManager`].
//!
//! **The region trust boundary lives here.** `GetRegion`'s response carries
//! a server-supplied `fleet_api_base_url`, and this client sends **Bearer
//! tokens** to whatever it adopts as its base URL. A tampered response, a
//! server malfunction or a poisoned DNS answer therefore turns an
//! unvalidated adoption into **credential exfiltration** — not "a bad
//! request". [`validate_region_url`] is the gate every server-supplied URL
//! must pass *before* it can become this client's base (design §4.5), and a
//! rejection keeps the bootstrap host ([`ClientBuilder::bootstrap_region`] /
//! [`NA_BOOTSTRAP`]).

use std::time::Duration;

use serde::de::DeserializeOwned;

use crate::api::users::{GetRegion, RegionInfo};
use crate::api::vehicles::GetVehicle;
use crate::endpoint::{FleetGet, VehicleEndpoint};
use crate::pricing::Pricing;
use crate::query::QuerySpec;
use crate::redaction::body_preview;
use crate::{Error, TokenManager};

/// Error-message length bound for this module's own errors (client
/// construction). Same budget as `decode`/`transport` — one crate-wide
/// message size contract, not a value picked fresh per module.
const MSG_MAX: usize = 512;

/// Per-request timeout applied when a caller never calls
/// [`ClientBuilder::request_timeout`] (design §9: connect 10s / request 30s).
///
/// This has to be generous, not tight: design §9.2 makes
/// [`crate::error::Error::TransportUnknown`] **never retryable** (it is the
/// duplicate-billing guard — a request that may already have reached the
/// server must not be resent). A short cap does not make a slow call safer,
/// it just converts a slow-but-successful call into an *unrecoverable*
/// Unknown, and that risk lands hardest on the billed `vehicle_data`
/// endpoint. Timing out early is not the safe direction here, so 30s errs
/// long. Fix round 1 (Important 1): the previous 10s value was justified as
/// "matches `crate::auth`'s file-lock deadline" — that equality was
/// coincidental (`send`'s token wait and its GET are sequential, so the two
/// budgets never compose), not a real invariant, and is not carried forward.
/// It is also **not** tuned for a vehicle wake — a firmware wake can
/// legitimately take longer than this, and
/// [`crate::error::Error::VehicleOffline`] (HTTP 408) is the expected,
/// non-error-shaped outcome for that case regardless of how long the timeout
/// is set. Callers with different needs should set an explicit value.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

// 연결 마감(10초)과 유휴 연결 수(호스트당 2)는 `net::agent` 한 곳에서 정합니다(design §9).

/// Retry is **opt-in** (design §9.0/§11 non-goal) — the default is exactly
/// one attempt, i.e. no retry. See [`Client::send`] and
/// [`ClientBuilder::max_attempts`] for why this must not change.
const DEFAULT_MAX_ATTEMPTS: u32 = 1;

/// Wall-clock ceiling on the *whole* opt-in retry chain — every attempt plus
/// every backoff sleep between them — when
/// [`ClientBuilder::max_attempts`] is greater than 1 (design §9: "total chain
/// deadline (only when opted in) 60s"). Irrelevant at the default of one attempt.
const DEFAULT_RETRY_DEADLINE: Duration = Duration::from_secs(60);

/// Starting delay for opt-in exponential backoff, before [`BACKOFF_MAX`]
/// caps it and jitter shrinks it (design §9: "exponential + jitter, cap
/// 30s" — the design fixes the cap, not the start; a short start keeps a
/// caller's opted-in retry chain cheap by default, since no evidence favours
/// a specific starting value over another).
const BACKOFF_INITIAL: Duration = Duration::from_millis(100);

/// Ceiling on any single opt-in backoff delay (design §9).
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// North America Fleet API bootstrap host.
///
/// This is a fixed *starting point* for the very first request a fresh
/// client makes (in particular `GetRegion`), not a claim about where a given
/// account's data actually lives — an EU account still resolves its real
/// region via `GetRegion`. It is the default when
/// [`ClientBuilder::bootstrap_region`] is never called.
pub const NA_BOOTSTRAP: &str = "https://fleet-api.prd.na.vn.cloud.tesla.com";

/// The Fleet API hosts a server-supplied `fleet_api_base_url` may name.
///
/// This is an **exact host set**, deliberately not a suffix rule: a
/// `host.ends_with("tesla.com")` test is defeated by
/// `tesla.com.attacker.net` *and* by `nottesla.com`, and a suffix test
/// against a full allowlist entry is defeated by
/// `evil-fleet-api.prd.na.vn.cloud.tesla.com`. All three are hosts an
/// attacker can register, and all three would receive a live Bearer token.
///
/// **The China region host is not here.** The design listed
/// `fleet-api.prd.cn.vn.cloud.tesla.cn` as an unverified guess (design O11)
/// and no measurement backs it, so it stays out: the default is **closed**,
/// and a permissive default whose failure mode is credential leakage is not
/// a convenience worth buying. A caller who needs it adds it explicitly with
/// [`ClientBuilder::allow_region_host`].
///
/// This list breaks if Tesla adds a region — that is the intended
/// direction of failure. The symptom is a 412 that recovery cannot fix, not
/// a token sent somewhere new.
pub const TESLA_REGION_HOSTS: &[&str] = &[
    "fleet-api.prd.na.vn.cloud.tesla.com",
    "fleet-api.prd.eu.vn.cloud.tesla.com",
];

/// Extra Fleet requests [`Client::send`] may spend recovering from a 401 and
/// a 412 — **one each, ever, for the whole call** — on top of whatever
/// [`ClientBuilder::max_attempts`] budgets for opt-in retry.
///
/// # This budget buys **no** extra request for a paid endpoint
///
/// A5c-2a T23 fix round 2. Everything below is written about *requests*, and
/// its whole framing is money ("turns into real money the moment A4 adds a
/// billed endpoint") — so the paid answer has to come first, because it is no
/// longer the same number:
///
/// - [`Pricing::Data`] — the two recovery arms run their **side effects**
///   (`reject_generation`, `invalidate_region` + re-resolve) but never re-send
///   `req`. A 401 and a 412 are both answered responses that `Error::billing`
///   calls `Billed`, so re-sending is a second charge. **`RECOVERY_BUDGET`
///   therefore contributes `0` to a paid request's count**: the ceiling is
///   `max_attempts`, which at the default is 1.
/// - [`Pricing::NotCharged`] — the arithmetic below applies unchanged.
///
/// **`max_attempts` is the remaining variable, and it is not overridden
/// here** (fix round 3). An earlier draft of this paragraph said the paid
/// ceiling was "**1**, whatever `max_attempts` says". That is false, and it is
/// the same shape of error this task kept finding elsewhere — an equals sign
/// where a bound belongs. Measured (PROBE-E: paid `vehicle_data`, always 503,
/// `.max_attempts(3)`): **3 requests**, `billing == NotBilled`. Opting in
/// really does multiply a paid request.
///
/// **Why that is not a hole in the same rule.** [`Error::is_retryable`]
/// admits exactly `TransportNotSent` and `500 | 502 | 503 | 504` — and design
/// §9.2's table judges **every one of those `NotBilled`** (a ≥500 on a paid
/// endpoint is not charged; a provably-unsent transport failure is not
/// charged). The recoveries had to be suppressed precisely because a 401 and a
/// 412 are `Billed`. So opt-in retry does not contradict the ruling; it
/// spends unbilled requests, which is the caller's explicit choice. Measured
/// control, same probe with a 412 instead of a 503 and the same
/// `.max_attempts(3)`: **1 request**, `billing == Billed`, `is_retryable ==
/// false`.
///
/// It is not free of *all* cost — a retried paid 5xx still draws on the
/// shared `device_data` quota, and Tesla's actual meter is not contractually
/// bound to §9.2's model. `fleetwire-capture` never opts in
/// (`scripts/check-wiring.sh` G5 forbids it); other callers should weigh it.
///
/// The recovery still costs **unbilled** traffic on the paid path: the 412
/// arm's re-resolve is one `users/region` call. That is what makes the next
/// call succeed, and it is free.
///
/// Fix round 1 (Important 1): the first cut of T8 gave every opt-in retry
/// attempt its **own** fresh 401/412 budget, which made the per-call ceiling
/// `max_attempts * (1 + RECOVERY_BUDGET)` — a real departure from design
/// §7's headline invariant, and one that turns into real money the moment A4
/// adds a billed endpoint: `max_attempts(3)` could spend up to 9 Fleet
/// requests on what the caller believed was "retry up to 3 times". A 401 is
/// a statement about the token, not about which attempt hit it — if
/// refreshing once didn't fix it, refreshing again on attempt 2 won't
/// either — so [`Client::send`] now holds `auth_retry_used`/
/// `region_retry_used` for its **entire** call, shared across every opt-in
/// retry attempt, and this constant is added to `max_attempts` **once**,
/// not multiplied by it: [`Client::send`]'s hard per-call ceiling is
/// `max_attempts + RECOVERY_BUDGET` Fleet requests **for an unpaid request**.
/// At the default (`max_attempts == 1`) that reproduces the pre-T8 bound
/// exactly: `1 + 2 = 3` (original + 401 retry + 412 retry). For a paid
/// request this constant adds **nothing**, so the ceiling is `max_attempts`
/// alone — 1 at the default, 3 if the caller opted into 3. Per the section
/// above, the `3` on this line is not the number to reason about *spend*
/// with.
///
/// The primary bound is still the pair of one-shot recovery flags; this
/// constant, added to `max_attempts`, is the **structural backstop** behind
/// them — neither recovery path is self-limiting on its own (adopting a
/// sibling process's token bumps the token generation, so every 401
/// recovery round produces a generation the *next* 401 would match, and
/// every 412 recovery round produces a freshly resolved region the next 412
/// would invalidate), so if a future edit weakens a flag, this turns what
/// would otherwise be an unbounded request loop into a bounded,
/// test-visible failure.
const RECOVERY_BUDGET: u32 = 2;

/// The resolved region base URL, with a generation counter.
///
/// **Not a `OnceCell`.** Design §4.3 requires the region to be
/// *invalidated* when a 412 says this account does not belong to the host it
/// was sent to, and a `OnceCell` cannot be invalidated — the draft design
/// contradicted itself here and cross-review caught it.
///
/// The generation exists for the same reason [`crate::Generation`] does on
/// the token cache: invalidation is **compare-and-clear**, so request A's
/// late-arriving 412 cannot wipe the region request B resolved in the
/// meantime. It lives *outside* the `Option` (rather than the design's
/// sketched `RwLock<Option<(u64, Url)>>`) precisely so an empty cache still
/// has a generation — otherwise "no value yet" would have no version to
/// compare against and the very first invalidation race would be undefined.
#[derive(Default)]
struct RegionCache {
    inner: std::sync::RwLock<RegionState>,
}

impl RegionCache {
    /// Takes the read lock. If the lock is poisoned, the cache is first cleared through the
    /// write side and then read (spec §0 M4).
    fn read(&self) -> std::sync::RwLockReadGuard<'_, RegionState> {
        if self.inner.is_poisoned() {
            drop(self.write());
        }
        self.inner
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Takes the write lock. If the lock is poisoned, the region URL, which may be
    /// half-changed, is discarded (a path without panics).
    fn write(&self) -> std::sync::RwLockWriteGuard<'_, RegionState> {
        match self.inner.write() {
            Ok(g) => g,
            Err(p) => {
                let mut g = p.into_inner();
                g.url = None;
                self.inner.clear_poison();
                g
            }
        }
    }
}

#[derive(Default)]
struct RegionState {
    /// Bumped on every cache **replacement**, never on a clear — the same
    /// rule [`TokenManager`] follows, so the two caches cannot drift into
    /// two different meanings of "generation".
    generation: u64,
    /// `None` until a server-supplied URL has passed
    /// [`validate_region_url`]. A failed resolution is never cached.
    url: Option<crate::Url>,
}

/// A Fleet API client bound to one [`TokenManager`].
///
/// Cheap to hold (owns a `ureq::Agent`, which is itself an `Arc`-backed
/// connection-pool handle) but **not** `Clone` — construct one per
/// long-lived caller via [`Client::builder`].
pub struct Client {
    agent: ureq::Agent,
    tokens: TokenManager,
    /// Where requests go until a region has been resolved and validated, and
    /// where they go back to if resolution fails or is refused.
    bootstrap: crate::Url,
    request_timeout: Duration,
    /// Total attempts [`Client::send`] may make of one logical request
    /// before giving up on a retryable failure (design §9.0). `1` (the
    /// default, [`DEFAULT_MAX_ATTEMPTS`]) means no retry — see
    /// [`ClientBuilder::max_attempts`].
    max_attempts: u32,
    /// Wall-clock budget for the *whole* opt-in retry chain, backoff sleeps
    /// included. Only consulted when `max_attempts > 1` — see
    /// [`ClientBuilder::retry_deadline`].
    retry_deadline: Duration,
    /// Exact hosts a server-supplied region URL may name.
    /// [`TESLA_REGION_HOSTS`] plus anything
    /// [`ClientBuilder::allow_region_host`] added.
    allowed_region_hosts: Vec<String>,
    /// Origins accepted verbatim — **tests only**, and `#[cfg(test)]` so it
    /// cannot exist in a consumer build. See
    /// [`ClientBuilder::allow_region_origin_for_tests`].
    #[cfg(test)]
    allowed_region_origins: Vec<TestOrigin>,
    region: RegionCache,
    /// **Test-only.** The number of GETs this client **tried to put** on the wire.
    /// It is incremented immediately before `execute_get`.
    ///
    /// # What it is for — and what it is **not**
    ///
    /// It measures the retry guard ("at the default, a second one is not sent") at **the
    /// boundary this crate actually owns**. The number of requests the server received
    /// (wiremock's `received_requests()`) is not this crate's behaviour — a failure can
    /// only be diagnosed if that difference can be told apart.
    ///
    /// **The "transport-layer retransmission" claim written down as the rationale when
    /// this field was first introduced is withdrawn** (A5c-2a T20 fix round 1). During the
    /// T20 implementation it was reported that `[region, vehicle_data, region, vehicle_data]`
    /// had been observed under workspace load, but the reviewer made **about 2 000
    /// observations** of three shapes (`GetRegion` 60 times × 23 runs, `GetVehicleData` 30
    /// times, and the original flaky test as it was 40 times × 18 runs) under
    /// `--test-threads=32` and CPU saturation, and there was **0 divergence**. It does not
    /// reproduce. That the 12 sibling tests still count `received_requests()` and are all
    /// green points to the same conclusion.
    ///
    /// **The only code path that produces that sequence is the 412 arm**
    /// (`invalidate_region` → `resolve_region` → `continue`) — and that arm increments this
    /// counter too. **T23 diagnostic guidance**: if `expect(1)` breaks, look at this value
    /// first. If it is 2, it is the 412 recovery, not the transport layer. **Not comparing**
    /// the two numbers was why T20 missed the diagnosis.
    #[cfg(test)]
    dispatches: std::sync::atomic::AtomicU32,
}

/// One `#[cfg(test)]` allowed origin, compared field by field.
///
/// **Not** `Url::origin().ascii_serialization()`. That serializes *every*
/// opaque origin to the literal string `"null"`, so registering a single
/// non-special-scheme URL would make every opaque-origin URL compare equal
/// to it. Nothing today can reach that (the hatch is test-only and reqwest
/// would refuse the request anyway), but a security-adjacent comparison
/// whose "not equal" case can collapse into "equal" is the wrong shape to
/// leave lying around — fix round 1, review Minor 3.
#[cfg(test)]
#[derive(PartialEq, Eq)]
struct TestOrigin {
    scheme: String,
    /// `Some` for every URL that has a host; a registered origin always
    /// does, so an incoming host-less URL can never match one.
    host: Option<String>,
    port: Option<u16>,
}

#[cfg(test)]
impl TestOrigin {
    fn of(url: &crate::Url) -> Self {
        Self {
            scheme: url.scheme().to_owned(),
            host: url.host_str().map(str::to_owned),
            port: url.port(),
        }
    }
}

/// Builder for [`Client`].
pub struct ClientBuilder {
    tokens: TokenManager,
    bootstrap: Option<crate::Url>,
    request_timeout: Duration,
    max_attempts: u32,
    retry_deadline: Duration,
    allowed_region_hosts: Vec<String>,
    #[cfg(test)]
    allowed_region_origins: Vec<TestOrigin>,
}

/// The outcome of [`Client::send_guarded`].
///
/// `#[non_exhaustive]`: a future refinement of the gate (e.g. a third
/// outcome for "probe said online but the paid call itself was skipped for
/// some other reason") must not be a breaking change for code that already
/// matches on this today.
#[non_exhaustive]
pub enum Guarded<T> {
    /// The gate observed the vehicle as `online`, so the paid call in
    /// [`Client::send_guarded`] was actually made and this is its response.
    Executed(T),
    /// The gate observed a `state` other than `online`, so
    /// [`Client::send_guarded`] **did not make the paid call** — that is the
    /// entire reason this method exists. Carries the observed state
    /// verbatim.
    Skipped {
        /// The vehicle's `state` as reported by the free probe (e.g.
        /// `"asleep"`, `"offline"`) at the moment of the gate check. Never
        /// `"online"` — that value takes the `Executed` arm instead. See
        /// [`Client::send_guarded`]'s doc for why this is a plain `String`
        /// rather than an enum.
        ///
        /// Can also be `""`: the probe's `VehicleSummary.state` is itself
        /// an `Option`, and this field defaults an absent state to the empty
        /// string rather than propagating an `Option` here. This is the
        /// **safe** direction — an unrecognized (including empty) state
        /// skips the paid call exactly like any other non-`"online"` value —
        /// but it means `""` is a real, reachable value of this field, not
        /// only Tesla's named connectivity words.
        state: String,
    },
}

/// Hand-written — **does not require, and does not print, `T`.**
///
/// A `#[derive(Debug)]` here would add a `T: Debug` bound to every caller of
/// `send_guarded` (most of this crate's response types deliberately do not
/// implement `Debug` at all, REQ-18 / `model.rs`'s module doc — a derive
/// would either fail to compile for them or, if it did compile, print a
/// response body verbatim into the first log line or panic message that
/// touched an `Executed` value). The gate's own free probe response already
/// carries a `backseat_token` field (measured 2026-08-03, pinned by the
/// `FIXTURE_VEHICLE_STATE_OFFLINE` test fixture) — this impl never gives
/// either variant a path to print that or any other response content.
/// Only the variant name is printed, and for `Skipped`, the observed `state`
/// string (which this gate only ever sets to a small, server-controlled
/// connectivity word, not free-form response content).
impl<T> core::fmt::Debug for Guarded<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Executed(_) => f.write_str("Guarded::Executed(..)"),
            Self::Skipped { state } => f
                .debug_struct("Guarded::Skipped")
                .field("state", state)
                .finish(),
        }
    }
}

/// Tesla's request quota, as reported by one response's headers.
///
/// Measured 2026-08-03 against the live API: `device_data` is metered
/// **per vehicle** at `limit = 60`, `remaining` decrements by 1 per call, and
/// **a 408 (asleep vehicle) consumes a slot too** — three consecutive calls
/// showed `remaining` step 59 → 58 → 57 and `reset` step 57 → 56 → 55 in
/// lockstep. Unmetered endpoints (e.g. [`GetRegion`]) send none of these
/// headers at all, which is why [`Metered::rate_limit`] is an `Option`.
///
/// **Observation only — this crate never throttles automatically.** The
/// quota is shared across every app on the account, so `remaining` here
/// cannot see what other callers have already spent; sleeping on our own
/// view of it would be sleeping blind. An in-flight wait would also break
/// the caller's own latency expectations for a single request. If you want
/// to back off, that decision belongs to the caller, informed by this value.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimit {
    /// The `ratelimit-device-data-per-vehicle-limit` header — the quota's
    /// ceiling (measured 2026-08-03: `60`, per vehicle).
    pub limit: u64,
    /// The `ratelimit-device-data-per-vehicle-remaining` header — slots left
    /// before Tesla starts rejecting `device_data` calls for this vehicle.
    pub remaining: u64,
    /// The `ratelimit-device-data-per-vehicle-reset` header's **raw** value.
    ///
    /// Not a [`Duration`](std::time::Duration), and not named
    /// `reset_secs` — the unit is an assumption, not a measurement. The
    /// 2026-08-03 measurement only watched this value decrease
    /// (57 → 56 → 55) across three calls a few seconds apart; it did not
    /// observe what happens at the boundary (does it wrap to `limit` at 0?
    /// reset to some larger value? is the window fixed, sliding, or a token
    /// bucket?). Treat this as an opaque counter until that is measured.
    pub reset_raw: u64,
}

/// A response paired with the request quota it reported.
///
/// **Why this exists instead of a `Client::last_rate_limit()` global
/// slot.** The quota is **per vehicle**, not per `Client`. A single slot on
/// `Client` would be overwritten by whichever VIN's call happened to land
/// last — wrong the moment a caller holds more than one vehicle, issues
/// concurrent calls, or clones the client — and a subsequent unmetered call
/// would wipe a still-useful value with `None`. Attaching the value to the
/// exact response that produced it is the only way attribution stays
/// unambiguous.
#[non_exhaustive]
pub struct Metered<T> {
    /// The decoded response.
    ///
    /// For [`Client::send_metered`] this is identical to what [`Client::send`]
    /// would have returned for the same request. Not so for
    /// [`Client::send_guarded`], whose `T` is a [`Guarded<_>`] — `send` never
    /// returns one, because it never gates.
    pub value: T,
    /// `None` when the response carried no `ratelimit-*` headers at all
    /// (measured 2026-08-03: every unmetered endpoint) — never a fabricated
    /// value assembled from a partial header set. See
    /// [`crate::transport::RawResponse::rate_limit`] for the parsing rule.
    pub rate_limit: Option<RateLimit>,
}

impl Client {
    /// Starts building a client around `tokens`.
    #[must_use]
    pub fn builder(tokens: TokenManager) -> ClientBuilder {
        ClientBuilder {
            tokens,
            bootstrap: None,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            retry_deadline: DEFAULT_RETRY_DEADLINE,
            allowed_region_hosts: TESLA_REGION_HOSTS.iter().map(|h| (*h).to_owned()).collect(),
            #[cfg(test)]
            allowed_region_origins: Vec::new(),
        }
    }

    /// The base URL the next request goes to, together with the region
    /// generation it was read at.
    ///
    /// The two are read under **one** guard: a caller that later invalidates
    /// on a 412 must present the generation that produced the URL it
    /// actually used, and reading them separately would let the pair drift.
    fn current_base(&self) -> (u64, crate::Url) {
        let state = self.region.read();
        (
            state.generation,
            state.url.clone().unwrap_or_else(|| self.bootstrap.clone()),
        )
    }

    /// The current region-cache generation. Test-only accessor — not `pub`,
    /// and `#[cfg(test)]`-gated rather than `#[allow(dead_code)]`-suppressed
    /// because production code reads the generation only as half of
    /// [`Client::current_base`]'s pair.
    #[cfg(test)]
    pub(crate) fn region_generation(&self) -> u64 {
        self.region.read().generation
    }

    /// Resolves this account's region and caches it. A populated cache is
    /// returned as-is (no request).
    ///
    /// The resolution request always goes to the **bootstrap** host (design
    /// §4.2): asking the host we are trying to replace would be circular.
    /// The response's `fleet_api_base_url` is adopted only if it passes
    /// [`validate_region_url`] — a refusal is an error and the cache stays
    /// empty, so requests keep going to the bootstrap host (design §4.2 step
    /// 3). Failures are never cached.
    ///
    /// Two concurrent callers may both issue the resolution request; the
    /// loser adopts the winner's value instead of overwriting it (so the
    /// generation moves once, not twice). This is deliberately *not*
    /// single-flighted: `GET /api/1/users/region` is unbilled (O1) and the
    /// duplicate is bounded by [`Client::send`]'s per-call request ceiling,
    /// so the machinery would buy nothing a token refresh's single-flight
    /// buys.
    ///
    /// # Errors
    ///
    /// [`Error::Auth`] if the token layer fails, if the response carries no
    /// `fleet_api_base_url`, or if the URL it carries fails the trust
    /// boundary; transport/decode errors from the resolution request itself
    /// propagate unchanged.
    ///
    /// This function does not check the token scopes itself; it relies on the command-scope
    /// pre-check of its caller (`send`).
    pub(crate) fn resolve_region(&self) -> Result<crate::Url, Error> {
        if let Some(url) = self.region.read().url.clone() {
            return Ok(url);
        }

        // No guard is held across this await — `current_base`/`invalidate`
        // callers must not block on a network round trip.
        let (_gen, token) = self.tokens.access_token_with_generation()?;
        let url = build_url(&self.bootstrap, &GetRegion.path(), &GetRegion.query());
        let raw = crate::transport::execute_get(
            &self.agent,
            url,
            &token,
            GetRegion.pricing(),
            self.request_timeout,
        )?;
        let info: RegionInfo = crate::decode::decode(
            raw.status,
            raw.content_type.as_deref(),
            &raw.body,
            GetRegion.pricing(),
            raw.retry_after,
            raw.www_authenticate.as_deref(),
        )?
        .payload;
        let raw_url = info
            .fleet_api_base_url
            .ok_or_else(|| region_refusal("the region response carried no fleet_api_base_url"))?;
        let validated = self.adopt_region_url(&raw_url)?;

        let mut state = self.region.write();
        if let Some(existing) = state.url.clone() {
            // A sibling resolved while we were on the wire. Keep theirs —
            // replacing it would bump the generation a second time for no
            // change, and hand a stale-generation invalidation a window.
            return Ok(existing);
        }
        state.generation = state.generation.saturating_add(1);
        state.url = Some(validated.clone());
        Ok(validated)
    }

    /// Clears the region cache **only if** `seen` is still the current
    /// generation (compare-and-clear).
    ///
    /// Unconditional clearing is the bug this guards against: request A's
    /// 412 arrives late, after request B already resolved a *correct*
    /// region, and an unconditional clear would throw B's answer away and
    /// send everyone back through resolution — repeatedly, under load. Same
    /// problem and same fix as [`TokenManager::invalidate_generation`].
    ///
    /// Clearing does not bump the generation; only a replacement does (see
    /// [`RegionState::generation`]).
    pub(crate) fn invalidate_region(&self, seen: u64) {
        let mut state = self.region.write();
        if state.generation == seen {
            state.url = None;
        }
    }

    /// Applies this client's region trust boundary to `raw`.
    ///
    /// Production behaviour is exactly [`validate_region_url`] against this
    /// client's host allowlist. The `#[cfg(test)]` branch additionally
    /// accepts a full origin registered by
    /// [`ClientBuilder::allow_region_origin_for_tests`] — see that method
    /// for why that hatch exists and why it cannot ship.
    fn adopt_region_url(&self, raw: &str) -> Result<crate::Url, Error> {
        #[cfg(test)]
        if let Ok(url) = crate::Url::parse(raw)
            && {
                let origin = TestOrigin::of(&url);
                self.allowed_region_origins.contains(&origin)
            }
            && check_region_url_shape(&url).is_ok()
        {
            return Ok(url);
        }
        validate_region_url(raw, &self.allowed_region_hosts)
    }

    /// How many GETs this client has **dispatched** — test-only, see the
    /// [`Client::dispatches`] field's doc for why the count is taken here and
    /// not from the server's request log.
    #[cfg(test)]
    pub(crate) fn dispatches(&self) -> u32 {
        self.dispatches.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Test-only accessor to the underlying [`TokenManager`] — not `pub`.
    /// `#[cfg(test)]`-gated rather than `#[allow(dead_code)]`-suppressed
    /// because it genuinely has no production caller: `send` reaches the
    /// token manager directly (`self.tokens`), never through this accessor.
    #[cfg(test)]
    pub(crate) fn tokens(&self) -> &TokenManager {
        &self.tokens
    }

    /// Sends one GET request and decodes its envelope-unwrapped body as
    /// `R::Response`, recovering once from a 401 and once from a 412, and —
    /// only when the caller has opted in via
    /// [`ClientBuilder::max_attempts`] — retrying a transient failure with
    /// backoff.
    ///
    /// The response type is **not** a free type parameter: it is
    /// [`FleetGet::Response`], bound by the endpoint (final fix wave, Minor
    /// 5). The earlier `send<R, T>` let `send::<GetRegion, VehicleSummary>`
    /// compile and fail at runtime as a decode error, which is exactly what
    /// the associated type exists to prevent.
    ///
    /// # The two recovery paths (design §4.3/§5.1) — unconditional, always on
    ///
    /// **401 → token (design §5.1).** The generation the request was sent
    /// with is handed to [`TokenManager::reject_generation`], which is
    /// itself a compare-and-reject, so one call expresses both of §5.1's
    /// branches with no window between the comparison and the decision:
    /// - the generation **moved** (a sibling request already refreshed) —
    ///   the call is a no-op, and the retry picks up the sibling's token
    ///   **without refreshing**. Refreshing here is the bug §5.1 exists to
    ///   prevent: it would discard a token that is known-good.
    /// - the generation still **matches** — the token value is recorded as
    ///   rejected, so the retry's `access_token_with_generation` call cannot
    ///   satisfy itself from the cache **or** from the identical on-disk
    ///   copy, and rotates (or adopts a sibling process's *different*
    ///   rotation) exactly once.
    ///
    /// **Why rejecting the value, not clearing the cache (final fix wave,
    /// Important 1).** A 401 is a server-side event: it does not move the
    /// token's `expires_at`. Merely clearing the in-memory cache therefore
    /// recovers nothing — the retry re-reads the same file, finds
    /// `is_fresh()` true, adopts it, and re-sends the byte-identical token
    /// that was just rejected. That is a guaranteed-useless extra Fleet
    /// request (free today, $0.002 plus a shared 60/min rate-limit slot once
    /// A4 adds a `Pricing::Data` endpoint) after which the caller never
    /// recovers without a process restart.
    /// `auth_401_forces_a_rotation_and_retries_with_a_different_token` pins
    /// the corrected behaviour on the token bytes actually put on the wire.
    ///
    /// The forced rotation bypasses nothing: it still happens inside the
    /// token manager's in-process + inter-process single flight, and it is
    /// still refused by [`crate::MIN_REFRESH_INTERVAL`] and by the
    /// unpersisted-rotation lockout. When the interval refuses it, the
    /// caller sees the **401**, not the interval error — see the error note
    /// below.
    ///
    /// Its `Err` is deliberately ignored: it can only be the in-process
    /// mutex deadline, which means another caller is mid-refresh — by the
    /// time we could have taken the lock the generation would have moved and
    /// the call would have been a no-op anyway. Treating that as fatal would
    /// convert a benign race into a failed request.
    ///
    /// **412 → region (design §4.3).** The region cache is invalidated
    /// (compare-and-clear, with the generation this request's base URL was
    /// read at), re-resolved once, and the request retried. If re-resolution
    /// fails or is refused, the original 412 is returned rather than a
    /// pointless second request to the same host.
    ///
    /// The 412 body is **never parsed to discover the right region**. The
    /// URL in that message is the region the request was *sent to*, not the
    /// one it should have gone to; `GET /api/1/users/region` is the only
    /// authority (design §4.3).
    ///
    /// **Fix round 1 (Important 1): both flags now live for this method's
    /// *entire* call, shared across every opt-in retry attempt below — they
    /// are not reset per attempt.** The first cut of T8 gave every retry
    /// attempt its own fresh 401/412 budget, which is wrong on two counts:
    /// it lets `max_attempts * RECOVERY_BUDGET` extra Fleet requests happen
    /// (real money the moment A4 adds a billed endpoint), and it is less
    /// correct, not just more expensive — a 401 is a statement about the
    /// token, not about which attempt hit it, so if refreshing once didn't
    /// fix it, refreshing again on attempt 2 won't either (same reasoning
    /// for 412/region). See [`RECOVERY_BUDGET`] for the exact resulting
    /// bound.
    ///
    /// # Region resolution happens **before** the first request
    ///
    /// Design §4.2's ordering is cache → resolve → bootstrap, and this
    /// method resolves up front when the cache is empty rather than waiting
    /// for a 412 to teach it (fix round 1 of T7, review Important 1). The
    /// asymmetry that settles it: `GET /api/1/users/region` is **always
    /// unbilled** (O1), so resolving eagerly costs an NA account one free
    /// round-trip of latency, while resolving lazily costs a non-NA account
    /// a **billed** failure on its first real call the moment A4 introduces
    /// `vehicle_data` — that 412 lands on whatever endpoint the caller
    /// invoked, not on the free region endpoint (design §4.3). Eager is the
    /// cheaper failure mode.
    ///
    /// A failed or refused pre-flight resolution is **not** an error: the
    /// request proceeds against the bootstrap host (design §4.2 step 3), and
    /// the 412 path below is still there to recover. This happens **once
    /// per call to this method**, regardless of how many opt-in retry
    /// attempts follow (fix round 1 of T8: the first cut ran this on every
    /// retry attempt, since it lived inside the per-attempt helper this
    /// method has since absorbed).
    ///
    /// [`GetRegion`] itself is exempt. It *is* the resolution primitive —
    /// dispatching it through region resolution would be circular, and would
    /// send a redundant second copy of the very request that just answered.
    /// A caller asking for the region still benefits from an already-warm
    /// cache; it just never triggers a fill.
    ///
    /// # Retry is opt-in (design §9.0) — read this before assuming it fires
    ///
    /// At the default (`max_attempts == 1`, [`DEFAULT_MAX_ATTEMPTS`]), a
    /// failure that reaches [`crate::decode::decode`] (i.e. survived any
    /// 401/412 recovery above) is returned immediately — no backoff, no
    /// deadline check, no extra Fleet request.
    /// `default_does_not_retry_even_on_retryable_5xx` pins the "no extra
    /// request" half of that claim on a status
    /// ([`Error::is_retryable`] says retry **would** be safe) deliberately
    /// chosen to tempt a future edit into treating retryable-in-principle as
    /// retry-now; `backoff_path_is_not_entered_by_default` pins the "no
    /// extra delay" half the same way.
    ///
    /// When `max_attempts > 1`, a decode failure or a transport-layer
    /// failure (before any status even exists) is retried when
    /// [`Error::is_retryable`] says so, up to `max_attempts` total decode
    /// attempts, each pair separated by an exponential-plus-jitter backoff
    /// sleep capped at [`BACKOFF_MAX`] (design §9: "exponential + jitter,
    /// cap 30s"). [`Error::TransportUnknown`] is never retried even
    /// though it is common and often actually safe to retry — see
    /// [`Error::is_retryable`]'s doc for why the line is drawn at the
    /// status/variant level rather than at "was this actually billed".
    ///
    /// ## The chain deadline covers backoff too
    ///
    /// [`ClientBuilder::retry_deadline`] (default 60s) bounds the elapsed
    /// wall-clock time of the *entire* call — every attempt plus every sleep
    /// between them — not just the network time of one attempt. It is
    /// checked before each sleep, and the sleep itself is clamped to
    /// whatever budget remains, so the chain cannot overrun the deadline by
    /// more than the in-flight attempt's own `request_timeout`. This is the
    /// gap [`ClientBuilder::request_timeout`] alone cannot close: a
    /// per-attempt timeout bounds one attempt, not a chain of several.
    ///
    /// **Deliberate deviation from design §9.2's literal text.** The
    /// design says to wrap the whole chain in a single `tokio::time::timeout`
    /// ("wrap the whole retry chain in `tokio::time::timeout`"). This method
    /// instead tracks `started.elapsed()` against `retry_deadline` and clamps
    /// each backoff sleep to whatever remains — the same idiom
    /// `auth::lock::retry_until_locked` already uses for its own deadline.
    /// Both close the gap §9.2 is actually about (a per-request timeout does
    /// not bound a multi-attempt chain); the difference is what happens at
    /// expiry. `tokio::time::timeout` cancels the in-flight future and
    /// produces an opaque `Elapsed` with no relation to what the chain was
    /// actually doing — here, that would discard a perfectly informative
    /// last error (e.g. `Error::Api` 503) in favor of a synthetic "timed
    /// out", for no functional gain, since the elapsed-check achieves the
    /// identical bound on total wall-clock time. Kept as a manual check
    /// rather than the literal `tokio::time::timeout`, not because the
    /// design was overlooked.
    ///
    /// # Request-count bound
    ///
    /// **The bound depends on `req.pricing()`, and the paid one is what
    /// costs money** (A5c-2a T23 fix round 2). Read the paid row first:
    ///
    /// | `req.pricing()` | Fleet requests for `req` itself, at the default `max_attempts == 1` |
    /// |---|---|
    /// | [`Pricing::Data`] (`vehicle_data`, `nearby_charging_sites`) | **exactly 1.** Neither recovery arm re-sends a paid request — a 401 and a 412 are both answered responses and `Error::billing` calls them `Billed`, so a second send is a second charge |
    /// | [`Pricing::NotCharged`] (everything else) | up to `max_attempts +` [`RECOVERY_BUDGET`] = **3** (original + one 401 retry + one 412 retry) |
    ///
    /// The `+ RECOVERY_BUDGET` ceiling is the structural backstop for the free
    /// row and still applies to it unchanged; see that constant's doc.
    ///
    /// **"1" is about `req`, not about the wire.** A paid call may still cost
    /// **unbilled** requests around it: up to 2 calls to
    /// [`Client::resolve_region`] (one pre-flight + one 412-recovery
    /// re-resolve — the recovery still happens for paid requests, it just does
    /// not re-fire `req`) and at most 1 token refresh. Those are
    /// [`Pricing::NotCharged`] and are what lets the *next* call succeed.
    ///
    /// For the free row the same two region calls and one refresh apply.
    ///
    /// Not included in that bound (T9, design §10.1 step 1): the scope
    /// advisory pre-check against `req.required_scopes()`, which runs
    /// **before** region resolution and can return
    /// [`Error::MissingScope`] having made zero Fleet requests at all. It
    /// costs at most one more call to
    /// [`TokenManager::access_token_with_generation`] than the loop below
    /// already makes on its first iteration — a cache hit in every case
    /// this method's own tests construct, since the token was just fetched
    /// (or refreshed) a moment earlier.
    ///
    /// # Errors
    ///
    /// The last attempt's error, unchanged, once 401/412 recovery and any
    /// opted-in retries are exhausted, the failure is not
    /// [`Error::is_retryable`], or the chain deadline has elapsed.
    /// Propagates [`TokenManager::access_token_with_generation`]'s errors
    /// (token directory/refresh failures) directly — those never reach the
    /// retry decision at all.
    ///
    /// **One exception, and it is about money (final fix wave, Important
    /// 2): a token-layer failure on a *later* iteration does not overwrite
    /// the error of the most recent actual attempt.** Iteration 1 may have
    /// received a real, *billed* 401 or 412; if iteration 2 then fails
    /// inside the token layer (typically
    /// [`crate::MIN_REFRESH_INTERVAL`] refusing the forced rotation), that
    /// error is `Error::Auth { pricing: None }` — [`Error::billing`]
    /// `== NotBilled`. Returning it would under-report spend and would tell
    /// the caller "token refresh suppressed" for what was really an
    /// authentication rejection by the server. The retained prior error is
    /// returned instead, pricing and message intact;
    /// `a_suppressed_recovery_refresh_still_reports_the_401s_billing` pins
    /// it. The token-layer error is only surfaced when there is no prior
    /// response error — i.e. on the first iteration.
    pub fn send<R: FleetGet>(&self, req: &R) -> Result<R::Response, Error> {
        // Thin wrapper — the body lives in `send_inner` exactly once, same
        // shape as `TokenManager::access_token`/`access_token_with_generation`
        // (A3 T5). `send_metered` is the other caller; it keeps the second
        // element this one discards.
        self.send_inner::<R, Typed<R>>(req)
            .map(|(value, _rate_limit)| value)
    }

    /// [`Client::send`], [`Client::send_metered`] and [`Client::send_raw`]
    /// share this implementation so the retry/recovery/scope-precheck
    /// contract [`Client::send`] documents exists exactly once. Not `pub` —
    /// callers use one of the three thin wrappers.
    ///
    /// **`S` is the only thing the three wrappers vary.** Everything that
    /// costs money or protects a token — the scope pre-check, region
    /// resolution and its host validation, the 401/412 recovery budget, the
    /// retry policy (`max_attempts`, default 1 = *no retry*), the transport's
    /// redirect refusal and body-size ceiling — lives here and cannot be
    /// bypassed by choosing a different `S`. A wrapper that wanted different
    /// behaviour on any of those would have to duplicate this loop, which is
    /// precisely what this parameter exists to prevent.
    fn send_inner<R: FleetGet, S: ResponseShape>(
        &self,
        req: &R,
    ) -> Result<(S::Out, Option<RateLimit>), Error> {
        let started = std::time::Instant::now();
        let mut auth_retry_used = false;
        let mut region_retry_used = false;
        let mut retries_used: u32 = 0;
        let mut total_requests: u32 = 0;
        let hard_cap = self.max_attempts.saturating_add(RECOVERY_BUDGET);

        // Design §10.1 step 1 — the cheapest possible check, ahead of region
        // resolution and every other Fleet request below (0, advisory).
        // It reads the access token's `scp` claim **without verifying the
        // token's signature**, so this is not an authorization decision —
        // it exists only to avoid paying for a call the server will
        // certainly reject. The server's own 403 remains the actual
        // authority (see `Error::MissingScope`'s doc).
        //
        // `Indeterminate` and `ContainsRequiredScope` MUST take this same
        // arm (design §10, this method's own contract). A JWT-shaped opaque
        // token or an unparseable payload gives `Indeterminate`, and there
        // is no way to tell that apart from "genuinely holds the scope" —
        // treating them differently here is precisely the bug this call
        // site must not have, because it would turn an advisory optimisation
        // into a false authorization denial. Only the one arm that names
        // `DoesNotContainRequiredScope` explicitly may block; a `match` that
        // grew a second blocking arm would be a visible diff in review, not
        // a silent divergence a refactor could introduce unnoticed.
        let (_scope_check_gen, scope_check_token) = self.tokens.access_token_with_generation()?;
        // 읽기 토큰이 명령 권한을 가지면 아무것도 보내지 않습니다(코스 내비 §3 위험 B, 10-09 refresh 확장 관찰).
        // 판단 불가(`Indeterminate`)는 막지 않습니다. 이 선검사의 fail-open 입장과 같습니다.
        if let crate::auth::UnverifiedScopeObservation::ContainsRequiredScope =
            crate::auth::observe_forbidden(&scope_check_token)
        {
            return Err(Error::CommandScopeOnReadToken);
        }
        let required = req.required_scopes();
        if let crate::auth::UnverifiedScopeObservation::DoesNotContainRequiredScope =
            crate::auth::observe_scopes(&scope_check_token, &required)
        {
            // No `held` field (final fix wave, Minor 3). `observe_scopes`
            // deliberately returns a verdict, not the parsed claims (REQ-18:
            // "a verdict, not a diagnostic"), so the only value this call
            // site could put there was an empty vector — which `Display`
            // then rendered as `(held: [])`, telling a user who *does* hold
            // `vehicle_device_data` that their token carries no scopes at
            // all and inviting an unnecessary re-OAuth. The variant is
            // `#[non_exhaustive]`, so removing the field now is free and
            // re-adding it later (if a truthful source ever exists) is
            // non-breaking.
            return Err(Error::MissingScope { required });
        }

        // Design §4.2 steps 1-3, once per `send` call (see the bound above).
        // `is_none()` is re-checked inside `resolve_region`, so a sibling
        // that filled the cache in between costs nothing.
        if !req_resolves_region(req) && self.region.read().url.is_none() {
            // Failure keeps the bootstrap host — §4.2 step 3.
            let _ = self.resolve_region();
        }

        // The error of the last response that actually came back from Fleet
        // and triggered a recovery (401/412). See this method's `# Errors`
        // note: a token-layer failure on a later iteration must not erase a
        // known-billed outcome.
        let mut prior_response_error: Option<Error> = None;

        loop {
            total_requests += 1;
            let (token_gen, token) = match self.tokens.access_token_with_generation() {
                Ok(v) => v,
                // First iteration: nothing to prefer, so this *is* the
                // error. Later iterations: the 401/412 that sent us back
                // here is the more truthful answer, in message and in
                // `billing()`.
                Err(e) => return Err(prior_response_error.unwrap_or(e)),
            };
            // 요청에 실을 토큰은 반복마다 다시 가져옵니다. 401/412 복구 뒤의 토큰은 방금 갱신된 것이고,
            // 스코프가 넓어지는 때가 바로 그때이므로 매번 이 토큰으로 다시 확인한 뒤에만 보냅니다.
            if let crate::auth::UnverifiedScopeObservation::ContainsRequiredScope =
                crate::auth::observe_forbidden(&token)
            {
                return Err(Error::CommandScopeOnReadToken);
            }
            let (region_gen, base) = self.current_base();
            let url = build_url(&base, &req.path(), &req.query());
            #[cfg(test)]
            self.dispatches
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let sent = crate::transport::execute_get(
                &self.agent,
                url,
                &token,
                req.pricing(),
                self.request_timeout,
            );

            let raw = match sent {
                Ok(raw) => raw,
                Err(e) => {
                    // A transport-layer failure has no status, so 401/412
                    // recovery cannot apply — only the opt-in retry path can.
                    match self.backoff_before_retry(
                        started,
                        retries_used,
                        &e,
                        total_requests,
                        hard_cap,
                    ) {
                        Some(next) => {
                            retries_used = next;
                            // Same rule as the 401/412 arms: an error from a
                            // real attempt outranks a token-layer error on a
                            // later iteration.
                            prior_response_error = Some(e);
                            continue;
                        }
                        None => return Err(e),
                    }
                }
            };

            // Structural backstop (see `RECOVERY_BUDGET`'s doc) — neither
            // recovery path is self-limiting on its own.
            //
            // **A paid request is never re-fired to recover from a 401 or a
            // 412** (A5c-2a T23 fix round 1, controller ruling). Both arms below
            // re-send *the same request*, and on `Pricing::Data` that second
            // request is a second charge. The two recoveries are not exempt from
            // this crate's own rule — they simply predate the paid endpoints:
            //
            // - A **412** is proof the first request *reached a Fleet server and
            //   was answered*, which is exactly the shape `Error::billing` calls
            //   `Billed` (status < 500 on a paid endpoint). Re-firing after it is
            //   the duplicate charge `RECOVERY_BUDGET`'s doc already worried about
            //   in the abstract ("turns into real money the moment A4 adds a
            //   billed endpoint") — it just never got wired to the pricing flag.
            // - A **401** is the same story with `Error::Auth { pricing: Some(..) }`,
            //   which design §9.2 bills.
            //
            // **What is suppressed is the *re-send*, and nothing else**
            // (T23 fix round 2). The first cut of this rule gated arm *entry* on
            // `!paid`, which also skipped `reject_generation` and
            // `invalidate_region` — so a consumer that calls **only** paid
            // endpoints could never heal: every call reused the same stale token
            // or stale region, took another 401/412, and **each one is
            // `Billing::Billed`**. Measured (two consecutive `send`s on one
            // client, pre-narrowing): 412 → both fail, `region_res == 1`;
            // 401 → both fail, `refreshes == 0`. That is a worse money outcome
            // than the duplicate charge the rule set out to prevent, and it made
            // this very comment false — it claimed the invalidation "was still"
            // happening while the new tests pinned `refresh_calls == 0`.
            //
            // So both arms now run their **side effects** for paid requests too,
            // and only the `continue` is withheld. The caller gets the 401/412
            // unchanged and pays for exactly one request; the **next** call goes
            // out on a freshly rotated token / freshly resolved region and
            // succeeds. `a_412_…`/`a_401_on_a_paid_endpoint_is_not_re_fired`
            // assert both halves — one request per `send`, second `send` **Ok**.
            //
            // Scope: this governs the two *recovery* arms only. The opt-in
            // retry path below (`backoff_before_retry`) is already off by default
            // (`max_attempts == 1`) and is additionally forbidden to
            // `fleetwire-capture` by `scripts/check-wiring.sh` G5, so it is left
            // to the caller's explicit choice rather than overridden here.
            //
            // A paid 412 still spends one **free** `users/region` request here.
            // That is the price of healing, it is unbilled, and it is bounded by
            // `region_retry_used` to once per call.
            let paid = matches!(req.pricing(), Pricing::Data { .. });
            let may_recover = total_requests < hard_cap;
            match raw.status.as_u16() {
                401 if may_recover && !auth_retry_used => {
                    auth_retry_used = true;
                    // The side effect runs for **every** pricing. Rejecting the
                    // generation is what makes the *next* call carry a different
                    // token; withholding it from paid callers is what left them
                    // unable to heal (see the block comment above).
                    //
                    // Compare-and-reject; see this method's doc for why the
                    // error is dropped rather than propagated.
                    let _ = self.tokens.reject_generation(token_gen);
                    if !paid {
                        // Retained before the retry: this response is real and
                        // may be billed, and the retry's token layer can fail
                        // with a `pricing: None` error that would understate it.
                        prior_response_error = S::error_of(&raw, req.pricing());
                        continue;
                    }
                    // Paid: fall through and surface the 401 the server sent.
                    // It is `Billing::Billed`, and a second request would be a
                    // second charge.
                }
                412 if may_recover && !region_retry_used => {
                    region_retry_used = true;
                    // Same rule as the 401 arm: invalidate and re-resolve for
                    // every pricing, so the next call has a usable region.
                    self.invalidate_region(region_gen);
                    let resolved = self.resolve_region().is_ok();
                    if resolved && !paid {
                        // Same reasoning as the 401 arm — a 412 is also a
                        // real, potentially billed response.
                        prior_response_error = S::error_of(&raw, req.pricing());
                        continue;
                    }
                    // Resolution failed or was refused, or this request is paid —
                    // fall through and surface the 412 the server actually sent.
                }
                _ => {}
            }

            // A4 T2 / A5c-2a T20: the response shape decides what a body
            // becomes. `Typed<R>` decodes into `R::WireResponse` and
            // assembles via `FleetGet::assemble` (carrying the envelope's
            // sibling `pagination`); `Raw` keeps the body verbatim. Both go
            // through the same status/content-type gates — see
            // `crate::decode::gate`.
            match S::finish(&raw, req.pricing()) {
                Ok(value) => {
                    // A4 T10: `raw.rate_limit` is `Copy` and `raw` was only
                    // borrowed above, so this reads the same headers
                    // `execute_get` captured for this exact response.
                    let rate_limit =
                        raw.rate_limit
                            .map(|(limit, remaining, reset_raw)| RateLimit {
                                limit,
                                remaining,
                                reset_raw,
                            });
                    return Ok((value, rate_limit));
                }
                Err(e) => {
                    match self.backoff_before_retry(
                        started,
                        retries_used,
                        &e,
                        total_requests,
                        hard_cap,
                    ) {
                        Some(next) => {
                            retries_used = next;
                            // Same rule as the 401/412 arms: an error from a
                            // real attempt outranks a token-layer error on a
                            // later iteration.
                            prior_response_error = Some(e);
                            continue;
                        }
                        None => return Err(e),
                    }
                }
            }
        }
    }

    /// Same as [`Client::send`], but also returns the request quota the
    /// response reported (measured 2026-08-03: paid `device_data` calls
    /// carry `ratelimit-device-data-per-vehicle-{limit,remaining,reset}`
    /// headers; unmetered endpoints send none of them).
    ///
    /// **Success path only.** An error response can carry these headers
    /// too (the 408s measured 2026-08-03 did), but surfacing that is a
    /// separate concern from this method — [`Error`] does not carry a
    /// `RateLimit` field, so a failed call never reaches here at all.
    ///
    /// See [`RateLimit`] and [`Metered`] for why there is no
    /// `Client::last_rate_limit()` global slot instead: a single slot would be
    /// overwritten by whichever response landed last, so the value must ride
    /// on the exact response that produced it.
    ///
    /// **The quota's scope is unmeasured** — open item `O16` in this
    /// crate's module doc. The header is *named* `per-vehicle`, but the O1
    /// evidence matrix's own text reads account-shared, and this subproject's
    /// measurement (one account, one vehicle) cannot tell the two apart. An
    /// earlier draft of this paragraph asserted `per-vehicle` here while the
    /// module doc asserted account-shared; neither claim is measured, and the
    /// reason for per-response delivery above does not depend on which is
    /// true.
    ///
    /// # Errors
    ///
    /// Same as [`Client::send`].
    pub fn send_metered<R: FleetGet>(&self, req: &R) -> Result<Metered<R::Response>, Error> {
        let (value, rate_limit) = self.send_inner::<R, Typed<R>>(req)?;
        Ok(Metered { value, rate_limit })
    }

    /// Same request as [`Client::send_metered`], but hands back the response
    /// **body verbatim** as one [`serde_json::Value`] instead of this
    /// endpoint's model type.
    ///
    /// # Why this exists — the one thing typed responses cannot do
    ///
    /// This crate's model types are deliberately tolerant: every one carries a
    /// `#[serde(flatten)] extra` map, so a field Tesla **adds** is absorbed
    /// silently and a consumer never breaks (`crate::model`'s module doc).
    /// That tolerance is also a blindfold — from a decoded
    /// [`crate::model::VehicleSummary`] there is no way to see *that* a field
    /// appeared, only that something ended up in `extra`. For the six
    /// endpoints whose `WireResponse` is a model type rather than a
    /// `serde_json::Value`, the raw bytes are gone by the time `send` returns,
    /// and [`FleetGet`] is sealed, so no downstream crate can define a
    /// `Value`-shaped request to get them back either.
    ///
    /// **This method is therefore the only way to observe Fleet API schema
    /// drift without shipping a new version of this library.** It is what the
    /// `fleetwire-capture` operations tool is built on.
    ///
    /// # It returns **unscrubbed** data
    ///
    /// The value is the response exactly as Tesla sent it: VIN, GPS
    /// coordinates, odometer, addresses, charge history — whatever the
    /// endpoint carries, with none of the redaction the error paths apply.
    ///
    /// - **Do not `Debug`-print, log, or serialise it** anywhere it could be
    ///   retained. The model types in [`crate::model`] deliberately implement
    ///   no `Debug` for this reason (REQ-18); a bare `Value` has one, and that
    ///   is a footgun this doc cannot remove.
    /// - Treat the value as PII for its whole lifetime, and scrub before it
    ///   reaches disk, a log, or a repository.
    ///
    /// # It does **not** validate the body against the endpoint
    ///
    /// [`Client::send`] proves the body still fits this endpoint's type;
    /// failing that check *is* the drift signal. `send_raw` proves only that
    /// the body is syntactically valid JSON and that the response was not an
    /// error. Any schema judgement is the caller's.
    ///
    /// # What is identical to [`Client::send`] — no guard is bypassed
    ///
    /// Same code path (`send_inner`), so all of it still applies: the scope
    /// pre-check; region resolution and its host validation; the token
    /// injection, expiry refresh and generation handling; the transport's
    /// **refusal to follow redirects** (a 3xx is [`Error::Api`], never a
    /// followed hop); the body-size ceiling and the content-type check; 4xx
    /// and 5xx becoming [`Error`] rather than being handed back as data; and
    /// the 401/412 recovery budget.
    ///
    /// **Including the retry policy.** `max_attempts` defaults to 1, which
    /// *is* "no retry" — there is no separate retry path here.
    ///
    /// **That default is not what stops a paid request from being billed
    /// twice, and two earlier drafts of this paragraph said it was.** The
    /// first blamed a 408; the second moved the same claim onto a *retryable
    /// 5xx*. Both are wrong, and the code says so in one place:
    /// [`Error::is_retryable`] admits exactly `TransportNotSent` and
    /// `500 | 502 | 503 | 504`, and on a [`Pricing::Data`] endpoint
    /// [`Error::billing`] judges **every one of those**
    /// [`crate::Billing::NotBilled`] (`error.rs`'s `Api` arm: `status >= 500`
    /// → `NotBilled`). A 408 *is* billed but is not retryable at all, so
    /// raising `max_attempts` could never duplicate one either. Measured
    /// (`RECOVERY_BUDGET`'s doc, PROBE-E — paid `vehicle_data`, always 503,
    /// `.max_attempts(3)`): **3 requests, `billing == NotBilled`**.
    ///
    /// What opting in actually spends on a paid endpoint is therefore
    /// *unbilled* requests plus shared `device_data` quota — against a meter
    /// Tesla does not contractually bind to design §9.2's model. That residual
    /// uncertainty, not a known billed duplicate, is why the default stays 1
    /// and why `fleetwire-capture` never opts in.
    ///
    /// The path that really could send a **billed** request twice under the
    /// default is the 401/412 recovery budget mentioned above (a 401 and a 412
    /// are `Billed`); it is pricing-aware and re-fires nothing paid.
    ///
    /// The envelope is **not** unwrapped: the value still has its
    /// `{"response": …}` wrapper, and any sibling of `response` — `pagination`,
    /// the measured `count` (open item O13) — is present. The typed path drops
    /// unmodelled siblings, which is exactly what a drift detector must not do.
    ///
    /// # Errors
    ///
    /// Same as [`Client::send`], plus [`Error::Decode`] when the body is not
    /// valid JSON at all.
    pub fn send_raw<R: FleetGet>(&self, req: &R) -> Result<Metered<serde_json::Value>, Error> {
        let (value, rate_limit) = self.send_inner::<R, Raw>(req)?;
        Ok(Metered { value, rate_limit })
    }

    /// Decides whether [`Client::send`]'s opt-in retry chain may attempt
    /// `err` again and, if so, sleeps the backoff delay for the attempt that
    /// follows and returns the incremented retry counter.
    ///
    /// Returns `None` (and never sleeps) when: retry was never opted into or
    /// its budget is spent (`retries_used + 1 >= max_attempts`); `err` is not
    /// [`Error::is_retryable`]; the structural request ceiling
    /// (`total_requests >= hard_cap`) would be crossed; or the chain deadline
    /// has already elapsed. Factored out of [`Client::send`] purely to avoid
    /// duplicating this decision across its transport-error and
    /// decode-error call sites — it is not a public retry primitive.
    fn backoff_before_retry(
        &self,
        started: std::time::Instant,
        retries_used: u32,
        err: &Error,
        total_requests: u32,
        hard_cap: u32,
    ) -> Option<u32> {
        if retries_used + 1 >= self.max_attempts
            || !err.is_retryable()
            || total_requests >= hard_cap
        {
            return None;
        }
        let elapsed = started.elapsed();
        if elapsed >= self.retry_deadline {
            return None;
        }
        // 위 검사가 막지만 뺄셈 자체도 넘치지 않게 합니다(clippy `unchecked_time_subtraction`, 10-14).
        let remaining = self.retry_deadline.saturating_sub(elapsed);
        std::thread::sleep(backoff_delay(retries_used).min(remaining));
        Some(retries_used + 1)
    }

    /// Checks `req`'s vehicle's connectivity state with a **free** probe
    /// before making `req`'s (possibly billed) call, and skips the call
    /// entirely when the vehicle is not `online`.
    ///
    /// Measured 2026-08-03 against the live API: `GET /api/1/vehicles/{vin}`
    /// (the probe this method issues) returns no `ratelimit-*` headers, so
    /// it does not draw on the metered `device_data` quota. A sleeping
    /// vehicle, by contrast, answers `vehicle_data` with HTTP 408 while
    /// **still** consuming both the $0.002 charge and one of the 60
    /// per-vehicle quota slots (watched `remaining` step 59 → 58 → 57 across
    /// three consecutive calls). This method spends the free probe to avoid
    /// paying for that 408 when it can.
    ///
    /// **Not a strict win — and on an unpaid endpoint, a pure loss.**
    /// `R: VehicleEndpoint` admits every one of this crate's 11 vehicle
    /// endpoints, 9 of which are [`Pricing::NotCharged`]. Gating one of those
    /// nine (e.g. `send_guarded(&GetVehicle::new(vin)?)`) buys nothing — the
    /// probe and the gated call are both free, so the method just issues the
    /// same request twice — and even on the two billed endpoints
    /// ([`crate::api::vehicles::GetVehicleData`],
    /// [`crate::api::vehicles::GetNearbyChargingSites`]), whether the gate
    /// saves money overall depends entirely on how often a caller's vehicle
    /// is actually asleep, which only the caller knows. That is why this
    /// crate does not gate every call automatically, why this method is not
    /// the default way to reach a `VehicleEndpoint`, and why the gate is
    /// worth reaching for only on those two billed endpoints.
    ///
    /// **Request bound: double [`Client::send`]'s.** [`Client::send`]
    /// documents its own worst case as `max_attempts + RECOVERY_BUDGET`
    /// Fleet requests. This method makes up to two such calls — the free
    /// probe, then (if online) `req`'s own call — so its worst case is
    /// **twice** that bound: `2 * (max_attempts + RECOVERY_BUDGET)`, which is
    /// 6 requests at the default (`max_attempts == 1`). Reassuringly, at
    /// most one of those two halves is ever billed (the probe is unmetered
    /// and, on the O1 evidence, unbilled — O17), so the gate can never cost
    /// more *money* than calling [`Client::send`] directly would — only more
    /// requests.
    ///
    /// **TOCTOU: reduces cost, does not eliminate it.** The vehicle can
    /// fall asleep in the gap between the probe response and the paid call
    /// this method then makes — that race is not closed, and the 408 it
    /// produces is, per the measurement above, still billed. This gate
    /// removes the *common* case (already asleep when the caller asks), not
    /// every case.
    ///
    /// **Fail-closed.** If the probe itself errors — network failure,
    /// 401, 5xx, anything — this method returns that error and does **not**
    /// fall through to the paid call. The gate exists to answer "should I
    /// spend money"; proceeding anyway when it cannot answer would cost more
    /// than not having the gate at all (a failed free probe *and* a paid
    /// call, instead of just the paid call). A caller who wants to force the
    /// paid call regardless can call [`Client::send`] directly — that is
    /// exactly why the two methods are kept separate rather than folding a
    /// "force" flag into one.
    ///
    /// `state` is deliberately a plain `String`, not an enum: the only bit
    /// this gate needs is "is it `online`". If Tesla ever renamed that
    /// string, an enum would either fail to parse the new value or collapse
    /// it to some `Unknown` variant, whereas a bare string passes whatever
    /// Tesla actually sent straight through to the caller. A rename would
    /// make every call here fall into [`Guarded::Skipped`] — which is the
    /// **safe** direction, since it drives the gate's cost to zero rather
    /// than to a false "go ahead and spend" — and the state string carried
    /// on `Skipped` is exactly the diagnostic a caller would need to notice
    /// and report the rename.
    ///
    /// Returns a [`Metered`] wrapper so the gate and the quota observation
    /// compose. Before A5 this method used [`Client::send`] twice and dropped
    /// the `ratelimit-*` headers entirely, which made the two mutually
    /// exclusive: the gate exists for the two billed endpoints, and those are
    /// the only endpoints that emit those headers. Saving money meant losing
    /// the quota view. `Metered<Guarded<T>>` adds no new type — the two
    /// orthogonal axes (was the paid call made / what quota did we observe)
    /// compose out of the types that already existed.
    ///
    /// # Errors
    ///
    /// The probe's error ([`Client::send`] on a [`GetVehicle`] built from
    /// `req.vin()`), unchanged, if the probe fails. Otherwise, `req`'s own
    /// error if the paid call is made and fails.
    pub fn send_guarded<R: VehicleEndpoint>(
        &self,
        req: &R,
    ) -> Result<Metered<Guarded<R::Response>>, Error> {
        // `req` is only ever constructed through a `::new` that already ran
        // `encode_vin_segment` on this exact VIN (every `VehicleEndpoint`
        // this crate ships follows that pattern — see e.g. `GetVehicle::new`
        // / `GetVehicleData::new`), so re-validating the same string here
        // cannot fail. `.expect` documents that invariant instead of
        // inventing an `Error` variant for a case that cannot occur.
        let probe = GetVehicle::new(req.vin())
            .expect("req.vin() was already validated by its own constructor");
        // Fail-closed: the probe's error propagates via `?` and the paid
        // call below is never reached.
        //
        // `send_inner`, not `send` — `send` discards the rate-limit channel,
        // which is exactly the composition failure A5 T2 fixes. The gate is
        // for the two billed endpoints and those are the only ones that emit
        // `ratelimit-*` headers, so gating used to mean giving up the quota
        // view.
        let (summary, probe_rate_limit) = self.send_inner::<_, Typed<GetVehicle>>(&probe)?;
        let state = summary.state.unwrap_or_default();
        if state != "online" {
            // The probe's own quota view, not a hardcoded `None`. Measured
            // 2026-08-03: `GET /vehicles/{vin}` emits no `ratelimit-*`
            // headers, so this is `None` today — but if that ever changes
            // the caller sees it.
            return Ok(Metered {
                value: Guarded::Skipped { state },
                rate_limit: probe_rate_limit,
            });
        }
        let (value, rate_limit) = self.send_inner::<R, Typed<R>>(req)?;
        Ok(Metered {
            value: Guarded::Executed(value),
            rate_limit,
        })
    }
}

impl ClientBuilder {
    /// Sets the bootstrap host this task's client sends every request to.
    /// Defaults to [`NA_BOOTSTRAP`] when never called.
    #[must_use]
    pub fn bootstrap_region(mut self, url: crate::Url) -> Self {
        self.bootstrap = Some(url);
        self
    }

    /// Sets the per-request timeout applied to every GET. Defaults to 30s
    /// ([`DEFAULT_REQUEST_TIMEOUT`]) when never called — see that constant
    /// for why the default errs long rather than tight.
    #[must_use]
    pub fn request_timeout(mut self, d: Duration) -> Self {
        self.request_timeout = d;
        self
    }

    /// Opts into retrying [`Error::is_retryable`] failures, up to `n` total
    /// attempts of one logical [`Client::send`] call (the original send plus
    /// `n - 1` retries, each separated by backoff — see [`Client::send`]'s
    /// doc for the exact chain).
    ///
    /// **Defaults to `1` — no retry.** Retry-on-by-default is an explicit
    /// non-goal of the upstream spec (design §11, restated at §9.0 after two
    /// independent reviewers flagged the earlier draft's backoff table and
    /// `is_retryable() == true` as dead code sitting next to `max_attempts =
    /// 1`). **Do not change this default** — [`Client::send`]'s
    /// `backoff_path_is_not_entered_by_default` test pins it.
    ///
    /// `n == 0` behaves like `1`: [`Client::send`] always makes at least one
    /// attempt, so this is "how many attempts total", not "a budget that can
    /// be spent down to zero requests".
    #[must_use]
    pub fn max_attempts(mut self, n: u32) -> Self {
        self.max_attempts = n.max(1);
        self
    }

    /// Sets the wall-clock budget for the *whole* opt-in retry chain —
    /// every attempt plus every backoff sleep between them, not a
    /// per-request timeout. Defaults to 60s ([`DEFAULT_RETRY_DEADLINE`],
    /// design §9.2).
    ///
    /// Only consulted when [`ClientBuilder::max_attempts`] is greater than
    /// 1; setting this alone does not enable retry.
    ///
    /// A per-attempt timeout ([`ClientBuilder::request_timeout`]) cannot
    /// substitute for this: it bounds one attempt, not a chain of several —
    /// Task 4 of this design was bitten by exactly that gap once already.
    #[must_use]
    pub fn retry_deadline(mut self, d: Duration) -> Self {
        self.retry_deadline = d;
        self
    }

    /// Adds one more host a server-supplied `fleet_api_base_url` may name,
    /// on top of [`TESLA_REGION_HOSTS`].
    ///
    /// **This widens the set of hosts that can receive your Bearer
    /// token.** Adding a host means "I accept that Tesla's region endpoint
    /// may redirect my credentials here". Everything else
    /// [`validate_region_url`] checks still applies — https, no userinfo,
    /// default port, no path/query/fragment — so this relaxes *only* the
    /// host set, never the scheme or the port.
    ///
    /// The intended use is a region the shipped allowlist has not caught up
    /// with; the China region host (`fleet-api.prd.cn.vn.cloud.tesla.cn`) is
    /// the known case, deliberately left out of the default for want of
    /// evidence (design O11).
    ///
    /// The host is lowercased, because `Url::host_str` reports a lowercased
    /// host and a mixed-case entry would otherwise sit in the allowlist
    /// silently matching nothing.
    #[must_use]
    pub fn allow_region_host(mut self, host: &str) -> Self {
        self.allowed_region_hosts.push(host.to_ascii_lowercase());
        self
    }

    /// Test-only: accept one full origin (`scheme://host:port`) verbatim as
    /// a region URL.
    ///
    /// **Why this exists.** `wiremock` serves plain HTTP on an ephemeral
    /// port, so a mock region response can only ever name
    /// `http://127.0.0.1:PORT` — which [`validate_region_url`] rejects
    /// twice over (not https, not the default port), correctly. Without a
    /// hatch, no test could exercise adoption, invalidation or 412 recovery
    /// end to end at all.
    ///
    /// It is `#[cfg(test)]` and `pub(crate)`, so it **cannot exist in a
    /// consumer build** — this is not a public "insecure mode", and
    /// [`ClientBuilder::allow_region_host`] deliberately does not grow one.
    /// The relaxation is confined to scheme+host+port: the userinfo, path,
    /// query and fragment checks still run (see
    /// [`check_region_url_shape`]), and the production adoption machinery
    /// (cache, generation, base-URL switch) is the same code either way.
    /// What no test using this hatch can prove is the *verdict* of the
    /// production host/scheme/port rules — that is what the direct
    /// `validate_region_url` unit tests and
    /// `adopts_a_valid_region_url_and_switches_base` are for.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn allow_region_origin_for_tests(mut self, url: &crate::Url) -> Self {
        self.allowed_region_origins.push(TestOrigin::of(url));
        self
    }

    /// Builds the [`Client`].
    ///
    /// # Redirects are disabled on the underlying `reqwest::Client`
    ///
    /// `reqwest`'s default policy follows up to 10 redirects automatically.
    /// [`crate::transport::execute_get`] has a guard that rejects a 3xx
    /// status itself, but that guard is unreachable dead code unless the
    /// underlying client is *also* configured not to follow redirects first
    /// — otherwise a 3xx response is already resolved to whatever it points
    /// at (200, another error, anything) before `execute_get` ever sees a
    /// status code. This is the one place the client is actually
    /// constructed, so this is where that policy has to be set.
    ///
    /// Fix round 1 (Minor 2): this doc previously framed the risk as
    /// "Authorization could follow a redirect to a different host" — that
    /// overstates it. `reqwest` 0.12.28 already strips the `Authorization`
    /// header itself on a cross-host redirect, regardless of this policy.
    /// `Policy::none()` is still the correct setting, for two reasons that
    /// hold independent of that stripping behaviour: a *same-host* redirect
    /// would still carry the header (and this crate never validates where a
    /// same-host redirect actually points), and design §4.6 wants a 3xx
    /// response to fail loudly through [`Error::Api`] rather than be
    /// silently resolved by the HTTP layer before this crate's own gates
    /// ever see it.
    ///
    /// # Errors
    ///
    /// Currently never fails: the ureq agent builds infallibly. The `Result`
    /// is kept so a future fallible setting does not change the public API.
    pub fn build(self) -> Result<Client, Error> {
        // 리다이렉트를 따라가지 않는 Agent 하나를 씁니다(design §4.6, 게이트 G2 가 `net::agent` 를 봅니다).
        let agent = crate::net::agent(self.request_timeout);
        let bootstrap = self
            .bootstrap
            .unwrap_or_else(|| crate::Url::parse(NA_BOOTSTRAP).expect("NA_BOOTSTRAP is valid"));
        Ok(Client {
            agent,
            tokens: self.tokens,
            bootstrap,
            request_timeout: self.request_timeout,
            max_attempts: self.max_attempts,
            retry_deadline: self.retry_deadline,
            allowed_region_hosts: self.allowed_region_hosts,
            #[cfg(test)]
            allowed_region_origins: self.allowed_region_origins,
            region: RegionCache::default(),
            #[cfg(test)]
            dispatches: std::sync::atomic::AtomicU32::new(0),
        })
    }
}

/// The refusal error every region trust-boundary rejection produces.
///
/// [`Error::Auth`] with `pricing: None`: no priced endpoint was reached (the
/// region endpoint is unbilled, and the request this would have enabled was
/// never sent), and the caller-facing question is the same one a 401 poses —
/// this client will not authenticate against that destination. Every message
/// goes through [`body_preview`], because `why` can quote a server-supplied
/// string.
fn region_refusal(why: &str) -> Error {
    Error::Auth {
        message: body_preview(
            &format!("refusing server-provided region URL: {why}"),
            MSG_MAX,
        ),
        pricing: None,
    }
}

/// Shape checks that hold for **any** accepted region URL, independent of
/// which host set it is being matched against.
///
/// Split out so the `#[cfg(test)]` origin hatch in
/// [`Client::adopt_region_url`] cannot accidentally skip them, and so there
/// is exactly one implementation of each rule to audit.
fn check_region_url_shape(url: &crate::Url) -> Result<(), Error> {
    if !url.username().is_empty() || url.password().is_some() {
        // `https://fleet-api…@evil.example` parses with `fleet-api…` as the
        // *username*: a human skimming the string reads a Tesla host, the
        // connection goes to `evil.example`.
        return Err(region_refusal("URL carries userinfo"));
    }
    if !matches!(url.path(), "" | "/") {
        return Err(region_refusal("URL carries a path"));
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(region_refusal("URL carries a query or fragment"));
    }
    Ok(())
}

/// Decides whether a server-supplied region URL may become this client's
/// base URL.
///
/// **This function is the crate's credential trust boundary.** A URL that
/// passes it receives live Bearer tokens, so the failure mode of a wrong
/// answer here is not "a bad request" — it is *the user's Tesla account
/// token, sent to whoever supplied the string*. Every check below must pass;
/// on any doubt the URL is refused and the caller keeps its bootstrap host
/// (design §4.5).
///
/// | # | check | what it stops |
/// |---|---|---|
/// | 1 | scheme is `https` | plaintext transmission of the token |
/// | 2 | host is in `allowed` **exactly** | sending the token to any other host |
/// | 3 | no userinfo | credential smuggling / parser confusion |
/// | 4 | default port | internal port probing via the token-bearing client |
/// | 5 | path is empty or `/` | path-prefix injection under the API paths |
/// | 6 | no query or fragment | same |
///
/// Check 2 is **exact equality**, never a suffix test. `ends_with` fails
/// three ways an attacker can register: `tesla.com.attacker.net`,
/// `nottesla.com`, and `evil-fleet-api.prd.na.vn.cloud.tesla.com`. The unit
/// test carries an input for each.
///
/// # Errors
///
/// [`Error::Auth`] with `pricing: None` naming the check that refused. The
/// message never carries the URL itself — a rejected URL is attacker-chosen
/// text, and the diagnostic value of echoing it does not outweigh feeding it
/// into a log or a pasted GitHub issue.
fn validate_region_url(raw: &str, allowed: &[String]) -> Result<crate::Url, Error> {
    let url = crate::Url::parse(raw).map_err(|_e| region_refusal("unparseable"))?;
    if url.scheme() != "https" {
        return Err(region_refusal("scheme is not https"));
    }
    check_region_url_shape(&url)?;
    let host = url
        .host_str()
        .ok_or_else(|| region_refusal("URL has no host"))?;
    if !allowed.iter().any(|a| a == host) {
        return Err(region_refusal("host is not an allowed Tesla region host"));
    }
    if !matches!(url.port(), None | Some(443)) {
        return Err(region_refusal("non-default port"));
    }
    Ok(url)
}

/// What [`Client::send_inner`] turns one raw response into.
///
/// **The only axis on which `send`, `send_metered` and [`Client::send_raw`]
/// differ.** Everything else — money, tokens, redirects, retries — is shared;
/// see `send_inner`'s doc. Implementors are zero-sized type-level tags, never
/// constructed.
trait ResponseShape {
    /// What the caller receives on success.
    type Out;

    /// The error a `>= 400` response produces, for the 401/412 retention rule
    /// (see [`response_error`]).
    fn error_of(raw: &crate::transport::RawResponse, pricing: Pricing) -> Option<Error>;

    /// The final decode of a response that was not recovered from.
    fn finish(raw: &crate::transport::RawResponse, pricing: Pricing) -> Result<Self::Out, Error>;
}

/// [`ResponseShape`] for [`Client::send`]/[`Client::send_metered`] — decode
/// into the endpoint's wire type and assemble its public type.
struct Typed<R>(std::marker::PhantomData<fn() -> R>);

impl<R: FleetGet> ResponseShape for Typed<R> {
    type Out = R::Response;

    fn error_of(raw: &crate::transport::RawResponse, pricing: Pricing) -> Option<Error> {
        response_error::<R::WireResponse>(raw, pricing)
    }

    fn finish(raw: &crate::transport::RawResponse, pricing: Pricing) -> Result<R::Response, Error> {
        let d = crate::decode::decode_with_txid_header::<R::WireResponse>(
            raw.status,
            raw.content_type.as_deref(),
            &raw.body,
            pricing,
            raw.retry_after,
            raw.www_authenticate.as_deref(),
            raw.x_txid.as_deref(),
        )?;
        Ok(R::assemble(d.payload, d.pagination))
    }
}

/// [`ResponseShape`] for [`Client::send_raw`] — keep the body verbatim.
struct Raw;

impl ResponseShape for Raw {
    type Out = serde_json::Value;

    /// The type parameter is irrelevant here and that is not an accident:
    /// [`crate::decode::decode_with_txid_header`] resolves every status
    /// `>= 400` **before** it looks at the body, so a 401/412 produces the same
    /// [`Error`] whatever it would have decoded into. `serde_json::Value` is
    /// named only because some type must be.
    fn error_of(raw: &crate::transport::RawResponse, pricing: Pricing) -> Option<Error> {
        response_error::<serde_json::Value>(raw, pricing)
    }

    fn finish(
        raw: &crate::transport::RawResponse,
        pricing: Pricing,
    ) -> Result<serde_json::Value, Error> {
        crate::decode::decode_body_raw(
            raw.status,
            raw.content_type.as_deref(),
            &raw.body,
            pricing,
            raw.retry_after,
            raw.www_authenticate.as_deref(),
            raw.x_txid.as_deref(),
        )
    }
}

/// The [`Error`] a response that [`Client::send`] is about to *recover from*
/// would have produced had it been returned to the caller.
///
/// Used only to retain a 401/412 across a recovery iteration (final fix
/// wave, Important 2), so that a token-layer failure on the next iteration —
/// `Error::Auth { pricing: None }`, [`Error::billing`] `== NotBilled` —
/// cannot erase an outcome the caller was actually charged for. It runs the
/// **same** [`crate::decode::decode_with_txid_header`] the non-recovered path
/// runs (A4 Task 3: including `raw.x_txid`), rather than hand-building an
/// error, so the retained value cannot drift from what the caller would
/// otherwise have seen (message, redaction, pricing, txid).
///
/// `None` only if `decode` somehow succeeds, which for the 4xx statuses this
/// is called with it cannot — `decode` returns `Err` for every status `>=
/// 400` before it looks at the body.
///
/// **This doc belongs to this function, not to [`ResponseShape`]** (A5c-2a
/// T20 fix round 1). The `ResponseShape` refactor moved the doc block off this
/// item and left it attached to the trait, where the closing paragraph read as
/// a statement about the trait's contract. `rustdoc`'s `-D warnings` gate does
/// not see it — both items are private — so the same rule that deleted
/// `CaptureError::RawUnavailable` applies here: **a false description is worse
/// than none.**
fn response_error<T: DeserializeOwned>(
    raw: &crate::transport::RawResponse,
    pricing: Pricing,
) -> Option<Error> {
    crate::decode::decode_with_txid_header::<T>(
        raw.status,
        raw.content_type.as_deref(),
        &raw.body,
        pricing,
        raw.retry_after,
        raw.www_authenticate.as_deref(),
        raw.x_txid.as_deref(),
    )
    .err()
}

/// Whether `req` is the region-resolution endpoint itself.
///
/// [`Client::send`]'s pre-flight resolution must not fire for it: resolving
/// the region *is* a [`GetRegion`] call, so dispatching one through region
/// resolution would be circular. Compared by path because that is the
/// endpoint's identity on the wire — [`FleetGet`] is sealed, so no
/// downstream type can collide with it.
fn req_resolves_region<R: FleetGet>(req: &R) -> bool {
    req.path() == GetRegion.path()
}

/// Combines a base URL, an endpoint path and its query into one request URL.
///
/// Uses [`crate::Url::set_path`]/`set_query` rather than string
/// concatenation: `path()` may already contain a percent-encoded VIN segment
/// (via [`crate::query::encode_vin_segment`]), and `set_path` passes an
/// already-valid escape (`%2F`) through unchanged rather than re-encoding its
/// `%` — verified against the `url` crate directly, since a double-encoding
/// bug here would corrupt every vehicle-scoped request.
fn build_url(base: &crate::Url, path: &str, query: &QuerySpec) -> crate::Url {
    let mut url = base.clone();
    url.set_path(path);
    let encoded = query.to_encoded();
    url.set_query(if encoded.is_empty() {
        None
    } else {
        Some(&encoded)
    });
    url
}

/// The backoff delay [`Client::send`] sleeps before the retry attempt that
/// follows a chain that has already spent `retries_used` retries (so
/// `retries_used == 0` is the delay before the *first* retry).
///
/// Exponential with full jitter, capped at [`BACKOFF_MAX`] (design §9:
/// "exponential + jitter, cap 30s"): the un-jittered delay doubles each
/// retry from [`BACKOFF_INITIAL`], is capped, and then a uniformly random
/// fraction of that capped value is actually slept — "full jitter" rather
/// than "capped exponential, no jitter" so many callers retrying the same
/// failure around the same time don't reconverge on the same schedule.
///
/// The randomness source is deliberately not a dependency: this crate has no
/// other need for one, and backoff jitter has no correctness requirement a
/// low-quality, non-cryptographic source can't meet. Sub-second wall-clock
/// jitter is enough — successive calls (separated by at least
/// [`BACKOFF_INITIAL`] of sleep) land at different sub-second offsets.
fn backoff_delay(retries_used: u32) -> Duration {
    // `min(20)`: caps the shift so `1u32 << shift` never overflows (`u32`
    // has 32 bits); the resulting un-jittered delay is already far past
    // `BACKOFF_MAX` well before `retries_used` gets anywhere near 20, so the
    // subsequent `.min(BACKOFF_MAX)` — not this shift limit — is what
    // actually bounds the value in practice.
    let shift = retries_used.min(20);
    let uncapped = BACKOFF_INITIAL.saturating_mul(1u32 << shift);
    let capped = uncapped.min(BACKOFF_MAX);
    Duration::from_secs_f64(capped.as_secs_f64() * jitter_fraction())
}

/// A pseudo-random value in `[0.0, 1.0)`.
///
/// See [`backoff_delay`] for why a `rand`-crate PRNG isn't needed here — but
/// the source still has to actually decorrelate the callers that full
/// jitter exists to decorrelate. **Fix round 1 (Important 2): sub-second
/// wall-clock time alone does not.** Two clients reacting to the *same*
/// synchronized failure at the *same* instant — precisely the scenario
/// jitter exists to protect against — would read nearly identical
/// `subsec_nanos()` values and retry in lockstep anyway, silently defeating
/// the whole point. The wall clock is kept (it is still what varies call to
/// call within one process/thread), but mixed with [`std::process::id`] and
/// the calling [`std::thread::Id`] so two different processes — or two
/// different threads/tasks in the same process — racing the same failure
/// diverge instead of computing the same fraction.
fn jitter_fraction() -> f64 {
    use std::hash::{Hash, Hasher};

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    nanos.hash(&mut hasher);
    std::process::id().hash(&mut hasher);
    std::thread::current().id().hash(&mut hasher);
    let mixed = hasher.finish();

    // `mixed % DENOM` bounded strictly below `DENOM`, so the division is
    // always in `[0.0, 1.0)` — never rounds up to exactly `1.0`.
    const DENOM: u64 = 1_000_000_007; // a prime comfortably above u32::MAX
    (mixed % DENOM) as f64 / DENOM as f64
}

#[cfg(test)]
mod tests;

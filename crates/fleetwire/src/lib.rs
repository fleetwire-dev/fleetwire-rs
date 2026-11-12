//! # fleetwire
//!
//! **Unofficial, read-only** Rust client for the Tesla Fleet API.
//! Not affiliated with, endorsed by, or sponsored by Tesla, Inc.
//!
//! GET operations only — no vehicle-command or signing code. Fleet API calls
//! may be metered (`vehicle_data`, `nearby_charging_sites` are billed Data;
//! the other 14 supported endpoints are not charged).
//!
//! Scope: 16 live-verified GET endpoints (12 vehicle + 4 user). 12 unverified
//! endpoints (charging/energy/partner) are intentionally excluded until verified.
//!
//! Status: **work in progress** — core types, the token layer
//! ([`TokenManager`]: owner-only on-disk storage, rotation persisted before
//! use, single-flight refresh in-process and across processes), and the full
//! HTTP-transport surface: all 16 endpoints across [`api::vehicles`] (12) and
//! [`api::users`] (4), of which exactly two —
//! [`api::vehicles::GetVehicleData`] and
//! [`api::vehicles::GetNearbyChargingSites`] — are billed
//! ([`pricing::Pricing::data`]); the other 14 are
//! [`pricing::Pricing::NotCharged`].
//!
//! # Three ways this crate deals with the two billed endpoints' cost — none of them alone is enough
//!
//! - **The type system** ([`pricing::Pricing`]). Every [`endpoint::FleetGet`]
//!   implementation reports its category ahead of the call via
//!   `req.pricing()`, so a caller can branch on cost before dispatching.
//!   What it guarantees: the two billed endpoints are tagged `Data`, the
//!   other 14 are tagged `NotCharged`, and that tagging can't drift silently
//!   because it lives on the same type as the request. What it does **not**
//!   guarantee: it is metadata, not a gate — nothing stops `Client::send`
//!   from making a `Data`-tagged call, and the `NotCharged` tag on the 14
//!   unpaid endpoints is asserted from the O1 evidence matrix's
//!   `pricing_category: null`, not confirmed against a billing dashboard
//!   (see O17 below).
//! - **The gate** ([`Client::send_guarded`]). Issues a free probe
//!   (`GET /api/1/vehicles/{vin}` — no `ratelimit-*` headers at all on that
//!   response, measured 2026-08-03, which is strong circumstantial evidence
//!   it sits outside the metered `device_data` category and not proof it is
//!   unbilled; see O17 below) and only makes the paid
//!   call when the probe reports `state == "online"`; otherwise it returns
//!   [`Guarded::Skipped`] and **spends nothing**. If the probe itself fails,
//!   it does not fall through to the paid call — see its own doc for the
//!   fail-closed rule. What it does **not** guarantee: this is a TOCTOU
//!   window, not a lock — the vehicle can fall asleep between the probe and
//!   the paid call, and that paid call still gets billed (a 408 consumes
//!   both money and rate-limit quota, measured 2026-08-03). It is also not a
//!   pure win: when the vehicle is actually online, the probe is a strictly
//!   *extra* round trip that bought nothing.
//!   The gate now composes with the quota observation — `send_guarded` returns
//!   `Metered<Guarded<T>>`. Before A5 the two were mutually exclusive: gating
//!   went through `send`, which drops the `ratelimit-*` headers that only the
//!   two billed endpoints emit.
//! - **Observation** ([`Client::send_metered`], [`Metered`], [`RateLimit`]).
//!   Surfaces the `ratelimit-device-data-per-vehicle-*` response headers
//!   attached to the exact response that carried them, rather than a
//!   `Client`-wide slot that multiple VINs or concurrent calls would
//!   overwrite. What it does **not** guarantee: it never throttles
//!   automatically (whether the quota is account-shared or per-vehicle is
//!   **unmeasured** — open item O16 below — and this crate's own view of
//!   `remaining` is incomplete under *either* reading: an account-shared
//!   quota is also spent by other apps, a per-vehicle one by other clients
//!   talking to the same vehicle), and it only covers the
//!   success path — the measured 408 in this subproject's evidence *did*
//!   carry these headers, but [`Client::send`]'s error path does not expose
//!   them anywhere a caller can reach (O14 below).
//!
//! [`Client`] starts on its configured **bootstrap** host
//! ([`ClientBuilder::bootstrap_region`] / [`NA_BOOTSTRAP`]) and adopts the
//! server-supplied `fleet_api_base_url` from [`api::users::RegionInfo`] only
//! after it passes a fixed host allowlist ([`TESLA_REGION_HOSTS`]) — that
//! string is Bearer-token-bearing, so adopting it unvalidated would be a
//! credential-exfiltration path. Refreshing needs an implementation of [`TokenRefresher`],
//! and this crate ships none other than [`HttpRefresher`]. Any refresher
//! implementation **must** set its own request timeout:
//! [`TokenManager::access_token`] holds both its in-process lock and the
//! cross-process file lock across the refresh await, so a refresher that
//! hangs blocks every caller in this process indefinitely and every sibling
//! process for the full lock deadline. Before dispatch, [`Client::send`]
//! also runs an advisory scope pre-check (design §10) — see
//! [`error::Error::MissingScope`]'s doc for what it does and does not prove.
//!
//! # What enforces read-only, and what does not
//!
//! The **primary** defence is the type system and module visibility, not
//! clippy. [`endpoint::FleetGet`] is sealed (only this crate can implement
//! it) and every implementation this crate ships issues a GET; the HTTP call
//! site itself (`crate::transport::execute_get`, private) never
//! constructs anything but a GET. `crates/fleetwire/clippy.toml`'s
//! `disallowed-methods` list (blocking the mutating `ureq::Agent`
//! methods — `post`, `put`, `patch`, `delete`, `head`, `run`) is a
//! **backstop**, not a third enforcement layer — saying three layers
//! "enforce" read-only overstates what a lint can do. It catches an
//! accidental call to one of those disallowed methods; it does **not**
//! catch an `http::Request` with `Method::POST` built directly and sent some
//! other way, a future
//! `#[allow(clippy::disallowed_methods)]` spreading over this rule, or a
//! second HTTP client crate added alongside `ureq`. None of those are
//! hypothetical categories of mistake this
//! crate has a structural defence against — they are gaps a determined (or
//! careless) contributor could still open. Read-only is a property of what
//! this crate's authors have chosen to write and seal, backed by a lint that
//! makes the *common* accident loud; it is not a property the compiler
//! proves.
//!
//! # Verification levels — what was measured against the live API, what was not
//!
//! A subset of this crate's behavior is pinned to responses actually
//! observed from Tesla's Fleet API. **2026-08-01** (see the A3 design doc's
//! §1 evidence log for full transcripts): the three distinct shapes a 401
//! can take depending on which upstream rejected it ([`error::Error::Auth`]'s
//! doc — two arrive as `application/json`, one as an empty `text/html` body
//! with a `WWW-Authenticate` header and no JSON at all, and all three
//! collapse to the same variant here rather than leaking as
//! [`error::Error::Decode`]); the 412 a wrong-region account receives, body
//! and all, including that it leaks an account UUID redaction had to be
//! taught to catch; that this crate's region-resolution flow (cache → the
//! server-authoritative `GET /api/1/users/region` → bootstrap fallback)
//! reproduces the real adopt/reject paths; and that OIDC discovery's
//! `token_endpoint` is not authoritative for where a refresh actually
//! succeeds (the deployed client refreshes against a *different* host than
//! discovery advertises, and the token's own `iss` disagrees with
//! discovery's `issuer`) — which is why [`HttpRefresher`] hardcodes its
//! endpoint rather than following discovery.
//!
//! **2026-08-03** (this subproject, A4 — vehicle offline/asleep for the
//! whole session, see the A4 design doc's §1 evidence log): the **408** shape
//! a sleeping vehicle's `vehicle_data` call returns — it uses the same
//! `{"response": …}` envelope every other status does (`response: null`),
//! carries an `error_description` key that is present but an **empty
//! string** (so this crate's error-message fallback chain must skip empty
//! strings rather than let one win by virtue of the key existing —
//! [`error::Error::Auth`] and [`error::Error::VehicleOffline`]'s message
//! resolution both do), and carries a `txid` in the body plus `x-txid`/
//! `x-request-id` headers, all preserved rather than redacted
//! ([`error::Error::VehicleOffline::txid`]); the `pagination` object arriving
//! as a **sibling** of `response` on list endpoints, not nested inside it,
//! alongside a top-level `count` this crate deliberately does not model —
//! the one measured sample had it equal to `pagination.count`, so a
//! separate field would just duplicate the same value under a second name
//! (O13 below tracks whether that equality always holds); the
//! `ratelimit-device-data-per-vehicle-*` response headers on paid calls
//! ([`RateLimit`]'s doc — `remaining` decrements by 1 per call and a 408
//! consumes a slot too); and that the free gate probe
//! (`GET /api/1/vehicles/{vin}`) sends **no** `ratelimit-*` headers at all,
//! which is circumstantial evidence — not proof of billing — that it sits
//! outside the metered `device_data` category (see O17 below for why this
//! stops short of "confirmed unbilled").
//!
//! **5xx responses still have no measured evidence at all** — this
//! crate's handling of them ([`error::Error::is_retryable`]'s 5xx allowlist)
//! rests on the upstream design spec's documentation, not on a response this
//! crate's authors have actually seen. The 408 half of this same gap closed
//! in A4 (above); 5xx did not. Likewise, the `NotCharged` classification of
//! the 14 unpaid endpoints comes from the O1 evidence matrix's
//! `pricing_category: null` field, not from a Tesla billing dashboard — no
//! usage-dashboard reconciliation has been done for any endpoint in this
//! crate, paid or free. Do not read this crate's test suite as proof any of
//! these shapes are correct — see the open items below.
//!
//! # Open items carried into and past this subproject (A2/A3/A4)
//!
//! - **O8**: the 401 shape for an *expired* access token specifically is
//!   unmeasured — every 401 shape actually captured so far came from a
//!   token that was never valid to begin with (malformed, absent, or
//!   deliberately wrong), not one that expired mid-lifetime. Resolves when
//!   a genuinely expired token is captured live.
//! - **O9**: whether an *unauthenticated* request (no account to attribute
//!   the call to) is billed at all is unknown. Resolves against Tesla's
//!   usage dashboard.
//! - **O10**: 408 resolved this subproject (see above); **5xx remains
//!   unmeasured**. Resolves when a real 5xx response is captured live.
//! - **O11**: the China region host (`…tesla.cn`) is a documentation-only
//!   guess, never confirmed against a live response, and is **deliberately
//!   absent from [`TESLA_REGION_HOSTS`]**, the default region allowlist — a
//!   caller who needs it must opt in explicitly rather than this crate
//!   guessing on their behalf (see [`ClientBuilder::allow_region_host`]).
//!   Resolves against a live CN-region response.
//! - **O12**: `per_page`'s default — the O1 matrix says `100`; the one live
//!   measurement (this account has a single vehicle) showed `1`, which is
//!   indistinguishable from "the real default" when there is only one item
//!   to page over. Resolves against an account with 2+ vehicles, or a
//!   re-measurement of `invitations`.
//! - **O13**: whether the top-level `count` sibling key can ever diverge
//!   from `pagination.count` — the one measured sample had them equal,
//!   which is why `count` is not modeled separately (two same-named,
//!   same-valued fields would be a `Page` API wart, not a feature).
//!   Resolves against a multi-item list response (`invitations` with more
//!   than one item is the best live candidate).
//! - **O14**: error-path rate-limit headers are not exposed anywhere in this
//!   crate's public API — [`Metered`] wraps [`Client::send`]'s success path
//!   only, even though the measured 408 in this subproject's evidence *did*
//!   carry `ratelimit-*` headers. Resolves in A5, or sooner if a caller
//!   needs it.
//! - **O15**: [`error::Error::VehicleOffline`] is produced from HTTP status
//!   408 alone, with no body-content check — a gateway-level 408 unrelated
//!   to vehicle sleep would be misclassified the same way. Not fixed here:
//!   moving classification onto body text would trade one hazard (an
//!   unrelated 408 misread as vehicle-asleep) for another (classifying on
//!   response *content* rather than status, which this crate's design
//!   otherwise avoids). Resolves once a non-vehicle-sleep 408 is actually
//!   observed and a decision can be made with real cases to compare.
//! - **O16**: whether the `device_data` rate-limit quota is shared
//!   account-wide or scoped per vehicle — the header name says
//!   `per-vehicle`, but the O1 matrix's own text says account-shared, and
//!   this subproject's measurement (one account, one vehicle) cannot tell
//!   the two apart. Resolves against 2+ vehicles on one account, or
//!   concurrent calls from a second app.
//! - **O17**: the `NotCharged` classification of the 14 unpaid endpoints
//!   (everything except [`api::vehicles::GetVehicleData`] and
//!   [`api::vehicles::GetNearbyChargingSites`]) rests on the O1 evidence
//!   matrix, not on a billing-dashboard reconciliation — likewise, the free
//!   gate probe's absence of `ratelimit-*` headers is strong circumstantial
//!   evidence it is unmetered, not proof it is unbilled. Resolves against a
//!   Tesla usage-dashboard audit covering all 16 endpoints.
//! - **O18**: the rate-limit window's type (fixed / sliding / token-bucket)
//!   and `reset`'s unit, base, and boundary behavior (does it wrap at 0?
//!   jump to `limit`? something else?) are unmeasured — the only evidence is
//!   three consecutive calls showing `reset` monotonically decrease
//!   (57 → 56 → 55), which three points cannot distinguish between those
//!   window types. Resolves against measurements spanning an actual window
//!   rollover.
//! - **O19**: `nearby_charging_sites`'s `detail` parameter's wire form is
//!   unverified — this crate sends a lowercase `"true"`/`"false"` by
//!   convention, but the vehicle was asleep (408) for every measurement
//!   attempt in this subproject, so the parameter never reached a live paid
//!   call to confirm Tesla accepts that form. Resolves once the vehicle is
//!   online during a `nearby_charging_sites` call.
//! - **O20** (*partially resolved 2026-08-04*): the O1 evidence matrix's
//!   scope wording is not uniform across the 4 user endpoints — `users/me`
//!   states its scope explicitly as `user_data` (not `openid`), while
//!   `users/orders` and `users/feature_config` were hedged as
//!   `user_data`-**like**. This crate used to unify all three under
//!   [`scope::Scope::UserData`] on that hedge. A live scope check — the
//!   resolution condition this entry named — has since been run against one
//!   of them. Fully resolves against a live scope check for `users/me` and
//!   `users/feature_config`, plus a withheld-scope contrast run for
//!   `users/orders`. Per endpoint:
//!   - `users/feature_config`: still [`scope::Scope::UserData`]. Unchanged;
//!     the docs bundle and this crate's declaration agree, and nothing
//!     contradicts it. **Still on the hedge, not on measurement.**
//!   - `users/orders`: **measured — `user_data` is not required.** A live
//!     `GET` returned 200 on a token whose `scp` lacked it, so the previous
//!     declaration made the free pre-check deny an operable endpoint. Now
//!     declared [`scope::Scope::VehicleDeviceData`], which the docs bundle
//!     names and the successful token carried — but note that only the
//!     *negative* half is measured: **that `vehicle_device_data` is
//!     required was never confirmed**, since no run withheld it. See
//!     [`api::users::ListOrders::required_scopes`].
//!
//! # Blocking API, no async runtime
//!
//! Every call blocks the calling thread (ureq + `std`). There is no runtime to
//! configure. Each request has one deadline (`ClientBuilder::request_timeout`)
//! covering connect, send and the capped body read; a server that trickles its
//! body is cut off at the deadline plus at most about one second.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod api;
mod auth;
mod client;
mod clock;
mod decode;
pub mod endpoint;
pub mod error;
pub mod model;
mod net;
pub mod pricing;
pub mod query;
mod redaction;
pub mod scope;
mod sealed;
#[cfg(test)]
mod test_support;
mod transport;

// Crate-root re-exports of the types callers touch on nearly every call
// (design §2's module map: `lib.rs # 공개 re-export`). Final fix wave (F11):
// there were none, so the shortest way to name the error type was
// `fleetwire::error::Error`. The module paths remain public, so both spellings
// work; these are the canonical ones.
// `auth` and `clock` are private modules, but these re-exported items are
// public API — so `#![deny(missing_docs)]` applies to every one of their
// public items and fields.
pub use crate::auth::{
    Generation, HttpRefresher, MIN_REFRESH_INTERVAL, REFRESH_TIMEOUT, Refreshed, TokenManager,
    TokenRefresher,
};
pub use crate::client::{
    Client, ClientBuilder, Guarded, Metered, NA_BOOTSTRAP, RateLimit, TESLA_REGION_HOSTS,
};
pub use crate::clock::{Clock, SystemClock};
pub use crate::endpoint::{FleetGet, VehicleEndpoint};
pub use crate::error::Error;
pub use crate::pricing::{Billing, Pricing};
pub use crate::scope::{Scope, ScopeExpr};
/// The URL type [`ClientBuilder::bootstrap_region`] takes.
///
/// **A re-export, so callers need not depend on `url` themselves.**
/// `scripts/check-wiring.sh` G1 forbids `fleetwire-capture` from depending on an
/// HTTP crate directly (one HTTP path, not two), so this is how it names the type.
pub use url::Url;

#[cfg(test)]
mod readonly_guard_tests {
    /// Keeps only the "active" text of `clippy.toml`, with its `#` comments removed.
    ///
    /// A substring check against the raw `include_str!` only trips when a line has been
    /// deleted outright — even when an entry is disabled with a TOML comment, as in
    /// `# { path = "ureq::Agent::head", .. }`, the characters `head` remain in the file as
    /// they were, so `.contains()` still returns true (it misses the regression, code review
    /// round 1). In TOML everything from `#` to the end of the line is a comment, so keeping
    /// only what precedes the first `#` on each line makes commented-out entries (whether
    /// individual lines or the whole `disallowed-methods = [...]` block) disappear from the
    /// active text.
    fn active_content(toml: &str) -> String {
        toml.lines()
            .map(|line| line.split('#').next().unwrap_or(""))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Checks that `clippy.toml` blocks all seven kinds of write methods and bypass routes.
    ///
    /// Checking only that the file exists would pass even with one entry missing (vacuous),
    /// so it asserts per method. The check runs against the text with comments stripped by
    /// `active_content`, so this test fails even when an individual entry is commented out
    /// with `#` or the whole `disallowed-methods = [...]` block is commented out (discipline
    /// article 1: a rule that forbids X needs an input that fails only when X is done).
    /// This test cannot prove that clippy actually applies those rules — that is the job
    /// of the `cargo clippy -D warnings` gate. What is blocked here is the regression where
    /// "someone deletes or comments out an entry in the list".
    #[test]
    fn clippy_toml_forbids_every_mutating_method() {
        let active = active_content(include_str!("../clippy.toml"));
        // GET 이 아닌 메서드는 Agent 메서드·자유 함수 둘 다, `net::agent` 를 거치지 않는 Agent·요청 경로도
        // 함께 막습니다(ureq 3.3.0 의 agent.rs·lib.rs·config.rs·request_ext.rs 전수, 최종 검토 M-2).
        for m in [
            "ureq::Agent::post",
            "ureq::Agent::put",
            "ureq::Agent::patch",
            "ureq::Agent::delete",
            "ureq::Agent::head",
            "ureq::Agent::options",
            "ureq::Agent::connect",
            "ureq::Agent::trace",
            "ureq::Agent::run",
            "ureq::Agent::new_with_defaults",
            "ureq::Agent::new_with_config",
            "ureq::run",
            "ureq::get",
            "ureq::post",
            "ureq::put",
            "ureq::patch",
            "ureq::delete",
            "ureq::head",
            "ureq::options",
            "ureq::connect",
            "ureq::trace",
            "ureq::agent",
            "ureq::config::Config::new_agent",
            "ureq::RequestExt::with_agent",
            "ureq::RequestExt::with_default_agent",
        ] {
            assert!(
                active.contains(m),
                "clippy.toml 에 {m} 금지 항목이 없다(주석 처리됨 포함)"
            );
        }
    }
}

/// Compiles the Rust snippets in the README as doctests, so the quick start cannot drift from the API.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;

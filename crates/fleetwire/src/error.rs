//! Crate-wide error types and billing/retry judgment (design §9.1–9.3).

use crate::pricing::{Billing, Pricing};
use crate::scope::ScopeExpr;
use std::time::Duration;

/// The upper bound applied to any server-supplied `Retry-After` value
/// (design §9.3: "a valid `Retry-After` is honoured but capped at 120 seconds").
const RETRY_AFTER_CAP: Duration = Duration::from_secs(120);

/// Crate-wide error type for all fallible read operations.
///
/// Every variant carries enough context — including, where relevant, the
/// [`Pricing`] category of the endpoint that was called — for
/// [`Error::billing`] to judge whether the request was actually charged.
/// See design §9.1–9.3 for the full judgment table and rationale.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The request was never sent to the server (e.g. DNS resolution or TCP
    /// connect failed before any bytes reached Tesla). Provably free — never
    /// billed regardless of endpoint pricing.
    #[error("request not sent: {message}")]
    #[non_exhaustive]
    TransportNotSent {
        /// A redacted, human-readable description of the transport failure.
        message: String,
        /// The pricing category of the endpoint that was being called.
        pricing: Pricing,
    },

    /// The request may or may not have reached the server (e.g. a read
    /// timeout or connection reset after the request was written). Whether
    /// it was processed — and thus billed — cannot be determined from this
    /// error alone.
    #[error("transport outcome unknown: {message}")]
    #[non_exhaustive]
    TransportUnknown {
        /// A redacted, human-readable description of the transport failure.
        message: String,
        /// The pricing category of the endpoint that was being called.
        pricing: Pricing,
    },

    /// The server returned a non-success HTTP status.
    #[error("HTTP {status}: {message}")]
    #[non_exhaustive]
    Api {
        /// The HTTP status code returned by the server.
        status: http::StatusCode,
        /// A redacted, human-readable description of the error.
        message: String,
        /// The server's `Retry-After` hint, if any. Read this through
        /// [`Error::retry_after`], which applies the 120s cap — this field
        /// may hold an uncapped value.
        retry_after: Option<Duration>,
        /// A redacted preview of the response body. Redaction is the
        /// caller's responsibility (see `redaction`/`decode`) — by the time
        /// a value reaches this field it MUST already have had tokens,
        /// secrets, VINs and coordinates stripped. This type performs
        /// no redaction of its own and never exposes a raw response body.
        ///
        /// Account-scoped ids (`id`, `vehicle_id`, `user_id`, …) are
        /// **deliberately preserved** in a preview: they are the primary
        /// handle for correlating a failure with a specific vehicle or
        /// request, and they are not credentials. The earlier wording here
        /// also promised ids were stripped, which no code performed (final
        /// fix wave / F9). Reading REQ-18 with REQ-20, the id-stripping
        /// clause is scoped to *fixture* sanitization — recorded test data
        /// that gets committed — and is handled by that separate policy, not
        /// by the live redaction layer.
        body_preview: String,
        /// The pricing category of the endpoint that was called.
        pricing: Pricing,
        /// Tesla's transaction id for this response (body `txid` → header
        /// `x-txid`). Hand this to Tesla support when filing a case (A4).
        ///
        /// ⚠️ **Not redacted.** Account UUIDs are masked because they can
        /// identify a person; a transaction id must survive verbatim or the
        /// value is useless for a support case (same reasoning as A3 T7's
        /// decision to preserve `x-request-id`). It is a correlation
        /// identifier, not a diagnostic message, so it is deliberately **not**
        /// interpolated into the `#[error(...)]` template above — available
        /// as a field, not auto-printed into every log line.
        txid: Option<String>,
    },

    /// The vehicle was asleep/offline and the request timed out with HTTP
    /// 408 before reaching the car. Distinguished from a generic
    /// [`Error::Api`] 408 because it is the expected, common case for a
    /// read-only client that deliberately does not wake vehicles.
    #[error("vehicle offline (408)")]
    #[non_exhaustive]
    VehicleOffline {
        /// The pricing category of the endpoint that was called.
        pricing: Pricing,
        /// Same contract as [`Error::Api::txid`] (A4). ⚠️ **Must be carried
        /// here too, not only on [`Error::Api`]** — the live-measured 408
        /// this field exists for classifies as `VehicleOffline`, not `Api`;
        /// putting `txid` on `Api` alone would lose it on exactly the
        /// response it was measured against (cross-model review caught
        /// this).
        txid: Option<String>,
    },

    /// The held OAuth scopes do not satisfy the endpoint's scope
    /// requirement. Detected client-side before any request is sent.
    ///
    /// ⚠️ This value is derived from scopes **observed on an unverified
    /// token** — the access token's signature is never checked to produce
    /// it, and nothing guarantees a Tesla access token is even a JWT. This
    /// is an **advisory cost guard that avoids paying for a call the server
    /// would certainly reject, not an authorization verdict**. The final
    /// authority is always the server's own 403 ([`Error::Api`] with
    /// `status == 403`) — see that variant's doc and
    /// [`Error::billing`]'s note on why a 403 is deliberately never folded
    /// into this variant (design §10).
    ///
    /// ⚠️ **There is deliberately no `held` field** (final fix wave, Minor
    /// 3). The pre-check that raises this is a *verdict, not a diagnostic*
    /// (REQ-18): it never carries the observed claims out, so the only value
    /// the single construction site could supply was an empty vector, and
    /// `Display` rendered it — a user holding `vehicle_device_data` but
    /// missing `vehicle_location` read `(held: [])` and reasonably concluded
    /// their token carried no scopes at all, the likely reaction being an
    /// unnecessary re-OAuth. A field that is always empty is worse than
    /// absent. This variant is `#[non_exhaustive]`, so adding a truthful
    /// `held` later — if the observation contract ever changes to report
    /// claims — is not a breaking change.
    #[error("missing required scope: {required:?}")]
    #[non_exhaustive]
    MissingScope {
        /// The scope expression the endpoint requires.
        required: ScopeExpr,
    },

    /// The request was not sent because the read token carries command scopes.
    ///
    /// A token refresh can widen the scopes up to what the app was consented for (observed
    /// 2026-10-09), so the read client fails closed before the request. No network call is
    /// made, so nothing is billed.
    #[error(
        "read token carries vehicle command scopes; remove them in the developer portal and refresh"
    )]
    CommandScopeOnReadToken,

    /// The response body could not be decoded into the expected type.
    ///
    /// Named `message` rather than the design doc's literal `source`:
    /// `thiserror::Error` unconditionally treats any field literally named
    /// `source` as the `std::error::Error::source()` chain (regardless of
    /// attributes), which requires that field to implement
    /// `std::error::Error` — incompatible with a plain `String`. Naming it
    /// `message`, consistent with every other variant's description field,
    /// carries the exact same redacted `String` the design specifies without
    /// exposing a foreign error type or source chain (design §9.1).
    #[error("failed to decode response (HTTP {status}): {message}")]
    #[non_exhaustive]
    Decode {
        /// A redacted description of the decode failure. `String`, not the
        /// originating `serde_json::Error`, so this public variant never
        /// exposes a foreign error type or its source chain (design §9.1).
        message: String,
        /// The HTTP status of the response that failed to decode.
        status: http::StatusCode,
        /// A redacted preview of the response body that failed to decode.
        /// Redaction is the caller's responsibility — see the note on
        /// [`Error::Api`]'s `body_preview` field.
        body_preview: String,
        /// The pricing category of the endpoint that was called.
        pricing: Pricing,
    },

    /// An authentication/authorization failure (a 401 from the API, or a
    /// token refresh that failed before any endpoint was called).
    ///
    /// Kept as its own variant rather than folded into [`Error::Api`] so the
    /// overwhelmingly common question a caller asks of a 401 — "do I need to
    /// re-authenticate?" — stays a one-arm pattern match.
    ///
    /// `pricing` is `Option` because the two ways to reach this variant
    /// differ (final fix wave / F3): a **401 response** was answered by a
    /// real endpoint, so it carries `Some(pricing)` and design §9.2's
    /// other-4xx row applies — a 401 from a `Pricing::Data` endpoint is
    /// [`Billing::Billed`], and reporting it as `NotBilled` (which this
    /// variant did unconditionally, having thrown the pricing away in
    /// `decode`) understates the caller's actual spend. A **pre-flight
    /// failure** such as a token refresh never reached a priced endpoint at
    /// all, so it carries `None` and is `NotBilled`. Design §9.1 already
    /// lists carrying `Pricing` into auth errors as intended work.
    #[error("authentication error: {message}")]
    #[non_exhaustive]
    Auth {
        /// A redacted, bounded description of the authentication failure
        /// (for a 401, the response body preview).
        message: String,
        /// The pricing category of the endpoint that was called, when this
        /// error came from an actual HTTP response. `None` when the failure
        /// happened before any endpoint was contacted (e.g. token refresh).
        pricing: Option<Pricing>,
    },
}

impl Error {
    /// Whether this error is safe to retry.
    ///
    /// Per design §9.3, only two cases qualify: a confirmed-unsent request
    /// ([`Error::TransportNotSent`]), and an [`Error::Api`] whose status is
    /// one of the transient 5xx codes 500, 502, 503, or 504. Everything
    /// else — including 408/[`Error::VehicleOffline`], 429, any other 5xx,
    /// [`Error::TransportUnknown`], and [`Error::Decode`] — is not
    /// retryable, to avoid duplicate billing or hammering a server that may
    /// already have processed the request.
    ///
    /// ⚠️ This judgment is stricter than it has to be — on the 14 unbilled
    /// endpoints, retrying [`Error::TransportUnknown`] would not cause
    /// duplicate billing (rate limiting still applies). It judges by status
    /// alone anyway, for three reasons: retry defaults to off, so there is
    /// no practical difference today; pulling the billing category into a
    /// safety rule would couple it to Tesla's pricing table, which can
    /// change out from under a client that isn't watching; and this method
    /// is public API, so narrowing its meaning later is a breaking change.
    pub fn is_retryable(&self) -> bool {
        match self {
            Error::TransportNotSent { .. } => true,
            Error::Api { status, .. } => matches!(status.as_u16(), 500 | 502 | 503 | 504),
            _ => false,
        }
    }

    /// The server-supplied retry delay, capped to 120 seconds.
    ///
    /// Only [`Error::Api`] can carry a `retry_after`; every other variant
    /// returns `None`.
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Error::Api { retry_after, .. } => retry_after.map(|d| d.min(RETRY_AFTER_CAP)),
            _ => None,
        }
    }

    /// Judges whether this outcome incurred a charge, per the design §9.2
    /// judgment table.
    ///
    /// `Pricing::NotCharged` endpoints are always [`Billing::NotBilled`]
    /// regardless of status. For `Pricing::Data` endpoints:
    /// - [`Error::TransportNotSent`] → [`Billing::NotBilled`] (provably unsent).
    /// - [`Error::TransportUnknown`] → [`Billing::Unknown`] (server processing unclear).
    /// - [`Error::Api`] with `status < 500` → [`Billing::Billed`]; `status >= 500` → [`Billing::NotBilled`].
    /// - [`Error::VehicleOffline`] (the 408 case) → [`Billing::Billed`].
    /// - [`Error::Decode`] with `status < 500` → [`Billing::Billed`]; `status >= 500` → [`Billing::NotBilled`].
    /// - [`Error::MissingScope`] carries no pricing context (the request is
    ///   never sent) → [`Billing::NotBilled`].
    /// - [`Error::Auth`] with `pricing: Some(Pricing::Data { .. })` → [`Billing::Billed`]
    ///   (final fix wave / F3): a 401 is an other-4xx answered by a real
    ///   endpoint, and design §9.2's table bills it. Every other shape —
    ///   `Some(Pricing::NotCharged)`, or `None` for a pre-flight failure such
    ///   as a token refresh that never reached an endpoint → [`Billing::NotBilled`].
    pub fn billing(&self) -> Billing {
        match self {
            Error::TransportNotSent { pricing, .. } => match pricing {
                Pricing::NotCharged => Billing::NotBilled,
                Pricing::Data { .. } => Billing::NotBilled,
            },
            Error::TransportUnknown { pricing, .. } => match pricing {
                Pricing::NotCharged => Billing::NotBilled,
                Pricing::Data { .. } => Billing::Unknown,
            },
            Error::Api {
                status, pricing, ..
            } => match pricing {
                Pricing::NotCharged => Billing::NotBilled,
                Pricing::Data { .. } => {
                    if status.as_u16() < 500 {
                        Billing::Billed
                    } else {
                        Billing::NotBilled
                    }
                }
            },
            Error::VehicleOffline { pricing, .. } => match pricing {
                Pricing::NotCharged => Billing::NotBilled,
                Pricing::Data { .. } => Billing::Billed,
            },
            Error::Decode {
                status, pricing, ..
            } => match pricing {
                Pricing::NotCharged => Billing::NotBilled,
                Pricing::Data { .. } => {
                    if status.as_u16() < 500 {
                        Billing::Billed
                    } else {
                        Billing::NotBilled
                    }
                }
            },
            Error::MissingScope { .. } | Error::CommandScopeOnReadToken => Billing::NotBilled,
            Error::Auth {
                pricing: Some(Pricing::Data { .. }),
                ..
            } => Billing::Billed,
            Error::Auth { .. } => Billing::NotBilled,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::{Billing, Pricing};
    use crate::scope::Scope;
    use std::time::Duration;

    #[test]
    fn not_charged_never_billed() {
        let e = Error::Api {
            status: http::StatusCode::OK,
            message: "".into(),
            retry_after: None,
            body_preview: "".into(),
            pricing: Pricing::NotCharged,
            txid: None,
        };
        assert_eq!(e.billing(), Billing::NotBilled);
    }

    /// A5 Task 1 reachability audit: [`Error::billing`] has fifteen match
    /// arms and the suite reached twelve. The three it never entered were the
    /// `Pricing::NotCharged` arms of [`Error::TransportNotSent`],
    /// [`Error::TransportUnknown`] and [`Error::Decode`] —
    /// `not_charged_never_billed` above pins only the [`Error::Api`] one, so
    /// "NotCharged is never billed" was asserted for a single variant and
    /// merely *assumed* for the rest of the matrix §8 names.
    ///
    /// The [`Error::TransportUnknown`] half is the load-bearing one. It is
    /// the only variant whose paid answer is [`Billing::Unknown`], so it is
    /// the only place where deleting the `NotCharged` arm changes the result
    /// — an unbilled endpoint would start reporting "possibly billed", which
    /// is exactly the ambiguity `Pricing::NotCharged` exists to rule out. The
    /// other two are asserted alongside it so the matrix is complete rather
    /// than complete-where-it-happened-to-differ.
    #[test]
    fn not_charged_is_never_billed_on_the_transport_and_decode_variants_either() {
        let not_sent = Error::TransportNotSent {
            message: String::new(),
            pricing: Pricing::NotCharged,
        };
        assert_eq!(not_sent.billing(), Billing::NotBilled);

        let unknown = Error::TransportUnknown {
            message: String::new(),
            pricing: Pricing::NotCharged,
        };
        // ⚠️ not `Billing::Unknown` — that is the *paid* answer for this
        // variant (`transport_unknown_paid_is_unknown_not_retryable`).
        assert_eq!(unknown.billing(), Billing::NotBilled);
        assert_ne!(unknown.billing(), Billing::Unknown);

        // Both sides of the `status < 500` split, so the arm is pinned
        // regardless of which branch the pricing check is written before.
        for status in [400_u16, 503] {
            let decode = Error::Decode {
                message: String::new(),
                status: http::StatusCode::from_u16(status).unwrap(),
                body_preview: String::new(),
                pricing: Pricing::NotCharged,
            };
            assert_eq!(decode.billing(), Billing::NotBilled, "status {status}");
        }
    }

    #[test]
    fn paid_success_billed_408_billed() {
        let ok = Error::Api {
            status: http::StatusCode::from_u16(408).unwrap(),
            message: "".into(),
            retry_after: None,
            body_preview: "".into(),
            pricing: Pricing::data(),
            txid: None,
        };
        assert_eq!(ok.billing(), Billing::Billed); // Data 408 = billed
        assert!(!ok.is_retryable()); // 408 재시도 금지
    }

    #[test]
    fn transport_unknown_paid_is_unknown_not_retryable() {
        let e = Error::TransportUnknown {
            message: "reset".into(),
            pricing: Pricing::data(),
        };
        assert_eq!(e.billing(), Billing::Unknown);
        assert!(!e.is_retryable());
    }

    #[test]
    fn transport_not_sent_retryable_not_billed() {
        let e = Error::TransportNotSent {
            message: "dns".into(),
            pricing: Pricing::data(),
        };
        assert_eq!(e.billing(), Billing::NotBilled);
        assert!(e.is_retryable());
    }

    #[test]
    fn retry_after_capped_120s() {
        let e = Error::Api {
            status: http::StatusCode::from_u16(429).unwrap(),
            message: "".into(),
            retry_after: Some(Duration::from_secs(999)),
            body_preview: "".into(),
            pricing: Pricing::NotCharged,
            txid: None,
        };
        assert_eq!(e.retry_after(), Some(Duration::from_secs(120)));
    }

    // --- Additional edge cases beyond the brief ---

    #[test]
    fn api_500_is_retryable() {
        let e = Error::Api {
            status: http::StatusCode::from_u16(500).unwrap(),
            message: "".into(),
            retry_after: None,
            body_preview: "".into(),
            pricing: Pricing::data(),
            txid: None,
        };
        assert!(e.is_retryable());
        assert_eq!(e.billing(), Billing::NotBilled);
    }

    #[test]
    fn api_all_transient_5xx_are_retryable() {
        for code in [500u16, 502, 503, 504] {
            let e = Error::Api {
                status: http::StatusCode::from_u16(code).unwrap(),
                message: "".into(),
                retry_after: None,
                body_preview: "".into(),
                pricing: Pricing::data(),
                txid: None,
            };
            assert!(e.is_retryable(), "expected {code} to be retryable");
        }
    }

    #[test]
    fn api_501_not_retryable() {
        let e = Error::Api {
            status: http::StatusCode::from_u16(501).unwrap(),
            message: "".into(),
            retry_after: None,
            body_preview: "".into(),
            pricing: Pricing::data(),
            txid: None,
        };
        assert!(!e.is_retryable());
        assert_eq!(e.billing(), Billing::NotBilled);
    }

    #[test]
    fn api_429_not_retryable_but_billed() {
        let e = Error::Api {
            status: http::StatusCode::from_u16(429).unwrap(),
            message: "".into(),
            retry_after: None,
            body_preview: "".into(),
            pricing: Pricing::data(),
            txid: None,
        };
        assert!(!e.is_retryable());
        assert_eq!(e.billing(), Billing::Billed);
    }

    #[test]
    fn retry_after_none_passes_through_as_none() {
        let e = Error::Api {
            status: http::StatusCode::from_u16(503).unwrap(),
            message: "".into(),
            retry_after: None,
            body_preview: "".into(),
            pricing: Pricing::data(),
            txid: None,
        };
        assert_eq!(e.retry_after(), None);
    }

    #[test]
    fn retry_after_uncapped_value_unchanged() {
        let e = Error::Api {
            status: http::StatusCode::from_u16(429).unwrap(),
            message: "".into(),
            retry_after: Some(Duration::from_secs(30)),
            body_preview: "".into(),
            pricing: Pricing::data(),
            txid: None,
        };
        assert_eq!(e.retry_after(), Some(Duration::from_secs(30)));
    }

    #[test]
    fn non_api_variants_have_no_retry_after() {
        assert_eq!(
            Error::TransportNotSent {
                message: "x".into(),
                pricing: Pricing::data(),
            }
            .retry_after(),
            None
        );
        assert_eq!(
            Error::VehicleOffline {
                pricing: Pricing::data(),
                txid: None,
            }
            .retry_after(),
            None
        );
        assert_eq!(
            Error::Auth {
                message: "x".into(),
                pricing: Some(Pricing::data()),
            }
            .retry_after(),
            None
        );
    }

    #[test]
    fn vehicle_offline_not_charged_is_not_billed() {
        let e = Error::VehicleOffline {
            pricing: Pricing::NotCharged,
            txid: None,
        };
        assert_eq!(e.billing(), Billing::NotBilled);
        assert!(!e.is_retryable());
    }

    #[test]
    fn vehicle_offline_data_is_billed() {
        let e = Error::VehicleOffline {
            pricing: Pricing::data(),
            txid: None,
        };
        assert_eq!(e.billing(), Billing::Billed);
        assert!(!e.is_retryable());
    }

    #[test]
    fn decode_status_below_500_is_billed() {
        let e = Error::Decode {
            message: "unexpected EOF".into(),
            status: http::StatusCode::OK,
            body_preview: "".into(),
            pricing: Pricing::data(),
        };
        assert_eq!(e.billing(), Billing::Billed);
        assert!(!e.is_retryable());
    }

    #[test]
    fn decode_status_5xx_is_not_billed() {
        let e = Error::Decode {
            message: "unexpected EOF".into(),
            status: http::StatusCode::from_u16(502).unwrap(),
            body_preview: "".into(),
            pricing: Pricing::data(),
        };
        assert_eq!(e.billing(), Billing::NotBilled);
    }

    #[test]
    fn missing_scope_is_not_billed_and_not_retryable() {
        let e = Error::MissingScope {
            required: ScopeExpr::One(Scope::VehicleDeviceData),
        };
        assert_eq!(e.billing(), Billing::NotBilled);
        assert!(!e.is_retryable());
    }

    /// Final fix wave (F3). The unpaid half of the old test: a pre-flight
    /// failure (token refresh) never reached an endpoint, carries no
    /// pricing, and is not billed.
    #[test]
    fn auth_is_not_billed_and_not_retryable() {
        let e = Error::Auth {
            message: "token refresh failed".into(),
            pricing: None,
        };
        assert_eq!(e.billing(), Billing::NotBilled);
        assert!(!e.is_retryable());
    }

    /// F3, the case that was wrong: a 401 answered by a **paid** endpoint is
    /// an other-4xx under design §9.2's table, so Tesla charged for it. The
    /// old `Auth(String)` variant threw the pricing away in `decode` and
    /// hardcoded `NotBilled`, understating the caller's spend on exactly the
    /// endpoint where spend matters.
    #[test]
    fn auth_401_on_paid_endpoint_is_billed() {
        let e = Error::Auth {
            message: "invalid bearer token".into(),
            pricing: Some(Pricing::data()),
        };
        assert_eq!(e.billing(), Billing::Billed);
        assert!(!e.is_retryable());
    }

    /// ...and the free-endpoint 401 stays `NotBilled`, so the fix is
    /// pricing-driven rather than "401 is always billed now".
    #[test]
    fn auth_401_on_not_charged_endpoint_is_not_billed() {
        let e = Error::Auth {
            message: "invalid bearer token".into(),
            pricing: Some(Pricing::NotCharged),
        };
        assert_eq!(e.billing(), Billing::NotBilled);
    }

    /// The paid-401 judgment must agree with the paid-403/404 judgment the
    /// same table produces via `Error::Api` — same status class, same
    /// endpoint pricing, same answer. If these ever disagree, one of the two
    /// arms has drifted from §9.2.
    #[test]
    fn auth_401_billing_agrees_with_api_other_4xx_billing() {
        let via_auth = Error::Auth {
            message: "".into(),
            pricing: Some(Pricing::data()),
        };
        let via_api = Error::Api {
            status: http::StatusCode::from_u16(403).unwrap(),
            message: "".into(),
            retry_after: None,
            body_preview: "".into(),
            pricing: Pricing::data(),
            txid: None,
        };
        assert_eq!(via_auth.billing(), via_api.billing());
        assert_eq!(via_auth.billing(), Billing::Billed);
    }
}

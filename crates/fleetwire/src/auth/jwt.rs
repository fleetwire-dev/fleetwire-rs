//! Unverified observation of the JWT payload — solely for the advisory scope pre-check.
//!
//! # This is not an authorization decision
//!
//! The values read here have **not had their signature verified**. Whoever controls the token
//! can put anything into `scp`, and then the request goes out as is and the server rejects it.
//! This module is therefore **a cost saving against accidental misconfiguration, not
//! adversarial cost-control**. The final authorization decision is always made by the Tesla
//! server.
//!
//! The safety is one-directional — if a scope is *observed* to be absent the request may be
//! skipped, but observing that it is present never leads to the conclusion that the
//! permission is held.
//!
//! # History of `expect(dead_code)` (removed in A3 Task 9)
//!
//! The items of this module are brought to life by the **request pipeline** (subproject A3) —
//! the [`super::TokenManager`] of A2 Task 7 deals only with the token lifecycle, so it did not
//! call the scope observation. The first call site of [`observe_scopes`] is
//! [`crate::client::Client::send`] (design §10.1 step 1) — when Task 9 wired it up, it removed
//! the `expect(dead_code)` that had remained on that function. The `allow` on
//! `MAX_JWT_PAYLOAD_BYTES` and [`UnverifiedScopeObservation`] remains — those two are not the
//! root of the dead subgraph but items hanging off it, so the dead_code analysis of 1.89.0
//! does not report them individually (although once the root, `observe_scopes`, alone is
//! cleared these two in fact come alive as well), and switching to `expect` produces an
//! "unfulfilled lint expectation" warning, so the `allow` form is kept.

use crate::scope::{Scope, ScopeExpr};
use base64::Engine as _;
use core::str::FromStr as _;

/// The upper bound for decoding the payload. Anything larger is not parsed and yields
/// [`UnverifiedScopeObservation::Indeterminate`].
pub(crate) const MAX_JWT_PAYLOAD_BYTES: usize = 8192;

/// Whether the required scopes were observed to be satisfied in an unverified token.
///
/// **Only `DoesNotContainRequiredScope` causes a request to be skipped.** The other two both
/// send the request — fail-open made into a type rather than a convention.
///
/// The variants **carry no data** (REQ-18). This type is a verdict, not a diagnostic, and
/// if values read from the token (scope strings, claims) were carried here they would flow
/// straight out into logs and errors. `the_verdict_carries_no_token_derived_data` holds this
/// in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub(crate) enum UnverifiedScopeObservation {
    /// The required scope was observed. **This does not mean the permission is held.**
    ContainsRequiredScope,
    /// The required scope is definitely absent. The only case in which the request may be
    /// skipped.
    DoesNotContainRequiredScope,
    /// No determination can be made — an opaque token, a parse failure, an unexpected
    /// structure and so on. The request is sent.
    Indeterminate,
}

/// The shared decoder that reads the `scp` string array of the payload.
///
/// It returns `Some` only when all of these conditions hold: 3 segments, the pre-decode size
/// bound, and a non-empty array of strings; everything else is `None` (= indeterminate). It
/// is kept in one place so that [`observe_scopes`] and [`observe_forbidden`] use the same
/// rules.
fn scp_items(access_token: &str) -> Option<Vec<String>> {
    // A compact JWS has exactly 3 segments. Anything else is not judged — 2 segments, and
    // equally 4 segments (a stray dot inside the payload, or a fragment of a 5-segment JWE).
    // The final `None` binding enforces "there must be no 4th". Without it, the `b` of
    // `a.b.c.d` would be misread as the payload.
    let mut parts = access_token.split('.');
    let (Some(_header), Some(payload_b64), Some(_sig), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };

    // The bound is checked **before** decoding — so as not to allocate a buffer as long as
    // the string the caller gave. base64 shrinks to 3/4, so a payload that passes this check
    // is at most 6144B.
    if payload_b64.len() > MAX_JWT_PAYLOAD_BYTES {
        return None;
    }

    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let Ok(payload) = engine.decode(payload_b64) else {
        return None;
    };
    // The decoded length is **an invariant, not a check**. The encoding bound above
    // guarantees `payload_b64.len() <= MAX`, and URL_SAFE_NO_PAD decodes n characters into
    // `floor(n*3/4)` bytes, so a payload that reaches here is at most 6144B.
    //
    // This relation **does not break even if the constant is changed** — both checks read
    // the same constant, so raising `MAX_JWT_PAYLOAD_BYTES` raises the encoding gate along
    // with it. A runtime branch here (`if ... { return Indeterminate }`) would therefore be
    // a dead branch that no input can reach. For the invariant to actually break, either
    // (a) a **separate** constant would have to appear on the encoding side or (b) the
    // decoder would have to be swapped for one whose expansion ratio exceeds 4/3. It is
    // better for those two to break loudly in a test than to be quietly passed off as
    // Indeterminate — hence a `debug_assert!`, not a branch.
    debug_assert!(
        payload.len() <= MAX_JWT_PAYLOAD_BYTES,
        "the invariant that the encoding bound implies the decoded length is broken"
    );
    // Do not change this early return into `.unwrap_or(Value::Null)`. For now `value` is
    // used for `scp` alone, so the result is the same thanks to `Null.get("scp") == None`,
    // but the equivalence breaks the moment a second claim such as `exp`/`aud` is read — a
    // token that failed to parse turns into "a token with every claim absent" and yields a
    // different verdict per claim.
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&payload) else {
        return None;
    };

    // scp must be an array of strings (measured 2026-07-31). Any other shape is not judged.
    // In particular, **an absent claim is not read as "no scopes"** — it can be seen that
    // way only when the issuer profile states "absent = no scopes", and we have no such
    // statement.
    let items = value.get("scp").and_then(serde_json::Value::as_array)?;
    // **An empty array is Indeterminate too, like an absent claim.** The only authority this
    // feature has is "skip when the required scope is specifically absent"; it has no
    // authority to interpret a degenerate claim shape. `[]` is degenerate in the same way as
    // absence — nobody issues an OAuth token with no scopes at all on purpose, so this is
    // not a statement by the issuer but a sign of an upstream anomaly.
    //
    // What is decisive is that the cost of an error is asymmetric. A wrong skip throws at
    // the user a `MissingScope` that we fabricated from an unverified claim, **with no
    // network call and no server response** — there is no way to check it. A wrong
    // Indeterminate costs no more than one request and a readable 403 (~$0.002 on a billed
    // endpoint).
    //
    // `["openid"]` is different. There the issuer has **stated a set of scopes that excludes
    // ours**, and `DoesNotContainRequiredScope` is the value for exactly that case.
    if items.is_empty() {
        return None;
    }
    // This is `any(!is_string)` — not `all`. If even one element is not a string, the meaning
    // of the array is unknown. Picking only the strings out of a mixed array such as
    // `["vehicle_device_data", 1]` would be pretending to understand a structure we did not
    // understand.
    if items.iter().any(|v| !v.is_string()) {
        return None;
    }
    Some(
        items
            .iter()
            .filter_map(serde_json::Value::as_str)
            .map(str::to_owned)
            .collect(),
    )
}

/// Observes the `scp` claim of an access token without verification and decides whether the
/// required scopes are satisfied.
///
/// This function **does not decide** whether the token is a JWT — it is called only when the
/// caller has opted in. An opaque token can happen to decode like a JWT, and mistaking it for
/// a JWT and blocking the request would break the fail-open principle. For the same reason
/// the header's `alg`/`typ` are not examined either — they are unverified values, so they
/// give no grounds for concluding "this is / is not a JWT".
pub(crate) fn observe_scopes(
    access_token: &str,
    required: &ScopeExpr,
) -> UnverifiedScopeObservation {
    use UnverifiedScopeObservation::Indeterminate;

    let Some(items) = scp_items(access_token) else {
        return Indeterminate;
    };

    // Unmodeled scopes (e.g. offline_access) are dropped. The required scopes always come
    // from our endpoint types and so are known values, and dropping unknown items does not
    // make the verdict wrong.
    //
    // This is the point of asymmetry: strict about the **type** of the elements (a single
    // non-string means Indeterminate) while lenient about the scope **values**. That
    // leniency was forced by the evidence — if `offline_access` could not be dropped, every
    // real token would collapse into Indeterminate and the feature would be dead. The
    // failure mode paid as the price is noted here: **if Tesla renames a modeled scope or
    // gives it an alias, a permission that is still granted silently becomes a wrong skip.**
    // (If `vehicle_device_data` gets a new name, it disappears from held and the requirement
    // cannot be satisfied.) When a sign arrives that a scope name has changed, update
    // `Scope::from_str` first.
    let held: Vec<Scope> = items
        .iter()
        .filter_map(|s| Scope::from_str(s).ok())
        .collect();

    if required.is_satisfied_by(&held) {
        UnverifiedScopeObservation::ContainsRequiredScope
    } else {
        UnverifiedScopeObservation::DoesNotContainRequiredScope
    }
}

/// The command scope names that must not be present on a read token. These names are kept in
/// this file only.
const FORBIDDEN_COMMAND_SCOPES: [&str; 3] =
    ["vehicle_cmds", "vehicle_charging_cmds", "energy_cmds"];

/// Observes whether the `scp` of an unverified token contains at least one command scope.
///
/// The result is `ContainsRequiredScope` if there is one, `DoesNotContainRequiredScope` if a
/// readable array contains none, and `Indeterminate` if it cannot be read. As with the other
/// verdicts of this module, `Indeterminate` does not block the request (fail-open).
pub(crate) fn observe_forbidden(access_token: &str) -> UnverifiedScopeObservation {
    let Some(items) = scp_items(access_token) else {
        return UnverifiedScopeObservation::Indeterminate;
    };
    if items
        .iter()
        .any(|s| FORBIDDEN_COMMAND_SCOPES.contains(&s.as_str()))
    {
        UnverifiedScopeObservation::ContainsRequiredScope
    } else {
        UnverifiedScopeObservation::DoesNotContainRequiredScope
    }
}

#[cfg(test)]
mod tests {
    // `use base64::Engine as _;` is not placed here — `use super::*` already pulls in the
    // module's same import, so it would be a duplicate, and this crate builds with
    // `-D warnings`, so `unused_imports` is a build failure.
    use super::*;
    use crate::scope::{Scope, ScopeExpr};

    /// Builds a JWT for tests. The signature is not verified, so any value is put in.
    fn jwt_with_payload(payload: &serde_json::Value) -> String {
        let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let header = engine.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let body = engine.encode(serde_json::to_vec(payload).expect("payload"));
        format!("{header}.{body}.not-a-real-signature")
    }

    fn required_device_data() -> ScopeExpr {
        ScopeExpr::One(Scope::VehicleDeviceData)
    }

    #[test]
    fn forbidden_command_scopes_are_observed() {
        let t = jwt_with_payload(&serde_json::json!({
            "scp": ["openid", "vehicle_device_data", "vehicle_charging_cmds"]
        }));
        assert_eq!(
            observe_forbidden(&t),
            UnverifiedScopeObservation::ContainsRequiredScope
        );
        let ok = jwt_with_payload(&serde_json::json!({
            "scp": ["openid", "vehicle_device_data", "vehicle_location"]
        }));
        assert_eq!(
            observe_forbidden(&ok),
            UnverifiedScopeObservation::DoesNotContainRequiredScope
        );
        for name in ["vehicle_cmds", "energy_cmds"] {
            let t = jwt_with_payload(&serde_json::json!({ "scp": ["openid", name] }));
            assert_eq!(
                observe_forbidden(&t),
                UnverifiedScopeObservation::ContainsRequiredScope
            );
        }
    }

    #[test]
    fn forbidden_check_is_indeterminate_when_unreadable() {
        for t in [
            "opaque".to_owned(),
            jwt_with_payload(&serde_json::json!({})),
            jwt_with_payload(&serde_json::json!({ "scp": [] })),
            jwt_with_payload(&serde_json::json!({ "scp": ["vehicle_cmds", 1] })),
        ] {
            assert_eq!(
                observe_forbidden(&t),
                UnverifiedScopeObservation::Indeterminate
            );
        }
    }

    #[test]
    fn real_tesla_scp_shape_is_recognized_including_unmodeled_scopes() {
        // The scp of the token measured on 2026-07-31, as is. offline_access is not modeled,
        // but the verdict must not collapse into Indeterminate because of it.
        let token = jwt_with_payload(&serde_json::json!({
            "scp": ["offline_access", "openid", "vehicle_device_data", "vehicle_location"]
        }));
        assert_eq!(
            observe_scopes(&token, &required_device_data()),
            UnverifiedScopeObservation::ContainsRequiredScope
        );
    }

    #[test]
    fn missing_required_scope_is_the_only_path_that_skips_a_request() {
        let token = jwt_with_payload(&serde_json::json!({ "scp": ["openid"] }));
        assert_eq!(
            observe_scopes(&token, &required_device_data()),
            UnverifiedScopeObservation::DoesNotContainRequiredScope
        );
    }

    #[test]
    fn every_ambiguous_shape_is_indeterminate_not_a_skip() {
        let opaque = "NA_9f2b1c7d8e4a5b6c7d8e9f0a";
        let two_segments = "eyJhbGciOiJSUzI1NiJ9.eyJzY3AiOltdfQ";
        let bad_base64 = "eyJhbGciOiJSUzI1NiJ9.!!!not-base64!!!.sig";
        let not_json = {
            let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
            format!("eyJhbGciOiJSUzI1NiJ9.{}.sig", engine.encode(b"plain text"))
        };
        let scp_absent = jwt_with_payload(&serde_json::json!({ "sub": "abc" }));
        let scp_is_string = jwt_with_payload(&serde_json::json!({ "scp": "vehicle_device_data" }));
        let scp_has_non_strings = jwt_with_payload(&serde_json::json!({ "scp": [1, 2] }));

        for (label, token) in [
            ("opaque token", opaque.to_owned()),
            ("two segments", two_segments.to_owned()),
            ("undecodable payload", bad_base64.to_owned()),
            ("payload is not json", not_json),
            ("scp claim absent", scp_absent),
            ("scp is a string not an array", scp_is_string),
            ("scp holds non-strings", scp_has_non_strings),
        ] {
            assert_eq!(
                observe_scopes(&token, &required_device_data()),
                UnverifiedScopeObservation::Indeterminate,
                "{label} must be Indeterminate — blocking the request breaks fail-open"
            );
        }
    }

    /// A payload over the bound is Indeterminate.
    ///
    /// This test reaches **only as far as the encoding gate** — the property "caught before
    /// allocation" is not proven here. That is owned by
    /// [`the_length_bound_is_checked_on_the_encoded_form_before_any_buffer_is_allocated`]
    /// (because this payload's decoded length is over the bound too, so it is caught by
    /// either of the two locations).
    #[test]
    fn an_oversized_payload_is_indeterminate() {
        let huge = jwt_with_payload(&serde_json::json!({
            "scp": ["vehicle_device_data"],
            "padding": "A".repeat(MAX_JWT_PAYLOAD_BYTES * 2),
        }));
        assert_eq!(
            observe_scopes(&huge, &required_device_data()),
            UnverifiedScopeObservation::Indeterminate
        );
    }

    #[test]
    fn all_expression_requires_every_scope() {
        let token = jwt_with_payload(&serde_json::json!({
            "scp": ["vehicle_device_data"]
        }));
        // ScopeExpr::All takes a Vec<ScopeExpr> — not a Vec<Scope>.
        let required = ScopeExpr::All(vec![
            ScopeExpr::One(Scope::VehicleDeviceData),
            ScopeExpr::One(Scope::VehicleLocation),
        ]);
        assert_eq!(
            observe_scopes(&token, &required),
            UnverifiedScopeObservation::DoesNotContainRequiredScope
        );
    }

    /// `Any` was not covered by the briefing, but it is one of the three variants of
    /// `is_satisfied_by` — if only `All`/`One` are covered, every test in this module passes
    /// even when the `Any` wiring is inverted.
    #[test]
    fn any_expression_is_satisfied_by_a_single_held_scope() {
        let token = jwt_with_payload(&serde_json::json!({ "scp": ["vehicle_location"] }));
        let either = ScopeExpr::Any(vec![
            ScopeExpr::One(Scope::VehicleDeviceData),
            ScopeExpr::One(Scope::VehicleLocation),
        ]);
        assert_eq!(
            observe_scopes(&token, &either),
            UnverifiedScopeObservation::ContainsRequiredScope
        );
        // The opposite direction is checked too — this kills the mutant in which `Any`
        // unconditionally yields true.
        let neither = ScopeExpr::Any(vec![
            ScopeExpr::One(Scope::UserData),
            ScopeExpr::One(Scope::Openid),
        ]);
        assert_eq!(
            observe_scopes(&token, &neither),
            UnverifiedScopeObservation::DoesNotContainRequiredScope
        );
    }

    /// Pins that the **number** of segments is actually counted.
    ///
    /// An implementation using `splitn(3, '.')` or `split('.').nth(1)` still reads the payload
    /// correctly at index 1, so it misreads 4- and 5-segment input as 3 segments. That
    /// misreading is not a fail-open violation but a case of "pretending to understand a
    /// structure we did not understand" — the same as reading a ciphertext fragment of a JWE
    /// (5 segments) as claims.
    #[test]
    fn only_a_three_segment_token_is_read_at_all() {
        let three = jwt_with_payload(&serde_json::json!({ "scp": ["vehicle_device_data"] }));
        // Control: the same payload is read when it has 3 segments. Without this, the
        // assertions below would be a vacuous test that passes because "everything is
        // Indeterminate anyway".
        assert_eq!(
            observe_scopes(&three, &required_device_data()),
            UnverifiedScopeObservation::ContainsRequiredScope
        );
        for (label, token) in [
            ("four segments", format!("{three}.extra")),
            ("five segments (JWE shape)", format!("{three}.extra.more")),
        ] {
            assert_eq!(
                observe_scopes(&token, &required_device_data()),
                UnverifiedScopeObservation::Indeterminate,
                "{label} is not a compact JWS — index 1 must not be read as the payload"
            );
        }
    }

    /// Pins that the bound is checked on the **encoded form**, that is, before a buffer is
    /// allocated.
    ///
    /// base64 expands by 4/3, so there is a range in which "the encoded length is over the
    /// bound and the decoded length is at or under the bound". An implementation that checks
    /// only after decoding lets this input through and returns `ContainsRequiredScope` — by
    /// then that implementation has already allocated a buffer larger than the bound.
    #[test]
    fn the_length_bound_is_checked_on_the_encoded_form_before_any_buffer_is_allocated() {
        let token = jwt_with_payload(&serde_json::json!({
            "scp": ["vehicle_device_data"],
            "padding": "A".repeat(MAX_JWT_PAYLOAD_BYTES * 7 / 8),
        }));
        let payload_b64 = token.split('.').nth(1).expect("payload segment");
        let decoded_len = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload_b64)
            .expect("decodable")
            .len();
        // This test is meaningful only while it is inside the targeted range. It checks
        // itself so that if the constant changes and it leaves the range, it breaks here
        // instead of silently becoming powerless.
        assert!(
            payload_b64.len() > MAX_JWT_PAYLOAD_BYTES,
            "the encoded length must exceed the bound for this test to be meaningful"
        );
        assert!(
            decoded_len <= MAX_JWT_PAYLOAD_BYTES,
            "the decoded length must be at or under the bound — otherwise a check after \
             decoding would catch this input too"
        );
        assert_eq!(
            observe_scopes(&token, &required_device_data()),
            UnverifiedScopeObservation::Indeterminate
        );
    }

    /// An empty array and an absent claim are **degenerate in the same way** — both are
    /// Indeterminate.
    ///
    /// The only authority this feature has is "skip when the required scope is specifically
    /// absent"; it has no authority to interpret a degenerate claim shape. A wrong skip
    /// produces a fabricated `MissingScope` with no server response and no way to check it,
    /// whereas a wrong Indeterminate costs no more than one request and a readable 403.
    ///
    /// `["openid"]` is examined alongside as a control — there the issuer has **stated** a
    /// set of scopes that excludes ours, so `DoesNotContain` is correct. Without it this
    /// would be a vacuous test that "passes if everything is made Indeterminate".
    #[test]
    fn an_empty_scp_array_is_as_indeterminate_as_an_absent_claim() {
        let empty = jwt_with_payload(&serde_json::json!({ "scp": [] }));
        assert_eq!(
            observe_scopes(&empty, &required_device_data()),
            UnverifiedScopeObservation::Indeterminate,
            "an empty array is read as a sign of an upstream anomaly, not as a statement by \
             the issuer"
        );
        let absent = jwt_with_payload(&serde_json::json!({ "sub": "abc" }));
        assert_eq!(
            observe_scopes(&absent, &required_device_data()),
            UnverifiedScopeObservation::Indeterminate
        );
        // Control: a set that is non-empty and lacks only ours is still a skip.
        let stated = jwt_with_payload(&serde_json::json!({ "scp": ["openid"] }));
        assert_eq!(
            observe_scopes(&stated, &required_device_data()),
            UnverifiedScopeObservation::DoesNotContainRequiredScope
        );
    }

    /// With `["scp": [1, 2]]` alone, the mutant that changes `any(|v| !v.is_string())` to
    /// `all(...)` survives — every element is a non-string, so the two predicates have the
    /// same value. Only a mixed array kills that mutant.
    #[test]
    fn an_array_mixing_strings_and_non_strings_is_indeterminate() {
        let token = jwt_with_payload(&serde_json::json!({ "scp": ["vehicle_device_data", 1] }));
        assert_eq!(
            observe_scopes(&token, &required_device_data()),
            UnverifiedScopeObservation::Indeterminate,
            "picking only the strings out of a structure we did not understand is pretending \
             to understand it"
        );
    }

    /// Controller decision 1: whether a token is a JWT is not decided from the header
    /// (`alg`/`typ`). The header is unverified input and so gives no grounds, and blocking a
    /// request on it breaks fail-open. An implementation that inspects the header fails on
    /// all three below.
    #[test]
    fn the_header_is_never_inspected() {
        let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let body = engine.encode(
            serde_json::to_vec(&serde_json::json!({ "scp": ["vehicle_device_data"] }))
                .expect("payload"),
        );
        for header in [
            "!!not-base64-at-all!!".to_owned(),
            engine.encode(b"plain text"),
            engine.encode(br#"{"alg":"none"}"#),
        ] {
            assert_eq!(
                observe_scopes(&format!("{header}.{body}.sig"), &required_device_data()),
                UnverifiedScopeObservation::ContainsRequiredScope,
                "the verdict must not change based on the header"
            );
        }
    }

    /// A JWT uses **URL-safe** base64 (`-`/`_`, no padding). An implementation that decodes
    /// with the standard alphabet (`+`/`/`) drops every payload containing those two
    /// characters into a decode failure and returns Indeterminate — being fail-open this is
    /// not dangerous, but for a considerable share of real tokens this feature is **silently
    /// disabled**. The payloads of the other tests happened not to contain the two characters
    /// and so did not catch the alphabet mutant (mutant M12).
    #[test]
    fn the_payload_is_decoded_with_the_url_safe_alphabet() {
        let token = jwt_with_payload(&serde_json::json!({
            "scp": ["vehicle_device_data"],
            "padding": "?>".repeat(6),
        }));
        let payload_b64 = token.split('.').nth(1).expect("payload segment");
        // Self-check: this payload must really contain URL-safe-only characters to be
        // meaningful.
        assert!(
            payload_b64.contains('-') && payload_b64.contains('_'),
            "this test is vacuous if the payload has no URL-safe-only characters: {payload_b64}"
        );
        assert_eq!(
            observe_scopes(&token, &required_device_data()),
            UnverifiedScopeObservation::ContainsRequiredScope
        );
    }

    /// Pins only that **the verdict type carries no data**.
    ///
    /// This must not be read as REQ-18 being fully discharged in this module. The only
    /// thing prevented here is "token-derived values being carried out in a variant"; if
    /// someone later puts a claim into a `tracing` call or a `panic!` message, this test
    /// stays silent. The fact that this module currently has no logging at all takes care of
    /// the rest.
    ///
    /// A fieldless enum is 1 byte — attaching a `String`/`Vec` breaks the first assertion.
    #[test]
    fn the_verdict_carries_no_token_derived_data() {
        assert_eq!(
            size_of::<UnverifiedScopeObservation>(),
            1,
            "if a variant carries data, that value soon flows out into logs and errors"
        );
        let token = jwt_with_payload(&serde_json::json!({
            "scp": ["offline_access", "vehicle_device_data"]
        }));
        assert_eq!(
            format!("{:?}", observe_scopes(&token, &required_device_data())),
            "ContainsRequiredScope"
        );
    }
}

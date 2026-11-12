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
/// ⚠️ The variants **carry no data** (REQ-18). This type is a verdict, not a diagnostic, and
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
    // compact JWS 는 정확히 3 세그먼트다. 그 외는 판단하지 않는다 — 2 세그먼트도,
    // 4 세그먼트(payload 안의 유실된 점, 혹은 5 세그먼트 JWE 의 조각)도 마찬가지다.
    // 마지막 `None` 바인딩이 "4번째가 없어야 한다" 를 강제한다. 이것이 없으면
    // `a.b.c.d` 의 `b` 를 payload 로 오독하게 된다.
    let mut parts = access_token.split('.');
    let (Some(_header), Some(payload_b64), Some(_sig), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return None;
    };

    // 디코드 **전에** 상한을 본다 — 호출자가 준 문자열 길이만큼 버퍼를 잡지 않기
    // 위해서다. base64 는 3/4 로 줄어드니 이 검사를 통과한 payload 는 6144B 이하다.
    if payload_b64.len() > MAX_JWT_PAYLOAD_BYTES {
        return None;
    }

    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let Ok(payload) = engine.decode(payload_b64) else {
        return None;
    };
    // 디코드 결과 길이는 **불변식이지 검사가 아니다**. 위 인코딩 상한이
    // `payload_b64.len() <= MAX` 를 보장하고 URL_SAFE_NO_PAD 는 n 글자를
    // `floor(n*3/4)` 바이트로 디코드하므로 여기 도달한 payload 는 최대 6144B 다.
    //
    // ⚠️ 이 관계는 **상수를 바꿔도 깨지지 않는다** — 두 검사가 같은 상수를 읽기
    // 때문에 `MAX_JWT_PAYLOAD_BYTES` 를 올리면 인코딩 게이트도 같이 올라간다.
    // 따라서 여기에 런타임 분기(`if ... { return Indeterminate }`)를 두면 어떤
    // 입력으로도 도달할 수 없는 죽은 가지가 된다. 불변식이 실제로 깨지려면
    // (a) 인코딩 쪽에 **별도의** 상수가 생기거나 (b) 팽창률이 4/3 을 넘는 디코더로
    // 갈아타야 한다. 그 둘은 조용히 Indeterminate 로 흘리는 것보다 테스트에서
    // 시끄럽게 깨지는 편이 낫다 — 그래서 분기가 아니라 `debug_assert!` 다.
    debug_assert!(
        payload.len() <= MAX_JWT_PAYLOAD_BYTES,
        "인코딩 상한이 디코딩 길이를 함의한다는 불변식이 깨졌다"
    );
    // ⚠️ 이 early return 을 `.unwrap_or(Value::Null)` 로 바꾸지 말 것. 지금은
    // `value` 를 `scp` 하나에만 쓰므로 `Null.get("scp") == None` 덕에 결과가 같지만,
    // `exp`/`aud` 등 두 번째 클레임을 읽는 순간 등가성이 깨진다 — 파싱 실패한 토큰이
    // "모든 클레임이 부재한 토큰" 으로 둔갑해 클레임별로 다른 판정을 낳는다.
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&payload) else {
        return None;
    };

    // scp 는 문자열 배열이어야 한다(2026-07-31 실측). 그 외 형태는 판단하지 않는다.
    // 특히 **클레임 부재를 "스코프 없음" 으로 읽지 않는다** — 발급자 프로필이
    // "부재 = 스코프 없음" 을 명시할 때만 그렇게 볼 수 있고, 우리는 그 명시가 없다.
    let items = value.get("scp").and_then(serde_json::Value::as_array)?;
    // **빈 배열도 부재와 같이 Indeterminate 다.** 이 기능의 권한은 "요구 스코프가
    // 콕 집어 없을 때 건너뛴다" 뿐이고, 퇴화한 클레임 모양을 해석할 권한은 없다.
    // `[]` 는 부재와 같은 종류로 퇴화해 있다 — 스코프가 하나도 없는 OAuth 토큰을
    // 일부러 발급하는 사람은 없으므로, 이건 발급자의 진술이 아니라 상류 이상 신호다.
    //
    // 오류 비용이 비대칭이라는 점이 결정적이다. 잘못된 skip 은 **네트워크 호출도
    // 서버 응답도 없이** 우리가 검증되지 않은 클레임으로 지어낸 `MissingScope` 를
    // 사용자에게 던진다 — 확인할 방법이 없다. 잘못된 Indeterminate 는 요청 한 번과
    // 읽을 수 있는 403(과금 엔드포인트면 ~$0.002)이 전부다.
    //
    // `["openid"]` 는 다르다. 그건 발급자가 **우리 것을 제외한 스코프 집합을 진술한**
    // 것이고, `DoesNotContainRequiredScope` 는 정확히 그 경우를 위한 값이다.
    if items.is_empty() {
        return None;
    }
    // `any(!is_string)` 이다 — `all` 이 아니다. 하나라도 문자열이 아니면 그 배열의
    // 의미를 모른다. `["vehicle_device_data", 1]` 처럼 섞인 배열에서 문자열만
    // 골라 읽으면, 우리가 이해하지 못한 구조를 이해한 척하는 것이 된다.
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

    // 모델링되지 않은 스코프(예: offline_access)는 버린다. 요구 스코프는 항상
    // 우리 엔드포인트 타입에서 오므로 알려진 값이고, 미지 항목을 버려도 판정이 틀리지 않는다.
    //
    // ⚠️ 여기가 원소 **타입**에는 엄격(비문자열 하나면 Indeterminate)하면서 스코프
    // **값**에는 관대한 비대칭 지점이다. 이 관대함은 증거가 강제한 것이다 —
    // `offline_access` 를 버리지 못하면 실 토큰 전부가 Indeterminate 로 무너져
    // 기능이 죽는다. 대가로 치르는 실패 모드를 적어 둔다: **Tesla 가 모델링된
    // 스코프를 개명하거나 별칭을 주면, 여전히 부여된 권한이 조용히 잘못된 skip 이
    // 된다.** (`vehicle_device_data` → 새 이름이면 held 에서 사라져 요구를 못 만족.)
    // 스코프 이름이 바뀌었다는 신호가 오면 `Scope::from_str` 을 먼저 갱신할 것.
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
    // ⚠️ `use base64::Engine as _;` 를 여기 두지 않는다 — `use super::*` 가 모듈의
    // 같은 import 를 이미 끌어오므로 중복이 되고, 이 크레이트는 `-D warnings` 라
    // `unused_imports` 가 곧 빌드 실패다.
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
        // 2026-07-31 실측 토큰의 scp 그대로. offline_access 는 모델링돼 있지 않지만
        // 그것 때문에 판정이 Indeterminate 로 무너지면 안 된다.
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
                "{label} 은 Indeterminate 여야 한다 — 요청을 막으면 fail-open 이 깨진다"
            );
        }
    }

    /// A payload over the bound is Indeterminate.
    ///
    /// ⚠️ This test reaches **only as far as the encoding gate** — the property "caught before
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
        // ⚠️ ScopeExpr::All 은 Vec<ScopeExpr> 를 받는다 — Vec<Scope> 가 아니다.
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
        // 반대 방향도 본다 — `Any` 가 무조건 참을 내는 변이를 죽인다.
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
        // 대조군: 같은 payload 가 3 세그먼트일 때는 읽힌다. 이게 없으면 아래 단언들이
        // "어차피 다 Indeterminate" 라는 이유로 통과하는 공허한 테스트가 된다.
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
                "{label} 은 compact JWS 가 아니다 — 인덱스 1 을 payload 로 읽으면 안 된다"
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
        // 이 테스트는 겨냥한 구간 안에 있을 때만 의미가 있다. 상수가 바뀌어 구간을
        // 벗어나면 조용히 무력해지는 대신 여기서 깨지도록 스스로 확인한다.
        assert!(
            payload_b64.len() > MAX_JWT_PAYLOAD_BYTES,
            "인코딩 길이가 상한을 넘어야 이 테스트가 의미를 갖는다"
        );
        assert!(
            decoded_len <= MAX_JWT_PAYLOAD_BYTES,
            "디코딩 길이는 상한 이하여야 한다 — 아니면 디코드 후 검사도 이 입력을 잡아 버린다"
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
            "빈 배열은 발급자의 진술이 아니라 상류 이상 신호로 읽는다"
        );
        let absent = jwt_with_payload(&serde_json::json!({ "sub": "abc" }));
        assert_eq!(
            observe_scopes(&absent, &required_device_data()),
            UnverifiedScopeObservation::Indeterminate
        );
        // 대조군: 비어 있지 않고 우리 것만 없는 집합은 여전히 skip 이다.
        let stated = jwt_with_payload(&serde_json::json!({ "scp": ["openid"] }));
        assert_eq!(
            observe_scopes(&stated, &required_device_data()),
            UnverifiedScopeObservation::DoesNotContainRequiredScope
        );
    }

    /// ⚠️ With `["scp": [1, 2]]` alone, the mutant that changes `any(|v| !v.is_string())` to
    /// `all(...)` survives — every element is a non-string, so the two predicates have the
    /// same value. Only a mixed array kills that mutant.
    #[test]
    fn an_array_mixing_strings_and_non_strings_is_indeterminate() {
        let token = jwt_with_payload(&serde_json::json!({ "scp": ["vehicle_device_data", 1] }));
        assert_eq!(
            observe_scopes(&token, &required_device_data()),
            UnverifiedScopeObservation::Indeterminate,
            "이해하지 못한 구조에서 문자열만 골라 읽으면 이해한 척하는 것이 된다"
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
                "헤더를 보고 판정을 바꾸면 안 된다"
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
        // 자기 검증: 이 payload 가 실제로 URL-safe 전용 글자를 담고 있어야 의미가 있다.
        assert!(
            payload_b64.contains('-') && payload_b64.contains('_'),
            "payload 에 URL-safe 전용 글자가 없으면 이 테스트는 공허하다: {payload_b64}"
        );
        assert_eq!(
            observe_scopes(&token, &required_device_data()),
            UnverifiedScopeObservation::ContainsRequiredScope
        );
    }

    /// Pins only that **the verdict type carries no data**.
    ///
    /// ⚠️ This must not be read as REQ-18 being fully discharged in this module. The only
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
            "변종에 데이터를 실으면 그 값은 곧 로그·오류로 흘러나간다"
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

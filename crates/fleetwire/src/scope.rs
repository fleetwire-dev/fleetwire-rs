//! OAuth scopes and scope-satisfaction expressions.

use serde::{Deserialize, Serialize};

/// A Tesla Fleet API OAuth scope.
///
/// The serde representation is the **wire** form — Tesla's OAuth `scope`
/// string is space-delimited `snake_case` (`vehicle_device_data
/// vehicle_location openid`), and a token response is where these values are
/// actually parsed from. Final fix wave (F4): without
/// `rename_all = "snake_case"` the derive emitted serde's default variant
/// name (`"VehicleDeviceData"`), which round-trips against itself but
/// matches nothing Tesla ever sends or accepts. It went unnoticed because
/// none of this module's tests serialized or deserialized a `Scope` at all —
/// see `serde_wire_form_is_snake_case_for_every_variant`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Scope {
    /// `vehicle_device_data` — vehicle telemetry/data.
    VehicleDeviceData,
    /// `vehicle_location` — precise location (double opt-in).
    VehicleLocation,
    /// `user_data` — user-context endpoints.
    UserData,
    /// `openid` — base identity.
    Openid,
}

/// The error returned when a wire string is none of the modeled [`Scope`]s.
///
/// Tesla also issues scopes that this crate does not model (for example `offline_access`).
/// That is **normal**, not an error condition, and the caller can simply drop such an entry.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct UnknownScope;

impl core::fmt::Display for UnknownScope {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // 스코프 문자열 자체를 담지 않는다 — 토큰 유래 값을 오류에 싣지 않기 위해서다.
        f.write_str("unrecognized scope")
    }
}

impl core::error::Error for UnknownScope {}

impl core::str::FromStr for Scope {
    type Err = UnknownScope;

    /// Converts a string in Tesla's wire form (snake_case) into a [`Scope`].
    ///
    /// Case is not normalized — the wire form is fixed as lowercase, and arbitrary
    /// normalization risks folding scopes that are actually different into the same one.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "openid" => Ok(Scope::Openid),
            "user_data" => Ok(Scope::UserData),
            "vehicle_device_data" => Ok(Scope::VehicleDeviceData),
            "vehicle_location" => Ok(Scope::VehicleLocation),
            _ => Err(UnknownScope),
        }
    }
}

/// A boolean expression over required scopes.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ScopeExpr {
    /// All sub-expressions must hold.
    All(Vec<ScopeExpr>),
    /// At least one sub-expression must hold.
    Any(Vec<ScopeExpr>),
    /// A single required scope.
    One(Scope),
}

impl ScopeExpr {
    /// Whether `held` scopes satisfy this expression.
    pub fn is_satisfied_by(&self, held: &[Scope]) -> bool {
        match self {
            ScopeExpr::One(s) => held.contains(s),
            ScopeExpr::All(v) => v.iter().all(|e| e.is_satisfied_by(held)),
            ScopeExpr::Any(v) => v.iter().any(|e| e.is_satisfied_by(held)),
        }
    }
    /// Returns the **entire required expression** (a clone of `self`) when
    /// `held` does not satisfy it, or `None` when it does — for advisory
    /// pre-checks.
    ///
    /// ⚠️ Whatever `held` a caller passes here is only as trustworthy as its
    /// source. `crate::client::Client::send`'s own pre-check (A3 Task 9,
    /// design §10) never actually holds a `held: &[Scope]` to call this
    /// with — it goes through `crate::auth`'s unverified, signature-free
    /// token observation instead, which reports a pass/fail/indeterminate
    /// verdict rather than a scope list (see [`crate::error::Error::MissingScope`]'s
    /// doc). Either way, a scope observed on an **unverified** access token
    /// proves nothing about actual authorization — it is an advisory cost
    /// guard against paying for a call the server will reject, never a
    /// substitute for the server's own 403.
    ///
    /// Note what this is not (final fix wave / F13b): it does **not** narrow
    /// the result down to the unsatisfied part of the expression. Given
    /// `All([One(Openid), One(UserData)])` and `held = [Openid]`, the return
    /// value is the whole `All([...])`, not the `One(UserData)` that is
    /// actually missing. The earlier doc said it returns "Some(self)", which
    /// is also literally wrong — `self` is `&ScopeExpr` and the value
    /// returned is an owned clone. The method name is a known mismatch with
    /// this behaviour and is being left alone deliberately; renaming it is a
    /// separate, deferred API decision.
    pub fn missing(&self, held: &[Scope]) -> Option<ScopeExpr> {
        if self.is_satisfied_by(held) {
            None
        } else {
            Some(self.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_requires_every_scope() {
        let e = ScopeExpr::All(vec![
            ScopeExpr::One(Scope::VehicleDeviceData),
            ScopeExpr::One(Scope::VehicleLocation),
        ]);
        assert!(!e.is_satisfied_by(&[Scope::VehicleDeviceData]));
        assert!(e.is_satisfied_by(&[Scope::VehicleDeviceData, Scope::VehicleLocation]));
    }
    #[test]
    fn any_requires_one() {
        let e = ScopeExpr::Any(vec![
            ScopeExpr::One(Scope::UserData),
            ScopeExpr::One(Scope::Openid),
        ]);
        assert!(e.is_satisfied_by(&[Scope::Openid]));
        assert!(!e.is_satisfied_by(&[Scope::VehicleLocation]));
    }
    #[test]
    fn one_matches_only_held_scope() {
        let e = ScopeExpr::One(Scope::UserData);
        assert!(e.is_satisfied_by(&[Scope::UserData]));
        assert!(!e.is_satisfied_by(&[Scope::Openid]));
        assert!(!e.is_satisfied_by(&[]));
    }
    #[test]
    fn missing_is_none_when_satisfied() {
        let e = ScopeExpr::One(Scope::Openid);
        assert_eq!(e.missing(&[Scope::Openid]), None);
    }
    #[test]
    fn missing_returns_self_when_unsatisfied() {
        let e = ScopeExpr::All(vec![
            ScopeExpr::One(Scope::Openid),
            ScopeExpr::One(Scope::UserData),
        ]);
        assert_eq!(e.missing(&[Scope::Openid]), Some(e.clone()));
    }
    #[test]
    fn nested_all_any_composes() {
        // All(Any(VehicleDeviceData, VehicleLocation), One(Openid))
        let e = ScopeExpr::All(vec![
            ScopeExpr::Any(vec![
                ScopeExpr::One(Scope::VehicleDeviceData),
                ScopeExpr::One(Scope::VehicleLocation),
            ]),
            ScopeExpr::One(Scope::Openid),
        ]);
        assert!(e.is_satisfied_by(&[Scope::VehicleLocation, Scope::Openid]));
        assert!(!e.is_satisfied_by(&[Scope::VehicleLocation]));
        assert!(!e.is_satisfied_by(&[Scope::Openid]));
    }
    #[test]
    fn all_of_empty_is_vacuously_true() {
        assert!(ScopeExpr::All(vec![]).is_satisfied_by(&[]));
    }
    #[test]
    fn any_of_empty_is_vacuously_false() {
        assert!(!ScopeExpr::Any(vec![]).is_satisfied_by(&[]));
    }

    /// Final fix wave (F4): `Scope`'s serde encoding must be Tesla's wire
    /// form, not serde's default variant name. Every variant is covered, and
    /// the expected string comes from an **exhaustive `match`** rather than a
    /// parallel array — so adding a variant without deciding its wire
    /// spelling is a compile error here, not a silent gap. (An enumerated
    /// list falling behind the shapes it must cover is exactly how two other
    /// defects on this branch happened.)
    #[test]
    fn serde_wire_form_is_snake_case_for_every_variant() {
        let all = [
            Scope::VehicleDeviceData,
            Scope::VehicleLocation,
            Scope::UserData,
            Scope::Openid,
        ];
        for scope in all {
            let wire = match scope {
                Scope::VehicleDeviceData => "vehicle_device_data",
                Scope::VehicleLocation => "vehicle_location",
                Scope::UserData => "user_data",
                Scope::Openid => "openid",
            };
            let json = format!("\"{wire}\"");
            assert_eq!(
                serde_json::to_string(&scope).unwrap(),
                json,
                "{scope:?} does not serialize to Tesla's wire form"
            );
            assert_eq!(
                serde_json::from_str::<Scope>(&json).unwrap(),
                scope,
                "{wire} does not deserialize back to {scope:?}"
            );
        }
    }

    /// The discriminating half: serde's **default** (variant-name) encoding
    /// must be rejected. Without this, a regression that dropped
    /// `rename_all` would still pass the round-trip test above, since a
    /// round-trip is self-consistent under any naming convention — that
    /// self-consistency is precisely why the bug was invisible.
    #[test]
    fn serde_rejects_the_default_variant_name_encoding() {
        assert!(serde_json::from_str::<Scope>("\"VehicleDeviceData\"").is_err());
        assert!(serde_json::from_str::<Scope>("\"UserData\"").is_err());
        assert!(serde_json::from_str::<Scope>("\"Openid\"").is_err());
    }

    #[test]
    fn from_str_accepts_every_modeled_scope_in_wire_form() {
        use core::str::FromStr;
        // 기대값을 Scope 자신이 아니라 Tesla 와이어 문자열에서 독립적으로 유도한다.
        let pairs = [
            ("openid", Scope::Openid),
            ("user_data", Scope::UserData),
            ("vehicle_device_data", Scope::VehicleDeviceData),
            ("vehicle_location", Scope::VehicleLocation),
        ];
        for (wire, expected) in pairs {
            assert_eq!(Scope::from_str(wire), Ok(expected), "wire form {wire}");
        }
    }

    #[test]
    fn from_str_rejects_unknown_scope_without_panicking() {
        use core::str::FromStr;
        // 실측 토큰의 scp 에 실제로 들어 있는 값 — 모델링돼 있지 않다.
        assert!(Scope::from_str("offline_access").is_err());
        assert!(Scope::from_str("").is_err());
        assert!(Scope::from_str("VehicleDeviceData").is_err()); // PascalCase 는 와이어 형식이 아니다
    }

    /// `from_str` must accept the **same** set of strings as the serde wire form.
    /// If the two paths diverge, Task 6's `scp` parsing and the token response's `scope`
    /// parsing reach different verdicts. The expected strings come from an **exhaustive
    /// `match`** rather than a parallel array, so adding a variant while leaving out its
    /// `from_str` arm becomes a compile error/failure instead of a silent hole (the same
    /// discipline as the F4 test).
    #[test]
    fn from_str_agrees_with_serde_wire_form_for_every_variant() {
        use core::str::FromStr;
        let all = [
            Scope::VehicleDeviceData,
            Scope::VehicleLocation,
            Scope::UserData,
            Scope::Openid,
        ];
        for scope in all {
            let wire = match scope {
                Scope::VehicleDeviceData => "vehicle_device_data",
                Scope::VehicleLocation => "vehicle_location",
                Scope::UserData => "user_data",
                Scope::Openid => "openid",
            };
            // serde 가 실제로 그 문자열을 내는지 먼저 고정하고,
            let serialized = serde_json::to_string(&scope).unwrap();
            assert_eq!(serialized, format!("\"{wire}\""));
            // from_str 이 같은 문자열을 같은 variant 로 받는지 확인한다.
            assert_eq!(
                Scope::from_str(wire),
                Ok(scope),
                "from_str disagrees with the serde wire form of {scope:?}"
            );
        }
    }

    /// Pins down that no normalization, trimming or partial matching is done.
    /// If any one of these passed, different scopes would be folded into the same one
    /// (`to_lowercase`), or a whole space-separated string would be mistaken for a single
    /// scope.
    #[test]
    fn from_str_does_not_normalize_or_trim_or_partial_match() {
        use core::str::FromStr;
        for bad in [
            "OPENID",                     // 대문자 정규화 금지
            "Openid",                     // serde 기본 variant 이름 인코딩
            "VEHICLE_DEVICE_DATA",        // 대문자 snake
            "vehicleDeviceData",          // camelCase
            " openid",                    // 선행 공백 — trim 금지
            "openid ",                    // 후행 공백 — trim 금지
            "openid\n",                   // 개행
            "open id",                    // 공백 삽입
            "openidx",                    // prefix 일치만으로 통과 금지
            "id",                         // suffix 일치만으로 통과 금지
            "openid vehicle_device_data", // 공백 구분 문자열 전체
            "offline_access",             // 실측 토큰의 미지 스코프
            "vehicle_cmds",               // 명령 스코프 — 이 크레이트는 모델링하지 않는다
            "",
        ] {
            assert!(
                Scope::from_str(bad).is_err(),
                "{bad:?} must not parse as a scope"
            );
        }
    }

    /// The error value does not carry the scope string (REQ-18: no exposure of
    /// token-derived values).
    #[test]
    fn unknown_scope_error_does_not_leak_the_offending_string() {
        use core::str::FromStr;
        let secret = "offline_access";
        let err = Scope::from_str(secret).unwrap_err();
        assert!(!format!("{err}").contains(secret));
        assert!(!format!("{err:?}").contains(secret));
    }

    /// The real evidence behind this task: the `scp` claim of a Tesla access token measured
    /// on this machine. `offline_access` is not modeled, so it must be **dropped**, and the
    /// other 3 must survive. Treating this as a failure of the whole would make Task 6's
    /// pre-check useless.
    #[test]
    fn real_token_scp_claim_drops_unknown_and_keeps_the_rest() {
        use core::str::FromStr;
        let scp = [
            "offline_access",
            "openid",
            "vehicle_device_data",
            "vehicle_location",
        ];
        let held: Vec<Scope> = scp.iter().filter_map(|s| Scope::from_str(s).ok()).collect();
        assert_eq!(
            held,
            vec![
                Scope::Openid,
                Scope::VehicleDeviceData,
                Scope::VehicleLocation
            ],
        );
        // 미지 스코프를 버려도 알려진 요구 스코프 판정은 그대로다.
        let required = ScopeExpr::All(vec![
            ScopeExpr::One(Scope::VehicleDeviceData),
            ScopeExpr::One(Scope::VehicleLocation),
        ]);
        assert!(required.is_satisfied_by(&held));
        assert_eq!(required.missing(&held), None);
        // 보유하지 않은 스코프는 여전히 미충족으로 남는다.
        assert!(!ScopeExpr::One(Scope::UserData).is_satisfied_by(&held));
    }

    /// A realistic parse of Tesla's space-delimited `scope` string from a
    /// token response, which is where these values actually arrive.
    #[test]
    fn tesla_token_scope_string_parses_into_scopes() {
        let raw = "openid vehicle_device_data vehicle_location user_data";
        let parsed: Vec<Scope> = raw
            .split(' ')
            .map(|s| serde_json::from_str::<Scope>(&format!("\"{s}\"")).unwrap())
            .collect();
        assert_eq!(
            parsed,
            vec![
                Scope::Openid,
                Scope::VehicleDeviceData,
                Scope::VehicleLocation,
                Scope::UserData,
            ]
        );
    }
}

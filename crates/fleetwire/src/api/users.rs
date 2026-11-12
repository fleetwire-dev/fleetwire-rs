//! User-scoped endpoints. Both modeled here are unpaid (O1 matrix: of the 16, only
//! `vehicle_data` and `nearby_charging_sites` are paid).

use crate::endpoint::FleetGet;
use crate::pricing::Pricing;
use crate::query::QuerySpec;
use crate::scope::{Scope, ScopeExpr};

/// `GET /api/1/users/region` — this account's Fleet API region and base URL.
///
/// The response's `fleet_api_base_url` is a **server-supplied string**,
/// and Bearer tokens are sent to whatever URL a caller adopts it as. Using it
/// without validation is a credential-exfiltration path — [`crate::Client`]
/// adopts it only after a fixed host allowlist
/// ([`crate::TESLA_REGION_HOSTS`]) plus scheme/port/path checks pass, and
/// keeps its bootstrap host on any refusal.
pub struct GetRegion;

impl crate::sealed::Sealed for GetRegion {}

impl FleetGet for GetRegion {
    passthrough_endpoint!(GetRegion, RegionInfo);

    fn required_scopes(&self) -> ScopeExpr {
        // O1 실측: region 조회는 스코프에 유연하다 — 실측 토큰의 scp
        // (openid·offline_access·vehicle_device_data·vehicle_location) 중
        // 모델링된 어느 하나만 있어도 통과시킨다.
        ScopeExpr::Any(vec![
            ScopeExpr::One(Scope::Openid),
            ScopeExpr::One(Scope::UserData),
            ScopeExpr::One(Scope::VehicleDeviceData),
        ])
    }
    fn pricing(&self) -> Pricing {
        Pricing::NotCharged
    }
    fn path(&self) -> String {
        "/api/1/users/region".to_owned()
    }
    fn query(&self) -> QuerySpec {
        QuerySpec::new()
    }
}

/// The payload inside `/api/1/users/region`'s envelope (`{"response": ...}`).
///
/// `#[non_exhaustive]` and every field `Option` + `#[serde(default)]`, same
/// tolerance policy as [`crate::model`]'s types: a field Tesla adds or omits
/// must never turn into a deserialization error for this crate's callers.
#[derive(Debug, serde::Deserialize)]
#[non_exhaustive]
pub struct RegionInfo {
    /// The account's region, e.g. `"na"` or `"eu"`.
    #[serde(default)]
    pub region: Option<String>,
    /// The Fleet API base URL this account should use.
    ///
    /// Server-supplied — see this module's [`GetRegion`] doc before using
    /// it as an HTTP destination for an authenticated request.
    #[serde(default)]
    pub fleet_api_base_url: Option<String>,
}

/// `GET /api/1/users/me` — the current user's profile.
///
/// **Requires a user-context token** — a 3rd-party business token is not
/// compatible (O1 common note).
/// The response is `serde_json::Value` — the live shape has not been
/// observed, and guessing field names into a strongly-typed public API would
/// be dishonest.
pub struct GetMe;

impl crate::sealed::Sealed for GetMe {}

impl FleetGet for GetMe {
    passthrough_endpoint!(GetMe, serde_json::Value);

    /// `user_data`, not `openid` (O1 correction no. 4) — unlike [`GetRegion`],
    /// which accepts several scopes, this endpoint requires exactly this
    /// one.
    fn required_scopes(&self) -> ScopeExpr {
        ScopeExpr::One(Scope::UserData)
    }
    fn pricing(&self) -> Pricing {
        Pricing::NotCharged
    }
    fn path(&self) -> String {
        "/api/1/users/me".to_owned()
    }
    fn query(&self) -> QuerySpec {
        QuerySpec::new()
    }
}

/// `GET /api/1/users/orders` — this account's order history.
///
/// **Requires a user-context token** — a 3rd-party business token is not
/// compatible (O1 common note).
/// The response is `serde_json::Value` — the live shape has not been
/// observed.
/// Named like a collection but **not paginated** — O1's evidence matrix
/// found no `pagination` sibling on this endpoint, so it is checked, not
/// assumed. If a live response is ever found to carry one, adding
/// [`crate::model::Page`] here is a breaking change and must be verified
/// before publishing.
pub struct ListOrders;

impl crate::sealed::Sealed for ListOrders {}

impl FleetGet for ListOrders {
    passthrough_endpoint!(ListOrders, serde_json::Value);

    /// **`vehicle_device_data`, not `user_data`** — and the two halves of
    /// that sentence have very different evidence behind them. A5 Task 1's
    /// scope contrast experiment (O20).
    ///
    /// **Proven (measured 2026-08-04, HTTP 200).** `GET /api/1/users/orders`
    /// succeeded on a token whose `scp` claim did **not** contain
    /// `user_data`. A success is a positive result, so this settles the
    /// question it answers: `user_data` is **not required** here. The earlier
    /// `ScopeExpr::One(Scope::UserData)` declaration was therefore not merely
    /// imprecise — it made the free pre-check refuse an endpoint the server
    /// answers, turning an operable call into a
    /// [`crate::Error::MissingScope`] denial for the very token this crate is
    /// operated with. That is exactly the failure mode `Client::send_inner`'s
    /// own comment warns about: an advisory optimisation degenerating into a
    /// false authorization denial.
    ///
    /// **Not proven: that `vehicle_device_data` *is* required.** The
    /// successful token happened to carry it, and Tesla's docs bundle
    /// declares it, but no contrast run was made against a token that lacks
    /// it — so this is the best-supported declaration available, not a
    /// measurement. Resolving it needs a call with `vehicle_device_data`
    /// withheld; until then the honest reading of this line is "the scope the
    /// evidence points at", not "the scope the server checks".
    ///
    /// See `docs/a5-test-audit.md` for the raw run (UTC, status, txid).
    fn required_scopes(&self) -> ScopeExpr {
        ScopeExpr::One(Scope::VehicleDeviceData)
    }
    fn pricing(&self) -> Pricing {
        Pricing::NotCharged
    }
    fn path(&self) -> String {
        "/api/1/users/orders".to_owned()
    }
    fn query(&self) -> QuerySpec {
        QuerySpec::new()
    }
}

/// `GET /api/1/users/feature_config` — this account's feature flags.
///
/// **Requires a user-context token** — a 3rd-party business token is not
/// compatible (O1 common note).
/// The response is `serde_json::Value` — the live shape has not been
/// observed.
pub struct GetFeatureConfig;

impl crate::sealed::Sealed for GetFeatureConfig {}

impl FleetGet for GetFeatureConfig {
    passthrough_endpoint!(GetFeatureConfig, serde_json::Value);

    /// `user_data`, not `openid` (O1 correction no. 4).
    fn required_scopes(&self) -> ScopeExpr {
        ScopeExpr::One(Scope::UserData)
    }
    fn pricing(&self) -> Pricing {
        Pricing::NotCharged
    }
    fn path(&self) -> String {
        "/api/1/users/feature_config".to_owned()
    }
    fn query(&self) -> QuerySpec {
        QuerySpec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope::Scope;

    #[test]
    fn get_region_contract() {
        let r = GetRegion;
        assert_eq!(r.path(), "/api/1/users/region");
        assert!(matches!(r.pricing(), Pricing::NotCharged));
        assert_eq!(r.query().to_encoded(), "");
    }

    /// `endpoint::tests::passthrough_assemble_ignores_pagination` only
    /// exercises the hand-written `DummyVehicles` double — it never calls
    /// `passthrough_endpoint!`'s own generated code. This pins the macro's
    /// actual expansion for a real endpoint: `wire` must survive unchanged
    /// even when pagination is supplied.
    #[test]
    fn get_region_assemble_passes_wire_through_and_ignores_pagination() {
        let wire = RegionInfo {
            region: Some("na".to_owned()),
            fleet_api_base_url: None,
        };
        let p = crate::model::Pagination {
            previous: None,
            next: Some(2),
            current: Some(1),
            per_page: Some(1),
            count: Some(9),
            pages: Some(9),
        };
        let out = <GetRegion as FleetGet>::assemble(wire, Some(p));
        assert_eq!(out.region.as_deref(), Some("na"));
    }

    /// O1: the region lookup is scope-flexible — holding any one of the modeled scopes must
    /// be enough to pass (if this flipped to `All`, which requires every one of them, many
    /// real tokens would be wrongly blocked with `MissingScope`).
    #[test]
    fn get_region_scopes_are_any_not_all() {
        let required = GetRegion.required_scopes();
        assert!(required.is_satisfied_by(&[Scope::Openid]));
        assert!(required.is_satisfied_by(&[Scope::UserData]));
        assert!(required.is_satisfied_by(&[Scope::VehicleDeviceData]));
        assert!(!required.is_satisfied_by(&[Scope::VehicleLocation]));
    }

    #[test]
    fn region_info_tolerates_missing_fields() {
        let v: RegionInfo = serde_json::from_str("{}").unwrap();
        assert!(v.region.is_none());
        assert!(v.fleet_api_base_url.is_none());
    }

    #[test]
    fn region_info_parses_both_fields() {
        let v: RegionInfo = serde_json::from_str(
            r#"{"region":"na","fleet_api_base_url":"https://fleet-api.prd.na.vn.cloud.tesla.com"}"#,
        )
        .unwrap();
        assert_eq!(v.region.as_deref(), Some("na"));
        assert_eq!(
            v.fleet_api_base_url.as_deref(),
            Some("https://fleet-api.prd.na.vn.cloud.tesla.com")
        );
    }

    /// The scopes of the three user endpoints are **not the same.** The earlier version of
    /// this test (named `user_endpoints_require_user_data_not_openid`) lumped the three
    /// together and pinned all of them to `user_data`, and so it worked to **protect** the
    /// wrong declaration on [`ListOrders`].
    ///
    /// The A5 Task 1 measurement (2026-08-04) broke that grouping — `/api/1/users/orders`
    /// returned **200** with a token that has no `user_data`. `GetMe` and
    /// `GetFeatureConfig` have no such counter-evidence, so they keep `user_data` as
    /// O1 correction no. 4 says (O20).
    ///
    /// [`GetRegion`] is not here — being `Any`, it is different in nature and
    /// `get_region_scopes_are_any_not_all` is dedicated to it.
    ///
    /// **It asserts in both directions.** For each endpoint it checks both "it is satisfied
    /// by the expected scope alone" and "it is not satisfied by the other scope alone".
    /// With only the first half, a loose declaration such as
    /// `Any(user_data, vehicle_device_data)` would pass, and so would a regression that puts
    /// [`ListOrders`] back to `UserData` — the second half is the reason this file exists
    /// (design §10.2, article 1).
    #[test]
    fn orders_requires_vehicle_device_data_while_me_and_feature_config_require_user_data() {
        // (엔드포인트, 만족시켜야 하는 스코프, 만족시키면 **안 되는** 스코프)
        let cases = [
            {
                let r = GetMe;
                (
                    r.path(),
                    r.required_scopes(),
                    r.pricing(),
                    Scope::UserData,
                    Scope::VehicleDeviceData,
                )
            },
            {
                let r = ListOrders;
                (
                    r.path(),
                    r.required_scopes(),
                    r.pricing(),
                    // 실측: user_data 없는 토큰으로 200 이었다.
                    Scope::VehicleDeviceData,
                    Scope::UserData,
                )
            },
            {
                let r = GetFeatureConfig;
                (
                    r.path(),
                    r.required_scopes(),
                    r.pricing(),
                    Scope::UserData,
                    Scope::VehicleDeviceData,
                )
            },
        ];

        for (path, scopes, pricing, wanted, rejected) in cases {
            assert!(matches!(pricing, Pricing::NotCharged), "{path}");
            assert!(
                scopes.is_satisfied_by(&[wanted]),
                "{path}: {wanted:?} 하나로 만족돼야 한다"
            );
            assert!(
                !scopes.is_satisfied_by(&[rejected]),
                "{path}: {rejected:?} 로 만족되면 안 된다 — \
                 세 엔드포인트를 한 덩어리로 묶던 회귀다"
            );
            // 셋 다 `openid` 만으로는 만족되지 않는다(O1 정정 4번) — `GetRegion`
            // 과 갈라지는 지점이라 스코프가 어느 쪽이든 유지돼야 한다.
            assert!(
                !scopes.is_satisfied_by(&[Scope::Openid]),
                "{path}: openid 만으로 만족되면 안 된다(O1 정정 4번)"
            );
        }
    }

    #[test]
    fn user_endpoint_paths_are_exact() {
        assert_eq!(GetMe.path(), "/api/1/users/me");
        assert_eq!(ListOrders.path(), "/api/1/users/orders");
        assert_eq!(GetFeatureConfig.path(), "/api/1/users/feature_config");
    }
}

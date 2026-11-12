//! Fleet API endpoint definitions (paths, methods, scope requirements).

use crate::{pricing::Pricing, query::QuerySpec, scope::ScopeExpr};
use serde::de::DeserializeOwned;

/// A strongly-typed read-only (GET) Fleet API endpoint.
///
/// This trait is sealed and cannot be implemented outside of `fleetwire`.
/// The `crate::sealed::Sealed` supertrait in its bounds is private, so
/// rustdoc renders an unimplementable bound with no explanation unless it is
/// spelled out here (final fix wave / F12). Sealing is deliberate: the set of
/// endpoints is fixed by what has been live-verified against Tesla's API, and
/// the transport layer relies on every implementor's `path`/`query`/`pricing`
/// being crate-authored — an external implementation could produce a request
/// this crate's read-only and billing guarantees do not cover. New endpoints
/// are added here, not by downstream crates.
pub trait FleetGet: crate::sealed::Sealed + Send + Sync {
    /// The type deserialized **directly** from the envelope's `response` key.
    ///
    /// May differ from [`Self::Response`] — list endpoints arrive on the wire
    /// as a `Vec<T>`, but callers get a [`crate::model::Page`] with pagination
    /// metadata attached.
    type WireResponse: DeserializeOwned;
    /// The type the caller receives.
    type Response;
    /// Assembles the wire value and its sibling metadata into the public type.
    ///
    /// **Associated function (no `self`).** Assembly must not depend on the
    /// request instance's values. A `&self` receiver would let an implementor
    /// mix request state into the response, which is not this layer's job.
    fn assemble(
        wire: Self::WireResponse,
        pagination: Option<crate::model::Pagination>,
    ) -> Self::Response;
    /// Scopes this call requires (may depend on request values).
    fn required_scopes(&self) -> ScopeExpr;
    /// Usage-based pricing category for this endpoint.
    fn pricing(&self) -> Pricing;
    /// Relative API path (VIN encoded as a path segment).
    fn path(&self) -> String;
    /// Structured query parameters.
    fn query(&self) -> QuerySpec;
}

/// A `FleetGet` scoped to a specific vehicle (`{vin}` path).
///
/// Sealed transitively via [`FleetGet`] — this trait cannot be implemented
/// outside of `fleetwire` either.
pub trait VehicleEndpoint: FleetGet {
    /// The target vehicle VIN.
    fn vin(&self) -> &str;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        pricing::Pricing,
        query::QuerySpec,
        scope::{Scope, ScopeExpr},
    };
    struct DummyVehicles;
    impl crate::sealed::Sealed for DummyVehicles {}
    impl FleetGet for DummyVehicles {
        type WireResponse = serde_json::Value;
        type Response = serde_json::Value;
        fn assemble(
            wire: Self::WireResponse,
            _pagination: Option<crate::model::Pagination>,
        ) -> Self::Response {
            wire
        }
        fn required_scopes(&self) -> ScopeExpr {
            ScopeExpr::One(Scope::VehicleDeviceData)
        }
        fn pricing(&self) -> Pricing {
            Pricing::NotCharged
        }
        fn path(&self) -> String {
            "/api/1/vehicles".into()
        }
        fn query(&self) -> QuerySpec {
            QuerySpec::new()
        }
    }
    #[test]
    fn fleetget_contract() {
        let d = DummyVehicles;
        assert_eq!(d.path(), "/api/1/vehicles");
        assert_eq!(d.pricing(), Pricing::NotCharged);
        assert!(
            d.required_scopes()
                .is_satisfied_by(&[Scope::VehicleDeviceData])
        );
    }

    /// A passthrough endpoint **discards** pagination — it returns the wire value as is.
    #[test]
    fn passthrough_assemble_ignores_pagination() {
        let p = crate::model::Pagination {
            previous: None,
            next: Some(2),
            current: Some(1),
            per_page: Some(1),
            count: Some(9),
            pages: Some(9),
        };
        let out = <DummyVehicles as FleetGet>::assemble(serde_json::json!({"a":1}), Some(p));
        assert_eq!(out, serde_json::json!({"a":1}));
    }

    /// assemble is an associated function — it does not take `&self`.
    /// This is because values of the request instance must not get mixed into the response
    /// (design §2.2).
    #[test]
    fn assemble_needs_no_request_instance() {
        // 인스턴스 없이 호출된다는 사실 자체가 계약이다.
        let out = <DummyVehicles as FleetGet>::assemble(serde_json::Value::Null, None);
        assert!(out.is_null());
    }
}

//! Vehicle list/identity/data endpoints (O1 matrix: of the 16, only `vehicle_data` and
//! `nearby_charging_sites` are paid — both are settled in this file).

use crate::endpoint::{FleetGet, VehicleEndpoint};
use crate::model::{
    Invitation, NearbyChargingSites, Page, Pagination, VehicleData, VehicleSummary,
};
use crate::pricing::Pricing;
use crate::query::{QuerySpec, VinError, encode_vin_segment};
use crate::scope::{Scope, ScopeExpr};

/// `GET /api/1/vehicles` — this account's vehicle list.
///
/// The wire response is a bare JSON array (plus, per Tesla's convention for
/// list endpoints, pagination metadata as a sibling of the envelope's
/// `response` key rather than nested inside it) — see [`crate::model::Page`]'s
/// doc for why this crate assembles a `Page` from the two pieces rather than
/// modeling a single combined response type here. `assemble` below is what
/// does that assembly (A4 Task 2) — `WireResponse` is only the bare array.
pub struct ListVehicles {
    /// Requested page number (1-based). Omitted from the query when `None`.
    pub page: Option<u64>,
    /// Requested page size. Omitted from the query when `None`.
    pub per_page: Option<u64>,
}

impl crate::sealed::Sealed for ListVehicles {}

impl FleetGet for ListVehicles {
    type WireResponse = Vec<VehicleSummary>;
    type Response = Page<VehicleSummary>;

    fn assemble(wire: Self::WireResponse, pagination: Option<Pagination>) -> Self::Response {
        Page {
            items: wire,
            pagination,
        }
    }

    fn required_scopes(&self) -> ScopeExpr {
        ScopeExpr::One(Scope::VehicleDeviceData)
    }
    fn pricing(&self) -> Pricing {
        Pricing::NotCharged
    }
    fn path(&self) -> String {
        "/api/1/vehicles".to_owned()
    }
    fn query(&self) -> QuerySpec {
        let mut q = QuerySpec::new();
        if let Some(page) = self.page {
            q.push("page", page);
        }
        if let Some(per_page) = self.per_page {
            q.push("per_page", per_page);
        }
        q
    }
}

/// `GET /api/1/vehicles/{vin}` — a single vehicle's identity/state summary.
///
/// Fix round 1 (Important 2): `vin` is deliberately **not** a public field
/// anymore. `FleetGet::path` returns a bare `String`, not a `Result` — A1's
/// trait gives `path()` no channel back to a caller for "this VIN is unsafe"
/// at call time. The previous shape (`pub vin: String` + a fixed
/// `"invalid-vin"` fallback inside `path()`) let a malformed VIN reach
/// [`Client::send`](crate::Client::send) anyway: a live-Bearer GET to
/// `/api/1/vehicles/invalid-vin`, a confusing remote 404, and a burned
/// rate-limit slot (60/min, shared account-wide) — laundering a caller's bug
/// into a server error. [`GetVehicle::new`] closes that: it is the only way
/// to construct a `GetVehicle`, it runs [`encode_vin_segment`]'s safety
/// check exactly once, and it stores the already-validated segment — so
/// `path()` is infallible **by construction**, not by falling back to a
/// placeholder.
///
/// Deliberately does not derive `Debug` — it holds a VIN, and REQ-18 treats
/// a VIN the same way [`crate::model::VehicleSummary`] does: not something a
/// derived `Debug` should print verbatim into a log line or panic message.
pub struct GetVehicle {
    /// The original VIN, unchanged — returned verbatim by
    /// [`VehicleEndpoint::vin`].
    vin: String,
    /// `vin`, already run through [`encode_vin_segment`] at construction
    /// time. `path()` reads this directly instead of calling
    /// `encode_vin_segment` (and having to decide what to do with an `Err`)
    /// on every call.
    encoded_segment: String,
}

impl GetVehicle {
    /// Validates `vin` and builds a `GetVehicle`.
    ///
    /// # Errors
    ///
    /// [`VinError::Unsafe`] if `vin` contains a path-traversal, separator,
    /// scheme, or control character, or is empty — see
    /// [`encode_vin_segment`]. No `GetVehicle` is constructed in that case,
    /// so there is no value here [`FleetGet::path`]/[`Client::send`](crate::Client::send)
    /// could ever be called on.
    pub fn new(vin: impl Into<String>) -> Result<Self, VinError> {
        let vin = vin.into();
        let encoded_segment = encode_vin_segment(&vin)?;
        Ok(Self {
            vin,
            encoded_segment,
        })
    }
}

impl crate::sealed::Sealed for GetVehicle {}

impl FleetGet for GetVehicle {
    passthrough_endpoint!(GetVehicle, VehicleSummary);

    fn required_scopes(&self) -> ScopeExpr {
        ScopeExpr::One(Scope::VehicleDeviceData)
    }
    fn pricing(&self) -> Pricing {
        Pricing::NotCharged
    }
    fn path(&self) -> String {
        format!("/api/1/vehicles/{}", self.encoded_segment)
    }
    fn query(&self) -> QuerySpec {
        QuerySpec::new()
    }
}

impl VehicleEndpoint for GetVehicle {
    fn vin(&self) -> &str {
        &self.vin
    }
}

/// The sub-data that can be selected with the `endpoints=` parameter of `vehicle_data`.
///
/// ⚠️ Why this is an enum: a free-form string turns a typo into a **paid request**.
/// It is `#[non_exhaustive]`, so Tesla adding a value is not a breaking change.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataEndpoint {
    /// `charge_state` — battery/charging status.
    ChargeState,
    /// `climate_state` — cabin climate status.
    ClimateState,
    /// `drive_state` — shift state, speed, heading.
    DriveState,
    /// `location_data` — precise GPS. Double opt-in: also requires the
    /// [`Scope::VehicleLocation`] scope (firmware 2023.38+).
    LocationData,
    /// `vehicle_config` — trim/options configuration.
    VehicleConfig,
    /// `vehicle_state` — odometer, locks, sentry, software version.
    VehicleState,
    /// `gui_settings` — dashboard display preferences.
    GuiSettings,
    /// `closures_state` — doors/windows/trunk/frunk open-closed state.
    ClosuresState,
}

impl DataEndpoint {
    /// Tesla's wire spelling for this endpoint (as it appears in the
    /// semicolon-joined `endpoints=` query value).
    fn as_wire(self) -> &'static str {
        match self {
            Self::ChargeState => "charge_state",
            Self::ClimateState => "climate_state",
            Self::DriveState => "drive_state",
            Self::LocationData => "location_data",
            Self::VehicleConfig => "vehicle_config",
            Self::VehicleState => "vehicle_state",
            Self::GuiSettings => "gui_settings",
            Self::ClosuresState => "closures_state",
        }
    }

    /// Whether requesting this sub-data requires the `vehicle_location`
    /// scope in addition to `vehicle_device_data`.
    fn needs_location(self) -> bool {
        matches!(self, Self::LocationData)
    }
}

/// `GET /api/1/vehicles/{vin}/vehicle_data` — ⚠️ **paid** ($0.002/request).
///
/// The only way to construct this is [`GetVehicleData::new`], which runs
/// [`encode_vin_segment`] once at construction (same pattern as
/// [`GetVehicle::new`]), so [`FleetGet::path`] is infallible by
/// construction.
pub struct GetVehicleData {
    /// The original VIN, unchanged — returned verbatim by
    /// [`VehicleEndpoint::vin`].
    vin: String,
    /// `vin`, already run through [`encode_vin_segment`] at construction
    /// time.
    encoded_segment: String,
    /// Requested sub-data. Determines both the `endpoints=` query value and
    /// (for [`DataEndpoint::LocationData`]) whether `vehicle_location` is
    /// added to [`FleetGet::required_scopes`].
    endpoints: Vec<DataEndpoint>,
}

impl GetVehicleData {
    /// Validates `vin` and builds a `GetVehicleData` requesting `endpoints`.
    ///
    /// # Errors
    ///
    /// [`VinError::Unsafe`] under the same conditions as
    /// [`GetVehicle::new`].
    pub fn new(vin: impl Into<String>, endpoints: Vec<DataEndpoint>) -> Result<Self, VinError> {
        let vin = vin.into();
        let encoded_segment = encode_vin_segment(&vin)?;
        Ok(Self {
            vin,
            encoded_segment,
            endpoints,
        })
    }
}

impl crate::sealed::Sealed for GetVehicleData {}

impl FleetGet for GetVehicleData {
    passthrough_endpoint!(GetVehicleData, VehicleData);

    /// ⚠️ Depends on the request's values — `vehicle_location` is only
    /// added when [`DataEndpoint::LocationData`] was requested. This is the
    /// reason A1 made `required_scopes` a **method** rather than a constant.
    fn required_scopes(&self) -> ScopeExpr {
        if self.endpoints.iter().any(|e| e.needs_location()) {
            ScopeExpr::All(vec![
                ScopeExpr::One(Scope::VehicleDeviceData),
                ScopeExpr::One(Scope::VehicleLocation),
            ])
        } else {
            ScopeExpr::One(Scope::VehicleDeviceData)
        }
    }
    fn pricing(&self) -> Pricing {
        Pricing::data()
    }
    fn path(&self) -> String {
        format!("/api/1/vehicles/{}/vehicle_data", self.encoded_segment)
    }
    fn query(&self) -> QuerySpec {
        let mut q = QuerySpec::new();
        if !self.endpoints.is_empty() {
            let joined = self
                .endpoints
                .iter()
                .map(|e| e.as_wire())
                .collect::<Vec<_>>()
                .join(";");
            q.push("endpoints", joined);
        }
        q
    }
}

impl VehicleEndpoint for GetVehicleData {
    fn vin(&self) -> &str {
        &self.vin
    }
}

/// `GET /api/1/vehicles/{vin}/nearby_charging_sites` — ⚠️ **paid**
/// ($0.002/request), the crate's second and last billed endpoint (O1).
///
/// Query params are `count`/`radius`/`detail` — **not** `page`/`per_page`.
/// This endpoint is not paginated, so [`passthrough_endpoint!`] is correct
/// here (no [`Page`] involved).
///
/// The only way to construct this is [`GetNearbyChargingSites::new`], which
/// runs [`encode_vin_segment`] once at construction (same pattern as
/// [`GetVehicle::new`]/[`GetVehicleData::new`]), so [`FleetGet::path`] is
/// infallible by construction.
pub struct GetNearbyChargingSites {
    /// The original VIN, unchanged — returned verbatim by
    /// [`VehicleEndpoint::vin`].
    vin: String,
    /// `vin`, already run through [`encode_vin_segment`] at construction
    /// time.
    encoded_segment: String,
    /// Requested site count. Omitted from the query when `None`.
    count: Option<u64>,
    /// Requested search radius. Omitted from the query when `None`.
    radius: Option<u64>,
    /// Whether to request detailed site info. Omitted from the query when
    /// `None`.
    detail: Option<bool>,
}

impl GetNearbyChargingSites {
    /// Validates `vin` and builds a `GetNearbyChargingSites` requesting
    /// `count`/`radius`/`detail`.
    ///
    /// # Errors
    ///
    /// [`VinError::Unsafe`] under the same conditions as
    /// [`GetVehicle::new`].
    pub fn new(
        vin: impl Into<String>,
        count: Option<u64>,
        radius: Option<u64>,
        detail: Option<bool>,
    ) -> Result<Self, VinError> {
        let vin = vin.into();
        let encoded_segment = encode_vin_segment(&vin)?;
        Ok(Self {
            vin,
            encoded_segment,
            count,
            radius,
            detail,
        })
    }
}

impl crate::sealed::Sealed for GetNearbyChargingSites {}

impl FleetGet for GetNearbyChargingSites {
    passthrough_endpoint!(GetNearbyChargingSites, NearbyChargingSites);

    fn required_scopes(&self) -> ScopeExpr {
        ScopeExpr::One(Scope::VehicleDeviceData)
    }
    fn pricing(&self) -> Pricing {
        Pricing::data()
    }
    fn path(&self) -> String {
        format!(
            "/api/1/vehicles/{}/nearby_charging_sites",
            self.encoded_segment
        )
    }
    fn query(&self) -> QuerySpec {
        let mut q = QuerySpec::new();
        if let Some(c) = self.count {
            q.push("count", c);
        }
        if let Some(r) = self.radius {
            q.push("radius", r);
        }
        if let Some(d) = self.detail {
            q.push("detail", d);
        }
        q
    }
}

impl VehicleEndpoint for GetNearbyChargingSites {
    fn vin(&self) -> &str {
        &self.vin
    }
}

/// `GET /api/1/vehicles/{vin}/invitations` — unpaid, and the crate's
/// **second** consumer of pagination (A4 Task 6) after [`ListVehicles`].
/// Unlike [`GetVehicle`]/[`GetVehicleData`]/[`GetNearbyChargingSites`], this
/// does not use `passthrough_endpoint!` — its `WireResponse` (`Vec<Invitation>`)
/// differs from its `Response` (`Page<Invitation>`), so `assemble` is
/// hand-written the same way [`ListVehicles`]'s is, to prove the mechanism
/// generalizes past the one endpoint it was originally built against.
///
/// ⚠️ `per_page`'s server-side ceiling is documented as 25 (O1), but **this
/// client does not clamp it**. The server is the authority on the limit; if
/// Tesla changes it, a client-side clamp would become wrong before the
/// server does. An out-of-range value is passed through unchanged and left
/// for the server to reject or ignore.
///
/// The only way to construct this is [`ListInvitations::new`], which runs
/// [`encode_vin_segment`] once at construction (same pattern as
/// [`GetVehicle::new`]), so [`FleetGet::path`] is infallible by
/// construction.
pub struct ListInvitations {
    /// The original VIN, unchanged — returned verbatim by
    /// [`VehicleEndpoint::vin`].
    vin: String,
    /// `vin`, already run through [`encode_vin_segment`] at construction
    /// time.
    encoded_segment: String,
    /// Requested page number (1-based). Omitted from the query when `None`.
    pub page: Option<u64>,
    /// Requested page size. Omitted from the query when `None`. ⚠️ Not
    /// clamped to the documented 25 ceiling — see the type's doc.
    pub per_page: Option<u64>,
}

impl ListInvitations {
    /// Validates `vin` and builds a `ListInvitations` requesting `page`/`per_page`.
    ///
    /// # Errors
    ///
    /// [`VinError::Unsafe`] under the same conditions as
    /// [`GetVehicle::new`].
    pub fn new(
        vin: impl Into<String>,
        page: Option<u64>,
        per_page: Option<u64>,
    ) -> Result<Self, VinError> {
        let vin = vin.into();
        let encoded_segment = encode_vin_segment(&vin)?;
        Ok(Self {
            vin,
            encoded_segment,
            page,
            per_page,
        })
    }
}

impl crate::sealed::Sealed for ListInvitations {}

impl FleetGet for ListInvitations {
    type WireResponse = Vec<Invitation>;
    type Response = Page<Invitation>;

    fn assemble(wire: Self::WireResponse, pagination: Option<Pagination>) -> Self::Response {
        Page {
            items: wire,
            pagination,
        }
    }

    fn required_scopes(&self) -> ScopeExpr {
        ScopeExpr::One(Scope::VehicleDeviceData)
    }
    fn pricing(&self) -> Pricing {
        Pricing::NotCharged
    }
    fn path(&self) -> String {
        format!("/api/1/vehicles/{}/invitations", self.encoded_segment)
    }
    fn query(&self) -> QuerySpec {
        let mut q = QuerySpec::new();
        if let Some(page) = self.page {
            q.push("page", page);
        }
        if let Some(per_page) = self.per_page {
            q.push("per_page", per_page);
        }
        q
    }
}

impl VehicleEndpoint for ListInvitations {
    fn vin(&self) -> &str {
        &self.vin
    }
}

/// Defines an unbilled, parameterless vehicle endpoint that takes only a VIN (A4 Task 7).
///
/// ⚠️ `GetFleetTelemetryErrors` (below) was **deliberately written by hand without this
/// macro** — when a mutation test changes the macro body, all 6 types made with this macro
/// change at the same time, so it is impossible to tell whether the table test really looks
/// at all 7 separately or passes by accident on "if one of them is right the rest must be
/// too". Only by injecting a mutation into the one hand-written type can it be confirmed, for
/// that one type, that "each check hung on the table actually reaches that value" — for the
/// other 6 it is still a matter of trusting that this macro's definition expanded correctly
/// for every type, not something proven by mutation (see task-7-report.md).
macro_rules! simple_vehicle_endpoint {
    ($name:ident, $segment:literal, $doc:literal) => {
        #[doc = $doc]
        ///
        /// ⚠️ The response is a `serde_json::Value` — no real response has been seen,
        /// and guessed field names are not made into a strongly typed public API. A
        /// type will be given once the structure is settled.
        pub struct $name {
            /// The original VIN, unchanged — returned verbatim by
            /// [`VehicleEndpoint::vin`].
            vin: String,
            /// `vin`, already run through [`encode_vin_segment`] at
            /// construction time.
            encoded_segment: String,
        }

        impl $name {
            /// Validates `vin` and builds a
            #[doc = concat!("[`", stringify!($name), "`].")]
            ///
            /// # Errors
            ///
            /// [`VinError::Unsafe`] under the same conditions as
            /// [`GetVehicle::new`].
            pub fn new(vin: impl Into<String>) -> Result<Self, VinError> {
                let vin = vin.into();
                let encoded_segment = encode_vin_segment(&vin)?;
                Ok(Self {
                    vin,
                    encoded_segment,
                })
            }
        }

        impl crate::sealed::Sealed for $name {}

        impl FleetGet for $name {
            passthrough_endpoint!($name, serde_json::Value);
            /// ☠️ **Measured wrong for at least one member of this macro.**
            /// On 2026-09-18 a token holding `vehicle_device_data` (and
            /// `user_data`, `vehicle_location`, but **not** `vehicle_cmds`)
            /// called every endpoint below. Eleven succeeded;
            /// [`GetMobileEnabled`] came back `HTTP 403 Forbidden:
            /// Unauthorized missing scopes`.
            ///
            /// The declaration is left alone on purpose. The 403 proves
            /// `vehicle_device_data` **alone is not sufficient** there; it does
            /// not say what *is* required (Tesla does not name the missing
            /// scope), and guessing `vehicle_cmds` into a read-only client's
            /// public API would be inventing a measurement. See
            /// [`crate::Error::MissingScope`]: this pre-check is a cost guard,
            /// and the server's 403 is the authority — so a declaration that is
            /// too permissive costs one refused free call, not a wrong verdict.
            fn required_scopes(&self) -> ScopeExpr {
                ScopeExpr::One(Scope::VehicleDeviceData)
            }
            fn pricing(&self) -> Pricing {
                Pricing::NotCharged
            }
            fn path(&self) -> String {
                format!(
                    concat!("/api/1/vehicles/{}/", $segment),
                    self.encoded_segment
                )
            }
            fn query(&self) -> QuerySpec {
                QuerySpec::new()
            }
        }

        impl VehicleEndpoint for $name {
            fn vin(&self) -> &str {
                &self.vin
            }
        }
    };
}

simple_vehicle_endpoint!(
    GetMobileEnabled,
    "mobile_enabled",
    "`GET /api/1/vehicles/{vin}/mobile_enabled` — whether the Tesla mobile app has remote access enabled for this vehicle. ☠️ **Measured 2026-09-18: refused with `HTTP 403 Forbidden: Unauthorized missing scopes` by a token that held the `vehicle_device_data` this type declares.** The other eleven endpoints in this macro succeeded with that same token, so the gap is this endpoint's, not the macro's. The vehicle command scope is the likely requirement — a read-only client will not be granting it, so `fleetwire-capture` records this one as a skipped endpoint rather than a failure."
);
simple_vehicle_endpoint!(
    GetRecentAlerts,
    "recent_alerts",
    "`GET /api/1/vehicles/{vin}/recent_alerts` — recent vehicle alerts."
);
simple_vehicle_endpoint!(
    GetReleaseNotes,
    "release_notes",
    "`GET /api/1/vehicles/{vin}/release_notes` — firmware release notes."
);
simple_vehicle_endpoint!(
    GetServiceData,
    "service_data",
    "`GET /api/1/vehicles/{vin}/service_data` — service appointment/status data."
);
simple_vehicle_endpoint!(
    ListDrivers,
    "drivers",
    "`GET /api/1/vehicles/{vin}/drivers` — drivers with access to this vehicle."
);
simple_vehicle_endpoint!(
    GetFleetTelemetryConfig,
    "fleet_telemetry_config",
    "`GET /api/1/vehicles/{vin}/fleet_telemetry_config` — this vehicle's fleet telemetry streaming configuration."
);

/// `GET /api/1/vehicles/{vin}/fleet_telemetry_errors` — recent fleet
/// telemetry streaming errors for this vehicle.
///
/// ⚠️ The response is a `serde_json::Value` — no real response has been seen, and guessed
/// field names are not made into a strongly typed public API. A type will be given once the
/// structure is settled.
///
/// ⚠️ This type was **written by hand** without the [`simple_vehicle_endpoint!`] macro — its
/// field/method shape is identical to the other 6 (mobile_enabled, recent_alerts,
/// release_notes, service_data, drivers, fleet_telemetry_config), but this is so that a
/// mutation test can be applied to this one type alone. Injecting a mutation into the macro
/// body changes the 6 all at once, so it cannot be known "whether the table test verifies
/// each of the 7" (see the macro doc above).
pub struct GetFleetTelemetryErrors {
    /// The original VIN, unchanged — returned verbatim by
    /// [`VehicleEndpoint::vin`].
    vin: String,
    /// `vin`, already run through [`encode_vin_segment`] at construction
    /// time.
    encoded_segment: String,
}

impl GetFleetTelemetryErrors {
    /// Validates `vin` and builds a `GetFleetTelemetryErrors`.
    ///
    /// # Errors
    ///
    /// [`VinError::Unsafe`] under the same conditions as [`GetVehicle::new`].
    pub fn new(vin: impl Into<String>) -> Result<Self, VinError> {
        let vin = vin.into();
        let encoded_segment = encode_vin_segment(&vin)?;
        Ok(Self {
            vin,
            encoded_segment,
        })
    }
}

impl crate::sealed::Sealed for GetFleetTelemetryErrors {}

impl FleetGet for GetFleetTelemetryErrors {
    passthrough_endpoint!(GetFleetTelemetryErrors, serde_json::Value);

    fn required_scopes(&self) -> ScopeExpr {
        ScopeExpr::One(Scope::VehicleDeviceData)
    }
    fn pricing(&self) -> Pricing {
        Pricing::NotCharged
    }
    fn path(&self) -> String {
        format!(
            "/api/1/vehicles/{}/fleet_telemetry_errors",
            self.encoded_segment
        )
    }
    fn query(&self) -> QuerySpec {
        QuerySpec::new()
    }
}

impl VehicleEndpoint for GetFleetTelemetryErrors {
    fn vin(&self) -> &str {
        &self.vin
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoint::{FleetGet, VehicleEndpoint};

    #[test]
    fn list_vehicles_contract() {
        let r = ListVehicles {
            page: Some(2),
            per_page: Some(50),
        };
        assert_eq!(r.path(), "/api/1/vehicles");
        // ⚠️ O1: 16개 중 유료는 vehicle_data·nearby_charging_sites 뿐이다.
        assert!(matches!(r.pricing(), Pricing::NotCharged));
        let q = r.query().to_encoded();
        assert!(
            q.contains("page=2") && q.contains("per_page=50"),
            "실제: {q}"
        );
    }

    #[test]
    fn list_vehicles_omits_absent_pagination() {
        let r = ListVehicles {
            page: None,
            per_page: None,
        };
        assert_eq!(r.query().to_encoded(), "", "없는 파라미터를 보내면 안 된다");
    }

    /// When only one side is present, the absent side must still be left out — looking only
    /// at the both-present and both-absent cases cannot catch the mutation "if either one is
    /// present, attach both".
    #[test]
    fn list_vehicles_includes_only_the_present_pagination_param() {
        let page_only = ListVehicles {
            page: Some(3),
            per_page: None,
        };
        assert_eq!(page_only.query().to_encoded(), "page=3");

        let per_page_only = ListVehicles {
            page: None,
            per_page: Some(10),
        };
        assert_eq!(per_page_only.query().to_encoded(), "per_page=10");
    }

    /// The VIN is encoded as a path segment — this prevents path injection.
    #[test]
    fn get_vehicle_encodes_vin_segment() {
        let r = GetVehicle::new("5YJ3E1EA7KF000000").unwrap();
        assert!(!r.path().contains("/../"), "실제: {}", r.path());
        assert_eq!(r.vin(), "5YJ3E1EA7KF000000");
    }

    /// The test above alone cannot catch the mutation where the implementation "always
    /// returns a fixed string" — this pins, as a positive case, that a normal VIN is actually
    /// carried in the path.
    #[test]
    fn get_vehicle_embeds_a_valid_vin_verbatim() {
        let r = GetVehicle::new("5YJ3E1EA7KF000000").unwrap();
        assert_eq!(r.path(), "/api/1/vehicles/5YJ3E1EA7KF000000");
    }

    /// ⚠️ `vehicle_location` is required only when location is requested.
    #[test]
    fn location_data_requires_the_location_scope() {
        let with = GetVehicleData::new(
            "5YJ3E1EA1JF000001",
            vec![DataEndpoint::ChargeState, DataEndpoint::LocationData],
        )
        .unwrap();
        assert!(
            with.required_scopes()
                .is_satisfied_by(&[Scope::VehicleDeviceData, Scope::VehicleLocation])
        );
        assert!(
            !with
                .required_scopes()
                .is_satisfied_by(&[Scope::VehicleDeviceData]),
            "위치를 요청했는데 device_data 만으로 만족되면 안 된다"
        );
    }

    #[test]
    fn without_location_data_only_device_data_is_required() {
        let without =
            GetVehicleData::new("5YJ3E1EA1JF000001", vec![DataEndpoint::ChargeState]).unwrap();
        assert!(
            without
                .required_scopes()
                .is_satisfied_by(&[Scope::VehicleDeviceData])
        );
    }

    /// ⚠️ If location_data is not specified, it must be absent from the query (F9 double
    /// opt-in).
    #[test]
    fn location_data_is_absent_from_the_query_unless_asked() {
        let r = GetVehicleData::new("5YJ3E1EA1JF000001", vec![DataEndpoint::ChargeState]).unwrap();
        let q = r.query().to_encoded();
        assert!(!q.contains("location_data"), "실제: {q}");
        assert!(q.contains("charge_state"), "실제: {q}");
    }

    /// It is paid. Misclassifying it throws the billing report off.
    #[test]
    fn vehicle_data_is_a_paid_endpoint() {
        let r = GetVehicleData::new("5YJ3E1EA1JF000001", vec![DataEndpoint::ChargeState]).unwrap();
        assert!(matches!(r.pricing(), Pricing::Data { .. }));
    }

    /// When endpoints is empty, the endpoints parameter is not put into the query at all.
    /// (This is left to Tesla's default behaviour — sending an empty string has an unclear
    /// meaning)
    #[test]
    fn empty_endpoints_omits_the_parameter() {
        let r = GetVehicleData::new("5YJ3E1EA1JF000001", vec![]).unwrap();
        assert_eq!(r.query().to_encoded(), "");
    }

    /// Fix round 1 (Important 2): a corrupted VIN is now blocked **at construction time** —
    /// there is no longer a fixed sentinel reached through `path()`. The `GetVehicle` value
    /// itself is never created, so there is nothing to call any of
    /// `path()`/`vin()`/`Client::send` on.
    #[test]
    fn get_vehicle_new_rejects_an_unsafe_vin() {
        // `.err().unwrap()` rather than `.unwrap_err()`: the latter requires
        // `GetVehicle: Debug` for its panic message, and `GetVehicle` holds a
        // VIN — deliberately not `Debug` for the same REQ-18 reason as
        // `crate::model::VehicleSummary`.
        let err = GetVehicle::new("5YJ/../admin").err().unwrap();
        assert_eq!(err, VinError::Unsafe);
    }

    /// The test above alone cannot catch the "always fails" mutation (the case where the
    /// constructor is flipped to reject valid VINs too) — a normal VIN must still succeed.
    #[test]
    fn get_vehicle_new_accepts_a_valid_vin() {
        assert!(GetVehicle::new("5YJ3E1EA7KF000000").is_ok());
    }

    #[test]
    fn get_vehicle_contract_scope_and_pricing() {
        let r = GetVehicle::new("5YJ3E1EA7KF000000").unwrap();
        assert!(matches!(r.pricing(), Pricing::NotCharged));
        assert!(
            r.required_scopes()
                .is_satisfied_by(&[Scope::VehicleDeviceData])
        );
        assert_eq!(r.query().to_encoded(), "");
    }

    /// It is paid (the second and last one). It uses only `count`/`radius`/`detail` —
    /// not `page`/`per_page` (O1).
    #[test]
    fn nearby_sites_is_paid_and_uses_count_radius_detail() {
        let r = GetNearbyChargingSites::new("5YJ3E1EA1JF000001", Some(5), Some(20), Some(true))
            .unwrap();
        assert!(matches!(r.pricing(), Pricing::Data { .. }));
        let q = r.query().to_encoded();
        assert!(
            q.contains("count=5") && q.contains("radius=20") && q.contains("detail=true"),
            "실제: {q}"
        );
        assert!(
            !q.contains("page"),
            "이 엔드포인트는 page 를 쓰지 않는다: {q}"
        );
    }

    #[test]
    fn nearby_sites_omits_absent_params() {
        let r = GetNearbyChargingSites::new("5YJ3E1EA1JF000001", None, None, None).unwrap();
        assert_eq!(r.query().to_encoded(), "");
    }

    #[test]
    fn nearby_sites_encodes_the_vin_segment() {
        assert!(GetNearbyChargingSites::new("5YJ/../admin", None, None, None).is_err());
    }

    /// `endpoint::tests::passthrough_assemble_ignores_pagination` only
    /// exercises the hand-written `DummyVehicles` double — it never calls
    /// `passthrough_endpoint!`'s own generated code. This pins the macro's
    /// actual expansion for a real endpoint: `wire` must survive unchanged
    /// even when pagination is supplied.
    #[test]
    fn get_vehicle_assemble_passes_wire_through_and_ignores_pagination() {
        let wire = VehicleSummary {
            id: None,
            vin: Some("5YJ3E1EA7KF000000".to_owned()),
            state: None,
            extra: Default::default(),
        };
        let p = crate::model::Pagination {
            previous: None,
            next: Some(2),
            current: Some(1),
            per_page: Some(1),
            count: Some(9),
            pages: Some(9),
        };
        let out = <GetVehicle as FleetGet>::assemble(wire, Some(p));
        assert_eq!(out.vin.as_deref(), Some("5YJ3E1EA7KF000000"));
    }

    #[test]
    fn invitations_is_paginated_and_free() {
        let r = ListInvitations::new("5YJ3E1EA1JF000001", Some(2), Some(25)).unwrap();
        assert!(matches!(r.pricing(), Pricing::NotCharged));
        let q = r.query().to_encoded();
        assert!(
            q.contains("page=2") && q.contains("per_page=25"),
            "실제: {q}"
        );
    }

    /// ⚠️ The client does not block values above 25 — that is left to the server's judgment.
    #[test]
    fn invitations_does_not_clamp_per_page() {
        let r = ListInvitations::new("5YJ3E1EA1JF000001", None, Some(999)).unwrap();
        assert!(r.query().to_encoded().contains("per_page=999"));
    }

    /// M1 canary: this must fail if `assemble` discards (ignores) pagination.
    /// Unlike `ListVehicles`/`GetVehicle`, **preserving** pagination is the right thing for
    /// this endpoint — this is the point that proves Task 2's plumbing is not exclusive to
    /// `ListVehicles`.
    #[test]
    fn invitations_assemble_preserves_items_and_pagination() {
        let wire = vec![Invitation {
            id: None,
            extra: Default::default(),
        }];
        let p = crate::model::Pagination {
            previous: None,
            next: Some(2),
            current: Some(1),
            per_page: Some(25),
            count: Some(30),
            pages: Some(2),
        };
        let page = <ListInvitations as FleetGet>::assemble(wire, Some(p));
        assert_eq!(page.items.len(), 1);
        assert_eq!(
            page.pagination.as_ref().and_then(|p| p.next),
            Some(2),
            "assemble 이 pagination 을 버리면 안 된다"
        );
    }

    /// When pagination is `None`, `Page.pagination` must be `None` as well — it must not
    /// turn into a filled-in default (see the `Page` documentation in model.rs).
    #[test]
    fn invitations_assemble_preserves_absent_pagination_as_none() {
        let wire: Vec<Invitation> = vec![];
        let page = <ListInvitations as FleetGet>::assemble(wire, None);
        assert!(page.pagination.is_none());
    }

    /// All 7 are unbilled, device_data and parameterless, and their paths are exact.
    #[test]
    fn simple_vehicle_endpoints_contract() {
        let vin = "5YJ3E1EA1JF000001";
        let cases: Vec<(String, ScopeExpr, Pricing, String)> = vec![
            {
                let r = GetMobileEnabled::new(vin).unwrap();
                (
                    r.path(),
                    r.required_scopes(),
                    r.pricing(),
                    r.query().to_encoded(),
                )
            },
            {
                let r = GetRecentAlerts::new(vin).unwrap();
                (
                    r.path(),
                    r.required_scopes(),
                    r.pricing(),
                    r.query().to_encoded(),
                )
            },
            {
                let r = GetReleaseNotes::new(vin).unwrap();
                (
                    r.path(),
                    r.required_scopes(),
                    r.pricing(),
                    r.query().to_encoded(),
                )
            },
            {
                let r = GetServiceData::new(vin).unwrap();
                (
                    r.path(),
                    r.required_scopes(),
                    r.pricing(),
                    r.query().to_encoded(),
                )
            },
            {
                let r = ListDrivers::new(vin).unwrap();
                (
                    r.path(),
                    r.required_scopes(),
                    r.pricing(),
                    r.query().to_encoded(),
                )
            },
            {
                let r = GetFleetTelemetryConfig::new(vin).unwrap();
                (
                    r.path(),
                    r.required_scopes(),
                    r.pricing(),
                    r.query().to_encoded(),
                )
            },
            {
                let r = GetFleetTelemetryErrors::new(vin).unwrap();
                (
                    r.path(),
                    r.required_scopes(),
                    r.pricing(),
                    r.query().to_encoded(),
                )
            },
        ];
        let expected_paths = [
            "mobile_enabled",
            "recent_alerts",
            "release_notes",
            "service_data",
            "drivers",
            "fleet_telemetry_config",
            "fleet_telemetry_errors",
        ];
        assert_eq!(cases.len(), expected_paths.len());
        for ((path, scopes, pricing, query), suffix) in cases.iter().zip(expected_paths) {
            assert!(path.ends_with(suffix), "경로 불일치: {path} vs {suffix}");
            assert!(path.starts_with("/api/1/vehicles/"), "{path}");
            assert!(
                matches!(pricing, Pricing::NotCharged),
                "{suffix} 는 무과금이어야 한다"
            );
            assert!(
                scopes.is_satisfied_by(&[Scope::VehicleDeviceData]),
                "{suffix}"
            );
            assert_eq!(query, "", "{suffix} 는 파라미터가 없다");
        }
    }

    /// VIN validation applies to all 7.
    #[test]
    fn simple_vehicle_endpoints_reject_an_unsafe_vin() {
        assert!(GetMobileEnabled::new("5YJ/../admin").is_err());
        assert!(GetRecentAlerts::new("5YJ/../admin").is_err());
        assert!(GetReleaseNotes::new("5YJ/../admin").is_err());
        assert!(GetServiceData::new("5YJ/../admin").is_err());
        assert!(ListDrivers::new("5YJ/../admin").is_err());
        assert!(GetFleetTelemetryConfig::new("5YJ/../admin").is_err());
        assert!(GetFleetTelemetryErrors::new("5YJ/../admin").is_err());
    }

    /// The `ends_with`/`starts_with` checks of the table test
    /// (`simple_vehicle_endpoints_contract`) also let through an implementation that returns
    /// a fixed string — they do not look at whether the VIN is actually carried in the path.
    /// This pins it as a positive case for the hand-written `GetFleetTelemetryErrors` only
    /// (for the same reason as `GetVehicle::get_vehicle_embeds_a_valid_vin_verbatim`).
    #[test]
    fn fleet_telemetry_errors_embeds_a_valid_vin_verbatim() {
        let r = GetFleetTelemetryErrors::new("5YJ3E1EA7KF000000").unwrap();
        assert_eq!(
            r.path(),
            "/api/1/vehicles/5YJ3E1EA7KF000000/fleet_telemetry_errors"
        );
        assert_eq!(r.vin(), "5YJ3E1EA7KF000000");
    }

    /// The test above alone cannot catch the "always fails" mutation (the case where the
    /// constructor is flipped to reject valid VINs too).
    #[test]
    fn fleet_telemetry_errors_new_accepts_a_valid_vin() {
        assert!(GetFleetTelemetryErrors::new("5YJ3E1EA7KF000000").is_ok());
    }

    /// If `path()` concatenated the original `vin` as is instead of `encoded_segment`, a VIN
    /// containing a literal `%` (but no `/`, so construction passes) would be carried into the
    /// path unchanged, and a downstream percent-decoder could mistake it for an encoded
    /// separator (the same threat model as `query.rs::vin_percent_encodes_literal_percent`).
    /// This case is essential for catching M3 (the mutation that passes the constructor's
    /// `encode_vin_segment` validation but has `path()` bypass the encoding).
    #[test]
    fn fleet_telemetry_errors_path_uses_the_encoded_segment_not_the_raw_vin() {
        let r = GetFleetTelemetryErrors::new("5YJ%2Fsecrets").unwrap();
        assert_eq!(
            r.path(),
            "/api/1/vehicles/5YJ%252Fsecrets/fleet_telemetry_errors",
            "실제: {}",
            r.path()
        );
        assert!(
            !r.path().contains("5YJ%2Fsecrets/fleet"),
            "실제: {}",
            r.path()
        );
    }
}

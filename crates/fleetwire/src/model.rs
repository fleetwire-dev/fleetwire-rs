//! Response and domain model types deserialized from Fleet API payloads.
//!
//! Every known field on every model here is `Option<T>` and every struct
//! carries a `#[serde(flatten)] extra` map, so a payload with fields Tesla
//! has added or omitted still deserializes instead of erroring (REQ-4).
//! **No type in this module opts into strict, closed-set field
//! validation** — fields this crate doesn't recognize always flow into
//! `extra` instead of failing deserialization, so an additive,
//! backward-compatible Tesla API change can never become a breaking change
//! for callers of this crate.
//!
//! **No type in this module implements `Debug`, and that is load-bearing**
//! (REQ-18; final fix wave / F10). [`VehicleSummary`] holds a VIN and an
//! `extra` map that in practice receives Tesla's `drive_state` — i.e. raw
//! GPS coordinates and the navigation destination address. Adding
//! `#[derive(Debug)]` to it is an ordinary, API-guidelines-endorsed change
//! that a future contributor would reach for without a second thought, and
//! it would print all of that verbatim into the first log line or
//! `unwrap()` panic that touched the value — the exact leak the
//! `crate::redaction` layer exists to prevent. If `Debug` is ever needed
//! here it MUST be hand-written and route every VIN/coordinate/free-text
//! location field through `redaction`; a derive is a REQ-18 violation.
//!
//! The structs here are `#[non_exhaustive]` (final fix wave / F8) so that
//! REQ-4's explicit growth path — promoting a field out of `extra` into a
//! named field as this crate learns Tesla's shape — stays a non-breaking
//! change for downstream callers. In-crate construction is unaffected.

use serde::Deserialize;
use serde_json::{Map, Value};

/// Tesla's outer `{"response": ...}` wrapper present on every Fleet API
/// payload.
///
/// Crate-private: only the decode path unwraps this once per call, so
/// endpoint callers only ever see the inner `T`, never the envelope itself.
#[derive(Deserialize)]
pub(crate) struct Envelope<T> {
    /// The payload nested inside Tesla's envelope.
    pub response: T,
    /// Page metadata that arrives as a **sibling** of `response` on list
    /// endpoints (measured 2026-08-03). `#[serde(default)]` so a response
    /// without the key (every non-list endpoint, and even some list
    /// responses) still parses instead of failing deserialization.
    ///
    /// The top-level `count` sibling is **not** modeled here — the one
    /// measured sample (a single-vehicle account) showed it equal to
    /// `pagination.count`, so modeling it separately would just produce two
    /// fields with the same name and the same value. Whether the two can
    /// ever diverge is unconfirmed (crate-level docs, open item O13).
    #[serde(default)]
    pub pagination: Option<Pagination>,
}

/// Minimal vehicle identity/state, tolerant of unknown or missing fields.
///
/// **Deliberately does not implement `Debug`** (REQ-18; final fix wave /
/// F10). This type carries a VIN in `vin` and, in practice, raw GPS
/// coordinates plus the navigation destination address inside `extra` (that
/// is where a `vehicle_data` payload's `drive_state` lands). A derived
/// `Debug` would print all of it verbatim the first time the value appeared
/// in a log line, an `assert_eq!` failure or an `unwrap()` panic message —
/// which is precisely what the `crate::redaction` layer exists to prevent, and it
/// would bypass that layer entirely because nothing routes a derive through
/// it. Adding `#[derive(Debug)]` here is a REQ-18 violation, not a
/// convenience: if `Debug` is needed, hand-write it and scrub `vin` and
/// every coordinate/location-shaped `extra` entry through `redaction`.
///
/// `#[non_exhaustive]` (final fix wave / F8): REQ-4's growth path is to
/// promote fields out of `extra` into named fields as this crate learns
/// Tesla's shape, and without this attribute each such promotion would be a
/// breaking change for any downstream caller using a struct literal or an
/// exhaustive destructuring pattern.
#[derive(Deserialize)]
#[non_exhaustive]
pub struct VehicleSummary {
    /// Vehicle id, kept as raw JSON rather than a fixed integer type since
    /// Tesla emits it as a number that can exceed safely-representable
    /// precision in some downstream (de)serializers.
    #[serde(default)]
    pub id: Option<Value>,
    /// Vehicle Identification Number.
    #[serde(default)]
    pub vin: Option<String>,
    /// Connectivity/shift state (e.g. `"online"`, `"asleep"`, `"offline"`).
    #[serde(default)]
    pub state: Option<String>,
    /// Response fields not modeled above, preserved losslessly.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// Cursor/offset metadata returned alongside a paginated list response.
#[derive(Deserialize)]
#[non_exhaustive]
pub struct Pagination {
    /// Previous page number, if any.
    pub previous: Option<u64>,
    /// Next page number, if any.
    pub next: Option<u64>,
    /// Current page number.
    pub current: Option<u64>,
    /// Requested page size.
    pub per_page: Option<u64>,
    /// Total item count across all pages.
    pub count: Option<u64>,
    /// Total page count.
    pub pages: Option<u64>,
}

/// One page of a paginated list response: items plus their [`Pagination`]
/// cursor.
///
/// Deliberately not `Deserialize` — a raw list response is a bare JSON array
/// alongside a sibling `pagination` object, not a single combined wire
/// shape. [`crate::endpoint::FleetGet::assemble`] is what puts the two
/// pieces together now (A4 Task 2) — through A3 the `pagination` sibling
/// never reached a caller at all, so nothing assembled a `Page` from
/// anything.
///
/// `pagination` is `Option`, and **not** filled in with a default when
/// absent: `None` means the server did not send pagination metadata at all,
/// which is a different fact from "it sent metadata and every field inside
/// was `null`". Collapsing the two by defaulting would let a caller read
/// `next == None` as "no more pages" and stop paginating early when the
/// truth is "the server told us nothing this time".
///
/// `#[non_exhaustive]` for the same reason as [`VehicleSummary`] (final fix
/// wave / F8) — a future field here (a raw cursor token, say) must not be a
/// breaking change.
#[non_exhaustive]
pub struct Page<T> {
    /// The page's items.
    pub items: Vec<T>,
    /// Cursor/offset metadata for this page, or `None` if the server sent
    /// none.
    pub pagination: Option<Pagination>,
}

/// `vehicle_data` response.
///
/// Sub-object fields have **not been verified against a live response**.
/// Per this crate's founding response-typing policy, speculative field
/// names are not exposed as a strongly-typed public API — each sub-object
/// stays a `serde_json::Value` until its shape is verified, at which point
/// it can be promoted (REQ-4's growth path). `#[serde(flatten)] extra`
/// preserves everything else losslessly in the meantime, so nothing is
/// lost by staying conservative here.
///
/// Deliberately does not implement `Debug`, for the same REQ-18 reason as
/// [`VehicleSummary`] — this type carries a VIN and, inside `extra`/the
/// sub-object `Value`s, raw GPS coordinates when `location_data` was
/// requested.
#[derive(Deserialize)]
#[non_exhaustive]
pub struct VehicleData {
    /// Vehicle id, kept as raw JSON for the same reason as
    /// [`VehicleSummary::id`].
    #[serde(default)]
    pub id: Option<Value>,
    /// Vehicle Identification Number.
    #[serde(default)]
    pub vin: Option<String>,
    /// Connectivity/shift state (e.g. `"online"`, `"asleep"`, `"offline"`).
    #[serde(default)]
    pub state: Option<String>,
    /// Battery/charging sub-object — unverified shape, see the type's doc.
    #[serde(default)]
    pub charge_state: Option<Value>,
    /// Cabin climate sub-object — unverified shape, see the type's doc.
    #[serde(default)]
    pub climate_state: Option<Value>,
    /// Shift state/location sub-object — unverified shape, see the type's
    /// doc. Only present when [`crate::api::vehicles::DataEndpoint::DriveState`]
    /// was requested and, for GPS coordinates, only when
    /// [`crate::api::vehicles::DataEndpoint::LocationData`] was also
    /// requested and the token held `vehicle_location`.
    #[serde(default)]
    pub drive_state: Option<Value>,
    /// Odometer/locks/sentry/software-version sub-object — unverified
    /// shape, see the type's doc.
    #[serde(default)]
    pub vehicle_state: Option<Value>,
    /// Response fields not modeled above, preserved losslessly.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `nearby_charging_sites` response.
///
/// The shape of the sub-lists' items has **not been verified against a
/// live response** — per this crate's founding response-typing policy,
/// speculative field names are not exposed as a strongly-typed public API,
/// so each list stays `Vec<serde_json::Value>` until its shape is verified
/// (REQ-4's growth path). `#[serde(flatten)] extra` preserves everything
/// else losslessly in the meantime, so nothing is lost by staying
/// conservative here.
#[derive(Deserialize)]
#[non_exhaustive]
pub struct NearbyChargingSites {
    /// Timestamp (Unix seconds) the congestion data was last synced.
    #[serde(default)]
    pub congestion_sync_time_utc_secs: Option<i64>,
    /// Nearby destination chargers — unverified item shape, see the type's
    /// doc.
    #[serde(default)]
    pub destination_charging: Option<Vec<Value>>,
    /// Nearby Superchargers — unverified item shape, see the type's doc.
    #[serde(default)]
    pub superchargers: Option<Vec<Value>>,
    /// Response fields not modeled above, preserved losslessly.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// `invitations` response item — a vehicle-sharing invitation.
///
/// **Unverified shape** — no live response has been observed for this
/// endpoint. Per this crate's founding response-typing policy, speculative
/// field names are not exposed as a strongly-typed public API, so this stays
/// minimal (only `id`, itself kept as raw JSON rather than guessed at) with
/// `#[serde(flatten)] extra` preserving everything else losslessly. Promote
/// fields out of `extra` once the real shape is confirmed (REQ-4's growth
/// path).
#[derive(Deserialize)]
#[non_exhaustive]
pub struct Invitation {
    /// Invitation id, kept as raw JSON for the same reason as
    /// [`VehicleSummary::id`] — unverified whether Tesla emits this as a
    /// number or string.
    #[serde(default)]
    pub id: Option<Value>,
    /// Response fields not modeled above, preserved losslessly.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tolerates_extra_and_missing() {
        let j = r#"{"vin":"5YJ...","new_field_tesla_added":42}"#;
        let v: VehicleSummary = serde_json::from_str(j).unwrap();
        assert_eq!(v.vin.as_deref(), Some("5YJ..."));
        assert!(v.state.is_none()); // 누락 필드 → None
        assert!(v.extra.contains_key("new_field_tesla_added")); // 미지 필드 보존
    }
    #[test]
    fn envelope_unwraps() {
        let j = r#"{"response":{"vin":"X"}}"#;
        let e: Envelope<VehicleSummary> = serde_json::from_str(j).unwrap();
        assert_eq!(e.response.vin.as_deref(), Some("X"));
    }

    // --- edge cases beyond the brief's two required tests ---

    #[test]
    fn extra_value_is_preserved_losslessly_not_just_the_key() {
        let j = r#"{"vin":"X","new_field":42}"#;
        let v: VehicleSummary = serde_json::from_str(j).unwrap();
        assert_eq!(v.extra.get("new_field"), Some(&Value::from(42)));
    }

    #[test]
    fn explicit_null_on_known_field_becomes_none_not_extra() {
        let j = r#"{"vin":"X","state":null}"#;
        let v: VehicleSummary = serde_json::from_str(j).unwrap();
        assert!(v.state.is_none());
        // consumed by the named `state` field, not by the flatten catch-all
        assert!(!v.extra.contains_key("state"));
    }

    #[test]
    fn id_accepts_numeric_json_value() {
        let j = r#"{"id":215299}"#;
        let v: VehicleSummary = serde_json::from_str(j).unwrap();
        assert_eq!(v.id, Some(Value::from(215299)));
    }

    #[test]
    fn empty_object_leaves_all_known_fields_none_and_extra_empty() {
        let v: VehicleSummary = serde_json::from_str("{}").unwrap();
        assert!(v.id.is_none());
        assert!(v.vin.is_none());
        assert!(v.state.is_none());
        assert!(v.extra.is_empty());
    }

    #[test]
    fn pagination_missing_fields_default_to_none() {
        let p: Pagination = serde_json::from_str("{}").unwrap();
        assert_eq!(p.previous, None);
        assert_eq!(p.next, None);
        assert_eq!(p.current, None);
        assert_eq!(p.per_page, None);
        assert_eq!(p.count, None);
        assert_eq!(p.pages, None);
    }

    #[test]
    fn pagination_parses_partial_fields() {
        let j = r#"{"current":1,"per_page":100,"count":1}"#;
        let p: Pagination = serde_json::from_str(j).unwrap();
        assert_eq!(p.current, Some(1));
        assert_eq!(p.per_page, Some(100));
        assert_eq!(p.count, Some(1));
        assert_eq!(p.next, None);
    }

    #[test]
    fn page_is_assembled_manually_from_items_and_pagination() {
        let page = Page {
            items: vec![VehicleSummary {
                id: None,
                vin: Some("A".into()),
                state: None,
                extra: Map::new(),
            }],
            pagination: Some(Pagination {
                previous: None,
                next: None,
                current: Some(1),
                per_page: Some(1),
                count: Some(1),
                pages: Some(1),
            }),
        };
        assert_eq!(page.items.len(), 1);
        assert_eq!(page.items[0].vin.as_deref(), Some("A"));
        assert_eq!(page.pagination.unwrap().current, Some(1));
    }
}

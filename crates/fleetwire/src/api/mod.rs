//! Concrete [`crate::endpoint::FleetGet`] implementations.
//!
//! This module only **implements** the endpoint traits — it does not define
//! them. `FleetGet`/`VehicleEndpoint` (and the crate-private `Sealed`
//! supertrait that closes them to external implementations) live in
//! [`crate::endpoint`]; every type here is one of the 16 live-verified Fleet
//! API GETs this crate models.

/// `WireResponse`/`assemble` boilerplate for endpoints with no pagination.
///
/// The wire type and the public type are the same, and any sibling metadata
/// is dropped.
macro_rules! passthrough_endpoint {
    ($t:ty, $resp:ty) => {
        type WireResponse = $resp;
        type Response = $resp;
        fn assemble(
            wire: Self::WireResponse,
            _pagination: Option<$crate::model::Pagination>,
        ) -> Self::Response {
            wire
        }
    };
}
// `users`/`vehicles` (declared below, textually after this point) see the
// macro without importing it — `macro_rules!` visibility is textual/lexical,
// not path-based. The re-export below exists only for modules outside that
// textual scope: `client.rs`'s own `#[cfg(test)]` fixture (`BilledLikeRegion`)
// is the sole such consumer today, so the export itself is `cfg(test)`-gated
// too — otherwise a non-test build has no user of the path and clippy's
// `unused_imports` (via `-D warnings`) rejects it.
#[cfg(test)]
pub(crate) use passthrough_endpoint;

pub mod users;
pub mod vehicles;

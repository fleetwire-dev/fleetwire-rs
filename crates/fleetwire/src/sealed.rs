//! Sealed marker preventing external `FleetGet` implementations.

/// Crate-private supertrait; external crates cannot implement it.
pub trait Sealed {}

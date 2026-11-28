//! Tesla's published Fleet API price table, and the cost of a call worked out before it is sent.
//!
//! This crate does not send anything. It reads the pricing category a `fleetwire` request type
//! already carries and turns it into an amount of money, in whole microdollars
//! ([`Microdollars`]) so that later sums are exact.
//!
//! ```
//! use fleetwire::api::vehicles::{DataEndpoint, GetVehicle, GetVehicleData};
//! use fleetwire_cost::{cost_of_request, Microdollars};
//!
//! // A made-up VIN. The requests are only built, never sent.
//! let vin = "00000000000000000";
//! let paid = GetVehicleData::new(vin, vec![DataEndpoint::ChargeState]).unwrap();
//! let free = GetVehicle::new(vin).unwrap();
//!
//! assert_eq!(cost_of_request(&paid), Ok(Microdollars::new(2_000))); // $0.002
//! assert_eq!(cost_of_request(&free), Ok(Microdollars::ZERO));
//! ```
//!
//! Not in this crate yet: a spending limit, a ledger file, refusing a call. Those come later in
//! the 0.3.0 work.

mod cost;
mod money;
mod table;

pub use cost::{CostError, cost_of_pricing, cost_of_request, cost_of_units};
pub use money::Microdollars;
pub use table::{Category, MONTHLY_DISCOUNT};

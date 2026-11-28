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
//! A [`DailyLimitGuard`] refuses a paid call before it is sent when it would take the day's total
//! above a limit. It counts only the calls that pass through it, keeps the total in memory, and
//! takes the day from the caller ([`DayKey`]) instead of reading the system clock.
//!
//! Not in this crate yet: a ledger file that keeps the total across runs. It comes later in the
//! 0.3.0 work.

mod cost;
mod limit;
mod money;
mod table;

pub use cost::{CostError, cost_of_pricing, cost_of_request, cost_of_units};
pub use limit::{DailyLimitGuard, DayKey, GuardError, LimitError, check_daily_limit};
pub use money::Microdollars;
pub use table::{Category, MONTHLY_DISCOUNT};

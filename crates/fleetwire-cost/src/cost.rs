//! The cost of a call, worked out before it is sent.

use std::fmt;

use fleetwire::{FleetGet, Pricing};

use crate::money::{Microdollars, PER_USD};
use crate::table::Category;

/// Why a cost could not be worked out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CostError {
    /// The request carries a pricing category this crate does not know. It is not assumed free.
    UnknownPricing,
    /// The request's own Data rate differs from the published table, so one of them is out of date.
    RateMismatch,
    /// The amount does not fit in a `u64` of microdollars.
    Overflow,
}

impl fmt::Display for CostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            CostError::UnknownPricing => "unknown pricing category, cost not assumed to be zero",
            CostError::RateMismatch => "request data rate differs from the published price table",
            CostError::Overflow => "cost does not fit in u64 microdollars",
        })
    }
}

impl std::error::Error for CostError {}

/// The cost of `units` units of `category`, rounded **up** to a whole microdollar.
///
/// Data, Command and Wake prices are whole microdollars, so their costs are exact. A streamed
/// signal costs about 6.67 microdollars, so a signal count is rounded up: a limit built on this never
/// counts less than Tesla's rate.
pub fn cost_of_units(category: Category, units: u64) -> Result<Microdollars, CostError> {
    let per_usd = u128::from(category.units_per_usd());
    let scaled = u128::from(units) * u128::from(PER_USD);
    let micros = scaled.div_ceil(per_usd);
    u64::try_from(micros)
        .map(Microdollars::new)
        .map_err(|_| CostError::Overflow)
}

/// The cost of one request with this pricing.
///
/// `NotCharged` costs zero. `Data` costs the published Data rate, after checking that the rate the
/// request carries agrees with the table. A category this crate does not know is an error,
/// never zero.
pub fn cost_of_pricing(pricing: Pricing) -> Result<Microdollars, CostError> {
    match pricing {
        Pricing::NotCharged => Ok(Microdollars::ZERO),
        Pricing::Data { usd_per_request } => {
            let table = cost_of_units(Category::Data, 1)?;
            // The request's rate is an f64 in dollars; the table's is exact. Agree to well under
            // one microdollar, or refuse to guess which one is right.
            let table_usd = table.get() as f64 / PER_USD as f64;
            if (usd_per_request - table_usd).abs() < 1e-9 {
                Ok(table)
            } else {
                Err(CostError::RateMismatch)
            }
        }
        _ => Err(CostError::UnknownPricing),
    }
}

/// The cost of sending `request` once, read from its type without any network call.
pub fn cost_of_request<R: FleetGet>(request: &R) -> Result<Microdollars, CostError> {
    cost_of_pricing(request.pricing())
}

#[cfg(test)]
mod tests;

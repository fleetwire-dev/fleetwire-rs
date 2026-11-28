//! Tesla's published usage-based price table for the Fleet API.
//!
//! Source: the "Usage Based Pricing" section of <https://developer.tesla.com/>, which the page
//! <https://developer.tesla.com/docs/fleet-api/billing-and-limits> points to for the rates. Read
//! on 2026-11-28; neither page carries a revision date. The rates are stored in the form Tesla
//! publishes them (units per dollar), so the table can be compared with the page line by line:
//!
//! | Category          | Published rate        | Per unit       |
//! |-------------------|-----------------------|----------------|
//! | Streaming signals | 150,000 signals / $1  | about $0.0000067 |
//! | Commands          | 1,000 requests / $1   | $0.001         |
//! | Data              | 500 requests / $1     | $0.002         |
//! | Wakes             | 50 requests / $1      | $0.02          |
//!
//! The same billing page says that requests with a status code below 500 are billed, and that
//! the monthly charge is rounded to the nearest $0.01. Neither rule is applied here: this table
//! prices a single call before it is sent.
//!
//! Listing Command and Wake prices records the published table. It does not add a command or a
//! wake request to this project, which sends GET requests only.

use crate::money::Microdollars;

/// A usage-based pricing category from Tesla's published table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Category {
    /// A paid read of vehicle data ("Data", 500 requests per dollar).
    Data,
    /// A vehicle command ("Commands", 1,000 requests per dollar).
    Command,
    /// A wake request ("Wakes", 50 requests per dollar).
    Wake,
    /// One signal streamed by Fleet Telemetry ("Streaming Signals", 150,000 signals per dollar).
    StreamingSignal,
}

impl Category {
    /// Every category in the table.
    pub const ALL: [Category; 4] = [
        Category::Data,
        Category::Command,
        Category::Wake,
        Category::StreamingSignal,
    ];

    /// How many units one US dollar buys, exactly as Tesla publishes it.
    pub const fn units_per_usd(self) -> u64 {
        match self {
            Category::Data => 500,
            Category::Command => 1_000,
            Category::Wake => 50,
            Category::StreamingSignal => 150_000,
        }
    }
}

/// The monthly discount Tesla gives each account: $10.
///
/// Source: <https://developer.tesla.com/> ("Each account receives a $10 monthly discount"); the
/// billing page calls it "a monthly discount of $10". Read on 2026-11-28. The pre-call cost of a
/// request does not subtract it.
pub const MONTHLY_DISCOUNT: Microdollars = Microdollars::new(10_000_000);

#[cfg(test)]
mod tests;

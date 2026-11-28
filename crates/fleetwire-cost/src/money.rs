//! An amount of US dollars in whole microdollars.

use std::fmt;

/// An amount of US dollars in whole microdollars (one millionth of a dollar).
///
/// An integer, so that adding many small charges gives the exact total. A Data request is
/// `Microdollars::new(2_000)`, i.e. $0.002.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Microdollars(u64);

/// Microdollars in one dollar.
pub(crate) const PER_USD: u64 = 1_000_000;

impl Microdollars {
    /// No money.
    pub const ZERO: Microdollars = Microdollars(0);

    /// An amount given in microdollars.
    pub const fn new(micros: u64) -> Microdollars {
        Microdollars(micros)
    }

    /// The amount in microdollars.
    pub const fn get(self) -> u64 {
        self.0
    }

    /// The sum, or `None` if it does not fit in a `u64`.
    pub const fn checked_add(self, other: Microdollars) -> Option<Microdollars> {
        match self.0.checked_add(other.0) {
            Some(v) => Some(Microdollars(v)),
            None => None,
        }
    }
}

/// Dollars with all six decimal places, e.g. `$0.002000`.
impl fmt::Display for Microdollars {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "${}.{:06}", self.0 / PER_USD, self.0 % PER_USD)
    }
}

#[cfg(test)]
mod tests;

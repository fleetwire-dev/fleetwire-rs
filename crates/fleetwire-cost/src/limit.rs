//! A daily spending limit that refuses a paid call before it is sent.
//!
//! [`check_daily_limit`] is the decision on its own: given the limit, what has been spent today
//! and the cost of the next call, it says whether the call may go out. [`DailyLimitGuard`] keeps
//! the running total for one day in memory and asks that question before it hands a request to
//! the caller's send function.
//!
//! The guard counts only the calls that pass through it. It does not read the system clock: the
//! caller names the day with a [`DayKey`] on every call, and a later day starts a new total. It
//! does not write anything to disk; the total is lost when the guard is dropped.
//!
//! The day only moves forward. A call named with a far later day (a clock that jumped ahead, or a
//! wrong [`DayKey::new`]) makes that the current day, and every call after it is counted against
//! that day until a later key comes or the guard is replaced. This fails towards refusing, not
//! towards spending. There is no reset method; build a new guard to start over.

use std::fmt;

use fleetwire::FleetGet;

use crate::cost::{CostError, cost_of_request};
use crate::money::Microdollars;

/// Seconds in a day.
const SECS_PER_DAY: i64 = 86_400;

/// The day a call is counted against, as chosen by the caller.
///
/// Any numbering works as long as a later day has a larger key. [`DayKey::from_unix_seconds`]
/// gives the usual one: whole days since the Unix epoch in a fixed UTC offset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DayKey(i64);

impl DayKey {
    /// A day named by the caller's own number.
    pub const fn new(day: i64) -> DayKey {
        DayKey(day)
    }

    /// The day that contains `unix_secs`, in local time `utc_offset_secs` ahead of UTC.
    ///
    /// Pure arithmetic on the arguments; the clock is the caller's to read. For example, with a
    /// UTC offset of `+09:00` (`32_400` seconds) the day changes at 15:00 UTC.
    pub const fn from_unix_seconds(unix_secs: i64, utc_offset_secs: i32) -> DayKey {
        // i64 + i32 cannot overflow except within a day of i64::MAX; saturate rather than wrap.
        let local = unix_secs.saturating_add(utc_offset_secs as i64);
        DayKey(local.div_euclid(SECS_PER_DAY))
    }

    /// The caller's number for this day.
    pub const fn get(self) -> i64 {
        self.0
    }
}

/// Why a call was refused before it was sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum LimitError {
    /// Sending the call would take today's total above the limit.
    Exceeded {
        /// The daily limit.
        limit: Microdollars,
        /// What has already been counted today.
        spent: Microdollars,
        /// What this call would cost.
        cost: Microdollars,
    },
    /// The cost of the call could not be worked out, so it is not assumed to be free.
    Cost(CostError),
}

impl fmt::Display for LimitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LimitError::Exceeded { limit, spent, cost } => write!(
                f,
                "daily limit reached: {spent} spent plus {cost} for this call is over {limit}"
            ),
            LimitError::Cost(e) => write!(f, "call refused because its cost is unknown: {e}"),
        }
    }
}

impl std::error::Error for LimitError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LimitError::Cost(e) => Some(e),
            LimitError::Exceeded { .. } => None,
        }
    }
}

/// Whether a call costing `cost` may be sent when `spent` has been counted today.
///
/// Returns today's total after the call. The call is refused when that total would be above
/// `limit`; a total exactly at the limit is allowed. A call that costs nothing is always allowed,
/// even when today's total is already over the limit. A cost that could not be worked out
/// (`Err`) is refused, never treated as zero. A total that does not fit in a `u64` is above the
/// limit and is refused as [`LimitError::Exceeded`].
pub fn check_daily_limit(
    limit: Microdollars,
    spent: Microdollars,
    cost: Result<Microdollars, CostError>,
) -> Result<Microdollars, LimitError> {
    let cost = cost.map_err(LimitError::Cost)?;
    if cost == Microdollars::ZERO {
        return Ok(spent);
    }
    match spent.checked_add(cost) {
        Some(total) if total <= limit => Ok(total),
        // A sum too large for a u64 is above any u64 limit.
        Some(_) | None => Err(LimitError::Exceeded { limit, spent, cost }),
    }
}

/// What came of [`DailyLimitGuard::send`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuardError<E> {
    /// The call was refused and the send function was not called.
    Refused(LimitError),
    /// The send function was called and returned this error. Its cost stays counted.
    Send(E),
}

impl<E: fmt::Display> fmt::Display for GuardError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GuardError::Refused(e) => write!(f, "{e}"),
            GuardError::Send(e) => write!(f, "{e}"),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for GuardError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            GuardError::Refused(e) => Some(e),
            GuardError::Send(e) => Some(e),
        }
    }
}

/// A daily limit on the cost of the calls that pass through it, kept in memory.
///
/// ```
/// use fleetwire::api::vehicles::{DataEndpoint, GetVehicleData};
/// use fleetwire_cost::{DailyLimitGuard, DayKey, GuardError, LimitError, Microdollars};
///
/// // Room for exactly one Data request ($0.002) a day.
/// let mut guard = DailyLimitGuard::new(Microdollars::new(2_000));
/// let today = DayKey::new(1);
/// // A made-up VIN. The request is only built; the "send" below is a stand-in.
/// let req = GetVehicleData::new("00000000000000000", vec![DataEndpoint::ChargeState]).unwrap();
///
/// let mut sent = 0;
/// let first: Result<(), GuardError<()>> = guard.send(today, &req, |_| { sent += 1; Ok(()) });
/// assert!(first.is_ok());
/// let second: Result<(), GuardError<()>> = guard.send(today, &req, |_| { sent += 1; Ok(()) });
/// assert!(matches!(second, Err(GuardError::Refused(LimitError::Exceeded { .. }))));
/// assert_eq!(sent, 1);
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyLimitGuard {
    limit: Microdollars,
    day: Option<DayKey>,
    spent: Microdollars,
}

impl DailyLimitGuard {
    /// A guard that allows up to `limit` a day, with nothing counted yet.
    pub const fn new(limit: Microdollars) -> DailyLimitGuard {
        DailyLimitGuard {
            limit,
            day: None,
            spent: Microdollars::ZERO,
        }
    }

    /// The daily limit.
    pub const fn limit(&self) -> Microdollars {
        self.limit
    }

    /// What has been counted against `day`.
    ///
    /// Zero for a day after the one being counted. An earlier day reads as the current day's
    /// total, because that is what a call naming it would be counted against.
    pub fn spent(&self, day: DayKey) -> Microdollars {
        match self.day {
            Some(current) if day > current => Microdollars::ZERO,
            _ => self.spent,
        }
    }

    /// Count the cost of `request` against `day`, or refuse it.
    ///
    /// On success the cost is counted before anything is sent, and the caller must then send the
    /// request exactly once. Use [`DailyLimitGuard::send`] to keep the two together.
    pub fn admit<R: FleetGet>(&mut self, day: DayKey, request: &R) -> Result<(), LimitError> {
        self.admit_cost(day, cost_of_request(request))
    }

    /// Count the cost of `request` against `day` and call `send` with it, or refuse it without
    /// calling `send`.
    ///
    /// The cost is counted before `send` runs and is not taken back if `send` fails: whether a
    /// failed request was billed is not known here, so it is counted as if it was.
    pub fn send<R, T, E, F>(
        &mut self,
        day: DayKey,
        request: &R,
        send: F,
    ) -> Result<T, GuardError<E>>
    where
        R: FleetGet,
        F: FnOnce(&R) -> Result<T, E>,
    {
        self.send_with_cost(day, cost_of_request(request), request, send)
    }

    /// [`DailyLimitGuard::send`] with the cost already worked out, so tests can reach the arms a
    /// real request type cannot produce.
    fn send_with_cost<R, T, E, F>(
        &mut self,
        day: DayKey,
        cost: Result<Microdollars, CostError>,
        request: &R,
        send: F,
    ) -> Result<T, GuardError<E>>
    where
        F: FnOnce(&R) -> Result<T, E>,
    {
        self.admit_cost(day, cost).map_err(GuardError::Refused)?;
        send(request).map_err(GuardError::Send)
    }

    fn admit_cost(
        &mut self,
        day: DayKey,
        cost: Result<Microdollars, CostError>,
    ) -> Result<(), LimitError> {
        // A later day starts a new total. An earlier day (a clock stepped back) does not: it is
        // counted against the current day, so a clock change cannot reopen spent budget.
        let spent = match self.day {
            Some(current) if day <= current => self.spent,
            _ => Microdollars::ZERO,
        };
        let total = check_daily_limit(self.limit, spent, cost)?;
        if self.day.is_none_or(|current| day > current) {
            self.day = Some(day);
        }
        self.spent = total;
        Ok(())
    }
}

#[cfg(test)]
mod tests;

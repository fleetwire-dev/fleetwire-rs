use super::*;

use std::cell::Cell;

use fleetwire::Pricing;
use fleetwire::api::vehicles::{DataEndpoint, GetVehicle, GetVehicleData};

use crate::cost::cost_of_pricing;

/// A made-up VIN. The requests are only built; the send functions below never reach a network.
const VIN: &str = "00000000000000000";

/// One Data request, in microdollars.
const DATA: Microdollars = Microdollars::new(2_000);

fn paid() -> GetVehicleData {
    GetVehicleData::new(VIN, vec![DataEndpoint::ChargeState]).unwrap()
}

fn free() -> GetVehicle {
    GetVehicle::new(VIN).unwrap()
}

/// A stand-in for the HTTP send that only counts how often it was called.
struct FakeSend {
    calls: Cell<u32>,
}

impl FakeSend {
    fn new() -> FakeSend {
        FakeSend {
            calls: Cell::new(0),
        }
    }

    fn ok<R>(&self) -> impl FnOnce(&R) -> Result<(), ()> + '_ {
        move |_| {
            self.calls.set(self.calls.get() + 1);
            Ok(())
        }
    }

    fn calls(&self) -> u32 {
        self.calls.get()
    }
}

// ── check_daily_limit: the threshold, one arm at a time ─────────────────────────────────────

#[test]
fn a_total_just_below_the_limit_is_allowed() {
    let limit = Microdollars::new(10_000);
    assert_eq!(
        check_daily_limit(limit, Microdollars::new(7_999), Ok(DATA)),
        Ok(Microdollars::new(9_999))
    );
}

#[test]
fn a_total_exactly_at_the_limit_is_allowed() {
    let limit = Microdollars::new(10_000);
    assert_eq!(
        check_daily_limit(limit, Microdollars::new(8_000), Ok(DATA)),
        Ok(Microdollars::new(10_000))
    );
}

#[test]
fn a_total_just_above_the_limit_is_refused() {
    let limit = Microdollars::new(10_000);
    assert_eq!(
        check_daily_limit(limit, Microdollars::new(8_001), Ok(DATA)),
        Err(LimitError::Exceeded {
            limit,
            spent: Microdollars::new(8_001),
            cost: DATA,
        })
    );
}

#[test]
fn a_zero_limit_refuses_every_paid_call() {
    assert!(matches!(
        check_daily_limit(Microdollars::ZERO, Microdollars::ZERO, Ok(DATA)),
        Err(LimitError::Exceeded { .. })
    ));
}

#[test]
fn a_free_call_passes_at_and_over_the_limit() {
    let limit = Microdollars::new(2_000);
    for spent in [2_000, 2_001, u64::MAX] {
        let spent = Microdollars::new(spent);
        assert_eq!(
            check_daily_limit(limit, spent, Ok(Microdollars::ZERO)),
            Ok(spent)
        );
    }
}

#[test]
fn unknown_pricing_is_refused_not_free() {
    assert_eq!(
        check_daily_limit(
            Microdollars::new(u64::MAX),
            Microdollars::ZERO,
            Err(CostError::UnknownPricing)
        ),
        Err(LimitError::Cost(CostError::UnknownPricing))
    );
}

#[test]
fn a_rate_mismatch_is_refused_not_free() {
    let cost = cost_of_pricing(Pricing::Data {
        usd_per_request: 0.001,
    });
    assert_eq!(cost, Err(CostError::RateMismatch), "precondition");
    assert_eq!(
        check_daily_limit(Microdollars::new(u64::MAX), Microdollars::ZERO, cost),
        Err(LimitError::Cost(CostError::RateMismatch))
    );
}

#[test]
fn a_total_that_overflows_is_refused() {
    assert_eq!(
        check_daily_limit(
            Microdollars::new(u64::MAX),
            Microdollars::new(u64::MAX - 1_999),
            Ok(DATA)
        ),
        Err(LimitError::Exceeded {
            limit: Microdollars::new(u64::MAX),
            spent: Microdollars::new(u64::MAX - 1_999),
            cost: DATA,
        })
    );
    // One microdollar less and the same sum fits exactly at the limit.
    assert_eq!(
        check_daily_limit(
            Microdollars::new(u64::MAX),
            Microdollars::new(u64::MAX - 2_000),
            Ok(DATA)
        ),
        Ok(Microdollars::new(u64::MAX))
    );
}

// ── DayKey ──────────────────────────────────────────────────────────────────────────────────

#[test]
fn utc_day_changes_between_the_last_and_first_second() {
    assert_eq!(DayKey::from_unix_seconds(86_399, 0), DayKey::new(0));
    assert_eq!(DayKey::from_unix_seconds(86_400, 0), DayKey::new(1));
}

#[test]
fn an_offset_moves_the_day_boundary() {
    // At +09:00 the day changes at 15:00 UTC.
    let kst = 9 * 3_600;
    let boundary = 86_400 - kst as i64;
    assert_eq!(DayKey::from_unix_seconds(boundary - 1, kst), DayKey::new(0));
    assert_eq!(DayKey::from_unix_seconds(boundary, kst), DayKey::new(1));
}

#[test]
fn times_before_the_epoch_round_down_to_the_earlier_day() {
    assert_eq!(DayKey::from_unix_seconds(-1, 0), DayKey::new(-1));
    assert_eq!(DayKey::from_unix_seconds(-86_400, 0), DayKey::new(-1));
    assert_eq!(DayKey::from_unix_seconds(-86_401, 0), DayKey::new(-2));
}

#[test]
fn extreme_times_do_not_panic() {
    assert_eq!(
        DayKey::from_unix_seconds(i64::MAX, i32::MAX),
        DayKey::new(i64::MAX.div_euclid(86_400))
    );
    assert_eq!(
        DayKey::from_unix_seconds(i64::MIN, i32::MIN),
        DayKey::new(i64::MIN.div_euclid(86_400))
    );
}

// ── DailyLimitGuard: nothing is sent when a call is refused ─────────────────────────────────

#[test]
fn a_paid_call_at_the_limit_is_refused_with_no_request_sent() {
    let mut guard = DailyLimitGuard::new(DATA.checked_add(DATA).unwrap());
    let day = DayKey::new(10);
    let net = FakeSend::new();

    assert_eq!(guard.send(day, &paid(), net.ok()), Ok(()));
    assert_eq!(guard.send(day, &paid(), net.ok()), Ok(()));
    assert_eq!(
        net.calls(),
        2,
        "precondition: both calls under the limit went out"
    );
    assert_eq!(guard.spent(day), Microdollars::new(4_000));

    let refused = guard.send(day, &paid(), net.ok());
    assert_eq!(
        refused,
        Err(GuardError::Refused(LimitError::Exceeded {
            limit: Microdollars::new(4_000),
            spent: Microdollars::new(4_000),
            cost: DATA,
        }))
    );
    assert_eq!(net.calls(), 2, "the refused call reached the send function");
    assert_eq!(guard.spent(day), Microdollars::new(4_000));
}

#[test]
fn admit_refuses_at_the_limit_and_does_not_count_the_refusal() {
    let mut guard = DailyLimitGuard::new(DATA);
    let day = DayKey::new(10);
    assert_eq!(guard.admit(day, &paid()), Ok(()));
    assert!(matches!(
        guard.admit(day, &paid()),
        Err(LimitError::Exceeded { .. })
    ));
    assert_eq!(guard.spent(day), DATA);
}

#[test]
fn a_free_call_is_sent_when_the_limit_is_used_up() {
    let mut guard = DailyLimitGuard::new(DATA);
    let day = DayKey::new(10);
    let net = FakeSend::new();
    assert_eq!(guard.send(day, &paid(), net.ok()), Ok(()));
    assert!(guard.send(day, &paid(), net.ok()).is_err(), "precondition");

    assert_eq!(guard.send(day, &free(), net.ok()), Ok(()));
    assert_eq!(net.calls(), 2);
    assert_eq!(guard.spent(day), DATA);
}

#[test]
fn an_unknown_cost_sends_nothing() {
    for err in [CostError::UnknownPricing, CostError::RateMismatch] {
        let mut guard = DailyLimitGuard::new(Microdollars::new(u64::MAX));
        let net = FakeSend::new();
        let out = guard.send_with_cost(DayKey::new(10), Err(err), &paid(), net.ok());
        assert_eq!(out, Err(GuardError::Refused(LimitError::Cost(err))));
        assert_eq!(net.calls(), 0, "{err:?}");
        assert_eq!(guard.spent(DayKey::new(10)), Microdollars::ZERO);
    }
}

#[test]
fn a_rate_mismatch_from_a_real_pricing_sends_nothing() {
    let mut guard = DailyLimitGuard::new(Microdollars::new(u64::MAX));
    let net = FakeSend::new();
    let cost = cost_of_pricing(Pricing::Data {
        usd_per_request: 0.003,
    });
    let out = guard.send_with_cost(DayKey::new(10), cost, &paid(), net.ok());
    assert_eq!(
        out,
        Err(GuardError::Refused(LimitError::Cost(
            CostError::RateMismatch
        )))
    );
    assert_eq!(net.calls(), 0);
}

#[test]
fn a_failed_send_stays_counted() {
    let mut guard = DailyLimitGuard::new(DATA);
    let day = DayKey::new(10);
    let out: Result<(), GuardError<&str>> = guard.send(day, &paid(), |_| Err("timed out"));
    assert_eq!(out, Err(GuardError::Send("timed out")));
    assert_eq!(guard.spent(day), DATA);

    let net = FakeSend::new();
    assert!(guard.send(day, &paid(), net.ok()).is_err());
    assert_eq!(net.calls(), 0);
}

// ── DailyLimitGuard: the day boundary ───────────────────────────────────────────────────────

#[test]
fn the_next_day_starts_a_new_total() {
    let mut guard = DailyLimitGuard::new(DATA);
    let net = FakeSend::new();
    let monday = DayKey::from_unix_seconds(86_399, 0);
    let tuesday = DayKey::from_unix_seconds(86_400, 0);

    assert_eq!(guard.send(monday, &paid(), net.ok()), Ok(()));
    assert!(
        guard.send(monday, &paid(), net.ok()).is_err(),
        "precondition"
    );
    assert_eq!(net.calls(), 1);

    assert_eq!(guard.spent(tuesday), Microdollars::ZERO);
    assert_eq!(guard.send(tuesday, &paid(), net.ok()), Ok(()));
    assert_eq!(net.calls(), 2);
    assert_eq!(guard.spent(tuesday), DATA);
}

#[test]
fn an_earlier_day_does_not_reopen_the_budget() {
    let mut guard = DailyLimitGuard::new(DATA);
    let net = FakeSend::new();
    assert_eq!(guard.send(DayKey::new(11), &paid(), net.ok()), Ok(()));

    // A clock stepped back a day is counted against the current day, not a fresh one.
    assert!(matches!(
        guard.send(DayKey::new(10), &paid(), net.ok()),
        Err(GuardError::Refused(LimitError::Exceeded { .. }))
    ));
    assert_eq!(net.calls(), 1);
    assert_eq!(guard.spent(DayKey::new(10)), DATA);
}

#[test]
fn an_admitted_earlier_day_does_not_move_the_day_back() {
    // Room for two calls a day. An earlier key that is admitted must not become the current day,
    // or the next call on the real day would start a fresh total.
    let mut guard = DailyLimitGuard::new(DATA.checked_add(DATA).unwrap());
    let net = FakeSend::new();
    assert_eq!(guard.send(DayKey::new(11), &paid(), net.ok()), Ok(()));
    assert_eq!(guard.send(DayKey::new(10), &paid(), net.ok()), Ok(()));
    assert!(matches!(
        guard.send(DayKey::new(11), &paid(), net.ok()),
        Err(GuardError::Refused(LimitError::Exceeded { .. }))
    ));
    assert_eq!(net.calls(), 2);
}

#[test]
fn a_later_day_stays_current_until_a_still_later_one() {
    let mut guard = DailyLimitGuard::new(DATA);
    let net = FakeSend::new();
    assert_eq!(guard.send(DayKey::new(1_000), &paid(), net.ok()), Ok(()));
    // Back on the real day, the calls still count against day 1,000.
    assert!(guard.send(DayKey::new(11), &paid(), net.ok()).is_err());
    assert_eq!(guard.send(DayKey::new(1_001), &paid(), net.ok()), Ok(()));
    assert_eq!(net.calls(), 2);
}

#[test]
fn a_refusal_on_a_new_day_leaves_the_current_day_alone() {
    let mut guard = DailyLimitGuard::new(DATA);
    assert_eq!(guard.admit(DayKey::new(10), &paid()), Ok(()));
    assert_eq!(
        guard.admit_cost(DayKey::new(11), Err(CostError::UnknownPricing)),
        Err(LimitError::Cost(CostError::UnknownPricing))
    );
    assert_eq!(guard.spent(DayKey::new(10)), DATA);
    assert_eq!(guard.admit(DayKey::new(11), &paid()), Ok(()));
}

// ── messages ────────────────────────────────────────────────────────────────────────────────

#[test]
fn messages_name_the_amounts() {
    let e = LimitError::Exceeded {
        limit: Microdollars::new(4_000),
        spent: Microdollars::new(4_000),
        cost: DATA,
    };
    assert_eq!(
        e.to_string(),
        "daily limit reached: $0.004000 spent plus $0.002000 for this call is over $0.004000"
    );
    assert_eq!(
        LimitError::Cost(CostError::UnknownPricing).to_string(),
        "call refused because its cost is unknown: unknown pricing category, cost not assumed to be zero"
    );
}

#[test]
fn error_messages_are_lowercase_without_trailing_punctuation() {
    let errors = [
        LimitError::Exceeded {
            limit: Microdollars::ZERO,
            spent: Microdollars::ZERO,
            cost: DATA,
        },
        LimitError::Cost(CostError::UnknownPricing),
        LimitError::Cost(CostError::RateMismatch),
        LimitError::Cost(CostError::Overflow),
    ];
    for e in errors {
        let m = e.to_string();
        assert!(
            m.chars().next().is_some_and(|c| c.is_ascii_lowercase()),
            "{m}"
        );
        assert!(!m.ends_with(['.', '!', '?']), "{m}");
    }
}

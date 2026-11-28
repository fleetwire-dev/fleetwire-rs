use super::*;

use fleetwire::api::users::GetMe;
use fleetwire::api::vehicles::{
    DataEndpoint, GetNearbyChargingSites, GetVehicle, GetVehicleData, ListVehicles,
};

/// A made-up VIN. The requests are only built, never sent.
const VIN: &str = "00000000000000000";

// ── cost_of_units: one test per category ────────────────────────────────────────────────────

#[test]
fn data_is_exactly_two_thousand_microdollars() {
    assert_eq!(
        cost_of_units(Category::Data, 1),
        Ok(Microdollars::new(2_000))
    );
    assert_eq!(
        cost_of_units(Category::Data, 1).map(|m| m.to_string()),
        Ok("$0.002000".into())
    );
    assert_eq!(
        cost_of_units(Category::Data, 500),
        Ok(Microdollars::new(1_000_000))
    );
}

#[test]
fn command_is_one_thousand_microdollars() {
    assert_eq!(
        cost_of_units(Category::Command, 1),
        Ok(Microdollars::new(1_000))
    );
    // The billing page's example: 1,211 commands is $1.211 before rounding the bill to cents.
    assert_eq!(
        cost_of_units(Category::Command, 1_211),
        Ok(Microdollars::new(1_211_000))
    );
}

#[test]
fn wake_is_twenty_thousand_microdollars() {
    assert_eq!(
        cost_of_units(Category::Wake, 1),
        Ok(Microdollars::new(20_000))
    );
    assert_eq!(
        cost_of_units(Category::Wake, 50),
        Ok(Microdollars::new(1_000_000))
    );
}

#[test]
fn streaming_signals_round_up_to_whole_microdollars() {
    // 1 signal = 6.67 microdollars, rounded up.
    assert_eq!(
        cost_of_units(Category::StreamingSignal, 1),
        Ok(Microdollars::new(7))
    );
    // 3 signals = exactly 20.
    assert_eq!(
        cost_of_units(Category::StreamingSignal, 3),
        Ok(Microdollars::new(20))
    );
}

#[test]
fn streaming_signals_just_below_at_and_above_one_dollar() {
    // 149,999 signals = 999,993.33 -> 999,994; 150,000 = exactly $1; 150,001 = 1,000,006.67 -> 1,000,007.
    assert_eq!(
        cost_of_units(Category::StreamingSignal, 149_999),
        Ok(Microdollars::new(999_994))
    );
    assert_eq!(
        cost_of_units(Category::StreamingSignal, 150_000),
        Ok(Microdollars::new(1_000_000))
    );
    assert_eq!(
        cost_of_units(Category::StreamingSignal, 150_001),
        Ok(Microdollars::new(1_000_007))
    );
}

#[test]
fn zero_units_cost_nothing_in_every_category() {
    for c in Category::ALL {
        assert_eq!(cost_of_units(c, 0), Ok(Microdollars::ZERO), "{c:?}");
    }
}

#[test]
fn overflow_threshold_just_below_and_just_above() {
    // Data costs 2,000 microdollars, so u64::MAX / 2,000 requests is the most that fits.
    let most = u64::MAX / 2_000;
    assert_eq!(
        cost_of_units(Category::Data, most),
        Ok(Microdollars::new(most * 2_000))
    );
    assert_eq!(
        cost_of_units(Category::Data, most + 1),
        Err(CostError::Overflow)
    );
}

#[test]
fn the_largest_unit_count_does_not_panic() {
    for c in Category::ALL {
        // Every rate is under a million units per dollar, so u64::MAX units overflow in each.
        assert_eq!(
            cost_of_units(c, u64::MAX),
            Err(CostError::Overflow),
            "{c:?}"
        );
    }
}

// ── cost_of_pricing: one test per arm ───────────────────────────────────────────────────────

#[test]
fn not_charged_is_zero() {
    assert_eq!(cost_of_pricing(Pricing::NotCharged), Ok(Microdollars::ZERO));
}

#[test]
fn the_standard_data_rate_agrees_with_the_table() {
    // `Pricing::data()` in the fleetwire crate and the published table must name the same price.
    assert_eq!(
        cost_of_pricing(Pricing::data()),
        Ok(Microdollars::new(2_000))
    );
}

#[test]
fn a_data_rate_that_differs_from_the_table_is_refused() {
    for usd in [0.001, 0.0021, 0.0, f64::NAN, -0.002] {
        let p = Pricing::Data {
            usd_per_request: usd,
        };
        assert_eq!(cost_of_pricing(p), Err(CostError::RateMismatch), "{usd}");
    }
}

#[test]
fn data_rate_tolerance_just_inside_and_just_outside() {
    let inside = Pricing::Data {
        usd_per_request: 0.002 + 1e-10,
    };
    let outside = Pricing::Data {
        usd_per_request: 0.002 + 1e-8,
    };
    assert_eq!(cost_of_pricing(inside), Ok(Microdollars::new(2_000)));
    assert_eq!(cost_of_pricing(outside), Err(CostError::RateMismatch));
}

// `Pricing` is `#[non_exhaustive]` and has no third variant today, so the `UnknownPricing` arm
// cannot be reached from a test in another crate. Its message is pinned here; the arm itself is
// the wildcard in `cost_of_pricing`.
#[test]
fn unknown_pricing_says_it_is_not_free() {
    assert_eq!(
        CostError::UnknownPricing.to_string(),
        "unknown pricing category, cost not assumed to be zero"
    );
}

#[test]
fn error_messages_are_lowercase_without_trailing_punctuation() {
    for e in [
        CostError::UnknownPricing,
        CostError::RateMismatch,
        CostError::Overflow,
    ] {
        let m = e.to_string();
        assert!(
            m.chars().next().is_some_and(|c| c.is_ascii_lowercase()),
            "{m}"
        );
        assert!(!m.ends_with(['.', '!', '?']), "{m}");
    }
}

// ── cost_of_request: the request types carry their own category ─────────────────────────────

#[test]
fn the_two_billed_requests_cost_the_data_rate() {
    let data = GetVehicleData::new(VIN, vec![DataEndpoint::ChargeState]).unwrap();
    let chargers = GetNearbyChargingSites::new(VIN, None, None, None).unwrap();
    assert_eq!(cost_of_request(&data), Ok(Microdollars::new(2_000)));
    assert_eq!(cost_of_request(&chargers), Ok(Microdollars::new(2_000)));
}

#[test]
fn free_requests_cost_nothing() {
    let list = ListVehicles {
        page: None,
        per_page: None,
    };
    assert_eq!(cost_of_request(&list), Ok(Microdollars::ZERO));
    assert_eq!(
        cost_of_request(&GetVehicle::new(VIN).unwrap()),
        Ok(Microdollars::ZERO)
    );
    assert_eq!(cost_of_request(&GetMe), Ok(Microdollars::ZERO));
}

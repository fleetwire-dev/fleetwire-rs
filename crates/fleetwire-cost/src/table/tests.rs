use super::*;

#[test]
fn rates_match_the_published_table() {
    assert_eq!(Category::Data.units_per_usd(), 500);
    assert_eq!(Category::Command.units_per_usd(), 1_000);
    assert_eq!(Category::Wake.units_per_usd(), 50);
    assert_eq!(Category::StreamingSignal.units_per_usd(), 150_000);
}

#[test]
fn all_lists_each_category_once() {
    for (i, a) in Category::ALL.iter().enumerate() {
        for b in &Category::ALL[i + 1..] {
            assert_ne!(a, b);
        }
    }
    assert_eq!(Category::ALL.len(), 4);
}

#[test]
fn no_rate_is_zero() {
    // A zero rate would be a division by zero in `cost_of_units`.
    for c in Category::ALL {
        assert!(c.units_per_usd() > 0, "{c:?}");
    }
}

#[test]
fn monthly_discount_is_ten_dollars() {
    assert_eq!(MONTHLY_DISCOUNT, Microdollars::new(10_000_000));
}

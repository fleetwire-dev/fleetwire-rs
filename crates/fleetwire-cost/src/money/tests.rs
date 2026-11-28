use super::*;

#[test]
fn display_shows_six_decimal_places() {
    assert_eq!(Microdollars::new(2_000).to_string(), "$0.002000");
    assert_eq!(Microdollars::ZERO.to_string(), "$0.000000");
    assert_eq!(Microdollars::new(10_000_000).to_string(), "$10.000000");
    assert_eq!(Microdollars::new(1_000_001).to_string(), "$1.000001");
}

#[test]
fn checked_add_at_the_top_of_the_range() {
    let top = Microdollars::new(u64::MAX - 1);
    assert_eq!(
        top.checked_add(Microdollars::new(1)),
        Some(Microdollars::new(u64::MAX))
    );
    assert_eq!(top.checked_add(Microdollars::new(2)), None);
}

#[test]
fn get_returns_what_new_was_given() {
    assert_eq!(Microdollars::new(7).get(), 7);
    assert_eq!(Microdollars::default(), Microdollars::ZERO);
}

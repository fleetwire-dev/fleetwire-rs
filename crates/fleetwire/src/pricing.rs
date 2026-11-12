//! Per-endpoint Data/Command pricing metadata (billed vs. free GET calls).

/// Per-endpoint usage-based pricing category (Tesla OpenAPI `pricing_category`).
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum Pricing {
    /// `pricing_category: null` — endpoint is not charged (14 of 16 GETs).
    NotCharged,
    /// Paid `device_data` request. `vehicle_data` and `nearby_charging_sites` only.
    Data {
        /// USD per request (currently 0.002).
        usd_per_request: f64,
    },
}

impl Pricing {
    /// The standard paid Data rate ($0.002/request).
    pub fn data() -> Pricing {
        Pricing::Data {
            usd_per_request: 0.002,
        }
    }
}

/// Whether a request outcome incurred a charge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Billing {
    /// A charge was incurred (status < 500 on a paid endpoint).
    Billed,
    /// No charge (all NotCharged endpoints; ≥500; confirmed not-sent).
    NotBilled,
    /// Charge status unknown (paid endpoint, server processing unclear).
    Unknown,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn data_is_two_tenths_cent() {
        match Pricing::data() {
            Pricing::Data { usd_per_request } => assert!((usd_per_request - 0.002).abs() < 1e-9),
            _ => panic!(),
        }
    }
    #[test]
    fn not_charged_distinct() {
        assert_ne!(Pricing::NotCharged, Pricing::data());
    }
    #[test]
    fn data_is_stable_across_calls() {
        assert_eq!(Pricing::data(), Pricing::data());
    }
    #[test]
    fn billing_variants_are_pairwise_distinct() {
        assert_ne!(Billing::Billed, Billing::NotBilled);
        assert_ne!(Billing::Billed, Billing::Unknown);
        assert_ne!(Billing::NotBilled, Billing::Unknown);
    }
}

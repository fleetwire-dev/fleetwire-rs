//! A region URL is where the bearer token goes next. Whatever the input, an accepted URL must be
//! https, on the default port, without userinfo, and on a host from the default allow-list.
//! A `Retry-After` value comes from the same untrusted place and must parse without panicking.
#![no_main]

use std::sync::Once;

use libfuzzer_sys::fuzz_target;

/// One host from the default allow-list. Random bytes almost never spell a valid URL, so the
/// input is also tried glued onto this one: that is where the interesting near-misses are
/// (`@other.host`, `:8443`, `.other.host`, a path).
const KNOWN: &str = "https://fleet-api.prd.na.vn.cloud.tesla.com";

static POSITIVE_CONTROL: Once = Once::new();

fn check(candidate: &str) {
    let Some(url) = fleetwire::fuzz::accepted_region_url(candidate) else {
        return;
    };
    assert_eq!(url.scheme(), "https");
    assert!(url.port().is_none(), "accepted a non-default port");
    assert!(
        url.username().is_empty() && url.password().is_none(),
        "accepted userinfo"
    );
    let host = url.host_str().unwrap_or_default();
    assert!(
        fleetwire::TESLA_REGION_HOSTS.contains(&host),
        "accepted a host off the allow-list: {host}"
    );
}

fuzz_target!(|data: &[u8]| {
    // A check that accepted nothing would pass every assertion below. See it accept once.
    POSITIVE_CONTROL.call_once(|| {
        assert!(
            fleetwire::fuzz::accepted_region_url(KNOWN).is_some(),
            "the known-good region URL was refused"
        );
    });
    let Ok(raw) = std::str::from_utf8(data) else {
        return;
    };
    let _ = fleetwire::fuzz::retry_after(raw);
    check(raw);
    check(&format!("{KNOWN}{raw}"));
    check(&format!("https://{raw}fleet-api.prd.na.vn.cloud.tesla.com"));
});

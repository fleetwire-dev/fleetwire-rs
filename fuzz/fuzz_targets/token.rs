//! Reading the scope claim of an arbitrary token must not panic. The check is advisory, so there
//! is nothing to assert about the verdict: only that every input gets one.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(token) = std::str::from_utf8(data) {
        fleetwire::fuzz::observe_token(token);
    }
});

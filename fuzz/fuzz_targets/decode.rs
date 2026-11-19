//! Decoding a response must not panic for any status, content type or body, and whatever error it
//! builds must print without panicking.
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // First three bytes pick the status, the content type and whether the endpoint is billed.
    let [a, b, c, body @ ..] = data else {
        return;
    };
    let status = 100 + (u16::from(*a) << 8 | u16::from(*b)) % 500;
    let content_type = match c % 4 {
        0 => Some("application/json"),
        1 => Some("text/html"),
        2 => Some("application/json; charset=utf-8"),
        _ => None,
    };
    fleetwire::fuzz::decode(status, content_type, body, c & 0x80 != 0);
});

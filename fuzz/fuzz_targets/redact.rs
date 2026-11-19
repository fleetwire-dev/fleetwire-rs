//! The redaction layer must not panic on any text, must keep a preview within its byte bound, and
//! must not let a planted secret through whatever surrounds it.
#![no_main]

use libfuzzer_sys::fuzz_target;

/// The three parts of a made-up, unsigned token in the shape the redaction patterns look for.
/// They are joined at run time so that no token-shaped literal sits in the source.
const PLANTED_PARTS: [&str; 3] = [
    "eyJhbGciOiJub25lIn0",
    "eyJzdWIiOiJmdXp6LXBsYW50ZWQifQ",
    "c2lnbmF0dXJlLW5vdC1yZWFs",
];
const SHORT: usize = 512;
/// Large enough that truncation never hides the planted token.
const LONG: usize = 1 << 20;

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };
    // Any text: no panic, and the preview respects its bound.
    let preview = fleetwire::fuzz::body_preview(text, SHORT);
    assert!(preview.len() <= SHORT, "preview is {} bytes", preview.len());

    // The same text with a bearer token planted in the middle: the token must not survive.
    let cut = (0..=text.len() / 2)
        .rev()
        .find(|i| text.is_char_boundary(*i))
        .unwrap_or(0);
    let (head, tail) = text.split_at(cut);
    let token = PLANTED_PARTS.join(".");
    let planted = format!("{head} Bearer {token} {tail}");
    let scrubbed = fleetwire::fuzz::body_preview(&planted, LONG);
    assert!(
        !scrubbed.contains(&token),
        "a planted bearer token survived redaction"
    );
});

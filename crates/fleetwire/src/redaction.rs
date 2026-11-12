//! Redaction of sensitive fields (secrets, VIN, location) from error
//! messages, body previews and log output.
//!
//! **On `Debug` (final fix wave / F10; updated by A2 Task 3).** REQ-18 also
//! governs `Debug` output. [`crate::model::VehicleSummary`] — the one type
//! holding a VIN and coordinates — deliberately does **not** implement `Debug`
//! at all (see its doc comment), and [`crate::error::Error`]'s derived `Debug`
//! prints only fields that were already routed through [`body_preview`] on the
//! way in.
//!
//! This module has exactly **two** `Debug` call sites, and both hold an access
//! token *and* a refresh token, so both hand-write `Debug` (a
//! `#[derive(Debug)]` on either would print both tokens in full) and both take
//! the marker from [`REDACTED`] rather than spelling `"[REDACTED]"` again — one
//! source, no drift. Both also destructure `self` in `fmt`, so adding a field
//! is a compile error rather than a silent omission:
//!
//! - [`crate::auth::store::TokenSet`] — the on-disk token set (A2 Task 3).
//! - [`crate::auth::Refreshed`] — the refresh response (A2 Task 7; hand-written
//!   in the A2 final fix wave / C1, where it was found still deriving `Debug`).
//!   This one is re-exported at the crate root and is produced by the transport
//!   layer's refresh parser, so a single `tracing::debug!(?refreshed)` there
//!   would have written a live refresh token to the log.
//!
//! If a future contributor adds a `Debug` impl to any type carrying a VIN,
//! coordinates or a token, it MUST be hand-written and route the sensitive
//! fields through this module — a `#[derive(Debug)]` there is a REQ-18
//! violation, not a formatting convenience. Follow `TokenSet`'s shape:
//! destructure, and use [`REDACTED`] for the marker.
//!
//! This is the crate's **single** redaction layer (REQ-18): the one place
//! secrets, VINs and coordinates get scrubbed before anything reaches a log,
//! an error message, or a body preview. Two complementary strategies, plus
//! an entry point that combines them:
//!
//! - [`redact_json`] — structured, key-allowlist redaction over a parsed
//!   `serde_json::Value`. This is the **primary** mechanism for JSON
//!   bodies: an allowlisted key's value is replaced outright, so a
//!   partial/malformed token shape can't slip past a regex miss. It also
//!   runs [`redact_text`] over every `Value::String` leaf that isn't
//!   already caught by the key allowlist (Fix round 1 / C1) — a JSON body
//!   is not "already serialized text" that `redact_text` alone can scrub,
//!   because `"key":"value"` doesn't match the `key=value` text pattern at
//!   all.
//! - [`redact_text`] — regex-based redaction over opaque strings (already
//!   serialized bodies, log lines, error messages, individual JSON string
//!   leaves). This is **auxiliary only** to the key allowlist. Coordinates
//!   are redacted by two independent patterns (Fix round 2 / C1): a
//!   key-anchored single-value pattern for the JSON shape (each of
//!   `latitude`/`longitude`/`native_latitude`/`native_longitude` is caught
//!   on its own, so how many sibling keys separate a pair, or whether they
//!   even appear as a pair at all, doesn't matter), and a bare `lat,lon`
//!   pattern for plain-text log lines. See `text_patterns()` for why
//!   pair-matching was replaced. The **navigation destination address**
//!   (`active_route_destination`) is redacted the same key-anchored way
//!   (Fix round 4 / finding i) — it is free text naming, in plain English,
//!   the very place `active_route_latitude` points at, so redacting the
//!   coordinate while leaving the address is no redaction at all.
//!
//! [`body_preview`] is the module's **only** `pub(crate)` entry point (final
//! fix wave / F1). [`redact_json`] and [`redact_text`] are private to this
//! module: each is individually weak — `redact_json` never touches a
//! `Value::Number` (so `{"latitude":37.5665}` passes through it untouched)
//! and cannot see the key beside a `destination` string, while `redact_text`
//! alone never matches JSON's `"key":"value"` syntax — and the contract that
//! only their *composition* is safe was previously enforced by author
//! discipline alone. It is now enforced by the language.
//!
//! [`body_preview`] is the entry point every caller reaches for: if
//! `raw` parses as JSON, it decodes → [`redact_json`] (which regex-scrubs
//! every string leaf as it goes) → re-serializes via `serde_json::to_string`
//! → runs [`redact_text`] once more over the *whole* serialized text →
//! truncates. For input that isn't valid JSON, it falls back to running
//! [`redact_text`] directly on `raw`. Either way, the result is what
//! `error::Error::Api`/`Error::Decode`'s `body_preview` field carries.
//!
//! **Re-serialization side effects (Fix round 2 / finding b).** This crate
//! depends on plain `serde_json` (no `preserve_order` feature), so
//! `serde_json::Map` is a `BTreeMap`: the round-trip through
//! `serde_json::from_str` → `redact_json` → `serde_json::to_string`
//! reorders object keys **alphabetically** (not wire order), **collapses
//! duplicate keys** (last one wins), and round-trips **floating-point**
//! numbers through `f64` (final fix wave / F13a: the earlier wording said
//! "numbers", which is wrong for integers — `serde_json::Number` keeps an
//! integer's exact `u64`/`i64` value and its decimal spelling, which is why
//! `"timestamp":1700000000123` survives byte-identically in
//! `body_preview_redacts_full_drive_state_shape`; only floats go through
//! `f64` and can therefore come back respelled, e.g. `1.2E+2` → `120.0`).
//! None of this is a redaction correctness issue — the key-anchored
//! coordinate pattern above does not depend on key order or adjacency, by
//! design — but a caller diffing a preview against the raw body should not
//! expect byte-for-byte structural fidelity.
//!
//! **The preview is not guaranteed to be valid JSON (Fix round 2 / finding
//! d).** A redaction match can span quotes, colons, or brace-adjacent
//! characters; Task 9 consumers must treat `body_preview`'s output as an
//! opaque debug string, never as JSON to re-parse.
//!
//! **Scope boundary: unlabeled decimals are never redacted.** A bare
//! decimal number with no coordinate-shaped key next to it (e.g.
//! `body_preview("37.5665", …)`, or a raw JSON number under an unrelated
//! key like `outside_temp`/`battery_range`) is indistinguishable from any
//! other float field and is intentionally left alone — redacting every
//! decimal in a body would destroy the preview's debugging value. What
//! *does* trigger coordinate redaction (Fix round 3, correcting an
//! inaccurate claim in this same sentence from Fix round 2): any JSON key
//! whose text *ends* in `latitude`/`longitude`/`long`/`lat`/`lon` —
//! regardless of prefix, so `latitude`, `native_latitude`, and
//! `active_route_latitude` all match identically, because Tesla adds new
//! prefixed variants (`active_route_*`) without notice and a hand-enumerated
//! key list silently falls behind — with an underscore required before
//! each prefix segment (so `"flat"`/`"salon"` do NOT match); the same
//! suffix-anchored word in unquoted `key=value` form for plain-text log
//! lines (`lat=37.5665`); or a bare comma-adjacent decimal pair in plain
//! text (`37.5665,126.9780`). Fix round 4 (finding iii) widened the
//! key-anchored patterns' integer part from `\d{1,3}` to `\d+`: because the
//! key already establishes that the number *is* a coordinate, truncating
//! the digit run only produced a half-scrubbed number sitting next to a
//! `[COORD]` marker (`{"e7_lat":375665000}` → `[COORD]665000`), which reads
//! as redacted while leaking. The unanchored `coord_pair` keeps `\d{1,3}`,
//! since there the digit count is the only thing making it coordinate-shaped.
//!
//! **Free-text location values (Fix round 4 / finding i).** The same
//! key-anchored, suffix-matching mechanism redacts the *value* of any key
//! whose text ends in `destination` (`"active_route_destination":"1234 Elm
//! St, Palo Alto, CA 94301"` → `"active_route_destination":"[REDACTED]"`).
//! Suffix-anchored for exactly the reason the coordinate keys are: Tesla
//! adds prefixed variants without notice and an enumerated key list falls
//! behind silently. This can only work on `body_preview`'s whole-text pass
//! over the serialized JSON — the `Value::String` leaf pass sees the address
//! with no surrounding key and has no way to tell it from any other free
//! text. The value body is escape-aware (`(?:[^"\\]|\\[\s\S])*`) so an
//! embedded `\"` doesn't end the match early.
//!
//! [`redact_transport_error`] (A3 Task 2): REQ-18 also requires scrubbing a
//! `reqwest::Error`'s `Display` and its `source()` chain, since `reqwest`
//! puts the request URL (which may carry a VIN as a path segment) straight
//! into `Display`. It lives in **this** module, not in `transport.rs` —
//! [`body_preview`] must stay the sole safe entry point for any
//! user-visible error string, and a redaction helper defined outside this
//! module would bypass that contract (this exact mistake was made once
//! already, in subproject A2, and caught in final review).

use std::sync::OnceLock;

use regex::Regex;
use serde_json::Value;

/// JSON object keys whose values are always replaced wholesale by
/// [`redact_json`]. Matched **after normalization** — see [`normalize_key`]
/// — so a single logical key covers every spelling Tesla, a proxy or a
/// gateway might emit for it. Deliberately narrow, exactly
/// these five (Fix round 1 / controller amendment: do not extend this to
/// cover `id_token`/`code`/etc — those are caught by the `redact_text`
/// string-leaf pass below instead, via the JWT/bearer/kv patterns). `vin`
/// is NOT here either: VINs are legitimate data this crate returns to
/// callers through the typed data path (`VehicleSummary` etc, never a raw
/// `Value`); the wholesale key-replacement mechanism stays reserved for
/// secrets. A `vin` *value* still gets regex-scrubbed by the string-leaf
/// pass like any other string, though — see
/// `redact_json_vin_key_not_wholesale_replaced_but_value_regex_scrubbed`.
const JSON_KEY_ALLOWLIST: &[&str] = &[
    "access_token",
    "refresh_token",
    "client_secret",
    "authorization",
    "token",
];

/// Fold a JSON object key to its comparison form: ASCII-lowercased, with
/// `-`, `_` and whitespace removed.
///
/// Final fix wave (F2). The previous comparison was
/// `k.eq_ignore_ascii_case(allowed)`, which tolerates case and nothing else,
/// so the byte-exact allowlist entry `access_token` missed `Access-Token`,
/// `accessToken`, `AccessToken` and `ACCESS-TOKEN` — and none of those
/// values is caught by the `redact_text` families either (a raw token is not
/// a JWT, carries no `Bearer ` prefix and has no `=`). The realistic trigger
/// is a proxy or gateway 4xx body echoing the request headers back, which is
/// exactly the body most likely to be handed to [`body_preview`].
///
/// Normalizing both sides is deliberately preferred over enumerating the
/// spellings: this branch has now been bitten twice by an enumerated list
/// silently falling behind the shapes it had to cover (the coordinate key
/// prefixes in fix rounds 2 and 3), so the allowlist stays at its five
/// logical keys and the *matcher* absorbs the variation instead.
fn normalize_key(k: &str) -> String {
    k.chars()
        .filter(|c| *c != '-' && *c != '_' && !c.is_whitespace())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// Whether `k` is one of [`JSON_KEY_ALLOWLIST`]'s five logical keys, under
/// [`normalize_key`] folding on both sides.
fn is_allowlisted_key(k: &str) -> bool {
    let normalized = normalize_key(k);
    JSON_KEY_ALLOWLIST
        .iter()
        .any(|allowed| normalize_key(allowed) == normalized)
}

/// The crate-wide redaction marker.
///
/// `pub(crate)` since A2 Task 3 (fix round 1): [`crate::auth::store::TokenSet`]'s
/// hand-written `Debug` needs the *marker* without needing an entry point —
/// [`body_preview`] is a body-preview tool, not a field marker. Exporting the
/// const is what keeps "single redaction layer" literally true; a second
/// hardcoded `"[REDACTED]"` elsewhere would be a second source that can drift.
pub(crate) const REDACTED: &str = "[REDACTED]";

/// `[REDACTED]` minus its closing bracket — the prefix an idempotency check
/// must test for (Fix round 4 / finding ii).
///
/// `]` is excluded from the `kv_secret` value character class on purpose (so
/// a match stops at a JSON array terminator instead of running past it), so
/// a *second* pass over an already-redacted `code=[REDACTED]` captures
/// `[REDACTED` — the bracket is left behind and the naive replacement
/// re-appends one, yielding `code=[REDACTED]]`. `body_preview` always runs
/// `redact_text` twice over JSON (once per string leaf, once over the whole
/// re-serialized document), so this fires on any real body carrying an
/// `access_token=`/`code=` inside a string value. Checking for the full
/// `[REDACTED]` would never match the truncated capture; check for this.
const REDACTED_OPEN: &str = "[REDACTED";

/// Recursively redact a JSON value: allowlisted keys wholesale, every other
/// string leaf via the [`redact_text`] regex layer.
///
/// **Private to this module** (final fix wave / F1): on its own this
/// function is not a safe redaction entry point — it never scrubs a
/// `Value::Number` (`{"latitude":37.5665}` survives it verbatim) and it
/// cannot see the key beside a `destination` string, both of which are only
/// closed by [`body_preview`]'s whole-text second pass. Call
/// [`body_preview`].
///
/// Structured, key-based redaction is REQ-18's primary mechanism for JSON:
/// walks objects and arrays, and for any object key that matches
/// [`JSON_KEY_ALLOWLIST`] under [`normalize_key`] folding, replaces the value with `"[REDACTED]"`
/// regardless of its shape (string, number, nested object, ...). Recurses
/// into every array element and every non-redacted object value, so a
/// secret nested arbitrarily deep (e.g. inside a `Vec` of objects) is still
/// caught.
///
/// Fix round 1 (C1): a non-allowlisted `Value::String` leaf is no longer
/// cloned untouched — it is passed through [`redact_text`], so secrets that
/// don't live under one of the five allowlisted keys (e.g. Tesla's OAuth
/// `id_token`, which is itself a JWT) still get scrubbed by the auxiliary
/// regex layer instead of surviving verbatim in JSON output.
///
/// Stack safety (A1 Task 9 fix round 1, Minor 5): this recursion has no
/// depth guard of its own. It is safe against attacker-controlled input
/// only because every caller reaches it exclusively via [`body_preview`],
/// which parses raw bytes through `serde_json::from_str`/`from_slice` —
/// `serde_json`'s default 128-level recursion limit — before this function
/// ever sees the `Value` tree. If a future caller (e.g. an A3 transport
/// layer) builds a `Deserializer` with `disable_recursion_limit()` and feeds
/// its output here, that guarantee no longer holds and a maliciously deep
/// document becomes a stack overflow with nothing in this crate to catch it.
fn redact_json(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, val) in map {
                if is_allowlisted_key(k) {
                    out.insert(k.clone(), Value::String(REDACTED.to_string()));
                } else {
                    out.insert(k.clone(), redact_json(val));
                }
            }
            Value::Object(out)
        }
        Value::Array(items) => Value::Array(items.iter().map(redact_json).collect()),
        Value::String(s) => Value::String(redact_text(s)),
        other => other.clone(),
    }
}

/// Compiled-regex cache for [`redact_text`]. `OnceLock` caches these
/// constant, compile-once regexes only — it holds no tokens or other
/// runtime secrets, so REQ-15's "no `OnceLock` for tokens" rule does not
/// apply here.
struct TextPatterns {
    bearer: Regex,
    jwt: Regex,
    kv_secret: Regex,
    destination_value: Regex,
    account_id: Regex,
    vin: Regex,
    coord_keyed: Regex,
    coord_kv: Regex,
    coord_pair: Regex,
}

fn text_patterns() -> &'static TextPatterns {
    static PATTERNS: OnceLock<TextPatterns> = OnceLock::new();
    PATTERNS.get_or_init(|| TextPatterns {
        // "Bearer <token>" — the HTTP Authorization scheme. `(?i)`: RFC
        // 7235 auth schemes are case-insensitive (Fix round 1 / I1). The
        // value is bounded to `[^\s",}\]]+` rather than `\S+` (Fix round 2
        // / finding c): over compact, whitespace-free re-serialized JSON,
        // a bare `\S+` runs from "Bearer " straight through the closing
        // quote and every following key/value to end-of-document — the
        // excluded characters (`"`, `,`, `}`, `]`) are exactly the ones
        // that terminate a JSON string/object/array, so the match now
        // stops there instead of consuming the rest of the preview.
        bearer: Regex::new(r#"(?i)Bearer\s+[^\s",}\]]+"#).expect("static regex"),
        // A bare JWT (header.payload.signature), independent of "Bearer ".
        // Base64url alphabet is already case-mixed by construction, so no
        // `(?i)` needed here.
        jwt: Regex::new(r"eyJ[\w-]+\.[\w-]+\.[\w-]+").expect("static regex"),
        // key=value secrets; the key is kept (see redact_text docs) so a
        // preview that loses this still shows which field was redacted.
        // `(?i)`: real payloads mix casing (`Access_Token`, `ACCESS_TOKEN`)
        // (Fix round 1 / I1). `code` added per controller decision closing
        // I2's residual (the OAuth authorization `code` query param is a
        // bearer-equivalent secret); given a leading `\b` rather than the
        // bare word, so `error_code=`/`status_code=` do NOT trip it — `_`
        // is a word character, so `\b` correctly refuses to match directly
        // after it (Fix round 2). Value bounded to `[^\s",}\]&]+` rather
        // than `\S+` (Fix round 3 / finding 2): adding `code` re-opened the
        // same over-consumption bug already fixed on `bearer` in round 2 —
        // over a compact re-serialized JSON body containing e.g. an OAuth
        // redirect URL, unbounded `\S+` ran straight through the closing
        // quote into every following key. `&` is excluded too, so a
        // sibling query-string parameter (`...code=X&state=Y`) survives.
        // The value is captured as group 2 (Fix round 4 / finding ii) so
        // `redact_text`'s closure can spot an already-redacted value and
        // leave it alone — see `REDACTED_OPEN`.
        kv_secret: Regex::new(
            r#"(?i)(refresh_token|client_secret|access_token|\bcode)=([^\s",}\]&]+)"#,
        )
        .expect("static regex"),
        // Fix round 4 (finding i): the navigation destination is a free-text
        // street address — `"active_route_destination":"1234 Elm St, Palo
        // Alto, CA 94301"`. Redacting `active_route_latitude` while leaving
        // this in plain English is not redaction. Suffix-anchored on any key
        // ending in `destination` for the same reason `coord_keyed` is
        // suffix-anchored: enumerating the literal `active_route_destination`
        // is exactly the mistake that let `active_route_*` leak through two
        // earlier rounds. Group 1 keeps the key and the value's OPENING
        // quote; the value body is escape-aware — `"[^"]*"` would stop at the
        // first `\"` inside the address and leave the tail unredacted, so
        // `(?:[^"\\]|\\[\s\S])*` consumes escape pairs whole. Only matches a
        // quoted string value, so `"active_route_destination":null` (what
        // Tesla sends with no active route) passes through untouched.
        // Idempotent: re-running it over `..."[REDACTED]"` reproduces the
        // same text.
        destination_value: Regex::new(
            r#"(?i)("(?:[a-z0-9]+_)*destination"\s*:\s*")(?:[^"\\]|\\[\s\S])*""#,
        )
        .expect("static regex"),
        // A3 Task 7 (design §8): the **account UUID** Tesla puts in a 412
        // body. Measured 2026-08-01: a request sent to the wrong region is
        // answered with `Account 801802ed-…-f69fc59b2ef0 must be registered
        // in the current region`, i.e. the account identifier in the clear.
        // OSS users paste error messages into GitHub issues verbatim.
        //
        // ⚠️ Deliberately **not** a global UUID substitution. A bare
        // `[0-9a-f]{8}-…` pattern would also erase `x-request-id` and every
        // other diagnostic correlation id, which is what an operator needs
        // to hand Tesla support. Anchored on the literal `Account ` context
        // instead — what survives is as much a part of this pattern's
        // contract as what is removed (see the unit test, which asserts
        // both halves). `\s+` rather than a single space so a line-wrapped
        // or re-spaced body is still caught.
        account_id: Regex::new(
            r"(?i)\bAccount\s+[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b",
        )
        .expect("static regex"),
        // Tesla VIN: 17 chars, ISO 3779 excludes I/O/Q. `(?i)` so a
        // lowercased VIN in a log line still gets caught (Fix round 1 /
        // I1).
        vin: Regex::new(r"(?i)\b[A-HJ-NPR-Z0-9]{17}\b").expect("static regex"),
        // Fix round 2 (C1, controller design call): coordinate PAIR
        // matching was structurally fragile — it depended on adjacency, on
        // exactly how many sibling keys intervened, and (silently) on
        // serde_json's alphabetical key ordering. Replaced with a
        // key-anchored pattern that redacts ONE coordinate value at a time:
        // capture group 1 is the quoted key + colon (kept, so the field
        // name survives in the preview — this also fixes finding (a),
        // where the old pair pattern swallowed an intervening key name
        // whole), the number after it is replaced with `[COORD]`. Matching
        // each field independently means no partner and no intervening key
        // can matter, and there is no dependency on key order — so this
        // does not care whether `preserve_order` is ever enabled.
        //
        // Fix round 3 (C1, Critical — the round-2 pattern was still an
        // enumerated key list: `(?:native_)?(?:lat|...)`. Tesla also emits
        // `active_route_latitude`/`active_route_longitude` (the active
        // navigation *destination* — typically the owner's home address,
        // the single most sensitive coordinate in the payload) whenever
        // navigation is running, and `(?:native_)?` doesn't match
        // `active_route_`. A hand-maintained prefix list will always fall
        // behind Tesla's API and the miss is silent, so this is now
        // suffix-anchored instead: `(?:[a-z0-9]+_)*` accepts *any* number
        // of underscore-terminated prefix segments before the coordinate
        // word. Two properties are load-bearing and must both be kept: the
        // closing `"` bounds the key so `lat` can't match a prefix of
        // `latitude`, and requiring each prefix segment to end in `_` means
        // `"flat"`/`"salon"` do NOT match while `"active_route_latitude"`
        // does. Also (Fix round 3, finding 3a/3b): the number may be
        // wrapped in an optional quote (`"latitude":"37.5665"`, a
        // string-serialized coordinate) and the fractional part is now
        // optional (`"latitude":37`, an integer-valued coordinate) — safe
        // to broaden in both directions specifically because the match is
        // already key-anchored, so over-matching here cannot leak an
        // unrelated field.
        //
        // Fix round 4 (finding iii): the integer part is `\d+`, not
        // `\d{1,3}`. The 3-digit cap was harmless only while `\.\d+` was
        // mandatory — an out-of-degree-range value then simply failed to
        // match and stayed visibly unredacted. Once round 3 made the
        // fraction optional, the cap started truncating the digit run and
        // leaving the remainder next to the marker:
        // `{"latitude":1234.5678}` → `{"latitude":[COORD]4.5678}` and
        // `{"e7_lat":375665000}` → `{"e7_lat":[COORD]665000}` — a partial
        // number that reads as scrubbed. The key already establishes the
        // number is a coordinate, so consume all of it.
        //
        // Final fix wave (F6): an optional exponent suffix
        // `(?:[eE][+-]?\d+)?` closes the same class of half-scrubbed leak as
        // round 4's `\d+` widening. `serde_json` re-serializes a small float
        // in exponent form (`{"latitude":1e-7}` round-trips as `1e-7`, not
        // `0.0000001`), and without this the match stopped at the mantissa:
        // `{"latitude":[COORD]e-7}` — a value that reads as scrubbed while
        // the exponent, i.e. the magnitude, survives. Deliberately NOT added
        // to the unanchored `coord_pair`: there the digit shape is the only
        // evidence the number is a coordinate at all, and accepting
        // exponents would widen it onto arbitrary float pairs.
        coord_keyed: Regex::new(
            r#"(?i)("(?:[a-z0-9]+_)*(?:latitude|longitude|long|lat|lon)"\s*:\s*)"?-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?"?"#,
        )
        .expect("static regex"),
        // Fix round 3, finding 3c: the same suffix-anchored coordinate
        // words in *unquoted* `key=value` form, for plain-text log lines
        // (`lat=37.5665`, `active_route_longitude=-122.143`) — the
        // `coord_keyed` pattern above requires quotes and thus never fires
        // outside JSON. Leading `\b` plays the same role the closing `"`
        // plays in `coord_keyed`: it blocks a match starting mid-word
        // (`"flat="`/`"salon="` still don't match). Integer part widened to
        // `\d+` for the same reason as `coord_keyed` (Fix round 4 / iii),
        // and the optional exponent suffix for the same reason as
        // `coord_keyed` (final fix wave / F6) — `lat=1e-7` left `lat=[COORD]e-7`.
        coord_kv: Regex::new(
            r"(?i)(\b(?:[a-z0-9]+_)*(?:latitude|longitude|long|lat|lon)=)-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?",
        )
        .expect("static regex"),
        // Bare "lat,lon" pair for plain-text log lines (not JSON), e.g.
        // "... at 37.5665,126.9780". `\d{3,}` was tightened to `\d+` in Fix
        // round 1 so low-precision coordinates aren't missed. The integer
        // part deliberately stays `\d{1,3}` (unlike the two key-anchored
        // patterns widened in Fix round 4): with no key to identify it, the
        // degree-range digit count is the only thing that makes this
        // coordinate-shaped at all. Deliberately
        // does NOT match an unlabeled single decimal (see the module-level
        // "Scope boundary" note) — it requires two decimals separated only
        // by a comma and optional whitespace. Known pre-existing gap
        // (flagged Fix round 3, explicitly deferred): this can over-reach
        // on a JSON array of two unrelated floats (`[37.5,126.9]`).
        coord_pair: Regex::new(r"-?\d{1,3}\.\d+,\s*-?\d{1,3}\.\d+").expect("static regex"),
    })
}

/// Redact secrets, VINs and coordinates out of an opaque string.
///
/// **Private to this module** (final fix wave / F1): the regex layer alone
/// is explicitly documented below as *auxiliary*, and never matches JSON's
/// `"key":"value"` syntax, so it is not a safe redaction entry point on its
/// own. Call [`body_preview`].
///
/// Auxiliary to [`redact_json`]'s key allowlist (REQ-18): this is the regex
/// layer, applied both to text that isn't a parsed JSON `Value` at all
/// (log lines, error messages, already serialized bodies) and, via
/// [`redact_json`], to individual JSON string leaves, and to the whole
/// re-serialized document in [`body_preview`]. Order matters: the
/// secret/token patterns (`Bearer ...`, bare JWTs, `key=value` secrets) are
/// applied *before* the VIN pattern, so a 17-character run inside a token
/// can never be partially consumed by the VIN regex first. Coordinates are
/// redacted last, key-anchored patterns before the bare-pair pattern —
/// see `text_patterns()`. The `key=value` patterns keep the key and
/// replace only the value (`access_token=[REDACTED]`), so a preview stays
/// debuggable instead of collapsing into a bare `[REDACTED]` with no field
/// name; both coordinate key-anchor patterns (`coord_keyed` for JSON,
/// `coord_kv` for unquoted log-line `key=value`) and the
/// `destination_value` pattern keep their key for the same reason.
///
/// **Idempotent** (Fix round 4 / finding ii): `redact_text(redact_text(x))
/// == redact_text(x)` for every pattern. This is a hard requirement, not a
/// nicety — `body_preview` runs this function *twice* over any JSON body
/// (once per `Value::String` leaf inside [`redact_json`], then once over the
/// whole re-serialized document), so a pattern that re-matches its own
/// output corrupts every real preview. See `REDACTED_OPEN`.
fn redact_text(s: &str) -> String {
    let p = text_patterns();
    let s = p.bearer.replace_all(s, REDACTED);
    let s = p.jwt.replace_all(&s, REDACTED);
    // Fix round 4 (finding ii): a closure rather than a `$1=[REDACTED]`
    // template, so a second application over an already-redacted value is a
    // no-op instead of appending another bracket. The `regex` crate has no
    // look-around, so a negative lookahead isn't available — inspecting the
    // capture is the way to express "unless it's already redacted".
    //
    // Final fix wave (F5): the guard is `==`, not `starts_with`. Round 4's
    // `caps[2].starts_with(REDACTED_OPEN)` turned an idempotency guard into
    // a scrubber bypass — any value literally *beginning* `[REDACTED`
    // suppressed its own redaction, so
    // `{"note":"code=[REDACTEDreal_secret_here"}` came back verbatim, and
    // the shape is attacker-choosable (it's a response body). The second
    // pass over an already-redacted `code=[REDACTED]` captures exactly
    // `[REDACTED` and nothing more, by construction — `]` is excluded from
    // the value character class — so equality is the precise condition and
    // preserves idempotency with no bypass surface.
    let s = p
        .kv_secret
        .replace_all(&s, |caps: &regex::Captures<'_>| {
            if &caps[2] == REDACTED_OPEN {
                caps[0].to_string()
            } else {
                format!("{}={REDACTED}", &caps[1])
            }
        })
        .into_owned();
    let s = p.destination_value.replace_all(&s, "${1}[REDACTED]\"");
    let s = redact_account_id(&s);
    let s = p.vin.replace_all(&s, "[VIN]");
    let s = p.coord_keyed.replace_all(&s, "${1}[COORD]");
    let s = p.coord_kv.replace_all(&s, "${1}[COORD]");
    let s = p.coord_pair.replace_all(&s, "[COORD]");
    s.into_owned()
}

/// Masks the **account UUID** Tesla ships in a response body.
///
/// Evidence (measured 2026-08-01): a request sent to the wrong region is
/// answered with a 412 whose body reads
/// `Account 801802ed-…-f69fc59b2ef0 must be registered in the current region`
/// — the account identifier in the clear. OSS users paste error messages
/// into GitHub issues verbatim (design §8), so it is stripped here.
///
/// ⚠️ **UUIDs are not substituted globally.** That would also erase
/// diagnostic identifiers such as `x-request-id`, which is exactly what an
/// operator needs to hand Tesla support. The match is anchored on the
/// `Account <uuid>` context; what is *kept* is as much this function's
/// contract as what is removed.
///
/// **Private, and reached only through [`redact_text`]'s pipeline** — never
/// exposed as a second entry point. A1's type contract is that
/// [`body_preview`] is the one safe entry point for untrusted text; A2 broke
/// that contract once by adding a side door, and this is deliberately not a
/// second occurrence. It is called directly only by its own unit test, which
/// has to assert on this pattern's two-sided contract in isolation.
///
/// Idempotent, as every pattern in this module must be (`body_preview` runs
/// [`redact_text`] twice over a JSON body): the output `Account [REDACTED]`
/// has no UUID left to match.
fn redact_account_id(s: &str) -> String {
    text_patterns()
        .account_id
        .replace_all(s, concat!("Account ", "[REDACTED]"))
        .into_owned()
}

/// Build a redacted, length-bounded preview of a raw response body.
///
/// Fix round 1 (C1): if `raw` parses as JSON, this now routes through
/// [`redact_json`] first (re-serializing afterward and running
/// [`redact_text`] once more over the whole text — see the module docs for
/// the re-serialization side effects this implies) instead of applying only
/// the auxiliary regex layer, which never matched JSON's `"key":"value"`
/// syntax. Input that isn't valid JSON (or fails to re-serialize) falls
/// back to the plain [`redact_text`] path.
///
/// Either way, the result is then truncated to at most `max` bytes on a
/// UTF-8 character boundary — it never splits a multi-byte codepoint, so
/// this never panics regardless of where `max` would otherwise fall.
/// Required for the decode path (A1 Task 9) to stay panic-free on
/// arbitrary Tesla response bytes.
pub(crate) fn body_preview(raw: &str, max: usize) -> String {
    let redacted = match serde_json::from_str::<Value>(raw) {
        Ok(v) => {
            let scrubbed = redact_json(&v);
            match serde_json::to_string(&scrubbed) {
                Ok(s) => redact_text(&s),
                Err(_) => redact_text(raw),
            }
        }
        Err(_) => redact_text(raw),
    };
    if redacted.len() <= max {
        return redacted;
    }
    // Largest char boundary <= max.
    let mut end = max;
    while end > 0 && !redacted.is_char_boundary(end) {
        end -= 1;
    }
    redacted[..end].to_string()
}

/// Turns a transport error into a human-readable string, appending the source chain as well
/// and redacting the result.
///
/// The error text may contain the request URL (and the VIN inside it), so the original text is
/// not emitted as is (REQ-18).
pub(crate) fn redact_transport_error(e: &(dyn std::error::Error + 'static)) -> String {
    let mut s = e.to_string();
    let mut src: Option<&dyn std::error::Error> = e.source();
    while let Some(inner) = src {
        s.push_str(": ");
        s.push_str(&inner.to_string());
        src = inner.source();
    }
    redact_text(&s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_bearer_and_vin_and_coord() {
        let s = "Authorization: Bearer eyJABCDEF.tok.sig VIN 5YJ3E1EA7KF000000 at 37.5665,126.9780";
        let r = redact_text(s);
        assert!(!r.contains("eyJABCDEF"));
        assert!(r.contains("[REDACTED]"));
        assert!(!r.contains("5YJ3E1EA7KF000000"));
        assert!(r.contains("[VIN]"));
        assert!(r.contains("[COORD]"));
    }

    /// A3 Task 7 (design §8): the 412 body's account UUID is masked, and the
    /// diagnostic request id next to it is **not**. Both halves are asserted
    /// — a global UUID substitution would satisfy the first and silently
    /// break the second, which is the actual failure mode this pattern was
    /// written to avoid.
    #[test]
    fn redacts_account_uuid_but_keeps_bare_uuid() {
        let s = "Account 801802ed-225d-4b13-acf6-f69fc59b2ef0 must be registered; \
                 request 0aea2788-ca44-4391-95a3-774e13da5b5d";
        let out = redact_account_id(s);
        assert!(
            !out.contains("801802ed-225d-4b13-acf6-f69fc59b2ef0"),
            "계정 ID 가 남았다: {out}"
        );
        assert!(
            out.contains("0aea2788-ca44-4391-95a3-774e13da5b5d"),
            "진단용 request id 까지 지우면 안 된다: {out}"
        );
    }

    /// The pattern is reachable from the **only** public entry point
    /// ([`body_preview`]), not just when called directly. Without this, a
    /// refactor that drops the `redact_account_id` call out of
    /// `redact_text`'s pipeline would leave the unit test above green while
    /// every real error message leaked the account id again.
    #[test]
    fn body_preview_masks_account_uuid_in_a_real_412_body() {
        let raw = r#"{"error":"Account 801802ed-225d-4b13-acf6-f69fc59b2ef0 must be registered in the current region"}"#;
        let out = body_preview(raw, 512);
        assert!(
            !out.contains("801802ed-225d-4b13-acf6-f69fc59b2ef0"),
            "{out}"
        );
        assert!(out.contains("[REDACTED]"), "{out}");
    }

    /// `body_preview` runs `redact_text` twice over a JSON body, so every
    /// pattern here must be idempotent — a second pass must not re-match its
    /// own output.
    #[test]
    fn account_id_redaction_is_idempotent() {
        let once = redact_account_id("Account 801802ed-225d-4b13-acf6-f69fc59b2ef0 x");
        assert_eq!(redact_account_id(&once), once);
    }

    #[test]
    fn redact_json_keys() {
        let v = serde_json::json!({"access_token":"SECRET_XYZ","vin":"5YJ...","nested":{"refresh_token":"R"}});
        let out = redact_json(&v);
        assert_eq!(out["access_token"], serde_json::json!("[REDACTED]"));
        assert_eq!(
            out["nested"]["refresh_token"],
            serde_json::json!("[REDACTED]")
        );
    }

    #[test]
    fn body_preview_redacts_then_truncates() {
        let p = body_preview("access_token=SECRET_XYZ_LONG_VALUE", 12);
        assert!(!p.contains("SECRET_XYZ"));
        // M3 (Fix round 1): was a tautology (`p.len() <= 12` is guaranteed
        // by the truncation contract regardless of redaction). Split into
        // an unconditional length bound plus an explicit short-prefix
        // check, so a truncate-then-redact regression (which would leak
        // "SEC..." within the first 12 bytes) is caught by content, not by
        // the length assertion alone.
        assert!(p.len() <= 12);
        assert!(!p.contains("SEC"));
    }

    #[test]
    fn sentinel_never_survives() {
        assert!(
            !redact_text("Bearer ACCESS_TOKEN_SHOULD_NOT_APPEAR")
                .contains("ACCESS_TOKEN_SHOULD_NOT_APPEAR")
        );
    }

    /// Resolution 2: `body_preview` must truncate on a UTF-8 char boundary,
    /// never mid-codepoint, even when `max` would otherwise split one.
    #[test]
    fn body_preview_truncates_on_char_boundary_multibyte() {
        // Korean text: each character is 3 bytes in UTF-8. max=13 lands
        // mid-codepoint under naive byte slicing.
        let raw = "안녕하세요 반갑습니다";
        let p = body_preview(raw, 13);
        // M3 (Fix round 1): dropped the `chars().all(|c| c != '\u{FFFD}')`
        // check — it was vacuous, since a `&str` is guaranteed valid UTF-8
        // by the type system (a mid-codepoint slice panics rather than
        // yielding U+FFFD). The real property under test is that this line
        // ran at all without panicking, plus the explicit length bound.
        assert!(p.len() <= 13);

        // Emoji (4-byte UTF-8) — sweep every max to prove no panic anywhere.
        let raw_emoji = "abc😀😀😀def";
        for max in 0..raw_emoji.len() {
            let p = body_preview(raw_emoji, max);
            assert!(p.len() <= max);
        }
    }

    #[test]
    fn redact_json_allowlist_case_insensitive() {
        let v = serde_json::json!({"Authorization": "Bearer abc", "AUTHORIZATION": "abc"});
        let out = redact_json(&v);
        assert_eq!(out["Authorization"], serde_json::json!("[REDACTED]"));
        assert_eq!(out["AUTHORIZATION"], serde_json::json!("[REDACTED]"));
    }

    #[test]
    fn redact_json_recurses_into_arrays() {
        let v = serde_json::json!([{"token": "abc"}, {"nested": [{"client_secret": "xyz"}]}]);
        let out = redact_json(&v);
        assert_eq!(out[0]["token"], serde_json::json!("[REDACTED]"));
        assert_eq!(
            out[1]["nested"][0]["client_secret"],
            serde_json::json!("[REDACTED]")
        );
    }

    #[test]
    fn redact_json_vin_key_not_wholesale_replaced_but_value_regex_scrubbed() {
        // Controller amendment (Fix round 1): `vin` stays OUT of the key
        // allowlist (its key must never trigger wholesale
        // `"[REDACTED]"` replacement — that's what this test pins), but
        // its string value is no longer exempt from the auxiliary
        // `redact_text` leaf pass, so a real VIN is regex-scrubbed to
        // "[VIN]" instead of surviving verbatim in JSON output.
        let v = serde_json::json!({"vin": "5YJ3E1EA7KF000000"});
        let out = redact_json(&v);
        assert_ne!(out["vin"], serde_json::json!("[REDACTED]"));
        assert_eq!(out["vin"], serde_json::json!("[VIN]"));
    }

    /// M1 (Fix round 1): pin wholesale replacement for an allowlisted key
    /// even when its value is a non-string (object) shape — the most
    /// likely property to be silently inverted by a refactor that
    /// "recurses everything then redacts strings", since that shape would
    /// recurse into `{"inner": "s"}` instead of replacing it outright.
    #[test]
    fn redact_json_wholesale_replaces_non_string_allowlisted_value() {
        let v = serde_json::json!({"token": {"inner": "s"}});
        let out = redact_json(&v);
        assert_eq!(out["token"], serde_json::json!("[REDACTED]"));
    }

    #[test]
    fn kv_secret_pattern_keeps_key_name() {
        let r = redact_text("client_secret=abc123 refresh_token=def456");
        assert!(r.contains("client_secret=[REDACTED]"));
        assert!(r.contains("refresh_token=[REDACTED]"));
        assert!(!r.contains("abc123"));
        assert!(!r.contains("def456"));
    }

    /// M2 (Fix round 1): relabeled from `secret_pattern_applied_before_vin_pattern`.
    /// This input does NOT actually discriminate pattern order: the greedy
    /// bounded `Bearer\s+...` match consumes the entire token in one shot
    /// regardless of whether the VIN pattern ran first or last, so this is
    /// a structural/documentation test of the invariant (secret patterns
    /// run before the VIN pattern — see `redact_text`'s doc comment and
    /// `text_patterns()`'s application order). The actually-discriminating
    /// case is `secret_pattern_before_vin_pattern_is_load_bearing` below
    /// (Fix round 2).
    #[test]
    fn bearer_token_fully_redacted_even_when_containing_vin_shaped_substring() {
        let r = redact_text("Bearer ABCDEFGHJKLMNPRST1234567890");
        assert!(!r.contains("ABCDEFGHJKLMNPRST"));
        assert!(r.contains("[REDACTED]"));
        assert!(!r.contains("[VIN]"));
    }

    /// Fix round 2: converts the ordering documentation into an actually
    /// discriminating test. `"5YJ3E1EA7KF000000"` alone has `\b` word
    /// boundaries on both sides independent of the preceding `"Bearer "` —
    /// under the current (secret-patterns-first) ordering this whole input
    /// is consumed by the `bearer` pattern and becomes exactly
    /// `"[REDACTED]"`; if the VIN pattern ran first instead, it would match
    /// the 17-char run before `bearer` got a chance, producing
    /// `"Bearer [VIN]"` — a different, wrong result (the token would look
    /// unredacted to a reader scanning for `[REDACTED]`).
    #[test]
    fn secret_pattern_before_vin_pattern_is_load_bearing() {
        let r = redact_text("Bearer 5YJ3E1EA7KF000000");
        assert_eq!(r, "[REDACTED]");
    }

    // --- Fix round 1: C1 concrete counterexamples, via body_preview -----

    /// C1: a JSON body's `refresh_token` value must not survive verbatim.
    /// Before the fix, `body_preview` never invoked `redact_json` at all —
    /// only the auxiliary `key=value` regex ran, which never matches
    /// JSON's `"key":"value"` syntax.
    #[test]
    fn body_preview_redacts_json_refresh_token() {
        let raw = r#"{"refresh_token":"NA_a1b2c3d4e5f6g7"}"#;
        let p = body_preview(raw, 200);
        assert!(!p.contains("NA_a1b2c3d4e5f6g7"));
        assert!(p.contains("[REDACTED]"));
    }

    /// C1 (baseline, kept from Fix round 1): the simple two-key
    /// `drive_state` shape must not survive verbatim. The full Tesla shape
    /// (with `native_*`) is covered separately below —
    /// `body_preview_redacts_native_coordinate_triple`.
    #[test]
    fn body_preview_redacts_json_coordinates() {
        let raw = r#"{"drive_state":{"latitude":37.5665,"longitude":126.978}}"#;
        let p = body_preview(raw, 200);
        assert!(!p.contains("37.5665"));
        assert!(!p.contains("126.978"));
        assert!(p.contains("[COORD]"));
    }

    /// C1 (Fix round 2, the actual re-review finding): Tesla's real
    /// `drive_state` carries FOUR coordinate-shaped fields for the same GPS
    /// fix — `latitude`/`longitude` AND `native_latitude`/
    /// `native_longitude` — with `native_location_supported` (a non-decimal
    /// value) sitting between the native pair. The Fix round 1 pair-pattern
    /// could bridge at most one intervening key, so `native_latitude`/
    /// `native_longitude` (two keys apart) leaked. The key-anchored
    /// pattern catches each field independently, so the number of
    /// intervening keys is irrelevant. Uses the untrimmed shape on purpose
    /// — this is what caught the Fix round 1 gap.
    #[test]
    fn body_preview_redacts_native_coordinate_triple() {
        let raw = r#"{"drive_state":{"latitude":37.5665,"longitude":126.978,"native_latitude":37.5665,"native_location_supported":1,"native_longitude":126.978,"native_type":"wgs"}}"#;
        let p = body_preview(raw, 1000);
        assert!(!p.contains("37.5665"));
        assert!(!p.contains("126.978"));
        // Non-coordinate keys/values in the same object must survive.
        assert!(p.contains("native_location_supported"));
        assert!(p.contains("native_type"));
        assert!(p.contains("wgs"));
    }

    /// Fix round 2 finding (a) regression test: numeric fields that are
    /// NOT coordinates (charge/climate telemetry) must not be treated as
    /// such, and — since the key-anchored pattern replaces only the number,
    /// never the key — every field name must survive in the preview. The
    /// Fix round 1 pair-pattern would have swallowed an intervening key
    /// name whole (e.g. `charge_rate` between two unrelated decimals);
    /// this proves that regression is closed.
    #[test]
    fn body_preview_charge_climate_fields_not_treated_as_coordinates() {
        let raw = r#"{"charge_state":{"battery_range":272.5,"charge_amps":16.0,"charge_energy_added":5.43,"charge_rate":0.0},"climate_state":{"driver_temp_setting":22.0,"inside_temp":21.5,"outside_temp":18.0,"passenger_temp_setting":22.0}}"#;
        let p = body_preview(raw, 1000);
        assert!(!p.contains("[COORD]"));
        for key in [
            "battery_range",
            "charge_amps",
            "charge_energy_added",
            "charge_rate",
            "driver_temp_setting",
            "inside_temp",
            "outside_temp",
            "passenger_temp_setting",
        ] {
            assert!(p.contains(key), "expected key {key} to survive: {p}");
        }
    }

    /// Scope boundary documented at the module level: an unlabeled bare
    /// decimal (no coordinate-shaped key, no comma-adjacent partner) is
    /// indistinguishable from any other float field and stays unredacted.
    #[test]
    fn body_preview_unlabeled_decimal_not_redacted_by_design() {
        let p = body_preview("37.5665", 200);
        assert_eq!(p, "37.5665");
    }

    // --- Fix round 1: I1 case-insensitivity ------------------------------

    #[test]
    fn redact_text_bearer_case_insensitive() {
        let r = redact_text("authorization: bearer abc123XYZSECRETVALUE");
        assert!(!r.contains("abc123XYZSECRETVALUE"));
        assert!(r.contains("[REDACTED]"));
    }

    #[test]
    fn redact_text_kv_secret_case_insensitive() {
        let r1 = redact_text("Access_Token=SECRETVALUE1");
        assert!(!r1.contains("SECRETVALUE1"));
        assert!(r1.contains("[REDACTED]"));

        let r2 = redact_text("ACCESS_TOKEN=SECRETVALUE2");
        assert!(!r2.contains("SECRETVALUE2"));
        assert!(r2.contains("[REDACTED]"));
    }

    #[test]
    fn redact_text_vin_case_insensitive() {
        let r = redact_text("vin 5yj3e1ea7kf000000 nearby");
        assert!(!r.contains("5yj3e1ea7kf000000"));
        assert!(r.contains("[VIN]"));
    }

    // --- Fix round 2: I2 residual (`code`) --------------------------------

    /// Controller decision closing I2's residual: the OAuth authorization
    /// `code` query parameter is a bearer-equivalent secret and must be
    /// redacted, but the bare word `code` is a dangerously common
    /// substring (`error_code`, `status_code`, `zip_code`, ...), so the
    /// pattern uses a leading `\b` to refuse matching directly after a
    /// word character like the `_` in `error_code`.
    #[test]
    fn kv_secret_code_pattern_redacts_oauth_code_but_not_error_code() {
        let r1 = redact_text("callback?code=NA_9f2b1c&state=x");
        assert!(!r1.contains("NA_9f2b1c"));
        assert!(r1.contains("[REDACTED]"));

        let r2 = redact_text("error_code=invalid_grant");
        assert_eq!(r2, "error_code=invalid_grant");
    }

    // --- Fix round 2: gap the re-reviewer flagged without scoring --------

    /// I2's mechanism (non-allowlisted secret caught via the string-leaf
    /// `redact_text` pass, not the JSON key allowlist) had no dedicated
    /// `body_preview`-level test. `id_token` is deliberately NOT in
    /// `JSON_KEY_ALLOWLIST` (per the controller amendment in Fix round 1) —
    /// it is still redacted because it's a JWT, and the JWT pattern runs
    /// over every string leaf regardless of key name.
    #[test]
    fn body_preview_redacts_json_id_token() {
        let raw = r#"{"id_token":"eyJhbGciOiJSUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dozjgNryP4J3jVmNHl0w5N_XgL0n3I9PlFUP0THsR8U"}"#;
        let p = body_preview(raw, 500);
        assert!(!p.contains("eyJzdWIiOiIxMjM0NTY3ODkwIn0"));
        assert!(p.contains("[REDACTED]"));
    }

    // --- Fix round 2: finding (c), bounded Bearer character class --------

    /// Fix round 2 finding (c), direct unit-level proof — CORRECTED in Fix
    /// round 3 (test-strength finding 4): the round-2 version of this test
    /// used an *empty* token (`"Bearer "` with nothing after the space
    /// before the next quote), so it merely proved the pattern doesn't
    /// match at all — not that it "stops at the quote" as the docstring
    /// claimed, since a `\S+`/bounded-class match needs at least one
    /// character and there was none available either way. A non-empty
    /// token is required to actually exercise the bound: with it, the
    /// match succeeds and stops exactly at the following `"`.
    #[test]
    fn bearer_pattern_bounded_class_does_not_eat_json_structure() {
        let r = redact_text(r#"Bearer abc123","yy":"more","zz":"tail_data"}"#);
        assert_eq!(r, r#"[REDACTED]","yy":"more","zz":"tail_data"}"#);
    }

    /// Fix round 2 finding (c), full-pipeline proof via `body_preview`,
    /// corrected the same way (Fix round 3 / finding 4): non-empty token.
    #[test]
    fn body_preview_bearer_does_not_eat_trailing_json_keys() {
        let raw = r#"{"scheme":"Bearer abc123","yy":"more","zz":"tail_data"}"#;
        let p = body_preview(raw, 500);
        assert!(!p.contains("abc123"));
        assert!(p.contains("yy"));
        assert!(p.contains("more"));
        assert!(p.contains("tail_data"));
    }

    // --- Fix round 3: C1 (Critical), suffix-anchored coordinate keys -----

    /// The actual re-review finding: round 2's `native_*` test was claimed
    /// (falsely — corrected in the report) to use "Tesla's real shape". It
    /// omitted `active_route_*` (present whenever navigation is active —
    /// the nav *destination*, typically the owner's home address, the most
    /// sensitive coordinate in the whole payload) and every other
    /// non-coordinate `drive_state` field. This test uses the full field
    /// set the coordinator specified: the five `active_route_*` fields,
    /// `gps_as_of`, `heading`, `power`, `shift_state`, `speed`, `timestamp`,
    /// alongside `latitude`/`longitude`/`native_*`.
    ///
    /// Fix round 4 (finding i): `active_route_destination` was `"Home"` in
    /// round 3 — the one value that makes "this field survives unredacted"
    /// read as harmless. Tesla actually puts the geocoded street address
    /// there, so the value here is a real address shape and the assertion
    /// below is now that it is **gone**.
    #[test]
    fn body_preview_redacts_full_drive_state_shape() {
        let raw = r#"{"drive_state":{
            "active_route_destination":"1234 Elm St, Palo Alto, CA 94301",
            "active_route_latitude":37.4419,
            "active_route_longitude":-122.143,
            "active_route_miles_to_arrival":5.3,
            "active_route_minutes_to_arrival":12,
            "gps_as_of":1700000000,
            "heading":271,
            "latitude":37.5665,
            "longitude":126.978,
            "native_latitude":37.5665,
            "native_location_supported":1,
            "native_longitude":126.978,
            "native_type":"wgs",
            "power":0,
            "shift_state":null,
            "speed":null,
            "timestamp":1700000000123
        }}"#;
        let p = body_preview(raw, 2000);

        // All six coordinate values gone (active_route x2, plain x2,
        // native x2 — 37.5665 and 126.978 each appear twice).
        for leaked in ["37.4419", "-122.143", "37.5665", "126.978"] {
            assert!(
                !p.contains(leaked),
                "coordinate value leaked: {leaked} in {p}"
            );
        }
        assert_eq!(p.matches("[COORD]").count(), 6);

        // Fix round 4 (finding i): the destination ADDRESS must be gone too
        // — no fragment of it may survive — while the key stays, so the
        // preview still shows which field was scrubbed.
        for leaked in ["1234 Elm St", "Palo Alto", "94301", "Elm"] {
            assert!(!p.contains(leaked), "address leaked: {leaked} in {p}");
        }
        assert!(p.contains(r#""active_route_destination":"[REDACTED]""#));

        // Every non-coordinate, non-location key AND its value survives.
        assert!(p.contains(r#""active_route_miles_to_arrival":5.3"#));
        assert!(p.contains(r#""active_route_minutes_to_arrival":12"#));
        assert!(p.contains(r#""gps_as_of":1700000000"#));
        assert!(p.contains(r#""heading":271"#));
        assert!(p.contains(r#""native_location_supported":1"#));
        assert!(p.contains(r#""native_type":"wgs""#));
        assert!(p.contains(r#""power":0"#));
        assert!(p.contains(r#""shift_state":null"#));
        assert!(p.contains(r#""speed":null"#));
        assert!(p.contains(r#""timestamp":1700000000123"#));
    }

    /// Finding 3a: a coordinate value serialized as a JSON string (not a
    /// bare number) must still be redacted — the key already tells us it's
    /// a coordinate, so the optional quote around the number is safe to
    /// accept.
    #[test]
    fn body_preview_redacts_string_serialized_coordinate() {
        let raw = r#"{"drive_state":{"latitude":"37.5665"}}"#;
        let p = body_preview(raw, 200);
        assert!(!p.contains("37.5665"));
        assert!(p.contains("[COORD]"));
    }

    /// Finding 3b: an integer-valued coordinate (no fractional part) must
    /// still be redacted.
    #[test]
    fn body_preview_redacts_integer_valued_coordinate() {
        let raw = r#"{"drive_state":{"latitude":37}}"#;
        let p = body_preview(raw, 200);
        assert!(!p.contains(r#""latitude":37}"#));
        assert!(p.contains("[COORD]"));
    }

    /// Finding 3c: `lat=`/`lon=` in plain-text log lines (unquoted key,
    /// `=` delimiter) were covered by nothing before this round, despite
    /// the module's stated purpose of covering "plain-text log lines".
    /// Also proves the word-boundary guard: `flat=` / `salon=` do NOT
    /// match, matching the same `"flat"`/`"salon"` exclusion documented
    /// for `coord_keyed`.
    #[test]
    fn coord_kv_pattern_redacts_bare_lat_lon_log_line() {
        let r = redact_text("lat=37.5665 lon=126.978 active_route_latitude=37.4419");
        assert!(!r.contains("37.5665"));
        assert!(!r.contains("126.978"));
        assert!(!r.contains("37.4419"));
        assert!(r.contains("lat=[COORD]"));
        assert!(r.contains("lon=[COORD]"));
        assert!(r.contains("active_route_latitude=[COORD]"));

        let r2 = redact_text("flat=37.5665 salon=126.978");
        assert_eq!(r2, "flat=37.5665 salon=126.978");
    }

    // --- Fix round 3: finding 2, kv_secret bounded character class -------

    /// Adding `code` to `kv_secret` in round 2 re-opened finding (c) on
    /// this sibling pattern: `\S+` ran past the closing quote of a JSON
    /// string value straight through a following sibling key. Bounded to
    /// `[^\s",}\]&]+` (same as `bearer`, plus `&` so a sibling query
    /// param survives too). Strengthened per the coordinator's note: the
    /// round-2 test only asserted the secret was gone, not that the
    /// trailing content survived — this one asserts both.
    ///
    /// Fix round 4 (finding ii): tightened from four `contains` checks to an
    /// exact `assert_eq!`. The `contains("[REDACTED]")` form was satisfied
    /// by the malformed double-bracket output this input actually produced
    /// (`code=[REDACTED]]`, from `redact_text` running twice — leaf pass then
    /// whole-text pass), so the bug lived under a green test.
    #[test]
    fn kv_secret_bounded_class_does_not_eat_json_structure() {
        let raw = r#"{"error":"redirect failed for https://auth.tesla.com/callback?code=NA_9f2b&state=abc","status":400}"#;
        let p = body_preview(raw, 500);
        assert_eq!(
            p,
            r#"{"error":"redirect failed for https://auth.tesla.com/callback?code=[REDACTED]&state=abc","status":400}"#
        );
    }

    // --- Fix round 4: (i) free-text destination address -------------------

    /// Finding (i): `active_route_destination` carries the geocoded street
    /// address of wherever the car is navigating to — in practice the
    /// owner's home. Redacting `active_route_latitude` (which the module
    /// calls the most sensitive coordinate in the payload *because* it is
    /// the home) while printing the same home as plain English text is not
    /// redaction. Key-anchored on any key ending in `destination`, in the
    /// whole-text pass — the string-leaf pass cannot see the key.
    #[test]
    fn body_preview_redacts_active_route_destination_address() {
        let raw = r#"{"drive_state":{"active_route_destination":"1234 Elm St, Palo Alto, CA 94301","active_route_latitude":37.4419,"active_route_longitude":-122.143}}"#;
        let p = body_preview(raw, 500);
        assert_eq!(
            p,
            r#"{"drive_state":{"active_route_destination":"[REDACTED]","active_route_latitude":[COORD],"active_route_longitude":[COORD]}}"#
        );
    }

    /// Suffix-anchoring, not enumeration — the property that failed in
    /// rounds 1 and 2 when coordinate keys were hand-listed. An unprefixed
    /// `destination` and a hypothetical future prefixed variant must both be
    /// caught by the same pattern, with no code change.
    #[test]
    fn destination_pattern_is_suffix_anchored_not_enumerated() {
        let raw = r#"{"destination":"9 Rue de Rivoli, Paris","route_v2_destination":"55 Sejong-daero, Seoul","destination_type":"address"}"#;
        let p = body_preview(raw, 500);
        assert!(!p.contains("Rivoli"), "unprefixed destination leaked: {p}");
        assert!(!p.contains("Sejong"), "prefixed destination leaked: {p}");
        assert!(p.contains(r#""destination":"[REDACTED]""#));
        assert!(p.contains(r#""route_v2_destination":"[REDACTED]""#));
        // `destination_type` does not END in `destination`, so it is not a
        // location value and survives — the suffix anchor is a bound, not a
        // substring search.
        assert!(p.contains(r#""destination_type":"address""#));
    }

    /// The value body must be escape-aware. `"[^"]*"` stops at the first
    /// `\"` inside the address and would leave the tail (`Seoul`) in the
    /// clear; `(?:[^"\\]|\\[\s\S])*` consumes the escape pair whole.
    #[test]
    fn destination_pattern_handles_escaped_quotes_in_value() {
        let raw = r#"{"active_route_destination":"The \"Big\" House, Seoul"}"#;
        let p = body_preview(raw, 500);
        assert!(!p.contains("Big"), "escaped-quote tail leaked: {p}");
        assert!(!p.contains("Seoul"), "escaped-quote tail leaked: {p}");
        assert_eq!(p, r#"{"active_route_destination":"[REDACTED]"}"#);
    }

    /// A null destination (what Tesla sends with no active route) is not a
    /// quoted string, carries no address, and must pass through — the
    /// pattern must not mangle unrelated shapes to reach it.
    #[test]
    fn destination_null_value_untouched() {
        let raw = r#"{"active_route_destination":null}"#;
        assert_eq!(body_preview(raw, 200), raw);
    }

    // --- Fix round 4: (ii) idempotency ------------------------------------

    /// Finding (ii): `body_preview` applies `redact_text` twice to any JSON
    /// body (string-leaf pass inside `redact_json`, then whole-text pass
    /// over the re-serialization), so every pattern must be a no-op on its
    /// own output. `kv_secret` was not: `]` is excluded from its value class,
    /// so the second pass captured `[REDACTED` and re-appended a bracket,
    /// producing `code=[REDACTED]]`.
    #[test]
    fn redact_text_is_idempotent_for_kv_secret() {
        let once = redact_text("access_token=SECRET1 code=NA_9f2b client_secret=CS3");
        assert_eq!(
            once,
            "access_token=[REDACTED] code=[REDACTED] client_secret=[REDACTED]"
        );
        assert_eq!(redact_text(&once), once, "kv_secret is not idempotent");
    }

    /// The same property across every other pattern in `redact_text`, so a
    /// future pattern addition that re-matches its own marker is caught here
    /// rather than silently corrupting previews.
    #[test]
    fn redact_text_is_idempotent_across_all_patterns() {
        let inputs = [
            "Authorization: Bearer eyJABCDEF.tok.sig",
            "VIN 5YJ3E1EA7KF000000 at 37.5665,126.9780",
            r#"{"latitude":37.5665,"native_longitude":126.978}"#,
            "lat=37.5665 lon=126.978",
            r#"{"active_route_destination":"1234 Elm St, Palo Alto, CA 94301"}"#,
            "callback?code=NA_9f2b1c&state=x",
        ];
        for raw in inputs {
            let once = redact_text(raw);
            let twice = redact_text(&once);
            assert_eq!(once, twice, "not idempotent for input: {raw}");
        }
    }

    // --- Fix round 4: (iii) coordinate integer part ------------------------

    /// Finding (iii): with the fractional part optional (round 3), a
    /// `\d{1,3}` integer cap stopped being a harmless total miss and became
    /// a partial leak that *looks* scrubbed — the marker is emitted and the
    /// rest of the digits survive right next to it. Both counterexamples the
    /// reviewer gave, asserted exactly.
    #[test]
    fn body_preview_redacts_out_of_range_coordinate_integer_part() {
        // Was: {"latitude":[COORD]4.5678}
        assert_eq!(
            body_preview(r#"{"latitude":1234.5678}"#, 200),
            r#"{"latitude":[COORD]}"#
        );
        // Was: {"e7_lat":[COORD]665000} — an E7-encoded coordinate, still
        // matched by the suffix anchor (`e7_` is a valid prefix segment).
        assert_eq!(
            body_preview(r#"{"e7_lat":375665000}"#, 200),
            r#"{"e7_lat":[COORD]}"#
        );
        // Same widening on the unquoted log-line form.
        assert_eq!(redact_text("e7_lat=375665000"), "e7_lat=[COORD]");
    }

    // --- Final fix wave: F2, normalized JSON key allowlist matching --------

    /// F2: the allowlist compared byte-exactly modulo case, so every
    /// separator variant of `access_token` bypassed wholesale redaction. The
    /// value here is deliberately chosen to be caught by *nothing else*: not
    /// a JWT (no `eyJ` prefix), no `Bearer ` prefix, no `=` for `kv_secret`,
    /// not 17 chars, not coordinate-shaped — so if the key doesn't match,
    /// the secret ships. `session_id` rides along as the untouched control:
    /// it is not on the allowlist under any folding and must survive, or the
    /// normalization has widened into a blanket redaction.
    #[test]
    fn redact_json_allowlist_matches_separator_and_camel_case_spellings() {
        for key in [
            "access_token",
            "Access-Token",
            "accessToken",
            "AccessToken",
            "ACCESS-TOKEN",
        ] {
            let raw = format!(r#"{{"{key}":"s3cr3tvalue","session_id":"abc123def456"}}"#);
            let p = body_preview(&raw, 500);
            assert!(!p.contains("s3cr3tvalue"), "{key} leaked its value: {p}");
            assert!(
                p.contains(&format!(r#""{key}":"[REDACTED]""#)),
                "{key} not wholesale-redacted: {p}"
            );
            // Control: a non-allowlisted key must be untouched, so this is a
            // key-matching fix and not "redact everything".
            assert!(
                p.contains(r#""session_id":"abc123def456""#),
                "unrelated key was collaterally redacted: {p}"
            );
        }
    }

    /// The same folding must apply to every logical key on the list, not just
    /// the one the reviewer happened to probe.
    #[test]
    fn redact_json_allowlist_folding_applies_to_all_five_keys() {
        for (key, value) in [
            ("Refresh-Token", "rrrrvalue"),
            ("clientSecret", "ccccvalue"),
            ("Authorization", "aaaavalue"),
            ("To_Ken", "ttttvalue"),
            ("REFRESHTOKEN", "zzzzvalue"),
        ] {
            let raw = format!(r#"{{"{key}":"{value}"}}"#);
            let p = body_preview(&raw, 500);
            assert!(!p.contains(value), "{key} leaked its value: {p}");
        }
    }

    /// `normalize_key` must not fold two *different* logical keys together,
    /// and must leave unrelated keys off the list. `id_token` is the pointed
    /// case: the controller amendment keeps it OFF the allowlist (it is
    /// caught as a JWT by the string-leaf pass instead), and folding
    /// separators must not accidentally promote it onto the list.
    #[test]
    fn normalize_key_does_not_widen_the_allowlist_beyond_its_five_keys() {
        assert!(is_allowlisted_key("access_token"));
        assert!(is_allowlisted_key("Access Token"));
        assert!(!is_allowlisted_key("id_token"));
        assert!(!is_allowlisted_key("idToken"));
        assert!(!is_allowlisted_key("token_type"));
        assert!(!is_allowlisted_key("vin"));
        assert!(!is_allowlisted_key("session_id"));
        // A non-allowlisted key whose value is a plain opaque string is
        // returned verbatim through the full pipeline.
        assert_eq!(
            body_preview(r#"{"token_type":"bearer_ish_value"}"#, 200),
            r#"{"token_type":"bearer_ish_value"}"#
        );
    }

    // --- Final fix wave: F5, the idempotency guard is not a bypass ---------

    /// F5: round 4's guard was `caps[2].starts_with(REDACTED_OPEN)`, so any
    /// value literally *beginning* `[REDACTED` suppressed its own redaction
    /// — and the value is response-body content, i.e. attacker-choosable.
    /// The second pass captures exactly `[REDACTED` and nothing more (the
    /// `]` is outside the value class), so `==` is the precise guard.
    #[test]
    fn kv_secret_guard_is_not_a_scrubber_bypass() {
        // The reviewer's exact bypass input.
        let p = body_preview(r#"{"note":"code=[REDACTEDreal_secret_here"}"#, 500);
        assert!(!p.contains("real_secret_here"), "bypass still open: {p}");
        assert_eq!(p, r#"{"note":"code=[REDACTED]"}"#);

        // The same trick on the other kv_secret keys.
        assert_eq!(
            redact_text("access_token=[REDACTEDx1 client_secret=[REDACTED_nope"),
            "access_token=[REDACTED] client_secret=[REDACTED]"
        );
    }

    /// ...while the property the guard exists for still holds: a genuine
    /// second pass over already-redacted text is a no-op, no doubled bracket.
    #[test]
    fn kv_secret_guard_still_idempotent_after_bypass_fix() {
        let once = redact_text("access_token=SECRET1 code=NA_9f2b client_secret=CS3");
        assert_eq!(
            once,
            "access_token=[REDACTED] code=[REDACTED] client_secret=[REDACTED]"
        );
        assert_eq!(redact_text(&once), once);
        assert_eq!(redact_text(&redact_text(&once)), once);
    }

    // --- Final fix wave: F6, exponent-notation coordinates -----------------

    /// F6: `serde_json` re-serializes a small float in exponent form, and the
    /// key-anchored patterns stopped at the mantissa — emitting the `[COORD]`
    /// marker while leaving the exponent (the magnitude) in the clear. Both
    /// counterexamples, asserted exactly.
    #[test]
    fn coordinate_exponent_notation_is_fully_redacted() {
        // Was: {"latitude":[COORD]e-7}
        assert_eq!(
            body_preview(r#"{"latitude":1e-7}"#, 200),
            r#"{"latitude":[COORD]}"#
        );
        // Unquoted log-line form. Was: lat=[COORD]e-7
        assert_eq!(redact_text("lat=1e-7"), "lat=[COORD]");
        // Positive/capital-E/explicit-sign spellings, and a full mantissa.
        assert_eq!(
            body_preview(r#"{"native_longitude":"1.269780E+2"}"#, 200),
            r#"{"native_longitude":[COORD]}"#
        );
        assert_eq!(
            redact_text("active_route_latitude=-3.75665e1"),
            "active_route_latitude=[COORD]"
        );
        // Still idempotent with the exponent branch in place.
        let once = redact_text(r#"{"latitude":1e-7}"#);
        assert_eq!(redact_text(&once), once);
    }

    /// The exponent suffix must NOT have been added to the unanchored
    /// `coord_pair` pattern: with no key to identify it, the digit shape is
    /// the only coordinate evidence, and accepting exponents would widen it
    /// onto arbitrary float pairs. A bare exponent pair stays untouched.
    #[test]
    fn coord_pair_does_not_accept_exponent_notation() {
        assert_eq!(redact_text("1.5e10,2.5e10"), "1.5e10,2.5e10");
        // ...while the plain decimal pair it exists for still matches.
        assert_eq!(redact_text("at 37.5665,126.9780"), "at [COORD]");
    }
}

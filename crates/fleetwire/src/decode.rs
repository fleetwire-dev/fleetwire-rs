//! Response-body decoding helpers shared across endpoints.

use crate::error::Error;
use crate::model::{Envelope, Pagination};
use crate::pricing::Pricing;
use crate::redaction::body_preview;
use serde_json::error::Category;

/// The unwrapped envelope payload plus its sibling pagination metadata, if
/// any (A4 Task 2). `None` means the server did not send a `pagination`
/// sibling at all — see [`crate::model::Page`]'s doc for why that must stay
/// distinct from "it sent one with every field `null`".
///
/// Not `#[derive(Debug)]`: `pagination`'s type, [`crate::model::Pagination`],
/// lives in a module where "no type implements `Debug`" is a REQ-18 rule
/// (see `model`'s module doc), and a derive here is unconditional on that
/// concrete field — it cannot be made to hold only when the generic `T`
/// does. `Debug` is instead hand-written below, exactly the escape hatch
/// `model.rs`'s own doc prescribes: `pagination` carries page/cursor
/// numbers, not a VIN, token, or coordinate, so summarizing it down to
/// "was one sent at all" keeps REQ-18's redaction intent intact without
/// forcing every caller back onto `.err().unwrap()`.
pub(crate) struct Decoded<T> {
    pub payload: T,
    pub pagination: Option<Pagination>,
}

impl<T: std::fmt::Debug> std::fmt::Debug for Decoded<T> {
    /// Prints `payload` verbatim (bounded by `T: Debug`, the caller's own
    /// choice) but never `pagination`'s field values — only whether one
    /// arrived. `pagination_debug_never_prints_field_values` pins this: it
    /// is the rule this impl exists to enforce, not an incidental property
    /// of `#[derive(Debug)]` (which is exactly what this type opts out of).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Decoded")
            .field("payload", &self.payload)
            .field("pagination", &self.pagination.is_some())
            .finish()
    }
}

/// Upper bound, in bytes, on any body excerpt embedded in a decode-path
/// error (`Error::Api::body_preview`, `Error::Decode::body_preview`, and the
/// bounded string carried by `Error::Auth`). The design (SDD §2.4–2.5, §9.1)
/// requires *a* bound but fixes no value — this is an A1 choice, kept small
/// enough that an error value never balloons to the size of a full Tesla
/// response while still useful for diagnosing a gateway/captive-portal body.
const BODY_PREVIEW_MAX: usize = 256;

/// Decode a Fleet API response into `T`, applying the status/content-type
/// gate (design SDD §2.4–2.5) before attempting to parse the body.
///
/// Order, exactly as the design specifies:
/// 1. `status >= 400` is resolved first, independent of content-type: 401
///    becomes [`Error::Auth`], 408 becomes [`Error::VehicleOffline`], and
///    every other `>= 400` status becomes [`Error::Api`]. This runs before
///    the content-type check, so a `>= 400` gateway error page (e.g. a 502
///    with `text/html`) is reported as `Error::Api`, not `Error::Decode`.
/// 2. A `status < 400` response whose content-type does not
///    case-insensitively contain `"json"` — including a *missing*
///    content-type — is rejected as [`Error::Decode`] before any parse is
///    attempted. This is the gate that catches a captive portal or
///    misconfigured gateway that slips a non-JSON 2xx past the status check;
///    Tesla's API always sets a JSON content-type on success, so treating an
///    absent one as non-JSON is the safe direction.
/// 3. The body is parsed as `Envelope<T>` (`{"response": ...}`); a parse
///    failure — including an empty body or JSON that lacks the `response`
///    key — is [`Error::Decode`].
/// 4. On success, the unwrapped `response` payload is returned together with
///    the envelope's sibling `pagination`, if the server sent one (A4 Task
///    2) — see [`Decoded`].
///
/// Never panics: every string this function builds from `body` goes through
/// [`String::from_utf8_lossy`] (never `str::from_utf8(..).unwrap()`), and
/// every error string that is derived from request/response bytes is routed
/// through [`body_preview`] — never a bare [`crate::redaction::redact_text`]
/// call — so it is both redacted *and* bounded to [`BODY_PREVIEW_MAX`]
/// before it is placed in an [`Error`].
///
/// Fix round 1 (Important 1) closed the *unbounded length* half of a
/// body-supplied-secret leak in `Error::Decode::message` (it previously
/// called `redact_text` directly on `serde_json::Error::to_string()` and on
/// the raw `Content-Type` header, bypassing `body_preview` entirely) but
/// left a gap open: `body_preview` only gets its stronger, key-based
/// redaction (REQ-18's primary layer, see `redaction::redact_json`) when its
/// input parses as a whole JSON document, and a `serde_json::Error`'s
/// `Display` interpolates raw field values into English prose that never
/// parses as JSON — so a secret-shaped value that doesn't happen to match
/// one of `redact_text`'s five regex families (Bearer/JWT, `key=value`,
/// VIN, coordinate) survived verbatim, merely capped at 256 bytes. Real
/// secrets (OAuth codes, session ids, short API keys) are well under that
/// cap, so the bound bought no confidentiality for the actual threat.
///
/// Fix round 2 (Important 1, closed) removes the leak at its source instead
/// of trying to out-regex it: `serde_json::Error::classify()` distinguishes
/// [`Category::Data`] — the *only* category whose `Display` interpolates
/// body content (`invalid type: string "..."`, `invalid value: ...`) — from
/// [`Category::Syntax`]/[`Category::Eof`]/[`Category::Io`], which are
/// grammar-level and value-free by construction (`expected ',' or '}' at
/// line…`, `EOF while parsing a value`; `Io` is unreachable from
/// `from_slice` over an in-memory `&[u8]` and would be value-free too). A
/// `Category::Data` error's message is rebuilt from `err.line()`/
/// `err.column()` only — no body bytes at all, not even redacted ones — so
/// there is nothing left for a pattern miss to leak. Syntax/Eof/Io keep
/// their verbatim `Display`, still bound-wrapped for uniformity, since no
/// body content can appear in them. `serde_json`'s `Data` clause never names
/// the offending field (no `serde_path_to_error` here), so the untrusted
/// half of the message was also the least diagnostically valuable half —
/// what's lost is the expected-type hint; what's kept is line/column plus
/// the sibling `body_preview` field, built from the raw body and so still
/// covered by `redact_json`'s full key-allowlist pass.
///
/// Wired into production code by Task 6: [`crate::Client::send`] is its only
/// non-test caller.
pub(crate) fn decode<T: serde::de::DeserializeOwned>(
    status: http::StatusCode,
    content_type: Option<&str>,
    body: &[u8],
    pricing: Pricing,
    retry_after: Option<std::time::Duration>,
    www_authenticate: Option<&str>,
) -> Result<Decoded<T>, Error> {
    decode_with_txid_header(
        status,
        content_type,
        body,
        pricing,
        retry_after,
        www_authenticate,
        None,
    )
}

/// The real implementation behind [`decode`] (A4 Task 3). `decode` is kept as
/// a thin wrapper passing `x_txid: None` so the ~27 existing call sites across
/// A1–A3 stay untouched; only [`crate::Client::send`] needs the header-borne
/// `txid` and calls this directly.
pub(crate) fn decode_with_txid_header<T: serde::de::DeserializeOwned>(
    status: http::StatusCode,
    content_type: Option<&str>,
    body: &[u8],
    pricing: Pricing,
    retry_after: Option<std::time::Duration>,
    www_authenticate: Option<&str>,
    x_txid: Option<&str>,
) -> Result<Decoded<T>, Error> {
    gate(
        status,
        content_type,
        body,
        pricing,
        retry_after,
        www_authenticate,
        x_txid,
    )?;
    match serde_json::from_slice::<Envelope<T>>(body) {
        Ok(envelope) => Ok(Decoded {
            payload: envelope.response,
            pagination: envelope.pagination,
        }),
        Err(err) => Err(body_parse_error(&err, body, status, pricing)),
    }
}

/// Decodes the response body as **one opaque `serde_json::Value`, envelope and
/// all**, after the exact same status and content-type gates
/// [`decode_with_txid_header`] applies.
///
/// **The whole body, not the envelope's `response` member.**
/// [`decode_with_txid_header`] parses `Envelope<T>` and hands back
/// `envelope.response` plus `pagination`; [`crate::model::Envelope`] carries no
/// `deny_unknown_fields`, so **any other top-level sibling Tesla sends is
/// dropped there without a trace** (the measured `count` sibling is exactly
/// such a key — crate-level open item O13). The one caller of this function
/// exists to *see* new fields, so it must not go through a struct that
/// silently eats them.
///
/// **The gates are shared, not re-implemented.** Everything
/// [`decode_with_txid_header`] refuses — `>= 400` (including the 401/408
/// classification), a non-JSON content-type — this refuses identically,
/// because both call [`gate`]. A second copy of that logic is exactly the
/// three-verbatim-copies mistake A5c-2a T19 spent a fix round undoing.
///
/// **No schema check whatsoever.** Any syntactically valid JSON document is
/// accepted, whatever shape it has. Deciding whether the body is plausible for
/// the endpoint that was called is the caller's problem.
pub(crate) fn decode_body_raw(
    status: http::StatusCode,
    content_type: Option<&str>,
    body: &[u8],
    pricing: Pricing,
    retry_after: Option<std::time::Duration>,
    www_authenticate: Option<&str>,
    x_txid: Option<&str>,
) -> Result<serde_json::Value, Error> {
    gate(
        status,
        content_type,
        body,
        pricing,
        retry_after,
        www_authenticate,
        x_txid,
    )?;
    serde_json::from_slice::<serde_json::Value>(body)
        .map_err(|err| body_parse_error(&err, body, status, pricing))
}

/// Turns a `serde_json` failure over the response body into [`Error::Decode`].
///
/// Shared by [`decode_with_txid_header`] and [`decode_body_raw`] so the
/// redaction rule below cannot hold in one and not the other.
fn body_parse_error(
    err: &serde_json::Error,
    body: &[u8],
    status: http::StatusCode,
    pricing: Pricing,
) -> Error {
    let lossy = String::from_utf8_lossy(body);
    // Fix round 2 (Important 1, still-open finding closed):
    // `Category::Data` is the only `serde_json` error category whose
    // `Display` interpolates raw body content (`invalid type:
    // string "..."`, `invalid value: ...`); `matches!` rather than
    // an exhaustive match, so a future `serde_json` category can't
    // silently fail to compile into "keep verbatim" (the safe
    // default for an unrecognized category — it would already have
    // been unrecognized under fix round 1's plain
    // `body_preview(&err.to_string(), ..)`, so this is strictly no
    // worse). Data errors are rebuilt from line/column alone — no
    // body bytes reach `message` at all, so there is nothing left
    // for a `redact_text` pattern miss to leak. Syntax/Eof (and Io,
    // unreachable from `from_slice`) never interpolate body content
    // to begin with, so their verbatim `Display` carries full
    // grammar-level diagnostic value with no confidentiality cost;
    // still wrapped in `body_preview` for the length bound and for
    // one uniform contract across every arm of this function.
    let message = if matches!(err.classify(), Category::Data) {
        body_preview(
            &format!(
                "invalid data at line {} column {}",
                err.line(),
                err.column()
            ),
            BODY_PREVIEW_MAX,
        )
    } else {
        body_preview(&err.to_string(), BODY_PREVIEW_MAX)
    };
    Error::Decode {
        message,
        status,
        body_preview: body_preview(&lossy, BODY_PREVIEW_MAX),
        pricing,
    }
}

/// The status and content-type gates every decode path shares.
///
/// **This is the single place that decides a response is an error rather
/// than a payload.** Both [`decode_with_txid_header`] and [`decode_body_raw`]
/// call it before they look at the body, so a 4xx/5xx can never be handed back
/// as a successfully decoded value by either — including
/// [`crate::Client::send_raw`], where "the caller gets a `Value`" would
/// otherwise be an easy way to turn an API error into data.
///
/// Order, exactly as the design specifies: status first (independent of
/// content-type), then content-type.
fn gate(
    status: http::StatusCode,
    content_type: Option<&str>,
    body: &[u8],
    pricing: Pricing,
    retry_after: Option<std::time::Duration>,
    www_authenticate: Option<&str>,
    x_txid: Option<&str>,
) -> Result<(), Error> {
    // Fix round 1 (Minor 7): computed lazily, only in the arms that actually
    // need it, instead of unconditionally at the top of the function (where
    // it was wasted work on the — overwhelmingly common — success path).
    // Fix round 2 (re-review correction): round 1 hoisted the computation to
    // the top of this `if` block, so the 408 arm — the routine case for a
    // client that never wakes the car — still paid for and discarded it.
    // Computed per-arm now; 408 never touches it.
    if status.as_u16() >= 400 {
        // A4 Task 3: shared by both non-401 arms below. Body `txid` wins over
        // the header (measured: the 408 fixture carries `txid` in the body;
        // the header is the fallback for responses that don't).
        let txid = extract_txid(body, x_txid);
        return Err(match status.as_u16() {
            // Final fix wave (F3): `pricing` is carried into the error
            // instead of being discarded. A 401 is an other-4xx answered by
            // a real endpoint, so design §9.2's table bills it on a
            // `Pricing::Data` endpoint — the old `Error::Auth(String)`
            // dropped the value here and `billing()` then hardcoded
            // `NotBilled`, silently understating spend.
            401 => Error::Auth {
                message: body_preview(&String::from_utf8_lossy(body), BODY_PREVIEW_MAX),
                pricing: Some(pricing),
            },
            // A4 Task 3: `txid` rides here, not only on `Error::Api` — the
            // live-measured 408 that motivates this field classifies as
            // `VehicleOffline`, so putting `txid` on `Api` alone would lose
            // it on exactly the response it was measured against.
            408 => Error::VehicleOffline { pricing, txid },
            _ => Error::Api {
                status,
                message: body_preview(
                    &error_message(body, www_authenticate, status),
                    BODY_PREVIEW_MAX,
                ),
                retry_after,
                body_preview: body_preview(&String::from_utf8_lossy(body), BODY_PREVIEW_MAX),
                pricing,
                txid,
            },
        });
    }

    let is_json = content_type
        .map(|ct| ct.to_ascii_lowercase().contains("json"))
        .unwrap_or(false);
    if !is_json {
        let lossy = String::from_utf8_lossy(body);
        // Fix round 1 (Important 1): was `redact_text(&format!(...))` —
        // unbounded. Fix round 2 (re-review correction): the round-1 comment
        // here claimed this also gained `body_preview`'s JSON-aware
        // key-allowlist pass — it does not and cannot: this formatted
        // string always begins with the literal `"unexpected content-type:
        // "`, which can never parse as a JSON document, so
        // `body_preview`'s `serde_json::from_str` attempt always fails here
        // and it always falls through to the plain-text fallback
        // (`redact_text`'s regex layer). What this wrapping actually buys is
        // the length bound plus that regex layer — the same uniform
        // contract as every other error string here, nothing more.
        // `content_type` is attacker/gateway-supplied text (an HTTP
        // response header), so it still needs both.
        return Err(Error::Decode {
            message: body_preview(
                &format!(
                    "unexpected content-type: {}",
                    content_type.unwrap_or("<none>")
                ),
                BODY_PREVIEW_MAX,
            ),
            status,
            body_preview: body_preview(&lossy, BODY_PREVIEW_MAX),
            pricing,
        });
    }

    Ok(())
}

/// Passes through only a string that has non-whitespace content.
///
/// A4 Task 3: the live-measured 408 gives `"error_description": ""` — the
/// key is present, but an empty value must never win a fallback chain just
/// because the key exists. Every step of [`error_message`] and **both**
/// branches of [`extract_txid`] (body `txid` and the header fallback) are
/// gated through this.
fn non_empty(s: Option<&str>) -> Option<&str> {
    s.filter(|v| !v.trim().is_empty())
}

/// Extracts a human-readable message from a Tesla response.
///
/// Priority: `error` in the body JSON → `error_description` in the body JSON →
/// `error_description` in `WWW-Authenticate` → `canonical_reason()`. Each step is conditional
/// on the value being **non-empty** (A4 Task 3).
///
/// Why step 2 (now step 3) is needed (measured 2026-08-01): a 401 answered by the Doorkeeper
/// upstream has a **0-byte** body and carries `error_description` only in the header. A parser
/// that reads only the body gets nothing in that case.
///
/// A4 Task 3 measurement (2026-08-03): a 408 gives `"error_description":""` — the body
/// `error_description` can **exist as an empty string too**, so that empty value must not
/// become the final message just because the key is present. Every step is gated through
/// `non_empty`.
///
/// This value is **not used for classification** — the wording is an implementation detail
/// that, even for the same 401, splits into `invalid authentication` / `invalid bearer token`
/// depending on the upstream. Classification is done by status alone (design §7).
fn error_message(body: &[u8], www_authenticate: Option<&str>, status: http::StatusCode) -> String {
    let parsed = serde_json::from_slice::<serde_json::Value>(body).ok();
    if let Some(v) = &parsed {
        if let Some(s) = non_empty(v.get("error").and_then(|e| e.as_str())) {
            return s.to_owned();
        }
        if let Some(s) = non_empty(v.get("error_description").and_then(|e| e.as_str())) {
            return s.to_owned();
        }
    }
    if let Some(desc) = www_authenticate
        .and_then(extract_error_description)
        .filter(|d| !d.trim().is_empty())
    {
        return desc;
    }
    status
        .canonical_reason()
        .unwrap_or("unknown error")
        .to_owned()
}

/// Extracts Tesla's transaction id: body `txid` first, then the header
/// `x-txid` (A4 Task 3). The result is **never** run through
/// [`body_preview`]/redaction at the call site — see [`crate::error::Error`]'s
/// `txid` field doc for why a transaction id must survive verbatim.
///
/// Final fix wave item 1: both branches are gated through [`non_empty`] —
/// a body `txid` of `""` must fall through to the header, exactly like the
/// live-measured 408's empty `error_description` must fall through in
/// [`error_message`]. Without the gate, `Some("")` from the body would win
/// outright and discard a perfectly good `x-txid` header value.
fn extract_txid(body: &[u8], header_txid: Option<&str>) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| non_empty(v.get("txid").and_then(|t| t.as_str())).map(str::to_owned))
        .or_else(|| non_empty(header_txid).map(str::to_owned))
}

/// Pulls `error_description="..."` out of `WWW-Authenticate`.
/// Escapes (`\"`) inside the quoted-string are respected.
fn extract_error_description(header: &str) -> Option<String> {
    let start = header.find("error_description=\"")? + "error_description=\"".len();
    let rest = &header[start..];
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push(chars.next()?),
            '"' => return Some(out),
            _ => out.push(c),
        }
    }
    None // 닫는 따옴표가 없으면 신뢰하지 않는다
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::Pricing;

    #[test]
    fn ok_json_unwraps_envelope() {
        let r = decode::<serde_json::Value>(
            http::StatusCode::OK,
            Some("application/json"),
            br#"{"response":{"vin":"X"}}"#,
            Pricing::NotCharged,
            None,
            None,
        )
        .unwrap();
        assert_eq!(r.payload["vin"], serde_json::json!("X"));
    }

    #[test]
    fn html_gateway_502_is_api_error_not_panic() {
        let e = decode::<serde_json::Value>(
            http::StatusCode::BAD_GATEWAY,
            Some("text/html"),
            b"<html>502</html>",
            Pricing::NotCharged,
            None,
            None,
        )
        .err()
        .unwrap();
        assert!(matches!(e, crate::error::Error::Api { .. }));
    }

    #[test]
    fn status_408_is_vehicle_offline() {
        let e = decode::<serde_json::Value>(
            http::StatusCode::from_u16(408).unwrap(),
            Some("application/json"),
            b"{}",
            Pricing::data(),
            None,
            None,
        )
        .err()
        .unwrap();
        assert!(matches!(e, crate::error::Error::VehicleOffline { .. }));
    }

    #[test]
    fn non_json_200_is_decode_error() {
        let e = decode::<serde_json::Value>(
            http::StatusCode::OK,
            Some("application/pdf"),
            b"%PDF",
            Pricing::NotCharged,
            None,
            None,
        )
        .err()
        .unwrap();
        assert!(matches!(e, crate::error::Error::Decode { .. }));
    }

    // --- Additional edge cases beyond the brief ---

    #[test]
    fn missing_content_type_is_decode_error() {
        // A missing content-type on a 2xx response is not something Tesla's
        // API does; it is the signature of a captive portal / misconfigured
        // gateway. `None` must fail loudly, not be treated as JSON.
        let e = decode::<serde_json::Value>(
            http::StatusCode::OK,
            None,
            br#"{"response":{}}"#,
            Pricing::NotCharged,
            None,
            None,
        )
        .err()
        .unwrap();
        assert!(matches!(e, crate::error::Error::Decode { .. }));
    }

    #[test]
    fn json_with_charset_and_vendor_suffix_are_accepted() {
        // Case-insensitive substring match: real-world content-type headers
        // carry a charset parameter or a vendor `+json` suffix and both must
        // still pass the gate.
        let r1 = decode::<serde_json::Value>(
            http::StatusCode::OK,
            Some("application/json; charset=utf-8"),
            br#"{"response":1}"#,
            Pricing::NotCharged,
            None,
            None,
        )
        .unwrap();
        assert_eq!(r1.payload, serde_json::json!(1));

        let r2 = decode::<serde_json::Value>(
            http::StatusCode::OK,
            Some("APPLICATION/VND.API+JSON"),
            br#"{"response":2}"#,
            Pricing::NotCharged,
            None,
            None,
        )
        .unwrap();
        assert_eq!(r2.payload, serde_json::json!(2));
    }

    #[test]
    fn empty_body_200_json_is_decode_error_not_panic() {
        // An empty body is not valid JSON. Must not panic (no unwrap on an
        // empty slice) and must surface as a normal decode error.
        let e = decode::<serde_json::Value>(
            http::StatusCode::OK,
            Some("application/json"),
            b"",
            Pricing::NotCharged,
            None,
            None,
        )
        .err()
        .unwrap();
        assert!(matches!(e, crate::error::Error::Decode { .. }));
    }

    #[test]
    fn status_401_is_auth_error_and_redacts_the_echoed_credential() {
        // Fix round 1 (Minor 4): the original body (`{"error":"invalid_token"}`)
        // contained nothing sensitive, so it could not tell `Error::Auth(body_preview(...))`
        // apart from `Error::Auth(lossy.to_string())` (no redaction at all). A
        // realistic 401 body echoes the offending credential and identifiers —
        // this one carries both a bearer token and a VIN, so the test fails
        // if either bypasses redaction.
        let body = br#"{"error":"invalid_bearer_token","error_description":"Bearer eyJhbGciOiJIUzI1NiJ9.raw-secret-value.sig","vin":"5YJ3E1EA7KF000000"}"#;
        let e = decode::<serde_json::Value>(
            http::StatusCode::UNAUTHORIZED,
            Some("application/json"),
            body,
            Pricing::NotCharged,
            None,
            None,
        )
        .err()
        .unwrap();
        match e {
            crate::error::Error::Auth { message, .. } => {
                assert!(message.contains("[REDACTED]"), "got: {message}");
                assert!(message.contains("[VIN]"), "got: {message}");
                assert!(!message.contains("raw-secret-value"));
                assert!(!message.contains("eyJhbGciOiJIUzI1NiJ9"));
                assert!(!message.contains("5YJ3E1EA7KF000000"));
            }
            other => panic!("expected Auth, got {other:?}"),
        }
    }

    #[test]
    fn status_401_html_proxy_auth_page_is_still_auth_error() {
        // Minor 3: a proxy/gateway 401 commonly arrives as an HTML challenge
        // page, not JSON. The status-first gate must still classify it as
        // Auth regardless of content-type.
        let e = decode::<serde_json::Value>(
            http::StatusCode::UNAUTHORIZED,
            Some("text/html"),
            b"<html><body>401 Unauthorized</body></html>",
            Pricing::NotCharged,
            None,
            None,
        )
        .err()
        .unwrap();
        assert!(matches!(e, crate::error::Error::Auth { .. }));
    }

    /// Final fix wave (F3): the 401 arm must carry the endpoint's pricing
    /// into the error, so `billing()` can apply design §9.2's other-4xx row.
    /// A 401 on a paid endpoint is `Billed` — Tesla charged for the request
    /// that came back 401 — and the old `Error::Auth(String)` reported
    /// `NotBilled` because `decode` threw the pricing away right here.
    #[test]
    fn status_401_on_paid_endpoint_reports_billed() {
        use crate::pricing::Billing;
        let e = decode::<serde_json::Value>(
            http::StatusCode::UNAUTHORIZED,
            Some("application/json"),
            br#"{"error":"invalid_token"}"#,
            Pricing::data(),
            None,
            None,
        )
        .err()
        .unwrap();
        match &e {
            crate::error::Error::Auth { pricing, .. } => {
                assert_eq!(*pricing, Some(Pricing::data()));
            }
            other => panic!("expected Auth, got {other:?}"),
        }
        assert_eq!(e.billing(), Billing::Billed);

        // The free-endpoint 401 through the same arm stays NotBilled.
        let free = decode::<serde_json::Value>(
            http::StatusCode::UNAUTHORIZED,
            Some("application/json"),
            br#"{"error":"invalid_token"}"#,
            Pricing::NotCharged,
            None,
            None,
        )
        .err()
        .unwrap();
        assert_eq!(free.billing(), Billing::NotBilled);
    }

    #[test]
    fn status_408_html_gateway_timeout_is_still_vehicle_offline() {
        // Minor 3: the everyday case for a read-only client that never wakes
        // the car is an nginx/gateway 408 timeout page, typically HTML, not
        // Tesla's own JSON. Must still classify as VehicleOffline.
        let e = decode::<serde_json::Value>(
            http::StatusCode::from_u16(408).unwrap(),
            Some("text/html"),
            b"<html><body>408 Request Timeout</body></html>",
            Pricing::data(),
            None,
            None,
        )
        .err()
        .unwrap();
        assert!(matches!(e, crate::error::Error::VehicleOffline { .. }));
    }

    #[test]
    fn non_utf8_body_does_not_panic() {
        // A gateway/proxy can return arbitrary bytes. `String::from_utf8_lossy`
        // must be used, never `from_utf8(...).unwrap()`, or this panics.
        let bad_utf8: &[u8] = &[0xff, 0xfe, 0xfd];
        let e = decode::<serde_json::Value>(
            http::StatusCode::INTERNAL_SERVER_ERROR,
            Some("text/plain"),
            bad_utf8,
            Pricing::data(),
            None,
            None,
        )
        .err()
        .unwrap();
        assert!(matches!(e, crate::error::Error::Api { .. }));
    }

    #[test]
    fn valid_json_missing_response_key_is_decode_error() {
        // Valid JSON that simply isn't Tesla's envelope shape must still be
        // rejected, not silently coerced.
        let e = decode::<serde_json::Value>(
            http::StatusCode::OK,
            Some("application/json"),
            br#"{"not_response":{"vin":"X"}}"#,
            Pricing::NotCharged,
            None,
            None,
        )
        .err()
        .unwrap();
        assert!(matches!(e, crate::error::Error::Decode { .. }));
    }

    #[test]
    fn status_ge_400_wins_over_bad_content_type() {
        // Gate order: status >= 400 is resolved before the content-type
        // check, even when the content-type is also non-JSON. This must not
        // produce a Decode error for a >=400 response.
        let e = decode::<serde_json::Value>(
            http::StatusCode::from_u16(503).unwrap(),
            None,
            b"upstream connect error",
            Pricing::data(),
            None,
            None,
        )
        .err()
        .unwrap();
        assert!(matches!(e, crate::error::Error::Api { .. }));
    }

    // --- Fix round 1 (Important 2): api_error_body_preview_is_redacted_and_bounded ---
    // was a tautology — (1) `len() <= BODY_PREVIEW_MAX` is guaranteed by
    // `body_preview`'s truncation contract regardless of whether redaction
    // ran, and (2) a 1000-byte needle can never occur in a <=256-byte
    // haystack, so it was unconditionally true. Split, the way
    // `redaction.rs` itself converged (round 4): an exact `assert_eq!` for a
    // short secret that fits comfortably under the bound (proves redaction
    // ran, with nothing left to the length contract to hide behind), plus a
    // separate case with a body long enough to force real truncation.

    #[test]
    fn api_error_body_preview_is_exact_when_under_bound() {
        let e = decode::<serde_json::Value>(
            http::StatusCode::BAD_GATEWAY,
            Some("application/json"),
            br#"{"access_token":"SECRET_XYZ","note":"leaked"}"#,
            Pricing::NotCharged,
            None,
            None,
        )
        .err()
        .unwrap();
        match e {
            crate::error::Error::Api { body_preview, .. } => {
                // Exact equality: the only way this passes is if `token`'s
                // allowlisted sibling key `access_token` was actually
                // redacted, and nothing else was touched or truncated.
                assert_eq!(
                    body_preview,
                    r#"{"access_token":"[REDACTED]","note":"leaked"}"#
                );
            }
            other => panic!("expected Api, got {other:?}"),
        }
    }

    #[test]
    fn api_error_body_preview_redacts_then_truncates_long_body() {
        let long_note = "A".repeat(400);
        let body = format!(r#"{{"access_token":"SECRET_XYZ","note":"{long_note}"}}"#);
        let e = decode::<serde_json::Value>(
            http::StatusCode::BAD_GATEWAY,
            Some("application/json"),
            body.as_bytes(),
            Pricing::NotCharged,
            None,
            None,
        )
        .err()
        .unwrap();
        match e {
            crate::error::Error::Api { body_preview, .. } => {
                // The redacted-then-serialized JSON is ASCII and well over
                // the bound, so truncation must land at exactly the byte
                // limit -- not "coincidentally under it" (which is what let
                // the tautology hide).
                assert_eq!(body_preview.len(), BODY_PREVIEW_MAX);
                assert!(!body_preview.contains("SECRET_XYZ"));
                // Proves redact-then-truncate ordering: if truncation ran
                // first, "access_token" would still be followed by the raw
                // secret's leading bytes instead of "[REDACTED]".
                assert!(body_preview.starts_with(r#"{"access_token":"[REDACTED]","note":""#));
            }
            other => panic!("expected Api, got {other:?}"),
        }
    }

    // --- Fix round 1 (Important 1 follow-through): the same rigor for
    // Error::Decode::message, which previously bypassed body_preview
    // entirely (unbounded, and skipping its JSON-aware redaction pass).

    #[test]
    fn decode_message_for_bad_content_type_is_exact_when_under_bound() {
        let e = decode::<serde_json::Value>(
            http::StatusCode::OK,
            Some("application/pdf"),
            b"%PDF",
            Pricing::NotCharged,
            None,
            None,
        )
        .err()
        .unwrap();
        match e {
            crate::error::Error::Decode { message, .. } => {
                assert_eq!(message, "unexpected content-type: application/pdf");
            }
            other => panic!("expected Decode, got {other:?}"),
        }
    }

    #[test]
    fn decode_message_for_bad_content_type_is_bounded_when_header_is_huge() {
        // A hostile/misconfigured `Content-Type` header is attacker-chosen
        // text; nothing bounds its length before it reaches this function,
        // so `decode` must bound it, not merely redact it.
        let huge_ct = format!("application/x-{}", "a".repeat(400));
        let e = decode::<serde_json::Value>(
            http::StatusCode::OK,
            Some(&huge_ct),
            b"whatever",
            Pricing::NotCharged,
            None,
            None,
        )
        .err()
        .unwrap();
        match e {
            crate::error::Error::Decode { message, .. } => {
                assert_eq!(message.len(), BODY_PREVIEW_MAX);
                assert!(message.starts_with("unexpected content-type: application/x-a"));
            }
            other => panic!("expected Decode, got {other:?}"),
        }
    }

    // --- Fix round 2 (Important 1, still-open finding closed) -------------
    // `TypedU64` (a `token: u64` field given a string) is shared by every
    // `Category::Data` test below — it's the reviewer's own probe shape,
    // reused rather than reinvented so each test's meaning stays legible.
    #[derive(Debug, serde::Deserialize)]
    struct TypedU64 {
        #[allow(dead_code)]
        token: u64,
    }

    #[test]
    fn decode_parse_error_data_category_hides_body_value_reviewer_probe() {
        // The reviewer's exact probe: `{"response":{"token":"NA_a1b2c3d4e5f6g7"}}`
        // against `token: u64`. `NA_a1b2c3d4e5f6g7` matches none of
        // `redact_text`'s five families (no `Bearer `/`eyJ` prefix, no `=`,
        // no quoted-JSON destination-key syntax, its `_` splits the 17-char
        // VIN pattern's contiguous run, no coordinate shape) — fix round 1
        // left it surviving verbatim, merely capped at 256 bytes. Fix round
        // 2 removes the value from `message` entirely for `Category::Data`,
        // so there is nothing left to leak regardless of pattern coverage.
        let body = br#"{"response":{"token":"NA_a1b2c3d4e5f6g7"}}"#;

        // Independently derive the expected line/column so this test does
        // not merely re-assert whatever `decode` happens to compute.
        let independent_err = match serde_json::from_slice::<crate::model::Envelope<TypedU64>>(body)
        {
            Ok(_) => panic!("expected the malformed body to fail to parse"),
            Err(err) => err,
        };
        assert_eq!(independent_err.classify(), Category::Data);
        let expected = format!(
            "invalid data at line {} column {}",
            independent_err.line(),
            independent_err.column()
        );

        let e = decode::<TypedU64>(
            http::StatusCode::OK,
            Some("application/json"),
            body,
            Pricing::NotCharged,
            None,
            None,
        )
        .err()
        .unwrap();
        match e {
            crate::error::Error::Decode { message, .. } => {
                assert_eq!(message, expected);
                assert!(!message.contains("NA_a1b2c3d4e5f6g7"));
            }
            other => panic!("expected Decode, got {other:?}"),
        }
    }

    #[test]
    fn decode_parse_error_data_category_hides_pattern_matching_secrets_too() {
        // A Data-category value that *would* have matched a `redact_text`
        // pattern (Bearer/JWT here) is now excluded from `message` the same
        // way as one that wouldn't have — the fix isn't "redact harder", it
        // stopped depending on the pattern at all for this category.
        let body = br#"{"response":{"token":"Bearer eyJhbGciOiJIUzI1NiJ9.raw-secret-value.sig"}}"#;
        let e = decode::<TypedU64>(
            http::StatusCode::OK,
            Some("application/json"),
            body,
            Pricing::NotCharged,
            None,
            None,
        )
        .err()
        .unwrap();
        match e {
            crate::error::Error::Decode { message, .. } => {
                assert!(!message.contains("raw-secret-value"));
                assert!(!message.contains("eyJhbGciOiJIUzI1NiJ9"));
                assert!(!message.contains("Bearer"));
                assert!(message.starts_with("invalid data at line"));
            }
            other => panic!("expected Decode, got {other:?}"),
        }
    }

    #[test]
    fn decode_parse_error_syntax_category_keeps_grammar_detail_verbatim() {
        // A trailing comma is a `Category::Syntax` error — grammar-level,
        // never interpolates body content — so its full verbatim `Display`
        // (the useful "what's wrong with the grammar" detail) must survive,
        // not be collapsed to the value-free line/column form that only
        // `Category::Data` needs.
        let body = br#"{"response":{"vin":"X",}}"#;
        let independent_err = match serde_json::from_slice::<crate::model::Envelope<TypedU64>>(body)
        {
            Ok(_) => panic!("expected the malformed body to fail to parse"),
            Err(err) => err,
        };
        assert_eq!(independent_err.classify(), Category::Syntax);
        let expected_detail = independent_err.to_string();
        assert!(
            expected_detail.contains("trailing comma"),
            "expected a real grammar detail, got: {expected_detail}"
        );

        let e = decode::<TypedU64>(
            http::StatusCode::OK,
            Some("application/json"),
            body,
            Pricing::NotCharged,
            None,
            None,
        )
        .err()
        .unwrap();
        match e {
            crate::error::Error::Decode { message, .. } => {
                assert_eq!(message, expected_detail);
            }
            other => panic!("expected Decode, got {other:?}"),
        }
    }

    /// Final fix wave (F7): pin the partition the whole `Category::Data`
    /// fix rests on — that `Data` is the **only** category whose message
    /// interpolates body content.
    ///
    /// `decode_parse_error_syntax_category_keeps_grammar_detail_verbatim`
    /// above cannot pin this: it derives its expectation from
    /// `independent_err.to_string()`, i.e. from serde_json itself, so a
    /// future serde_json that started echoing input into a `Syntax` message
    /// would make `decode` leak while that test stayed green (both sides of
    /// its `assert_eq!` would move together). This test instead asserts
    /// against a body-derived token chosen so it cannot appear in a
    /// grammar-level message by coincidence, and that no `redact_text`
    /// family would rescue: no `eyJ`/`Bearer ` prefix, no `=`, not 17 chars,
    /// not coordinate-shaped, not under an allowlisted key. If serde_json
    /// ever widens what a Syntax message carries, this fails and the
    /// `matches!(err.classify(), Category::Data)` condition in `decode` must
    /// be widened with it.
    #[test]
    fn syntax_category_message_never_interpolates_body_content() {
        // A missing comma after `note` → Category::Syntax, with the
        // distinctive value sitting immediately before the error position
        // (so a serde that echoed nearby input would echo *this*). `token`
        // is given a valid `u64` on purpose: a type mismatch there would
        // raise a Data error first and never reach the grammar fault.
        let body = br#"{"response":{"token":1,"note":"ZZQXVDISTINCTIVEBODYTOKEN" "x":2}}"#;
        let needle = "ZZQXVDISTINCTIVEBODYTOKEN";

        // `unwrap_err()` is unavailable here: it needs `T: Debug`, and
        // `Envelope<T>` deliberately has no `Debug` (see `model`'s F10 note).
        let independent_err = match serde_json::from_slice::<crate::model::Envelope<TypedU64>>(body)
        {
            Ok(_) => panic!("expected the malformed body to fail to parse"),
            Err(err) => err,
        };
        assert_eq!(
            independent_err.classify(),
            Category::Syntax,
            "probe no longer produces a Syntax error; rewrite it"
        );

        let e = decode::<TypedU64>(
            http::StatusCode::OK,
            Some("application/json"),
            body,
            Pricing::NotCharged,
            None,
            None,
        )
        .err()
        .unwrap();
        match e {
            crate::error::Error::Decode { message, .. } => {
                assert!(
                    !message.contains(needle),
                    "a Syntax-category message interpolated body content — \
                     `decode`'s Category::Data-only guard is no longer sufficient: {message}"
                );
            }
            other => panic!("expected Decode, got {other:?}"),
        }
    }

    /// The Eof half of the same partition. An unterminated string is
    /// `Category::Eof`, and its message must likewise carry no body content.
    #[test]
    fn eof_category_message_never_interpolates_body_content() {
        // An unterminated string → Category::Eof, again with the distinctive
        // value at the error position.
        let body = br#"{"response":{"token":1,"note":"ZZQXVDISTINCTIVEBODYTOKEN"#;
        let needle = "ZZQXVDISTINCTIVEBODYTOKEN";

        // `unwrap_err()` is unavailable here: it needs `T: Debug`, and
        // `Envelope<T>` deliberately has no `Debug` (see `model`'s F10 note).
        let independent_err = match serde_json::from_slice::<crate::model::Envelope<TypedU64>>(body)
        {
            Ok(_) => panic!("expected the malformed body to fail to parse"),
            Err(err) => err,
        };
        assert_eq!(
            independent_err.classify(),
            Category::Eof,
            "probe no longer produces an Eof error; rewrite it"
        );

        let e = decode::<TypedU64>(
            http::StatusCode::OK,
            Some("application/json"),
            body,
            Pricing::NotCharged,
            None,
            None,
        )
        .err()
        .unwrap();
        match e {
            crate::error::Error::Decode { message, .. } => {
                assert!(
                    !message.contains(needle),
                    "an Eof-category message interpolated body content: {message}"
                );
            }
            other => panic!("expected Decode, got {other:?}"),
        }
    }

    // --- Fix round 2, residual test gap 1: the content-type message path
    // was never proven to redact, only to bound (`application/pdf` passes
    // identically with redaction removed). --------------------------------

    #[test]
    fn decode_message_for_bad_content_type_redacts_secret_shaped_header() {
        // "text/html" (no "json" substring, so the gate still rejects it)
        // with a Bearer/JWT-shaped fragment stitched into the header value —
        // a malformed or hostile proxy is not bound to send a clean MIME
        // type. The message is built from this same string, so it must be
        // scrubbed exactly like every other body/header-derived string.
        let hostile_ct = "text/html; Bearer eyJhbGciOiJIUzI1NiJ9.raw-secret-value.sig";
        assert!(!hostile_ct.to_ascii_lowercase().contains("json")); // sanity: still hits this gate
        let e = decode::<serde_json::Value>(
            http::StatusCode::OK,
            Some(hostile_ct),
            b"whatever",
            Pricing::NotCharged,
            None,
            None,
        )
        .err()
        .unwrap();
        match e {
            crate::error::Error::Decode { message, .. } => {
                assert!(message.contains("[REDACTED]"), "got: {message}");
                assert!(!message.contains("raw-secret-value"));
                assert!(!message.contains("eyJhbGciOiJIUzI1NiJ9"));
            }
            other => panic!("expected Decode, got {other:?}"),
        }
    }

    // --- Fix round 2, residual test gap 2: coord_keyed/coord_kv/coord_pair
    // — the patterns Task 8 rewrote most — were never exercised through
    // `decode` at all. ------------------------------------------------------

    #[test]
    fn decode_api_error_body_preview_redacts_drive_state_coordinates() {
        let body = br#"{"drive_state":{"latitude":37.5665,"longitude":126.9780}}"#;
        let e = decode::<serde_json::Value>(
            http::StatusCode::BAD_GATEWAY,
            Some("application/json"),
            body,
            Pricing::NotCharged,
            None,
            None,
        )
        .err()
        .unwrap();
        match e {
            crate::error::Error::Api { body_preview, .. } => {
                assert!(body_preview.contains("[COORD]"), "got: {body_preview}");
                assert!(!body_preview.contains("37.5665"));
                assert!(!body_preview.contains("126.978"));
            }
            other => panic!("expected Api, got {other:?}"),
        }
    }

    // --- A3 Task 3: error message maturation + retry_after plumbing -------

    use std::time::Duration;

    /// Measured E1 case A — the body's `error` is surfaced as the message.
    #[test]
    fn api_message_comes_from_body_error_field() {
        // Deviation from the brief's literal assertion (reported in
        // task-3-report.md): Tesla's real wording for this error is exactly
        // "invalid bearer token", which — once it becomes `message` and
        // passes through `body_preview` for redaction, same as every other
        // body-derived string in this module — collides with
        // `redaction.rs`'s already-reviewed `Bearer\s+[^\s",}\]]+` pattern
        // (it exists to strip a literal `Authorization: Bearer <token>`
        // value, not to special-case the English word "bearer"). The regex
        // has no way to distinguish the two, and weakening it to let this
        // one phrase through would reopen the credential leak it was
        // written to close. The assertion below checks for the correct,
        // security-preserving output instead of the raw phrase.
        let body = br#"{"error":"invalid bearer token"}"#;
        let e = decode::<serde_json::Value>(
            http::StatusCode::from_u16(403).unwrap(),
            Some("application/json"),
            body,
            Pricing::NotCharged,
            None,
            None,
        )
        .err()
        .unwrap();
        assert!(e.to_string().contains("invalid [REDACTED]"), "실제: {e}");
    }

    /// Measured E1 case B — when the body is empty, the error_description in
    /// WWW-Authenticate becomes the message. A parser that reads only the body comes away
    /// empty-handed here.
    #[test]
    fn api_message_falls_back_to_www_authenticate() {
        let e = decode::<serde_json::Value>(
            http::StatusCode::from_u16(403).unwrap(),
            Some("text/html"),
            b"",
            Pricing::NotCharged,
            None,
            Some(r#"Bearer realm="Doorkeeper", error="invalid_token", error_description="The access token is invalid""#),
        )
        .err().unwrap();
        assert!(
            e.to_string().contains("The access token is invalid"),
            "실제: {e}"
        );
    }

    /// When neither is present, it falls back to canonical_reason.
    #[test]
    fn api_message_falls_back_to_canonical_reason() {
        let e = decode::<serde_json::Value>(
            http::StatusCode::from_u16(502).unwrap(),
            Some("text/html"),
            b"",
            Pricing::NotCharged,
            None,
            None,
        )
        .err()
        .unwrap();
        assert!(e.to_string().contains("Bad Gateway"), "실제: {e}");
    }

    /// Mutation-test addition (M1 strengthening, not in the brief): the two
    /// pinned priority tests above each supply only *one* signal (body-only,
    /// header-only), so swapping `error_message`'s priority order produces
    /// the identical result in both — the mutation survives them. This test
    /// supplies both signals at once so the two priorities actually
    /// disagree, which is the only way to pin which one wins.
    #[test]
    fn api_message_prefers_body_error_over_www_authenticate_when_both_present() {
        let body = br#"{"error":"body wins"}"#;
        let e = decode::<serde_json::Value>(
            http::StatusCode::from_u16(403).unwrap(),
            Some("application/json"),
            body,
            Pricing::NotCharged,
            None,
            Some(r#"Bearer error_description="header loses""#),
        )
        .err()
        .unwrap();
        assert!(e.to_string().contains("body wins"), "실제: {e}");
        assert!(!e.to_string().contains("header loses"), "실제: {e}");
    }

    /// 401 + text/html + 0 bytes does not leak out as Decode (this prevents billing
    /// misclassification). Pins the contract that the status check comes before the
    /// content-type check.
    #[test]
    fn empty_html_401_is_auth_not_decode() {
        let e = decode::<serde_json::Value>(
            http::StatusCode::from_u16(401).unwrap(),
            Some("text/html"),
            b"",
            Pricing::data(),
            None,
            None,
        )
        .err()
        .unwrap();
        assert!(matches!(e, Error::Auth { .. }), "실제: {e:?}");
        assert!(matches!(e.billing(), crate::pricing::Billing::Billed));
    }

    /// retry_after is passed through to Error::Api and the 120-second cap is applied.
    #[test]
    fn retry_after_is_carried_and_capped() {
        let e = decode::<serde_json::Value>(
            http::StatusCode::from_u16(429).unwrap(),
            Some("application/json"),
            b"{}",
            Pricing::NotCharged,
            Some(Duration::from_secs(600)),
            None,
        )
        .err()
        .unwrap();
        assert_eq!(e.retry_after(), Some(Duration::from_secs(120)));
    }

    /// Fix round 1 (Important): `Decoded`'s hand-written `Debug` must not
    /// print `Pagination`'s field values — only whether one is present.
    /// Distinctive, easy-to-spot numbers in every field are the only way a
    /// rule forbidding "prints the values" can be shown to fail *only* when
    /// it actually does: a `None` pagination or an all-default `Pagination`
    /// would pass this test even with the plain `#[derive(Debug)]` this impl
    /// replaces (nothing distinctive to leak), which is why none of those
    /// are used here.
    #[test]
    fn pagination_debug_never_prints_field_values() {
        let d = Decoded {
            payload: "irrelevant",
            pagination: Some(Pagination {
                previous: Some(918_273),
                next: Some(645_129),
                current: Some(837_465),
                per_page: Some(192_837),
                count: Some(564_738),
                pages: Some(291_837),
            }),
        };
        let out = format!("{d:?}");
        for needle in ["918273", "645129", "837465", "192837", "564738", "291837"] {
            assert!(!out.contains(needle), "실제: {out}");
        }
        // The presence signal itself must still survive — this isn't "print
        // nothing", it's "print whether one arrived, never its contents".
        assert!(out.contains("true"), "실제: {out}");
        assert!(out.contains("irrelevant"), "실제: {out}");
    }

    // --- A4 Task 3: empty error_description skipped + txid preserved -------

    /// An empty error_description must not win as the message (the measured 408 gives "").
    /// In this fixture error is empty too, so the fallback chain **actually runs** —
    /// with `FIXTURE_408_OFFLINE` alone, error wins as first priority and the test is
    /// vacuous.
    #[test]
    fn empty_error_is_skipped_and_description_wins() {
        let e = decode::<serde_json::Value>(
            http::StatusCode::from_u16(400).unwrap(),
            Some("application/json"),
            crate::test_support::FIXTURE_ERR_EMPTY_ERROR.as_bytes(),
            Pricing::NotCharged,
            None,
            None,
        )
        .unwrap_err();
        assert!(e.to_string().contains("could not be reached"), "실제: {e}");
    }

    /// When both are empty, it falls back to canonical_reason.
    #[test]
    fn both_empty_falls_through_to_canonical_reason() {
        let body = br#"{"response":null,"error":"","error_description":""}"#;
        let e = decode::<serde_json::Value>(
            http::StatusCode::from_u16(502).unwrap(),
            Some("application/json"),
            body,
            Pricing::NotCharged,
            None,
            None,
        )
        .unwrap_err();
        assert!(e.to_string().contains("Bad Gateway"), "실제: {e}");
    }

    /// The txid of a 408 is carried on VehicleOffline.
    /// If it were carried only on Error::Api, the txid would be lost in **the very
    /// response we measured**.
    #[test]
    fn txid_survives_on_vehicle_offline() {
        let e = decode::<serde_json::Value>(
            http::StatusCode::from_u16(408).unwrap(),
            Some("application/json"),
            crate::test_support::FIXTURE_408_OFFLINE.as_bytes(),
            Pricing::data(),
            None,
            None,
        )
        .unwrap_err();
        match e {
            Error::VehicleOffline { txid, .. } => {
                assert_eq!(txid.as_deref(), Some("432e7faaa94cf732bdda8f82f2184eea"));
            }
            other => panic!("VehicleOffline 이어야 한다: {other:?}"),
        }
    }

    /// When the body has no txid, it falls back to the x-txid header.
    #[test]
    fn txid_falls_back_to_the_header() {
        let e = decode_with_txid_header::<serde_json::Value>(
            http::StatusCode::from_u16(500).unwrap(),
            Some("application/json"),
            br#"{"error":"boom"}"#,
            Pricing::NotCharged,
            None,
            None,
            Some("hdr-txid-123"),
        )
        .unwrap_err();
        match e {
            Error::Api { txid, .. } => assert_eq!(txid.as_deref(), Some("hdr-txid-123")),
            other => panic!("Api 여야 한다: {other:?}"),
        }
    }

    /// Mutation-test strengthening (M4, not in the brief): the two priority
    /// tests above (`txid_survives_on_vehicle_offline`, body-only;
    /// `txid_falls_back_to_the_header`, header-only) each supply only *one*
    /// signal, so swapping `extract_txid`'s fallback order produces the
    /// identical result in both — the mutation survives them (verified:
    /// injecting the swap locally left both green). This test supplies both
    /// a body `txid` and a header `x-txid` with different values, so the two
    /// priorities actually disagree — the only way to pin which one wins.
    #[test]
    fn txid_prefers_body_over_header_when_both_present() {
        let e = decode_with_txid_header::<serde_json::Value>(
            http::StatusCode::from_u16(500).unwrap(),
            Some("application/json"),
            br#"{"error":"boom","txid":"body-txid-wins"}"#,
            Pricing::NotCharged,
            None,
            None,
            Some("header-txid-loses"),
        )
        .unwrap_err();
        match e {
            Error::Api { txid, .. } => assert_eq!(txid.as_deref(), Some("body-txid-wins")),
            other => panic!("Api 여야 한다: {other:?}"),
        }
    }

    /// Final fix wave item 1: the body `txid` branch used to accept
    /// `Some("")` unconditionally, short-circuiting the header fallback
    /// before `non_empty` ever ran on it — only the header branch was
    /// gated. The live-measured 408 already proves Tesla emits empty-string
    /// values in this key family (`error_description: ""`); this asserts
    /// the same tolerance one level down, for `txid`.
    #[test]
    fn txid_falls_back_to_the_header_when_body_txid_is_empty() {
        let e = decode_with_txid_header::<serde_json::Value>(
            http::StatusCode::from_u16(500).unwrap(),
            Some("application/json"),
            br#"{"error":"boom","txid":""}"#,
            Pricing::NotCharged,
            None,
            None,
            Some("432e7faaa94cf732bdda8f82f2184eea"),
        )
        .unwrap_err();
        match e {
            Error::Api { txid, .. } => {
                assert_eq!(txid.as_deref(), Some("432e7faaa94cf732bdda8f82f2184eea"));
            }
            other => panic!("Api 여야 한다: {other:?}"),
        }
    }

    /// Mutation-test strengthening (M3, not in the brief): `txid_is_not_redacted`
    /// below uses the real 408 fixture's txid, a 32-char hex string that
    /// happens to match none of `redact_text`'s five families (not 17 chars,
    /// no Bearer/JWT prefix, no `=`, not coordinate-shaped) — so a mutation
    /// that *did* route `txid` through `body_preview` would survive that test
    /// undetected (verified: injecting the mutation locally left it green).
    /// This test instead uses a VIN-shaped txid, a value redaction actively
    /// rewrites to `[VIN]` everywhere else in this crate, so a redaction leak
    /// on this field is unmissable.
    ///
    /// **Scope, corrected in fix round 1**: status 500 here reaches only
    /// the `Error::Api` arm of `decode`'s `>= 400` match. It says nothing
    /// about the `Error::VehicleOffline` (408) arm, which is a *separate*
    /// construction site with its own `txid` wiring. Round 1 review found
    /// that arm actually did wrap `txid` in `body_preview` — a real
    /// redaction bug on exactly the response this field was measured
    /// against — and this test could not have caught it, because it never
    /// executes that arm. See `txid_survives_verbatim_on_vehicle_offline_too`
    /// below for the branch this one does not cover.
    #[test]
    fn txid_survives_verbatim_even_when_it_is_vin_shaped() {
        let body = br#"{"error":"boom","txid":"5YJ3E1EA1JF000001"}"#;
        let e = decode::<serde_json::Value>(
            http::StatusCode::from_u16(500).unwrap(),
            Some("application/json"),
            body,
            Pricing::NotCharged,
            None,
            None,
        )
        .unwrap_err();
        match &e {
            Error::Api { txid, .. } => {
                assert_eq!(txid.as_deref(), Some("5YJ3E1EA1JF000001"));
            }
            other => panic!("Api 여야 한다: {other:?}"),
        }
        assert!(
            format!("{e:?}").contains("5YJ3E1EA1JF000001"),
            "VIN-shaped txid must survive verbatim, not be redacted: {e:?}"
        );
    }

    /// Fix round 1 (Critical fix): branch-specific counterpart to
    /// `txid_survives_verbatim_even_when_it_is_vin_shaped` above, which only
    /// ever reaches the `Error::Api` arm (status 500) and so could not have
    /// caught a redaction leak that lived on the `Error::VehicleOffline`
    /// (408) arm alone. That is exactly what happened: the 408 arm wrapped
    /// `txid` in `body_preview` while the `Api` arm did not, and no existing
    /// test exercised a 408 with a secret-shaped txid to expose it
    /// (`FIXTURE_408_OFFLINE`'s real txid is 32 hex chars, which matches none
    /// of `redact_text`'s five families and so is a no-op under either
    /// version of the code). This test drives status 408 specifically, with
    /// both a VIN-shaped and a Bearer/JWT-shaped txid — two shapes
    /// `redact_text` actively rewrites elsewhere in this crate — and asserts
    /// each survives verbatim on `Error::VehicleOffline`.
    #[test]
    fn txid_survives_verbatim_on_vehicle_offline_too() {
        let vin_body = br#"{"error":"boom","txid":"5YJ3E1EA1JF000001"}"#;
        let e = decode::<serde_json::Value>(
            http::StatusCode::from_u16(408).unwrap(),
            Some("application/json"),
            vin_body,
            Pricing::data(),
            None,
            None,
        )
        .unwrap_err();
        match &e {
            Error::VehicleOffline { txid, .. } => {
                assert_eq!(txid.as_deref(), Some("5YJ3E1EA1JF000001"));
            }
            other => panic!("VehicleOffline 이어야 한다: {other:?}"),
        }
        assert!(
            format!("{e:?}").contains("5YJ3E1EA1JF000001"),
            "VIN-shaped txid on the 408/VehicleOffline arm must survive verbatim: {e:?}"
        );

        let bearer_body =
            br#"{"error":"boom","txid":"Bearer eyJhbGciOiJIUzI1NiJ9.raw-secret-value.sig"}"#;
        let e2 = decode::<serde_json::Value>(
            http::StatusCode::from_u16(408).unwrap(),
            Some("application/json"),
            bearer_body,
            Pricing::data(),
            None,
            None,
        )
        .unwrap_err();
        match &e2 {
            Error::VehicleOffline { txid, .. } => {
                assert_eq!(
                    txid.as_deref(),
                    Some("Bearer eyJhbGciOiJIUzI1NiJ9.raw-secret-value.sig")
                );
            }
            other => panic!("VehicleOffline 이어야 한다: {other:?}"),
        }
        assert!(
            format!("{e2:?}").contains("raw-secret-value"),
            "Bearer/JWT-shaped txid on the 408/VehicleOffline arm must survive verbatim: {e2:?}"
        );
    }

    /// The txid is not redacted — it is needed for Tesla support inquiries.
    #[test]
    fn txid_is_not_redacted() {
        let e = decode::<serde_json::Value>(
            http::StatusCode::from_u16(408).unwrap(),
            Some("application/json"),
            crate::test_support::FIXTURE_408_OFFLINE.as_bytes(),
            Pricing::data(),
            None,
            None,
        )
        .unwrap_err();
        assert!(
            format!("{e:?}").contains("432e7faa"),
            "txid 가 사라졌다: {e:?}"
        );
    }

    /// M5 killer: `txid` must exist only as a field and is not exposed in the `Display`
    /// template of `#[error(...)]` (it is a correlation identifier, not a human-readable
    /// diagnostic message). A mutation that slips `{txid}` into
    /// `#[error("vehicle offline (408)")]`/`#[error("HTTP {status}: {message}")]` is killed
    /// only when this test exists — `Debug` (`txid_is_not_redacted`) does not inspect
    /// `Display`, so it cannot catch that mutation.
    #[test]
    fn txid_never_appears_in_display_message() {
        let offline = decode::<serde_json::Value>(
            http::StatusCode::from_u16(408).unwrap(),
            Some("application/json"),
            crate::test_support::FIXTURE_408_OFFLINE.as_bytes(),
            Pricing::data(),
            None,
            None,
        )
        .unwrap_err();
        assert!(
            !offline.to_string().contains("432e7faa"),
            "txid leaked into VehicleOffline's Display: {offline}"
        );

        let api = decode_with_txid_header::<serde_json::Value>(
            http::StatusCode::from_u16(500).unwrap(),
            Some("application/json"),
            br#"{"error":"boom"}"#,
            Pricing::NotCharged,
            None,
            None,
            Some("hdr-txid-123"),
        )
        .unwrap_err();
        assert!(
            !api.to_string().contains("hdr-txid-123"),
            "txid leaked into Api's Display: {api}"
        );
    }
}

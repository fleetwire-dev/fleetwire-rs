//! A single HTTP GET round trip. It **knows nothing** about retries, region resolution or token
//! refresh — the layer above it (`client`) adds those.

use std::time::{Duration, Instant};

use crate::net::{ReadError, Stage, read_capped};

use crate::error::Error;
use crate::pricing::Pricing;
use crate::redaction::body_preview;

/// The response body cap. When it is exceeded, reading is cut off **while accumulating**.
pub(crate) const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// The error message length cap (the same value as A1 `decode` uses).
const MSG_MAX: usize = 512;

/// The result of a single round trip. Interpretation is done by `decode`.
pub(crate) struct RawResponse {
    pub status: http::StatusCode,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
    pub retry_after: Option<Duration>,
    pub www_authenticate: Option<String>,
    /// The `x-txid` response header, if any (A4 Task 3). `decode`'s header
    /// fallback for [`Error::Api`]/[`Error::VehicleOffline`]'s `txid` field
    /// when the body carries none.
    pub x_txid: Option<String>,
    /// `(limit, remaining, reset_raw)` parsed from the
    /// `ratelimit-device-data-per-vehicle-{limit,remaining,reset}` headers
    /// (A4 Task 10, measured 2026-08-03).
    ///
    /// **`Some` only when all three headers are present and all three
    /// parse as `u64`.** A partial set (e.g. two of three present, or one
    /// present but not numeric) yields `None` — inventing a value from a
    /// partial read would be worse than reporting nothing.
    pub rate_limit: Option<(u64, u64, u64)>,
}

/// A hand-written `Debug` — `body` is the original content as is (raw bytes before redaction).
/// This response may also be reused by the token refresh parser (REQ-18, see the `redaction.rs`
/// module documentation), so with `#[derive(Debug)]` an access_token/refresh_token could be
/// printed as is (even if only in byte-array form). It shows only the length and never prints
/// the body.
impl std::fmt::Debug for RawResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawResponse")
            .field("status", &self.status)
            .field("content_type", &self.content_type)
            .field("body_len", &self.body.len())
            .field("retry_after", &self.retry_after)
            .field("www_authenticate", &self.www_authenticate)
            .field("x_txid", &self.x_txid)
            .field("rate_limit", &self.rate_limit)
            .finish()
    }
}

/// Parses `Retry-After` — both delta-seconds and HTTP-date (RFC 9110).
///
/// If HTTP-date were left out, the result would silently become `None` and the cap would be
/// meaningless. A time in the past is folded to `ZERO`, not to a negative value.
pub(crate) fn parse_retry_after(raw: &str) -> Option<Duration> {
    let raw = raw.trim();
    if let Ok(secs) = raw.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    let when = httpdate::parse_http_date(raw).ok()?;
    Some(
        when.duration_since(std::time::SystemTime::now())
            .unwrap_or(Duration::ZERO),
    )
}

/// Wired into production code by Task 6: [`crate::Client::send`] is its only
/// non-test caller.
///
/// This is a synchronous round trip (stage 1 of removing tokio). One `request_timeout` is
/// applied to one request, and the body is read by [`crate::net::read_capped`] under the same
/// deadline and cap. The billing stage of an error is decided by [`crate::net::classify`]
/// (only connection refused and the like are NotSent; the rest are Unknown).
pub(crate) fn execute_get(
    agent: &ureq::Agent,
    url: crate::Url,
    bearer: &str,
    pricing: Pricing,
    request_timeout: Duration,
) -> Result<RawResponse, Error> {
    // 마감은 보내기 전에 잡습니다. ureq 의 전체 마감과 같은 시각이고, 본문 읽기는 이 시각을 직접 셉니다.
    let deadline = Instant::now()
        .checked_add(request_timeout)
        .unwrap_or_else(Instant::now);
    let sent = agent
        .get(url.as_str())
        .header("authorization", &format!("Bearer {bearer}"))
        .call();

    let mut resp = match sent {
        Ok(r) => r,
        Err(e) => {
            let message = body_preview(&crate::net::redact_ureq_error(&e), MSG_MAX);
            return Err(match crate::net::classify(&e) {
                Stage::NotSent => Error::TransportNotSent { message, pricing },
                Stage::Unknown => Error::TransportUnknown { message, pricing },
            });
        }
    };

    let status = resp.status();
    let header = |k: &str| {
        resp.headers()
            .get(k)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let content_type = header("content-type");
    let www_authenticate = header("www-authenticate");
    let retry_after = header("retry-after").as_deref().and_then(parse_retry_after);
    // A4 Task 3: same `header()` closure as `www-authenticate` — the body is
    // the primary source (see `decode::extract_txid`), this is only the
    // fallback for a response whose txid never made it into the JSON body.
    let x_txid = header("x-txid");
    // A4 Task 10: same `header()` closure again. All-or-nothing — see
    // `RawResponse::rate_limit`'s doc for why a partial set is discarded
    // rather than partially trusted.
    let num = |k: &str| header(k).and_then(|v| v.trim().parse::<u64>().ok());
    let rate_limit = match (
        num("ratelimit-device-data-per-vehicle-limit"),
        num("ratelimit-device-data-per-vehicle-remaining"),
        num("ratelimit-device-data-per-vehicle-reset"),
    ) {
        (Some(l), Some(r), Some(rs)) => Some((l, r, rs)),
        _ => None,
    };

    // 3xx 는 decode 의 `>=400` 게이트에 안 걸려 content-type 검사로 내려가고
    // Error::Decode 가 된다. redirect 를 따라가지 않기로 했으므로(design §4.6)
    // 여기서 Error::Api 로 만들어 시끄럽게 실패시킨다. body 를 읽기 전에 막는다.
    if status.is_redirection() {
        return Err(Error::Api {
            status,
            message: body_preview(
                &format!("unexpected redirect ({status}); fleetwire does not follow redirects"),
                MSG_MAX,
            ),
            retry_after,
            body_preview: String::new(),
            pricing,
            txid: x_txid,
        });
    }

    // buffer-then-check 금지 — `read_capped` 가 누적하며 상한과 마감을 검사합니다.
    let body = match read_capped(
        &mut resp.body_mut().as_reader(),
        MAX_RESPONSE_BYTES,
        deadline,
    ) {
        Ok(body) => body,
        Err(ReadError::CapExceeded) => {
            return Err(Error::TransportUnknown {
                message: body_preview(
                    &format!("response too large (cap {MAX_RESPONSE_BYTES} bytes)"),
                    MSG_MAX,
                ),
                pricing,
            });
        }
        Err(ReadError::Deadline) => {
            // 서버가 이미 처리했을 수 있습니다 → Unknown(재시도 금지).
            return Err(Error::TransportUnknown {
                message: body_preview("request timed out", MSG_MAX),
                pricing,
            });
        }
        Err(ReadError::Io(e)) => {
            return Err(Error::TransportUnknown {
                message: body_preview(&crate::redaction::redact_transport_error(&e), MSG_MAX),
                pricing,
            });
        }
    };

    Ok(RawResponse {
        status,
        content_type,
        body,
        retry_after,
        www_authenticate,
        x_txid,
        rate_limit,
    })
}

#[cfg(test)]
mod tests;

//! The ureq backend of `TokenRefresher` — **the only POST in the crate**.
//!
//! ## Why `fleet-auth…` is hardcoded (why OIDC discovery is not followed)
//!
//! Measured on 2026-08-01: `https://fleet-auth.prd.vn.cloud.tesla.com/oauth2/v3/.well-known/
//! openid-configuration` advertises `token_endpoint = https://auth.tesla.com/oauth2/v3/token`
//! and `refresh_token` is **absent** from `grant_types_supported`. Yet an actual refresh
//! succeeds only when sent to `fleet-auth…`. Moreover, the `iss` of a real token is
//! `https://auth.tesla.com/oauth2/v3/nts`, which **does not match** the `issuer` of discovery.
//!
//! → **Discovery is not the authority for this endpoint.** Introducing discovery or adding
//!   strict issuer validation for the sake of "correctness" **breaks refresh and rejects our
//!   own tokens**.

use std::time::{Duration, Instant};

use crate::auth::{Refreshed, TokenRefresher};
use crate::error::Error;
use crate::net::{ReadError, read_capped, redact_ureq_error};
use crate::redaction::body_preview;

/// The timeout of the refresh request.
///
/// ⚠️ It **must be shorter** than the `state_deadline` (10 seconds) of `TokenManager`. During
/// a refresh the other callers wait up to `state_deadline` for the `refreshing` flag of the
/// state cell to be lowered, so if this exceeded 10 seconds, sibling callers would receive a
/// `token state lock:` error in the middle of a slow but successful refresh (design §9.1).
pub const REFRESH_TIMEOUT: Duration = Duration::from_secs(8);

const TOKEN_URL: &str = "https://fleet-auth.prd.vn.cloud.tesla.com/oauth2/v3/token";
// 메시지 길이 예산은 부모 모듈 한 곳에 있다 — 여기 다시 적으면 두 값이 갈라진다.
use super::MSG_MAX;

/// The response JSON. If `expires_in` is missing it is set to 0.
///
/// ⚠️ Correction from review round 1 (minor #4): a **lower bound** on this value is not A2's
/// policy. All A2 clamps is the `MAX_TOKEN_LIFETIME` upper bound in the [`crate::auth`] module
/// docs, and the lower bound (A2 carry-forward #1) does not exist yet — Task 5 is to
/// introduce it on `TokenManager` as `MIN_REFRESH_INTERVAL`. Until then `expires_in: 0` (or
/// anything under 60 seconds) flows in as is, with no limit applied.
#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    #[serde(default)]
    expires_in: u64,
}

/// The ureq backend of [`TokenRefresher`].
///
/// # ⚠️ No retries
///
/// When [`refresh`](TokenRefresher::refresh) fails (timeouts included) it **never** retries
/// on its own, and no consumer code wrapping this type may retry either. The server may
/// already have rotated the refresh_token chain with only the response lost — in that case
/// the refresh_token we hold is already dead, and a retry attempts the rotation again with
/// that dead token and definitively invalidates even a chain that might still have been
/// alive. The only way to recover is a full re-OAuth. `HttpRefresher` is `pub` and
/// re-exported at the crate root, so consumer code that wraps this type in a general-purpose
/// retry decorator can break this contract — do not wrap it.
pub struct HttpRefresher {
    agent: ureq::Agent,
    client_id: String,
    token_url: crate::Url,
    timeout: Duration,
}

impl HttpRefresher {
    /// `client_id` is the OAuth client_id of a public client — the client_secret is never
    /// sent (design §4).
    pub fn new(client_id: String) -> Self {
        Self {
            // 리다이렉트를 따라가면 자격 증명이 다른 호스트로 갈 수 있습니다(design §4.6) — `net::agent` 가 막습니다.
            agent: crate::net::agent(REFRESH_TIMEOUT),
            client_id,
            token_url: TOKEN_URL.parse().expect("const URL parses"),
            timeout: REFRESH_TIMEOUT,
        }
    }

    /// Points the refresher at a mock server in tests.
    pub fn with_token_url(mut self, url: crate::Url) -> Self {
        self.token_url = url;
        self
    }

    /// Shortens the timeout in tests.
    pub fn with_timeout(mut self, d: Duration) -> Self {
        self.timeout = d;
        self.agent = crate::net::agent(d);
        self
    }
}

/// ⚠️ Tokens and secrets are never printed.
impl core::fmt::Debug for HttpRefresher {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("HttpRefresher")
            .field("token_url", &self.token_url.as_str())
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl TokenRefresher for HttpRefresher {
    fn refresh(&self, refresh_token: &str) -> Result<Refreshed, Error> {
        let form = [
            ("grant_type", "refresh_token"),
            ("client_id", self.client_id.as_str()),
            ("refresh_token", refresh_token),
        ];
        // 보내기와 본문 읽기를 **하나의** 마감으로 묶습니다(review round 1 critical #1). 헤더는 빨리 오고
        // 본문만 멈추는 응답이어도 `self.timeout` 안에 끝나야 `state_deadline`(10초) 예산을 지킵니다.
        let deadline = Instant::now()
            .checked_add(self.timeout)
            .unwrap_or_else(Instant::now);

        // 이 크레이트에서 POST 가 허용되는 **유일한 지점**입니다.
        // clippy.toml 이 나머지 전부를 막습니다(design §3).
        #[allow(clippy::disallowed_methods)]
        let sent = self.agent.post(self.token_url.as_str()).send_form(form);

        // ⚠️ 서버가 회전을 완료했는데 응답만 유실됐을 수 있습니다(헤더만 오고 본문이 멈춘 경우 포함).
        // 그 경우 우리가 가진 refresh_token 은 이미 무효이고 복구는 재-OAuth 뿐입니다.
        // **재시도하지 않습니다** — 재시도는 악화입니다.
        let timed_out = || Error::Auth {
            message: body_preview("token refresh timed out", MSG_MAX),
            pricing: None,
        };
        let mut resp = match sent {
            Ok(r) => r,
            Err(ureq::Error::Timeout(_)) => return Err(timed_out()),
            Err(e) => {
                return Err(Error::Auth {
                    message: body_preview(&redact_ureq_error(&e), MSG_MAX),
                    pricing: None,
                });
            }
        };
        let status = resp.status();
        let body = match read_capped(
            &mut resp.body_mut().as_reader(),
            crate::transport::MAX_RESPONSE_BYTES,
            deadline,
        ) {
            Ok(body) => body,
            Err(ReadError::Deadline) => return Err(timed_out()),
            Err(ReadError::CapExceeded) => {
                return Err(Error::Auth {
                    message: body_preview(
                        &format!(
                            "token refresh response too large (cap {} bytes)",
                            crate::transport::MAX_RESPONSE_BYTES
                        ),
                        MSG_MAX,
                    ),
                    pricing: None,
                });
            }
            Err(ReadError::Io(e)) => {
                return Err(Error::Auth {
                    message: body_preview(&crate::redaction::redact_transport_error(&e), MSG_MAX),
                    pricing: None,
                });
            }
        };

        if !status.is_success() {
            return Err(Error::Auth {
                message: body_preview(
                    &format!(
                        "token refresh failed: HTTP {status}: {}",
                        String::from_utf8_lossy(&body)
                    ),
                    MSG_MAX,
                ),
                pricing: None,
            });
        }

        let parsed: TokenResponse = serde_json::from_slice(&body).map_err(|e| Error::Auth {
            message: body_preview(&format!("token refresh response: {e}"), MSG_MAX),
            pricing: None,
        })?;

        Ok(Refreshed::new(
            parsed.access_token,
            parsed.refresh_token,
            parsed.expires_in,
        ))
    }
}

#[cfg(test)]
mod tests;

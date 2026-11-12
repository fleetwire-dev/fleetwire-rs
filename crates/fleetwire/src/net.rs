//! Common parts of the synchronous ureq transport. Agent configuration, body reading and error
//! stage classification are kept in one place.

use std::io::{self, Read};
use std::time::{Duration, Instant};

/// The connect deadline. It must be set for a stalled connect to be reported as
/// `Timeout::Connect` (not sent).
pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The only place in this crate that creates an Agent (gate G2 inspects this function's
/// configuration).
///
/// It does not follow redirects (so that credentials do not go to another host), ignores
/// environment-variable proxies, and does not turn status codes into errors (3xx, 4xx and 5xx
/// are interpreted by `decode`).
pub(crate) fn agent(global: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(CONNECT_TIMEOUT.min(global)))
        .timeout_global(Some(global))
        .max_redirects(0)
        .http_status_as_error(false)
        .proxy(None)
        .max_idle_connections_per_host(2)
        .user_agent(concat!("fleetwire/", env!("CARGO_PKG_VERSION")))
        .build()
        .into()
}

/// The reason `read_capped` stopped.
#[derive(Debug)]
pub(crate) enum ReadError {
    CapExceeded,
    Deadline,
    Io(io::Error),
}

/// Reads a body under a size cap and a deadline. Exactly `max` bytes passes.
///
/// ureq's `timeout_global` waits another second on every read even after the deadline (spec §0
/// C1), so the deadline is counted here directly. The overrun is at most one read (up to about
/// 1 second). The cap is checked while accumulating, and once it is exceeded the rest of the
/// body is not read (buffer-then-check is forbidden).
pub(crate) fn read_capped(
    reader: &mut impl Read,
    max: usize,
    deadline: Instant,
) -> Result<Vec<u8>, ReadError> {
    let mut body = Vec::new();
    let mut buf = [0u8; 16 * 1024];
    loop {
        if Instant::now() >= deadline {
            return Err(ReadError::Deadline);
        }
        let n = match reader.read(&mut buf) {
            Ok(0) => return Ok(body),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            // ureq 는 마감이 지나면 읽기를 `Timeout` I/O 오류로 끝냅니다. 마감 뒤의 오류는 「마감」으로 봅니다.
            Err(_) if Instant::now() >= deadline => return Err(ReadError::Deadline),
            Err(e) => return Err(ReadError::Io(e)),
        };
        let chunk = buf.get(..n).unwrap_or_default();
        if body.len().saturating_add(chunk.len()) > max {
            return Err(ReadError::CapExceeded);
        }
        body.extend_from_slice(chunk);
    }
}

/// The verdict on whether a request reached the server (the input to billing and retry
/// classification).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stage {
    /// The request was definitely not sent.
    NotSent,
    /// It is not known whether the server processed the request.
    Unknown,
}

/// Sorts ureq 3.3 errors into stages (spec §4, r1.1 table). Anything not known is Unknown.
///
/// ⚠️ A DNS failure (`Io(Uncategorized)`) and a TLS failure (`Io(InvalidData)`) are Unknown
/// because their stage cannot be proven. A write failure on a socket taken from the pool is
/// also Unknown, because it only shows up as a reset or EOF on the next read (this prevents
/// double billing).
pub(crate) fn classify(e: &ureq::Error) -> Stage {
    use ureq::Error as E;
    use ureq::Timeout as T;
    match e {
        E::Io(io) if io.kind() == io::ErrorKind::ConnectionRefused => Stage::NotSent,
        E::Timeout(T::Connect | T::Resolve) => Stage::NotSent,
        E::HostNotFound
        | E::BadUri(_)
        | E::Http(_)
        | E::RequireHttpsOnly(_)
        | E::InvalidProxyUrl
        | E::TlsRequired => Stage::NotSent,
        _ => Stage::Unknown,
    }
}

/// Redacts the error string (including the source chain) and removes the URL query (REQ-18).
pub(crate) fn redact_ureq_error(e: &ureq::Error) -> String {
    let s = crate::redaction::redact_transport_error(e);
    match s.find('?') {
        Some(i) => format!("{}?<redacted>", s.get(..i).unwrap_or_default()),
        None => s,
    }
}

#[cfg(test)]
mod tests;

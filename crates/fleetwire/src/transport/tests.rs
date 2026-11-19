use super::*;
use crate::pricing::Pricing;
use fleetwire_testkit::matchers::{header, method, path};
use fleetwire_testkit::{Mock, MockServer, ResponseTemplate};
use std::io::Read;
use std::time::{Duration, Instant};

/// Exactly the production Agent config, which does not follow redirects (`net::agent`).
fn client() -> ureq::Agent {
    crate::net::agent(std::time::Duration::from_secs(10))
}

/// A 200 + JSON comes back as it is. The Authorization header is attached.
#[test]
fn get_carries_bearer_and_returns_body() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/vehicles"))
        .and(header("authorization", "Bearer tok-abc"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(br#"{"response":[]}"#.to_vec(), "application/json"),
        )
        .mount(&server);

    let url = format!("{}/api/1/vehicles", server.uri()).parse().unwrap();
    let r = execute_get(
        &client(),
        url,
        "tok-abc",
        Pricing::NotCharged,
        std::time::Duration::from_secs(5),
    )
    .unwrap();

    assert_eq!(r.status.as_u16(), 200);
    assert_eq!(r.body, br#"{"response":[]}"#.to_vec());
    assert_eq!(r.content_type.as_deref(), Some("application/json"));
}

/// Measured E1 case B — 401 + text/html + 0 bytes + WWW-Authenticate.
/// The header has to be preserved for T3 to be able to pull out the error_description.
#[test]
fn preserves_www_authenticate_on_empty_html_401() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(401)
                .insert_header("content-type", "text/html")
                .insert_header(
                    "www-authenticate",
                    r#"Bearer realm="Doorkeeper", error="invalid_token", error_description="The access token is invalid""#,
                )
                .set_body_raw(Vec::new(), "text/html"),
        )
        .mount(&server);

    let url = format!("{}/api/1/vehicles", server.uri()).parse().unwrap();
    let r = execute_get(
        &client(),
        url,
        "t",
        Pricing::NotCharged,
        std::time::Duration::from_secs(5),
    )
    .unwrap();

    assert_eq!(r.status.as_u16(), 401);
    assert!(r.body.is_empty(), "as measured, the body should be 0 bytes");
    assert!(
        r.www_authenticate
            .unwrap()
            .contains("The access token is invalid")
    );
}

/// The delta-seconds form of Retry-After.
#[test]
fn parses_retry_after_delta_seconds() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "30")
                .set_body_raw(b"{}".to_vec(), "application/json"),
        )
        .mount(&server);

    let url = format!("{}/x", server.uri()).parse().unwrap();
    let r = execute_get(
        &client(),
        url,
        "t",
        Pricing::NotCharged,
        std::time::Duration::from_secs(5),
    )
    .unwrap();

    assert_eq!(r.retry_after, Some(std::time::Duration::from_secs(30)));
}

/// RFC 9110 also allows an HTTP-date in Retry-After.
/// If it were left out, the result would silently become None and the cap would be meaningless.
#[test]
fn parses_retry_after_http_date() {
    // A time in the past -> 0 seconds (it is not made negative).
    assert_eq!(
        parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"),
        Some(std::time::Duration::ZERO)
    );
    // None when it is in neither form.
    assert_eq!(parse_retry_after("not-a-date"), None);
}

/// A 3xx is not caught by decode's `>=400` and leaks out as Decode. The transport stops it.
#[test]
fn redirect_status_becomes_api_error_not_decode() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(302).insert_header("location", "https://evil.example/"))
        .mount(&server);

    let url = format!("{}/x", server.uri()).parse().unwrap();
    let err = execute_get(
        &client(),
        url,
        "t",
        Pricing::NotCharged,
        std::time::Duration::from_secs(5),
    )
    .unwrap_err();

    assert!(
        matches!(err, crate::error::Error::Api { status, .. } if status.as_u16() == 302),
        "actual: {err:?}"
    );
}

/// The size cap must cut the read off **while accumulating** — buffer-then-check is forbidden
/// (design §9). How it is told apart: give a body far larger than the cap and see whether the
/// returned error is the size overrun.
#[test]
fn aborts_while_streaming_when_body_exceeds_cap() {
    let server = MockServer::start();
    let huge = vec![b'x'; MAX_RESPONSE_BYTES + 1024];
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(huge, "application/json"))
        .mount(&server);

    let url = format!("{}/x", server.uri()).parse().unwrap();
    let err = execute_get(
        &client(),
        url,
        "t",
        Pricing::NotCharged,
        std::time::Duration::from_secs(10),
    )
    .unwrap_err();

    let msg = err.to_string();
    assert!(msg.contains("response too large"), "actual: {msg}");
}

// --- Fix round 1: M2 mechanism kill ------------------------------------

/// A reader that hands out fixed-size chunks and records how many times it is read.
/// With buffer-then-check, everything has to be read to the end (`Ok(0)`) even after the cap is
/// crossed, so the read count becomes larger.
struct CountingChunks {
    chunks: Vec<&'static [u8]>,
    idx: usize,
    reads: usize,
}

impl Read for CountingChunks {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.reads += 1;
        match self.chunks.get(self.idx) {
            Some(c) => {
                let n = c.len().min(buf.len());
                buf[..n].copy_from_slice(&c[..n]);
                self.idx += 1;
                Ok(n)
            }
            None => Ok(0),
        }
    }
}

/// `aborts_while_streaming_when_body_exceeds_cap` looks only at the resulting message and
/// cannot tell buffer-then-check (M2) apart. This test counts **how many times the reader was
/// read** and so checks the mechanism itself (discipline 3: assert the mechanism, not the
/// value).
///
/// Four 5-byte chunks (20 bytes in total), cap 10 bytes: with a check while accumulating it
/// stops at the third chunk, so the read count is exactly 3. With buffer-then-check it is 5, up
/// to the `Ok(0)` that signals the end.
#[test]
fn accumulate_capped_stops_polling_once_cap_is_crossed() {
    let mut r = CountingChunks {
        chunks: vec![b"AAAAA", b"BBBBB", b"CCCCC", b"DDDDD"],
        idx: 0,
        reads: 0,
    };
    let far = Instant::now() + Duration::from_secs(5);
    let err = read_capped(&mut r, 10, far).unwrap_err();
    assert!(matches!(err, ReadError::CapExceeded));
    assert_eq!(
        r.reads, 3,
        "read count must prove early abort, not a full drain to the end of the body"
    );
}

// --- Fix round 1: M1 boundary kill --------------------------------------

/// A size exactly equal to the cap must pass, and just one byte over must fail — the mutation
/// that changes `>` to `>=` (M1) flips the first half.
#[test]
fn accumulate_capped_exact_boundary_succeeds_one_byte_over_fails() {
    let max = 4usize;
    let far = Instant::now() + Duration::from_secs(5);
    let mut ok: &[u8] = b"AAAA";
    assert_eq!(read_capped(&mut ok, max, far).unwrap().len(), max);
    let mut over: &[u8] = b"AAAAA";
    assert!(matches!(
        read_capped(&mut over, max, far),
        Err(ReadError::CapExceeded)
    ));
}

// --- Fix round 1: M4 kill + redact_transport_error coverage ------------

/// Gets a port nobody is using yet: binds to port 0, reads the port number the OS
/// assigned and drops the listener at once. A connection to that port is certain to
/// fail with connection-refused — this is more deterministic than hardcoding a
/// platform-dependent reserved port (for example TCP port 1).
fn unused_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

/// M4: if the `e.is_connect()` branch is flipped (for example `!e.is_connect()`), this
/// test dies. A connect failure must be `TransportNotSent` (retryable, certainly not
/// billed) and must never become `TransportUnknown` (must not be retried) — flipped, a
/// request that is in fact safe to retry would be classified as not retryable, and so
/// never retried, under the wrong assumption that "the server may already have processed
/// it".
#[test]
fn connect_failure_is_transport_not_sent_not_unknown() {
    let port = unused_port();
    let url = format!("http://127.0.0.1:{port}/x").parse().unwrap();

    let err = execute_get(
        &client(),
        url,
        "t",
        Pricing::NotCharged,
        std::time::Duration::from_secs(5),
    )
    .unwrap_err();

    match err {
        Error::TransportNotSent { .. } => {}
        other => panic!("expected TransportNotSent, got: {other:?}"),
    }
}

/// Makes a real error with the connect-failure fixture and checks that the VIN planted in the
/// URL path does not survive in the message (REQ-18).
///
/// ureq's connection error text does not include the URL, so only "does not leak" is asserted
/// here. Whether an error that carries a URL is actually scrubbed is checked by
/// `ureq_error_with_url_is_redacted`.
#[test]
fn connect_error_message_does_not_leak_vin_from_url() {
    let port = unused_port();
    let vin = "5YJ3E1EA7KF000000";
    let url = format!("http://127.0.0.1:{port}/api/1/vehicles/{vin}/vehicle_data")
        .parse()
        .unwrap();

    let err = execute_get(
        &client(),
        url,
        "t",
        Pricing::NotCharged,
        std::time::Duration::from_secs(5),
    )
    .unwrap_err();

    let msg = err.to_string();
    assert!(
        !msg.contains(vin),
        "VIN leaked in transport error message: {msg}"
    );
}

/// Checks that a ureq error that carries a URL (`BadUri`) goes out with the VIN and the query
/// removed (REQ-18).
#[test]
fn ureq_error_with_url_is_redacted() {
    let vin = "5YJ3E1EA7KF000000";
    let e = ureq::Error::BadUri(format!(
        "https://x/api/1/vehicles/{vin}/vehicle_data?endpoints=location_data"
    ));
    let msg = crate::net::redact_ureq_error(&e);
    assert!(!msg.contains(vin), "VIN leaked: {msg}");
    assert!(msg.contains("[VIN]"), "expected VIN marker, got: {msg}");
    assert!(!msg.contains("location_data"), "query leaked: {msg}");
}

/// When the connection is cut in the middle of the body after the headers, the server has
/// already processed the request, so it must be Unknown (must not be retried).
#[test]
fn get_cut_mid_body_is_unknown() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(b"{\"response\":[1,2,3]}".to_vec(), "application/json")
                .cut_after_body_bytes(3),
        )
        .mount(&server);
    let url = format!("{}/x", server.uri()).parse().unwrap();
    let err = execute_get(
        &client(),
        url,
        "t",
        Pricing::NotCharged,
        Duration::from_secs(5),
    )
    .unwrap_err();
    assert!(
        matches!(err, Error::TransportUnknown { .. }),
        "expected TransportUnknown, got: {err:?}"
    );
}

/// A server that trickles the body one byte at a time must end as Unknown within
/// `request_timeout` + 1 second (spec §0 C1).
/// Negative control: with the deadline check in `read_capped` removed, ureq extends the deadline
/// by 1 second on every read and the call is held for tens of seconds.
#[test]
fn get_trickling_body_times_out_as_unknown() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(vec![b' '; 1024], "application/json")
                .trickle_body(1, Duration::from_millis(20)),
        )
        .mount(&server);
    let url = format!("{}/x", server.uri()).parse().unwrap();
    let t = Instant::now();
    let err = execute_get(
        &crate::net::agent(Duration::from_millis(200)),
        url,
        "t",
        Pricing::NotCharged,
        Duration::from_millis(200),
    )
    .unwrap_err();
    assert!(
        matches!(err, Error::TransportUnknown { .. }),
        "expected TransportUnknown, got: {err:?}"
    );
    assert!(
        t.elapsed() < Duration::from_millis(1_500),
        "{:?}",
        t.elapsed()
    );
}

// --- A4 T10: rate-limit header parsing ----------------------------------

/// All three `ratelimit-*` headers present and numeric → `Some`.
#[test]
fn rate_limit_is_some_when_all_three_headers_parse() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ratelimit-device-data-per-vehicle-limit", "60")
                .insert_header("ratelimit-device-data-per-vehicle-remaining", "59")
                .insert_header("ratelimit-device-data-per-vehicle-reset", "57")
                .set_body_raw(b"{}".to_vec(), "application/json"),
        )
        .mount(&server);

    let url = format!("{}/x", server.uri()).parse().unwrap();
    let r = execute_get(
        &client(),
        url,
        "t",
        Pricing::NotCharged,
        std::time::Duration::from_secs(5),
    )
    .unwrap();

    assert_eq!(r.rate_limit, Some((60, 59, 57)));
}

/// All three headers present, but one is **not numeric** → `None`,
/// not a value with the bad field silently replaced by `0`.
///
/// This is the case
/// `client::tests::partial_or_unparsable_headers_yield_none` does
/// **not** cover: that fixture only ever sets two of the three headers
/// (the third is entirely absent), so a mutation that changes
/// `.and_then(...ok())` to `.map(...unwrap_or(0))` — turning "failed to
/// parse" into "defaults to 0" — sails through it untouched: the
/// all-three-mandatory `match` still sees the *absent* header as `None`
/// regardless of how a present-but-bad value would have been handled,
/// so that test's assertion still holds. Confirmed by injecting the
/// mutation directly: `cargo test client::tests::partial_or_unparsable_headers_yield_none`
/// still passes. This test closes that gap by making all three headers
/// present, with `limit` set to a non-numeric string.
#[test]
fn rate_limit_is_none_when_a_present_header_fails_to_parse() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ratelimit-device-data-per-vehicle-limit", "sixty")
                .insert_header("ratelimit-device-data-per-vehicle-remaining", "59")
                .insert_header("ratelimit-device-data-per-vehicle-reset", "57")
                .set_body_raw(b"{}".to_vec(), "application/json"),
        )
        .mount(&server);

    let url = format!("{}/x", server.uri()).parse().unwrap();
    let r = execute_get(
        &client(),
        url,
        "t",
        Pricing::NotCharged,
        std::time::Duration::from_secs(5),
    )
    .unwrap();

    assert_eq!(
        r.rate_limit, None,
        "a present-but-unparsable header must not be defaulted to 0"
    );
}

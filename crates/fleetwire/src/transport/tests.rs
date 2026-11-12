use super::*;
use crate::pricing::Pricing;
use fleetwire_testkit::matchers::{header, method, path};
use fleetwire_testkit::{Mock, MockServer, ResponseTemplate};
use std::io::Read;
use std::time::{Duration, Instant};

/// 리다이렉트를 따르지 않는 프로덕션 Agent 설정 그대로입니다(`net::agent`).
fn client() -> ureq::Agent {
    crate::net::agent(std::time::Duration::from_secs(10))
}

/// 200 + JSON 이 그대로 실려 온다. Authorization 헤더가 붙는다.
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

/// 실측 E1 케이스 B — 401 + text/html + 0바이트 + WWW-Authenticate.
/// 헤더가 보존돼야 T3 가 error_description 을 꺼낼 수 있다.
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
    assert!(r.body.is_empty(), "실측상 body 는 0바이트여야 한다");
    assert!(
        r.www_authenticate
            .unwrap()
            .contains("The access token is invalid")
    );
}

/// Retry-After 의 delta-seconds 형식.
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

/// RFC 9110 은 Retry-After 에 HTTP-date 도 허용한다.
/// 빠뜨리면 조용히 None 이 되어 상한이 무의미해진다.
#[test]
fn parses_retry_after_http_date() {
    // 과거 시각 → 0초(음수로 만들지 않는다).
    assert_eq!(
        parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"),
        Some(std::time::Duration::ZERO)
    );
    // 형식이 아니면 None.
    assert_eq!(parse_retry_after("not-a-date"), None);
}

/// 3xx 는 decode 의 `>=400` 에 안 걸려 Decode 로 샌다. transport 가 막는다.
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
        "실제: {err:?}"
    );
}

/// 크기 상한은 **누적 중** 끊겨야 한다 — buffer-then-check 금지(design §9).
/// 판별 방법: 상한보다 훨씬 큰 body 를 주고, 반환된 에러가 크기 초과인지 본다.
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
    assert!(msg.contains("response too large"), "실제: {msg}");
}

// --- Fix round 1: M2 mechanism kill ------------------------------------

/// 고정 크기 조각을 내놓으며 읽힌 횟수를 기록하는 리더입니다.
/// buffer-then-check 라면 상한을 넘겨도 끝(`Ok(0)`)까지 다 읽어야 하므로 읽기 횟수가 더 많아집니다.
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

/// 결과 메시지만 보는 `aborts_while_streaming_when_body_exceeds_cap` 은
/// buffer-then-check(M2)를 구별하지 못합니다. 이 시험은 **몇 번 읽혔는지**를 세어
/// 매커니즘 자체를 확인합니다(규율 3: 값이 아니라 매커니즘을 단언하라).
///
/// 5바이트 조각 4개(총 20바이트), 상한 10바이트: 누적 중 검사라면 셋째 조각에서 멈추므로
/// 읽기 횟수는 정확히 3 입니다. buffer-then-check 라면 끝을 알리는 `Ok(0)` 까지 5 입니다.
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

/// 상한과 정확히 같은 크기는 통과하고, 1바이트만 넘으면 실패해야 합니다 — `>` 를 `>=` 로 바꾸는
/// 뮤테이션(M1)은 앞의 절반을 뒤집습니다.
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

/// 아직 아무도 쓰지 않는 포트를 얻는다: 포트 0 으로 바인드해서 OS 가 배정한
/// 포트 번호를 읽고, 리스너를 즉시 drop 한다. 그 포트로의 연결은
/// connection-refused 로 확정 실패한다 — 플랫폼에 의존하는 예약 포트(예:
/// TCP 포트 1)를 하드코딩하는 것보다 결정적이다.
fn unused_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    port
}

/// M4: `e.is_connect()` 분기가 뒤집히면(예: `!e.is_connect()`) 이 테스트가
/// 죽는다. connect 실패는 `TransportNotSent`(재시도 가능·비과금 확정)여야
/// 하고, 절대 `TransportUnknown`(재시도 금지)이 되면 안 된다 — 뒤집히면
/// "이미 서버가 처리했을 수도 있다"는 잘못된 가정 아래 영원히 재시도되지
/// 않을 요청을, 사실은 재시도해도 안전한데 재시도 금지로 분류하게 된다.
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

/// 연결 실패 픽스처로 진짜 오류를 만들고, URL 경로에 심은 VIN 이 메시지에 살아남지 않는지 봅니다(REQ-18).
///
/// ureq 의 연결 오류 문구에는 URL 이 들어가지 않으므로 여기서는 「새지 않음」만 단언합니다.
/// URL 이 든 오류가 실제로 지워지는지는 `ureq_error_with_url_is_redacted` 가 봅니다.
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

/// URL 을 담은 ureq 오류(`BadUri`)가 VIN·질의를 지운 채 나가는지 봅니다(REQ-18).
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

/// 헤더 뒤 본문 중간에 끊기면 서버가 이미 처리했으므로 Unknown 이어야 합니다(재시도 금지).
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

/// 본문을 1바이트씩 흘리는 서버는 `request_timeout` + 1초 안에 Unknown 으로 끝나야 합니다(스펙 §0 C1).
/// 음성 대조: `read_capped` 의 마감 검사를 지우면 ureq 가 읽을 때마다 마감을 1초씩 늘려 수십 초 붙잡힙니다.
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

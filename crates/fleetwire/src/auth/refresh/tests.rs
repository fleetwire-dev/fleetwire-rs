use super::*;
use fleetwire_testkit::matchers::{body_string_contains, header, method, path};
use fleetwire_testkit::{Mock, MockServer, ResponseTemplate};

fn refresher(server: &MockServer) -> HttpRefresher {
    HttpRefresher::new("cid-123".into())
        .with_token_url(format!("{}/oauth2/v3/token", server.uri()).parse().unwrap())
}

/// Design invariant I2 — refresh goes to `fleet-auth…`, **not** to
/// `auth.tesla.com`, for the measured reasons in this module's header.
///
/// A5 Task 1's reachability audit found this invariant completely
/// unpinned: rewriting `TOKEN_URL` to `https://auth.tesla.com/oauth2/v3/
/// token` left all 366 tests green, because every other test in this
/// module reaches for [`HttpRefresher::with_token_url`] to point at a
/// mock server and so never observes the default. The failure that
/// escapes is silent and total — refresh stops working against the real
/// Fleet API while the suite stays green.
///
/// Asserted through [`HttpRefresher::new`] rather than against the
/// constant directly, so the wiring is covered too: a default that stops
/// being applied fails this just as a changed constant does. Both sides
/// are checked, since the discovery document actively advertises the
/// wrong host and "correcting" the code to match it is the specific
/// mistake the module header warns against.
#[test]
fn the_default_token_url_is_the_fleet_auth_host_not_auth_tesla_com() {
    let r = HttpRefresher::new("cid-123".into());
    assert_eq!(
        r.token_url.as_str(),
        "https://fleet-auth.prd.vn.cloud.tesla.com/oauth2/v3/token"
    );
    assert_eq!(
        r.token_url.host_str(),
        Some("fleet-auth.prd.vn.cloud.tesla.com")
    );
    assert_ne!(r.token_url.host_str(), Some("auth.tesla.com"));
    assert_eq!(r.token_url.scheme(), "https");
}

/// 성공 경로 — form-urlencoded, public client(secret 없음).
#[test]
fn posts_form_urlencoded_without_client_secret() {
    let server = MockServer::start();
    Mock::given(method("POST"))
        .and(path("/oauth2/v3/token"))
        .and(header("content-type", "application/x-www-form-urlencoded"))
        .and(body_string_contains("grant_type=refresh_token"))
        .and(body_string_contains("client_id=cid-123"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            br#"{"access_token":"new-a","refresh_token":"new-r","expires_in":28800}"#.to_vec(),
            "application/json",
        ))
        .mount(&server);

    let got = refresher(&server).refresh("old-r").unwrap();
    assert_eq!(got.access_token, "new-a");
    assert_eq!(got.refresh_token, "new-r");
    assert_eq!(got.expires_in_secs, 28_800);
}

/// client_secret 을 절대 보내지 않는다(public client).
#[test]
fn never_sends_client_secret() {
    let server = MockServer::start();
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            br#"{"access_token":"a","refresh_token":"r","expires_in":1}"#.to_vec(),
            "application/json",
        ))
        .mount(&server);

    let _ = refresher(&server).refresh("old-r");

    let reqs = server.received_requests().unwrap();
    let body = String::from_utf8_lossy(&reqs[0].body).to_string();
    assert!(!body.contains("client_secret"), "본문: {body}");
}

/// OAuth 오류 응답이 리댁션된 Error 로 올라온다.
#[test]
fn oauth_error_becomes_error_not_panic() {
    let server = MockServer::start();
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(400)
                .set_body_raw(br#"{"error":"invalid_grant"}"#.to_vec(), "application/json"),
        )
        .mount(&server);

    let e = refresher(&server).refresh("old-r").unwrap_err();
    assert!(e.to_string().contains("invalid_grant"), "실제: {e}");
}

/// 토큰이 에러 메시지에 새지 않는다.
#[test]
fn refresh_token_never_appears_in_error() {
    let server = MockServer::start();
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_raw(b"boom".to_vec(), "text/plain"))
        .mount(&server);

    let e = refresher(&server)
        .refresh("SUPERSECRET-REFRESH")
        .unwrap_err();
    assert!(
        !format!("{e}{e:?}").contains("SUPERSECRET-REFRESH"),
        "실제: {e:?}"
    );
}

/// M4 킬: redirect 를 따라가면 자격증명(토큰)이 다른 호스트로 넘어간다.
/// `redirect::Policy::none()` 이 실제로 걸려 있는지, 두 번째 서버가
/// **요청을 전혀 받지 않는 것**으로 확인한다 — 첫 응답 body 파싱 실패는
/// 부수 효과일 뿐이고, 여기서 세는 것은 두 번째 서버의 요청 수(mechanism)다.
#[test]
fn does_not_follow_redirect_to_second_host() {
    let redirect_target = MockServer::start();
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            br#"{"access_token":"a","refresh_token":"r","expires_in":1}"#.to_vec(),
            "application/json",
        ))
        .mount(&redirect_target);

    let server = MockServer::start();
    Mock::given(method("POST"))
        .and(path("/oauth2/v3/token"))
        .respond_with(ResponseTemplate::new(302).insert_header(
            "Location",
            format!("{}/oauth2/v3/token", redirect_target.uri()),
        ))
        .mount(&server);

    // redirect 를 따라갔다면 200 을 파싱해서 성공했을 것이다. 따라가지
    // 않으므로 302 자체가 에러가 된다 — 그 값 자체는 부차적이다.
    let _ = refresher(&server).refresh("old-r");

    let redirected_reqs = redirect_target.received_requests().unwrap();
    assert_eq!(
        redirected_reqs.len(),
        0,
        "리다이렉트 대상 서버가 요청을 받았다 — redirect::Policy::none() 이 걸리지 않았다"
    );
}

/// review round 1 critical #1 킬: 헤더는 즉시 오고 본문만 멈추는 응답을
/// 재현한다. `wiremock::ResponseTemplate::set_delay` 는 쓸 수 없다 —
/// wiremock 0.6 은 응답을 언제나 완성된 `Full<Bytes>` 로 통째로 만든
/// 뒤 지연시키므로(response_template.rs), "헤더는 빠르고 본문만 느림"
/// 을 표현하지 못하고 delay 를 걸면 헤더 수신 자체(=`send()`)가 늦어져
/// 예전(버그) 코드도 `send()` 만 감싼 timeout 에 이미 걸려버린다 —
/// 즉 그 방법으로는 이 회귀를 구별하지 못한다. 그래서 원시 TCP 로 헤더를
/// 먼저 완전히 보내고(`send()` 를 즉시 resolve 시킨 뒤) 본문은 영원히
/// 보내지 않는 손으로 만든 서버를 쓴다.
#[test]
fn timeout_bounds_the_body_read_not_just_the_headers() {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        // 요청을 헤더 끝까지만 대충 흘려보냅니다.
        loop {
            let n = socket.read(&mut buf).unwrap_or(0);
            if n == 0 || buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        // 헤더는 완전하고 즉시 보냅니다 — 요청 호출은 이 시점에 돌아옵니다.
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 60\r\n\r\n",
            )
            .unwrap();
        socket.flush().unwrap();
        // 본문은 보내지 않습니다 — 시험이 끝나면 프로세스와 함께 사라집니다.
        std::thread::sleep(Duration::from_secs(30));
    });

    let url: crate::Url = format!("http://127.0.0.1:{port}/oauth2/v3/token")
        .parse()
        .unwrap();
    let refresher = HttpRefresher::new("cid-123".into())
        .with_token_url(url)
        .with_timeout(Duration::from_millis(200));

    let start = std::time::Instant::now();
    let e = refresher.refresh("old-r").unwrap_err();
    let elapsed = start.elapsed();

    assert!(e.to_string().contains("timed out"), "실제: {e}");
    // 값이 아니라 매커니즘을 단언한다(규율 3): 헤더만 감싼 예전 코드라면
    // `send()` 는 수 ms 만에 끝나고 이후 `bytes()` 가 무제한으로
    // 걸려 서버의 30초 sleep 까지 매달렸을 것이다. 하나의 timeout 이라면
    // 설정한 200ms 근처에서 반환되어야 합니다. 다만 `read_capped` 는 마감을 읽기 한 번(약 1초)까지 넘길 수
    // 있다고 약속합니다(`net.rs` 의 `read_capped` 문서). 상한은 그 약속 + 지터 1초 = 2.2초로 둡니다
    // (10-28 verify 부하에서 1초 상한이 1.202초 = 200ms + 읽기 한 번으로 실패했습니다). 30초와는 여전히 13배 차이입니다.
    assert!(
        elapsed < Duration::from_millis(200) + Duration::from_secs(2),
        "본문 읽기가 timeout 에 감싸이지 않았다 — 경과 {elapsed:?}"
    );
}

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

/// The success path — form-urlencoded, public client (no secret).
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

/// The client_secret is never sent (public client).
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
    assert!(!body.contains("client_secret"), "body: {body}");
}

/// An OAuth error response comes up as a redacted Error.
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
    assert!(e.to_string().contains("invalid_grant"), "actual: {e}");
}

/// The token does not leak into the error message.
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
        "actual: {e:?}"
    );
}

/// M4 kill: following a redirect hands the credentials (the token) to another host.
/// Whether the no-redirect setting is really in effect is confirmed by the second
/// server **receiving no request at all** — the failure to parse the first response body
/// is only a side effect, and what is counted here is the number of requests at the second
/// server (the mechanism).
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

    // Had the redirect been followed, the 200 would have been parsed and the call would have
    // succeeded. It is not followed, so the 302 itself becomes an error — that value itself
    // is secondary.
    let _ = refresher(&server).refresh("old-r");

    let redirected_reqs = redirect_target.received_requests().unwrap();
    assert_eq!(
        redirected_reqs.len(),
        0,
        "the redirect target received a request — the refresher's agent follows redirects"
    );
}

/// review round 1 critical #1 kill: reproduces a response whose headers arrive at once and
/// whose body alone stalls. A delay on the whole response cannot be used — it holds back
/// the headers as well, so it cannot express "fast headers, slow body only";
/// setting a delay makes the receipt of the headers itself (= `send()`) late, so the old
/// (buggy) code is also already caught by the timeout that wrapped only `send()` — that is,
/// that method cannot tell this regression apart. So a hand-made server is used that first
/// sends the headers completely over raw TCP (resolving `send()` at once) and never sends
/// the body.
#[test]
fn timeout_bounds_the_body_read_not_just_the_headers() {
    use std::io::{Read, Write};

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();

    std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        let mut buf = [0u8; 4096];
        // Roughly drain the request, only up to the end of the headers.
        loop {
            let n = socket.read(&mut buf).unwrap_or(0);
            if n == 0 || buf[..n].windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        // The headers are sent complete and at once — the request call returns at this point.
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 60\r\n\r\n",
            )
            .unwrap();
        socket.flush().unwrap();
        // The body is never sent — this thread goes away with the process when the test ends.
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

    assert!(e.to_string().contains("timed out"), "actual: {e}");
    // This asserts the mechanism, not the value (discipline 3): with the old code that
    // wrapped only the headers, `send()` would finish within a few ms and the following
    // `bytes()` would then block without limit, hanging until the server's 30-second sleep.
    // With a single timeout it must return near the configured 200ms. However, `read_capped`
    // promises that it may overrun the deadline by up to one read (about 1 second) (the
    // `read_capped` docs in `net.rs`). The upper bound is set to that promise + 1 second of
    // jitter = 2.2 seconds (under the 10-28 verify load a 1-second upper bound failed at
    // 1.202 seconds = 200ms + one read). That is still a 13-fold difference from 30 seconds.
    assert!(
        elapsed < Duration::from_millis(200) + Duration::from_secs(2),
        "the body read is not wrapped by the timeout — elapsed {elapsed:?}"
    );
}

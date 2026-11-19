use std::io::{Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use crate::matchers::{any, body_string_contains, header, method, path};
use crate::{Mock, MockServer, ResponseTemplate};

/// Sends a raw request as it is and returns the whole response as a string.
fn raw(server: &MockServer, req: &str) -> String {
    let mut s = TcpStream::connect(server.address()).unwrap();
    s.write_all(req.as_bytes()).unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    out
}

#[test]
fn matching_mock_answers_and_records_the_request() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/vehicles"))
        .and(header("authorization", "Bearer t"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(b"{}".to_vec(), "application/json"))
        .expect(1)
        .mount(&server);
    let out = raw(
        &server,
        "GET /api/1/vehicles?x=1 HTTP/1.1\r\nHost: a\r\nAuthorization: Bearer t\r\n\r\n",
    );
    assert!(out.starts_with("HTTP/1.1 200"), "{out}");
    assert!(out.contains("content-type: application/json"), "{out}");
    assert!(out.ends_with("{}"), "{out}");
    let reqs = server.received_requests().unwrap();
    assert_eq!(reqs.len(), 1);
    assert_eq!(reqs[0].url.path(), "/api/1/vehicles");
    assert_eq!(reqs[0].url.query(), Some("x=1"));
    assert_eq!(reqs[0].header("AUTHORIZATION"), Some("Bearer t"));
    assert_eq!(reqs[0].method, "GET");
}

#[test]
fn unmatched_request_gets_404() {
    let server = MockServer::start();
    Mock::given(path("/a"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server);
    let out = raw(&server, "GET /b HTTP/1.1\r\nHost: a\r\n\r\n");
    assert!(out.starts_with("HTTP/1.1 404"), "{out}");
}

#[test]
fn form_body_is_read_by_content_length_and_matched() {
    let server = MockServer::start();
    Mock::given(body_string_contains("grant_type=refresh_token"))
        .respond_with(ResponseTemplate::new(200))
        .mount(&server);
    let body = "grant_type=refresh_token&client_id=c";
    let out = raw(
        &server,
        &format!(
            "POST /t HTTP/1.1\r\nHost: a\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ),
    );
    assert!(out.starts_with("HTTP/1.1 200"), "{out}");
    assert_eq!(server.received_requests().unwrap()[0].body, body.as_bytes());
}

#[test]
fn priority_then_registration_order_then_exhaustion() {
    let server = MockServer::start();
    Mock::given(any())
        .respond_with(ResponseTemplate::new(500))
        .mount(&server);
    Mock::given(any())
        .respond_with(ResponseTemplate::new(201))
        .with_priority(1)
        .up_to_n_times(1)
        .mount(&server);
    assert!(raw(&server, "GET / HTTP/1.1\r\nHost: a\r\n\r\n").starts_with("HTTP/1.1 201"));
    assert!(raw(&server, "GET / HTTP/1.1\r\nHost: a\r\n\r\n").starts_with("HTTP/1.1 500"));
}

#[test]
#[should_panic(expected = "expected 2")]
fn verify_panics_on_unmet_expectation() {
    let server = MockServer::start();
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200))
        .expect(2)
        .named("twice")
        .mount(&server);
    let _ = raw(&server, "GET / HTTP/1.1\r\nHost: a\r\n\r\n");
    server.verify();
}

#[test]
fn drop_does_not_double_panic_while_unwinding() {
    // Negative control: if Drop did not check panicking(), this test would kill the process
    // with an abort.
    let r = std::panic::catch_unwind(|| {
        let server = MockServer::start();
        Mock::given(any())
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server);
        panic!("first");
    });
    assert!(r.is_err());
}

#[test]
fn trickle_body_spreads_bytes_over_time() {
    let server = MockServer::start();
    Mock::given(any())
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("abcd")
                .trickle_body(1, Duration::from_millis(50)),
        )
        .mount(&server);
    let t = Instant::now();
    let out = raw(&server, "GET / HTTP/1.1\r\nHost: a\r\n\r\n");
    assert!(out.ends_with("abcd"), "{out}");
    assert!(
        t.elapsed() >= Duration::from_millis(150),
        "{:?}",
        t.elapsed()
    );
}

#[test]
fn cut_after_body_bytes_closes_mid_body() {
    let server = MockServer::start();
    Mock::given(any())
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_string("abcdefgh")
                .cut_after_body_bytes(3),
        )
        .mount(&server);
    let out = raw(&server, "GET / HTTP/1.1\r\nHost: a\r\n\r\n");
    assert!(out.contains("content-length: 8"), "{out}");
    assert!(out.ends_with("abc"), "{out}");
}

#[test]
fn concurrent_connections_are_served_in_parallel() {
    let server = MockServer::start();
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(300)))
        .mount(&server);
    let t = Instant::now();
    std::thread::scope(|s| {
        for _ in 0..4 {
            s.spawn(|| raw(&server, "GET / HTTP/1.1\r\nHost: a\r\n\r\n"));
        }
    });
    // Without a thread per connection this takes at least 300ms x 4 = 1.2s.
    assert!(
        t.elapsed() < Duration::from_millis(900),
        "{:?}",
        t.elapsed()
    );
}

use std::io::{self, Read};
use std::time::{Duration, Instant};

use fleetwire_testkit::matchers::any;
use fleetwire_testkit::{Mock, MockServer, ResponseTemplate};

use super::*;

/// A reader that pauses for `gap` on every read and yields 1 byte (imitates a slow server).
struct Trickle {
    left: usize,
    gap: Duration,
}

impl Read for Trickle {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.left == 0 || buf.is_empty() {
            return Ok(0);
        }
        std::thread::sleep(self.gap);
        buf[0] = b'x';
        self.left -= 1;
        Ok(1)
    }
}

/// A reader that counts how many times it is read. It shows whether it is called again after
/// the cap is exceeded.
struct Counting {
    reads: usize,
}

impl Read for Counting {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.reads += 1;
        let n = buf.len().min(4);
        buf[..n].fill(b'y');
        Ok(n)
    }
}

fn far() -> Instant {
    Instant::now() + Duration::from_secs(5)
}

#[test]
fn read_capped_accepts_exactly_max() {
    let mut r: &[u8] = &[7u8; 10];
    assert_eq!(read_capped(&mut r, 10, far()).unwrap().len(), 10);
}

#[test]
fn read_capped_rejects_max_plus_one() {
    let mut r: &[u8] = &[7u8; 11];
    assert!(matches!(
        read_capped(&mut r, 10, far()),
        Err(ReadError::CapExceeded)
    ));
}

#[test]
fn read_capped_stops_reading_once_the_cap_is_exceeded() {
    // An endless reader: unless the cap check happens while accumulating, this test never
    // finishes.
    let mut r = Counting { reads: 0 };
    assert!(matches!(
        read_capped(&mut r, 10, far()),
        Err(ReadError::CapExceeded)
    ));
    assert_eq!(r.reads, 3, "4+4 then the third read crosses 10 and stops");
}

#[test]
fn read_capped_stops_a_trickling_body_at_the_deadline() {
    // Negative control: with the deadline check removed, this test takes more than 10 seconds
    // before it finishes.
    let mut r = Trickle {
        left: 10_000,
        gap: Duration::from_millis(1),
    };
    let t = Instant::now();
    let got = read_capped(&mut r, 1 << 20, Instant::now() + Duration::from_millis(200));
    assert!(matches!(got, Err(ReadError::Deadline)));
    assert!(
        t.elapsed() < Duration::from_millis(1_300),
        "{:?}",
        t.elapsed()
    );
}

#[test]
fn classify_refused_connection_is_not_sent() {
    // Port 1 is a privileged port, so no other test can bind it, and normally nothing listens
    // there, so a connection is always refused. The earlier approach of binding a free port and
    // releasing it failed when another test bound the same port in between (once on 2026-10-14,
    // MSRV). Same as `CLOSED_URL` in `fleetwire-notify`.
    let e = agent(Duration::from_secs(2))
        .get("http://127.0.0.1:1/")
        .call()
        .unwrap_err();
    assert_eq!(classify(&e), Stage::NotSent, "{e:?}");
}

#[test]
fn classify_global_timeout_is_unknown() {
    let server = MockServer::start();
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_millis(800)))
        .mount(&server);
    let e = agent(Duration::from_millis(200))
        .get(&server.uri())
        .call()
        .unwrap_err();
    assert_eq!(classify(&e), Stage::Unknown, "{e:?}");
}

#[test]
fn classify_io_other_than_refused_is_unknown() {
    let e = ureq::Error::Io(io::Error::new(io::ErrorKind::ConnectionReset, "reset"));
    assert_eq!(classify(&e), Stage::Unknown);
    let e = ureq::Error::Io(io::Error::new(io::ErrorKind::InvalidData, "tls"));
    assert_eq!(classify(&e), Stage::Unknown);
    assert_eq!(classify(&ureq::Error::ConnectionFailed), Stage::Unknown);
}

#[test]
fn agent_does_not_follow_redirects() {
    let second = MockServer::start();
    Mock::given(any())
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&second);
    let first = MockServer::start();
    Mock::given(any())
        .respond_with(ResponseTemplate::new(302).insert_header("location", second.uri()))
        .mount(&first);
    let resp = agent(Duration::from_secs(2))
        .get(&first.uri())
        .call()
        .unwrap();
    assert_eq!(resp.status().as_u16(), 302);
}

#[test]
fn agent_returns_error_statuses_as_responses() {
    let server = MockServer::start();
    Mock::given(any())
        .respond_with(ResponseTemplate::new(503))
        .mount(&server);
    let resp = agent(Duration::from_secs(2))
        .get(&server.uri())
        .call()
        .unwrap();
    assert_eq!(resp.status().as_u16(), 503);
}

/// Selects the side that runs only in the child process. The parent re-runs just this one test
/// with `HTTPS_PROXY` set.
const PROXY_PROBE_ENV: &str = "FLEETWIRE_PROXY_PROBE_CHILD";

#[test]
fn agent_ignores_proxy_env() {
    if std::env::var_os(PROXY_PROBE_ENV).is_some() {
        // ureq reads the proxy from the environment variables when it builds the Agent config.
        // `.proxy(None)` must clear it.
        assert!(
            agent(Duration::from_secs(1)).config().proxy().is_none(),
            "the agent picked up the proxy from the environment"
        );
        return;
    }
    // Parent: re-runs the same test binary with the proxy environment variables (`set_var` is
    // unsafe, so it is not used).
    // Negative control: removing `.proxy(None)` from `net::agent` makes the child fail, and so
    // this test fails (final review M-1).
    let exe = std::env::current_exe().expect("test binary path should be known");
    let out = std::process::Command::new(exe)
        .args([
            "--exact",
            "net::tests::agent_ignores_proxy_env",
            "--test-threads=1",
        ])
        .env(PROXY_PROBE_ENV, "1")
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .env("HTTP_PROXY", "http://127.0.0.1:9")
        .env("ALL_PROXY", "http://127.0.0.1:9")
        .output()
        .expect("re-running the test binary should work");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("1 passed"),
        "child run failed or ran nothing: {}\n{stdout}",
        out.status
    );
}

#[test]
fn redact_ureq_error_drops_the_query() {
    let e = ureq::Error::BadUri("https://x/api?access_token=SECRET".to_owned());
    assert!(!redact_ureq_error(&e).contains("SECRET"));
}

/// A reader that simply returns a read error.
struct Broken;

impl Read for Broken {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(io::ErrorKind::ConnectionReset, "reset"))
    }
}

#[test]
fn read_capped_passes_io_errors_through() {
    match read_capped(&mut Broken, 10, far()) {
        Err(ReadError::Io(e)) => assert_eq!(e.kind(), io::ErrorKind::ConnectionReset),
        other => panic!("expected an I/O error, got {other:?}"),
    }
}

/// Any read error that arrives after the deadline is reported as the deadline, so the caller can
/// say "timed out" (headers arrived, then the body stalled). The error here is deliberately not a
/// timeout kind, so this test guards the "past the deadline" branch on its own.
#[test]
fn read_capped_reports_an_io_error_past_the_deadline_as_deadline() {
    struct LateBroken;
    impl Read for LateBroken {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            std::thread::sleep(Duration::from_millis(60));
            Err(io::Error::new(io::ErrorKind::ConnectionReset, "reset"))
        }
    }
    let got = read_capped(
        &mut LateBroken,
        10,
        Instant::now() + Duration::from_millis(30),
    );
    assert!(matches!(got, Err(ReadError::Deadline)), "{got:?}");
}

/// A reader that fails at once with the given error, long before the deadline.
struct FailsWith(fn() -> io::Error);

impl Read for FailsWith {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        Err((self.0)())
    }
}

/// ureq's global timer can run out a moment before our deadline under load; its error arrives
/// as an I/O error of kind `Other` wrapping `ureq::Error::Timeout` (2026-11-16 verify failure).
#[test]
fn read_capped_reports_a_wrapped_ureq_timeout_before_the_deadline_as_deadline() {
    let mut r = FailsWith(|| ureq::Error::Timeout(ureq::Timeout::Global).into_io());
    let got = read_capped(&mut r, 10, far());
    assert!(matches!(got, Err(ReadError::Deadline)), "{got:?}");
}

#[test]
fn read_capped_reports_a_timed_out_kind_before_the_deadline_as_deadline() {
    let mut r = FailsWith(|| io::Error::new(io::ErrorKind::TimedOut, "socket read timed out"));
    let got = read_capped(&mut r, 10, far());
    assert!(matches!(got, Err(ReadError::Deadline)), "{got:?}");
}

/// The other side of the branch: a non-timeout ureq error wrapped the same way stays an I/O error.
#[test]
fn read_capped_keeps_a_wrapped_non_timeout_ureq_error_as_io() {
    let mut r = FailsWith(|| ureq::Error::ConnectionFailed.into_io());
    let got = read_capped(&mut r, 10, far());
    assert!(matches!(got, Err(ReadError::Io(_))), "{got:?}");
}

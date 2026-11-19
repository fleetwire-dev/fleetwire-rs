//! Mock server with one accept thread plus one thread per connection.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::ops::{RangeFrom, RangeInclusive};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;

use crate::{Match, Respond, ResponseTemplate};

/// Cap on the request line and headers together. Over it, the server sends 400 and closes.
const MAX_HEAD_BYTES: usize = 64 * 1024;

/// One received request.
#[derive(Clone, Debug)]
pub struct Request {
    pub method: String,
    pub url: url::Url,
    pub headers: Headers,
    pub body: Vec<u8>,
}

impl Request {
    /// Looks up a header value (case-insensitive name, first value).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(HeaderValue::as_str)
    }
}

/// The received header list. The `get(..)?.to_str()` shape is kept the same as `http::HeaderMap`.
#[derive(Clone, Debug, Default)]
pub struct Headers(Vec<(String, HeaderValue)>);

impl Headers {
    /// The first value whose name is the same (case-insensitive).
    pub fn get(&self, name: &str) -> Option<&HeaderValue> {
        self.0
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &str)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_str()))
    }
}

/// One header value. The test server accepts UTF-8 only, so `to_str` never fails.
#[derive(Clone, Debug)]
pub struct HeaderValue(String);

impl HeaderValue {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn to_str(&self) -> Result<&str, std::convert::Infallible> {
        Ok(&self.0)
    }
}

/// Expected number of calls.
#[derive(Clone, Debug)]
pub struct Times {
    min: u64,
    max: Option<u64>,
}

impl From<u64> for Times {
    fn from(n: u64) -> Self {
        Self {
            min: n,
            max: Some(n),
        }
    }
}

impl From<RangeInclusive<u64>> for Times {
    fn from(r: RangeInclusive<u64>) -> Self {
        Self {
            min: *r.start(),
            max: Some(*r.end()),
        }
    }
}

impl From<RangeFrom<u64>> for Times {
    fn from(r: RangeFrom<u64>) -> Self {
        Self {
            min: r.start,
            max: None,
        }
    }
}

/// A mock that is still collecting matchers.
pub struct MockBuilder {
    matchers: Vec<Box<dyn Match>>,
}

/// A mock whose matchers and response are set.
pub struct Mock {
    matchers: Vec<Box<dyn Match>>,
    responder: Box<dyn Respond>,
    expect: Option<Times>,
    up_to: Option<u64>,
    priority: u8,
    name: Option<String>,
}

impl Mock {
    pub fn given(m: impl Match + 'static) -> MockBuilder {
        MockBuilder {
            matchers: vec![Box::new(m)],
        }
    }

    pub fn expect(mut self, n: impl Into<Times>) -> Self {
        self.expect = Some(n.into());
        self
    }

    pub fn up_to_n_times(mut self, n: u64) -> Self {
        self.up_to = Some(n);
        self
    }

    /// A smaller value is tried first. The default is 5 (same as wiremock).
    pub fn with_priority(mut self, p: u8) -> Self {
        self.priority = p;
        self
    }

    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    pub fn mount(self, server: &MockServer) {
        let mut mocks = lock(&server.shared.mocks);
        let seq = mocks.len();
        mocks.push(Mounted {
            mock: self,
            hits: 0,
            seq,
        });
    }
}

impl MockBuilder {
    pub fn and(mut self, m: impl Match + 'static) -> Self {
        self.matchers.push(Box::new(m));
        self
    }

    pub fn respond_with(self, r: impl Respond + 'static) -> Mock {
        Mock {
            matchers: self.matchers,
            responder: Box::new(r),
            expect: None,
            up_to: None,
            priority: 5,
            name: None,
        }
    }
}

struct Mounted {
    mock: Mock,
    hits: u64,
    seq: usize,
}

struct Shared {
    mocks: Mutex<Vec<Mounted>>,
    requests: Mutex<Vec<Request>>,
    stop: AtomicBool,
}

/// A mock server running on loopback. Dropping it checks the expected call counts and stops it.
pub struct MockServer {
    addr: SocketAddr,
    shared: Arc<Shared>,
}

/// Lock poisoning happens only on a test failure (a panic in another thread), so the contents
/// are used as they are.
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl MockServer {
    pub fn start() -> Self {
        let listener =
            TcpListener::bind("127.0.0.1:0").expect("loopback bind should succeed in tests");
        let addr = listener
            .local_addr()
            .expect("bound listener should have an address");
        let shared = Arc::new(Shared {
            mocks: Mutex::new(Vec::new()),
            requests: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
        });
        let accept_shared = Arc::clone(&shared);
        thread::spawn(move || {
            for stream in listener.incoming() {
                if accept_shared.stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(stream) = stream else { continue };
                let conn_shared = Arc::clone(&accept_shared);
                thread::spawn(move || handle(stream, addr, &conn_shared));
            }
        });
        Self { addr, shared }
    }

    pub fn uri(&self) -> String {
        format!("http://{}", self.addr)
    }

    pub fn address(&self) -> SocketAddr {
        self.addr
    }

    pub fn register(&self, mock: Mock) {
        mock.mount(self);
    }

    /// Every received request. Always `Some`, to keep the same shape as wiremock.
    pub fn received_requests(&self) -> Option<Vec<Request>> {
        Some(lock(&self.shared.requests).clone())
    }

    /// Panics when a mock broke its expected call count.
    pub fn verify(&self) {
        let mocks = lock(&self.shared.mocks);
        for m in mocks.iter() {
            let Some(t) = &m.mock.expect else { continue };
            let ok = m.hits >= t.min && t.max.is_none_or(|max| m.hits <= max);
            if !ok {
                let name = m.mock.name.clone().unwrap_or_else(|| format!("#{}", m.seq));
                let want = match t.max {
                    Some(max) if max == t.min => format!("{max}"),
                    Some(max) => format!("{}..={max}", t.min),
                    None => format!("{}..", t.min),
                };
                panic!("mock {name}: expected {want}, got {}", m.hits);
            }
        }
    }
}

impl Drop for MockServer {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        // Wakes the accept loop. Even if this fails, the thread ends with the process.
        let _ = TcpStream::connect(self.addr);
        if !thread::panicking() {
            self.verify();
        }
    }
}

/// Handles one connection: reads one request, writes one response and closes.
fn handle(stream: TcpStream, addr: SocketAddr, shared: &Shared) {
    let Ok(read_half) = stream.try_clone() else {
        return;
    };
    let mut reader = BufReader::new(read_half);
    let mut out = stream;
    let req = match read_request(&mut reader, addr) {
        Some(Ok(req)) => req,
        Some(Err(())) => {
            let _ = out.write_all(
                b"HTTP/1.1 400 Bad Request\r\ncontent-length: 0\r\nconnection: close\r\n\r\n",
            );
            return;
        }
        None => return,
    };
    lock(&shared.requests).push(req.clone());
    let template = {
        let mut mocks = lock(&shared.mocks);
        let mut order: Vec<usize> = (0..mocks.len()).collect();
        order.sort_by_key(|&i| (mocks[i].mock.priority, mocks[i].seq));
        let chosen = order.into_iter().find(|&i| {
            let m = &mocks[i];
            m.mock.up_to.is_none_or(|n| m.hits < n)
                && m.mock.matchers.iter().all(|x| x.matches(&req))
        });
        match chosen {
            Some(i) => {
                mocks[i].hits += 1;
                Some(mocks[i].mock.responder.respond(&req))
            }
            None => None,
        }
    };
    let template = template.unwrap_or_else(|| ResponseTemplate::new(404));
    write_response(&mut out, &template);
}

/// Reads a request. `None` when the connection closed right away, `Some(Err(()))` when the
/// request is malformed.
fn read_request(
    reader: &mut BufReader<TcpStream>,
    addr: SocketAddr,
) -> Option<Result<Request, ()>> {
    let mut head = Vec::new();
    let mut total = 0usize;
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(0) => {
                return if head.is_empty() { None } else { Some(Err(())) };
            }
            Ok(n) => total += n,
            Err(_) => return None,
        }
        if total > MAX_HEAD_BYTES {
            return Some(Err(()));
        }
        let line = line.trim_end_matches(['\r', '\n']).to_owned();
        if line.is_empty() {
            break;
        }
        head.push(line);
    }
    let mut lines = head.into_iter();
    let Some(request_line) = lines.next() else {
        return Some(Err(()));
    };
    let mut parts = request_line.split(' ');
    let (Some(method), Some(target)) = (parts.next(), parts.next()) else {
        return Some(Err(()));
    };
    let Ok(url) = url::Url::parse(&format!("http://{addr}{target}")) else {
        return Some(Err(()));
    };
    let mut headers = Vec::new();
    for l in lines {
        let Some((k, v)) = l.split_once(':') else {
            return Some(Err(()));
        };
        headers.push((k.trim().to_owned(), HeaderValue(v.trim().to_owned())));
    }
    let headers = Headers(headers);
    let len = headers
        .get("content-length")
        .map(|v| v.as_str().parse::<usize>());
    let mut body = Vec::new();
    match len {
        Some(Ok(n)) => {
            body.resize(n, 0);
            if reader.read_exact(&mut body).is_err() {
                return Some(Err(()));
            }
        }
        Some(Err(_)) => return Some(Err(())),
        None => {}
    }
    Some(Ok(Request {
        method: method.to_owned(),
        url,
        headers,
        body,
    }))
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        412 => "Precondition Failed",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "Unknown",
    }
}

/// Writes the response. Write errors are ignored (some tests have the client hang up first).
fn write_response(out: &mut TcpStream, t: &ResponseTemplate) {
    if let Some(d) = t.delay {
        thread::sleep(d);
    }
    let mut head = format!("HTTP/1.1 {} {}\r\n", t.status, reason(t.status));
    for (k, v) in &t.headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str(&format!(
        "content-length: {}\r\nconnection: close\r\n\r\n",
        t.body.len()
    ));
    if out.write_all(head.as_bytes()).is_err() || out.flush().is_err() {
        return;
    }
    let body: &[u8] = match t.cut_after {
        Some(n) => t.body.get(..n).unwrap_or(&t.body),
        None => &t.body,
    };
    match t.trickle {
        Some((chunk, gap)) => {
            for piece in body.chunks(chunk) {
                if out.write_all(piece).is_err() || out.flush().is_err() {
                    return;
                }
                thread::sleep(gap);
            }
        }
        None => {
            let _ = out.write_all(body);
        }
    }
    let _ = out.flush();
    let _ = out.shutdown(std::net::Shutdown::Both);
}

#[cfg(test)]
mod tests;

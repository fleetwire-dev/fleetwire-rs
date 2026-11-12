//! 수락 스레드 하나 + 연결마다 스레드 하나인 모의 서버입니다.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::ops::{RangeFrom, RangeInclusive};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread;

use crate::{Match, Respond, ResponseTemplate};

/// 요청 줄과 헤더를 합친 상한입니다. 넘으면 400 을 보내고 닫습니다.
const MAX_HEAD_BYTES: usize = 64 * 1024;

/// 받은 요청 하나입니다.
#[derive(Clone, Debug)]
pub struct Request {
    pub method: String,
    pub url: url::Url,
    pub headers: Headers,
    pub body: Vec<u8>,
}

impl Request {
    /// 헤더 값을 찾습니다(이름 대소문자 무시, 첫 값).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(HeaderValue::as_str)
    }
}

/// 받은 헤더 목록입니다. `get(..)?.to_str()` 모양은 `http::HeaderMap` 과 같게 맞춥니다.
#[derive(Clone, Debug, Default)]
pub struct Headers(Vec<(String, HeaderValue)>);

impl Headers {
    /// 이름(대소문자 무시)이 같은 첫 값입니다.
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

/// 헤더 값 하나입니다. 시험 서버는 UTF-8 로만 받으므로 `to_str` 는 실패하지 않습니다.
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

/// 기대 횟수입니다.
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

/// 매처를 모으는 중인 목입니다.
pub struct MockBuilder {
    matchers: Vec<Box<dyn Match>>,
}

/// 매처와 응답이 정해진 목입니다.
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

    /// 작을수록 먼저 봅니다. 기본은 5 입니다(wiremock 과 같음).
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

/// 루프백에서 도는 모의 서버입니다. 버리면 기대 횟수를 검사하고 멈춥니다.
pub struct MockServer {
    addr: SocketAddr,
    shared: Arc<Shared>,
}

/// 잠금 오염은 시험 실패(다른 스레드의 panic)에서만 생기므로 내용을 그대로 씁니다.
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

    /// 받은 요청 전부입니다. wiremock 과 모양을 맞추려고 언제나 `Some` 입니다.
    pub fn received_requests(&self) -> Option<Vec<Request>> {
        Some(lock(&self.shared.requests).clone())
    }

    /// 기대 횟수를 어긴 목이 있으면 panic 합니다.
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
        // 수락 루프를 깨웁니다. 실패해도 스레드는 프로세스와 함께 끝납니다.
        let _ = TcpStream::connect(self.addr);
        if !thread::panicking() {
            self.verify();
        }
    }
}

/// 연결 하나를 처리합니다. 요청 하나를 읽고 응답 하나를 쓴 뒤 닫습니다.
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

/// 요청을 읽습니다. 연결이 바로 닫혔으면 `None`, 형식이 틀렸으면 `Some(Err(()))` 입니다.
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

/// 응답을 씁니다. 쓰기 오류는 무시합니다(클라이언트가 먼저 끊는 시험이 있습니다).
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

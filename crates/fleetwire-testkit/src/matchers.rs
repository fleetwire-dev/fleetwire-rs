//! wiremock 과 같은 이름의 매처입니다.

use crate::{Match, Request};

struct MethodIs(String);
impl Match for MethodIs {
    fn matches(&self, req: &Request) -> bool {
        req.method.eq_ignore_ascii_case(&self.0)
    }
}

/// 메서드가 같으면 맞습니다(대소문자 무시).
pub fn method(m: impl AsRef<str>) -> impl Match + 'static {
    MethodIs(m.as_ref().to_owned())
}

struct PathIs(String);
impl Match for PathIs {
    fn matches(&self, req: &Request) -> bool {
        req.url.path() == self.0
    }
}

/// 경로가 정확히 같으면 맞습니다(질의 문자열은 보지 않습니다).
pub fn path(p: impl AsRef<str>) -> impl Match + 'static {
    PathIs(p.as_ref().to_owned())
}

struct HeaderIs(String, String);
impl Match for HeaderIs {
    fn matches(&self, req: &Request) -> bool {
        req.header(&self.0) == Some(self.1.as_str())
    }
}

/// 헤더 값이 정확히 같으면 맞습니다(이름은 대소문자 무시).
pub fn header(name: impl AsRef<str>, value: impl AsRef<str>) -> impl Match + 'static {
    HeaderIs(name.as_ref().to_owned(), value.as_ref().to_owned())
}

struct Any;
impl Match for Any {
    fn matches(&self, _req: &Request) -> bool {
        true
    }
}

/// 언제나 맞습니다.
pub fn any() -> impl Match + 'static {
    Any
}

struct BodyContains(String);
impl Match for BodyContains {
    fn matches(&self, req: &Request) -> bool {
        String::from_utf8_lossy(&req.body).contains(self.0.as_str())
    }
}

/// 본문(UTF-8 손실 변환)에 문자열이 들어 있으면 맞습니다.
pub fn body_string_contains(s: &str) -> impl Match + 'static {
    BodyContains(s.to_owned())
}

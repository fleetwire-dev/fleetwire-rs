//! Matchers with the same names as wiremock's.

use crate::{Match, Request};

struct MethodIs(String);
impl Match for MethodIs {
    fn matches(&self, req: &Request) -> bool {
        req.method.eq_ignore_ascii_case(&self.0)
    }
}

/// Matches when the method is the same (case-insensitive).
pub fn method(m: impl AsRef<str>) -> impl Match + 'static {
    MethodIs(m.as_ref().to_owned())
}

struct PathIs(String);
impl Match for PathIs {
    fn matches(&self, req: &Request) -> bool {
        req.url.path() == self.0
    }
}

/// Matches when the path is exactly the same (the query string is not looked at).
pub fn path(p: impl AsRef<str>) -> impl Match + 'static {
    PathIs(p.as_ref().to_owned())
}

struct HeaderIs(String, String);
impl Match for HeaderIs {
    fn matches(&self, req: &Request) -> bool {
        req.header(&self.0) == Some(self.1.as_str())
    }
}

/// Matches when the header value is exactly the same (the name is case-insensitive).
pub fn header(name: impl AsRef<str>, value: impl AsRef<str>) -> impl Match + 'static {
    HeaderIs(name.as_ref().to_owned(), value.as_ref().to_owned())
}

struct Any;
impl Match for Any {
    fn matches(&self, _req: &Request) -> bool {
        true
    }
}

/// Always matches.
pub fn any() -> impl Match + 'static {
    Any
}

struct BodyContains(String);
impl Match for BodyContains {
    fn matches(&self, req: &Request) -> bool {
        String::from_utf8_lossy(&req.body).contains(self.0.as_str())
    }
}

/// Matches when the body (lossy UTF-8 conversion) contains the string.
pub fn body_string_contains(s: &str) -> impl Match + 'static {
    BodyContains(s.to_owned())
}

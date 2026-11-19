//! Test-only std HTTP/1.1 mock server. It offers a synchronous API shaped like wiremock 0.6.
//!
//! One thread per connection and `connection: close` on every response. A request body is read
//! only by `Content-Length` (our client never sends chunked requests).
//!
//! Test-only. This server parses incoming input by hand, so a production crate must not take it
//! as a normal dependency: HTTP/1.1 server framing is on the list of things this project does
//! not build for production use.

// This is a test-only crate, so failing the test with panic! when an expectation is not met is
// the intended behaviour (same as wiremock's `expect`). Only this crate is exempted from the
// workspace panic threshold for product code (clippy::panic).
#![allow(clippy::panic)]

pub mod matchers;
mod response;
mod server;

pub use response::ResponseTemplate;
pub use server::{HeaderValue, Headers, Mock, MockBuilder, MockServer, Request, Times};

/// Decides whether one request meets a condition.
pub trait Match: Send + Sync {
    fn matches(&self, req: &Request) -> bool;
}

/// Builds the response to give to one request.
pub trait Respond: Send + Sync {
    fn respond(&self, req: &Request) -> ResponseTemplate;
}

impl Respond for ResponseTemplate {
    fn respond(&self, _req: &Request) -> ResponseTemplate {
        self.clone()
    }
}

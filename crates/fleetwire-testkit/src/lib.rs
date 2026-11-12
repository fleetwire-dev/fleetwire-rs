//! 시험 전용 std HTTP/1.1 모의 서버입니다. wiremock 0.6 과 같은 모양의 API 를 동기로 냅니다.
//!
//! 연결마다 스레드 하나, 응답마다 `connection: close` 입니다. 요청 본문은 `Content-Length` 로만 읽습니다
//! (우리 클라이언트는 chunked 요청을 보내지 않습니다).
//!
//! 시험 전용입니다. 들어오는 입력을 직접 파싱하는 서버이므로 프로덕션 크레이트의 정상 의존성으로 쓰지 않습니다
//! (`.claude/rules/dependencies.md` 「직접 만들지 않는 것」 — HTTP/1.1 서버 프레이밍).

// 시험 전용 크레이트라 기대가 어긋나면 panic! 으로 시험을 실패시키는 것이 정상 동작입니다(wiremock 의 `expect` 와 같음).
// 워크스페이스의 제품 코드 패닉 문턱(clippy::panic)에서 이 크레이트만 뺍니다.
#![allow(clippy::panic)]

pub mod matchers;
mod response;
mod server;

pub use response::ResponseTemplate;
pub use server::{HeaderValue, Headers, Mock, MockBuilder, MockServer, Request, Times};

/// 요청 하나가 조건에 맞는지 판정합니다.
pub trait Match: Send + Sync {
    fn matches(&self, req: &Request) -> bool;
}

/// 요청 하나에 줄 응답을 만듭니다.
pub trait Respond: Send + Sync {
    fn respond(&self, req: &Request) -> ResponseTemplate;
}

impl Respond for ResponseTemplate {
    fn respond(&self, _req: &Request) -> ResponseTemplate {
        self.clone()
    }
}

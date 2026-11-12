//! 응답 틀입니다.

use std::time::Duration;

/// 모의 서버가 돌려줄 응답 하나입니다.
#[derive(Clone, Debug)]
pub struct ResponseTemplate {
    pub(crate) status: u16,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
    pub(crate) delay: Option<Duration>,
    pub(crate) trickle: Option<(usize, Duration)>,
    pub(crate) cut_after: Option<usize>,
}

impl ResponseTemplate {
    pub fn new(status: u16) -> Self {
        Self {
            status,
            headers: Vec::new(),
            body: Vec::new(),
            delay: None,
            trickle: None,
            cut_after: None,
        }
    }

    pub fn set_body_raw(mut self, body: impl Into<Vec<u8>>, mime: &str) -> Self {
        self.body = body.into();
        self.insert_header("content-type", mime)
    }

    pub fn set_body_json(self, v: impl serde::Serialize) -> Self {
        let body = serde_json::to_vec(&v).expect("test values should serialize to JSON");
        self.set_body_raw(body, "application/json")
    }

    pub fn set_body_string(mut self, s: impl Into<String>) -> Self {
        self.body = s.into().into_bytes();
        if !self.headers.iter().any(|(k, _)| k == "content-type") {
            self.headers
                .push(("content-type".to_owned(), "text/plain".to_owned()));
        }
        self
    }

    /// 같은 이름이 있으면 바꿉니다. 이름은 소문자로 저장합니다.
    pub fn insert_header(mut self, name: impl AsRef<str>, value: impl AsRef<str>) -> Self {
        let name = name.as_ref().to_ascii_lowercase();
        self.headers.retain(|(k, _)| *k != name);
        self.headers.push((name, value.as_ref().to_owned()));
        self
    }

    /// 상태 줄을 보내기 전에 기다립니다.
    pub fn set_delay(mut self, d: Duration) -> Self {
        self.delay = Some(d);
        self
    }

    /// 헤더는 바로 보내고, 본문은 `chunk` 바이트마다 `gap` 만큼 쉬며 보냅니다.
    pub fn trickle_body(mut self, chunk: usize, gap: Duration) -> Self {
        self.trickle = Some((chunk.max(1), gap));
        self
    }

    /// `content-length` 는 전체 길이로 알리고, 본문을 `n` 바이트만 보낸 뒤 연결을 끊습니다.
    pub fn cut_after_body_bytes(mut self, n: usize) -> Self {
        self.cut_after = Some(n);
        self
    }
}

//! Response template.

use std::time::Duration;

/// One response the mock server will return.
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

    /// Replaces a header of the same name if there is one. The name is stored in lowercase.
    pub fn insert_header(mut self, name: impl AsRef<str>, value: impl AsRef<str>) -> Self {
        let name = name.as_ref().to_ascii_lowercase();
        self.headers.retain(|(k, _)| *k != name);
        self.headers.push((name, value.as_ref().to_owned()));
        self
    }

    /// Waits before sending the status line.
    pub fn set_delay(mut self, d: Duration) -> Self {
        self.delay = Some(d);
        self
    }

    /// Sends the headers at once, and the body with a pause of `gap` after every `chunk` bytes.
    pub fn trickle_body(mut self, chunk: usize, gap: Duration) -> Self {
        self.trickle = Some((chunk.max(1), gap));
        self
    }

    /// Announces the full length in `content-length`, sends only `n` bytes of the body and then
    /// closes the connection.
    pub fn cut_after_body_bytes(mut self, n: usize) -> Self {
        self.cut_after = Some(n);
        self
    }
}

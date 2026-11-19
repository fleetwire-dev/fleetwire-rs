# fleetwire A3 — HTTP 전송 계층 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** fleetwire 가 Tesla Fleet API 를 실제로 읽을 수 있게 하는 HTTP 전송 계층을 만든다 — 공개 API 로 end-to-end 동작하며, POST 는 토큰 refresh 한 곳뿐이고, Bearer 토큰이 Tesla 호스트 밖으로 나가지 않는다.

**Architecture:** `Client`(reqwest 소유 + 리전 캐시) → `transport`(타임아웃·크기상한·3xx 차단·헤더 파싱) → A1 의 `decode()` 게이트. 인증은 A2 `TokenManager` 에 세대(generation)와 회전 최소 간격을 추가해 동시 401 복구와 refresh 폭주를 동시에 막는다. 엔드포인트는 A1 의 `FleetGet`/`VehicleEndpoint` trait 을 **구현**만 한다(trait 신설 없음).

**Tech Stack:** Rust 2024 · MSRV 1.89.0 · reqwest(rustls) · tokio(sync,time) · wiremock(dev) · serde/serde_json · http · thiserror

**설계 스펙:** [`2026-08-02-fleetwire-a3-http-transport-design.md`](../specs/2026-08-02-fleetwire-a3-http-transport-design.md) — 이 계획의 §참조는 전부 그 문서를 가리킨다.

---

## Global Constraints

이 절의 항목은 **모든 태스크의 요구사항에 암묵적으로 포함**된다.

- **코드 레포는 `<workspace>`** (branch `main`, remote 없음). BASE = `40858d8`.
  ⚠️ `<docs repo>` 는 스펙·계획 보관처다. **코드 커밋 금지.**
- git author = `Jeong Seung Hwan <4435353+draken1@users.noreply.github.com>` (D7).
- **MSRV 1.89.0** · edition 2024 · toolchain `1.97.1`.
- **`unsafe_code = "forbid"`** (workspace lint). 우회 금지.
- **읽기 전용 불변식**: 공개 API 에 POST 생성 경로 없음. 크레이트 전체에서 POST 는 `auth/refresh.rs` 한 곳뿐.
- **`tokio` 런타임 의존은 `["sync","time"]` 유지.** `macros`/`rt`/`rt-multi-thread` 는 **dev-dependencies 에만**.
- **`Cargo.lock` 을 항상 함께 커밋한다.** 빠뜨리면 `--locked` 게이트 3개가 전부 깨진다(A2 에서 두 번 걸림).
- 테스트는 **모듈 내부 `#[cfg(test)]`** 로 작성한다. 이 크레이트에는 `tests/` 디렉터리가 없다(A2 carry-forward: 통합테스트는 `FixedClock` 을 못 써서 clock 주입이 불가).
- 모든 에러 문자열은 `redaction::body_preview` 를 통과한다. 새 타입은 `Debug` 를 손으로 쓰거나 자격증명 필드를 갖지 않는다.
- PATH: 새 셸마다 `export PATH="$HOME/.cargo/bin:$HOME/.local/bin:$PATH"`.
- `cargo fmt` 의 mod/import 알파벳 재정렬은 정상이다 — 리뷰어에 cosmetic 으로 미리 알릴 것.

### 매 태스크 게이트 (전부 통과해야 커밋)

```bash
cd <workspace>
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked
cargo +1.89.0 build --locked          # MSRV 게이트
```

### ⚠️ 매 태스크 brief 에 넣을 테스트 규율 5조 (A2 가 확립, 효과 실증됨)

1. **X 를 금지하는 규칙에는 X 를 했을 때만 실패하는 입력이 필요하다.**
2. **자기 테스트를 mutation-test 하고 생존자를 정직하게 분류하라.** 죽일 수 없으면 무엇이 필요한지와 그 비용을 대라 — **"안 하기로 했다"를 "할 수 없다"로 격상하지 말 것.**
3. **값이 아니라 기전을 단언하라.** 카운터로 호출 횟수를 세라. "에러가 났다"는 "무엇이 일어나지 않았다"를 증명하지 못한다.
4. **커버리지를 부풀리지 말 것.** 판별력 없는 입력은 제외하고, 예측한 실패가 안 나면 near-duplicate 대신 **mutation 으로 기존 커버리지를 증명**하라.
5. **vacuous 하게 통과할 수 있으면 큰 소리로 말하라** — 무력화되는 조건이 있으면 `eprintln!`.

---

## File Structure

| 파일 | 책임 | 태스크 |
|---|---|---|
| `clippy.toml` (신규, 레포 루트) | `disallowed-methods` — POST/PUT/PATCH/DELETE/HEAD/request 금지 | T1 |
| `Cargo.toml` ×2 | reqwest(런타임) · wiremock(dev) | T1 |
| `src/transport.rs` (신규) | 한 번의 HTTP 왕복: 타임아웃·크기상한·3xx 차단·헤더 파싱 → `decode()` 호출 | T2·T3 |
| `src/decode.rs` (수정) | `retry_after`·`message` 채우기, `dead_code` 해제 | T3 |
| `src/auth/refresh.rs` (신규) | `TokenRefresher` 의 reqwest 백엔드 — 유일한 POST | T4 |
| `src/auth/mod.rs` (수정) | 토큰 **세대** + **회전 최소 간격** | T5 |
| `src/client.rs` (신규) | `Client` + 빌더. reqwest 소유, 리전 캐시, 신뢰경계 검증 | T6·T7 |
| `src/api/{mod,users,vehicles}.rs` (신규) | 구체 엔드포인트 3개 (`FleetGet`/`VehicleEndpoint` **구현**) | T6 |
| `src/lib.rs` (수정) | 신규 모듈 export | T1·T6·T7 |

**태스크 순서 근거:** T1(의존성+가드) → T2(전송 뼈대) → T3(에러 성숙) → T4(refresh, 여기서 처음 토큰이 실린다) → T5(인증 정확성) → T6(엔드포인트+리전) → T7(신뢰경계·redirect — **토큰이 나가는 경로가 완성된 직후**) → T8(재시도) → T9(스코프 배선).

⚠️ T7 을 T6 뒤에 두는 이유: 리전 URL 을 **쓰기 시작하는 시점**이 T6 이므로, 검증 없는 상태로 남는 커밋 구간을 1개로 최소화한다. T6 은 부트스트랩 호스트만 쓰고 서버 URL 채택은 T7 에서 열린다.

---

### Task 1: 의존성 + read-only 3계층 가드

**Files:**
- Create: `clippy.toml`
- Modify: `Cargo.toml` (workspace), `crates/fleetwire/Cargo.toml`, `crates/fleetwire/src/lib.rs`
- Test: `crates/fleetwire/src/lib.rs` 내 `#[cfg(test)]`

**Interfaces:**
- Consumes: 없음 (첫 태스크)
- Produces: `reqwest` 사용 가능. `clippy.toml` 이 POST 계열을 차단. `wiremock` dev-dep 사용 가능.

⚠️ **`clippy.toml` 은 reqwest 를 추가하는 이 커밋에 반드시 함께 들어간다**(설계 §3, carry-forward #3). 나중에 붙이면 그 사이 커밋이 무방비다.

- [ ] **Step 1: `clippy.toml` 작성**

```toml
# fleetwire 는 GET 전용 읽기 라이브러리다(REQ-19 / design §3).
# 이것은 **보조** 방어선이다 — 본진은 타입 시스템(공개 API 에 POST 생성 경로 없음)과
# 모듈 가시성(reqwest 를 pub 로 노출하지 않음)이다. clippy 는
# `Request::new(Method::POST, ..)`·`RequestBuilder` 우회·`allow` 확산·다른 HTTP crate
# 추가를 막지 못한다. 실수 방지용이지 불변식의 증명이 아니다.
disallowed-methods = [
  { path = "reqwest::Client::post",    reason = "fleetwire 는 GET 전용" },
  { path = "reqwest::Client::put",     reason = "fleetwire 는 GET 전용" },
  { path = "reqwest::Client::patch",   reason = "fleetwire 는 GET 전용" },
  { path = "reqwest::Client::delete",  reason = "fleetwire 는 GET 전용" },
  { path = "reqwest::Client::head",    reason = "fleetwire 는 GET 전용" },
  { path = "reqwest::Client::request", reason = "메서드 임의 지정 우회 방지" },
]
```

- [ ] **Step 2: workspace `Cargo.toml` 에 clippy 린트를 켠다**

`[workspace.lints.rust]` 절 **아래**에 추가한다(기존 `unsafe_code = "forbid"` 는 그대로 둔다):

```toml
# 린트를 켜야 clippy.toml 이 효력을 갖는다.
[workspace.lints.clippy]
disallowed_methods = "deny"
```

- [ ] **Step 3: crate `Cargo.toml` 에 의존성 추가**

`[dependencies]` 에 추가:

```toml
# 유일한 HTTP 백엔드. default-features=false 로 native-tls·기본 압축을 끊고
# rustls 만 쓴다. ⚠️ feature unification 상 reqwest 가 tokio 의 net/rt/io-util 을
# 끌어오므로 mio/libc/socket2 가 lockfile 에 들어온다 — 수용된 예산이다(design §12).
reqwest = { version = "0.12", default-features = false, features = [
    "rustls-tls",
    "json",
    "stream",
] }
```

`[dev-dependencies]` 에 추가:

```toml
# HTTP 테스트 서버. dev 전용이라 소비자 트리에 들어가지 않는다.
wiremock = "0.6"
```

- [ ] **Step 4: 가드가 실제로 작동하는지 확인하는 테스트를 쓴다**

`crates/fleetwire/src/lib.rs` 맨 아래에 추가한다. 이 테스트는 **규율 1조**(X 를 금지하는 규칙엔 X 를 했을 때만 실패하는 입력)를 지킨다 — 파일이 존재하는지가 아니라 **여섯 메서드가 전부 목록에 있는지**를 본다.

```rust
#[cfg(test)]
mod readonly_guard_tests {
    /// `clippy.toml` 이 POST 계열 6종을 전부 막고 있는지 확인한다.
    ///
    /// 파일 존재만 보면 항목이 하나 빠져도 통과하므로(vacuous), 메서드별로 단언한다.
    /// ⚠️ 이 테스트는 clippy 가 실제로 그 규칙을 적용하는지는 증명하지 못한다 —
    /// 그건 `cargo clippy -D warnings` 게이트의 몫이다. 여기서 막는 것은
    /// "누군가 목록에서 한 줄을 지우는" 회귀다.
    #[test]
    fn clippy_toml_forbids_every_mutating_method() {
        let toml = include_str!("../../../clippy.toml");
        for m in [
            "reqwest::Client::post",
            "reqwest::Client::put",
            "reqwest::Client::patch",
            "reqwest::Client::delete",
            "reqwest::Client::head",
            "reqwest::Client::request",
        ] {
            assert!(toml.contains(m), "clippy.toml 에 {m} 금지 항목이 없다");
        }
    }
}
```

- [ ] **Step 5: 테스트가 실패하는지 먼저 확인한다**

`clippy.toml` 의 `reqwest::Client::head` 줄을 **임시로 주석 처리**하고:

Run: `cd <workspace> && cargo test --locked readonly_guard`
Expected: FAIL — `clippy.toml 에 reqwest::Client::head 금지 항목이 없다`

주석을 되돌린다.

- [ ] **Step 6: 전체 게이트 실행**

```bash
cd <workspace>
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked
cargo +1.89.0 build --locked
```
Expected: 전부 PASS. 테스트 수 227 → 228.

⚠️ `cargo clippy` 가 `disallowed_methods` 를 새로 켜면서 **기존 코드에서 예상치 못한 위반**을 낼 수 있다. 나오면 그 지점을 리포트에 적고 임의로 `allow` 하지 말 것.

- [ ] **Step 7: 커밋**

```bash
cd <workspace>
git add clippy.toml Cargo.toml Cargo.lock crates/fleetwire/Cargo.toml crates/fleetwire/src/lib.rs
git commit -m "feat(a3): reqwest 도입 + read-only clippy 가드 동시 투입

clippy.toml 을 reqwest 와 **같은 커밋**에 넣는다 — 나중에 붙이면 그 사이 커밋이
무방비다(design §3 / A2 carry-forward #3).

reqwest 는 default-features=false + rustls-tls·json·stream.
feature unification 으로 mio/libc/socket2 가 lockfile 에 들어오는 것은
수용된 예산이다(design §12)."
```

---

### Task 2: `transport` — 한 번의 HTTP 왕복

**Files:**
- Create: `crates/fleetwire/src/transport.rs`
- Modify: `crates/fleetwire/src/lib.rs` (`mod transport;` 추가 — `pub` 아님)
- Test: `crates/fleetwire/src/transport.rs` 내 `#[cfg(test)]`

**Interfaces:**
- Consumes: T1 의 `reqwest`·`wiremock`.
- Produces:
  ```rust
  pub(crate) struct RawResponse {
      pub status: http::StatusCode,
      pub content_type: Option<String>,
      pub body: Vec<u8>,
      pub retry_after: Option<std::time::Duration>,
      pub www_authenticate: Option<String>,
  }
  pub(crate) const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
  pub(crate) async fn execute_get(
      http: &reqwest::Client,
      url: reqwest::Url,
      bearer: &str,
      pricing: crate::pricing::Pricing,
      request_timeout: std::time::Duration,
  ) -> Result<RawResponse, crate::error::Error>;
  ```

이 태스크는 **재시도·리전·토큰 갱신을 모르는** 순수한 1회 왕복이다. 그 위 계층은 T5~T8 이 얹는다.

- [ ] **Step 1: 실패하는 테스트 4개를 쓴다**

`crates/fleetwire/src/transport.rs` 하단에:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::Pricing;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn client() -> reqwest::Client {
        // redirect 차단은 T7 에서 검증한다. 여기서는 기본 빌더로 충분하다.
        reqwest::Client::builder().build().unwrap()
    }

    /// 200 + JSON 이 그대로 실려 온다. Authorization 헤더가 붙는다.
    #[tokio::test]
    async fn get_carries_bearer_and_returns_body() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/1/vehicles"))
            .and(header("authorization", "Bearer tok-abc"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(br#"{"response":[]}"#.to_vec(), "application/json"),
            )
            .mount(&server)
            .await;

        let url = format!("{}/api/1/vehicles", server.uri()).parse().unwrap();
        let r = execute_get(&client(), url, "tok-abc", Pricing::NotCharged,
                            std::time::Duration::from_secs(5))
            .await
            .unwrap();

        assert_eq!(r.status.as_u16(), 200);
        assert_eq!(r.body, br#"{"response":[]}"#.to_vec());
        assert_eq!(r.content_type.as_deref(), Some("application/json"));
    }

    /// 실측 E1 케이스 B — 401 + text/html + 0바이트 + WWW-Authenticate.
    /// 헤더가 보존돼야 T3 가 error_description 을 꺼낼 수 있다.
    #[tokio::test]
    async fn preserves_www_authenticate_on_empty_html_401() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(401)
                    .insert_header("content-type", "text/html")
                    .insert_header(
                        "www-authenticate",
                        r#"Bearer realm="Doorkeeper", error="invalid_token", error_description="The access token is invalid""#,
                    )
                    .set_body_raw(Vec::new(), "text/html"),
            )
            .mount(&server)
            .await;

        let url = format!("{}/api/1/vehicles", server.uri()).parse().unwrap();
        let r = execute_get(&client(), url, "t", Pricing::NotCharged,
                            std::time::Duration::from_secs(5))
            .await
            .unwrap();

        assert_eq!(r.status.as_u16(), 401);
        assert!(r.body.is_empty(), "실측상 body 는 0바이트여야 한다");
        assert!(r.www_authenticate.unwrap().contains("The access token is invalid"));
    }

    /// Retry-After 의 delta-seconds 형식.
    #[tokio::test]
    async fn parses_retry_after_delta_seconds() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(429)
                    .insert_header("retry-after", "30")
                    .set_body_raw(b"{}".to_vec(), "application/json"),
            )
            .mount(&server)
            .await;

        let url = format!("{}/x", server.uri()).parse().unwrap();
        let r = execute_get(&client(), url, "t", Pricing::NotCharged,
                            std::time::Duration::from_secs(5))
            .await
            .unwrap();

        assert_eq!(r.retry_after, Some(std::time::Duration::from_secs(30)));
    }

    /// 크기 상한은 **누적 중** 끊겨야 한다 — buffer-then-check 금지(design §9).
    /// 판별 방법: 상한보다 훨씬 큰 body 를 주고, 반환된 에러가 크기 초과인지 본다.
    #[tokio::test]
    async fn aborts_while_streaming_when_body_exceeds_cap() {
        let server = MockServer::start().await;
        let huge = vec![b'x'; MAX_RESPONSE_BYTES + 1024];
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_raw(huge, "application/json"))
            .mount(&server)
            .await;

        let url = format!("{}/x", server.uri()).parse().unwrap();
        let err = execute_get(&client(), url, "t", Pricing::NotCharged,
                              std::time::Duration::from_secs(10))
            .await
            .unwrap_err();

        let msg = err.to_string();
        assert!(msg.contains("response too large"), "실제: {msg}");
    }
}
```

- [ ] **Step 2: 테스트가 실패하는지 확인한다**

Run: `cd <workspace> && cargo test --locked transport::`
Expected: FAIL — `cannot find function execute_get` / `cannot find type RawResponse`

- [ ] **Step 3: 최소 구현을 쓴다**

```rust
//! 한 번의 HTTP GET 왕복. 재시도·리전 해석·토큰 갱신을 **모른다** —
//! 그 위 계층(`client`)이 얹는다.

use std::time::Duration;

use futures_util::StreamExt as _;

use crate::error::Error;
use crate::pricing::Pricing;
use crate::redaction::body_preview;

/// 응답 본문 상한. 초과하면 **누적 도중** 끊는다.
pub(crate) const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

/// 에러 메시지 길이 상한(A1 `decode` 와 같은 값을 쓴다).
const MSG_MAX: usize = 512;

/// 한 번의 왕복 결과. 해석은 `decode` 가 한다.
pub(crate) struct RawResponse {
    pub status: http::StatusCode,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
    pub retry_after: Option<Duration>,
    pub www_authenticate: Option<String>,
}

/// `Retry-After` 를 파싱한다. delta-seconds 만 이 단계에서 처리하고
/// HTTP-date 는 T3 에서 더한다.
fn parse_retry_after(raw: &str) -> Option<Duration> {
    raw.trim().parse::<u64>().ok().map(Duration::from_secs)
}

pub(crate) async fn execute_get(
    http: &reqwest::Client,
    url: reqwest::Url,
    bearer: &str,
    pricing: Pricing,
    request_timeout: Duration,
) -> Result<RawResponse, Error> {
    let sent = tokio::time::timeout(
        request_timeout,
        http.get(url).bearer_auth(bearer).send(),
    )
    .await;

    let resp = match sent {
        // 타임아웃: 서버가 이미 처리했을 수 있다 → Unknown(재시도 금지).
        Err(_) => {
            return Err(Error::TransportUnknown {
                message: body_preview("request timed out", MSG_MAX),
                pricing,
            });
        }
        Ok(Err(e)) => {
            // connect 실패는 전송 전이 확실하고, 그 외(read timeout·reset)는 불명이다.
            let message = body_preview(&crate::redaction::redact_transport_error(&e), MSG_MAX);
            return Err(if e.is_connect() {
                Error::TransportNotSent { message, pricing }
            } else {
                Error::TransportUnknown { message, pricing }
            });
        }
        Ok(Ok(r)) => r,
    };

    let status = resp.status();
    let header = |k: &str| {
        resp.headers()
            .get(k)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let content_type = header("content-type");
    let www_authenticate = header("www-authenticate");
    let retry_after = header("retry-after").as_deref().and_then(parse_retry_after);

    // ⚠️ buffer-then-check 금지 — 누적하며 상한을 검사한다.
    let mut body = Vec::new();
    let mut stream = resp.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| Error::TransportUnknown {
            message: body_preview(&crate::redaction::redact_transport_error(&e), MSG_MAX),
            pricing,
        })?;
        if body.len() + chunk.len() > MAX_RESPONSE_BYTES {
            return Err(Error::TransportUnknown {
                message: body_preview(
                    &format!("response too large (cap {MAX_RESPONSE_BYTES} bytes)"),
                    MSG_MAX,
                ),
                pricing,
            });
        }
        body.extend_from_slice(&chunk);
    }

    Ok(RawResponse {
        status,
        content_type,
        body,
        retry_after,
        www_authenticate,
    })
}
```

`Cargo.toml` `[dependencies]` 에 추가:

```toml
# reqwest 의 bytes_stream() 을 순회하기 위한 StreamExt.
futures-util = { version = "0.3", default-features = false }
```

`redaction.rs` 에 추가(⚠️ **`redaction` 모듈 안에** 둔다 — design §8, A2 에서 이 계약이 한 번 깨졌다):

```rust
/// `reqwest::Error` 를 사람이 읽을 문자열로 바꾸되 URL·source chain 을 리댁션한다.
///
/// `reqwest::Error` 의 `Display` 는 요청 URL 을 그대로 넣는데, 그 URL 에는 VIN 이
/// path segment 로 들어 있을 수 있다. OSS 사용자는 에러 메시지를 GitHub 이슈에
/// 그대로 붙여넣으므로(design §8) 여기서 끊는다.
pub(crate) fn redact_transport_error(e: &reqwest::Error) -> String {
    let mut s = e.to_string();
    let mut src: Option<&dyn std::error::Error> = std::error::Error::source(e);
    while let Some(inner) = src {
        s.push_str(": ");
        s.push_str(&inner.to_string());
        src = inner.source();
    }
    redact_text(&s)
}
```

`lib.rs` 에 `mod transport;` 를 추가한다(**`pub` 아님** — 가시성이 read-only 강제의 2계층이다).

- [ ] **Step 4: 테스트 통과 확인**

Run: `cd <workspace> && cargo test --locked transport::`
Expected: 4개 전부 PASS

- [ ] **Step 5: mutation-test 하고 생존자를 분류한다**

최소 아래 4종을 주입해 각각 테스트가 죽는지 확인하고, 리포트에 **kill/생존**을 적는다.

| # | mutation | 죽어야 하는 테스트 |
|---|---|---|
| M1 | `body.len() + chunk.len() > MAX` → `>=` | 크기 상한 (경계) |
| M2 | 크기 검사를 루프 **밖**으로 옮김(buffer-then-check) | `aborts_while_streaming_...` |
| M3 | `www_authenticate` 를 항상 `None` 으로 | 빈 401 테스트 |
| M4 | `e.is_connect()` 분기를 뒤집음 | (생존 예상 — 현재 테스트가 없음) |

⚠️ M4 가 생존하면 **"할 수 없다"가 아니라 "안 했다"**로 정직하게 적고, 무엇이 필요한지(connect 실패를 유발하는 픽스처 = 닫힌 포트로 요청) 쓸 것.

- [ ] **Step 6: 전체 게이트 + 커밋**

```bash
cd <workspace>
cargo fmt --all && cargo clippy --all-targets --all-features --locked -- -D warnings && cargo test --locked && cargo +1.89.0 build --locked
git add Cargo.lock crates/fleetwire/Cargo.toml crates/fleetwire/src/transport.rs crates/fleetwire/src/redaction.rs crates/fleetwire/src/lib.rs
git commit -m "feat(a3): transport — 1회 GET 왕복 (크기상한 누적검사·헤더 보존)

buffer-then-check 를 금지하고 bytes_stream 누적 중 상한을 검사한다.
401 이 text/html+0바이트로 오는 실측 케이스에서도 WWW-Authenticate 를
보존해 T3 가 error_description 을 꺼낼 수 있게 한다.

redact_transport_error 는 redaction 모듈 **안에** 둔다 — body_preview 만이
안전한 진입점이라는 A1 의 타입 계약을 지키기 위해서다(design §8)."
```

---

### Task 3: 에러 성숙 — `message` 채우기 + `Retry-After` HTTP-date + 3xx 차단

**Files:**
- Modify: `crates/fleetwire/src/decode.rs`, `crates/fleetwire/src/transport.rs`
- Test: 두 파일의 `#[cfg(test)]`

**Interfaces:**
- Consumes: T2 의 `RawResponse{ status, content_type, body, retry_after, www_authenticate }`.
- Produces:
  ```rust
  // decode.rs — 시그니처가 바뀐다. 호출자는 T6 이후 client.rs 뿐이다.
  pub(crate) fn decode<T: serde::de::DeserializeOwned>(
      status: http::StatusCode,
      content_type: Option<&str>,
      body: &[u8],
      pricing: Pricing,
      retry_after: Option<Duration>,      // 신규
      www_authenticate: Option<&str>,     // 신규
  ) -> Result<T, Error>;
  // transport.rs
  pub(crate) fn parse_retry_after(raw: &str) -> Option<Duration>;  // HTTP-date 도 처리
  ```

⚠️ `decode()` 의 `#[allow(dead_code)]` 와 `TODO(a3)` 주석을 **이 태스크에서 제거**한다.

⚠️ **`decode()` 의 status-first 순서를 절대 바꾸지 말 것.** `if status >= 400` 이 content-type 검사보다 **먼저** 오기 때문에 401+`text/html`+빈 body 가 `Error::Decode`(=`Billed` 오분류)로 새지 않는다. 이 순서가 계약이다(design §7.1).

- [ ] **Step 1: 실패하는 테스트 5개를 쓴다**

`decode.rs` 의 기존 `#[cfg(test)] mod tests` 에 추가:

```rust
    /// 실측 E1 케이스 A — body 의 `error` 가 message 로 올라온다.
    #[test]
    fn api_message_comes_from_body_error_field() {
        let body = br#"{"error":"invalid bearer token"}"#;
        let e = decode::<serde_json::Value>(
            http::StatusCode::from_u16(403).unwrap(),
            Some("application/json"),
            body,
            Pricing::NotCharged,
            None,
            None,
        )
        .unwrap_err();
        assert!(e.to_string().contains("invalid bearer token"), "실제: {e}");
    }

    /// 실측 E1 케이스 B — body 가 비어 있으면 WWW-Authenticate 의
    /// error_description 이 message 가 된다. body 만 읽는 파서는 여기서 빈손이다.
    #[test]
    fn api_message_falls_back_to_www_authenticate() {
        let e = decode::<serde_json::Value>(
            http::StatusCode::from_u16(403).unwrap(),
            Some("text/html"),
            b"",
            Pricing::NotCharged,
            None,
            Some(r#"Bearer realm="Doorkeeper", error="invalid_token", error_description="The access token is invalid""#),
        )
        .unwrap_err();
        assert!(e.to_string().contains("The access token is invalid"), "실제: {e}");
    }

    /// 둘 다 없으면 canonical_reason 으로 떨어진다.
    #[test]
    fn api_message_falls_back_to_canonical_reason() {
        let e = decode::<serde_json::Value>(
            http::StatusCode::from_u16(502).unwrap(),
            Some("text/html"),
            b"",
            Pricing::NotCharged,
            None,
            None,
        )
        .unwrap_err();
        assert!(e.to_string().contains("Bad Gateway"), "실제: {e}");
    }

    /// ⚠️ 401 + text/html + 0바이트가 Decode 로 새지 않는다(과금 오분류 방지).
    /// status 검사가 content-type 검사보다 먼저라는 계약을 고정한다.
    #[test]
    fn empty_html_401_is_auth_not_decode() {
        let e = decode::<serde_json::Value>(
            http::StatusCode::from_u16(401).unwrap(),
            Some("text/html"),
            b"",
            Pricing::data(),
            None,
            None,
        )
        .unwrap_err();
        assert!(matches!(e, Error::Auth { .. }), "실제: {e:?}");
        assert!(matches!(e.billing(), crate::pricing::Billing::Billed));
    }

    /// retry_after 가 Error::Api 로 전달되고 120초 상한이 걸린다.
    #[test]
    fn retry_after_is_carried_and_capped() {
        let e = decode::<serde_json::Value>(
            http::StatusCode::from_u16(429).unwrap(),
            Some("application/json"),
            b"{}",
            Pricing::NotCharged,
            Some(Duration::from_secs(600)),
            None,
        )
        .unwrap_err();
        assert_eq!(e.retry_after(), Some(Duration::from_secs(120)));
    }
```

`transport.rs` 의 `#[cfg(test)] mod tests` 에 추가:

```rust
    /// RFC 9110 은 Retry-After 에 HTTP-date 도 허용한다.
    /// 빠뜨리면 조용히 None 이 되어 상한이 무의미해진다.
    #[test]
    fn parses_retry_after_http_date() {
        // 과거 시각 → 0초(음수로 만들지 않는다).
        assert_eq!(
            parse_retry_after("Wed, 21 Oct 2015 07:28:00 GMT"),
            Some(std::time::Duration::ZERO)
        );
        // 형식이 아니면 None.
        assert_eq!(parse_retry_after("not-a-date"), None);
    }

    /// 3xx 는 decode 의 `>=400` 에 안 걸려 Decode 로 샌다. transport 가 막는다.
    #[tokio::test]
    async fn redirect_status_becomes_api_error_not_decode() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(302).insert_header("location", "https://evil.example/"),
            )
            .mount(&server)
            .await;

        let url = format!("{}/x", server.uri()).parse().unwrap();
        let err = execute_get(&client(), url, "t", Pricing::NotCharged,
                              std::time::Duration::from_secs(5))
            .await
            .unwrap_err();

        assert!(matches!(err, crate::error::Error::Api { status, .. } if status.as_u16() == 302),
                "실제: {err:?}");
    }
```

- [ ] **Step 2: 테스트가 실패하는지 확인한다**

Run: `cd <workspace> && cargo test --locked decode:: transport::`
Expected: FAIL — `decode` 인자 개수 불일치, `parse_retry_after` 비공개, 3xx 테스트는 `unwrap_err` 에서 panic

- [ ] **Step 3: 구현한다**

`transport.rs`:

```rust
/// `Retry-After` 파싱 — delta-seconds 와 HTTP-date 둘 다(RFC 9110).
///
/// HTTP-date 를 빠뜨리면 조용히 `None` 이 되어 상한이 무의미해진다.
/// 과거 시각은 음수가 아니라 `ZERO` 로 접는다.
pub(crate) fn parse_retry_after(raw: &str) -> Option<Duration> {
    let raw = raw.trim();
    if let Ok(secs) = raw.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    let when = httpdate::parse_http_date(raw).ok()?;
    Some(
        when.duration_since(std::time::SystemTime::now())
            .unwrap_or(Duration::ZERO),
    )
}
```

`execute_get` 의 status 획득 직후에 3xx 차단을 넣는다(⚠️ **body 를 읽기 전에**):

```rust
    // 3xx 는 decode 의 `>=400` 게이트에 안 걸려 content-type 검사로 내려가고
    // Error::Decode 가 된다. redirect 를 따라가지 않기로 했으므로(design §4.6)
    // 여기서 Error::Api 로 만들어 시끄럽게 실패시킨다.
    if status.is_redirection() {
        return Err(Error::Api {
            status,
            message: body_preview(
                &format!("unexpected redirect ({status}); fleetwire does not follow redirects"),
                MSG_MAX,
            ),
            retry_after,
            body_preview: String::new(),
            pricing,
        });
    }
```

`decode.rs` — 시그니처에 두 인자를 더하고, `_ => Error::Api` arm 의 `message`/`retry_after` 를 바꾼다:

```rust
/// Tesla 응답에서 사람이 읽을 메시지를 뽑는다.
///
/// 우선순위: body JSON 의 `error` → `WWW-Authenticate` 의 `error_description`
/// → `canonical_reason()`.
///
/// ⚠️ 2단계가 필요한 이유(실측 2026-08-01): Doorkeeper 상류가 답하는 401 은
/// body 가 **0바이트**고 `error_description` 이 헤더에만 있다. body 만 읽는
/// 파서는 그 경우 아무것도 못 얻는다.
///
/// ⚠️ 이 값은 **분류에 쓰지 않는다** — 같은 401 이라도 상류에 따라 문구가
/// `invalid authentication` / `invalid bearer token` 으로 갈리는 구현 세부다.
/// 분류는 status 로만 한다(design §7).
fn error_message(body: &[u8], www_authenticate: Option<&str>, status: http::StatusCode) -> String {
    if let Ok(v) = serde_json::from_slice::<serde_json::Value>(body)
        && let Some(s) = v.get("error").and_then(|e| e.as_str())
    {
        return s.to_owned();
    }
    if let Some(desc) = www_authenticate.and_then(extract_error_description) {
        return desc;
    }
    status.canonical_reason().unwrap_or("unknown error").to_owned()
}

/// `WWW-Authenticate` 의 `error_description="..."` 를 꺼낸다.
/// quoted-string 안의 이스케이프(`\"`)를 존중한다.
fn extract_error_description(header: &str) -> Option<String> {
    let start = header.find("error_description=\"")? + "error_description=\"".len();
    let rest = &header[start..];
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push(chars.next()?),
            '"' => return Some(out),
            _ => out.push(c),
        }
    }
    None // 닫는 따옴표가 없으면 신뢰하지 않는다
}
```

`_ => Error::Api` arm 을 다음으로 바꾼다:

```rust
            _ => Error::Api {
                status,
                message: body_preview(
                    &error_message(body, www_authenticate, status),
                    BODY_PREVIEW_MAX,
                ),
                retry_after,
                body_preview: body_preview(&String::from_utf8_lossy(body), BODY_PREVIEW_MAX),
                pricing,
            },
```

`#[allow(dead_code)]` 와 그 위의 `TODO(a3)` 주석 블록을 **삭제**한다.

`Cargo.toml` `[dependencies]` 에 추가:

```toml
# Retry-After 의 HTTP-date 형식 파싱(RFC 9110). stdlib 에 날짜 파서가 없다.
httpdate = "1"
```

- [ ] **Step 4: 테스트 통과 확인**

Run: `cd <workspace> && cargo test --locked`
Expected: 전부 PASS. `decode` 의 기존 테스트들도 새 인자(`None, None`)로 고쳐야 한다 — 고칠 때 **단언은 바꾸지 말 것**.

- [ ] **Step 5: mutation-test**

| # | mutation | 죽어야 하는 테스트 |
|---|---|---|
| M1 | `error_message` 의 1·2 순위를 맞바꿈 | `..._from_body_error_field` |
| M2 | `extract_error_description` 이 항상 `None` | `..._falls_back_to_www_authenticate` |
| M3 | `decode` 의 `if status >= 400` 을 content-type 검사 **뒤로** 이동 | `empty_html_401_is_auth_not_decode` |
| M4 | `is_redirection()` 차단 제거 | `redirect_status_becomes_api_error_not_decode` |
| M5 | `parse_retry_after` 의 HTTP-date 분기 제거 | `parses_retry_after_http_date` |

**M3 이 가장 중요하다** — 과금 오분류를 막는 순서 계약이다. 죽지 않으면 테스트가 약한 것이다.

- [ ] **Step 6: 전체 게이트 + 커밋**

```bash
cd <workspace>
cargo fmt --all && cargo clippy --all-targets --all-features --locked -- -D warnings && cargo test --locked && cargo +1.89.0 build --locked
git add Cargo.lock crates/fleetwire/Cargo.toml crates/fleetwire/src/decode.rs crates/fleetwire/src/transport.rs
git commit -m "feat(a3): Error::Api.message 채우기 + Retry-After HTTP-date + 3xx 차단

message 우선순위 = body error → WWW-Authenticate error_description →
canonical_reason. 2단계가 필요한 이유는 실측이다 — Doorkeeper 상류의 401 은
body 가 0바이트고 error_description 이 헤더에만 있다.

decode 의 status-first 순서를 테스트로 고정했다. 이 순서가 401+text/html+
빈 body 를 Decode(=Billed 오분류)로 새지 않게 하는 계약이다.

3xx 는 decode 의 >=400 게이트에 안 걸리므로 transport 가 Error::Api 로 막는다."
```

---

### Task 4: `TokenRefresher` 의 reqwest 백엔드 — 크레이트 유일의 POST

**Files:**
- Create: `crates/fleetwire/src/auth/refresh.rs`
- Modify: `crates/fleetwire/src/auth/mod.rs` (`mod refresh;` + re-export), `crates/fleetwire/src/lib.rs`
- Test: `crates/fleetwire/src/auth/refresh.rs` 내 `#[cfg(test)]`

**Interfaces:**
- Consumes: A2 의 `TokenRefresher` trait, `Refreshed::new(access, refresh, expires_in_secs)`. T2 의 `MAX_RESPONSE_BYTES`.
- Produces:
  ```rust
  pub struct HttpRefresher { /* 비공개 */ }
  impl HttpRefresher {
      pub fn new(client_id: String) -> Self;
      pub fn with_token_url(self, url: reqwest::Url) -> Self;   // 테스트용
      pub fn with_timeout(self, d: Duration) -> Self;
  }
  impl TokenRefresher for HttpRefresher { /* refresh() */ }
  pub const REFRESH_TIMEOUT: Duration = Duration::from_secs(8);
  ```

- [ ] **Step 1: 실패하는 테스트 4개를 쓴다**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_string_contains, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn refresher(server: &MockServer) -> HttpRefresher {
        HttpRefresher::new("cid-123".into())
            .with_token_url(format!("{}/oauth2/v3/token", server.uri()).parse().unwrap())
    }

    /// 성공 경로 — form-urlencoded, public client(secret 없음).
    #[tokio::test]
    async fn posts_form_urlencoded_without_client_secret() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/oauth2/v3/token"))
            .and(header("content-type", "application/x-www-form-urlencoded"))
            .and(body_string_contains("grant_type=refresh_token"))
            .and(body_string_contains("client_id=cid-123"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                br#"{"access_token":"new-a","refresh_token":"new-r","expires_in":28800}"#.to_vec(),
                "application/json",
            ))
            .mount(&server)
            .await;

        let got = refresher(&server).refresh("old-r").await.unwrap();
        assert_eq!(got.access_token, "new-a");
        assert_eq!(got.refresh_token, "new-r");
        assert_eq!(got.expires_in_secs, 28_800);
    }

    /// ⚠️ client_secret 을 절대 보내지 않는다(public client).
    #[tokio::test]
    async fn never_sends_client_secret() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                br#"{"access_token":"a","refresh_token":"r","expires_in":1}"#.to_vec(),
                "application/json",
            ))
            .mount(&server)
            .await;

        let _ = refresher(&server).refresh("old-r").await;

        let reqs = server.received_requests().await.unwrap();
        let body = String::from_utf8_lossy(&reqs[0].body).to_string();
        assert!(!body.contains("client_secret"), "본문: {body}");
    }

    /// OAuth 오류 응답이 리댁션된 Error 로 올라온다.
    #[tokio::test]
    async fn oauth_error_becomes_error_not_panic() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(400).set_body_raw(
                br#"{"error":"invalid_grant"}"#.to_vec(),
                "application/json",
            ))
            .mount(&server)
            .await;

        let e = refresher(&server).refresh("old-r").await.unwrap_err();
        assert!(e.to_string().contains("invalid_grant"), "실제: {e}");
    }

    /// ⚠️ 토큰이 에러 메시지에 새지 않는다.
    #[tokio::test]
    async fn refresh_token_never_appears_in_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(500).set_body_raw(
                b"boom".to_vec(),
                "text/plain",
            ))
            .mount(&server)
            .await;

        let e = refresher(&server).refresh("SUPERSECRET-REFRESH").await.unwrap_err();
        assert!(!format!("{e}{e:?}").contains("SUPERSECRET-REFRESH"), "실제: {e:?}");
    }
}
```

- [ ] **Step 2: 실패 확인**

Run: `cd <workspace> && cargo test --locked auth::refresh`
Expected: FAIL — `cannot find type HttpRefresher`

- [ ] **Step 3: 구현한다**

```rust
//! `TokenRefresher` 의 reqwest 백엔드 — **크레이트 유일의 POST**.
//!
//! ## 왜 `fleet-auth…` 하드코딩인가 (OIDC discovery 를 따르지 않는 이유)
//!
//! 2026-08-01 실측: `https://fleet-auth.prd.vn.cloud.tesla.com/oauth2/v3/.well-known/
//! openid-configuration` 은 `token_endpoint = https://auth.tesla.com/oauth2/v3/token`
//! 을 광고하고 `grant_types_supported` 에 `refresh_token` 이 **없다**. 그런데 실제
//! refresh 는 `fleet-auth…` 로 보내야 성공한다. 게다가 실토큰의 `iss` 는
//! `https://auth.tesla.com/oauth2/v3/nts` 로 discovery 의 `issuer` 와 **불일치**한다.
//!
//! → **discovery 는 이 엔드포인트의 권위가 아니다.** "정확성"을 이유로 discovery 를
//!   도입하거나 엄격한 issuer 검증을 넣으면 **refresh 가 깨지고 자기 토큰을 거부**한다.

use std::time::Duration;

use crate::auth::{Refreshed, TokenRefresher};
use crate::error::Error;
use crate::redaction::{body_preview, redact_transport_error};

/// refresh 요청 timeout.
///
/// ⚠️ `TokenManager` 의 `state_deadline`(10초)보다 **짧아야 한다**. tokio state
/// mutex 가 이 POST 를 가로질러 유지되므로, 10초를 넘으면 느리지만 성공하는
/// refresh 도중에 형제 호출자들이 `token state lock:` 오류를 받는다(design §9.1).
pub const REFRESH_TIMEOUT: Duration = Duration::from_secs(8);

const TOKEN_URL: &str = "https://fleet-auth.prd.vn.cloud.tesla.com/oauth2/v3/token";
const MSG_MAX: usize = 512;

/// 응답 JSON. `expires_in` 이 없으면 0 으로 두고 상위(A2)의 하한 정책에 맡긴다.
#[derive(serde::Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: String,
    #[serde(default)]
    expires_in: u64,
}

pub struct HttpRefresher {
    http: reqwest::Client,
    client_id: String,
    token_url: reqwest::Url,
    timeout: Duration,
}

impl HttpRefresher {
    pub fn new(client_id: String) -> Self {
        Self {
            http: reqwest::Client::builder()
                // redirect 를 따라가면 자격증명이 다른 호스트로 갈 수 있다(design §4.6).
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("rustls client builds"),
            client_id,
            token_url: TOKEN_URL.parse().expect("const URL parses"),
            timeout: REFRESH_TIMEOUT,
        }
    }

    /// 테스트에서 mock 서버를 가리키게 한다.
    pub fn with_token_url(mut self, url: reqwest::Url) -> Self {
        self.token_url = url;
        self
    }

    pub fn with_timeout(mut self, d: Duration) -> Self {
        self.timeout = d;
        self
    }
}

/// ⚠️ 토큰·비밀은 절대 찍지 않는다.
impl core::fmt::Debug for HttpRefresher {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("HttpRefresher")
            .field("token_url", &self.token_url.as_str())
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

impl TokenRefresher for HttpRefresher {
    fn refresh<'a>(
        &'a self,
        refresh_token: &'a str,
    ) -> std::pin::Pin<Box<dyn Future<Output = Result<Refreshed, Error>> + Send + 'a>> {
        Box::pin(async move {
            let form = [
                ("grant_type", "refresh_token"),
                ("client_id", self.client_id.as_str()),
                ("refresh_token", refresh_token),
            ];

            // 이 크레이트에서 POST 가 허용되는 **유일한 지점**이다.
            // clippy.toml 이 나머지 전부를 막는다(design §3).
            #[allow(clippy::disallowed_methods)]
            let fut = self.http.post(self.token_url.clone()).form(&form).send();

            let resp = match tokio::time::timeout(self.timeout, fut).await {
                Err(_) => {
                    // ⚠️ 서버가 회전을 완료했는데 응답만 유실됐을 수 있다.
                    // 그 경우 우리가 가진 refresh_token 은 이미 무효고 복구는
                    // 재-OAuth 뿐이다. **재시도하지 않는다** — 재시도는 악화다.
                    return Err(Error::Auth {
                        message: body_preview("token refresh timed out", MSG_MAX),
                        pricing: None,
                    });
                }
                Ok(Err(e)) => {
                    return Err(Error::Auth {
                        message: body_preview(&redact_transport_error(&e), MSG_MAX),
                        pricing: None,
                    });
                }
                Ok(Ok(r)) => r,
            };

            let status = resp.status();
            let bytes = resp.bytes().await.map_err(|e| Error::Auth {
                message: body_preview(&redact_transport_error(&e), MSG_MAX),
                pricing: None,
            })?;
            let body = &bytes[..bytes.len().min(crate::transport::MAX_RESPONSE_BYTES)];

            if !status.is_success() {
                return Err(Error::Auth {
                    message: body_preview(
                        &format!("token refresh failed: HTTP {status}: {}",
                                 String::from_utf8_lossy(body)),
                        MSG_MAX,
                    ),
                    pricing: None,
                });
            }

            let parsed: TokenResponse =
                serde_json::from_slice(body).map_err(|e| Error::Auth {
                    message: body_preview(&format!("token refresh response: {e}"), MSG_MAX),
                    pricing: None,
                })?;

            Ok(Refreshed::new(
                parsed.access_token,
                parsed.refresh_token,
                parsed.expires_in,
            ))
        })
    }
}
```

`auth/mod.rs` 에 `mod refresh;` + `pub use refresh::{HttpRefresher, REFRESH_TIMEOUT};`, `lib.rs` 의 re-export 목록에 `HttpRefresher` 추가.

- [ ] **Step 4: 통과 확인**

Run: `cd <workspace> && cargo test --locked auth::refresh`
Expected: 4개 전부 PASS

- [ ] **Step 5: mutation-test**

| # | mutation | 죽어야 하는 테스트 |
|---|---|---|
| M1 | form 에 `("client_secret","x")` 추가 | `never_sends_client_secret` |
| M2 | `.form(&form)` → `.json(&...)` (content-type 변경) | `posts_form_urlencoded_...` |
| M3 | 에러 메시지에 `refresh_token` 을 넣음 | `refresh_token_never_appears_in_error` |
| M4 | `redirect::Policy::none()` 제거 | (생존 예상 — T7 이 테스트를 더한다. 그 사실을 리포트에 적을 것) |

- [ ] **Step 6: 게이트 + 커밋**

```bash
cd <workspace>
cargo fmt --all && cargo clippy --all-targets --all-features --locked -- -D warnings && cargo test --locked && cargo +1.89.0 build --locked
git add Cargo.lock crates/fleetwire/src/auth/refresh.rs crates/fleetwire/src/auth/mod.rs crates/fleetwire/src/lib.rs
git commit -m "feat(a3): HttpRefresher — 크레이트 유일의 POST

fleet-auth 호스트 하드코딩을 유지하고 그 근거(OIDC discovery 가 권위가 아님)를
모듈 rustdoc 에 실측과 함께 박았다. discovery 를 따르면 refresh 가 깨지고
엄격한 issuer 검증을 넣으면 자기 토큰을 거부하게 된다.

refresh timeout 8s < state_deadline 10s. 타임아웃 시 재시도하지 않는다 —
서버가 회전을 마쳤는데 응답만 유실된 경우 재시도는 악화다."
```

---

### Task 5: 토큰 **세대** + **회전 최소 간격** (A2 모듈 수정)

**Files:**
- Modify: `crates/fleetwire/src/auth/mod.rs`
- Test: 같은 파일의 `#[cfg(test)]`

**Interfaces:**
- Consumes: A2 의 `TokenManager`, `Clock`, `EXPIRY_SKEW`.
- Produces:
  ```rust
  /// 토큰 세대. refresh 가 성공할 때마다 1 증가한다.
  pub type Generation = u64;
  impl TokenManager {
      /// 기존 메서드는 그대로 둔다(호환).
      pub async fn access_token(&self) -> Result<String, Error>;
      /// 신규 — 401 복구가 쓰는 형태.
      pub async fn access_token_with_generation(&self) -> Result<(Generation, String), Error>;
      /// 신규 — 자기가 본 세대가 아직 current 일 때만 stale 처리한다.
      pub async fn invalidate_generation(&self, seen: Generation) -> Result<(), Error>;
  }
  pub const MIN_REFRESH_INTERVAL: Duration = Duration::from_secs(60);
  ```

⚠️ **A2 의 227 테스트가 전부 통과해야 한다** — 회귀 게이트다(design §6.3).

⚠️ `MIN_REFRESH_INTERVAL = 60s` 는 `EXPIRY_SKEW` 와 **같은 값**이다. 논리가 한 문장으로 닫힌다: *"신선하다고 볼 수 없는 수명은 회전도 할 수 없다."*

- [ ] **Step 1: 실패하는 테스트 5개를 쓴다**

A2 의 기존 테스트 모듈에 추가한다(`FixedClock` 등 기존 픽스처를 재사용할 것).

```rust
    /// 회전 간격 내 재요청은 **캐시 반환이 아니라 에러**다.
    /// 캐시를 주면 죽은 토큰을 전송하게 된다(design §6.1).
    #[tokio::test]
    async fn refresh_within_min_interval_errors_and_sends_nothing() {
        // expires_in=0 인 토큰을 주는 refresher — 매 호출이 refresh 를 태우려 한다.
        let (mgr, calls) = manager_with_short_lived_tokens(0).await;

        let _ = mgr.access_token().await;           // 1회차: refresh 발생
        let first = calls.load(Ordering::SeqCst);
        let e = mgr.access_token().await.unwrap_err(); // 2회차: 간격에 막힘

        assert_eq!(calls.load(Ordering::SeqCst), first,
                   "간격 내에는 refresh 를 **한 번도 더** 태우지 않아야 한다");
        assert!(e.to_string().contains("refresh"), "원인을 지목해야 한다: {e}");
    }

    /// 그 에러의 pricing 은 None 이다 — 엔드포인트에 닿지 않았으므로 Billed 로 세면 안 된다.
    #[tokio::test]
    async fn min_interval_error_is_not_billed() {
        let (mgr, _) = manager_with_short_lived_tokens(0).await;
        let _ = mgr.access_token().await;
        let e = mgr.access_token().await.unwrap_err();
        assert!(matches!(&e, Error::Auth { pricing: None, .. }), "실제: {e:?}");
        assert!(matches!(e.billing(), crate::pricing::Billing::NotBilled));
    }

    /// 시계가 뒤로 가면 "간격 미충족"으로 본다(보수적 방향).
    #[tokio::test]
    async fn clock_going_backwards_counts_as_interval_not_met() {
        let (mgr, calls) = manager_with_short_lived_tokens(0).await;
        let _ = mgr.access_token().await;
        let first = calls.load(Ordering::SeqCst);
        rewind_clock(&mgr, Duration::from_secs(3600));   // 1시간 역행
        let _ = mgr.access_token().await;
        assert_eq!(calls.load(Ordering::SeqCst), first,
                   "시계 역행이 회전 억제를 무력화하면 안 된다");
    }

    /// 세대는 refresh 성공 때마다 오른다.
    #[tokio::test]
    async fn generation_increases_on_each_refresh() {
        let (mgr, _) = manager_with_normal_tokens().await;
        let (g1, _) = mgr.access_token_with_generation().await.unwrap();
        mgr.invalidate_generation(g1).await.unwrap();
        advance_clock(&mgr, MIN_REFRESH_INTERVAL + Duration::from_secs(1));
        let (g2, _) = mgr.access_token_with_generation().await.unwrap();
        assert!(g2 > g1, "g1={g1} g2={g2}");
    }

    /// ⚠️ 낡은 세대로 무효화를 시도하면 **아무 일도 일어나지 않는다**.
    /// 이게 없으면 동시 401 이 방금 갱신된 토큰을 죽인다(design §5.1).
    #[tokio::test]
    async fn stale_generation_cannot_invalidate_newer_token() {
        let (mgr, calls) = manager_with_normal_tokens().await;
        let (g1, _) = mgr.access_token_with_generation().await.unwrap();
        mgr.invalidate_generation(g1).await.unwrap();
        advance_clock(&mgr, MIN_REFRESH_INTERVAL + Duration::from_secs(1));
        let (g2, tok2) = mgr.access_token_with_generation().await.unwrap();
        let after_g2 = calls.load(Ordering::SeqCst);

        // 뒤늦게 도착한 401 이 **낡은** 세대로 무효화를 시도한다.
        mgr.invalidate_generation(g1).await.unwrap();
        let (g3, tok3) = mgr.access_token_with_generation().await.unwrap();

        assert_eq!(g3, g2, "낡은 세대의 무효화가 먹히면 안 된다");
        assert_eq!(tok3, tok2, "멀쩡한 토큰이 버려지면 안 된다");
        assert_eq!(calls.load(Ordering::SeqCst), after_g2, "refresh 가 더 일어나면 안 된다");
    }
```

⚠️ **헬퍼는 새로 쓰기 전에 A2 것을 먼저 찾아라.** `manager_with_short_lived_tokens`·`manager_with_normal_tokens`·`advance_clock`·`rewind_clock` 이 필요한데, A2 는 이미 `FixedClock`·refresh 호출 카운터·0600 토큰 디렉터리 픽스처를 갖고 있다. 착수 전에 반드시 실행할 것:

```bash
cd <workspace>
grep -n "FixedClock\|fn manager\|AtomicUsize\|fn with_clock\|tempdir\|0o700\|0o600" \
  crates/fleetwire/src/auth/mod.rs | head -40
```

**있는 것은 재사용하고, 없는 것만 같은 스타일로 더한다.** 새로 만들면 A2 가 이미 밟은 함정(umask 때문에 0600 단언이 자기 구현에 실패하는 것 — T4 fix 라운드에서 실측으로 밝혀졌다)을 다시 밟는다.

`advance_clock`/`rewind_clock` 은 `FixedClock` 의 내부 시각을 옮기는 헬퍼다. A2 에 없으면 다음 형태로 더한다:

```rust
    /// FixedClock 을 앞/뒤로 옮긴다. 테스트 전용.
    fn shift_clock(clock: &FixedClock, by: Duration, forward: bool) {
        let cur = clock.now();
        let next = if forward { cur + by } else { cur - by };
        clock.set(next);      // ⚠️ FixedClock 의 실제 setter 이름을 확인할 것
    }
```

- [ ] **Step 2: 실패 확인**

Run: `cd <workspace> && cargo test --locked auth::`
Expected: FAIL — `access_token_with_generation` 없음, 간격 테스트는 refresh 가 계속 일어나 카운터가 증가

- [ ] **Step 3: 구현한다**

`State` 구조체(캐시와 금지 플래그를 같은 뮤텍스가 지키는 그것)에 두 필드를 더한다:

```rust
    /// 토큰 세대. refresh 성공 시 1 증가.
    generation: Generation,
    /// 마지막 refresh **성공** 시각. 프로세스 메모리에만 둔다.
    last_refresh_at: Option<std::time::SystemTime>,
```

```rust
/// 토큰 세대. refresh 가 성공할 때마다 1 증가한다.
pub type Generation = u64;

/// 회전 최소 간격 — `EXPIRY_SKEW` 와 같은 값이다.
///
/// 논리가 한 문장으로 닫힌다: **"신선하다고 볼 수 없는 수명은 회전도 할 수 없다."**
///
/// ⚠️ **프로세스 로컬이다.** 프로세스 간 보장은 이 상수가 아니라 A2 의 flock
/// single-flight 가 한다. 새 프로세스는 이 간격을 모른 채 즉시 refresh 할 수 있고,
/// 그것이 의도된 동작이다 — 이 장치의 목적은 **한 프로세스의 폭주 억제**이지
/// 전역 rate limiter 가 아니다(design §6.4).
pub const MIN_REFRESH_INTERVAL: Duration = Duration::from_secs(60);
```

refresh 를 태우기 직전(락 안)에 게이트를 넣는다:

```rust
        if let Some(last) = state.last_refresh_at {
            let now = self.clock.now();
            // ⚠️ 뺄셈이 아니라 checked_duration_since 다 — 시계가 뒤로 가면
            // underflow 하거나 음수가 된다. None(=역행)은 "간격 미충족"으로
            // 본다(보수적 방향).
            let elapsed = now.duration_since(last).ok();
            if elapsed.is_none_or(|e| e < MIN_REFRESH_INTERVAL) {
                return Err(Error::Auth {
                    message: crate::redaction::body_preview(
                        &format!(
                            "token refresh suppressed: minimum rotation interval is {}s. \
                             The server may be reporting an expires_in inside the {}s \
                             freshness window, or the clock moved backwards.",
                            MIN_REFRESH_INTERVAL.as_secs(),
                            EXPIRY_SKEW.as_secs(),
                        ),
                        512,
                    ),
                    // 엔드포인트에 닿지 않았다 → 과금 아님.
                    pricing: None,
                });
            }
        }
```

refresh 성공 직후:

```rust
        state.generation += 1;
        state.last_refresh_at = Some(self.clock.now());
```

새 메서드 둘:

**리팩터 방향**: 기존 `access_token()` 의 본문을 `access_token_with_generation()` 으로 **옮기고**, `access_token()` 은 그것을 호출해 토큰만 꺼내는 얇은 래퍼로 만든다. 그래야 두 경로가 갈리지 않는다(로직을 복제하면 한쪽만 고쳐지는 사고가 난다).

```rust
    /// 유효한 access token 을 준다.
    ///
    /// 세대가 필요하면 [`Self::access_token_with_generation`] 을 쓴다. 이 메서드는
    /// 그것의 얇은 래퍼다 — 두 경로가 갈리지 않게 하기 위해서다.
    pub async fn access_token(&self) -> Result<String, Error> {
        self.access_token_with_generation().await.map(|(_, t)| t)
    }

    /// 세대와 토큰을 함께 준다. 401 복구는 **이 형태를 써야 한다** — 자기가 보낸
    /// 세대를 알아야 "내가 쓴 토큰이 아직 current 인가"를 판정할 수 있다(design §5.1).
    ///
    /// (본문은 기존 `access_token()` 의 것을 그대로 옮긴 뒤, 마지막에 캐시된
    /// 토큰과 함께 `state.generation` 을 반환하도록만 고친다. 락 획득·신선도
    /// 검사·single-flight·저장 순서는 **A2 가 검증한 그대로 유지한다.**)
    pub async fn access_token_with_generation(&self) -> Result<(Generation, String), Error> {
        // … 기존 access_token() 본문 …
        // 반환 지점만 다음으로 바꾼다:
        //   Ok(token)  →  Ok((state.generation, token))
        // ⚠️ generation 은 **락을 놓기 전에** 읽어야 한다. 놓고 읽으면 그 사이
        //    다른 태스크가 회전시킨 값을 보게 되어 세대-토큰 짝이 어긋난다.
        todo!("A2 본문 이식 — 위 주석대로")
    }

    /// `seen` 세대가 **아직 current 일 때만** 캐시를 stale 로 만든다.
    ///
    /// ⚠️ 이 비교가 없으면 동시 401 이 방금 갱신된 토큰을 죽인다:
    /// A 가 세대 N 으로 보낸 요청의 401 이, B 가 이미 만든 N+1 을 버리게 된다.
    ///
    /// 세대가 이미 올라갔다면 **아무 일도 하지 않고 Ok** 다 — 다른 요청이 이미
    /// 고쳐 놓았으므로 호출자는 새 토큰으로 그냥 재전송하면 된다.
    pub async fn invalidate_generation(&self, seen: Generation) -> Result<(), Error> {
        let mut state = tokio::time::timeout(self.state_deadline, self.state.lock())
            .await
            // ⚠️ 기존 access_token() 이 쓰는 `token state lock:` 에러 생성 코드를
            //    그대로 재사용한다. 문구가 갈리면 진단이 어려워진다.
            .map_err(|_| Error::Auth {
                message: crate::redaction::body_preview(
                    "token state lock: timed out waiting for in-process token state",
                    512,
                ),
                pricing: None,
            })?;
        if state.generation == seen {
            // 캐시를 비우면 다음 access_token() 이 신선도 검사에서 미스가 나
            // refresh 경로로 간다. **여기서 직접 refresh 하지 않는다** —
            // single-flight·회전 간격 게이트를 우회하게 되기 때문이다.
            state.cached = None;
        }
        Ok(())
    }
```

⚠️ `state.cached` 는 A2 의 실제 필드명으로 바꿀 것 — `State` 구조체를 먼저 읽고 캐시를 담은 필드가 무엇인지 확인한다. 이름을 추측하지 말 것.

⚠️ `todo!()` 는 **이 계획의 지시이지 커밋할 코드가 아니다.** Step 4 를 통과하려면 실제 본문이 들어가야 한다.

- [ ] **Step 4: 통과 확인 + A2 회귀**

Run: `cd <workspace> && cargo test --locked`
Expected: 신규 5개 PASS **그리고 A2 의 227개가 전부 그대로 PASS**.

⚠️ A2 테스트가 깨지면 **테스트를 고치지 말고** 무엇이 왜 깨졌는지 리포트에 적을 것. A2 테스트는 검증된 계약이다.

- [ ] **Step 5: mutation-test**

| # | mutation | 죽어야 하는 테스트 |
|---|---|---|
| M1 | 간격 내에 에러 대신 **캐시 토큰 반환** | `..._errors_and_sends_nothing` |
| M2 | `< MIN_REFRESH_INTERVAL` → `<=` (경계) | 경계 테스트 (없으면 **추가**) |
| M3 | `duration_since(...).ok()` → 뺄셈 | `clock_going_backwards_...` |
| M4 | `state.generation == seen` → 무조건 무효화 | `stale_generation_cannot_invalidate_newer_token` |
| M5 | `pricing: None` → `Some(Pricing::data())` | `min_interval_error_is_not_billed` |

- [ ] **Step 6: 게이트 + 커밋**

```bash
cd <workspace>
cargo fmt --all && cargo clippy --all-targets --all-features --locked -- -D warnings && cargo test --locked && cargo +1.89.0 build --locked
git add crates/fleetwire/src/auth/mod.rs
git commit -m "feat(a3): 토큰 세대 + 회전 최소 간격 (expires_in 하한 해소)

세대와 간격은 **다른 문제**를 푼다 — 세대는 정확성(누가 무엇을 무효화해도
되는가), 간격은 폭주 억제다. 60초 간격을 single-flight 정확성의 대체재로
쓰지 않는다.

간격 내 재요청은 캐시 반환이 아니라 원인을 지목하는 에러다. 캐시를 주면
죽은 토큰을 전송하게 되고, 그건 carry-forward 가 금지한 max 하한과 같은
실패다. 그 에러의 pricing 은 None — 엔드포인트에 닿지 않았다.

시계 역행은 checked_duration_since 로 명시 처리하고 '간격 미충족'으로 본다."
```

---

### Task 6: 엔드포인트 3개 + `Client` 뼈대 (부트스트랩 호스트만)

**Files:**
- Create: `crates/fleetwire/src/client.rs`, `crates/fleetwire/src/api/mod.rs`, `crates/fleetwire/src/api/users.rs`, `crates/fleetwire/src/api/vehicles.rs`
- Modify: `crates/fleetwire/src/lib.rs`
- Test: 각 파일의 `#[cfg(test)]`

**Interfaces:**
- Consumes: A1 의 `FleetGet`·`VehicleEndpoint`·`QuerySpec`·`encode_vin_segment`·`decode`·`Pricing`·`ScopeExpr`·`Scope`·`VehicleSummary`. T2 의 `execute_get`. T5 의 `access_token_with_generation`.
- Produces:
  ```rust
  // api/users.rs
  pub struct GetRegion;
  #[derive(serde::Deserialize)] #[non_exhaustive]
  pub struct RegionInfo { pub region: Option<String>, pub fleet_api_base_url: Option<String> }
  // api/vehicles.rs
  pub struct ListVehicles { pub page: Option<u64>, pub per_page: Option<u64> }
  pub struct GetVehicle { pub vin: String }
  // client.rs
  pub struct Client { /* 비공개 */ }
  pub struct ClientBuilder { /* 비공개 */ }
  impl Client {
      pub fn builder(tokens: TokenManager) -> ClientBuilder;
      pub async fn send<R: FleetGet, T: DeserializeOwned>(&self, req: &R) -> Result<T, Error>;
  }
  impl ClientBuilder {
      pub fn bootstrap_region(self, url: reqwest::Url) -> Self;
      pub fn request_timeout(self, d: Duration) -> Self;
      pub fn build(self) -> Result<Client, Error>;
  }
  pub const NA_BOOTSTRAP: &str = "https://fleet-api.prd.na.vn.cloud.tesla.com";
  ```

⚠️ 이 태스크의 `Client` 는 **부트스트랩 호스트만 쓴다.** 서버가 준 `fleet_api_base_url` 을 채택하는 것은 **T7 이 신뢰경계를 만든 뒤**다. 검증 없이 쓰는 커밋을 남기지 않기 위해서다.

⚠️ **`FleetGet` / `VehicleEndpoint` trait 을 새로 만들지 말 것** — `endpoint.rs` 에 이미 있다. 여기서는 **구현**만 한다. `Sealed` 도 crate-private 이므로 각 타입에 `impl crate::sealed::Sealed for X {}` 가 필요하다.

⚠️ 과금 카테고리는 **엔드포인트마다 다르다**(O1 매트릭스). 이 세 개는 **전부 `Pricing::NotCharged`** 다.

- [ ] **Step 1: 실패하는 테스트를 쓴다**

`api/vehicles.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::endpoint::{FleetGet, VehicleEndpoint};

    #[test]
    fn list_vehicles_contract() {
        let r = ListVehicles { page: Some(2), per_page: Some(50) };
        assert_eq!(r.path(), "/api/1/vehicles");
        // ⚠️ O1: 16개 중 유료는 vehicle_data·nearby_charging_sites 뿐이다.
        assert!(matches!(r.pricing(), Pricing::NotCharged));
        let q = r.query().to_encoded();
        assert!(q.contains("page=2") && q.contains("per_page=50"), "실제: {q}");
    }

    #[test]
    fn list_vehicles_omits_absent_pagination() {
        let r = ListVehicles { page: None, per_page: None };
        assert_eq!(r.query().to_encoded(), "", "없는 파라미터를 보내면 안 된다");
    }

    /// VIN 은 path segment 로 인코딩된다 — 경로 주입을 막는다.
    #[test]
    fn get_vehicle_encodes_vin_segment() {
        let r = GetVehicle { vin: "5YJ/../admin".into() };
        assert!(!r.path().contains("/../"), "실제: {}", r.path());
        assert_eq!(r.vin(), "5YJ/../admin");
    }
}
```

`client.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// 봉투가 벗겨지고 타입이 나온다.
    #[tokio::test]
    async fn send_unwraps_envelope() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/1/users/region"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                br#"{"response":{"region":"na","fleet_api_base_url":"https://fleet-api.prd.na.vn.cloud.tesla.com"}}"#.to_vec(),
                "application/json",
            ))
            .mount(&server)
            .await;

        let c = test_client(&server).await;
        let got: RegionInfo = c.send(&GetRegion).await.unwrap();
        assert_eq!(got.region.as_deref(), Some("na"));
    }

    /// charset 이 붙어도 붙지 않아도 파싱된다(실측: 성공은 charset 있음, 412 는 없음).
    #[tokio::test]
    async fn accepts_json_content_type_with_and_without_charset() {
        for ct in ["application/json", "application/json; charset=utf-8"] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(ResponseTemplate::new(200)
                    .set_body_raw(br#"{"response":{"region":"na"}}"#.to_vec(), ct))
                .mount(&server)
                .await;
            let c = test_client(&server).await;
            let got: RegionInfo = c.send(&GetRegion).await.unwrap();
            assert_eq!(got.region.as_deref(), Some("na"), "content-type={ct}");
        }
    }
}
```

- [ ] **Step 2: 실패 확인**

Run: `cd <workspace> && cargo test --locked api:: client::`
Expected: FAIL — 타입 없음

- [ ] **Step 3: 구현한다**

`api/users.rs`:

```rust
//! user 엔드포인트. 전부 무과금이다(O1 매트릭스).

use crate::endpoint::FleetGet;
use crate::pricing::Pricing;
use crate::query::QuerySpec;
use crate::scope::{Scope, ScopeExpr};

/// `GET /api/1/users/region` — 계정의 리전과 base URL.
///
/// ⚠️ 반환된 `fleet_api_base_url` 은 **서버가 준 문자열**이고 그 URL 로 Bearer
/// 토큰이 실려 나간다. 검증 없이 채택하면 자격증명 유출이다 — `client` 의
/// 신뢰경계 검사를 반드시 통과시킬 것(design §4.5).
pub struct GetRegion;

impl crate::sealed::Sealed for GetRegion {}

impl FleetGet for GetRegion {
    fn required_scopes(&self) -> ScopeExpr {
        // 실측 토큰의 scp 는 openid·offline_access·vehicle_device_data·vehicle_location.
        // O1: "region 은 스코프 유연" — 보유 스코프 중 하나면 통과한다.
        ScopeExpr::Any(&[Scope::Openid, Scope::UserData, Scope::VehicleDeviceData])
    }
    fn pricing(&self) -> Pricing { Pricing::NotCharged }
    fn path(&self) -> String { "/api/1/users/region".to_owned() }
    fn query(&self) -> QuerySpec { QuerySpec::new() }
}

/// `/api/1/users/region` 의 응답 본문(봉투 안쪽).
#[derive(serde::Deserialize)]
#[non_exhaustive]
pub struct RegionInfo {
    /// 예: `"na"`.
    #[serde(default)]
    pub region: Option<String>,
    /// 이 계정이 써야 할 Fleet API base URL.
    #[serde(default)]
    pub fleet_api_base_url: Option<String>,
}
```

`api/vehicles.rs` — `ListVehicles`(page/per_page, `Pricing::NotCharged`), `GetVehicle`(VIN, `VehicleEndpoint` 구현, `encode_vin_segment` 사용). 둘 다 `Sealed` 구현.

`client.rs` — 빌더, `send()`:

```rust
    pub async fn send<R: FleetGet, T: serde::de::DeserializeOwned>(
        &self,
        req: &R,
    ) -> Result<T, Error> {
        let (_gen, token) = self.tokens.access_token_with_generation().await?;
        let base = self.base_url();                 // T6: 부트스트랩만. T7 이 캐시를 연다.
        let url = build_url(&base, &req.path(), &req.query())?;
        let raw = crate::transport::execute_get(
            &self.http, url, &token, req.pricing(), self.request_timeout,
        ).await?;
        crate::decode::decode(
            raw.status,
            raw.content_type.as_deref(),
            &raw.body,
            req.pricing(),
            raw.retry_after,
            raw.www_authenticate.as_deref(),
        )
    }
```

⚠️ `decode` 의 content-type 검사는 `.contains("json")` 이라 **이미 mime 파라미터에 관대하다**(A1). exact match 로 바꾸지 말 것.

**테스트 픽스처를 `client.rs` 의 `#[cfg(test)]` 안에 함께 만든다.** T7·T8·T9 가 전부 이 헬퍼들을 쓰므로 여기서 한 번에 정의한다.

```rust
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// scp 클레임을 가진 가짜 JWT 를 만든다(서명은 아무 값 — 검증하지 않으므로).
    fn fake_jwt(scopes: &[&str]) -> String {
        use base64::Engine as _;
        let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let payload = serde_json::json!({ "scp": scopes, "exp": 9_999_999_999u64 });
        format!(
            "{}.{}.sig",
            b64.encode(br#"{"alg":"RS256","typ":"JWT"}"#),
            b64.encode(serde_json::to_vec(&payload).unwrap()),
        )
    }

    /// 항상 같은 토큰을 돌려주는 refresher + 호출 카운터.
    struct CountingRefresher {
        token: String,
        calls: Arc<AtomicUsize>,
    }

    impl TokenRefresher for CountingRefresher {
        fn refresh<'a>(
            &'a self,
            _refresh_token: &'a str,
        ) -> std::pin::Pin<Box<dyn Future<Output = Result<Refreshed, Error>> + Send + 'a>> {
            let token = self.token.clone();
            let calls = Arc::clone(&self.calls);
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(Refreshed::new(token, "refresh-token".into(), 28_800))
            })
        }
    }

    /// mock 서버를 부트스트랩 리전으로 삼고, 그 host 를 allowlist 에 넣은 빌더.
    /// ⚠️ allowlist 를 여는 것은 **테스트 한정**이다 — 프로덕션 기본값은 닫혀 있다.
    async fn test_client_builder(server: &MockServer) -> ClientBuilder {
        let (b, _) = test_client_builder_with(server, Some(&["vehicle_device_data",
                                                            "vehicle_location",
                                                            "openid"])).await;
        b
    }

    async fn test_client_builder_with(
        server: &MockServer,
        scopes: Option<&[&str]>,
    ) -> (ClientBuilder, Arc<AtomicUsize>) {
        let token = match scopes {
            Some(s) => fake_jwt(s),
            None => "opaque-not-a-jwt".to_owned(),
        };
        let calls = Arc::new(AtomicUsize::new(0));
        let dir = tempdir_for_tokens();          // A2 의 기존 테스트 픽스처를 재사용한다
        let tokens = TokenManager::new(
            dir,
            Arc::new(CountingRefresher { token, calls: Arc::clone(&calls) }),
        );
        let url: reqwest::Url = server.uri().parse().unwrap();
        let b = Client::builder(tokens)
            .bootstrap_region(url)
            .request_timeout(std::time::Duration::from_secs(5));
        // ⚠️ T7 이 이 함수에 `.allow_region_host(&host)` 를 **추가**한다.
        //    T6 시점에는 그 메서드가 아직 없으므로 여기서 호출하면 컴파일이 깨진다.
        (b, calls)
    }

    async fn test_client(server: &MockServer) -> Client {
        test_client_builder(server).await.build().unwrap()
    }

    async fn test_client_counting_refreshes(server: &MockServer) -> (Client, Arc<AtomicUsize>) {
        let (b, calls) = test_client_builder_with(
            server, Some(&["vehicle_device_data", "vehicle_location", "openid"])).await;
        (b.build().unwrap(), calls)
    }

    async fn test_client_with_scopes(server: &MockServer, scopes: &[&str]) -> Client {
        let (b, _) = test_client_builder_with(server, Some(scopes)).await;
        b.build().unwrap()
    }

    async fn test_client_with_opaque_token(server: &MockServer) -> Client {
        let (b, _) = test_client_builder_with(server, None).await;
        b.build().unwrap()
    }

    /// 다른 요청이 이미 세대를 올려 둔 상태를 만든다(§5.1 동시 401 시나리오).
    async fn bump_generation_out_of_band(c: &Client) {
        let (g, _) = c.tokens().access_token_with_generation().await.unwrap();
        c.tokens().invalidate_generation(g).await.unwrap();
        let _ = c.tokens().access_token_with_generation().await.unwrap();
    }
```

⚠️ `Client` 에 `pub(crate) fn tokens(&self) -> &TokenManager` 를 더한다(테스트 전용 접근자, `pub` 아님).
⚠️ `tempdir_for_tokens()` 는 **A2 의 기존 테스트 픽스처를 먼저 찾아 재사용한다.** A2 는 0600 fixture 헬퍼를 이미 갖고 있다(T4 fix 라운드에서 umask 문제로 만든 것) — 새로 만들면 같은 함정을 다시 밟는다.

- [ ] **Step 4: 통과 확인**

Run: `cd <workspace> && cargo test --locked`
Expected: 전부 PASS

- [ ] **Step 5: mutation-test**

| # | mutation | 죽어야 하는 테스트 |
|---|---|---|
| M1 | `ListVehicles::pricing()` → `Pricing::data()` | `list_vehicles_contract` |
| M2 | `page: None` 인데도 `page=` 를 붙임 | `..._omits_absent_pagination` |
| M3 | `encode_vin_segment` 대신 raw VIN 삽입 | `get_vehicle_encodes_vin_segment` |
| M4 | `decode` 의 content-type 검사를 exact match 로 | `accepts_json_content_type_with_and_without_charset` |

- [ ] **Step 6: 게이트 + 커밋**

```bash
cd <workspace>
cargo fmt --all && cargo clippy --all-targets --all-features --locked -- -D warnings && cargo test --locked && cargo +1.89.0 build --locked
git add Cargo.lock crates/fleetwire/src/client.rs crates/fleetwire/src/api crates/fleetwire/src/lib.rs
git commit -m "feat(a3): Client + 엔드포인트 3개 (region·vehicles 목록·차량 상태)

A1 의 FleetGet/VehicleEndpoint trait 을 **구현**만 한다 — trait 은 이미 있다.
셋 다 Pricing::NotCharged 다(O1: 16개 중 유료는 vehicle_data 와
nearby_charging_sites 뿐).

이 커밋의 Client 는 부트스트랩 호스트만 쓴다. 서버가 준 fleet_api_base_url
채택은 T7 이 신뢰경계를 만든 뒤에 연다 — 검증 없이 쓰는 커밋을 남기지 않는다."
```

---

### Task 7: 🔴 리전 신뢰경계 + redirect 차단 + 412 복구 + 401 복구

**Files:**
- Modify: `crates/fleetwire/src/client.rs`
- Test: 같은 파일의 `#[cfg(test)]`

**Interfaces:**
- Consumes: T5 의 `access_token_with_generation`·`invalidate_generation`, T6 의 `GetRegion`·`RegionInfo`.
- Produces:
  ```rust
  pub const TESLA_REGION_HOSTS: &[&str] = &[
      "fleet-api.prd.na.vn.cloud.tesla.com",
      "fleet-api.prd.eu.vn.cloud.tesla.com",
  ];
  impl ClientBuilder { pub fn allow_region_host(self, host: &str) -> Self; }
  // 내부
  fn validate_region_url(raw: &str, allowed: &[String]) -> Result<reqwest::Url, Error>;
  impl Client {
      /// 현재 리전 캐시 세대. 테스트 전용 접근자(`pub` 아님).
      pub(crate) async fn region_generation(&self) -> u64;
      /// 리전을 해석해 캐시에 넣는다(검증 §4.5 통과 필수). 이미 있으면 그대로 둔다.
      pub(crate) async fn resolve_region(&self) -> Result<reqwest::Url, Error>;
      /// `seen` 세대일 때만 캐시를 지운다(compare-and-clear).
      pub(crate) async fn invalidate_region(&self, seen: u64);
  }
  ```

**이 태스크가 이 계획에서 가장 중요한 보안 항목이다.** `fleet_api_base_url` 은 서버가 준 문자열이고 그 URL 로 Bearer 토큰이 실려 나간다.

⚠️ 중국 리전 호스트는 **증거가 없어 allowlist 에서 뺀다**(design O11). 필요한 사용자는 `allow_region_host()` 로 넣는다.

- [ ] **Step 1: 실패하는 테스트 8개를 쓴다**

```rust
    /// 🔴 악성 리전 URL 6종을 전부 거부한다. 하나라도 통과하면 자격증명 유출이다.
    #[test]
    fn rejects_every_untrusted_region_url() {
        let allowed: Vec<String> =
            TESLA_REGION_HOSTS.iter().map(|s| s.to_string()).collect();
        for bad in [
            "http://fleet-api.prd.na.vn.cloud.tesla.com",          // 평문
            "https://evil.example",                                 // 외부 호스트
            "https://fleet-api.prd.na.vn.cloud.tesla.com.evil.net", // 접미 매칭 우회
            "https://user:pw@fleet-api.prd.na.vn.cloud.tesla.com",  // userinfo
            "https://fleet-api.prd.na.vn.cloud.tesla.com:8443",     // 비표준 포트
            "https://fleet-api.prd.na.vn.cloud.tesla.com/evil/path",// path 주입
            "https://fleet-api.prd.na.vn.cloud.tesla.com/?x=1",     // query
        ] {
            assert!(validate_region_url(bad, &allowed).is_err(), "통과하면 안 됨: {bad}");
        }
    }

    #[test]
    fn accepts_legitimate_region_urls() {
        let allowed: Vec<String> =
            TESLA_REGION_HOSTS.iter().map(|s| s.to_string()).collect();
        for ok in [
            "https://fleet-api.prd.na.vn.cloud.tesla.com",
            "https://fleet-api.prd.na.vn.cloud.tesla.com/",
            "https://fleet-api.prd.eu.vn.cloud.tesla.com",
        ] {
            assert!(validate_region_url(ok, &allowed).is_ok(), "거부하면 안 됨: {ok}");
        }
    }

    /// 🔴 3xx 를 따라가지 않는다 — Authorization 이 다른 호스트로 가면 안 된다.
    #[tokio::test]
    async fn never_follows_redirect_with_authorization() {
        let evil = MockServer::start().await;
        Mock::given(method("GET")).respond_with(ResponseTemplate::new(200)
            .set_body_raw(br#"{"response":{}}"#.to_vec(), "application/json"))
            .mount(&evil).await;

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(302)
                .insert_header("location", format!("{}/steal", evil.uri())))
            .mount(&server).await;

        let c = test_client(&server).await;
        let r: Result<RegionInfo, _> = c.send(&GetRegion).await;
        assert!(r.is_err(), "리다이렉트를 따라가면 안 된다");
        assert!(evil.received_requests().await.unwrap().is_empty(),
                "🔴 토큰이 외부 호스트로 나갔다");
    }

    /// 412 → 리전 재해석 1회 → 재전송. **기전을 센다**(요청 수), "에러가 났다"가 아니라.
    #[tokio::test]
    async fn region_412_triggers_one_reresolve_and_retry() {
        let server = MockServer::start().await;
        // /vehicles: 1번째 412, 2번째 200.
        Mock::given(method("GET"))
            .and(path("/api/1/vehicles"))
            .respond_with(ResponseTemplate::new(412).set_body_raw(
                br#"{"error":"must be registered in the current region"}"#.to_vec(),
                "application/json",
            ))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/1/vehicles"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                br#"{"response":[]}"#.to_vec(),
                "application/json",
            ))
            .mount(&server)
            .await;
        // 리전 재해석은 mock 서버 자신을 가리키게 돌려준다(allowlist 우회를 위해
        // 테스트 빌더가 server 의 host 를 allow_region_host 로 넣어 둔다).
        Mock::given(method("GET"))
            .and(path("/api/1/users/region"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(r#"{{"response":{{"region":"na","fleet_api_base_url":"{}"}}}}"#,
                        server.uri()).into_bytes(),
                "application/json",
            ))
            .mount(&server)
            .await;

        let c = test_client(&server).await;
        let got: Vec<crate::model::VehicleSummary> =
            c.send(&ListVehicles { page: None, per_page: None }).await.unwrap();
        assert!(got.is_empty());

        let reqs = server.received_requests().await.unwrap();
        let vehicles = reqs.iter().filter(|r| r.url.path() == "/api/1/vehicles").count();
        let region = reqs.iter().filter(|r| r.url.path() == "/api/1/users/region").count();
        assert_eq!(vehicles, 2, "원요청 + 412 재시도 = 2회여야 한다");
        assert_eq!(region, 1, "리전 재해석은 정확히 1회여야 한다");
    }

    /// 412 가 계속 나도 무한루프가 아니다 — 요청 수 상한이 있다.
    #[tokio::test]
    async fn repeated_412_terminates() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/1/vehicles"))
            .respond_with(ResponseTemplate::new(412).set_body_raw(
                br#"{"error":"must be registered in the current region"}"#.to_vec(),
                "application/json",
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/1/users/region"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(r#"{{"response":{{"fleet_api_base_url":"{}"}}}}"#, server.uri())
                    .into_bytes(),
                "application/json",
            ))
            .mount(&server)
            .await;

        let c = test_client(&server).await;
        let e = c
            .send::<_, Vec<crate::model::VehicleSummary>>(&ListVehicles { page: None, per_page: None })
            .await
            .unwrap_err();
        assert!(matches!(&e, Error::Api { status, .. } if status.as_u16() == 412), "실제: {e:?}");

        let reqs = server.received_requests().await.unwrap();
        let vehicles = reqs.iter().filter(|r| r.url.path() == "/api/1/vehicles").count();
        assert_eq!(vehicles, 2, "412 재시도는 1회한 — 무한루프면 이 값이 폭발한다");
    }

    /// 401 → refresh 1회 → 재전송 1회. 2번째 401 은 종료.
    #[tokio::test]
    async fn auth_401_refreshes_once_then_gives_up() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/1/users/region"))
            .respond_with(ResponseTemplate::new(401).set_body_raw(
                br#"{"error":"invalid authentication"}"#.to_vec(),
                "application/json",
            ))
            .mount(&server)
            .await;

        // refresh 호출 횟수를 세는 refresher 를 심는다.
        let (c, refresh_calls) = test_client_counting_refreshes(&server).await;
        let e = c.send::<_, RegionInfo>(&GetRegion).await.unwrap_err();

        assert!(matches!(e, Error::Auth { .. }), "실제: {e:?}");
        assert_eq!(refresh_calls.load(Ordering::SeqCst), 1, "refresh 는 정확히 1회");
        let reqs = server.received_requests().await.unwrap();
        assert_eq!(reqs.len(), 2, "원요청 + 401 재시도 = 2회");
    }

    /// ⚠️ 세대가 다르면 refresh 없이 즉시 재전송한다(design §5.1).
    /// 이게 없으면 동시 401 이 방금 갱신된 토큰을 죽인다.
    #[tokio::test]
    async fn concurrent_401_with_stale_generation_does_not_refresh() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/1/users/region"))
            .respond_with(ResponseTemplate::new(401).set_body_raw(
                br#"{"error":"invalid authentication"}"#.to_vec(),
                "application/json",
            ))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/1/users/region"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                br#"{"response":{"region":"na"}}"#.to_vec(),
                "application/json",
            ))
            .mount(&server)
            .await;

        let (c, refresh_calls) = test_client_counting_refreshes(&server).await;
        // 다른 요청이 이미 세대를 올려 둔 상태를 만든다.
        bump_generation_out_of_band(&c).await;
        let before = refresh_calls.load(Ordering::SeqCst);

        let got: RegionInfo = c.send(&GetRegion).await.unwrap();
        assert_eq!(got.region.as_deref(), Some("na"));
        assert_eq!(
            refresh_calls.load(Ordering::SeqCst), before,
            "세대가 이미 올라갔으면 refresh 를 **한 번도 더** 태우면 안 된다"
        );
    }

    /// 401 재시도와 412 재시도가 서로를 재설정하지 않는다 — Fleet 요청 총 3회 상한.
    #[tokio::test]
    async fn auth_and_region_retries_do_not_reset_each_other() {
        let server = MockServer::start().await;
        // /vehicles 가 401 → 412 → 412 … 순으로 답해 두 복구가 연쇄되게 한다.
        Mock::given(method("GET"))
            .and(path("/api/1/vehicles"))
            .respond_with(ResponseTemplate::new(401)
                .set_body_raw(br#"{"error":"invalid authentication"}"#.to_vec(), "application/json"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/1/vehicles"))
            .respond_with(ResponseTemplate::new(412)
                .set_body_raw(br#"{"error":"must be registered"}"#.to_vec(), "application/json"))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/1/users/region"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(r#"{{"response":{{"fleet_api_base_url":"{}"}}}}"#, server.uri())
                    .into_bytes(),
                "application/json",
            ))
            .mount(&server)
            .await;

        let c = test_client(&server).await;
        let _ = c
            .send::<_, Vec<crate::model::VehicleSummary>>(&ListVehicles { page: None, per_page: None })
            .await;

        let reqs = server.received_requests().await.unwrap();
        let vehicles = reqs.iter().filter(|r| r.url.path() == "/api/1/vehicles").count();
        assert_eq!(
            vehicles, 3,
            "원요청 + 401 재시도 + 412 재시도 = 3회 상한. \
             플래그가 서로를 재설정하면 이 값이 커진다"
        );
    }

    /// ⚠️ 리전 캐시 무효화는 **자기가 본 generation 일 때만** 성립한다.
    /// 낡은 generation 으로 무효화하면 아무 일도 일어나지 않아야 한다 —
    /// 그렇지 않으면 요청 A 의 412 가 B 가 이미 갱신해 둔 리전을 지운다.
    #[tokio::test]
    async fn stale_generation_cannot_clear_newer_region_cache() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/1/users/region"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                format!(r#"{{"response":{{"fleet_api_base_url":"{}"}}}}"#, server.uri())
                    .into_bytes(),
                "application/json",
            ))
            .mount(&server)
            .await;

        let c = test_client(&server).await;
        let g1 = c.region_generation().await;      // 아직 미해석
        c.resolve_region().await.unwrap();          // 해석 1회 → generation 상승
        let g2 = c.region_generation().await;
        assert!(g2 > g1);

        // 뒤늦게 도착한 412 가 **낡은** generation 으로 무효화를 시도한다.
        c.invalidate_region(g1).await;
        assert_eq!(c.region_generation().await, g2, "낡은 세대의 무효화가 먹히면 안 된다");

        let region_calls = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == "/api/1/users/region")
            .count();
        assert_eq!(region_calls, 1, "무효화가 잘못 먹으면 재해석이 한 번 더 일어난다");
    }

    /// ⚠️ 412 바디의 계정 UUID 가 에러 메시지에 남지 않는다. x-request-id 는 남는다.
    #[tokio::test]
    async fn account_uuid_is_redacted_but_request_id_survives() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(412)
                .insert_header("x-request-id", "0aea2788-ca44-4391-95a3-774e13da5b5d")
                .set_body_raw(
                    br#"{"error":"Account 00000000-1111-4222-8333-444444444444 must be registered in the current region"}"#.to_vec(),
                    "application/json"))
            .mount(&server).await;

        let c = test_client(&server).await;
        let e = c.send::<_, RegionInfo>(&GetRegion).await.unwrap_err();
        let rendered = format!("{e}{e:?}");
        assert!(!rendered.contains("00000000-1111-4222-8333-444444444444"),
                "🔴 계정 UUID 가 노출됐다: {rendered}");
    }
```

- [ ] **Step 2: 실패 확인**

Run: `cd <workspace> && cargo test --locked client::`
Expected: FAIL — `validate_region_url` 없음, redirect 테스트에서 evil 서버가 요청을 받음

- [ ] **Step 3: 구현한다**

```rust
/// 검증된 Tesla 리전 호스트. **정확한 호스트 집합**이다 —
/// `*.tesla.com` 같은 접미 매칭은 `tesla.com.attacker.net` 에 취약하다.
///
/// ⚠️ 중국 리전(`…tesla.cn`)은 증거가 없어 뺐다. 필요하면
/// [`ClientBuilder::allow_region_host`] 로 넣는다.
pub const TESLA_REGION_HOSTS: &[&str] = &[
    "fleet-api.prd.na.vn.cloud.tesla.com",
    "fleet-api.prd.eu.vn.cloud.tesla.com",
];

/// 🔴 서버가 준 리전 URL 을 채택해도 되는지 판정한다.
///
/// 이 함수가 이 크레이트의 **자격증명 신뢰경계**다. 통과한 URL 로 Bearer 토큰이
/// 실려 나가므로, 실패 모드는 "잘못된 요청"이 아니라 **자격증명 유출**이다.
/// 하나라도 실패하면 거부하고 부트스트랩 호스트를 유지한다(design §4.5).
fn validate_region_url(raw: &str, allowed: &[String]) -> Result<reqwest::Url, Error> {
    let reject = |why: &str| Error::Auth {
        message: crate::redaction::body_preview(
            &format!("refusing server-provided region URL: {why}"), 512),
        pricing: None,
    };
    let url = reqwest::Url::parse(raw).map_err(|_| reject("unparseable"))?;
    if url.scheme() != "https" { return Err(reject("scheme is not https")); }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(reject("URL carries userinfo"));
    }
    let host = url.host_str().ok_or_else(|| reject("no host"))?;
    if !allowed.iter().any(|a| a == host) {
        return Err(reject("host is not an allowed Tesla region host"));
    }
    if !matches!(url.port(), None | Some(443)) { return Err(reject("non-default port")); }
    if !matches!(url.path(), "" | "/") { return Err(reject("URL carries a path")); }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(reject("URL carries a query or fragment"));
    }
    Ok(url)
}
```

**T6 의 테스트 픽스처를 갱신한다.** `test_client_builder_with` 에 allowlist 등록을 더한다 — 이 줄은 T7 에서 처음 컴파일된다(T6 시점에는 메서드가 없었다):

```rust
        let url: reqwest::Url = server.uri().parse().unwrap();
        let host = url.host_str().unwrap().to_owned();
        let b = Client::builder(tokens)
            .bootstrap_region(url)
            .allow_region_host(&host)   // ← T7 에서 추가되는 줄
            .request_timeout(std::time::Duration::from_secs(5));
```

⚠️ mock 서버 host 를 allowlist 에 넣는 것은 **테스트 한정**이다. `allow_region_host` 의 기본값은 닫혀 있고(`TESLA_REGION_HOSTS` 만), 그 기본값을 "편하니까" 뒤집지 말 것 — 실패 모드가 자격증명 유출이다.

reqwest 클라이언트 빌드 시 **redirect 차단**:

```rust
            .redirect(reqwest::redirect::Policy::none())
```

리전 캐시는 **generation 을 가진 `RwLock`** 이다(⚠️ `OnceCell` 은 무효화가 불가능하다):

```rust
/// 리전 캐시. `OnceCell` 이 아닌 이유는 **무효화가 필요하기 때문**이다 —
/// 412 를 받으면 캐시를 버리고 다시 물어야 한다. `OnceCell` 은 그럴 수 없다.
struct RegionCache {
    inner: tokio::sync::RwLock<Option<(u64, reqwest::Url)>>,
}
```

무효화는 **compare-and-clear** 다 — 자기가 본 generation 일 때만 지운다. 그래야 요청 A 의 412 가, 그 사이 B 가 갱신해 둔 리전을 지우지 않는다(§5.1 과 같은 패턴).

`send()` 에 두 복구 경로를 넣되 **플래그는 독립**이고 서로를 재설정하지 않는다.

**계정 UUID redaction 을 `redaction.rs` 에 추가한다**(design §8 — 이 단계가 없으면 위 `account_uuid_is_redacted_...` 테스트가 통과할 수 없다):

```rust
/// 응답 본문에 실려 오는 **계정 UUID** 를 가린다.
///
/// 근거(실측 2026-08-01): 리전이 틀린 요청의 412 응답이
/// `Account 801802ed-…-f69fc59b2ef0 must be registered…` 처럼 계정 식별자를
/// 평문으로 흘린다. OSS 사용자는 에러 메시지를 GitHub 이슈에 그대로 붙여넣는다.
///
/// ⚠️ **UUID 를 전역 치환하지 않는다.** 그러면 `x-request-id` 같은 진단용
/// 식별자까지 지워져 장애 대응이 어려워진다. `Account <uuid>` 문맥에 한정한다 —
/// 무엇을 남기고 무엇을 지웠는지가 이 함수의 계약이다.
fn redact_account_id(s: &str) -> String {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let re = RE.get_or_init(|| {
        regex::Regex::new(
            r"(?i)\bAccount\s+[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\b",
        )
        .expect("static pattern compiles")
    });
    re.replace_all(s, concat!("Account ", "[REDACTED]")).into_owned()
}
```

⚠️ 이 함수를 **`redact_text` 의 파이프라인 안에서 호출**한다. 별도 진입점을 만들면 `body_preview` 만이 안전한 진입점이라는 A1 의 타입 계약이 깨진다(design §8 — A2 에서 이 계약이 한 번 깨졌다).

`redaction.rs` 에 단위 테스트도 함께 넣는다:

```rust
    #[test]
    fn redacts_account_uuid_but_keeps_bare_uuid() {
        let s = "Account 00000000-1111-4222-8333-444444444444 must be registered; \
                 request 0aea2788-ca44-4391-95a3-774e13da5b5d";
        let out = redact_account_id(s);
        assert!(!out.contains("00000000-1111-4222-8333-444444444444"), "계정 ID 가 남았다: {out}");
        assert!(out.contains("0aea2788-ca44-4391-95a3-774e13da5b5d"),
                "진단용 request id 까지 지우면 안 된다: {out}");
    }
```

- [ ] **Step 4: 통과 확인**

Run: `cd <workspace> && cargo test --locked`
Expected: 전부 PASS

- [ ] **Step 5: mutation-test (이 태스크는 특히 철저히)**

| # | mutation | 죽어야 하는 테스트 |
|---|---|---|
| M1 | `url.scheme() != "https"` 검사 제거 | `rejects_every_untrusted_region_url` |
| M2 | allowlist 를 `host.ends_with("tesla.com")` 로 | 같은 테스트(접미 우회 케이스) |
| M3 | userinfo 검사 제거 | 같은 테스트 |
| M4 | path 검사 제거 | 같은 테스트 |
| M5 | `Policy::none()` 제거 | `never_follows_redirect_with_authorization` |
| M6 | 리전 무효화를 compare-and-clear 대신 무조건 | `stale_generation_cannot_clear_newer_region_cache` |
| M7 | 401·412 플래그를 하나로 합침 | `..._do_not_reset_each_other` |

⚠️ **M1~M4 중 하나라도 생존하면 그 테스트는 자격증명 유출을 막지 못한다.** 생존자는 반드시 리포트에 적을 것.

- [ ] **Step 6: 게이트 + 커밋**

```bash
cd <workspace>
cargo fmt --all && cargo clippy --all-targets --all-features --locked -- -D warnings && cargo test --locked && cargo +1.89.0 build --locked
git add crates/fleetwire/src/client.rs
git commit -m "feat(a3): 리전 신뢰경계 + redirect 차단 + 401/412 복구

🔴 서버가 준 fleet_api_base_url 은 검증 없이 쓰면 Bearer 토큰을 임의 호스트로
보내는 경로다. https·정확한 호스트 allowlist·userinfo 없음·기본 포트·path/query
없음을 전부 통과해야 채택한다. 접미 매칭을 쓰지 않는 이유는 tesla.com.attacker.net
때문이다.

redirect 는 Policy::none() 으로 차단한다 — 인증 요청에서 리다이렉트는
Authorization 이 다른 호스트로 따라갈 수 있는 경로다.

리전 캐시는 OnceCell 이 아니라 generation RwLock 이다. 412 무효화가 필요한데
OnceCell 은 무효화가 불가능하다. 무효화는 compare-and-clear —
자기가 본 세대일 때만 지운다.

401 과 412 재시도 플래그는 독립이고 서로를 재설정하지 않는다(Fleet 요청 3회 상한)."
```

---

### Task 8: 재시도 opt-in (기본은 여전히 끔)

**Files:**
- Modify: `crates/fleetwire/src/client.rs`
- Test: 같은 파일

**Interfaces:**
- Produces: `impl ClientBuilder { pub fn max_attempts(self, n: u32) -> Self; pub fn retry_deadline(self, d: Duration) -> Self; }`

⚠️ 기본 `max_attempts = 1`(재시도 없음)은 상위 스펙 §11 의 non-goal 이다. **기본값을 바꾸지 말 것.**

- [ ] **Step 1: 실패하는 테스트 4개를 쓴다**

```rust
    /// 기본값에서는 5xx 여도 재시도하지 않는다(상위 스펙 §11 non-goal).
    #[tokio::test]
    async fn default_does_not_retry_even_on_retryable_5xx() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503)
                .set_body_raw(b"{}".to_vec(), "application/json"))
            .mount(&server)
            .await;

        let c = test_client(&server).await;   // 기본 빌더 = max_attempts 1
        let e = c.send::<_, RegionInfo>(&GetRegion).await.unwrap_err();
        assert!(e.is_retryable(), "503 은 재시도 **가능**하다고 판정돼야 한다");

        let reqs = server.received_requests().await.unwrap();
        assert_eq!(reqs.len(), 1, "판정이 가능이어도 기본값에서는 보내지 않는다");
    }

    /// max_attempts(3) 을 켜면 500·502·503·504 만 재시도한다.
    #[tokio::test]
    async fn opt_in_retries_only_transient_5xx() {
        for (status, expected_sends) in [(503_u16, 3_usize), (501, 1)] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(ResponseTemplate::new(status)
                    .set_body_raw(b"{}".to_vec(), "application/json"))
                .mount(&server)
                .await;

            let c = test_client_builder(&server)
                .await
                .max_attempts(3)
                .retry_deadline(std::time::Duration::from_secs(5))
                .build()
                .unwrap();
            let _ = c.send::<_, RegionInfo>(&GetRegion).await;

            let reqs = server.received_requests().await.unwrap();
            assert_eq!(reqs.len(), expected_sends,
                       "status={status} 는 {expected_sends}회 전송돼야 한다");
        }
    }

    /// ⚠️ TransportUnknown 은 켜도 재시도하지 않는다 — 재시도가 곧 중복 과금이다.
    #[tokio::test]
    async fn transport_unknown_is_never_retried_even_when_enabled() {
        let server = MockServer::start().await;
        // 응답을 지연시켜 요청 timeout 을 유발한다 → TransportUnknown.
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_secs(30))
                .set_body_raw(b"{}".to_vec(), "application/json"))
            .mount(&server)
            .await;

        let c = test_client_builder(&server)
            .await
            .max_attempts(3)
            .request_timeout(std::time::Duration::from_millis(200))
            .build()
            .unwrap();
        let e = c.send::<_, RegionInfo>(&GetRegion).await.unwrap_err();

        assert!(matches!(e, Error::TransportUnknown { .. }), "실제: {e:?}");
        assert!(!e.is_retryable(), "Unknown 은 재시도 금지 판정이어야 한다");
        let reqs = server.received_requests().await.unwrap();
        assert_eq!(reqs.len(), 1, "재시도를 켜도 Unknown 은 다시 보내지 않는다");
    }

    /// 백오프 경로가 기본값에서 실행되지 않는다 — "죽은 코드"가 아니라
    /// "조건부 코드"임을 고정한다(design §9.0).
    #[tokio::test]
    async fn backoff_path_is_not_entered_by_default() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(503)
                .set_body_raw(b"{}".to_vec(), "application/json"))
            .mount(&server)
            .await;

        // 기본값에서는 백오프 대기가 없으므로 즉시 끝난다.
        let c = test_client(&server).await;
        let t0 = std::time::Instant::now();
        let _ = c.send::<_, RegionInfo>(&GetRegion).await;
        assert!(t0.elapsed() < std::time::Duration::from_millis(500),
                "기본값에서 백오프를 타면 안 된다: {:?}", t0.elapsed());
    }
```

- [ ] **Step 2~4:** 실패 확인 → 구현(`Error::is_retryable()` 을 그대로 쓴다. **재구현하지 말 것**) → 통과 확인

`is_retryable()` 의 rustdoc 에 다음을 덧붙인다:

```
/// ⚠️ 이 판정은 필요 이상으로 엄격하다 — 무과금 엔드포인트 14개에서는
/// `TransportUnknown` 을 재시도해도 중복 과금이 없다(rate limit 은 여전히 적용).
/// 그럼에도 status 만으로 판정하는 이유는 셋이다: 재시도 기본값이 off 라 실효
/// 차이가 없고, 과금 카테고리를 안전 규칙에 끌어들이면 요금표 변경에 연동돼
/// 깨지기 쉬워지며, 이 함수는 공개 API 라 의미를 나중에 좁히는 것이 breaking 이다.
```

- [ ] **Step 5: mutation-test** — M1 기본값을 3으로 / M2 `TransportUnknown` 을 retryable 로 / M3 5xx 목록에 501 추가

- [ ] **Step 6: 게이트 + 커밋**

```bash
git commit -m "feat(a3): 재시도 opt-in 명문화 (기본은 여전히 끔)

초안은 max_attempts=1 과 백오프 표·is_retryable=true 를 나란히 적어
'기본값에서 죽은 코드'라는 지적을 양쪽 모델에서 받았다. 기본값은 그대로 두되
백오프·체인 deadline 은 max_attempts>1 일 때만 도는 것을 테스트로 고정한다.

is_retryable 이 과금 카테고리를 보지 않는 이유를 rustdoc 에 남겼다 —
후속 기여자가 '무과금인데 왜 재시도 안 하지'로 되돌리지 않도록."
```

---

### Task 9: REQ-8 스코프 advisory 배선 + 문서 마감

**Files:**
- Modify: `crates/fleetwire/src/client.rs`, `crates/fleetwire/src/auth/mod.rs`(`observe_scopes` 의 `expect(dead_code)` 제거), `crates/fleetwire/src/scope.rs`(doc caveat), `crates/fleetwire/src/lib.rs`(크레이트 문서), `README.md`
- Test: `client.rs`

**Interfaces:**
- Consumes: A2 의 `observe_scopes`, A1 의 `ScopeExpr::missing`.

⚠️ **`Indeterminate` 와 `ContainsRequiredScope` 를 코드에서 동일하게 다룰 것.** 호출부에서 갈라지면 fail-open 이 거기서 깨진다(design §10).

- [ ] **Step 1: 실패하는 테스트 4개를 쓴다**

```rust
    /// 스코프가 확실히 없으면 **네트워크 이전에** 막는다(💰 0).
    /// ⚠️ "에러가 났다"로는 부족하다 — 서버 수신 요청이 0건임을 단언한다.
    #[tokio::test]
    async fn missing_scope_blocks_before_any_request() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_raw(br#"{"response":[]}"#.to_vec(), "application/json"))
            .mount(&server)
            .await;

        // scp 에 vehicle_device_data 가 **없는** JWT 를 심는다.
        let c = test_client_with_scopes(&server, &["openid", "offline_access"]).await;
        let e = c
            .send::<_, Vec<crate::model::VehicleSummary>>(&ListVehicles { page: None, per_page: None })
            .await
            .unwrap_err();

        assert!(matches!(e, Error::MissingScope { .. }), "실제: {e:?}");
        assert!(matches!(e.billing(), crate::pricing::Billing::NotBilled));
        assert!(server.received_requests().await.unwrap().is_empty(),
                "선검사가 막았다면 요청이 나가면 안 된다");
    }

    /// ⚠️ JWT 가 아니거나 파싱 불가면 **통과**시킨다(fail-open).
    /// 서명 미검증 payload 는 권위가 없다 — 최종 판정은 서버의 403 이다.
    #[tokio::test]
    async fn opaque_token_passes_through() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200)
                .set_body_raw(br#"{"response":[]}"#.to_vec(), "application/json"))
            .mount(&server)
            .await;

        // JWT 가 아닌 불투명 토큰 — scp 를 관찰할 수 없다.
        let c = test_client_with_opaque_token(&server).await;
        let got: Vec<crate::model::VehicleSummary> = c
            .send(&ListVehicles { page: None, per_page: None })
            .await
            .unwrap();

        assert!(got.is_empty());
        assert_eq!(server.received_requests().await.unwrap().len(), 1,
                   "관찰 불가는 **통과**여야 한다(fail-open)");
    }

    /// ⚠️ Indeterminate 와 ContainsRequiredScope 가 **같은 경로**를 탄다.
    /// 호출부에서 갈라지면 fail-open 이 거기서 깨진다(design §10).
    #[tokio::test]
    async fn indeterminate_and_contains_take_the_same_path() {
        for scopes in [
            None,                                 // Indeterminate (JWT 아님)
            Some(&["vehicle_device_data"][..]),   // ContainsRequiredScope
        ] {
            let server = MockServer::start().await;
            Mock::given(method("GET"))
                .respond_with(ResponseTemplate::new(200)
                    .set_body_raw(br#"{"response":[]}"#.to_vec(), "application/json"))
                .mount(&server)
                .await;

            let c = match scopes {
                None => test_client_with_opaque_token(&server).await,
                Some(s) => test_client_with_scopes(&server, s).await,
            };
            let r: Result<Vec<crate::model::VehicleSummary>, _> =
                c.send(&ListVehicles { page: None, per_page: None }).await;

            assert!(r.is_ok(), "두 경우 모두 통과해야 한다: scopes={scopes:?}");
            assert_eq!(server.received_requests().await.unwrap().len(), 1,
                       "두 경우 모두 요청이 정확히 1회 나가야 한다");
        }
    }

    /// 403 은 `MissingScope` 가 아니라 `Error::Api` 다 — 과금 판정이 다르기 때문이다.
    /// MissingScope 는 네트워크 이전 차단(💰 0), 403 은 돈이 든 서버 거절이다.
    #[tokio::test]
    async fn server_403_is_api_error_not_missing_scope() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(403)
                .set_body_raw(br#"{"error":"insufficient scope"}"#.to_vec(), "application/json"))
            .mount(&server)
            .await;

        // 선검사는 통과시키되 서버가 거절하게 만든다.
        let c = test_client_with_scopes(&server, &["vehicle_device_data"]).await;
        let e = c
            .send::<_, Vec<crate::model::VehicleSummary>>(&ListVehicles { page: None, per_page: None })
            .await
            .unwrap_err();

        assert!(matches!(&e, Error::Api { status, .. } if status.as_u16() == 403), "실제: {e:?}");
        assert!(!matches!(e, Error::MissingScope { .. }),
                "서버 403 을 MissingScope 로 접으면 과금 판정이 뭉개진다");
        assert_eq!(server.received_requests().await.unwrap().len(), 1,
                   "이 요청은 실제로 나갔다 — 무과금 엔드포인트라 $0 이지만 나간 건 사실이다");
    }
```

- [ ] **Step 2~4:** 실패 확인 → 구현 → 통과 확인

`scope.rs` 의 `MissingScope` doc 에 caveat 추가:

```
/// ⚠️ 이 값은 **검증되지 않은 토큰에서 관찰된** 스코프에 근거한다. access token 이
/// 항상 JWT 라는 보장이 없고 서명 미검증 payload 는 권위가 없다. 이것은
/// **불필요한 유료 호출을 줄이는 advisory 비용 가드**이지 권한 판정이 아니다 —
/// 최종 판정은 서버의 403 이다.
```

- [ ] **Step 5: 크레이트 문서 마감**

`lib.rs` 크레이트 문서에 추가:
- read-only 강제의 **본진은 타입·가시성이고 clippy 는 보조**라는 사실
- 검증 수준(무엇이 실측이고 무엇이 문서 근거인지)
- 미결 O8(만료 토큰 401 형태)·O9(무인증 과금)·O10(408·5xx 실증거)·O11(중국 리전)

- [ ] **Step 6: 게이트 + 커밋**

```bash
cd <workspace>
cargo fmt --all && cargo clippy --all-targets --all-features --locked -- -D warnings && cargo test --locked && cargo +1.89.0 build --locked
cargo doc --no-deps --locked
git add -A
git commit -m "feat(a3): 스코프 advisory 배선 + 크레이트 문서 마감

Indeterminate 와 ContainsRequiredScope 를 코드에서 동일 취급한다 —
호출부에서 갈라지면 fail-open 이 거기서 깨진다.

403 은 MissingScope 로 바꾸지 않는다. MissingScope 는 네트워크 이전의
advisory 차단(💰 0)이고 403 은 돈이 든 서버 거절이라 billing() 이 갈린다.
과금 판정이 다르면 타입도 달라야 한다."
```

---

## 완료 조건

- [ ] 227 → **최소 290** 테스트, 전부 green
- [ ] `cargo clippy --all-targets --all-features --locked -- -D warnings` 통과
- [ ] `cargo +1.89.0 build --locked` (MSRV) 통과
- [ ] `cargo fmt --all -- --check` 통과
- [ ] `grep -rn "\.post(\|\.put(\|\.delete(\|\.patch(" crates/` 결과가 **`auth/refresh.rs` 한 곳뿐**
- [ ] 실측 401 3형태(A/E/B)가 전부 `Error::Auth` 이고 `Decode` 로 새지 않음
- [ ] 악성 리전 URL 7종이 전부 거부됨
- [ ] `Cargo.lock` 이 매 커밋에 포함됨

## A4 로 넘기는 것

- 나머지 13개 엔드포인트 · 페이지네이션 자동 순회(opt-in)
- **O8** 만료 access token 의 401 형태 실증거
- **O10** 408·5xx 실응답 증거 (현재 문서 근거뿐)
- **O11** 중국 리전 호스트 검증
- **O4** `GET /vehicles/{vin}` 의 실제 과금 카테고리를 대시보드로 대조
- A2 이월: `kv_secret` 멱등 가드 우회 · `acquire` 의 락 경로 symlink 방어

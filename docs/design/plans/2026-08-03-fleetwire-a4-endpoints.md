# fleetwire A4 — 엔드포인트 완성 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 라이브 검증 가능한 16개 엔드포인트 전부를 강타입으로 호출할 수 있게 하고, 페이지네이션 메타데이터를 호출자에게 전달하며, 첫 유료 호출을 비용 게이트·쿼터 관측과 함께 도입한다.

**Architecture:** `FleetGet` 에 `WireResponse` + `assemble` 조립 계약을 넣어 wire 타입과 공개 타입을 분리한다(초안의 `Response = Page<T>` 는 컴파일 불가였다). `Envelope` 가 `pagination` 형제 키를 포착하고 `decode` 가 `Decoded<T>` 로 넘기면 `send` 가 조립한다. 유료 2개는 `send_guarded`(fail-closed)와 `send_metered`(응답별 쿼터)로 감싼다.

**Tech Stack:** Rust 2024 · MSRV 1.89.0 · reqwest(rustls) · serde/serde_json · wiremock(dev) · http · thiserror

**설계 스펙:** [`2026-08-03-fleetwire-a4-endpoints-design.md`](../specs/2026-08-03-fleetwire-a4-endpoints-design.md) — 이 계획의 §참조는 전부 그 문서를 가리킨다.

---

## Global Constraints

이 절의 항목은 **모든 태스크의 요구사항에 암묵적으로 포함**된다.

- **코드 레포는 `<workspace>`** (branch `main`, remote 없음). BASE = `4d2d33a`, **316 테스트 통과**.
  ⚠️ `<docs repo>` 는 스펙·계획 보관처다. **코드 커밋 금지.**
- git author = `Jeong Seung Hwan <4435353+draken1@users.noreply.github.com>`.
  `git -c user.email=4435353+draken1@users.noreply.github.com -c user.name="Jeong Seung Hwan" commit …`
- **MSRV 1.89.0** · edition 2024 · `unsafe_code = "forbid"`.
- **읽기 전용 불변식**: POST 는 `auth/refresh.rs` **한 곳뿐**. 새 엔드포인트는 전부 GET.
- **`Cargo.lock` 을 항상 함께 커밋**한다(의존성이 바뀌면).
- 테스트는 **모듈 내부 `#[cfg(test)]`**. `tests/` 디렉터리 없음.
- 모든 사용자 노출 에러 문자열은 `redaction::body_preview` 를 통과한다.
- PATH: 새 셸마다 `export PATH="$HOME/.cargo/bin:$PATH"`.
- **기존 A1~A3 테스트를 수정하지 말 것.** 깨지면 멈추고 보고한다.

### 매 태스크 게이트 (5종 전부 통과해야 커밋)

```bash
cd <workspace>
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked
cargo +1.89.0 test --locked --no-run     # ⚠️ build 아님 — build 는 테스트 코드를 컴파일하지 않는다
cargo doc --no-deps --locked
```

### 매 brief 에 넣을 테스트 규율 5조 (A2 확립, A3 에서 효과 재확인)

1. **X 를 금지하는 규칙에는 X 를 했을 때만 실패하는 입력이 필요하다.**
2. **자기 테스트를 mutation-test 하고 생존자를 정직하게 분류하라** — "안 하기로 했다"를 "할 수 없다"로 격상하지 말 것.
3. **값이 아니라 기전을 단언하라.** `server.received_requests()` 로 요청 수를 세라.
4. 커버리지를 부풀리지 말 것.
5. vacuous 하게 통과할 수 있으면 큰 소리로 말하라.

### 실측 픽스처 (A4 태스크들이 공유, T1 이 만든다)

전부 2026-08-03 실측 원문이다. **바이트를 바꾸지 말 것.**

```rust
// test_support.rs
/// 408, 차량 offline. ⚠️ 봉투를 쓰고 `error_description` 이 **빈 문자열**이다.
pub(crate) const FIXTURE_408_OFFLINE: &str = r#"{"response":null,"error":"vehicle unavailable: vehicle is offline or asleep","error_description":"","txid":"432e7faaa94cf732bdda8f82f2184eea"}"#;

/// 목록 응답. ⚠️ `pagination` 과 `count` 가 `response` 의 **형제 키**다.
pub(crate) const FIXTURE_VEHICLES_PAGED: &str = r#"{"response":[{"id":1,"vin":"5YJ3E1EA1JF000001","state":"offline"}],"pagination":{"previous":null,"next":null,"current":1,"per_page":1,"count":1,"pages":1},"count":1}"#;

/// 게이트용 상태 조회. ⚠️ `backseat_token` 이 성공 응답에 실려 온다.
pub(crate) const FIXTURE_VEHICLE_STATE_OFFLINE: &str = r#"{"response":{"id":1,"vin":"5YJ3E1EA1JF000001","state":"offline","backseat_token":"SECRET-BACKSEAT","display_name":"","in_service":false}}"#;

/// ⚠️ `error` 가 **빈 문자열**이라 T3 의 폴백 사슬이 실제로 돈다.
/// FIXTURE_408_OFFLINE 만으로는 폴백이 아예 안 돌아 테스트가 vacuous 해진다(설계 §8 T-b).
pub(crate) const FIXTURE_ERR_EMPTY_ERROR: &str = r#"{"response":null,"error":"","error_description":"the vehicle could not be reached","txid":"deadbeef"}"#;
```

rate-limit 헤더 3종(실측값):
`ratelimit-device-data-per-vehicle-limit: 60` · `-remaining: 59` · `-reset: 57`

---

## File Structure

| 파일 | 책임 | 태스크 |
|---|---|---|
| `src/endpoint.rs` | `FleetGet` 에 `WireResponse`·`assemble` 추가 + `passthrough_endpoint!` 매크로 | T1 |
| `src/model.rs` | `Envelope` 가 `pagination` 포착 · `Page.pagination: Option<_>` · 응답 타입 신설 | T1·T2·T4~T8 |
| `src/decode.rs` | `Decoded<T>` 반환 · 에러 메시지 사슬 · `txid` 추출 | T2·T3 |
| `src/error.rs` | `Api`·`VehicleOffline` 에 `txid` | T3 |
| `src/client.rs` | `send` 조립 · `send_guarded` · `send_metered` | T2·T9·T10 |
| `src/api/vehicles.rs` | 유료 2 + 단순 7 + invitations = 10 엔드포인트 | T4~T7 |
| `src/api/users.rs` | me · orders · feature_config | T8 |
| `src/test_support.rs` | 실측 픽스처 4종 | T1 |
| `src/lib.rs` | export | 전 태스크 |

**태스크 순서 근거:** T1(조립 계약 — 전부가 의존) → T2(페이지네이션 배관) → T3(에러 성숙) → T4~T8(엔드포인트 13개) → T9(게이트) → T10(쿼터 관측) → T11(문서 마감).

⚠️ T9(게이트)를 유료 엔드포인트(T4·T5) **뒤**에 두는 이유: 게이트할 대상이 없으면 게이트 테스트가 가짜가 된다.

---

### Task 1: 조립 계약 (`WireResponse` + `assemble`) + 실측 픽스처

**Files:**
- Modify: `crates/fleetwire/src/endpoint.rs`, `crates/fleetwire/src/api/users.rs`, `crates/fleetwire/src/api/vehicles.rs`, `crates/fleetwire/src/test_support.rs`
- Test: 같은 파일들의 `#[cfg(test)]`

**Interfaces:**
- Consumes: A3 의 `FleetGet { type Response; required_scopes; pricing; path; query }`, `Pagination`, `Page<T>`.
- Produces:
  ```rust
  pub trait FleetGet: crate::sealed::Sealed + Send + Sync {
      type WireResponse: serde::de::DeserializeOwned;
      type Response;
      fn assemble(wire: Self::WireResponse, pagination: Option<Pagination>) -> Self::Response;
      fn required_scopes(&self) -> ScopeExpr;
      fn pricing(&self) -> Pricing;
      fn path(&self) -> String;
      fn query(&self) -> QuerySpec;
  }
  macro_rules! passthrough_endpoint { ($t:ty, $resp:ty) => { … } }
  ```
  + `test_support` 의 픽스처 4종(Global Constraints 참조).

⚠️ **이 태스크는 아직 페이지네이션을 배관하지 않는다.** 기존 3개 엔드포인트는 전부 `WireResponse = Response` 이고 `assemble` 이 wire 를 그대로 반환한다. 배관은 T2 다.

- [ ] **Step 1: 실패하는 테스트를 쓴다**

`endpoint.rs` 의 `#[cfg(test)] mod tests` 에 추가:

```rust
    /// passthrough 엔드포인트는 pagination 을 **버린다** — wire 를 그대로 낸다.
    #[test]
    fn passthrough_assemble_ignores_pagination() {
        let p = Pagination {
            previous: None, next: Some(2), current: Some(1),
            per_page: Some(1), count: Some(9), pages: Some(9),
        };
        let out = <DummyVehicles as FleetGet>::assemble(serde_json::json!({"a":1}), Some(p));
        assert_eq!(out, serde_json::json!({"a":1}));
    }

    /// ⚠️ assemble 은 연관 함수다 — `&self` 를 받지 않는다.
    /// 요청 인스턴스 값이 응답에 섞이면 안 되기 때문이다(설계 §2.2).
    #[test]
    fn assemble_needs_no_request_instance() {
        // 인스턴스 없이 호출된다는 사실 자체가 계약이다.
        let out = <DummyVehicles as FleetGet>::assemble(serde_json::Value::Null, None);
        assert!(out.is_null());
    }
```

`DummyVehicles` 의 impl 에 `type WireResponse = serde_json::Value;` 를 더한다(기존 `type Response = serde_json::Value;` 유지).

- [ ] **Step 2: 실패 확인**

Run: `cd <workspace> && cargo test --locked endpoint::`
Expected: FAIL — `no function or associated item named 'assemble'`

- [ ] **Step 3: 구현한다**

`endpoint.rs`:

```rust
pub trait FleetGet: crate::sealed::Sealed + Send + Sync {
    /// 봉투의 `response` 키에서 **직접** 역직렬화되는 타입.
    ///
    /// 공개 [`Self::Response`] 와 다를 수 있다 — 목록 엔드포인트는 wire 에서 `Vec<T>` 로
    /// 오지만 호출자에게는 페이지 메타데이터가 붙은 [`crate::model::Page`] 를 준다.
    type WireResponse: serde::de::DeserializeOwned;
    /// 호출자가 받는 타입.
    type Response;
    /// wire 값과 형제 메타데이터를 공개 타입으로 조립한다.
    ///
    /// ⚠️ **연관 함수다(`&self` 없음).** 조립은 요청 인스턴스의 값에 의존하지 않는다.
    /// `&self` 를 받으면 구현자가 요청 상태를 응답에 섞을 수 있고, 그건 이 계층의 책임이 아니다.
    fn assemble(wire: Self::WireResponse, pagination: Option<crate::model::Pagination>)
        -> Self::Response;
    fn required_scopes(&self) -> ScopeExpr;
    fn pricing(&self) -> Pricing;
    fn path(&self) -> String;
    fn query(&self) -> QuerySpec;
}
```

`api/mod.rs` 에 매크로(13개 중 11개가 이 형태):

```rust
/// 페이지네이션이 없는 엔드포인트의 `WireResponse`/`assemble` 보일러플레이트.
///
/// wire 타입과 공개 타입이 같고, 형제 메타데이터를 버린다.
macro_rules! passthrough_endpoint {
    ($t:ty, $resp:ty) => {
        type WireResponse = $resp;
        type Response = $resp;
        fn assemble(
            wire: Self::WireResponse,
            _pagination: Option<$crate::model::Pagination>,
        ) -> Self::Response {
            wire
        }
    };
}
pub(crate) use passthrough_endpoint;
```

기존 3개 엔드포인트를 매크로로 이전한다:
- `GetRegion`: `passthrough_endpoint!(GetRegion, RegionInfo);` (기존 `type Response = RegionInfo;` 제거)
- `GetVehicle`: `passthrough_endpoint!(GetVehicle, VehicleSummary);`
- `ListVehicles`: **T2 가 `Page` 로 바꾼다.** 지금은 `passthrough_endpoint!(ListVehicles, Vec<VehicleSummary>);`

`test_support.rs` 에 Global Constraints 의 픽스처 4종을 `pub(crate) const` 로 추가한다.

- [ ] **Step 4: 통과 확인**

Run: `cd <workspace> && cargo test --locked`
Expected: 316 + 2 = **318 통과**. 기존 테스트 무변경.

- [ ] **Step 5: mutation-test**

| # | mutation | 죽어야 하는 테스트 |
|---|---|---|
| M1 | `assemble` 이 `wire` 대신 `Default::default()` 반환 | `passthrough_assemble_ignores_pagination` |
| M2 | 매크로가 `pagination` 을 `Response` 에 섞음 | 같은 테스트 |

- [ ] **Step 6: 게이트 + 커밋**

```bash
cd <workspace>
cargo fmt --all && cargo clippy --all-targets --all-features --locked -- -D warnings && cargo test --locked && cargo +1.89.0 test --locked --no-run && cargo doc --no-deps --locked
git add crates/fleetwire/src/endpoint.rs crates/fleetwire/src/api crates/fleetwire/src/test_support.rs
git commit -m "feat(a4): FleetGet 에 WireResponse+assemble 조립 계약 도입

설계 초안의 Response=Page<T> 는 컴파일되지 않았다 — wire 의 response 는 Vec<T> 인데
공개 타입은 Page<T> 라 제네릭 send 가 무엇으로 역직렬화할지 알 방법이 없었다.
wire 타입과 공개 타입을 분리하고 조립을 명시 계약으로 만든다.

assemble 은 연관 함수다 — 조립이 요청 인스턴스 값에 의존하면 안 된다.
13개 중 11개가 passthrough 라 매크로로 보일러플레이트를 줄인다."
```

---

### Task 2: 페이지네이션 배관 — `Envelope` → `Decoded` → `send`

**Files:**
- Modify: `crates/fleetwire/src/model.rs`, `crates/fleetwire/src/decode.rs`, `crates/fleetwire/src/client.rs`, `crates/fleetwire/src/api/vehicles.rs`
- Test: 같은 파일들

**Interfaces:**
- Consumes: T1 의 `WireResponse`/`assemble`/`passthrough_endpoint!`, 픽스처 `FIXTURE_VEHICLES_PAGED`.
- Produces:
  ```rust
  // model.rs
  pub(crate) struct Envelope<T> { pub response: T, pub pagination: Option<Pagination> }
  pub struct Page<T> { pub items: Vec<T>, pub pagination: Option<Pagination> }  // Option 으로 변경
  // decode.rs
  pub(crate) struct Decoded<T> { pub payload: T, pub pagination: Option<Pagination> }
  pub(crate) fn decode<T>(…) -> Result<Decoded<T>, Error>   // 반환 타입 변경
  // api/vehicles.rs
  impl FleetGet for ListVehicles { type WireResponse = Vec<VehicleSummary>; type Response = Page<VehicleSummary>; }
  ```

⚠️ **최상위 `count` 는 모델링하지 않는다.** 실측에서 `count` == `pagination.count` == 1 로 같은 값이었다. 별도 필드를 두면 `Page.count` 와 `Page.pagination.count` 라는 이름도 값도 같은 필드가 둘 생긴다. serde 는 모르는 키를 무시하므로 파싱은 깨지지 않는다.

⚠️ **`Page.pagination` 은 `Option` 이다 — 기본값으로 채우지 않는다.** `None` = 서버가 안 줬다. 전 필드가 `None` 인 `Some(Pagination)` 과 **다르다.** 기본값으로 채우면 호출자가 `next == None` 을 "다음 페이지 없음"으로 읽어 순회를 조기 종료한다.

- [ ] **Step 1: 실패하는 테스트 4개를 쓴다**

`client.rs` 테스트 모듈에:

```rust
    /// 실측 픽스처의 pagination 형제 키가 Page 에 도달한다.
    #[tokio::test]
    async fn pagination_sibling_reaches_the_caller() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/1/vehicles"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                crate::test_support::FIXTURE_VEHICLES_PAGED.as_bytes().to_vec(),
                "application/json",
            ))
            .mount(&server)
            .await;

        let c = test_client(&server).await;
        let page: Page<VehicleSummary> = c
            .send(&ListVehicles { page: None, per_page: None })
            .await
            .unwrap();

        assert_eq!(page.items.len(), 1);
        let p = page.pagination.expect("서버가 pagination 을 줬다");
        assert_eq!(p.count, Some(1));
        assert_eq!(p.pages, Some(1));
        assert_eq!(p.current, Some(1));
    }

    /// ⚠️ pagination 이 없으면 None 이다 — 전 필드 None 인 Some 과 구분된다.
    #[tokio::test]
    async fn absent_pagination_is_none_not_defaulted() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/1/vehicles"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                br#"{"response":[{"vin":"5YJ3E1EA1JF000001"}]}"#.to_vec(),
                "application/json",
            ))
            .mount(&server)
            .await;

        let c = test_client(&server).await;
        let page: Page<VehicleSummary> = c
            .send(&ListVehicles { page: None, per_page: None })
            .await
            .unwrap();

        assert_eq!(page.items.len(), 1, "목록 자체는 유효하므로 실패시키지 않는다");
        assert!(page.pagination.is_none(), "부재는 None 이어야 한다");
    }

    /// passthrough 엔드포인트는 pagination 이 와도 영향받지 않는다.
    #[tokio::test]
    async fn passthrough_endpoint_ignores_a_pagination_sibling() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/1/users/region"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                br#"{"response":{"region":"na"},"pagination":{"count":7}}"#.to_vec(),
                "application/json",
            ))
            .mount(&server)
            .await;

        let c = test_client(&server).await;
        let got: RegionInfo = c.send(&GetRegion).await.unwrap();
        assert_eq!(got.region.as_deref(), Some("na"));
    }

    /// page/per_page 가 쿼리에 실린다(요청 쪽 회귀 가드).
    #[test]
    fn list_vehicles_sends_pagination_params() {
        let q = ListVehicles { page: Some(3), per_page: Some(25) }.query().to_encoded();
        assert!(q.contains("page=3") && q.contains("per_page=25"), "실제: {q}");
    }
```

- [ ] **Step 2: 실패 확인**

Run: `cd <workspace> && cargo test --locked client::`
Expected: FAIL — `Page` 로 추론되지 않음 / `pagination` 필드 타입 불일치

- [ ] **Step 3: 구현한다**

`model.rs`:

```rust
#[derive(serde::Deserialize)]
pub(crate) struct Envelope<T> {
    pub response: T,
    /// 목록 응답에서 `response` 의 **형제**로 오는 페이지 메타데이터(실측 2026-08-03).
    /// ⚠️ 최상위 `count` 도 형제로 오지만 `pagination.count` 와 같은 값이라 모델링하지 않는다.
    #[serde(default)]
    pub pagination: Option<Pagination>,
}

#[non_exhaustive]
pub struct Page<T> {
    pub items: Vec<T>,
    /// `None` = 서버가 메타데이터를 **주지 않았다**. 전 필드가 `None` 인 `Some` 과 다르다 —
    /// 기본값으로 채우면 호출자가 `next == None` 을 "다음 페이지 없음"으로 읽어
    /// 순회를 조기 종료한다.
    pub pagination: Option<Pagination>,
}
```

⚠️ `Page` 의 기존 doc("호출자가 두 조각으로 조립한다")을 고친다 — A3 까지 그 두 번째 조각은 호출자에게 **도달하지 않았다**. 이제 `assemble` 이 조립한다고 적는다.

`decode.rs`:

```rust
pub(crate) struct Decoded<T> {
    pub payload: T,
    pub pagination: Option<Pagination>,
}
```

`decode` 의 성공 arm 을 바꾼다(⚠️ **`status >= 400` 이 content-type 검사보다 먼저인 순서를 절대 바꾸지 말 것** — 그 순서가 401/408 을 `Decode` 오분류에서 막는다):

```rust
    match serde_json::from_slice::<Envelope<T>>(body) {
        Ok(e) => Ok(Decoded { payload: e.response, pagination: e.pagination }),
        // ⚠️ Err arm 은 **한 줄도 바꾸지 않는다.** A1 이 serde_json 의 `Category::Data` 만
        //    바디 값을 보간한다는 것을 소스로 검증해 그 카테고리에서만 값을 제거하도록
        //    만든 코드다. 반환 타입만 `Decoded<T>` 로 바뀌므로 이 arm 은
        //    `Err(...)` 를 그대로 돌려주면 되고 본문 수정이 필요 없다.
        Err(err) => { /* A1 의 기존 본문을 그대로 둔다 */ }
    }
```

`client.rs` 의 `send` 마지막 단계:

```rust
        let decoded = crate::decode::decode::<R::WireResponse>(
            raw.status, raw.content_type.as_deref(), &raw.body,
            req.pricing(), raw.retry_after, raw.www_authenticate.as_deref(),
        )?;
        Ok(R::assemble(decoded.payload, decoded.pagination))
```

`api/vehicles.rs` 의 `ListVehicles`:

```rust
impl FleetGet for ListVehicles {
    type WireResponse = Vec<VehicleSummary>;
    type Response = Page<VehicleSummary>;
    fn assemble(wire: Self::WireResponse, pagination: Option<Pagination>) -> Self::Response {
        Page { items: wire, pagination }
    }
    // required_scopes / pricing / path / query 는 그대로
}
```

⚠️ 기존 A3 테스트 중 `ListVehicles` 응답을 `Vec<VehicleSummary>` 로 받는 것들이 **컴파일되지 않는다.** 이는 승인된 breaking 변경이다(`publish = false`). **단언은 바꾸지 말고** 타입만 `Page<VehicleSummary>` 로 고치고 `.items` 를 경유하게 한다. 단언 의미가 바뀌어야 한다면 멈추고 보고할 것.

- [ ] **Step 4: 통과 확인**

Run: `cd <workspace> && cargo test --locked`
Expected: 318 + 4 = **322 통과**

- [ ] **Step 5: mutation-test**

| # | mutation | 죽어야 하는 테스트 |
|---|---|---|
| M1 | `Envelope.pagination` 을 항상 `None` 으로 | `pagination_sibling_reaches_the_caller` |
| M2 | 부재 시 `Some(Pagination::default())` 로 채움 | `absent_pagination_is_none_not_defaulted` |
| M3 | `decode` 의 `status >= 400` 검사를 content-type 뒤로 이동 | A3 의 `empty_html_401_is_auth_not_decode` (기존 테스트가 잡아야 한다) |

**M3 이 살아남으면 보고할 것** — A3 의 순서 계약이 무너졌다는 뜻이다.

- [ ] **Step 6: 게이트 + 커밋**

```bash
cd <workspace>
cargo fmt --all && cargo clippy --all-targets --all-features --locked -- -D warnings && cargo test --locked && cargo +1.89.0 test --locked --no-run && cargo doc --no-deps --locked
git add crates/fleetwire/src/model.rs crates/fleetwire/src/decode.rs crates/fleetwire/src/client.rs crates/fleetwire/src/api/vehicles.rs
git commit -m "feat(a4): 페이지네이션 배관 — pagination 형제 키가 호출자에게 도달한다

A3 까지 decode 가 봉투의 response 만 벗기고 형제 pagination 을 **전량 폐기**했다.
Page<T>/Pagination 이 소비자 0개였던 근본 원인이고, model.rs 의 '호출자가 두 조각으로
조립한다'는 doc 은 도달하지 않는 조각을 전제하고 있었다.

Page.pagination 은 Option 이다 — 기본값으로 채우면 '서버가 안 줌'과 '전부 null'을
구분할 수 없고, 호출자가 next==None 을 보고 순회를 조기 종료한다.

최상위 count 는 모델링하지 않는다 — 실측상 pagination.count 와 같은 값이라
이름도 값도 같은 필드가 둘 생긴다."
```

---

### Task 3: 에러 성숙 — 빈 문자열 건너뛰기 + `txid`

**Files:**
- Modify: `crates/fleetwire/src/error.rs`, `crates/fleetwire/src/decode.rs`
- Test: 같은 파일들

**Interfaces:**
- Consumes: T1 의 픽스처 `FIXTURE_408_OFFLINE`·`FIXTURE_ERR_EMPTY_ERROR`.
- Produces:
  ```rust
  Error::Api            { status, message, retry_after, body_preview, pricing, txid: Option<String> }
  Error::VehicleOffline { pricing, txid: Option<String> }
  ```

⚠️ `Error` 는 **enum 과 각 variant 양쪽에** `#[non_exhaustive]` 가 있다(코드 확인 완료) — struct-like variant 에 필드를 더하는 것도 non-breaking 이다.

⚠️ **`error_description` 은 두 곳에 있다.** 바디 필드(408 실측)와 `WWW-Authenticate` 파라미터(401 실측). A3 는 헤더 쪽만 봤다. A4 는 바디 쪽을 사슬에 넣되 **빈 문자열을 건너뛴다.**

최종 순서: body `error`(비어있지 않을 것) → body `error_description`(비어있지 않을 것) → `WWW-Authenticate` 의 `error_description`(비어있지 않을 것) → `canonical_reason()`.

- [ ] **Step 1: 실패하는 테스트 5개를 쓴다**

`decode.rs` 테스트 모듈에:

```rust
    /// ⚠️ 빈 error_description 이 메시지로 이기면 안 된다(408 실측이 "" 를 준다).
    /// 이 픽스처는 error 도 비어 있어 폴백 사슬이 **실제로 돈다** — FIXTURE_408_OFFLINE
    /// 만으로는 error 가 1순위로 이겨서 테스트가 vacuous 하다.
    #[test]
    fn empty_error_is_skipped_and_description_wins() {
        let e = decode::<serde_json::Value>(
            http::StatusCode::from_u16(400).unwrap(),
            Some("application/json"),
            crate::test_support::FIXTURE_ERR_EMPTY_ERROR.as_bytes(),
            Pricing::NotCharged, None, None,
        ).unwrap_err();
        assert!(e.to_string().contains("could not be reached"), "실제: {e}");
    }

    /// 둘 다 비어 있으면 canonical_reason 으로 떨어진다.
    #[test]
    fn both_empty_falls_through_to_canonical_reason() {
        let body = br#"{"response":null,"error":"","error_description":""}"#;
        let e = decode::<serde_json::Value>(
            http::StatusCode::from_u16(502).unwrap(),
            Some("application/json"), body, Pricing::NotCharged, None, None,
        ).unwrap_err();
        assert!(e.to_string().contains("Bad Gateway"), "실제: {e}");
    }

    /// 408 의 txid 가 VehicleOffline 에 실린다.
    /// ⚠️ Error::Api 에만 실으면 **우리가 실측한 바로 그 응답**에서 txid 가 소실된다.
    #[test]
    fn txid_survives_on_vehicle_offline() {
        let e = decode::<serde_json::Value>(
            http::StatusCode::from_u16(408).unwrap(),
            Some("application/json"),
            crate::test_support::FIXTURE_408_OFFLINE.as_bytes(),
            Pricing::data(), None, None,
        ).unwrap_err();
        match e {
            Error::VehicleOffline { txid, .. } => {
                assert_eq!(txid.as_deref(), Some("432e7faaa94cf732bdda8f82f2184eea"));
            }
            other => panic!("VehicleOffline 이어야 한다: {other:?}"),
        }
    }

    /// 바디에 txid 가 없으면 헤더 x-txid 로 폴백한다.
    #[test]
    fn txid_falls_back_to_the_header() {
        let e = decode_with_txid_header::<serde_json::Value>(
            http::StatusCode::from_u16(500).unwrap(),
            Some("application/json"), br#"{"error":"boom"}"#,
            Pricing::NotCharged, Some("hdr-txid-123"),
        ).unwrap_err();
        match e {
            Error::Api { txid, .. } => assert_eq!(txid.as_deref(), Some("hdr-txid-123")),
            other => panic!("Api 여야 한다: {other:?}"),
        }
    }

    /// ⚠️ txid 는 리댁션되지 않는다 — Tesla 지원 문의에 필요하다.
    #[test]
    fn txid_is_not_redacted() {
        let e = decode::<serde_json::Value>(
            http::StatusCode::from_u16(408).unwrap(),
            Some("application/json"),
            crate::test_support::FIXTURE_408_OFFLINE.as_bytes(),
            Pricing::data(), None, None,
        ).unwrap_err();
        assert!(format!("{e:?}").contains("432e7faa"), "txid 가 사라졌다: {e:?}");
    }
```

- [ ] **Step 2: 실패 확인**

Run: `cd <workspace> && cargo test --locked decode::`
Expected: FAIL — `txid` 필드 없음, 빈 문자열이 이김

- [ ] **Step 3: 구현한다**

`error.rs` — 두 variant 에 필드 추가:

```rust
    Api {
        // … 기존 필드
        /// Tesla 트랜잭션 ID(바디 `txid` → 헤더 `x-txid`). 지원 문의 시 이 값을 준다.
        ///
        /// ⚠️ **리댁션 대상이 아니다.** 계정 UUID 는 가리지만 트랜잭션 ID 는 보존해야
        /// 진단이 가능하다(A3 T7 의 `x-request-id` 보존 결정과 같은 논리).
        /// 다만 상관관계 식별자이므로 `Display` 에는 자동 노출하지 않고 필드로만 둔다.
        txid: Option<String>,
    },
    VehicleOffline {
        pricing: Pricing,
        /// 위와 동일. ⚠️ 실측한 408 이 바로 이 variant 로 오므로 여기 없으면
        /// 관측된 txid 가 통째로 소실된다.
        txid: Option<String>,
    },
```

⚠️ `#[error(...)]` 템플릿에 `txid` 를 넣지 **않는다**(Display 비노출).

`decode.rs` — 메시지 사슬과 txid 추출:

```rust
/// 비어 있지 않은 문자열만 통과시킨다.
///
/// ⚠️ 408 실측이 `"error_description": ""` 를 준다 — 키가 있다는 이유로 빈 문자열이
/// 최종 메시지가 되면 안 된다.
fn non_empty(s: Option<&str>) -> Option<&str> {
    s.filter(|v| !v.trim().is_empty())
}

fn error_message(body: &[u8], www_authenticate: Option<&str>, status: http::StatusCode) -> String {
    let parsed = serde_json::from_slice::<serde_json::Value>(body).ok();
    if let Some(v) = &parsed {
        if let Some(s) = non_empty(v.get("error").and_then(|e| e.as_str())) {
            return s.to_owned();
        }
        if let Some(s) = non_empty(v.get("error_description").and_then(|e| e.as_str())) {
            return s.to_owned();
        }
    }
    if let Some(d) = www_authenticate.and_then(extract_error_description)
        && !d.trim().is_empty()
    {
        return d;
    }
    status.canonical_reason().unwrap_or("unknown error").to_owned()
}

/// 바디 `txid` → 헤더 `x-txid` 순.
fn extract_txid(body: &[u8], header_txid: Option<&str>) -> Option<String> {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|v| v.get("txid").and_then(|t| t.as_str()).map(str::to_owned))
        .or_else(|| non_empty(header_txid).map(str::to_owned))
}
```

**`extract_txid` 를 실제로 배선한다** — 두 arm 모두:

```rust
    if status.as_u16() >= 400 {
        let txid = extract_txid(body, x_txid);          // ← 두 arm 이 공유
        return Err(match status.as_u16() {
            401 => Error::Auth { /* 기존 그대로 */ },
            408 => Error::VehicleOffline { pricing, txid },
            _ => Error::Api {
                status,
                message: body_preview(&error_message(body, www_authenticate, status),
                                      BODY_PREVIEW_MAX),
                retry_after,
                body_preview: body_preview(&String::from_utf8_lossy(body), BODY_PREVIEW_MAX),
                pricing,
                txid,
            },
        });
    }
```

⚠️ `txid` 는 `body_preview` 를 **통과시키지 않는다** — 리댁션 대상이 아니기 때문이다(§6.2).

**시그니처 정리**: 실제 함수는 `x_txid` 를 받는 쪽이고, 기존 이름을 얇은 래퍼로 남긴다 —
A1~A3 의 호출부 27곳을 건드리지 않기 위해서다.

```rust
/// 실제 구현. 테스트가 헤더 txid 를 주입할 때 이 이름을 쓴다.
pub(crate) fn decode_with_txid_header<T: serde::de::DeserializeOwned>(
    status: http::StatusCode,
    content_type: Option<&str>,
    body: &[u8],
    pricing: Pricing,
    retry_after: Option<std::time::Duration>,
    www_authenticate: Option<&str>,
    x_txid: Option<&str>,
) -> Result<Decoded<T>, Error> { /* 위 본문 */ }

/// 기존 호출부 호환 래퍼(헤더 txid 없음).
pub(crate) fn decode<T: serde::de::DeserializeOwned>(
    status: http::StatusCode,
    content_type: Option<&str>,
    body: &[u8],
    pricing: Pricing,
    retry_after: Option<std::time::Duration>,
    www_authenticate: Option<&str>,
) -> Result<Decoded<T>, Error> {
    decode_with_txid_header(status, content_type, body, pricing, retry_after, www_authenticate, None)
}
```

⚠️ 위 테스트의 `decode_with_txid_header` 호출은 인자 7개다 — `retry_after`/`www_authenticate` 자리에
`None, None` 을 넣어야 한다. 테스트 코드의 인자 개수를 실제 시그니처에 맞출 것.

`transport.rs` 의 `RawResponse` 에 `x_txid: Option<String>` 을 더하고 `execute_get` 에서 `x-txid`
헤더를 읽는다(`www_authenticate` 를 읽는 것과 같은 `header()` 클로저 사용). `client.rs` 의 `send` 는
`decode_with_txid_header(..., raw.x_txid.as_deref())` 를 호출한다.

- [ ] **Step 4: 통과 확인**

Run: `cd <workspace> && cargo test --locked`
Expected: 322 + 5 = **327 통과**. 기존 `Error::VehicleOffline { pricing }` 패턴 매칭이 깨지면 `..` 를 더해 고치되 **단언은 바꾸지 말 것.**

- [ ] **Step 5: mutation-test**

| # | mutation | 죽어야 하는 테스트 |
|---|---|---|
| M1 | `non_empty` 를 항등함수로 | `empty_error_is_skipped_and_description_wins` |
| M2 | `txid` 를 `Api` 에만 실음 | `txid_survives_on_vehicle_offline` |
| M3 | `txid` 를 `body_preview` 로 통과시킴(리댁션) | `txid_is_not_redacted` |
| M4 | 바디/헤더 폴백 순서 뒤바꿈 | `txid_falls_back_to_the_header` |
| M5 | `#[error]` 템플릿에 `{txid}` 추가 | (생존 예상 — Display 비노출 테스트가 없으면 **추가할 것**) |

- [ ] **Step 6: 게이트 + 커밋**

```bash
cd <workspace>
cargo fmt --all && cargo clippy --all-targets --all-features --locked -- -D warnings && cargo test --locked && cargo +1.89.0 test --locked --no-run && cargo doc --no-deps --locked
git add crates/fleetwire/src/error.rs crates/fleetwire/src/decode.rs crates/fleetwire/src/transport.rs crates/fleetwire/src/client.rs
git commit -m "feat(a4): 빈 error_description 건너뛰기 + txid 보존

408 실측이 error_description 을 **빈 문자열**로 준다 — 키가 있다는 이유로 빈 메시지가
최종 결과가 되면 안 된다. 사슬 전 단계에 '비어있지 않을 것' 조건을 넣는다.

txid 를 Api 와 VehicleOffline **양쪽**에 싣는다. Api 에만 실으면 우리가 실측한 바로 그
408 이 VehicleOffline 로 분류되면서 txid 가 소실된다.

txid 는 리댁션하지 않는다(지원 문의에 필요) — 단 상관관계 식별자이므로 Display 에는
자동 노출하지 않고 필드로만 둔다."
```

---

### Task 4: `vehicle_data` — 첫 유료 엔드포인트 + 조건부 스코프

**Files:**
- Modify: `crates/fleetwire/src/api/vehicles.rs`, `crates/fleetwire/src/model.rs`
- Test: 같은 파일들

**Interfaces:**
- Consumes: T1 의 `passthrough_endpoint!`, A3 의 `GetVehicle::new` 패턴(`encode_vin_segment`).
- Produces:
  ```rust
  #[non_exhaustive]
  pub enum DataEndpoint { ChargeState, ClimateState, DriveState, LocationData, VehicleConfig, VehicleState, GuiSettings, ClosuresState }
  pub struct GetVehicleData { /* private */ }
  impl GetVehicleData { pub fn new(vin: impl Into<String>, endpoints: Vec<DataEndpoint>) -> Result<Self, VinError>; }
  pub struct VehicleData { /* Option 필드 + flatten extra */ }
  ```

⚠️ **`Pricing::data()`** — 이 크레이트에서 처음으로 돈이 나가는 엔드포인트다.

⚠️ **조건부 스코프가 이 태스크의 핵심이다.** `ScopeExpr` 의 실제 정의는 `All(Vec<ScopeExpr>)`·`Any(Vec<ScopeExpr>)`·`One(Scope)` 다 — 설계 스케치의 `All(&[Scope])` 가 **아니다.**

⚠️ **`DataEndpoint` 는 열거형이다.** 자유 문자열은 오타를 유료 요청으로 바꾼다. `#[non_exhaustive]` 로 두어 Tesla 가 값을 추가해도 breaking 이 아니게 한다.

- [ ] **Step 1: 실패하는 테스트 5개를 쓴다**

```rust
    /// ⚠️ 위치를 요청할 때만 vehicle_location 을 요구한다.
    #[test]
    fn location_data_requires_the_location_scope() {
        let with = GetVehicleData::new("5YJ3E1EA1JF000001",
            vec![DataEndpoint::ChargeState, DataEndpoint::LocationData]).unwrap();
        assert!(with.required_scopes()
            .is_satisfied_by(&[Scope::VehicleDeviceData, Scope::VehicleLocation]));
        assert!(!with.required_scopes().is_satisfied_by(&[Scope::VehicleDeviceData]),
            "위치를 요청했는데 device_data 만으로 만족되면 안 된다");
    }

    #[test]
    fn without_location_data_only_device_data_is_required() {
        let without = GetVehicleData::new("5YJ3E1EA1JF000001",
            vec![DataEndpoint::ChargeState]).unwrap();
        assert!(without.required_scopes().is_satisfied_by(&[Scope::VehicleDeviceData]));
    }

    /// ⚠️ location_data 를 지정하지 않으면 쿼리에 그게 없어야 한다(F9 이중 opt-in).
    #[test]
    fn location_data_is_absent_from_the_query_unless_asked() {
        let r = GetVehicleData::new("5YJ3E1EA1JF000001",
            vec![DataEndpoint::ChargeState]).unwrap();
        let q = r.query().to_encoded();
        assert!(!q.contains("location_data"), "실제: {q}");
        assert!(q.contains("charge_state"), "실제: {q}");
    }

    /// 유료다. 오분류하면 과금 보고가 틀어진다.
    #[test]
    fn vehicle_data_is_a_paid_endpoint() {
        let r = GetVehicleData::new("5YJ3E1EA1JF000001", vec![DataEndpoint::ChargeState]).unwrap();
        assert!(matches!(r.pricing(), Pricing::Data { .. }));
    }

    /// endpoints 가 비면 쿼리에 endpoints 파라미터를 아예 넣지 않는다.
    /// (Tesla 기본 동작에 맡긴다 — 빈 문자열을 보내면 의미가 불명확하다)
    #[test]
    fn empty_endpoints_omits_the_parameter() {
        let r = GetVehicleData::new("5YJ3E1EA1JF000001", vec![]).unwrap();
        assert_eq!(r.query().to_encoded(), "");
    }
```

- [ ] **Step 2: 실패 확인**

Run: `cd <workspace> && cargo test --locked api::vehicles`
Expected: FAIL — `GetVehicleData` 없음

- [ ] **Step 3: 구현한다**

```rust
/// `vehicle_data` 의 `endpoints=` 파라미터로 선택할 수 있는 하위 데이터.
///
/// ⚠️ 열거형인 이유: 자유 문자열은 오타를 **유료 요청**으로 바꾼다.
/// `#[non_exhaustive]` 이므로 Tesla 가 값을 추가해도 breaking 이 아니다.
#[non_exhaustive]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DataEndpoint {
    ChargeState, ClimateState, DriveState, LocationData,
    VehicleConfig, VehicleState, GuiSettings, ClosuresState,
}

impl DataEndpoint {
    fn as_wire(self) -> &'static str {
        match self {
            Self::ChargeState => "charge_state",
            Self::ClimateState => "climate_state",
            Self::DriveState => "drive_state",
            Self::LocationData => "location_data",
            Self::VehicleConfig => "vehicle_config",
            Self::VehicleState => "vehicle_state",
            Self::GuiSettings => "gui_settings",
            Self::ClosuresState => "closures_state",
        }
    }
    /// 이 항목이 `vehicle_location` 스코프를 요구하는가.
    fn needs_location(self) -> bool {
        matches!(self, Self::LocationData)
    }
}

/// `GET /api/1/vehicles/{vin}/vehicle_data` — ⚠️ **유료**($0.002/요청).
pub struct GetVehicleData {
    vin: String,
    encoded_segment: String,
    endpoints: Vec<DataEndpoint>,
}

impl GetVehicleData {
    pub fn new(vin: impl Into<String>, endpoints: Vec<DataEndpoint>) -> Result<Self, VinError> {
        let vin = vin.into();
        let encoded_segment = encode_vin_segment(&vin)?;
        Ok(Self { vin, encoded_segment, endpoints })
    }
}

impl crate::sealed::Sealed for GetVehicleData {}

impl FleetGet for GetVehicleData {
    passthrough_endpoint!(GetVehicleData, VehicleData);

    /// ⚠️ 요청값에 의존한다 — 위치를 요청할 때만 `vehicle_location` 을 더한다.
    /// A1 이 `required_scopes` 를 상수가 아니라 **메서드**로 둔 이유가 이것이다.
    fn required_scopes(&self) -> ScopeExpr {
        if self.endpoints.iter().any(|e| e.needs_location()) {
            ScopeExpr::All(vec![
                ScopeExpr::One(Scope::VehicleDeviceData),
                ScopeExpr::One(Scope::VehicleLocation),
            ])
        } else {
            ScopeExpr::One(Scope::VehicleDeviceData)
        }
    }
    fn pricing(&self) -> Pricing { Pricing::data() }
    fn path(&self) -> String {
        format!("/api/1/vehicles/{}/vehicle_data", self.encoded_segment)
    }
    fn query(&self) -> QuerySpec {
        let mut q = QuerySpec::new();
        if !self.endpoints.is_empty() {
            let joined = self.endpoints.iter().map(|e| e.as_wire())
                .collect::<Vec<_>>().join(";");
            q.push("endpoints", joined);
        }
        q
    }
}

impl VehicleEndpoint for GetVehicleData {
    fn vin(&self) -> &str { &self.vin }
}
```

`model.rs` 에 `VehicleData` 를 A1 의 U2 정책대로 추가한다:

```rust
/// `vehicle_data` 응답.
///
/// ⚠️ 하위 객체의 필드는 **실응답으로 검증하지 못했다** — 추측한 이름을 강타입으로
/// 내보내지 않는다는 상위 스펙 원칙에 따라 `serde_json::Value` 로 둔다.
/// 검증되면 그때 구조를 준다.
#[derive(serde::Deserialize)]
#[non_exhaustive]
pub struct VehicleData {
    #[serde(default)] pub id: Option<serde_json::Value>,
    #[serde(default)] pub vin: Option<String>,
    #[serde(default)] pub state: Option<String>,
    #[serde(default)] pub charge_state: Option<serde_json::Value>,
    #[serde(default)] pub climate_state: Option<serde_json::Value>,
    #[serde(default)] pub drive_state: Option<serde_json::Value>,
    #[serde(default)] pub vehicle_state: Option<serde_json::Value>,
    #[serde(flatten)] pub extra: serde_json::Map<String, serde_json::Value>,
}
```

- [ ] **Step 4: 통과 확인**

Run: `cd <workspace> && cargo test --locked`
Expected: 327 + 5 = **332 통과**

- [ ] **Step 5: mutation-test**

| # | mutation | 죽어야 하는 테스트 |
|---|---|---|
| M1 | `needs_location` 을 항상 `false` | `location_data_requires_the_location_scope` |
| M2 | `needs_location` 을 항상 `true` | `without_location_data_only_device_data_is_required` |
| M3 | `pricing()` 을 `NotCharged` 로 | `vehicle_data_is_a_paid_endpoint` |
| M4 | 빈 `endpoints` 에도 파라미터를 넣음 | `empty_endpoints_omits_the_parameter` |
| M5 | `location_data` 를 항상 쿼리에 추가 | `location_data_is_absent_from_the_query_unless_asked` |

- [ ] **Step 6: 게이트 + 커밋**

```bash
cd <workspace>
cargo fmt --all && cargo clippy --all-targets --all-features --locked -- -D warnings && cargo test --locked && cargo +1.89.0 test --locked --no-run && cargo doc --no-deps --locked
git add crates/fleetwire/src/api/vehicles.rs crates/fleetwire/src/model.rs
git commit -m "feat(a4): vehicle_data — 크레이트 첫 유료 엔드포인트 + 조건부 스코프

위치를 요청할 때만 vehicle_location 을 요구한다. A1 이 required_scopes 를 상수가 아니라
메서드로 둔 이유가 이것이다.

DataEndpoint 는 열거형이다 — 자유 문자열은 오타를 유료 요청으로 바꾼다.
non_exhaustive 라 Tesla 가 값을 추가해도 breaking 이 아니다.

하위 객체는 실응답으로 검증하지 못해 Value 로 둔다 — 추측한 필드명을 강타입 공개 API 로
만드는 것보다 정직하다."
```

---

### Task 5: `nearby_charging_sites` — 두 번째 유료 엔드포인트

**Files:** Modify `crates/fleetwire/src/api/vehicles.rs`, `crates/fleetwire/src/model.rs`

**Interfaces:**
- Produces: `pub struct GetNearbyChargingSites { /* private */ }` + `new(vin, count, radius, detail) -> Result<Self, VinError>`, `pub struct NearbyChargingSites`.

⚠️ **`page`/`per_page` 가 아니다.** 이 엔드포인트는 `count`·`radius`·`detail` 을 쓴다(O1). 페이지네이션 없음 → `passthrough_endpoint!`.

- [ ] **Step 1: 실패하는 테스트 3개를 쓴다**

```rust
    #[test]
    fn nearby_sites_is_paid_and_uses_count_radius_detail() {
        let r = GetNearbyChargingSites::new("5YJ3E1EA1JF000001", Some(5), Some(20), Some(true))
            .unwrap();
        assert!(matches!(r.pricing(), Pricing::Data { .. }));
        let q = r.query().to_encoded();
        assert!(q.contains("count=5") && q.contains("radius=20") && q.contains("detail=true"),
            "실제: {q}");
        assert!(!q.contains("page"), "이 엔드포인트는 page 를 쓰지 않는다: {q}");
    }

    #[test]
    fn nearby_sites_omits_absent_params() {
        let r = GetNearbyChargingSites::new("5YJ3E1EA1JF000001", None, None, None).unwrap();
        assert_eq!(r.query().to_encoded(), "");
    }

    #[test]
    fn nearby_sites_encodes_the_vin_segment() {
        assert!(GetNearbyChargingSites::new("5YJ/../admin", None, None, None).is_err());
    }
```

- [ ] **Step 2: 실패 확인** — Run: `cargo test --locked api::vehicles`; Expected: FAIL, 타입 없음

- [ ] **Step 3: 구현한다**

`GetVehicleData` 와 같은 형태로 만들되 `query()` 만 다르다:

```rust
    fn query(&self) -> QuerySpec {
        let mut q = QuerySpec::new();
        if let Some(c) = self.count { q.push("count", c); }
        if let Some(r) = self.radius { q.push("radius", r); }
        if let Some(d) = self.detail { q.push("detail", d); }
        q
    }
    fn pricing(&self) -> Pricing { Pricing::data() }
    fn required_scopes(&self) -> ScopeExpr { ScopeExpr::One(Scope::VehicleDeviceData) }
    fn path(&self) -> String {
        format!("/api/1/vehicles/{}/nearby_charging_sites", self.encoded_segment)
    }
```

`model.rs` 에:

```rust
/// `nearby_charging_sites` 응답.
///
/// ⚠️ 하위 목록의 항목 구조를 실응답으로 보지 못했다 — 추측한 필드명을 강타입 공개 API 로
/// 만들지 않는다. `extra` 가 전부를 보존하므로 데이터 손실은 없다.
#[derive(serde::Deserialize)]
#[non_exhaustive]
pub struct NearbyChargingSites {
    #[serde(default)] pub congestion_sync_time_utc_secs: Option<i64>,
    #[serde(default)] pub destination_charging: Option<Vec<serde_json::Value>>,
    #[serde(default)] pub superchargers: Option<Vec<serde_json::Value>>,
    #[serde(flatten)] pub extra: serde_json::Map<String, serde_json::Value>,
}
```

- [ ] **Step 4: 통과 확인** — Expected: **335 통과**

- [ ] **Step 5: mutation-test** — M1 `pricing()`→`NotCharged` / M2 없는 파라미터도 넣음 / M3 `encode_vin_segment` 우회

- [ ] **Step 6: 게이트 + 커밋**

```bash
git add crates/fleetwire/src/api/vehicles.rs crates/fleetwire/src/model.rs
git commit -m "feat(a4): nearby_charging_sites — 두 번째(마지막) 유료 엔드포인트

page/per_page 가 아니라 count/radius/detail 을 쓴다(O1). 페이지네이션 없음."
```

---

### Task 6: `invitations` — 두 번째 페이지네이션 소비자

**Files:** Modify `crates/fleetwire/src/api/vehicles.rs`, `crates/fleetwire/src/model.rs`

**Interfaces:** `pub struct ListInvitations { pub page: Option<u64>, pub per_page: Option<u64> }` + `new(vin) -> Result<…>`, `type Response = Page<Invitation>`, `pub struct Invitation`.

⚠️ `per_page` 최대 25(O1). **클라이언트에서 검증하지 않는다** — 서버가 판정할 일이고, 한도가 바뀌면 클라이언트가 먼저 틀린다. 대신 rustdoc 에 적는다.

- [ ] **Step 1: 실패하는 테스트 2개를 쓴다**

```rust
    #[test]
    fn invitations_is_paginated_and_free() {
        let r = ListInvitations::new("5YJ3E1EA1JF000001", Some(2), Some(25)).unwrap();
        assert!(matches!(r.pricing(), Pricing::NotCharged));
        let q = r.query().to_encoded();
        assert!(q.contains("page=2") && q.contains("per_page=25"), "실제: {q}");
    }

    /// ⚠️ 25 초과를 클라이언트가 막지 않는다 — 서버 판정에 맡긴다.
    #[test]
    fn invitations_does_not_clamp_per_page() {
        let r = ListInvitations::new("5YJ3E1EA1JF000001", None, Some(999)).unwrap();
        assert!(r.query().to_encoded().contains("per_page=999"));
    }
```

- [ ] **Step 2: 실패 확인** — Run: `cargo test --locked api::vehicles`; Expected: FAIL, `ListInvitations` 없음

- [ ] **Step 3: 구현한다**

```rust
/// `GET /api/1/vehicles/{vin}/invitations` — 무과금, 페이지네이션 있음.
///
/// ⚠️ `per_page` 의 서버측 상한은 25 로 알려져 있으나(O1) **클라이언트가 clamp 하지 않는다** —
/// 서버가 판정할 일이고, 한도가 바뀌면 클라이언트가 먼저 틀린다.
pub struct ListInvitations {
    vin: String,
    encoded_segment: String,
    pub page: Option<u64>,
    pub per_page: Option<u64>,
}

impl ListInvitations {
    pub fn new(vin: impl Into<String>, page: Option<u64>, per_page: Option<u64>)
        -> Result<Self, VinError>
    {
        let vin = vin.into();
        let encoded_segment = encode_vin_segment(&vin)?;
        Ok(Self { vin, encoded_segment, page, per_page })
    }
}

impl crate::sealed::Sealed for ListInvitations {}

impl FleetGet for ListInvitations {
    type WireResponse = Vec<Invitation>;
    type Response = Page<Invitation>;
    fn assemble(wire: Self::WireResponse, pagination: Option<Pagination>) -> Self::Response {
        Page { items: wire, pagination }
    }
    fn required_scopes(&self) -> ScopeExpr { ScopeExpr::One(Scope::VehicleDeviceData) }
    fn pricing(&self) -> Pricing { Pricing::NotCharged }
    fn path(&self) -> String {
        format!("/api/1/vehicles/{}/invitations", self.encoded_segment)
    }
    fn query(&self) -> QuerySpec {
        let mut q = QuerySpec::new();
        if let Some(p) = self.page { q.push("page", p); }
        if let Some(pp) = self.per_page { q.push("per_page", pp); }
        q
    }
}

impl VehicleEndpoint for ListInvitations {
    fn vin(&self) -> &str { &self.vin }
}
```

`model.rs` 에:

```rust
/// 차량 공유 초대. ⚠️ 실응답을 보지 못했다 — 필드를 추측해 강타입으로 만들지 않는다.
/// `extra` 가 전부를 보존하므로 데이터 손실은 없다. 구조가 확정되면 그때 필드를 준다.
#[derive(serde::Deserialize)]
#[non_exhaustive]
pub struct Invitation {
    #[serde(default)] pub id: Option<serde_json::Value>,
    #[serde(flatten)] pub extra: serde_json::Map<String, serde_json::Value>,
}
```

- [ ] **Step 4: 통과 확인** — Run: `cargo test --locked`; Expected: **337 통과**

- [ ] **Step 5: mutation-test** — M1 `assemble` 이 `pagination` 을 버림 / M2 `per_page` 를 25로 clamp

- [ ] **Step 6: 게이트 + 커밋**

```bash
git commit -m "feat(a4): invitations — 두 번째 페이지네이션 엔드포인트

per_page 최대 25 를 클라이언트에서 clamp 하지 않는다 — 서버가 판정할 일이고,
한도가 바뀌면 클라이언트가 먼저 틀린다. rustdoc 에만 적는다."
```

---

### Task 7: 단순 vehicle 엔드포인트 7개

**Files:** Modify `crates/fleetwire/src/api/vehicles.rs`, `crates/fleetwire/src/model.rs`

**Interfaces:** `GetMobileEnabled` · `GetRecentAlerts` · `GetReleaseNotes` · `GetServiceData` · `ListDrivers` · `GetFleetTelemetryConfig` · `GetFleetTelemetryErrors` — 전부 `new(vin) -> Result<Self, VinError>`, `Pricing::NotCharged`, `ScopeExpr::One(Scope::VehicleDeviceData)`, 파라미터 없음, `passthrough_endpoint!`.

| 타입 | 경로 |
|---|---|
| `GetMobileEnabled` | `/api/1/vehicles/{vin}/mobile_enabled` |
| `GetRecentAlerts` | `/api/1/vehicles/{vin}/recent_alerts` |
| `GetReleaseNotes` | `/api/1/vehicles/{vin}/release_notes` |
| `GetServiceData` | `/api/1/vehicles/{vin}/service_data` |
| `ListDrivers` | `/api/1/vehicles/{vin}/drivers` |
| `GetFleetTelemetryConfig` | `/api/1/vehicles/{vin}/fleet_telemetry_config` |
| `GetFleetTelemetryErrors` | `/api/1/vehicles/{vin}/fleet_telemetry_errors` |

⚠️ 응답 타입은 전부 **`serde_json::Value`** 로 둔다 — 실응답을 보지 못했다. rustdoc 에 "검증되지 않음, 구조가 확정되면 타입을 준다"고 적는다. 추측한 필드명을 공개 API 로 만드는 것보다 정직하다.

- [ ] **Step 1: 실패하는 테스트를 쓴다** — 7개를 한 테이블 테스트로:

```rust
    /// 7개가 전부 무과금·device_data·파라미터 없음이고, 경로가 정확하다.
    #[test]
    fn simple_vehicle_endpoints_contract() {
        let vin = "5YJ3E1EA1JF000001";
        let cases: Vec<(String, ScopeExpr, Pricing, String)> = vec![
            { let r = GetMobileEnabled::new(vin).unwrap();
              (r.path(), r.required_scopes(), r.pricing(), r.query().to_encoded()) },
            { let r = GetRecentAlerts::new(vin).unwrap();
              (r.path(), r.required_scopes(), r.pricing(), r.query().to_encoded()) },
            { let r = GetReleaseNotes::new(vin).unwrap();
              (r.path(), r.required_scopes(), r.pricing(), r.query().to_encoded()) },
            { let r = GetServiceData::new(vin).unwrap();
              (r.path(), r.required_scopes(), r.pricing(), r.query().to_encoded()) },
            { let r = ListDrivers::new(vin).unwrap();
              (r.path(), r.required_scopes(), r.pricing(), r.query().to_encoded()) },
            { let r = GetFleetTelemetryConfig::new(vin).unwrap();
              (r.path(), r.required_scopes(), r.pricing(), r.query().to_encoded()) },
            { let r = GetFleetTelemetryErrors::new(vin).unwrap();
              (r.path(), r.required_scopes(), r.pricing(), r.query().to_encoded()) },
        ];
        let expected_paths = [
            "mobile_enabled", "recent_alerts", "release_notes", "service_data",
            "drivers", "fleet_telemetry_config", "fleet_telemetry_errors",
        ];
        assert_eq!(cases.len(), expected_paths.len());
        for ((path, scopes, pricing, query), suffix) in cases.iter().zip(expected_paths) {
            assert!(path.ends_with(suffix), "경로 불일치: {path} vs {suffix}");
            assert!(path.starts_with("/api/1/vehicles/"), "{path}");
            assert!(matches!(pricing, Pricing::NotCharged), "{suffix} 는 무과금이어야 한다");
            assert!(scopes.is_satisfied_by(&[Scope::VehicleDeviceData]), "{suffix}");
            assert_eq!(query, "", "{suffix} 는 파라미터가 없다");
        }
    }

    /// VIN 검증이 7개 전부에 걸린다.
    #[test]
    fn simple_vehicle_endpoints_reject_an_unsafe_vin() {
        assert!(GetMobileEnabled::new("5YJ/../admin").is_err());
        assert!(GetRecentAlerts::new("5YJ/../admin").is_err());
        assert!(GetReleaseNotes::new("5YJ/../admin").is_err());
        assert!(GetServiceData::new("5YJ/../admin").is_err());
        assert!(ListDrivers::new("5YJ/../admin").is_err());
        assert!(GetFleetTelemetryConfig::new("5YJ/../admin").is_err());
        assert!(GetFleetTelemetryErrors::new("5YJ/../admin").is_err());
    }
```

- [ ] **Step 2: 실패 확인** — Run: `cargo test --locked api::vehicles`; Expected: FAIL, 타입 7개 없음

- [ ] **Step 3: 구현한다**

7개가 경로만 다르고 나머지가 동일하므로 **선언 매크로 하나**로 만든다. 반복 코드를 7번 복사하면 한 곳만 고쳐지는 사고가 난다.

```rust
/// VIN 하나만 받는 무과금·무파라미터 vehicle 엔드포인트를 정의한다.
macro_rules! simple_vehicle_endpoint {
    ($name:ident, $segment:literal, $doc:literal) => {
        #[doc = $doc]
        ///
        /// ⚠️ 응답은 `serde_json::Value` 다 — 실응답을 보지 못했고, 추측한 필드명을
        /// 강타입 공개 API 로 만들지 않는다. 구조가 확정되면 그때 타입을 준다.
        pub struct $name {
            vin: String,
            encoded_segment: String,
        }

        impl $name {
            pub fn new(vin: impl Into<String>) -> Result<Self, VinError> {
                let vin = vin.into();
                let encoded_segment = encode_vin_segment(&vin)?;
                Ok(Self { vin, encoded_segment })
            }
        }

        impl crate::sealed::Sealed for $name {}

        impl FleetGet for $name {
            passthrough_endpoint!($name, serde_json::Value);
            fn required_scopes(&self) -> ScopeExpr {
                ScopeExpr::One(Scope::VehicleDeviceData)
            }
            fn pricing(&self) -> Pricing { Pricing::NotCharged }
            fn path(&self) -> String {
                format!(concat!("/api/1/vehicles/{}/", $segment), self.encoded_segment)
            }
            fn query(&self) -> QuerySpec { QuerySpec::new() }
        }

        impl VehicleEndpoint for $name {
            fn vin(&self) -> &str { &self.vin }
        }
    };
}

simple_vehicle_endpoint!(GetMobileEnabled, "mobile_enabled",
    "`GET /api/1/vehicles/{vin}/mobile_enabled` — 모바일 접근 허용 여부.");
simple_vehicle_endpoint!(GetRecentAlerts, "recent_alerts",
    "`GET /api/1/vehicles/{vin}/recent_alerts` — 최근 차량 경고.");
simple_vehicle_endpoint!(GetReleaseNotes, "release_notes",
    "`GET /api/1/vehicles/{vin}/release_notes` — 펌웨어 릴리스 노트.");
simple_vehicle_endpoint!(GetServiceData, "service_data",
    "`GET /api/1/vehicles/{vin}/service_data` — 서비스 상태.");
simple_vehicle_endpoint!(ListDrivers, "drivers",
    "`GET /api/1/vehicles/{vin}/drivers` — 등록된 운전자.");
simple_vehicle_endpoint!(GetFleetTelemetryConfig, "fleet_telemetry_config",
    "`GET /api/1/vehicles/{vin}/fleet_telemetry_config` — 텔레메트리 설정.");
simple_vehicle_endpoint!(GetFleetTelemetryErrors, "fleet_telemetry_errors",
    "`GET /api/1/vehicles/{vin}/fleet_telemetry_errors` — 텔레메트리 오류.");
```

- [ ] **Step 4: 통과 확인** — Run: `cargo test --locked`; Expected: **339 통과**

- [ ] **Step 5: mutation-test** — M1 하나의 `pricing()` 을 `data()` 로 / M2 하나의 경로 오타 / M3 하나가 `encode_vin_segment` 우회

⚠️ 매크로로 만들었으므로 **한 타입만 개별 수정하려면 매크로 밖으로 꺼내야 한다.** mutation 은 매크로 본문을 고치는 것으로 대신하되, 그러면 7개가 동시에 바뀌어 "테이블 테스트가 7개를 다 보는가"를 검증하지 못한다 — **한 타입을 매크로에서 빼내 손으로 쓴 뒤 그것만 변이**시킬 것.

⚠️ M1·M2·M3 을 **각기 다른 타입에** 주입할 것. 한 타입에만 주입하면 테이블 테스트가 나머지 6개를 커버하는지 알 수 없다.

- [ ] **Step 6: 게이트 + 커밋**

```bash
git commit -m "feat(a4): 단순 vehicle 엔드포인트 7개

전부 무과금·device_data·파라미터 없음. 응답은 Value 로 둔다 — 실응답을 보지 못했고,
추측한 필드명을 강타입 공개 API 로 만드는 것보다 정직하다."
```

---

### Task 8: user 엔드포인트 3개

**Files:** Modify `crates/fleetwire/src/api/users.rs`, `crates/fleetwire/src/model.rs`

**Interfaces:** `GetMe` · `ListOrders` · `GetFeatureConfig` — 단위 구조체(VIN 없음), `Pricing::NotCharged`, 파라미터 없음.

| 타입 | 경로 |
|---|---|
| `GetMe` | `/api/1/users/me` |
| `ListOrders` | `/api/1/users/orders` |
| `GetFeatureConfig` | `/api/1/users/feature_config` |

⚠️ **스코프가 `user_data` 류다 — `openid` 가 아니다**(O1 정정 4번). A3 의 `GetRegion` 이 `Any([Openid, UserData, VehicleDeviceData])` 인 것과 다르다. 이 3개는 `ScopeExpr::One(Scope::UserData)` 로 둔다.

⚠️ 이 셋은 **user-context 토큰**이 필요하다(3rd-party business 토큰 비호환, O1 공통 주석). rustdoc 에 적는다.

- [ ] **Step 1: 실패하는 테스트 2개를 쓴다**

```rust
    #[test]
    fn user_endpoints_require_user_data_not_openid() {
        for (path, scopes, pricing) in [
            { let r = GetMe; (r.path(), r.required_scopes(), r.pricing()) },
            { let r = ListOrders; (r.path(), r.required_scopes(), r.pricing()) },
            { let r = GetFeatureConfig; (r.path(), r.required_scopes(), r.pricing()) },
        ] {
            assert!(matches!(pricing, Pricing::NotCharged), "{path}");
            assert!(scopes.is_satisfied_by(&[Scope::UserData]), "{path}");
            assert!(!scopes.is_satisfied_by(&[Scope::Openid]),
                "{path}: openid 만으로 만족되면 안 된다(O1 정정 4번)");
        }
    }

    #[test]
    fn user_endpoint_paths_are_exact() {
        assert_eq!(GetMe.path(), "/api/1/users/me");
        assert_eq!(ListOrders.path(), "/api/1/users/orders");
        assert_eq!(GetFeatureConfig.path(), "/api/1/users/feature_config");
    }
```

- [ ] **Step 2: 실패 확인** — Run: `cargo test --locked api::users`; Expected: FAIL, 타입 3개 없음

- [ ] **Step 3: 구현한다**

VIN 이 없어 단위 구조체이고, T7 의 매크로는 VIN 을 전제하므로 **재사용하지 않는다.** 3개뿐이라 별도 매크로도 두지 않는다.

```rust
/// `GET /api/1/users/me` — 현재 사용자 프로필.
///
/// ⚠️ **user-context 토큰이 필요하다** — 3rd-party business 토큰과 호환되지 않는다(O1).
/// ⚠️ 응답은 `serde_json::Value` 다. 실응답을 보지 못했다.
pub struct GetMe;

impl crate::sealed::Sealed for GetMe {}

impl FleetGet for GetMe {
    passthrough_endpoint!(GetMe, serde_json::Value);
    /// ⚠️ `user_data` 다 — `openid` 가 아니다(O1 정정 4번).
    fn required_scopes(&self) -> ScopeExpr { ScopeExpr::One(Scope::UserData) }
    fn pricing(&self) -> Pricing { Pricing::NotCharged }
    fn path(&self) -> String { "/api/1/users/me".to_owned() }
    fn query(&self) -> QuerySpec { QuerySpec::new() }
}

/// `GET /api/1/users/orders` — 주문 내역.
/// ⚠️ user-context 토큰 필요. 응답은 `serde_json::Value`.
pub struct ListOrders;

impl crate::sealed::Sealed for ListOrders {}

impl FleetGet for ListOrders {
    passthrough_endpoint!(ListOrders, serde_json::Value);
    fn required_scopes(&self) -> ScopeExpr { ScopeExpr::One(Scope::UserData) }
    fn pricing(&self) -> Pricing { Pricing::NotCharged }
    fn path(&self) -> String { "/api/1/users/orders".to_owned() }
    fn query(&self) -> QuerySpec { QuerySpec::new() }
}

/// `GET /api/1/users/feature_config` — 계정 기능 플래그.
/// ⚠️ user-context 토큰 필요. 응답은 `serde_json::Value`.
pub struct GetFeatureConfig;

impl crate::sealed::Sealed for GetFeatureConfig {}

impl FleetGet for GetFeatureConfig {
    passthrough_endpoint!(GetFeatureConfig, serde_json::Value);
    fn required_scopes(&self) -> ScopeExpr { ScopeExpr::One(Scope::UserData) }
    fn pricing(&self) -> Pricing { Pricing::NotCharged }
    fn path(&self) -> String { "/api/1/users/feature_config".to_owned() }
    fn query(&self) -> QuerySpec { QuerySpec::new() }
}
```

⚠️ `ListOrders` 는 이름이 목록이지만 **`Page` 를 쓰지 않는다** — O1 에 페이지네이션이 없다고 돼 있다. 응답이 실제로 `pagination` 을 주면 그때 `Page` 로 바꾼다(그때는 breaking 이므로 publish 전에 확인할 것).

- [ ] **Step 4: 통과 확인** — Run: `cargo test --locked`; Expected: **341 통과**

- [ ] **Step 5: mutation-test** — M1 `UserData`→`Openid` / M2 경로 오타 / M3 `pricing()`→`data()`

- [ ] **Step 6: 게이트 + 커밋**

```bash
git commit -m "feat(a4): user 엔드포인트 3개 (me·orders·feature_config)

스코프는 user_data 다 — openid 가 아니다(O1 정정 4번). GetRegion 이 Any 인 것과 다르다.
user-context 토큰 필수라는 사실을 rustdoc 에 적는다."
```

---

### Task 9: `send_guarded` — 비용 게이트 (fail-closed)

**Files:** Modify `crates/fleetwire/src/client.rs`
**Test:** 같은 파일

**Interfaces:**
```rust
#[non_exhaustive]
pub enum Guarded<T> { Executed(T), Skipped { state: String } }
impl Client {
    pub async fn send_guarded<R: VehicleEndpoint>(&self, req: &R)
        -> Result<Guarded<R::Response>, Error>;
}
```

⚠️ **fail-closed**: 게이트 호출이 실패하면 유료 호출을 **강행하지 않고** 그 에러를 올린다. 게이트의 존재 이유가 "모르면 돈 쓰지 말자"인데 모르게 된 상황에서 강행하면 게이트가 없는 것보다 나쁘다(실패한 게이트 요청 + 유료 요청).

⚠️ `state` 를 열거형으로 좁히지 않는다. 판정은 `state == "online"` 하나만 본다. 새 상태가 생기면 열거형은 파싱 실패하거나 `Unknown` 으로 접히는데, 게이트에 필요한 건 "온라인인가" 뿐이다.

- [ ] **Step 1: 실패하는 테스트 4개를 쓴다**

```rust
    /// ⚠️ offline 이면 유료 요청이 **0회** 나간다. 게이트의 존재 이유다.
    #[tokio::test]
    async fn guard_skips_the_paid_call_when_offline() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/1/vehicles/5YJ3E1EA1JF000001"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                crate::test_support::FIXTURE_VEHICLE_STATE_OFFLINE.as_bytes().to_vec(),
                "application/json"))
            .mount(&server).await;

        let c = test_client(&server).await;
        let req = GetVehicleData::new("5YJ3E1EA1JF000001", vec![DataEndpoint::ChargeState]).unwrap();
        let got = c.send_guarded(&req).await.unwrap();

        assert!(matches!(got, Guarded::Skipped { ref state } if state == "offline"), "{got:?}");
        let paid = server.received_requests().await.unwrap().iter()
            .filter(|r| r.url.path().ends_with("/vehicle_data")).count();
        assert_eq!(paid, 0, "게이트가 막았는데 유료 요청이 나갔다");
    }

    /// online 이면 실제로 호출한다.
    #[tokio::test]
    async fn guard_executes_when_online() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/1/vehicles/5YJ3E1EA1JF000001"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                br#"{"response":{"state":"online"}}"#.to_vec(), "application/json"))
            .mount(&server).await;
        Mock::given(method("GET"))
            .and(path("/api/1/vehicles/5YJ3E1EA1JF000001/vehicle_data"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                br#"{"response":{"vin":"5YJ3E1EA1JF000001"}}"#.to_vec(), "application/json"))
            .mount(&server).await;

        let c = test_client(&server).await;
        let req = GetVehicleData::new("5YJ3E1EA1JF000001", vec![DataEndpoint::ChargeState]).unwrap();
        assert!(matches!(c.send_guarded(&req).await.unwrap(), Guarded::Executed(_)));
    }

    /// ⚠️ 게이트가 실패하면 fail-closed — 유료 호출을 강행하지 않는다.
    #[tokio::test]
    async fn guard_failure_does_not_force_the_paid_call() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/1/vehicles/5YJ3E1EA1JF000001"))
            .respond_with(ResponseTemplate::new(503).set_body_raw(b"{}".to_vec(), "application/json"))
            .mount(&server).await;

        let c = test_client(&server).await;
        let req = GetVehicleData::new("5YJ3E1EA1JF000001", vec![DataEndpoint::ChargeState]).unwrap();
        assert!(c.send_guarded(&req).await.is_err());

        let paid = server.received_requests().await.unwrap().iter()
            .filter(|r| r.url.path().ends_with("/vehicle_data")).count();
        assert_eq!(paid, 0, "게이트가 실패했는데 유료 요청이 나갔다 — fail-open 이다");
    }

    /// ⚠️ backseat_token 이 게이트 응답에 있는데 에러·로그로 새면 안 된다.
    #[tokio::test]
    async fn guard_does_not_leak_backseat_token() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/1/vehicles/5YJ3E1EA1JF000001"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                crate::test_support::FIXTURE_VEHICLE_STATE_OFFLINE.as_bytes().to_vec(),
                "application/json"))
            .mount(&server).await;

        let c = test_client(&server).await;
        let req = GetVehicleData::new("5YJ3E1EA1JF000001", vec![DataEndpoint::ChargeState]).unwrap();
        let got = c.send_guarded(&req).await.unwrap();
        assert!(!format!("{got:?}").contains("SECRET-BACKSEAT"), "토큰이 샜다: {got:?}");
    }
```

`Guarded<T>` 에 `Debug` 가 필요하다 — ⚠️ **`T: Debug` 를 요구하지 말고 `Skipped` 만 찍는 수동 `Debug`** 를 쓴다. `Executed` 의 내용을 찍으면 응답 본문이 통째로 로그에 갈 수 있다.

- [ ] **Step 2: 실패 확인** — Run: `cargo test --locked client::`; Expected: FAIL, `send_guarded` 없음

- [ ] **Step 3: 구현한다**

```rust
/// [`Client::send_guarded`] 의 결과.
#[non_exhaustive]
pub enum Guarded<T> {
    /// 차량이 online 이라 실제로 호출했다.
    Executed(T),
    /// 차량이 online 이 아니라 **유료 호출을 하지 않았다**. 관측된 상태 문자열을 그대로 준다.
    Skipped { state: String },
}

/// ⚠️ `T: Debug` 를 요구하지 **않는다.** `Executed` 의 내용을 찍으면 응답 본문이 통째로
/// 로그로 갈 수 있다 — 게이트 응답에 `backseat_token` 이 실려 온다는 것을 실측했다.
impl<T> core::fmt::Debug for Guarded<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Executed(_) => f.write_str("Guarded::Executed(<response>)"),
            Self::Skipped { state } => f
                .debug_struct("Guarded::Skipped")
                .field("state", state)
                .finish(),
        }
    }
}

impl Client {
    /// 유료 호출 전에 차량 상태를 **무과금으로** 확인하고, online 이 아니면 건너뛴다.
    ///
    /// 실측(2026-08-03): 상태 조회 `GET /api/1/vehicles/{vin}` 에는 `ratelimit-*` 헤더가
    /// 없다(= device_data 쿼터를 쓰지 않는다). 반면 수면 차량에 `vehicle_data` 를 치면
    /// 408 이 나면서 요금과 쿼터 슬롯을 **둘 다** 태운다.
    ///
    /// ⚠️ **비용을 줄이지 제거하지 않는다.** 상태 확인과 실호출 사이에 차가 잠들 수 있고
    /// (TOCTOU), 그러면 408 이 나며 그 408 은 과금된다.
    ///
    /// ⚠️ **항상 이득인 것은 아니다.** 차가 online 이면 게이트는 순수한 추가 왕복이다.
    /// 절감은 차가 자주 잘 때만 생긴다 — 어느 쪽인지는 호출 패턴이 정하며, 그래서
    /// [`Client::send`] 와 별도 메서드로 두었다.
    ///
    /// ⚠️ **게이트 자체가 실패하면 유료 호출을 강행하지 않는다(fail-closed).** 게이트의
    /// 존재 이유가 "모르면 돈 쓰지 말자"인데, 모르게 된 상황에서 강행하면 게이트가 없느니만
    /// 못하다(실패한 게이트 요청 + 유료 요청). 강행이 필요하면 [`Client::send`] 를 직접 부른다.
    pub async fn send_guarded<R: VehicleEndpoint>(
        &self,
        req: &R,
    ) -> Result<Guarded<R::Response>, Error> {
        let probe = GetVehicle::new(req.vin())?;
        // fail-closed — `?` 로 그대로 올린다.
        let summary: VehicleSummary = self.send(&probe).await?;
        let state = summary.state.unwrap_or_default();
        if state != "online" {
            return Ok(Guarded::Skipped { state });
        }
        Ok(Guarded::Executed(self.send(req).await?))
    }
}
```

⚠️ `state` 를 열거형으로 좁히지 않는다. 판정은 `state == "online"` 하나만 본다. 새 상태가 생기면
열거형은 파싱 실패하거나 `Unknown` 으로 접히는데, 게이트에 필요한 건 "온라인인가" 뿐이다.
`"online"` 표기가 바뀌면 게이트가 전부 `Skipped` 로 접히는데 이는 **비용이 0이 되는 방향**이라
안전하고, `Skipped` 에 실린 문자열이 진단을 준다. 이 판단을 rustdoc 에 적는다.

- [ ] **Step 4: 통과 확인** — Run: `cargo test --locked`; Expected: **345 통과**

- [ ] **Step 5: mutation-test**

| # | mutation | 죽어야 하는 테스트 |
|---|---|---|
| M1 | `state != "online"` 에도 실행 | `guard_skips_the_paid_call_when_offline` |
| M2 | 게이트 실패 시 유료 호출 강행(fail-open) | `guard_failure_does_not_force_the_paid_call` |
| M3 | `Guarded` 에 `#[derive(Debug)]` (T 를 찍음) | `guard_does_not_leak_backseat_token` |

- [ ] **Step 6: 게이트 + 커밋**

```bash
git commit -m "feat(a4): send_guarded — 비용 게이트 (fail-closed)

게이트 엔드포인트가 무과금임을 실측으로 확인했고(ratelimit 헤더 없음), 수면 차량에
vehicle_data 를 치면 408 이 나면서 \$0.002 + 쿼터 슬롯 1개를 태운다는 것도 실측했다.

게이트 호출이 실패하면 유료 호출을 강행하지 않는다 — 존재 이유가 '모르면 돈 쓰지 말자'인데
모르게 된 상황에서 강행하면 게이트가 없느니만 못하다.

⚠️ 게이트가 항상 이득은 아니다. 차가 online 이면 순수한 추가 왕복이다."
```

---

### Task 10: `RateLimit` + `Metered<T>` — 응답별 쿼터 관측

**Files:** Modify `crates/fleetwire/src/client.rs`, `crates/fleetwire/src/transport.rs`
**Test:** 같은 파일들

**Interfaces:**
```rust
#[non_exhaustive]
pub struct RateLimit { pub limit: u64, pub remaining: u64, pub reset_raw: u64 }
#[non_exhaustive]
pub struct Metered<T> { pub value: T, pub rate_limit: Option<RateLimit> }
impl Client { pub async fn send_metered<R: FleetGet>(&self, req: &R) -> Result<Metered<R::Response>, Error>; }
```

⚠️ **`Client::last_rate_limit()` 같은 전역 슬롯을 두지 않는다.** 쿼터는 **차량당**인데 전역 슬롯 하나에 담으면 여러 VIN·동시 호출·`Client` 클론에서 서로 덮어쓰고 오귀속한다. 값을 만든 그 응답에 붙어 있어야 어느 호출의 것인지 자명하다.

⚠️ **자동 스로틀링을 하지 않는다.** 60/분은 계정 공유라 다른 앱이 쓴 분량을 모르고, 진행 중인 대기는 호출자의 지연 예측을 깨뜨린다.

⚠️ `reset_raw` 다 — 단위가 초라는 것은 **추정**이다(실측은 감소만 봤고 경계 동작을 안 봤다).

- [ ] **Step 1: 실패하는 테스트 3개를 쓴다**

```rust
    /// 유료 엔드포인트의 rate-limit 헤더가 응답에 붙어서 온다.
    #[tokio::test]
    async fn metered_surfaces_the_rate_limit_headers() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/1/vehicles/5YJ3E1EA1JF000001/vehicle_data"))
            .respond_with(ResponseTemplate::new(200)
                .insert_header("ratelimit-device-data-per-vehicle-limit", "60")
                .insert_header("ratelimit-device-data-per-vehicle-remaining", "59")
                .insert_header("ratelimit-device-data-per-vehicle-reset", "57")
                .set_body_raw(br#"{"response":{"vin":"5YJ3E1EA1JF000001"}}"#.to_vec(),
                    "application/json"))
            .mount(&server).await;

        let c = test_client(&server).await;
        let req = GetVehicleData::new("5YJ3E1EA1JF000001", vec![DataEndpoint::ChargeState]).unwrap();
        let m = c.send_metered(&req).await.unwrap();
        let rl = m.rate_limit.expect("유료 엔드포인트는 헤더를 준다");
        assert_eq!((rl.limit, rl.remaining, rl.reset_raw), (60, 59, 57));
    }

    /// ⚠️ 무과금 엔드포인트는 헤더가 없다 → None (실측 E4).
    #[tokio::test]
    async fn metered_is_none_for_an_unmetered_endpoint() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/1/users/region"))
            .respond_with(ResponseTemplate::new(200).set_body_raw(
                br#"{"response":{"region":"na"}}"#.to_vec(), "application/json"))
            .mount(&server).await;

        let c = test_client(&server).await;
        let m = c.send_metered(&GetRegion).await.unwrap();
        assert!(m.rate_limit.is_none(), "무과금 응답에 rate limit 이 생겼다");
    }

    /// 헤더가 일부만 있거나 숫자가 아니면 None 이다 — 부분값을 지어내지 않는다.
    #[tokio::test]
    async fn partial_or_unparsable_headers_yield_none() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/1/users/region"))
            .respond_with(ResponseTemplate::new(200)
                .insert_header("ratelimit-device-data-per-vehicle-limit", "sixty")
                .insert_header("ratelimit-device-data-per-vehicle-remaining", "59")
                .set_body_raw(br#"{"response":{"region":"na"}}"#.to_vec(), "application/json"))
            .mount(&server).await;

        let c = test_client(&server).await;
        assert!(c.send_metered(&GetRegion).await.unwrap().rate_limit.is_none());
    }
```

- [ ] **Step 2: 실패 확인** — Run: `cargo test --locked client::`; Expected: FAIL, `send_metered` 없음

- [ ] **Step 3: 구현한다**

`transport.rs` — `RawResponse` 에 세 값을 싣는다:

```rust
pub(crate) struct RawResponse {
    // … 기존 필드
    /// `ratelimit-device-data-per-vehicle-{limit,remaining,reset}` 원시값.
    /// ⚠️ **셋이 모두 있고 모두 파싱될 때만** `Some` 이다 — 부분값을 지어내지 않는다.
    pub rate_limit: Option<(u64, u64, u64)>,
}
```

`execute_get` 의 헤더 추출부에:

```rust
    let num = |k: &str| header(k).and_then(|v| v.trim().parse::<u64>().ok());
    let rate_limit = match (
        num("ratelimit-device-data-per-vehicle-limit"),
        num("ratelimit-device-data-per-vehicle-remaining"),
        num("ratelimit-device-data-per-vehicle-reset"),
    ) {
        (Some(l), Some(r), Some(rs)) => Some((l, r, rs)),
        // ⚠️ 하나라도 없거나 숫자가 아니면 전체를 버린다. 반쪽 값은 오해를 부른다.
        _ => None,
    };
```

`client.rs`:

```rust
/// Tesla 가 응답 헤더로 알려주는 요청 쿼터 잔량.
///
/// 실측(2026-08-03): device_data 는 **차량당** `limit = 60` 이고, `remaining` 이 호출마다
/// 1씩 줄며, **408 도 슬롯을 소모한다**. 무과금 엔드포인트는 이 헤더를 보내지 않는다.
///
/// ⚠️ **관측 전용이다. 이 크레이트는 자동 스로틀링을 하지 않는다.** 이 쿼터는 계정 단위로
/// 공유되므로 다른 앱이 쓴 분량을 우리는 알 수 없고, 우리 잔량만 보고 대기하면 헛기다린다.
/// 진행 중인 대기는 호출자의 지연 예측도 깨뜨린다.
#[non_exhaustive]
pub struct RateLimit {
    pub limit: u64,
    pub remaining: u64,
    /// ⚠️ 헤더의 **원시값**이다. 단위가 초라는 것은 추정이다 — 실측은 값이 감소하는 것만
    /// 봤고 0 에서의 경계 동작을 보지 않았다.
    pub reset_raw: u64,
}

/// 응답과 그 응답이 보고한 쿼터 잔량.
///
/// ⚠️ 쿼터는 **차량당**이므로 값은 그것을 만든 응답에 붙어 있어야 한다. `Client` 에 전역
/// 슬롯 하나로 두면 여러 VIN·동시 호출·클론에서 서로 덮어쓰고 오귀속한다.
#[non_exhaustive]
pub struct Metered<T> {
    pub value: T,
    /// 헤더가 없으면 `None`(무과금 엔드포인트).
    pub rate_limit: Option<RateLimit>,
}

impl Client {
    /// [`Client::send`] 와 같되 응답이 보고한 쿼터 잔량을 함께 준다.
    ///
    /// ⚠️ **성공 경로만 덮는다.** 에러 응답도 이 헤더를 보내지만(실측한 408 이 그랬다)
    /// `Error` 에 싣는 것은 별개 문제라 이 API 로는 오지 않는다.
    pub async fn send_metered<R: FleetGet>(&self, req: &R) -> Result<Metered<R::Response>, Error> {
        /* send 와 같은 경로를 쓰되 RawResponse.rate_limit 을 함께 반환하도록 내부 함수를
           공유한다. 로직을 복제하지 말 것 — 한쪽만 고쳐지는 사고가 난다. */
        todo!("send 의 본문을 내부 함수로 추출해 send/send_metered 가 공유")
    }
}
```

⚠️ **`send` 의 로직을 복제하지 말 것.** 본문을 `send_inner(req) -> Result<(R::Response, Option<RateLimit>), Error>` 로 추출하고 `send` 는 첫 값만, `send_metered` 는 둘 다 쓴다. A3 T5 가 `access_token`/`access_token_with_generation` 에서 쓴 것과 같은 패턴이다.

⚠️ 위 `todo!()` 는 **계획의 지시이지 커밋할 코드가 아니다.** Step 4 를 통과하려면 실제 본문이 들어가야 한다.

- [ ] **Step 4: 통과 확인** — Run: `cargo test --locked`; Expected: **348 통과**

- [ ] **Step 5: mutation-test** — M1 셋 중 하나만 있어도 `Some` / M2 파싱 실패 시 0 으로 대체 / M3 전역 슬롯으로 되돌림

- [ ] **Step 6: 게이트 + 커밋**

```bash
git commit -m "feat(a4): 응답별 rate-limit 관측 (Metered<T>)

실측에서 API 가 잔량을 헤더로 준다는 걸 발견했다(차량당 60, 408 도 슬롯 소모).
클라이언트가 이를 완전히 무시하고 있었다.

전역 last_rate_limit() 을 두지 않는다 — 쿼터는 차량당인데 전역 슬롯 하나에 담으면
여러 VIN·동시 호출에서 서로 덮어쓰고 오귀속한다.

자동 스로틀링은 하지 않는다. 60/분은 계정 공유라 다른 앱 분량을 모르고,
진행 중인 대기는 호출자의 지연 예측을 깨뜨린다."
```

---

### Task 11: 문서 마감 + O 항목 기록

**Files:** Modify `crates/fleetwire/src/lib.rs`, `README.md`, `crates/fleetwire/src/model.rs`

- [ ] **Step 1: 크레이트 문서를 갱신한다**

`lib.rs` 크레이트 문서에 추가/수정:

- **엔드포인트 16개 전부 지원**으로 갱신. 유료 2개(`vehicle_data`·`nearby_charging_sites`)를 **명시**하고 나머지 14개가 무과금임을 적는다.
- **비용 다루는 세 수단**: 타입(`Pricing`) · 게이트(`send_guarded`) · 관측(`Metered`). 각각이 무엇을 보장하고 무엇을 보장하지 않는지.
- **검증 수준**: 실측한 것(401 3형태 · 412 리전거절 · 리전 해석 · OIDC discovery 부적합 · **408** · **pagination 형제 키** · **rate-limit 헤더** · **게이트 무과금 정황**) vs 문서 근거뿐인 것(**5xx** · 유료 2개 외 11개의 과금 카테고리).
- read-only 강제: 본진은 타입·가시성, clippy 는 **보조**. 이 문구를 약화시키지 말 것.

- [ ] **Step 2: 미결 항목을 기록한다**

`lib.rs` 에 O8·O9·O10(5xx 잔여)·O11·O12·O13·O14·O15·O16·O17·O18 을 한 절로 모은다. 각 한 줄 + 해소 조건.

- [ ] **Step 3: `Page` 의 stale doc 을 고친다**

`model.rs` 의 "호출자가 두 조각으로 조립한다"를 실제 동작으로 바꾼다 — `assemble` 이 조립하고, `pagination` 이 `None` 이면 서버가 주지 않았다는 뜻이라고 적는다.

- [ ] **Step 4: 게이트 전체 + `cargo doc`**

```bash
cd <workspace>
cargo fmt --all -- --check && cargo clippy --all-targets --all-features --locked -- -D warnings && cargo test --locked && cargo +1.89.0 test --locked --no-run && cargo doc --no-deps --locked
```

- [ ] **Step 5: 불변식 최종 확인**

```bash
grep -rn '\.post(\|\.put(\|\.delete(\|\.patch(\|Method::POST' crates/ | grep -v '^Binary'
```
Expected: `auth/refresh.rs` 한 곳 + `lib.rs` 의 문서 언급뿐.

- [ ] **Step 6: 커밋**

```bash
git add -A
git commit -m "docs(a4): 크레이트 문서 마감 — 16 엔드포인트·비용 3수단·검증 수준

실측한 것과 문서 근거뿐인 것을 명확히 가른다. 5xx 는 여전히 미측정이고,
유료 2개 외 11개의 무과금은 O1 매트릭스 전제이지 실측이 아니다.

read-only 강제의 본진은 타입·가시성이고 clippy 는 보조다 — 이 문구를 약화시키지 않는다."
```

---

## 완료 조건

- [ ] 316 → **최소 348** 테스트, 전부 green
- [ ] 5종 게이트 전부 통과(⚠️ MSRV 는 `test --no-run`)
- [ ] `grep` 결과 POST 가 `auth/refresh.rs` 한 곳뿐
- [ ] 16개 엔드포인트 전부 `FleetGet` 구현, 유료 2개만 `Pricing::data()`
- [ ] `Page<T>` 가 실제로 채워지고 `pagination` 부재가 `None` 으로 구분됨
- [ ] `send_guarded` 가 offline 에서 유료 요청 0회
- [ ] `backseat_token`·계정 UUID 가 새지 않고, `txid` 는 **보존**됨

## A5 로 넘기는 것

- **O10 잔여**: 5xx 실응답 증거
- **O17**: 유료 2개 외 11개의 과금 카테고리 대시보드 대조
- **O14**: 에러 경로의 rate-limit 노출(`Metered` 는 성공 경로만)
- **O15**: 408 을 status 만으로 분류하는 문제(게이트웨이발 408)
- **O12·O13·O16·O18**: 페이지네이션 기본값·`count` 중복·쿼터 공유범위·창 종류
- 응답 타입이 `serde_json::Value` 인 엔드포인트들의 구조 확정(실응답 확보 후)
- A3 이월: `kv_secret` 멱등 가드 우회 · `acquire` 의 symlink 방어

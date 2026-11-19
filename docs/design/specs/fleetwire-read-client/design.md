# fleetwire 설계

## 1. 설계 개요

`fleetwire`는 Tesla Fleet API의 라이브 검증 가능한 GET 16개를 강타입으로 제공하는 비동기 Rust 라이브러리다. 기본 사용 경로는 엔드포인트 타입과 편의 메서드이며, 아직 지원되지 않은 GET을 위한 제한된 `send_raw`를 별도로 둔다. 비용·보안·동시성 안전성을 기능 정확성과 같은 수준의 불변식으로 취급한다.

### 목표

- vehicle 12개와 user 4개를 강타입 GET API로 제공한다.
- 차량 연결성, 스코프, 과금 가능성을 요청 전후에 명시적으로 다룬다.
- 토큰 회전과 다중 프로세스 refresh를 데이터 손실 및 중복 refresh 없이 처리한다.
- Tesla 응답 필드 변화에는 관대하되 라이브 구조 변화는 별도 파이프라인으로 감지한다.
- 공개 API와 내부 구현 모두에서 Fleet API 쓰기 동작을 차단한다.

### 비목표

- charging·energy·partner의 나머지 GET 12개 지원 또는 라이브 검증
- Fleet API command/write API 지원
- JWT 서명 검증 또는 advisory `scp`를 권한 판정 근거로 사용하는 것
- 자동 무제한 페이지 순회 또는 기본 자동 재시도

## 2. 시스템 구조

```text
사용자 코드
   │
   ├─ Client 편의 메서드 ──┐
   ├─ send(FleetGet) ──────┼─> Endpoint/Query/Scope/Pricing 계층
   ├─ send_guarded(...) ───┘              │
   └─ send_raw(relative, QuerySpec) ──────┤
                                          ▼
                              요청 실행 파이프라인
                 scope preflight → region/base URL → gate
                    → auth → retry/deadline → HTTP GET
                    → bounded stream → status/content type
                    → envelope/decode → typed response
                         │                 │
                         ▼                 ▼
                  TokenManager        Error/Billing
             mutex + flock + disk-first      │
                         │                   ▼
                         └────────────> Redactor
```

### 제안 모듈 경계

```text
src/
├── lib.rs                  # 공개 re-export, crate 문서
├── client.rs               # Client, send/send_guarded/send_raw, 편의 메서드
├── endpoint/
│   ├── mod.rs              # FleetGet, VehicleEndpoint, Sealed
│   ├── vehicle.rs          # vehicle 12개 endpoint tag
│   └── user.rs             # user 4개 endpoint tag
├── query.rs                # QuerySpec, 검증·인코딩
├── model/
│   ├── common.rs           # Envelope, Page, 페이지네이션, 확장 필드
│   ├── vehicle.rs          # vehicle 응답 타입
│   └── user.rs             # user 응답 타입
├── scope.rs                # Scope, ScopeExpr, advisory JWT scp parser
├── pricing.rs              # Pricing, Billing
├── error.rs                # Error와 endpoint-aware 판정 메서드
├── auth/
│   ├── mod.rs              # TokenManager API
│   ├── refresh.rs          # 유일한 POST 예외
│   └── store.rs            # 0600, symlink 방어, atomic/fsync 저장
├── region.rs               # region 해석, OnceCell cache, NA fallback
├── transport.rs            # 비공개 reqwest client, GET 실행, bounded stream
├── retry.rs                # retry decision, backoff, chain deadline
└── redaction.rs            # 모든 외부 표현 전 단일 정제 계층
```

`transport`, raw `reqwest::Client`, 토큰 원문과 redaction 전 오류는 crate-private으로 유지한다. 공개 오류는 이미 정제된 소유 데이터만 가진다.

## 3. 공개 API와 타입 설계

### 3.1 엔드포인트 trait

```rust
use serde::de::DeserializeOwned;

pub trait FleetGet: sealed::Sealed + Send + Sync {
    type Response: DeserializeOwned;

    fn required_scopes(&self) -> ScopeExpr;
    fn pricing(&self) -> Pricing;
    fn path(&self) -> String;
    fn query(&self) -> QuerySpec;
}

pub trait VehicleEndpoint: FleetGet {
    fn vin(&self) -> &str;
}
```

- `sealed::Sealed`로 외부 구현을 차단해 내부의 GET-only·same-origin 불변식을 보존한다.
- `pricing()`은 기본 구현 없이 모든 endpoint tag가 `Pricing::{NotCharged, Data { usd_per_request: 0.002 }}` 중 하나를 의식적으로 선언하게 한다.
- endpoint에서 얻은 `Pricing`은 요청과 오류 컨텍스트가 carry하여 `Error::billing()`의 판정에 사용한다.
- `required_scopes()`는 `VehicleData`의 위치 옵션처럼 값에 의존하므로 const가 아니다.
- `path()`는 endpoint 내부에서만 조립하며 VIN은 path segment로 인코딩한다.
- `query()`는 문자열 연결이 아닌 key/value 구조를 반환한다.

### 3.2 호출 API

```rust
pub enum Guarded<T> {
    Executed(T),
    Skipped { connectivity: Connectivity },
}

impl Client {
    pub async fn send<R: FleetGet>(&self, request: &R)
        -> Result<R::Response, Error>;

    pub async fn send_guarded<R: VehicleEndpoint>(&self, request: &R)
        -> Result<Guarded<R::Response>, Error>;

    pub async fn send_raw(&self, relative_path: &str, query: QuerySpec)
        -> Result<serde_json::Value, Error>;
}
```

편의 메서드는 `client.vehicles(page)`, `client.vehicle(&vin)`, `client.vehicle_data(tag)`처럼 endpoint tag를 구성한 뒤 위 메서드로 위임한다. 차량의 비용 민감 호출에는 대응하는 guarded 편의 메서드를 제공한다. trait 기반 확장은 유지하되 일반 사용자가 복잡한 제네릭 오류를 접하지 않게 한다.

### 3.3 raw GET 안전성

`send_raw` 입력은 다음 순서로 검증한다.

1. scheme, authority, backslash, NUL, scheme-relative(`//`)가 없는 상대경로인지 검사한다.
2. path를 정규화해 `..`가 API root 또는 origin 경계를 벗어나지 않는지 검사한다.
3. 캐시된 리전 base URL과 `Url::join` 후 scheme·host·port가 원본과 같은지 재검증한다.
4. `QuerySpec`을 serializer로 인코딩한다.
5. 내부 GET 전용 실행기만 호출한다.

이 탈출구는 강타입 기본 설계를 대체하지 않으며 HTTP method를 받지 않는다.

## 4. 엔드포인트 카탈로그

가격은 엔드포인트별로 `Pricing::{NotCharged, Data { usd_per_request: 0.002 }}` 중 하나다. OpenAPI `pricing_category=device_data`인 `vehicle_data`와 `nearby_charging_sites` 2개만 Data 유료이며, `pricing_category=null`인 나머지 14개는 무과금이다.

| # | Endpoint tag / 편의 메서드 | GET 경로 | 요구 스코프 | 가격 | 특수 동작 |
|---:|---|---|---|---|---|
| 1 | `Vehicles` / `vehicles` | `/api/1/vehicles` | `vehicle_device_data` | `Pricing::NotCharged` | `Page<VehicleSummary>`, query `page`·`per_page`, 기본 100 |
| 2 | `Vehicle` / `vehicle` | `/api/1/vehicles/{vin}` | `vehicle_device_data` | `Pricing::NotCharged` | `state`를 무료 연결성 게이트에 사용 |
| 3 | `VehicleData` / `vehicle_data` | `/api/1/vehicles/{vin}/vehicle_data` | 기본 `vehicle_device_data`; 위치 요청 시 `All([vehicle_device_data, vehicle_location])` | `Pricing::Data { usd_per_request: 0.002 }` | 가장 비싼 데이터 경로, 위치 이중 opt-in |
| 4 | `MobileEnabled` / `mobile_enabled` | `/api/1/vehicles/{vin}/mobile_enabled` | `vehicle_device_data` | `Pricing::NotCharged` | guarded 제공 |
| 5 | `NearbyChargingSites` / `nearby_charging_sites` | `/api/1/vehicles/{vin}/nearby_charging_sites` | `vehicle_device_data` | `Pricing::Data { usd_per_request: 0.002 }` | page 아님, 별도 QuerySpec의 `count`·`radius`·`detail`, guarded 제공 |
| 6 | `RecentAlerts` / `recent_alerts` | `/api/1/vehicles/{vin}/recent_alerts` | `vehicle_device_data` | `Pricing::NotCharged` | guarded 제공 |
| 7 | `ReleaseNotes` / `release_notes` | `/api/1/vehicles/{vin}/release_notes` | `vehicle_device_data` | `Pricing::NotCharged` | guarded 제공 |
| 8 | `ServiceData` / `service_data` | `/api/1/vehicles/{vin}/service_data` | `vehicle_device_data` | `Pricing::NotCharged` | guarded 제공 |
| 9 | `Drivers` / `drivers` | `/api/1/vehicles/{vin}/drivers` | `vehicle_device_data` | `Pricing::NotCharged` | guarded 제공 |
| 10 | `Invitations` / `invitations` | `/api/1/vehicles/{vin}/invitations` | `vehicle_device_data` | `Pricing::NotCharged` | `Page<Invitation>`, query `page`·`per_page`, 페이지 최대 25 |
| 11 | `FleetTelemetryConfig` / `fleet_telemetry_config` | `/api/1/vehicles/{vin}/fleet_telemetry_config` | `vehicle_device_data` | `Pricing::NotCharged` | guarded 제공 |
| 12 | `FleetTelemetryErrors` / `fleet_telemetry_errors` | `/api/1/vehicles/{vin}/fleet_telemetry_errors` | `vehicle_device_data` | `Pricing::NotCharged` | guarded 제공 |
| 13 | `UserMe` / `user_me` | `/api/1/users/me` | user-context 토큰 + `user_data` 계열 | `Pricing::NotCharged` | 3rd-party business 토큰 비호환, 사용자 정보 |
| 14 | `UserRegion` / `user_region` | `/api/1/users/region` | user-context 토큰 + 여러 허용 스코프 중 하나 | `Pricing::NotCharged` | base URL 해석에 사용 |
| 15 | `UserOrders` / `user_orders` | `/api/1/users/orders` | user-context 토큰 + `user_data` 계열 | `Pricing::NotCharged` | 3rd-party business 토큰 비호환, 주문 목록 |
| 16 | `UserFeatureConfig` / `user_feature_config` | `/api/1/users/feature_config` | user-context 토큰 + `user_data` 계열 | `Pricing::NotCharged` | 3rd-party business 토큰 비호환, 기능 설정 |

`Vehicle` 자체는 게이트를 위한 probe이므로 동일 endpoint에 대한 재귀적 gate를 걸지 않는다. 다른 `VehicleEndpoint`의 `send_guarded`만 `Vehicle`을 먼저 호출한다.

## 5. 요청 실행 파이프라인

### 5.1 일반 `send`

1. endpoint의 `ScopeExpr`, `Pricing`, path, query를 얻고 `Pricing`을 요청 및 오류 컨텍스트에 유지한다.
2. access token이 파싱 가능한 JWT이면 `scp`를 advisory로 대조한다. 명백한 불일치는 네트워크 없이 `MissingScope`; 파싱 실패는 fail-open이다.
3. `RegionResolver`에서 base URL을 얻는다. 최초 해석 실패 시 NA로 폴백한다.
4. 경로를 same-origin으로 결합하고 인증 header를 추가한다. 이 시점까지 원문 토큰은 로깅 가능한 타입에 들어가지 않는다.
5. 전체 재시도 체인을 `tokio::time::timeout`으로 감싼다.
6. 각 시도는 connect 10초·request 30초 설정을 사용한다.
7. 응답 body를 `bytes_stream()`으로 읽으며 누적 제한 초과 즉시 stream을 drop한다.
8. status와 Content-Type을 먼저 분류한다.
9. 성공 JSON이면 envelope를 해체해 `R::Response`로 decode한다.
10. 401은 분류 후 refresh를 최대 한 번 수행하고 원 요청을 한 번만 다시 보낸다.
11. 모든 외부 오류·로그 표현은 `Redactor`에서 정제된 뒤 생성한다.

### 5.2 `send_guarded`

```text
endpoint.vin()
   └─ GET /vehicles/{vin} (Pricing::NotCharged)
          ├─ state == online ──> send(endpoint) ──> Guarded::Executed
          └─ otherwise ─────────────────────────> Guarded::Skipped
```

게이트 probe는 OpenAPI `pricing_category=null`로 실증된 무과금 요청이다. 따라서 `send_guarded`는 무료 state 확인으로 대상 유료 호출의 무의미한 408을 줄인다. `Connectivity`는 원래 state 문자열을 민감하지 않은 정규화 enum(`Online`, `Asleep`, `Offline`, `Unknown`)으로 보존한다.

### 5.3 위치 데이터

```rust
pub struct VehicleData<'a> {
    vin: &'a str,
    include: VehicleDataSections,
}

impl FleetGet for VehicleData<'_> {
    fn required_scopes(&self) -> ScopeExpr {
        if self.include.location_data() {
            ScopeExpr::All(vec![
                Scope::VehicleDeviceData,
                Scope::VehicleLocation,
            ])
        } else {
            ScopeExpr::One(Scope::VehicleDeviceData)
        }
    }

    fn query(&self) -> QuerySpec {
        QuerySpec::from_sections(&self.include)
    }
    // pricing과 path도 명시적으로 구현
}
```

펌웨어 2023.38 이상에서 위치는 query의 `location_data`와 OAuth `vehicle_location`이 모두 있어야 하는 이중 opt-in이다.

## 6. 응답과 페이지 모델

```rust
#[derive(serde::Deserialize)]
struct Envelope<T> {
    response: T,
}

#[derive(serde::Deserialize)]
pub struct VehicleSummary {
    #[serde(default)]
    pub id: Option<serde_json::Value>,
    #[serde(default)]
    pub vin: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(flatten)]
    pub extra: serde_json::Map<String, serde_json::Value>,
}

#[derive(serde::Deserialize)]
pub struct Pagination {
    pub previous: Option<u64>,
    pub next: Option<u64>,
    pub current: Option<u64>,
    pub per_page: Option<u64>,
    pub count: Option<u64>,
    pub pages: Option<u64>,
}

pub struct Page<T> {
    pub items: Vec<T>,
    pub pagination: Pagination,
}
```

모든 알려진 응답 필드는 `Option<T>`와 `#[serde(default)]`를 함께 사용한다. 모든 구조체는 `#[serde(flatten)] extra`를 두며 `deny_unknown_fields`는 사용하지 않는다. 이는 느슨한 무검증이 아니라, API 추가 필드에 대한 전방 호환과 원문 보존 정책이다.

차량 목록과 초대 목록은 offset이나 cursor가 아니라 query `page`와 `per_page`를 사용하고, 응답의 `pagination { previous, next, current, per_page, count, pages }`를 그대로 노출한다. `all_pages(max_pages)`는 `max_pages > 0`을 요구하고 반복 페이지도 감지한다. 각 페이지는 독립 호출이므로 기본 호출과 분리한다. 두 목록은 현재 `Pricing::NotCharged`이지만 60회/분 rate limit은 적용된다. 차량은 기본 page size 100, invitations는 입력을 1..=25로 제한한다.

`nearby_charging_sites`는 페이지네이션 대상이 아니다. 해당 endpoint 전용 `NearbyChargingSitesQuery`가 `count`, `radius`, `detail`을 검증하고 `QuerySpec`으로 직렬화하며 `page` 또는 `per_page`를 받지 않는다.

## 7. 인증과 토큰 수명주기

### 7.1 호스트 분리

- 로그인: `https://auth.tesla.com/oauth2/v3/authorize`
- refresh: `https://fleet-auth.prd.vn.cloud.tesla.com/oauth2/v3/token`

호스트 분리는 Tesla 규격이며 통합하거나 같은 origin으로 강제하지 않는다. refresh POST는 Fleet API GET-only 정책의 유일한 인증 예외다.

### 7.2 401 처리

401 분류기는 토큰 만료·인증 challenge 등 refresh 가능한 경우와 scope·형식·기타 인증 실패를 구분한다. 하나의 논리 요청은 `refresh_attempted` 플래그를 가지며 refresh 가능한 401도 최대 한 번만 refresh한다. refresh 후 원 요청도 한 번만 재시도한다. 다시 401이면 `Error::Auth` 또는 `Error::Api`로 종료한다.

### 7.3 다중 프로세스 single-flight

```text
refresh 필요
  1. tokio Mutex 획득 (await 전체 보유)
  2. ~/.tesla/tokens.lock 열기 (고정 inode, 삭제 금지)
  3. timeout 포함 flock(LOCK_EX)
  4. token file 재읽기
  5. access 유효? ─ yes → Adopted
                 └ no  → refresh POST 1회
                            └ 새 token disk-first 저장
  6. flock 해제
  7. Mutex 해제
  8. 원 요청 1회 재시도
```

고정 lock 파일은 stale 상태를 정리하지 않는다. `flock`은 프로세스 종료 때 커널이 해제하며 파일을 삭제하면 기존 inode holder와 새 inode holder가 동시에 생겨 상호배제가 깨진다. 토큰 값은 회전 가능하므로 `OnceCell`/`OnceLock`에 넣지 않는다.

### 7.4 disk-first 저장

flock 보유 중 다음 순서로 저장한다.

1. 안전한 디렉터리와 regular file 여부, 소유권 및 symlink 부재를 검사한다.
2. 동일 디렉터리에 `O_CREAT|O_EXCL|O_NOFOLLOW`, mode 0600으로 임시 파일을 만든다.
3. 완전한 새 토큰 JSON을 쓰고 파일을 `fsync`한다.
4. 원자적으로 token 파일로 `rename`한다.
5. 부모 디렉터리를 열어 `fsync`한다.
6. 성공 후에만 메모리 access token을 교체한다.

새 refresh token 저장 실패는 치명적인 `Auth` 오류다. 저장 전에 원 요청을 재시도하지 않는다.

## 8. 리전 해석

`RegionResolver`는 내부 `OnceCell<BaseUrl>`을 가진다. 초기화 시 bootstrap 가능한 Fleet API 주소로 `/api/1/users/region`을 호출하고 응답의 리전 URL을 same-origin 정책에 사용할 기준으로 저장한다. 실패 시 `https://fleet-api.prd.na.vn.cloud.tesla.com`을 캐시한다. 폴백 이벤트는 token·VIN·좌표 없이 구조화 로그로 남긴다.

리전 캐시만 `OnceCell`을 사용한다. 토큰과 달리 한 Client 수명 동안 불변으로 취급할 수 있기 때문이다.

## 9. 오류, 과금 및 재시도

### 9.1 공개 오류

```rust
pub enum Error {
    TransportNotSent { message: String, pricing: Pricing },
    TransportUnknown { message: String, pricing: Pricing },
    Api {
        status: http::StatusCode,
        message: String,
        retry_after: Option<std::time::Duration>,
        body_preview: String,
        pricing: Pricing,
    },
    VehicleOffline { pricing: Pricing },
    MissingScope { required: ScopeExpr, held: Vec<Scope> },
    Decode {
        message: String,
        status: http::StatusCode,
        body_preview: String,
        pricing: Pricing,
    },
    Auth(String),
}

pub enum Pricing {
    NotCharged,
    Data { usd_per_request: f64 },
}

pub enum Billing { Billed, NotBilled, Unknown }
```

실제 구현에서 `source`를 보존하더라도 공개 `Display`/`Debug`는 정제된 표현만 출력한다. `reqwest::Error` 자체를 그대로 공개 variant에 보관해 URL 또는 source chain이 노출되지 않게 한다. endpoint의 `FleetGet::pricing()` 값은 요청 실행 시 오류 컨텍스트로 복사되어 `Error::billing()`이 endpoint 과금 카테고리를 판정할 수 있게 한다. `MissingScope`처럼 네트워크 전 오류와 인증 자체 오류도 요청 endpoint 컨텍스트가 필요한 경우 내부 공통 래퍼로 `Pricing`을 carry한다.

### 9.2 판정표

| 결과 | `Pricing::NotCharged` 과금 판정 | `Pricing::Data` 과금 판정 | 재시도 | 이유 |
|---|---|---|---|---|
| DNS/connect 미전송 확실 | `NotBilled` | `NotBilled` | 허용 | `TransportNotSent` |
| read timeout/reset | `NotBilled` | `Unknown` | 금지 | 서버 처리 여부 불명, 중복 과금 방지 |
| 408 | `NotBilled` | `Billed` | 금지 | Data이면 `$0.002`, `VehicleOffline` |
| 기타 4xx | `NotBilled` | `Billed` | 금지 | Data에서 status `< 500` |
| 429 | `NotBilled` | `Billed` | 금지 | 공유 60/분 한도, Data 중복 과금 방지 |
| 500, 502, 503, 504 | `NotBilled` | `NotBilled` | 명시 설정 시 허용 | 일시적 5xx |
| 기타 5xx | `NotBilled` | `NotBilled` | 금지 | 허용 목록 외 |
| 성공 `< 500` | `NotBilled` | `Billed` | 해당 없음 | 엔드포인트별 과금 카테고리 |

`Pricing::NotCharged`인 14개 엔드포인트는 status와 무관하게 항상 `Billing::NotBilled`다. `Pricing::Data`인 2개 엔드포인트에서 `TransportUnknown`(read timeout·connection reset 등)에는 HTTP status가 없어 서버 처리 여부를 증명할 수 없으므로 보수적으로 `Unknown`을 기본값으로 둔다. 반면 `TransportNotSent`는 DNS·connect 실패처럼 **미전송이 확실한** 경우만 쓰므로 판정표대로 `NotBilled`다. 재시도 가능 여부와 과금 확정 여부는 별개다.

> **정정 이력 (2026-07-30, A1 구현 중 발견)**
> - 위 문단은 원래 `TransportNotSent`가 `Unknown`이라고 서술해 바로 위 판정표(`NotBilled`)와 모순이었다. REQ-11의 수용 기준("DNS·connect 실패처럼 미전송이 확실한 경우 `TransportNotSent`", "전송 또는 처리 여부 불명 오류는 `Unknown`")과 판정표가 일치하므로 산문 쪽을 정정했다 — 원 문장은 `TransportUnknown`을 가리키며 변수명을 잘못 쓴 것으로 판단된다.
> - §9.1의 `Decode { source: String, .. }`을 `message: String`으로 정정했다. `thiserror` derive는 `source`라는 **이름**의 필드를 무조건 `std::error::Error::source()` 체인으로 잡아 해당 필드에 `Error` 구현을 요구하며, opt-out 속성이 없다(`thiserror-impl` `prop.rs:105`, `attr.rs`에는 opt-in용 `#[source]`만 존재). 따라서 `source: String`은 **컴파일 불가**였다. 대안인 newtype 래핑은 §9.1이 금지하는 foreign error 타입·source chain 노출을 오히려 유발하므로 개명이 유일하게 정합한 해법이다.

### 9.3 재시도 정책

- 기본 `max_attempts = 1`.
- opt-in 시 `TransportNotSent`, 500, 502, 503, 504만 허용한다.
- exponential backoff에 jitter를 적용하고 산출 지연을 30초로 cap한다.
- 유효한 `Retry-After`는 존중하되 120초로 cap한다.
- 전체 체인 deadline보다 긴 대기는 즉시 timeout으로 종료한다.
- 429의 `RateLimit-*`와 `Retry-After`는 오류에 관찰 정보로 보존하지만 자동 재시도하지 않는다.
- 무과금 엔드포인트는 과금 관점에서 재시도해도 중복과금이 없지만, 기본 `max_attempts = 1`과 위 allowlist는 유지하고 Realtime Data 60회/분 rate limit을 준수한다.

## 10. HTTP 계층

`ClientBuilder`가 각 `Client` 인스턴스용 장수 `reqwest::Client`를 한 번 만든다.

```rust
reqwest::Client::builder()
    .connect_timeout(Duration::from_secs(10))
    .timeout(Duration::from_secs(30))
    .pool_max_idle_per_host(2)
    .pool_idle_timeout(Duration::from_secs(90));
```

프로세스 전역 static은 사용하지 않는다. 논리 호출 전체는 `tokio::time::timeout(chain_deadline, retry_loop(...))`으로 감싸 개별 request timeout 사이의 backoff까지 제한한다.

본문은 `bytes_stream()`의 각 chunk를 받자마자 `checked_add`로 누적 크기를 계산한다. 상한 초과 전에 허용되는 양만 보관하고, 초과가 확인되면 stream을 즉시 drop한다. 오류용 body preview도 같은 bounded reader와 redaction을 거친다.

## 11. 민감정보 정제 설계

모든 로그·오류·fixture 출력은 다음 하나의 `Redactor` API만 사용한다.

```rust
pub(crate) trait Redactor {
    fn redact_json(&self, value: &serde_json::Value) -> serde_json::Value;
    fn redact_text(&self, text: &str) -> RedactedString;
    fn redact_transport_error(&self, error: &reqwest::Error) -> RedactedError;
}
```

처리 우선순위:

1. JSON을 파싱할 수 있으면 allowlist 키만 보존하고 token/secret 키, VIN, 좌표, id를 구조적으로 치환한다.
2. URL은 query와 path segment를 구조적으로 정제한다.
3. 비구조 문자열에만 Bearer, JWT 형태, refresh token, client secret, VIN, 위도·경도 regex를 보조 적용한다.
4. `Display`, `Debug`, source chain, tracing field, body preview에는 정제된 새 타입만 전달한다.

fixture 정제기는 `[REDACTED]`, `[VIN]`, `[COORD]` 또는 결정적 비식별 placeholder를 사용한다. 정제 전 캡처는 CI artifact나 git 작업 트리에 남기지 않고 제한된 임시 저장소에서 즉시 폐기한다.

## 12. GET-only 강제

1. **타입 시스템**: `FleetGet`은 method를 노출하지 않고 내부 실행기는 GET만 생성한다. 공개 API에 임의 method 인자가 없다.
2. **모듈 가시성**: raw `reqwest::Client`와 request builder는 crate-private이다.
3. **정적 lint**: workspace 수준에서 `reqwest::Client::{post, put, delete, request}`를 금지하고 `disallowed_methods = "deny"`로 활성화한다.

refresh 모듈 한 곳의 `post`만 다음처럼 좁게 예외 처리한다.

```rust
// OAuth token refresh is the sole POST exception; Fleet API traffic remains GET-only.
#[allow(clippy::disallowed_methods)]
async fn refresh_token(/* ... */) -> Result<TokenSet, Error> {
    // reqwest::Client::post(FLEET_AUTH_TOKEN_URL) ...
}
```

`build.rs` 기반 검사나 코드 생성은 도입하지 않는다.

## 13. 테스트와 드리프트 감지

### 13.1 공개 CI — $0

- 유닛: QuerySpec 인코딩, same-origin 거부, `ScopeExpr`, fail-open JWT parser, gate, endpoint별 Pricing/Billing, retry allowlist, deadline, 페이지 limit, token state machine, atomic store helper, redaction.
- fixture 계약: 16개 응답의 알려진 필드 누락·추가 필드·봉투 예외·오류 응답 파싱 회귀.
- 불변식: 공개 API에서 write method 부재, disallowed-methods lint, lock 파일 미삭제, token `OnceCell` 부재.

fixture는 저장된 입력에 대한 parser 회귀만 검증한다. 현재 서비스 구조의 드리프트를 감지하지 않는다.

### 13.2 주간 라이브 작업

1. token이 있는 보호 환경에서 16개 endpoint를 주 1회 호출한다.
2. 가능한 차량에 대해 주차·주행·충전 상태를 나누어 캡처한다.
3. 원문을 정제기로 통과시켜 token·VIN·좌표·id를 제거한다.
4. 값이 아니라 key path, JSON kind, 배열 item shape, nullable 여부 중심의 구조 지문을 만든다.
5. 커밋된 이전 지문과 새 라이브 지문을 diff한다.
6. 차이가 있으면 endpoint, path, 타입 변화만 담은 이슈를 생성한다.
7. 정제된 신규 fixture 반영은 별도 검토로 수행한다.

16개 엔드포인트 각 1회 기준 유료 호출은 2회이므로 기본 비용은 주당 $0.004, `2 × $0.002 × 4.33 ≈ 월 $0.02`다. region, gate, pagination, 상태별 반복은 별도 계측하여 월 $10 한도에 포함한다. 정적 fixture를 커밋된 스냅샷과 비교하는 자기비교는 사용하지 않는다.

## 14. 릴리스와 호환성

릴리스 게이트는 다음을 모두 수행한다.

```text
cargo clippy -- -D warnings
cargo semver-checks
cargo deny check
cargo test --locked
MSRV toolchain cargo test --locked
```

manifest에 Apache-2.0, MSRV, `[package.metadata.docs.rs] all-features = true`를 선언한다. application 성격의 재현성과 공개 CI를 위해 `Cargo.lock`을 커밋한다. 공급망 공격 표면을 줄이기 위해 `build.rs`를 만들지 않는다.

## 15. 계승 불변식

| 불변식 | 설계 반영 |
|---|---|
| I1 새 refresh token 즉시 저장 | flock 아래 disk-first 저장 후에만 원 요청 재시도 |
| I2 인증 호스트 분리 | login은 `auth.tesla.com`, refresh는 `fleet-auth...tesla.com` |
| I3 리전 캐시와 NA 폴백 | `RegionResolver`의 전용 `OnceCell`, 실패 시 NA |
| I4 token 파일 안전성 | 0600, `.tmp`+rename, 파일·디렉터리 fsync, symlink 방어 |
| I5 408 상위 흡수 가능 | `Error::VehicleOffline` 공개 variant와 재시도 금지 |

## 16. 요구사항 매핑 매트릭스

| 요구사항 | 설계 요소 | 검증 방식 |
|---|---|---|
| REQ-1 | 엔드포인트 카탈로그의 vehicle 12 + user 4, user-context/user_data 스코프, 명시적 비목표 | 16개 endpoint/편의 메서드 API 검사와 라이브 캡처 목록 검사 |
| REQ-2 | `FleetGet`, `VehicleEndpoint`, 편의 메서드 위임 | compile test와 endpoint별 trait 구현 검사 |
| REQ-3 | 상대경로 전용 `send_raw`, same-origin 검증 | 절대·scheme-relative·host 탈출 입력 거부 테스트 |
| REQ-4 | `Option<T>`, `serde(default)`, flatten extra, unknown 허용 | 누락/추가 필드 fixture 계약 테스트 |
| REQ-5 | status/Content-Type 우선 분류와 `Envelope<T>` | 봉투·비봉투·잘못된 Content-Type 테스트 |
| REQ-6 | `Page<T>`, `page`+`per_page`, 전체 pagination 메타데이터, `all_pages(max_pages)`, nearby 전용 QuerySpec | 기본 100, invitations 최대 25, 페이지 상한, nearby query 테스트 |
| REQ-7 | `VehicleDataSections`, 동적 `ScopeExpr::All`과 QuerySpec | 위치 query/scope 조합 테스트 |
| REQ-8 | advisory JWT `scp` parser와 fail-open | All/Any/One, mismatch 무전송, parse 실패 통과 테스트 |
| REQ-9 | `send_guarded`, 무료 state probe, `Guarded<T>` | online 실행 및 비-online skip와 probe `NotCharged` 테스트 |
| REQ-10 | endpoint별 `Pricing`, endpoint-aware `Billing`, 게이트와 비용 계측 | 2 Data/14 NotCharged 및 pricing별 status billing table 테스트와 예산 문서 검사 |
| REQ-11 | `Pricing`을 carry하는 공개 `Error` enum과 판정 메서드 | transport·408·decode·API 분류 및 endpoint-aware billing 테스트 |
| REQ-12 | 기본 1회, retry allowlist, jitter/cap/deadline, 무과금에도 rate limit 유지 | 429/408/unknown 무재시도 및 5xx allowlist 테스트 |
| REQ-13 | 인스턴스 소유 reqwest client, timeout, bounded stream | builder 설정 검사와 chunk 초과 즉시 중단 테스트 |
| REQ-14 | 로그인/refresh 호스트 분리, 401 1회 상태 플래그 | URL 상수와 refresh/replay 횟수 테스트 |
| REQ-15 | await mutex + timeout flock + lock 아래 재읽기/Adopted | 동시 task 및 다중 process 통합 테스트, lock 파일 존속 검사 |
| REQ-16 | 0600, nofollow, tmp/fsync/rename/dir fsync disk-first | 파일 mode·symlink·장애 주입 저장 순서 테스트 |
| REQ-17 | `RegionResolver`와 NA cached fallback | 성공 cache 및 실패 fallback 테스트 |
| REQ-18 | 단일 `Redactor`, 구조화 우선·regex 보조 | token/VIN/좌표/id와 reqwest URL/source 정제 테스트 |
| REQ-19 | sealed GET API, 비공개 transport, disallowed-methods | Clippy deny와 공개 API 검토, refresh 단일 allow 검사 |
| REQ-20 | $0 공개 CI와 주간 라이브 구조 지문 diff | fixture 회귀·정제기 테스트 및 보호된 scheduled workflow |
| REQ-21 | clippy/semver/deny/test/MSRV/docs.rs/lock/no build.rs | 릴리스 CI와 패키지 내용 검사 |
| REQ-22 | 범위·검증 수준 명시와 이 매핑 표 | REQ ID 교차 참조 검사 및 문서 리뷰 체크리스트 |

## 부록: O1 정정 이력(2026-07-29)

1. **과금 per-endpoint**: 16개 전체를 Data로 보던 가정을 폐기했다. `Pricing::{NotCharged, Data { usd_per_request: 0.002 }}`로 바꾸고 `vehicle_data`·`nearby_charging_sites` 2개만 Data, 나머지 14개는 NotCharged로 확정했다.
2. **Billing 판정**: 무과금 14개는 status와 무관하게 `Billing::NotBilled`로 고정했다. 유료 2개만 status `<500`이면 `Billed`, `>=500`이면 `NotBilled`, 전송 또는 처리 여부 불명이면 `Unknown`을 적용하도록 endpoint의 `Pricing`을 오류 컨텍스트가 carry하게 했다.
3. **비용 게이트·재시도**: `GET /vehicles/{vin}` state 게이트가 무과금임을 반영했다. 무과금 endpoint의 재시도는 중복과금이 없지만 기본 `max_attempts=1`, 기존 재시도 allowlist, 60회/분 rate limit은 유지했다.
4. **user 스코프**: user 4개를 `openid`로 일괄 표기한 내용을 user-context 토큰과 `user_data` 계열 스코프로 정정했다. 3rd-party business 토큰은 비호환이며 `/users/region`은 여러 스코프 중 하나로 접근 가능함을 반영했다.
5. **페이지네이션**: offset/cursor가 아니라 query `page`+`per_page`와 응답 `pagination { previous, next, current, per_page, count, pages }`를 사용하도록 정정했다. vehicles 기본 100, invitations 최대 25를 유지하고 `nearby_charging_sites`는 page가 아닌 `count`·`radius`·`detail` 전용 QuerySpec으로 분리했다.

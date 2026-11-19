# fleetwire 읽기 클라이언트 — 설계 스펙

> 상태: **설계 확정 / 구현 미착수**. 2026-07-24 KST.
> 범위: 서브프로젝트 **A(읽기 GET Rust 라이브러리)** 만. MCP·명령·클라우드는 별도.
> 검증: 설계 검토 4라운드 + 로컬 실험 +
> `xai-org/grok-build`(Apache-2.0, rev `69f0ba8`) 소스 분석 + Rust 커뮤니티 관례 조사.

## 0. 무엇을 만드는가

`fleetwire` = Tesla Fleet API 의 **Rust 클라이언트 라이브러리**(OSS 예정, 이름은 가칭).
1단계는 읽기 GET. 최종 목표는 명령 72개 포함 전체지만 **이 스펙의 범위가 아니다**.

### 서브프로젝트 분해 (이 스펙은 A 만)
```
E(클라우드 이전) ──→ 공개키 상시 호스팅 ──┬─→ C(명령 72개: virtual key pairing 필수)
                                        └─→ D(Fleet Telemetry: mTLS 수신 서버)
A(읽기) ──→ B(MCP 서버) ──→ F(OSS 릴리스)
```
A·B 는 지금 착수 가능. C·D 는 E 가 선행 조건.

### 두 개념을 분리한다 (혼동 금지)
| 개념 | 값 |
|---|---|
| **라이브러리가 구현하는 엔드포인트** | GET 28개 (문서화된 전부) |
| **운영 토큰이 실제로 가진 권한** | 4 스코프 → **16개만 호출 가능** |
나머지 12개는 컴파일되지만 런타임에 `MissingScope` 로 막힌다. **이는 결함이 아니라 설계**다 —
OSS 사용자는 다른 스코프를 가질 수 있고, 라이브러리는 "왜 안 되는지"를 설명해야 한다.

## 1. 외부 사실 (버전드 정책 테이블)

> ⚠️ 영구 사실이 아니다. 재검증 주기: 분기 1회 또는 Tesla 공지 시.
> 출처는 전부 `developer.tesla.com`, 2026-07-24 KST 조회.

| # | 사실 | 문서 경로 |
|---|---|---|
| F1 | 종량제(2025-01-01~). **Data $0.002/req**, Commands $0.001, **Wakes $0.02**, Streaming $1/150k | `/docs/fleet-api/billing-and-limits`, `/#usage-based-pricing` |
| F2 | 개인 할인 **월 $10(계정당)** = Data 5,000회 | 同 |
| F3 | **status < 500 인 모든 응답이 과금.** ≥500 만 무과금 → **408(차량 수면)도 과금** | 同 |
| F4 | Rate limit: Data **60/분**, Wakes 3/분, Commands 30/분. 디바이스+계정 단위, 같은 계정의 여러 앱이 공유 | 同 |
| F5 | 공식 권고: **`GET /api/1/vehicles/{vin}` 의 `state`** 로 연결성 확인 후 유료 작업 | `/docs/fleet-api/getting-started/best-practices` |
| F6 | `vehicle_charging_cmds` 는 **읽기 포함**(충전 이력·금액·위치). read-only 전용 charging 스코프 없음 | `/docs/fleet-api/authentication/overview` |
| F7 | `GET /dx/charging/sessions` 는 **business fleet 전용** | `/docs/fleet-api/endpoints/charging-endpoints` |
| F8 | JSON 엔드포인트는 `{"response": …}` 봉투 사용. **invoice(PDF)·인증오류·게이트웨이 응답은 예외** | `/docs/fleet-api/endpoints/*` |
| F9 | 펌웨어 2023.38+ 는 `?endpoints=…location_data…` 없으면 위치 생략(+`vehicle_location` 스코프 필요) | `/docs/fleet-api/endpoints/vehicle-endpoints` |

## 2. 확정된 결정

| # | 결정 |
|---|---|
| U1 | 읽기 28개 **전체 강타입**(serde struct) |
| U2 | 런타임 **관대**(`Option` + `default` + `flatten extra`). `deny_unknown_fields` **미사용** |
| U3 | **이중 API** — `send`(typed) + `send_guarded`(state 게이트) + `send_raw`(탈출구) |
| U4 | 운영 토큰 **최소권한 4 스코프**(`openid offline_access vehicle_device_data vehicle_location`). ⚠️ 이는 **토큰 정책**이지 라이브러리 범위가 아니다 |
| U5 | 토큰 scp **참고용 선검사** → `MissingScope`(네트워크·과금 0) |
| U6 | 기존 코드 복사 없이 신규 개발. **불변식 5개만 계승** |
| U7 | 과금 여부를 **3상태로 공개**(`Billed`/`NotBilled`/`Unknown`) |

## 3. crate·모듈 구조

```
fleetwire/
├── Cargo.toml           # [workspace] + [workspace.dependencies] + [workspace.lints]
├── rust-toolchain.toml  # 버전 고정 + 승급 정책 주석
├── clippy.toml          # disallowed-methods
├── rustfmt.toml · LICENSE(Apache-2.0) · README.md
└── crates/fleetwire/src/
    ├── lib.rs · client.rs · auth.rs · error.rs · scope.rs · pricing.rs · redact.rs
    └── api/
        ├── mod.rs        # FleetGet trait (sealed)
        └── vehicles.rs(12) users.rs(4) charging.rs(3) energy.rs(6) partner.rs(3)
```

- **cargo workspace 이되 crate 는 하나.** 명령 계층(C)이 올 때 `crates/fleetwire-vcp` 추가로 끝난다.
- 요청 struct 와 응답 타입을 **같은 파일**에 (grok-build `rpc/{git,fs,…}.rs` 관례).
- **`build.rs` 를 만들지 않는다** — 공급망 공격 표면이고 필요 없다.

### 읽기 전용 강제 — 3계층
1. **타입 시스템(본진)**: 공개 API 에 POST 생성 경로가 없다. `send_raw` 도 GET 고정.
2. **모듈 가시성**: raw `reqwest` 접근을 `pub` 로 노출하지 않는다.
3. **clippy(보조)**:
```toml
# clippy.toml
disallowed-methods = [
  { path = "reqwest::Client::post",   reason = "fleetwire 는 GET 전용" },
  { path = "reqwest::Client::put",    reason = "동일" },
  { path = "reqwest::Client::delete", reason = "동일" },
  { path = "reqwest::Client::request", reason = "메서드 임의 지정 우회 방지" },
]
```
```toml
# Cargo.toml — 린트를 실제로 켜야 효력이 있다
[workspace.lints.clippy]
disallowed_methods = "deny"
```
예외는 `auth.rs` 의 토큰 refresh(POST) **한 곳**만 `#[allow]` + 사유 주석.

> ⚠️ **clippy 는 보조 방어선이다.** 다른 HTTP crate 추가·`allow` 확산으로 우회 가능.
> 본진은 1·2 이고 clippy 는 실수 방지용임을 문서에 명시한다.

## 4. 요청/응답 타이핑

```rust
pub trait FleetGet: Sealed + Send + Sync {
    type Response: DeserializeOwned;
    fn required_scopes(&self) -> ScopeExpr;   // const 아님 — 파라미터 의존
    fn pricing(&self) -> Pricing;             // 기본값 없음 — 필수 명시(fail-safe)
    fn path(&self) -> String;
    fn query(&self) -> QuerySpec;             // 문자열 아님 — 인코딩·주입 방지
}

/// state 게이트 가능한 엔드포인트만 구현 → 컴파일타임 보장
pub trait VehicleEndpoint: FleetGet { fn vin(&self) -> &str; }
```

**기본값을 두지 않는다.** `PRICING = Free` 기본값은 fail-unsafe — 새 엔드포인트에서 빠뜨리면
유료 호출이 무료로 표시되고 게이트가 작동하지 않는다.

**`query()` 는 문자열이 아니라 타입**이다. 직접 조립하면 percent-encoding 누락·중복 키·
쿼리 주입이 생긴다. `path()` 도 세그먼트 단위 인코딩 + `../` 차단.

### 발견성 — 편의 메서드는 필수
```rust
impl Client {
    pub async fn vehicle_data(&self, tag: &str) -> Result<VehicleDataResp, Error> {
        self.send(&VehicleData::new(tag)).await   // 위임만
    }
}
```
Rust 커뮤니티가 **"과한 제네릭은 에러 메시지를 망친다"** 를 흔한 실수로 꼽는다. 대부분의
사용자가 trait 을 직접 다루지 않도록 카테고리별 편의 메서드를 **필수로** 제공한다.

### serde 정책
```rust
#[derive(Debug, Clone, Deserialize, Default)]
#[serde(default)]
pub struct ChargeState {
    pub battery_level: Option<i64>,
    #[serde(flatten, skip_serializing_if = "Map::is_empty")]
    pub extra: Map<String, Value>,
}
```
- **`deny_unknown_fields` 미사용.** serde 공식문서가 `flatten` 과의 조합을 **미지원**으로 명시
  (`serde.rs/container-attrs.html`, 이슈 `serde-rs/serde#2384`). 로컬 실험에서는 의도대로
  동작했으나(2026-07-24) **보장되지 않는 동작 위에 드리프트 감지를 얹지 않는다.**
- **`strict` feature 를 만들지 않는다** — Cargo feature unification 으로 downstream 런타임까지
  엄격해져 사용자 앱이 깨질 수 있다.
- 드리프트 감지는 **§8 라이브 캡처**로 완전히 분리한다.

### 봉투·예외·페이지네이션
- **봉투**: `client.rs` 가 **status 와 Content-Type 을 먼저 판별한 뒤** 해체.
  모든 body 를 같은 봉투로 가정하면 PDF·비정형 오류에서 깨진다.
- **invoice PDF**: trait 밖 별도 메서드 `charging_invoice_pdf(&self, id) -> Result<Bytes, Error>`.
  MIME 검증 + 크기 상한 적용.
- **페이지네이션**: `Page<T>` + 다음 커서 노출. 자동 전체 순회는 **opt-in**
  `all_pages(max_pages)` 로만, doc 에 과금 경고.
- **응답 타입 공유**: 공유 전 **필드 패리티 검증 필수**(O5). 검증 전에는 개수를 확정하지 않는다.

## 5. 인증·동시성

### 토큰 갱신 순서
```
on 401(분류 후) 또는 expires_soon:
  1. in-process single-flight 획득 (tokio::sync::Mutex, .await 가로질러 보유, 취소 정책 포함)
  2. 고정 파일 ~/.tesla/tokens.lock 에 flock(LOCK_EX) — 타임아웃 있음
  3. 락 아래에서 토큰 파일 RE-READ        // 형제가 이미 갱신했을 수 있음
  4. access 가 아직 유효 → Adopted, refresh 없이 반환
  5. 아니면 refresh 1회 → 새 refresh_token 있으면 **즉시 persist**
     (락 보유 중 disk-first: write .tmp → fsync → rename → dir fsync, mode 0600, symlink 방어)
  6. flock 해제 → mutex 해제
  7. 원 요청을 새 access token 으로 **1회만** 재시도
```

**🔴 락 파일을 절대 삭제하지 않는다.**
flock 은 프로세스 종료 시 **커널이 자동 해제**한다. 죽은 holder 처리는 불필요하다.
잠긴 파일을 unlink 하면 **기존 holder 와 새 inode holder 가 동시에 존재해 상호배제가 깨진다**
(grok-build 는 unlink-to-break 를 쓰지만 이 위험을 감수하는 선택이다. 우리는 안 쓴다).
타임아웃 시 **에러로 종료**하거나 대기하되, **inode 를 교체하지 않는다.**
PID 생존 검사도 신뢰하지 않는다(PID 재사용·PID namespace·권한 문제).

**`OnceCell`/`OnceLock` 을 토큰에 쓰지 않는다** — "한 번 초기화"이지 "매시간 한 번"이 아니다.
리전 base URL 캐시에만 사용한다.

**401 을 무조건 refresh 로 해석하지 않는다** — scope·revocation·audience 오류일 수 있다.
분류 후 refresh 는 **1회로 제한**한다.

### 락 소유권
```rust
pub trait TokenStore: Send + Sync {
    async fn get(&self) -> Result<Tokens, Error>;
    /// CAS 계약: `expected` 와 현재 저장값이 다르면 Err(Conflict) — 형제 갱신 감지용
    async fn compare_and_swap(&self, expected: &Tokens, new: Tokens) -> Result<(), Error>;
}
pub struct FileTokenStore { … }    // 기본: 파일 + flock
pub struct MemoryTokenStore { … }  // 단일 프로세스·테스트용
```
`get`/`refresh` 만으로는 DB·시크릿매니저 backend 의 트랜잭션 계약을 표현할 수 없다.
**CAS 를 계약에 포함**한다. 여러 호스트가 같은 토큰을 공유하는 경우는 **명시적 non-goal**이다.

### HTTP 계층
```rust
// 장수 공유 + clone. connect 10s · 요청 30s · pool_max_idle_per_host(2) · pool_idle_timeout(90s)
// 재시도 체인 전체를 tokio::time::timeout 으로 감싼다 (reqwest.timeout() 은 체인을 못 막음)
```
- 프로세스 전역 `static` 대신 **`Client` 인스턴스가 소유**한다 — proxy/TLS/테스트 구성을 막지 않기 위해.
- **응답 크기 상한**: `bytes_stream()` 누적 검사. buffer-then-check 금지. JSON·PDF·오류 body **전부**.
- 타임아웃 수치는 초기값이며 실측으로 조정한다.

### 계승 불변식 5개
| # | 불변식 |
|---|---|
| I1 | refresh 회전 시 **즉시 저장**(실패 = 재-OAuth) |
| I2 | refresh 는 `fleet-auth.prd.vn.cloud.tesla.com`, 로그인은 `auth.tesla.com` (⚠️ OIDC discovery 로 재확인 필요) |
| I3 | 리전 base URL **캐시 + NA 폴백** (⚠️ 잘못된 리전 전송 위험 — 폴백 안전성 재확인 필요) |
| I4 | 토큰 파일 **0600 + 원자적 쓰기** + fsync + dir fsync + symlink 방어 |
| I5 | 408 을 에러로 올리되 상위가 흡수 가능 |

## 6. 에러·과금 판정

```rust
pub enum Error {
    /// 요청이 전송되지 않았음이 확실 — DNS·connect 실패
    TransportNotSent { source: reqwest::Error },
    /// 전송됐으나 처리 여부 불명 — read timeout·connection reset
    TransportUnknown { source: reqwest::Error },
    Api { status: u16, message: String, retry_after: Option<Duration>, body_preview: String },
    VehicleOffline,                                              // 408
    MissingScope { required: ScopeExpr, held: Vec<Scope> },
    Decode { source: serde_json::Error, status: u16, body_preview: String },
    Auth(String),
}

pub enum Billing { Billed, NotBilled, Unknown }

impl Error {
    pub fn billing(&self) -> Billing {
        match self {
            Error::TransportNotSent { .. } | Error::MissingScope { .. } => Billing::NotBilled,
            Error::TransportUnknown { .. }                              => Billing::Unknown,
            Error::Api { status, .. } if *status >= 500                 => Billing::NotBilled,
            Error::Api { .. } | Error::VehicleOffline | Error::Decode { .. } => Billing::Billed,
            Error::Auth(_)                                              => Billing::Unknown,
        }
    }
    /// 재시도 판정 — 단 한 곳. **과금 불명이면 재시도하지 않는다.**
    pub fn is_retryable(&self) -> bool {
        match self {
            Error::TransportNotSent { .. } => true,
            Error::Api { status, .. } => matches!(status, 500 | 502 | 503 | 504),
            _ => false,   // Unknown·429·408·4xx 전부 재시도 안 함
        }
    }
    pub fn retry_after(&self) -> Option<Duration>;
}
```

**핵심 정정**: "전송 실패 = 무과금 = 재시도 안전" 은 **틀렸다**. read timeout·connection reset 은
서버가 이미 처리·과금한 뒤일 수 있다. `TransportNotSent` 와 `TransportUnknown` 을 분리하고
**Unknown 은 재시도하지 않는다** — 재시도가 곧 중복 과금이기 때문이다.
(근거: 종량제 API 중첩 재시도로 청구가 2배가 된 실사례 다수.)

5xx 도 전부가 아니라 **일시 오류(500·502·503·504)** 만 재시도한다. 501·505 는 재시도 의미 없음.
기본 `max_attempts = 1`. exponential + jitter, cap 30s. `Retry-After` **120초 상한**.

### 스코프 선검사 — 참고용(advisory)
```rust
pub enum ScopeExpr { All(&'static [Scope]), Any(&'static [Scope]), One(Scope) }
```
- `vehicle_data` 는 `include_location` 에 따라 필요 스코프가 달라지므로 **메서드**여야 한다.
- ⚠️ **access token 이 항상 JWT 라는 보장이 없고, 서명 미검증 payload 는 권위가 없다**.
  → scp 선검사는 **불필요한 유료 호출을 줄이는 최적화**일 뿐, 권한의 근거가 아니다.
  최종 판정은 서버의 403 이다. 파싱 실패 시 **막지 말고 통과**시킨다(fail-open).

### 비용 게이트
```rust
pub enum Guarded<T> { Executed(T), Skipped { connectivity: VehicleConnectivity } }
pub async fn send_guarded<R: VehicleEndpoint>(&self, req: &R) -> Result<Guarded<R::Response>, Error>;
```
⚠️ doc 에 명시: **게이트는 비용을 줄이지 제거하지 않는다.** state 확인과 실제 호출 사이 TOCTOU 존재.
state 조회 자체의 과금 카테고리는 **미확정(O4)** 이므로 절감폭은 실측 전까지 확정하지 않는다.

### 호출 순서 (비용 낮은 검사부터)
```
1. required_scopes() vs 토큰 scp   → MissingScope (💰 0, advisory)
2. [guarded] GET /vehicles/{vin}   → Skipped (💰 state 조회분)
3. 실제 호출                        → 💰 pricing()
4. 실패 시 is_retryable()           → NotBilled 인 것만 재시도
5. 모든 에러 메시지 → redact()
```

### redaction
`Bearer`/JWT/`refresh_token`/`client_secret` → `[REDACTED]` · VIN → `[VIN]` · 좌표 → `[COORD]`.
- **구조화 redaction 우선**: JSON 은 키 allowlist 기반으로 처리하고 regex 는 보조로만 쓴다
  (좌표 포맷·URL 인코딩·중첩 JSON 을 regex 로 다 잡을 수 없다).
- `reqwest::Error` 의 URL·source chain, `Debug` 출력, tracing 필드까지 포함해야 한다.
- 근거: OSS 사용자는 에러 메시지를 GitHub 이슈에 붙여넣는다.

## 7. 엔드포인트 증거 매트릭스 (구현 전 완성 필요)

각 엔드포인트마다 아래를 기록한다. **이 표가 완성되기 전에는 U1(전체 강타입)을 확정으로 보지 않는다.**

| 열 | 내용 |
|---|---|
| method · path | 공식문서 원문 |
| required scope | F6 기준, AllOf/AnyOf |
| pricing category | Data/Free/Wake — 엔드포인트 문서 확장 UI 확인 필요 |
| rate-limit bucket | F4 |
| content type | JSON / PDF |
| pagination | 유무·방식 |
| 검증 상태 | verified(실응답) / unverified(문서 추측) |
| 근거 | 문서 URL + 조회 시각, 또는 캡처 시각·펌웨어 |

현재 알려진 것:
- **verified 가능**: vehicle 12 + user 4 = **16** (현 스코프)
- **파트너 토큰 필요**: partner 3
- **스코프 미보유**: charging 3 (F6 — `vehicle_charging_cmds`)
- **하드웨어 미보유**: energy 6
- **개인 계정 불가**: `/dx/charging/sessions` (F7)

## 8. 테스트와 드리프트 감지

| 계층 | 위치 | 비용 | 지키는 것 |
|---|---|---|---|
| 유닛 | 공개 CI | $0 | 로직·불변식·스코프·redaction |
| fixture 계약 | 공개 CI | $0 | **파싱 회귀만**(드리프트 감지 아님) |
| **라이브 캡처** | 토큰 보유 환경 | ~$0.14/월 | **실제 스키마 드리프트** |

> ❗ **정적 fixture 로는 드리프트를 감지할 수 없다.** 커밋된 fixture 를 커밋된 스냅샷과 비교하면
> 영원히 자기 자신과 일치한다. 감지는 **라이브 응답**이 있어야만 성립한다.

### 라이브 캡처
```
1. 라이브 가능한 16개 GET 호출 — 여러 차량 상태에서(주차·주행·충전)
   ※ 한 상태만 캡처하면 조건부 필드·배열 variant·nullability 를 놓친다
2. raw JSON → 구조화 정제(VIN/좌표/id/display_name) → fixtures/live/<endpoint>/<state>.json
3. 프로덕션 타입 역직렬화 + **구조 지문(fingerprint) diff**
   ※ 키셋만 비교하면 타입·null 허용·enum·배열 원소 구조 변화를 놓친다
4. 차이 발견 → 이슈 생성   ❗ 조용히 Option 붙이고 넘어가지 말 것
비용: 16 × $0.002 × 4.33주 ≈ 월 $0.14
```
- 정제기 **자체에 테스트**. 정제 누락 = 개인정보 커밋.
- 정제 누락 외 유출 경로도 막는다: 원본 임시 파일 삭제, CI artifact 미업로드, 이슈 본문 정제.
- 실행 위치는 **토큰이 있는 환경**이면 된다. 회사 자산에서 개인 차량을 상시 폴링하지 않기 위해
  클라우드 이전(E) 후 VM 에서 돌리는 것이 **정책상 선호**이지 기술적 필수는 아니다.

### 검증 수준을 crate 문서에 명시
```rust
//! ## 검증 수준
//! - ✅ verified: 실차량 응답으로 검증 (vehicle 12, user 4) — 캡처 시각·펌웨어 기록
//! - ⚠️ unverified: 공식문서 스키마 기반 **추측** (charging 3, energy 6). 필드가 틀릴 수 있음
//! - partner 3: 파트너 토큰 발급 시 검증 가능
```

### 필수 테스트
```
serde_absorbs_unknown_field_into_extra    // flatten 동작이 serde 업그레이드로 바뀌면 감지
serde_default_fills_missing_with_none
concurrent_refresh_single_flight           // N 태스크 → refresh 1회
cross_process_lock_adopts                  // 프로세스 2개 → Adopted
lock_timeout_errors_without_unlink         // 타임아웃 시 inode 교체 안 함
billing_classification_matrix              // 각 Error → Billing 매핑
retry_never_on_unknown_billing             // TransportUnknown 은 재시도 안 함
redact_removes_vin_coords_tokens
I1~I5 불변식 (I5 는 billing()==Billed 단정 포함)
```

## 9. 릴리스 준비 (Rust 커뮤니티 관례)

```bash
cargo clippy -- -D warnings     # 경고를 에러로
cargo semver-checks             # 공개 API 를 실수로 깨는지 자동 검사
cargo deny check                # 의존성 취약점·라이선스
cargo test --locked             # 재현 가능한 빌드
```
```toml
[package.metadata.docs.rs]      # docs.rs 빌드 보장 — 깨진 docs.rs 는 신뢰성 실패
all-features = true
```
- **MSRV 선언 + CI job**
- **`Cargo.lock` 커밋**(라이브러리도)
- **`build.rs` 없음**
- 근거: Rust API Guidelines `C-SEALED` 등 · Cargo Book SemVer 호환성 · docs.rs metadata

## 10. 미결·전제

| # | 항목 | 해소 시점 |
|---|---|---|
| O1 | 엔드포인트 증거 매트릭스(§7) 완성 | **구현 착수 전** |
| O2 | partner 3 — 파트너 토큰 발급 후 검증 | 별도 |
| O3 | `/api/1/products` 의 실제 스코프 | 실측 |
| O4 | `GET /vehicles/{vin}` 의 Pricing Category → 게이트 절감폭 확정 | 사용량 대시보드 |
| O5 | 응답 타입 공유 후보의 필드 패리티 | 구현 중 |
| O6 | I2(auth 호스트) OIDC discovery 재확인 · I3(NA 폴백) 안전성 | 구현 전 |
| O7 | Tesla 의 refresh token 재사용 탐지 정책 — 현재 근거는 타사 사례뿐 | 미해결 |

## 11. Non-goals

- MCP 서버(B) · 명령 72개(C) · Fleet Telemetry(D) · 클라우드 인프라(E)
- 자동 전체 페이지 순회 기본 활성화 (opt-in 만)
- 여러 호스트가 같은 토큰 파일을 공유하는 구성
- 멀티 차량·플릿 규모 최적화
- 재시도 기본 활성화

## 12. 검증 이력

| 라운드 | 도구 | 잡아낸 것 |
|---|---|---|
| 실험 | 로컬 cargo | `flatten`+`deny_unknown_fields` 는 오늘 동작하나 **serde 공식 미지원** → 채택 안 함 |
| 외부 | tech-insight-search | serde 공식 근거 · `reqwest.timeout()` 재시도 미차단 · 종량제 중복과금 실사례 · 라이브 캡처 레시피 · Rust OSS 관례 |
| 소스 | grok-build `69f0ba8` | flock+PID · disk-first persist · `Adopted` · poisoned pool 복구 · **redaction 미비**(반면교사) |

> 참조 구현을 그대로 베끼지 않았다. grok-build 의 unlink-to-break·무제한 error body·
> 자동 redaction 부재는 **채택하지 않은** 항목이다.

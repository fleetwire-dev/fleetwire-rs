# fleetwire A4 — 엔드포인트 완성 · 페이지네이션 · 비용 게이트 설계 스펙

> 서브프로젝트 A(읽기 라이브러리)의 네 번째 조각. A3 가 만든 전송 계층 위에 **나머지 13개
> 엔드포인트**를 얹고, **첫 유료 호출**을 도입하며, 페이지네이션과 비용 게이트를 완성한다.
>
> | | |
> |---|---|
> | 코드 레포 | `<workspace>` branch `main`, BASE `4d2d33a` (A3 완료, 316 테스트) |
> | 상위 스펙 | `2026-07-24-fleetwire-read-client-design.md` (§4 타이핑·§6 게이트·§11 non-goals) |
> | 직전 이월 | `2026-08-03-fleetwire-a3-carry-forward.md` |
> | 증거 매트릭스 | `2026-07-29-fleetwire-o1-endpoint-evidence-matrix.md` |
> | 작성 | 2026-08-03 KST |

---

## 0. A4 가 끝났을 때 참인 것

1. 라이브 검증 가능한 **16개 엔드포인트 전부**를 강타입으로 호출할 수 있다.
2. 목록 응답의 **페이지네이션 메타데이터가 호출자에게 도달**한다(현재는 소실된다).
3. **첫 유료 호출**이 들어오고, 그 비용이 타입(`Pricing`)·게이트(`send_guarded`)·관측
   (`RateLimit`)의 세 수단으로 다뤄진다.
4. 408 이 **실측된 형태**로 분류된다 — 더 이상 문서 근거가 아니다.
5. read-only 불변식 유지: POST 는 여전히 `auth/refresh.rs` 한 곳뿐.

---

## 1. 이 설계를 떠받치는 실측 증거

⚠️ **2026-08-03 KST 실측.** 차량 = jsh Model 3(`LRW…`), 측정 시점 **state = offline(수면)**.
총 비용 ≈ **$0.01**(`vehicle_data` 5회, 전부 408이나 과금됨). 원본은 §12 부록.

### E1. `pagination` 은 `response` 의 형제 키다 — 그리고 `count` 도 있다

`GET /api/1/vehicles` → 200:

```
최상위 키: ["count", "pagination", "response"]
pagination: {"previous":null,"next":null,"current":1,"per_page":1,"count":1,"pages":1}
```

- ✅ `pagination` 의 6개 필드가 `model.rs::Pagination` 과 **정확히 일치**한다.
- ⚠️ **최상위 `count` 가 따로 있다.** `Envelope` 도 `Pagination` 도 모델링하지 않았고 O1 에도 없다.
- ⚠️ 파라미터 없이 호출해도 `per_page: 1` — O1 의 "기본 100" 과 어긋난다. 차량이 1대뿐이라
  **판별 불가**이며 단정하지 않는다(**O12**).

**함의**: `decode()` 는 `Envelope<T>{response}` 만 벗기므로 `pagination`·`count` 가 **전부 소실**된다.
`Page<T>`/`Pagination` 이 A3 이후 소비자 0개인 근본 원인이며, `model.rs` 의
"호출자가 두 조각으로 조립한다"는 doc 은 **도달하지 않는 조각**을 전제하고 있었다.

### E2. 408 의 실제 형태 — O10 의 408 절반 해소

`GET /api/1/vehicles/{vin}/vehicle_data`, 차량 offline:

```
HTTP 408 · content-type: application/json; charset=utf-8 · 142 bytes
{"response":null,"error":"vehicle unavailable: vehicle is offline or asleep",
 "error_description":"","txid":"432e7faa…"}
```

- ⚠️ **408 은 `{"response": …}` 봉투를 쓴다**(`response: null`). A1 `decode` 의 status-first 순서가
  여기서도 load-bearing이다 — content-type 을 먼저 봤다면 봉투 파싱으로 내려갔을 것이다.
- ⚠️ **`error_description` 이 키로 존재하되 빈 문자열**이다. "키가 있으면 쓴다"는 폴백은
  **빈 메시지**를 낳는다. A3 는 body `error` 를 1순위로 둬서 우연히 안전하지만
  **빈 문자열을 건너뛰는 규칙이 없다**.
- 🆕 `txid`(바디) · `x-txid`(헤더) · `x-request-id`(헤더) 셋이 함께 온다.

### E3. 🆕 rate-limit 잔량이 헤더로 온다 (현재 전량 폐기 중)

```
ratelimit-device-data-per-vehicle-limit: 60
ratelimit-device-data-per-vehicle-remaining: 59
ratelimit-device-data-per-vehicle-reset: 9
```

연속 3회 호출로 창 롤오버를 배제하고 측정:

| 호출 | remaining | reset |
|---|---|---|
| 1 | 59 | 57 |
| 2 | 58 | 56 |
| 3 | 57 | 55 |

⚠️ **위 표의 3회는 §1 첫 예시(`reset: 9`)와 다른 시점의 측정이다.** 같은 실측으로 읽으면 수치가
충돌한다. 3회 모두 **408** 이었다.

**실측이 지지하는 것** (이 이상 단정하지 않는다):
- `remaining` 이 **호출당 1씩 감소**한다. 3회 연속 408 에서 59→58→57.
- 따라서 **408 도 쿼터를 소모한다** — 단 이 측정 동안 다른 호출자가 없었다는 전제 위에서다.
- `limit = 60`, 키는 **차량당**(헤더 이름이 `per-vehicle`).
- `reset` 이 초 단위로 **감소**한다(57→56→55).

**실측이 지지하지 **않는** 것** — 가설로 남긴다:
- 창의 종류. 3개 표본으로는 **fixed / sliding / token-bucket 을 구분할 수 없다**.
- `reset` 의 단위·기준시각·경계 동작. 감소는 봤지만 0 에서 무슨 일이 나는지 안 봤다.
- 계정 전역 공유 여부. 헤더는 per-vehicle 을 말하지만 O1 은 계정 공유라 한다 — **미검증**(O16).

→ 그럼에도 설계에 필요한 결론은 성립한다: **수면 차량 폴링은 돈과 쿼터를 동시에 태운다.**

### E4. 비용 게이트는 진짜 무과금이다 — O4 해소

`GET /api/1/vehicles/{vin}` → 200:

- **`ratelimit-*` 헤더가 하나도 없다** → device_data 쿼터 카테고리가 아니다.

⚠️ **"무과금 확정"이라 쓰지 않는다.** 헤더 부재는 쿼터 카테고리에 대한 증거이지 **청구에 대한
증거가 아니다**. 초안은 이를 "O4 해소"로 적었는데, 같은 문서가
O9(무인증 요청 과금 여부)는 대시보드가 필요하다고 하면서 여기서는 헤더만으로 단정하는
**이중 잣대**였다.

정확한 표현: **헤더 부재 + O1 매트릭스의 `pricing_category: null` 이 서로를 보강하는 강한 정황**이다.
확정은 사용량 대시보드 대조가 필요하며, 그것이 O4 의 원래 해소 조건이었다. **O4 는 열어 둔다.**
- 최상위 키는 `["response"]` 뿐(페이지네이션 없음).
- `response.state = "offline"` — 게이트가 필요로 하는 값을 정확히 준다.
- ⚠️ 응답에 **`backseat_token`** 필드가 있다 — redaction 키 allowlist 점검 대상.

**→ `send_guarded` 가 실측으로 정당화된다**: 무과금 1회로 $0.002 + 쿼터 1슬롯을 아낀다.

---

## 2. 페이지네이션 — `Envelope` 를 형제 인식형으로

### 2.1 구조

```rust
// model.rs — crate-internal
pub(crate) struct Envelope<T> {
    pub response: T,
    #[serde(default)] pub pagination: Option<Pagination>,
}

// decode.rs — crate-internal
pub(crate) struct Decoded<T> {
    pub payload: T,
    pub pagination: Option<Pagination>,
}
```

⚠️ **최상위 `count` 는 모델링하지 않는다.** 실측에서 최상위 `count` = `pagination.count` = 1 로
**같은 값**이었다. `Pagination` 이 이미 `count` 를 갖고 있으므로 별도 필드를 두면
`Page.count` 와 `Page.pagination.count` 라는 **이름이 같고 값도 같은 두 필드**가 생긴다.
serde 는 모르는 키를 기본적으로 무시하므로 포착하지 않아도 파싱은 깨지지 않는다.

⚠️ 다만 이는 **1건 표본**이다(차량 1대). 둘이 갈릴 수 있다는 증거가 나오면 그때 추가한다 —
지금 넣으면 근거 없이 헷갈리는 공개 필드를 하나 만드는 것이다. **O13** 으로 기록한다.

`decode()` 가 `Decoded<T>` 를 반환하고, `Client::send` 가 엔드포인트의 `Response` 타입에 맞춰 조립한다.

### 2.2 `Response` 타입이 페이지네이션 유무를 말한다 — 조립 계약이 필요하다

⚠️ **초안은 컴파일되지 않았다**. wire 위의 `response` 는 `Vec<T>` 인데 공개
`Response` 는 `Page<T>` 다. 제네릭 `send` 는 **무엇으로 역직렬화할지 알 방법이 없었다** —
`Decoded<T>` 를 반환해도 `T` 가 결정되지 않는다.

→ `FleetGet` 에 **wire 타입과 조립 함수를 명시적 계약으로** 넣는다:

```rust
pub trait FleetGet: crate::sealed::Sealed + Send + Sync {
    /// 봉투의 `response` 에서 직접 역직렬화되는 타입.
    type WireResponse: DeserializeOwned;
    /// 호출자가 받는 타입.
    type Response;
    /// wire 값 + 형제 메타데이터 → 공개 타입.
    fn assemble(wire: Self::WireResponse, pagination: Option<Pagination>) -> Self::Response;
    // … 기존 required_scopes / pricing / path / query
}
```

- 페이지네이션 없는 엔드포인트: `WireResponse = Response`, `assemble` 은 wire 를 그대로 반환.
  기본 구현을 줄 수 없으므로(연관타입이 같다는 걸 컴파일러가 모른다) **매크로 하나로 보일러플레이트를
  줄인다** — 13개 중 11개가 이 형태다.
- 페이지네이션 있는 엔드포인트: `WireResponse = Vec<T>`, `Response = Page<T>`,
  `assemble` 이 `Page { items: wire, pagination: … }` 을 만든다.

```rust
impl FleetGet for ListVehicles {
    type WireResponse = Vec<VehicleSummary>;
    type Response = Page<VehicleSummary>;      // 변경 (A3 는 Vec 였다)
}
impl FleetGet for GetVehicle {
    type WireResponse = VehicleSummary;
    type Response = VehicleSummary;            // 유지
}
```

⚠️ `assemble` 이 **연관 함수(`Self::` 수신자 없음)** 인 이유: 조립은 요청 인스턴스의 값에 의존하지
않는다. `&self` 를 받으면 구현자가 요청 상태를 응답에 섞을 수 있고, 그건 이 계층의 책임이 아니다.

⚠️ **`ListVehicles::Response` 가 `Vec<VehicleSummary>` → `Page<VehicleSummary>` 로 바뀐다.**
A3 공개 API 의 breaking 변경이지만 `publish = false` 라 **지금은 공짜, publish 후에는 breaking** 이다.

`Page<T>` 는 현재 형태를 유지한다 — 필드를 더하지 않는다:

```rust
#[non_exhaustive]
pub struct Page<T> {
    pub items: Vec<T>,
    pub pagination: Pagination,
}
```

바뀌는 것은 **채워진다는 사실**이다. A3 까지 `Page<T>` 는 소비자가 없었고 `model.rs` 의
"호출자가 두 조각으로 조립한다"는 doc 은 도달하지 않는 조각을 전제했다. 그 doc 도 함께 고친다.

⚠️ **`pagination` 이 없으면 `Option` 으로 남긴다 — 기본값으로 채우지 않는다.**

```rust
#[non_exhaustive]
pub struct Page<T> {
    pub items: Vec<T>,
    /// `None` = 서버가 메타데이터를 **주지 않았다**. 전 필드가 `None` 인 `Pagination` 과 다르다.
    pub pagination: Option<Pagination>,
}
```

초안은 전 필드 `None` 인 기본값으로 채우려 했으나 검토에서 지적됐다:
그러면 **"서버가 메타데이터를 생략함"과 "메타데이터가 실제로 전부 null 임"을 구분할 수 없고**,
호출자가 `next == None` 을 "다음 페이지 없음"으로 읽어 **순회를 조기 종료**할 수 있다.
`Option<Pagination>` 이 정직하고, 실패시키지 않는다는 원래 의도도 그대로 지킨다.

### 2.3 자동 순회는 만들지 않는다

상위 스펙 §11 의 non-goal 이다. `Page` 는 `pagination.next` 를 노출할 뿐, 순회는 호출자 몫이다.
편의 메서드(`next_page()` 등)도 A4 범위 밖 — 넣으면 "opt-in 만" 조항이 사실상 무력화된다.

---

## 3. 엔드포인트 13개

O1 매트릭스 기준. **A3 가 이미 한 3개**(`vehicles` 목록 · `vehicles/{vin}` · `users/region`)는 제외.

### 3.1 유료 2개 — `Pricing::data()`

| 엔드포인트 | 파라미터 | 스코프 |
|---|---|---|
| `GET /api/1/vehicles/{vin}/vehicle_data` | `endpoints=`(하위 데이터 선택) | `vehicle_device_data` (+위치 요청 시 `vehicle_location`) |
| `GET /api/1/vehicles/{vin}/nearby_charging_sites` | `count`·`radius`·`detail` (⚠️ page 아님) | `vehicle_device_data` |

**`vehicle_data` 의 조건부 스코프**가 이 태스크의 핵심 설계 지점이다:

```rust
pub struct GetVehicleData { vin: String, endpoints: Vec<DataEndpoint>, }

impl FleetGet for GetVehicleData {
    fn required_scopes(&self) -> ScopeExpr {
        if self.endpoints.iter().any(|e| e.needs_location()) {
            ScopeExpr::All(&[Scope::VehicleDeviceData, Scope::VehicleLocation])
        } else {
            ScopeExpr::One(Scope::VehicleDeviceData)
        }
    }
}
```

A1 이 `required_scopes()` 를 **상수가 아니라 메서드**로 둔 이유가 정확히 이것이다.
⚠️ 펌웨어 2023.38+ 는 `location_data` 를 **명시하지 않으면 위치를 생략**한다(F9) — 이중 opt-in이다.

`DataEndpoint` 는 문자열 자유입력이 아니라 **열거형**으로 둔다. 자유입력은 오타를 유료 요청으로 바꾼다.

### 3.2 파라미터 있는 무과금 1개

`GET /api/1/vehicles/{vin}/invitations` — `page`·`per_page`(최대 25). `type Response = Page<Invitation>`.

### 3.3 단순 10개 — path + `Response` 타입만

`mobile_enabled` · `recent_alerts` · `release_notes` · `service_data` · `drivers` ·
`fleet_telemetry_config` · `fleet_telemetry_errors` · `users/me` · `users/orders` · `users/feature_config`

전부 `Pricing::NotCharged`, 파라미터 없음, 페이지네이션 없음.
⚠️ user 계열 4개는 O1 정정대로 스코프가 `user_data` 류다 — `openid` 가 아니다.

### 3.4 응답 타입 정책

A1 의 U2(런타임 관대)를 그대로 잇는다: `Option` + `#[serde(default)]` + `#[serde(flatten)] extra`,
`deny_unknown_fields` **미사용**. `#[non_exhaustive]`.

⚠️ **검증하지 못한 필드를 강타입으로 내보내지 않는다**(상위 스펙의 근본 규칙). 실응답을 못 본
엔드포인트는 응답 본문을 `serde_json::Value` 로 두고 그 사실을 rustdoc 에 적는다 — 추측한 필드
이름을 공개 API 로 만드는 것보다 정직하다. 어느 엔드포인트가 그런지는 구현 중 실측으로 확정한다.

---

## 4. `send_guarded` — 비용 게이트

```rust
#[non_exhaustive]
pub enum Guarded<T> {
    Executed(T),
    Skipped { state: String },
}

impl Client {
    pub async fn send_guarded<R: VehicleEndpoint>(&self, req: &R)
        -> Result<Guarded<R::Response>, Error>;
}
```

1. `GET /api/1/vehicles/{vin}` (무과금 — E4 실측) → `response.state`
2. `state != "online"` ⇒ `Skipped { state }` 반환, **유료 호출 없음**
3. `online` ⇒ 실제 호출 → `Executed`

⚠️ **`VehicleEndpoint` 로만 제한한다.** VIN 이 없으면 게이트할 대상이 없다. 유료 2개 모두
`/vehicles/{vin}/…` 경로라 커버된다.

⚠️ **doc 에 TOCTOU 를 명시한다**: 게이트는 비용을 **줄이지 제거하지 않는다.** 상태 확인과 실호출
사이에 차가 잠들 수 있고, 그러면 408 이 나며 그 408 은 과금된다(E3).

⚠️ **게이트를 자동으로 걸지 않는다.** `send()` 는 그대로 두고 `send_guarded()` 를 별도로 둔다 —
호출자가 모르는 사이 추가 요청이 나가는 것이 더 나쁘다.

⚠️ **게이트가 항상 이득인 것은 아니다.** 차가 `online` 이면 게이트는 **순수한 추가 왕복**이다
(무과금이지만 지연은 늘고 rate limit 창도 다르다). 절감은 차가 자고 있을 때만 생긴다.
초안은 "$0.002 절감"만 적어 이 긴장을 감췄다 — rustdoc 에 **양쪽을 다** 적는다:
자주 자는 차량에는 이득, 상시 online 이면 손해. 어느 쪽인지는 호출 패턴이 정하며 그 판단은
라이브러리가 아니라 호출자의 것이다.

### 4.1 게이트 자체가 실패하면 — **fail-closed**

초안에 정의가 없었다. 게이트 호출이 네트워크 오류·401·5xx 로 실패하면
**유료 호출을 강행하지 않고 그 에러를 그대로 올린다.**

근거: 게이트의 존재 이유가 "모르면 돈 쓰지 말자"인데, 상태를 **모르게 된** 상황에서 강행하면
게이트가 없는 것보다 나쁘다(실패한 게이트 요청 + 유료 요청). 호출자가 강행을 원하면
`send()` 를 직접 부르면 된다 — 그게 두 메서드를 나눈 이유다.

### 4.2 `state` 문자열을 열거형으로 좁히지 않는다

게이트 판정은 `state == "online"` **하나만** 본다. `asleep`·`offline`·`in_service` 등을 열거형으로
모델링하지 않는 이유: 서버가 새 상태를 추가하면 열거형은 파싱 실패하거나 `Unknown` 으로 접히는데,
게이트에 필요한 정보는 "온라인인가 아닌가" 뿐이다. `Guarded::Skipped { state: String }` 로
**관측한 문자열을 그대로** 넘겨 호출자가 판단하게 한다.

⚠️ 이는 `state` 문자열이 안정적이라는 **전제**다. `"online"` 표기가 바뀌면
게이트가 전부 `Skipped` 로 접힌다 — 조용한 실패가 아니라 **비용이 0이 되는 방향**이라 안전 방향이고,
`Skipped` 에 실린 문자열이 진단을 준다. 이 판단을 rustdoc 에 적는다.

---

## 5. 🆕 rate-limit 관측

```rust
#[non_exhaustive]
pub struct RateLimit {
    pub limit: u64,
    pub remaining: u64,
    /// ⚠️ 헤더의 원시 값. 단위가 초라는 것은 **추정**이다(E3 각주 참조).
    pub reset_raw: u64,
}
```

⚠️ **`Client::last_rate_limit()` 같은 전역 슬롯을 두지 않는다.** 초안이 그렇게 설계했으나
검토에서 잡혔다 — 이 쿼터는 **차량당**인데 전역 슬롯 하나에 담으면
여러 VIN·동시 호출·`Client` 클론에서 **서로 덮어쓰고 오귀속**한다. 무과금 응답이 직전의
유용한 값을 지우는 문제도 있다.

→ **응답별 메타데이터로 준다.** 값을 만든 그 응답에 붙어 있어야 어느 VIN·어느 호출의 것인지가
자명하다:

```rust
#[non_exhaustive]
pub struct Metered<T> {
    pub value: T,
    /// 헤더가 없으면 `None`(무과금 엔드포인트 — E4).
    pub rate_limit: Option<RateLimit>,
}

impl Client { pub async fn send_metered<R: FleetGet>(&self, req: &R)
    -> Result<Metered<R::Response>, Error>; }
```

`send()` 는 그대로 둔다 — 쿼터를 안 보는 호출자에게 래퍼를 강요하지 않는다.

⚠️ **에러 경로의 쿼터 정보는 이 API 로 오지 않는다.** 실측한 408 이 바로 그 경우다(에러인데
헤더가 있었다). `Error` 에 붙이는 것은 §6 의 `txid` 와 같은 문제라 **A4 범위 밖으로 둔다** —
`Metered` 는 성공 경로만 덮는다는 한계를 rustdoc 에 적는다(**O14**).

⚠️ **관측 전용이다. 자동 스로틀링을 하지 않는다.** 근거 둘:
- 60/분은 **계정 공유**라 다른 앱이 쓴 분량을 이 클라이언트는 모른다 — 우리 잔량만 보고 자면 헛잔다.
- 진행 중인 대기는 호출자의 지연 예측을 깨뜨린다. A3 가 재시도를 기본 끄기로 한 것과 같은 사상이다.

이 사실을 `RateLimit` 의 rustdoc 에 적는다.

---

## 6. 에러 처리 보강

### 6.1 `error_description` 은 **두 곳**에 있다 — 초안이 혼동했다

- **바디** 필드: 408 이 `"error_description": ""` 를 준다(E2).
- **헤더** `WWW-Authenticate` 의 파라미터: 401 케이스 B 가 여기에만 설명을 담는다(A3 실측).

A3 의 `error_message()` 는 body `error` → `WWW-Authenticate` 의 `error_description` → `canonical_reason`
순이다. **바디의 `error_description` 은 아예 보지 않는다.** A4 는 이를 사슬에 넣되,
**빈 문자열을 건너뛴다** — 그러지 않으면 408 에서 키가 있다는 이유로 빈 메시지가 이긴다.

최종 순서: body `error`(비어있지 않을 것) → body `error_description`(비어있지 않을 것) →
`WWW-Authenticate` 의 `error_description`(비어있지 않을 것) → `canonical_reason()`.

### 6.2 `txid` 는 `Error::Api` **만으로는 부족하다**

⚠️ 초안은 `Error::Api` 에만 싣기로 했는데, **우리가 실측한 그 408 은 `VehicleOffline` 로 분류된다** —
즉 `txid` 를 관측한 바로 그 응답에서 `txid` 가 소실된다(자기모순).

→ `txid` 를 받는 variant 는 **`Api` 와 `VehicleOffline` 둘 다**로 한다.

```rust
Error::Api          { …, txid: Option<String> }
Error::VehicleOffline { pricing, txid: Option<String> }
```

`Error` 는 **enum 과 각 variant 양쪽에** `#[non_exhaustive]` 가 붙어 있다(코드 확인) — 따라서
struct-like variant 에 필드를 더하는 것도 non-breaking 이다. 바디 `txid` → 헤더 `x-txid` 순으로 채운다.

⚠️ **`txid` 는 redaction 대상이 아니다.** 계정 UUID 는 가리지만 트랜잭션 ID 는 **보존해야** Tesla 지원에
문의할 수 있다. A3 T7 이 `x-request-id` 를 보존하기로 한 결정과 같은 논리다.
⚠️ 다만 상관관계 식별자이므로 **`Display` 에 자동 노출하지 않고 필드로만** 둔다.

### 6.3 408 분류의 알려진 한계 (A4 가 만들지 않았고, 고치지도 않는다)

A1 이 **status 만으로** 408 → `VehicleOffline` 로 매핑한다. HTTP 408 은 일반적으로 "request timeout"
이므로 게이트웨이발 408 은 오분류된다. 실측 바디는 `"vehicle unavailable: vehicle is offline or asleep"`
이라 구분 근거가 존재하지만, **분류를 바디 문구로 옮기는 것은 A3 가 확립한 "문구는 분류에 쓰지 않는다"
원칙과 충돌**한다. A4 는 이 긴장을 **기록만 하고 바꾸지 않는다** — 다른 408 을 실제로 관측한 뒤
결정할 일이다(**O15**).

⚠️ **`backseat_token`**(E4)이 redaction 키 allowlist 에 걸리는지 확인한다. 이름에 `token` 이 있으니
현재 정규식/allowlist 로 잡힐 가능성이 높지만, **추정하지 말고 테스트로 고정한다.**

---

## 7. 모듈 구조

```
crates/fleetwire/src/api/
├── mod.rs        # 모듈 루트 (trait 은 endpoint.rs 소유)
├── users.rs      # region(A3) + me · orders · feature_config
├── vehicles.rs   # 목록·상태(A3) + vehicle_data · nearby_charging_sites ·
│                 #   mobile_enabled · recent_alerts · release_notes ·
│                 #   service_data · drivers · invitations ·
│                 #   fleet_telemetry_{config,errors}
└── (신규 파일 없음)
```

⚠️ `vehicles.rs` 가 12개 엔드포인트를 담아 커진다. **응답 타입은 같은 파일에** 둔다(상위 스펙 §3 의
grok-build 관례: 요청 struct 와 응답 타입을 같은 파일에). 파일이 ~800줄을 넘으면 구현 중
`vehicles/` 디렉터리로 분할하되, **분할 자체를 목표로 삼지 않는다.**

수정: `model.rs`(Envelope·Page) · `decode.rs`(Decoded 반환) · `client.rs`(send 조립·send_guarded·
RateLimit) · `error.rs`(txid) · `lib.rs`(export).

---

## 8. 테스트

wiremock 기반은 A3 그대로. **실측 픽스처를 바이트 그대로** 넣는다:

| # | 픽스처 | 근거 |
|---|---|---|
| F1 | 408 바디 전문(`response:null` + 빈 `error_description` + `txid`) | E2 |
| F2 | 페이지네이션 응답(`count`·`pagination` 형제 키) | E1 |
| F3 | 게이트 응답(`state:offline`, ratelimit 헤더 **없음**) | E4 |
| F4 | ratelimit 헤더 3종 | E3 |

### 반드시 있어야 할 테스트

| # | 무엇 | 왜 |
|---|---|---|
| T-a | 408 이 `VehicleOffline` 이고 `Decode` 로 새지 않음 | E2 — 봉투를 쓰므로 순서 계약이 여기서도 걸린다 |
| T-b | 빈 `error_description` 이 메시지로 **이기지 않음** | E2. ⚠️ **F1 만으로는 vacuous** — F1 은 비어있지 않은 `error` 를 갖고 그게 1순위라 폴백이 아예 안 돈다. **`error` 가 비었거나 없는 별도 픽스처**가 필요하다 |
| T-c | `pagination`·`count` 가 `Page<T>` 에 도달 | E1 |
| T-d | `pagination` 부재 시 기본값으로 채우고 **실패하지 않음** | §2.2 |
| T-e | `send_guarded` 가 offline 에서 **유료 요청을 0회** 보냄 | 게이트 존재 이유. 요청 수를 셀 것 |
| T-f | `vehicle_data` 가 위치 요청 시에만 `vehicle_location` 을 요구 | §3.1 조건부 스코프 |
| T-g | `location_data` 미지정 시 쿼리에 그게 **없음** | F9 이중 opt-in |
| T-h | 무과금 응답에서 `last_rate_limit()` 이 `None` | E4 |
| T-i | `txid` 가 바디→헤더 순으로 채워지고 **리댁션되지 않음** | §6 |
| T-j | `backseat_token` 이 **성공 경로에서도** 새지 않음 | E4. ⚠️ 관측 위치가 **성공 응답**이므로 에러 메시지만 보면 부족하다. `flatten extra` 보존·`Debug` 출력·직렬화·`body_preview` **전부** 검사할 것 |
| T-l | 게이트 호출 자체가 실패할 때의 정책이 고정됨 | §4.1 fail-closed |
| T-m | `Page.pagination` 이 부재 시 `None` 이고, 전 필드 `None` 인 `Some` 과 **구분됨** | §2.2 |
| T-k | 유료 2개가 `Pricing::data()`, 나머지 11개가 `NotCharged` | 과금 오분류 방지 |

### 테스트 규율 5조 (A2 확립, A3 에서 효과 재확인 — A4 도 이어받음)

1. **X 를 금지하는 규칙에는 X 를 했을 때만 실패하는 입력이 필요하다.**
2. **자기 테스트를 mutation-test 하고 생존자를 정직하게 분류하라** — "안 하기로 했다"를
   "할 수 없다"로 격상하지 말 것.
3. **값이 아니라 기전을 단언하라.** 요청 수를 세라.
4. 커버리지를 부풀리지 말 것.
5. vacuous 하게 통과할 수 있으면 큰 소리로 말하라.

---

## 9. 게이트 (매 태스크)

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked
cargo +1.89.0 test --locked --no-run     # ⚠️ build 아님 — 테스트 코드를 컴파일하지 않는다
cargo doc --no-deps --locked
```

---

## 10. 명시적 비범위

- 페이지 자동 순회 · `next_page()` 편의 메서드 (상위 스펙 §11)
- rate-limit 자동 스로틀링 (§5)
- `send()` 에 게이트 자동 적용 (§4)
- charging 3 · energy 6 · partner 3 — 실응답 검증 불가라 상위 스펙이 16개로 축소한 그대로
- 명령(POST) 계층 · MCP 서버 · Fleet Telemetry

## 11. 미결

| # | 항목 | 해소 시점 |
|---|---|---|
| O8 | 만료 access token 의 401 형태 | 미측정 |
| O9 | 무인증 요청의 과금 여부 | 사용량 대시보드 |
| O10 | **408 해소**(E2). **5xx 는 여전히 미측정** | 부분 해소 |
| O11 | 중국 리전 호스트 | 미검증 |
| **O12** | `per_page` 기본값 — O1 은 100, 실측은 1(차량 1대라 판별 불가) | 차량 2대 이상 또는 invitations 로 재측정 |
| **O13** | 최상위 `count` 와 `pagination.count` 가 갈릴 수 있는가 — 1건 표본에선 동일 | 다건 목록(invitations)으로 재측정 |
| **O14** | 에러 경로의 rate-limit 헤더가 API 로 노출되지 않음 — `Metered` 는 성공 경로만 | A5 또는 필요 시 |
| **O15** | 408 을 status 만으로 `VehicleOffline` 로 매핑 — 게이트웨이발 408 오분류 가능 | 다른 408 을 실제 관측한 뒤 |
| **O16** | rate limit 이 계정 공유인가 차량 전용인가 — 헤더는 per-vehicle, O1 은 계정 공유 | 2대 이상 또는 다중 앱 동시 호출 |
| **O17** | 유료 2개 외 11개의 `NotCharged` 는 **O1 매트릭스 전제**이지 실측이 아니다 | 대시보드 대조 |
| **O18** | rate limit 창의 종류(fixed/sliding/token-bucket)와 `reset` 경계 동작 | 창 경계를 걸친 연속 측정 |

## 12. 부록 — 실측 원본

세션 scratchpad `a4-evidence.md` 에 요청·응답 헤더 전문 보존.
O1 증거 매트릭스에 `count` 형제 키 · 408 형태 · rate-limit 헤더 · 게이트 무과금 행을 추가해야 한다.

비용: `vehicle_data` 5회(전부 408) × $0.002 ≈ **$0.01**. ⚠️ 단가는 O1/F1 문서 근거이고
대시보드 대조를 하지 않았으므로 이 금액도 **추정**이다. 무과금 호출은 별도.

---

## 13. 검증 이력

| 라운드 | 도구 | 잡아낸 것 |
|---|---|---|
| 실측 | curl(무과금 3 + 유료 5) | `count` 형제 키 · 408 봉투+빈 `error_description`+`txid` · rate-limit 헤더 3종 · 게이트에 헤더 없음 · `backseat_token` |
| self-review | 본인 | `Page.count` 를 넣으면 `Page.pagination.count` 와 **같은 이름·같은 값**의 필드가 둘 생김 → 철회 |

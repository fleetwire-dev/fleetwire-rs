# fleetwire 요구사항

## 1. 목적과 범위

`fleetwire`는 Apache-2.0으로 배포하는 Tesla Fleet API 읽기 전용 Rust 클라이언트 라이브러리다. 이번 스펙은 실제 계정과 보유 하드웨어로 라이브 검증 가능한 GET 16개(vehicle 12개, user 4개)만 다룬다. charging 3개, energy 6개, partner 3개는 검증할 스코프 또는 하드웨어가 없으므로 범위에서 제외한다.

## 2. 용어와 외부 제약

- **Data 호출**: 유료 카테고리인 `vehicle_data`와 `nearby_charging_sites` 호출은 호출당 $0.002이며, 월 $10 개인 할인은 계정당 Data 5,000회에 해당한다. 나머지 14개 엔드포인트는 `pricing_category: null`인 무과금 호출이다.
- **과금 응답**: 유료 Data 엔드포인트에서 HTTP status가 500 미만인 모든 응답이다. 408과 429도 유료 엔드포인트에서는 과금되며, 500 이상만 무과금이다. 무과금 엔드포인트는 status와 무관하게 과금되지 않는다.
- **연결성 게이트**: 무과금인 `GET /api/1/vehicles/{vin}`의 `state`를 확인한 뒤 유료 차량 호출을 실행할지 결정하는 절차다.
- **봉투(envelope)**: 일반 JSON 응답의 `{"response": ...}` 형식이다. 인증 오류와 게이트웨이 응답은 예외일 수 있다.
- **라이브 검증**: 실제 Tesla 서비스에 호출하여 현재 응답 구조를 확인하는 작업이다.

## 3. 기능 및 비기능 요구사항

### REQ-1 — 지원 엔드포인트와 범위 고정

**WHEN** 사용자가 지원되는 읽기 API를 호출하면 **THE SYSTEM SHALL** 다음 16개 엔드포인트에 대해서만 강타입 GET 요청을 제공한다.

- Vehicle: `/api/1/vehicles`, `/api/1/vehicles/{vin}`, `/api/1/vehicles/{vin}/vehicle_data`, `/mobile_enabled`, `/nearby_charging_sites`, `/recent_alerts`, `/release_notes`, `/service_data`, `/drivers`, `/invitations`, `/fleet_telemetry_config`, `/fleet_telemetry_errors`
- User: `/api/1/users/me`, `/api/1/users/region`, `/api/1/users/orders`, `/api/1/users/feature_config`

**Acceptance Criteria**
- 공개 강타입 API에 위 16개의 GET 엔드포인트가 빠짐없이 존재한다.
- vehicle 12개는 `vehicle_device_data`를 요구하고, 위치 데이터 요청은 추가로 `vehicle_location`을 요구한다.
- user 4개는 user-context 토큰과 `user_data` 계열 스코프를 요구하며 3rd-party business 토큰과 호환되지 않는다. `/users/region`은 여러 허용 스코프 중 하나로 접근할 수 있다.
- 범위 밖 12개 엔드포인트를 검증 완료 또는 지원 완료로 표시하지 않는다.

### REQ-2 — 엔드포인트 타입과 편의 API

**WHEN** 개발자가 엔드포인트 값을 구성하면 **THE SYSTEM SHALL** 봉인된 `FleetGet` 구현으로 응답 타입, 요구 스코프, 가격, 경로, 구조화 쿼리를 명시하고 차량 게이트 대상에는 `VehicleEndpoint`를 구현한다.

**Acceptance Criteria**
- `FleetGet`은 `Sealed + Send + Sync`이며 `Response`, `required_scopes()`, `pricing()`, `path()`, `query()`를 가진다.
- `pricing()`에는 기본 구현이 없어서 모든 엔드포인트가 가격을 명시한다.
- `query()`는 인코딩과 주입 방지를 위해 문자열이 아닌 `QuerySpec`을 반환한다.
- `Client::send<R: FleetGet>(&R)`와 `Client::send_guarded<R: VehicleEndpoint>(&R)`를 제공한다.
- 대부분의 사용자가 trait을 직접 다루지 않도록 16개 엔드포인트별 얇은 편의 메서드를 제공하고 각 메서드는 `send()` 또는 `send_guarded()`로 위임한다.

### REQ-3 — 제한된 raw 탈출구

**WHEN** 사용자가 아직 강타입으로 지원되지 않는 읽기 경로를 호출하면 **THE SYSTEM SHALL** `send_raw(relative_path, query) -> Result<Value, Error>`를 제공하되 상대경로와 동일 origin의 GET만 허용한다.

**Acceptance Criteria**
- 절대 URL, scheme-relative URL, host를 바꿀 수 있는 입력, 경로 정규화 후 origin을 벗어나는 입력을 거부한다.
- raw 쿼리도 구조화된 쿼리 타입을 사용한다.
- `send_raw`는 POST·PUT·PATCH·DELETE를 생성할 수 없다.
- 강타입 기본 경로와 raw 탈출구의 공존을 문서화한다.

### REQ-4 — 응답 모델의 전방 호환성

**WHEN** Tesla가 응답 필드를 누락하거나 새로운 필드를 추가하면 **THE SYSTEM SHALL** 알려진 모든 응답 필드를 `Option<T>`와 `#[serde(default)]`로 역직렬화하고 미지 필드를 `#[serde(flatten)] extra: Map<String, Value>`에 보존한다.

**Acceptance Criteria**
- 모든 응답 구조체의 알려진 필드는 선택적이며 기본값 처리를 한다.
- `deny_unknown_fields`를 사용하지 않는다.
- 새 필드를 포함하거나 기존 선택 필드가 없는 fixture가 파싱된다.
- 미지 필드가 `extra`에서 손실 없이 조회된다.

### REQ-5 — 봉투, 상태 및 콘텐츠 판별

**WHEN** HTTP 응답을 수신하면 **THE SYSTEM SHALL** status와 Content-Type을 먼저 판별한 뒤 성공 JSON에 대해 `{"response": T}` 봉투를 해체하고 예외 응답은 적절한 오류로 분류한다.

**Acceptance Criteria**
- 역직렬화 전에 status와 Content-Type을 검사한다.
- 정상 봉투에서 `response` 값만 반환한다.
- 인증 오류와 게이트웨이의 비봉투 응답도 panic 없이 오류로 변환한다.
- 디코드 실패는 status와 redaction된 `body_preview`를 포함한 `Error::Decode`가 된다.

### REQ-6 — 페이지네이션과 명시적 전체 순회

**WHEN** 사용자가 차량 목록 또는 초대 목록을 요청하면 **THE SYSTEM SHALL** query `page`와 `per_page`를 사용하는 `Page<T>` 및 응답의 `pagination { previous, next, current, per_page, count, pages }`를 노출하며 자동 전체 순회는 명시적 `all_pages(max_pages)` 호출에서만 수행한다.

**Acceptance Criteria**
- 차량 목록의 기본 페이지 크기는 100이다.
- 초대 목록의 페이지 크기는 최대 25를 넘지 못한다.
- 다음 페이지 값과 `previous`, `next`, `current`, `per_page`, `count`, `pages` 페이지네이션 메타데이터를 호출자에게 노출한다.
- `all_pages(max_pages)`는 상한을 필수로 받고 각 페이지가 별도 호출임을 문서화한다. 현재 차량 목록과 초대 목록은 무과금이지만 rate limit은 적용된다.
- 기본 편의 메서드는 한 페이지만 요청한다.
- `nearby_charging_sites`는 페이지네이션을 사용하지 않고 별도 `QuerySpec`으로 `count`, `radius`, `detail`을 표현한다.

### REQ-7 — 위치 데이터 이중 opt-in

**WHEN** 사용자가 `vehicle_data`에서 위치 데이터를 요청하면 **THE SYSTEM SHALL** 구조화 쿼리에 `endpoints=...location_data...`를 포함하고 요구 스코프를 `vehicle_device_data`와 `vehicle_location`의 결합으로 계산한다.

**Acceptance Criteria**
- 위치 미요청 시 `location_data` 쿼리와 `vehicle_location` 요구가 없다.
- 위치 요청 시 두 스코프 모두 필요하다.
- 펌웨어 2023.38 이상에서 쿼리와 스코프 중 하나만 충족해도 충분하다고 문서화하지 않는다.
- 파라미터에 따라 스코프가 달라지므로 `required_scopes()`는 const가 아니다.

### REQ-8 — advisory 스코프 선검사

**WHEN** access token이 파싱 가능한 JWT이고 `scp`가 존재하면 **THE SYSTEM SHALL** `ScopeExpr::{All, Any, One}`으로 요구 스코프를 호출 전에 대조하고 불일치 시 네트워크 요청 없이 `MissingScope`를 반환한다.

**IF** access token이 JWT가 아니거나 payload 또는 `scp`를 파싱할 수 없으면 **THEN THE SYSTEM SHALL** 선검사를 fail-open으로 통과시키고 권한의 최종 판정을 서버 403에 맡긴다.

**Acceptance Criteria**
- 스코프 불일치가 확인되면 과금 가능한 요청을 전송하지 않는다.
- JWT 서명을 검증한 것처럼 취급하거나 선검사를 권한 근거로 사용하지 않는다.
- 파싱 실패만으로 요청을 차단하지 않는다.
- `All`, `Any`, `One` 각각의 판정 로직을 검증한다.

### REQ-9 — 차량 연결성 게이트

**WHEN** 사용자가 `send_guarded()`로 차량 엔드포인트를 호출하면 **THE SYSTEM SHALL** 먼저 무과금인 `/api/1/vehicles/{vin}`의 `state`를 조회하고 상태가 `online`일 때만 대상 유료 호출을 실행한다.

**IF** 상태가 `online`이 아니면 **THEN THE SYSTEM SHALL** 대상 호출을 생략하고 `Guarded::Skipped { connectivity }`를 반환한다.

**Acceptance Criteria**
- 실행된 응답은 `Guarded::Executed(T)`로 반환한다.
- 게이트에 사용하는 연결성 값은 차량 정보 응답의 `state`다.
- offline·asleep·unknown 상태에서는 대상 호출이 전송되지 않는다.
- 가장 비싼 `vehicle_data`를 포함한 차량 유료 작업에 guarded 편의 경로를 제공한다.
- 게이트 조회 자체는 `Pricing::NotCharged`인 무과금 호출임을 문서화한다.

### REQ-10 — 비용과 과금 인식

**WHILE** 라이브러리가 종량제 Fleet API를 사용하면 **THE SYSTEM SHALL** 월 $10 크레딧 범위 운영을 지원하도록 유료 호출 전 게이트, 기본 재시도 비활성, 엔드포인트별 명시적 가격 메타데이터 및 결과별 과금 판정을 제공한다.

**Acceptance Criteria**
- `Pricing::{NotCharged, Data { usd_per_request: 0.002 }}`로 엔드포인트별 가격을 표현한다.
- 16개 중 `vehicle_data`와 `nearby_charging_sites` 2개만 `Pricing::Data { usd_per_request: 0.002 }`이고 나머지 14개는 `Pricing::NotCharged`다.
- 참고 가격으로 Wakes $0.02, Commands $0.001, 월 $10 = Data 5,000회를 문서화한다.
- `Billing::{Billed, NotBilled, Unknown}`과 엔드포인트의 `Pricing`을 반영하는 `Error::billing()`을 제공한다.
- 무과금 14개는 status와 무관하게 `NotBilled`로 판정한다.
- 유료 2개는 status `< 500`은 `Billed`, status `>= 500`은 `NotBilled`, 전송 또는 처리 여부 불명은 `Unknown`으로 판정한다.
- 유료 2개에서는 408과 429가 각각 $0.002 과금될 수 있고, 무과금 14개에서는 동일 status도 과금되지 않음을 명시한다.
- 비용 최적화가 검증되지 않은 무과금 보장으로 표현되지 않는다.

### REQ-11 — 오류 모델과 408 처리

**WHEN** 요청 또는 응답 처리가 실패하면 **THE SYSTEM SHALL** 실패를 `TransportNotSent`, `TransportUnknown`, `Api`, `VehicleOffline`, `MissingScope`, `Decode`, `Auth` 중 하나로 분류하고 재시도·과금·대기 정보를 질의 가능하게 하며, 과금 판정에는 해당 엔드포인트의 `Pricing`을 사용한다.

**Acceptance Criteria**
- DNS·connect 실패처럼 미전송이 확실한 경우 `TransportNotSent`를 사용한다.
- read timeout·connection reset처럼 서버 처리 여부가 불명인 경우 `TransportUnknown`을 사용한다.
- API 오류는 status, message, retry_after, redaction된 body_preview를 보존한다.
- 408은 `VehicleOffline` 오류로 올리며 상위 폴링 루프가 variant match로 흡수할 수 있다.
- `Error::is_retryable()`, 엔드포인트 `Pricing`을 반영하는 `Error::billing()`, `Error::retry_after()`를 제공한다.
- 무과금 엔드포인트 오류는 status와 무관하게 `Billing::NotBilled`이며, 유료 엔드포인트의 전송 또는 처리 여부 불명 오류는 `Billing::Unknown`이다.

### REQ-12 — 과금 안전 재시도와 rate limit

**WHEN** 재시도 정책을 적용하면 **THE SYSTEM SHALL** 기본 `max_attempts = 1`로 두고 명시적으로 활성화된 경우에도 `TransportNotSent`와 일시적 500·502·503·504만 재시도한다.

**IF** 응답이 `TransportUnknown`, 408, 429 또는 기타 4xx이면 **THEN THE SYSTEM SHALL** 중복 과금 방지를 위해 재시도하지 않는다.

**Acceptance Criteria**
- backoff는 exponential + jitter이며 개별 지연은 30초 이하이다.
- `Retry-After`를 존중하되 대기는 120초를 넘지 않는다.
- Realtime Data의 디바이스+계정 공유 한도 60회/분과 429의 `RateLimit-*` 헤더를 호출자가 관찰할 수 있다.
- 같은 계정의 여러 앱이 한도를 공유함을 문서화한다.
- 재시도 체인의 전체 시도 횟수가 설정값을 넘지 않는다.
- 무과금 엔드포인트는 과금 관점에서 재시도해도 중복과금이 없지만, 위 재시도 허용 목록과 기본 `max_attempts = 1`은 그대로 적용하며 60회/분 rate limit을 준수한다.

### REQ-13 — HTTP 수명주기와 자원 제한

**WHEN** `Client`를 생성하면 **THE SYSTEM SHALL** 인스턴스가 소유하는 장수 `reqwest::Client`를 connect 10초, 요청 30초, host당 idle connection 2개, idle timeout 90초로 구성한다.

**WHILE** 재시도 체인을 실행하면 **THE SYSTEM SHALL** 체인 전체를 `tokio::time::timeout`으로 제한하고 응답은 `bytes_stream()`으로 읽어 누적 크기 상한을 넘는 즉시 중단한다.

**Acceptance Criteria**
- HTTP client는 프로세스 전역 static이 아니다.
- 전체 체인 timeout이 개별 `reqwest` timeout과 별도로 존재한다.
- 본문 전체를 버퍼링한 뒤 크기를 검사하지 않는다.
- 크기 초과 시 추가 chunk 읽기를 중단하고 분류 가능한 오류를 반환한다.

### REQ-14 — 인증 호스트와 401 분류

**WHEN** 사용자가 OAuth 로그인을 시작하면 **THE SYSTEM SHALL** `https://auth.tesla.com/oauth2/v3/authorize`를 사용하고, token refresh가 필요하면 `https://fleet-auth.prd.vn.cloud.tesla.com/oauth2/v3/token`에 POST한다.

**IF** 원 요청이 401을 반환하면 **THEN THE SYSTEM SHALL** 원인을 분류한 후에만 refresh를 최대 1회 수행하고 새 access token으로 원 요청을 정확히 1회 재시도한다.

**Acceptance Criteria**
- 로그인과 refresh의 서로 다른 호스트를 오타로 통합하지 않는다.
- 모든 401을 무조건 refresh로 해석하지 않는다.
- 하나의 원 요청에서 refresh와 인증 재시도는 각각 1회를 넘지 않는다.
- 인증 외 Fleet API 공개 경로에는 POST 생성 수단이 없다.

### REQ-15 — 프로세스 및 프로세스 간 refresh 단일 실행

**WHEN** access token refresh가 필요하면 **THE SYSTEM SHALL** in-process `tokio::sync::Mutex`를 await 구간 전체에 걸쳐 보유한 뒤 고정 파일 `~/.tesla/tokens.lock`에 timeout이 있는 `flock(LOCK_EX)`를 획득하고, 락 아래에서 토큰 파일을 다시 읽는다.

**IF** 다시 읽은 access token이 아직 유효하면 **THEN THE SYSTEM SHALL** 형제 프로세스가 갱신한 토큰을 `Adopted`로 채택하고 refresh 요청을 보내지 않는다.

**Acceptance Criteria**
- 동일 프로세스의 동시 요청은 refresh single-flight로 합쳐진다.
- 여러 프로세스가 같은 토큰 파일을 공유해도 refresh POST가 중복 실행되지 않는다.
- mutex와 flock 획득에는 유한 timeout 또는 전체 작업 deadline이 적용된다.
- `tokens.lock` 파일은 정상·오류·프로세스 종료 경로 어디에서도 삭제하지 않는다.
- 토큰 저장에 `OnceCell` 또는 `OnceLock`을 사용하지 않는다.

### REQ-16 — refresh token 회전과 안전한 저장

**WHEN** refresh가 성공해 새 refresh token을 받으면 **THE SYSTEM SHALL** flock을 보유한 상태에서 새 토큰을 원 요청 재시도보다 먼저 disk-first로 즉시 저장한다.

**Acceptance Criteria**
- `.tmp` 쓰기, 파일 `fsync`, 원자적 `rename`, 부모 디렉터리 `fsync` 순서를 따른다.
- 토큰 파일 mode는 0600이다.
- 토큰 파일과 임시 파일에 대한 symlink 공격을 방어한다.
- 저장에 실패하면 새 토큰을 사용해 계속 진행하지 않고 명시적 인증 오류를 반환한다.
- 새 refresh token 미저장으로 다음 갱신에서 재-OAuth가 필요해지는 위험을 문서화한다.

### REQ-17 — 리전 해석과 캐시

**WHEN** Fleet API base URL이 아직 결정되지 않았으면 **THE SYSTEM SHALL** `/api/1/users/region`을 호출해 리전 base URL을 해석하고 성공한 결과를 캐시한다.

**IF** 리전 해석이 실패하면 **THEN THE SYSTEM SHALL** NA base URL `https://fleet-api.prd.na.vn.cloud.tesla.com`로 폴백한다.

**Acceptance Criteria**
- 리전 base URL 캐시에만 `OnceCell` 또는 `OnceLock`을 사용할 수 있다.
- 해석된 base URL은 이후 요청에서 재사용된다.
- 폴백 발생을 민감정보 없이 관찰 가능하게 한다.
- base URL 조합 후 same-origin 정책을 유지한다.

### REQ-18 — 민감정보 redaction

**WHEN** 로그, 오류 메시지, body preview, fixture 또는 `Debug` 표현을 생성하려 하면 **THE SYSTEM SHALL** 생성 전에 단일 redaction 계층을 통과시켜 Bearer/JWT/refresh_token/client_secret을 `[REDACTED]`, VIN을 `[VIN]`, 좌표를 `[COORD]`로, **자유 텍스트 목적지 주소를 `[REDACTED]`로** 치환한다.

**Acceptance Criteria**
- JSON은 키 allowlist 기반 구조화 redaction을 우선하고 regex는 보조로만 사용한다.
- `reqwest::Error`의 URL, source chain 및 `Debug` 출력도 정제한다.
- 토큰, VIN, 좌표, **목적지 주소**가 로그·오류 메시지·커밋된 fixture에 남지 않는다.
- redaction 이전 원문을 formatting 또는 tracing 인자로 전달하지 않는다.
- id 등 라이브 캡처의 식별자도 fixture 정제 정책에 따라 제거 또는 안정적 대체값으로 바꾼다. **단 live body preview는 id를 보존한다** — id 제거는 fixture 정제 정책의 범위이며, 디버깅 가능성을 위해 계정 범위 id는 preview에 남긴다.

> **정정 이력 (2026-07-30, A1 구현 중 오너 판정)**
> 원 요구는 Bearer/JWT/refresh_token/client_secret·VIN·좌표 6종만 열거했고 자유 텍스트 목적지는 빠져 있었다. A1 T8 리뷰에서 `drive_state.active_route_destination`(예: `"1234 Elm St, Palo Alto, CA 94301"`)이 무처치 통과함이 드러났다 — 같은 장소를 가리키는 `active_route_latitude`/`longitude`는 "페이로드 최고 민감 좌표(통상 소유자 자택)"라며 정성껏 지우면서, 그 자택을 영어로 명시하는 형제 필드는 평문으로 흘리는 자기모순이었다. fleetwire는 OSS 배포 라이브러리라 남의 자택 주소가 로그에 남는 형태로 배포된다. 구현은 5키 allowlist를 건드리지 않고 regex 계층에 키 접미 앵커(`"(?:[a-z0-9]+_)*destination"`)로 추가했다.
> 마지막 항목의 id 단서도 명확히 했다 — 구현은 live preview에서 id를 지우지 않으며, REQ-20의 id 제거 테스트는 fixture 정제 범위다.

### REQ-19 — 읽기 전용 강제

**WHILE** 라이브러리가 Fleet API 요청을 구성하면 **THE SYSTEM SHALL** 타입 시스템, 모듈 가시성, Clippy 금지 목록의 3계층으로 GET 전용 속성을 강제한다.

**Acceptance Criteria**
- 공개 API에 Fleet API POST·PUT·PATCH·DELETE 요청 생성 경로가 없다.
- raw `reqwest::Client` 접근은 비공개다.
- `reqwest::Client::{post, put, delete, request}`를 `clippy::disallowed-methods`로 금지한다.
- `[workspace.lints.clippy] disallowed_methods = "deny"`로 lint가 실제 활성화된다.
- token refresh POST 한 곳만 사유 주석과 `#[allow]`로 예외 처리한다.

### REQ-20 — 무과금 테스트와 라이브 드리프트 감지

**WHEN** 공개 CI가 실행되면 **THE SYSTEM SHALL** 네트워크와 과금 없이 로직·불변식·스코프·redaction 유닛 테스트 및 fixture 파싱 계약 테스트를 실행한다.

**WHEN** 토큰 보유 라이브 환경의 주간 작업이 실행되면 **THE SYSTEM SHALL** 16개 엔드포인트를 여러 차량 상태에서 캡처하고 민감정보를 정제한 구조 지문을 커밋된 기준과 비교하여 차이가 있으면 이슈를 생성한다.

**Acceptance Criteria**
- fixture 계약 테스트를 파싱 회귀 검증으로만 설명하고 라이브 드리프트 감지라고 부르지 않는다.
- 정적 fixture를 자기 자신의 스냅샷과 비교하는 방식을 드리프트 감지로 사용하지 않는다.
- 라이브 캡처는 주차·주행·충전 상태를 가능한 범위에서 포함한다.
- 정제기 자체의 토큰·VIN·좌표·id 제거 테스트가 있다.
- 주 1회 16개 엔드포인트 각 1호출 기준 유료 호출 예상 비용 `2 × $0.002 × 4.33 ≈ 월 $0.02`를 예산에 반영하고, 상태·게이트·페이지에 따른 추가 호출은 별도로 계측한다.

### REQ-21 — 릴리스 품질과 공급망 제약

**WHEN** 릴리스 후보를 검증하면 **THE SYSTEM SHALL** `cargo clippy -- -D warnings`, `cargo semver-checks`, `cargo deny check`, `cargo test --locked` 및 선언한 MSRV CI job을 모두 통과시킨다.

**Acceptance Criteria**
- `[package.metadata.docs.rs] all-features = true`를 설정한다.
- MSRV를 manifest와 문서에 선언하고 해당 버전으로 CI를 수행한다.
- `Cargo.lock`을 커밋한다.
- `build.rs`를 만들지 않는다.
- crate 라이선스 메타데이터와 배포 파일이 Apache-2.0을 나타낸다.

### REQ-22 — 문서화의 정직성과 추적성

**WHERE** 기능, 테스트, 라이브 응답 또는 호환성을 문서화하면 **THE SYSTEM SHALL** 실제 검증한 범위와 검증하지 못한 범위를 구분하고 모든 요구사항을 설계 및 구현 태스크에 추적 가능하게 연결한다.

**Acceptance Criteria**
- 라이브 검증하지 못한 12개 읽기 GET을 이번 범위의 검증 완료로 주장하지 않는다.
- fixture 통과를 현재 Tesla 응답과의 일치로 주장하지 않는다.
- 비용, 과금 여부 또는 권한을 추론한 경우 그 한계를 명시한다.
- 모든 `REQ-N`이 `design.md` 매핑 매트릭스와 `tasks.md`에 각각 최소 한 번 등장한다.

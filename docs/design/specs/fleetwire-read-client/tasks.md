# fleetwire 구현 태스크

> 2026-07-24 KST

## 실행 원칙

- 태스크는 위에서 아래 순서로 수행한다. 선행 태스크의 공개 타입이나 불변식을 임시 우회하지 않는다.
- 이번 구현 범위는 **라이브 검증 가능한 GET 16개**로 고정한다(vehicle 12 + user 4).
- 공개 CI 테스트는 **과금과 외부 네트워크 없이** 실행한다. 라이브 검증은 보호된 별도 환경에서만.
- **파이프라인을 먼저 세우고 편의 API 를 얹는다.** 조각을 다 만든 뒤 마지막에 연결하는 빅뱅 통합을 하지 않는다.
- 각 태스크 완료 시 관련 수용기준과 문서의 검증 수준을 함께 확인한다.

---

## 0. 스캐폴딩과 정책

- [ ] 0.1 Apache-2.0 메타데이터·edition·MSRV 선언을 가진 `fleetwire` crate 를 workspace 에 추가한다.
- [ ] 0.2 `src` 모듈 골격을 설계 경계대로 만들고 raw transport 와 auth 내부를 비공개로 둔다.
- [ ] 0.3 `[package.metadata.docs.rs] all-features = true` 설정.
- [ ] 0.4 clippy `disallowed-methods` 에 `reqwest::Client::{post,put,delete,request}` 등록 +
      `[workspace.lints.clippy] disallowed_methods = "deny"` 로 **실제 활성화**.
- [ ] 0.5 `Cargo.lock` 커밋 대상 포함, `build.rs` 를 만들지 않는다.
- [ ] 0.6 **rust-toolchain.toml**(버전 고정 + 승급 정책 주석)과 **deny.toml** 초기값을 만든다.
- [ ] 0.7 **조기 PR CI 워크플로** — `cargo test` + `cargo clippy -- -D warnings` + MSRV job.
      ※ 릴리스 게이트(14.1)와 분리. 테스트가 "작성만 되고 CI 에서 안 도는" 공백을 막는다.
- [ ] 0.8 **mock HTTP 테스트 하니스**와 `fixtures/` 디렉토리 규약을 세운다.
      ※ 이후 모든 계약 테스트가 이것을 전제한다.
- [ ] 0.9 범위가 GET 16개이며 미검증 12개는 제외되었다는 crate-level 문서 골격 추가.

_Requirements: REQ-1, REQ-19, REQ-21, REQ-22_

## 1. 엔드포인트 타입 계층

- [ ] 1.1 외부 구현을 막는 `sealed::Sealed` + `FleetGet` + `VehicleEndpoint` trait 구현.
- [ ] 1.2 `FleetGet` 에 `Response`, `required_scopes()`, `pricing()`, `path()`, `query()` 정의.
      **`pricing()`·`required_scopes()` 에 기본값을 두지 않는다**(누락 시 컴파일 에러 = fail-safe).
- [ ] 1.3 percent-encoding·중복 키를 안전하게 다루는 `QuerySpec`. 내부에서도 문자열 조립 금지.
- [ ] 1.4 `Scope`, `ScopeExpr::{All, Any, One}` 과 조합 판정 로직.
- [ ] 1.5 `Pricing::{NotCharged, Data { usd_per_request: 0.002 }}`와 `Billing` 타입. endpoint마다 정확한 가격을 명시하고 요청·오류 컨텍스트가 `Pricing`을 carry한다.
- [ ] 1.6 path segment VIN 안전 인코딩 helper(`../`·scheme·authority 차단 포함).

_Requirements: REQ-2, REQ-3, REQ-6_

## 2. 응답 모델과 봉투

- [ ] 2.1 `Envelope<T>`, `Page<T>`, `pagination { previous, next, current, per_page, count, pages }` 및 공통 응답 타입.
- [ ] 2.2 모든 알려진 응답 필드에 `Option<T>` + `#[serde(default)]`, 각 구조체에 `#[serde(flatten)] extra`.
- [ ] 2.3 **`deny_unknown_fields` 가 어느 응답 모델에도 사용되지 않게 한다.**
- [ ] 2.4 **status 와 Content-Type 판정 후에만** 봉투를 해체하는 decode 함수.
- [ ] 2.5 인증 오류·게이트웨이·비 JSON 응답을 panic 없이 오류로 변환하는 기반.

_Requirements: REQ-4, REQ-5_

## 3. 에러·과금 판정

> ⚠️ 3.6(body_preview)은 **4장 redaction 계층 완성 후에 최종 배선**한다.
> 임시 문자열 치환으로 채우면 "단일 redaction 계층" 불변식이 깨진다.

- [ ] 3.1 `Error` variant — `TransportNotSent` / `TransportUnknown` / `Api` / `VehicleOffline` /
      `MissingScope` / `Decode` / `Auth`. 각 endpoint 요청에서 발생한 오류는 `Error::billing()`이 참조할 `Pricing`을 보존한다.
- [ ] 3.2 **`TransportNotSent` vs `TransportUnknown` 판정표를 코드와 문서에 못 박는다.**

      | 상황 | 분류 | `Pricing::NotCharged` 과금 | `Pricing::Data` 과금 |
      |---|---|---|---|
      | DNS 실패 · connect 실패 · TLS 핸드셰이크 실패 | `TransportNotSent` | NotBilled | NotBilled |
      | request 전송 후 read timeout | `TransportUnknown` | NotBilled | Unknown |
      | 헤더 수신 후 body 중 연결 끊김 | `TransportUnknown` | NotBilled | Unknown |
      | 전체 deadline 초과 | `TransportUnknown` | NotBilled | Unknown |

      ※ 이 표가 없으면 과금 Unknown 비율이 구현자 재량이 된다.
- [ ] 3.3 `Billing { Billed, NotBilled, Unknown }` 과 endpoint-aware `Error::billing()` 매핑.
      무과금 14개는 status 불문 `NotBilled`; 유료 2개는 status `<500`이면 `Billed`, `>=500`이면 `NotBilled`, 전송·처리 불명이면 `Unknown`이다.
- [ ] 3.4 `is_retryable()` — **재시도 대상은 `TransportNotSent` 와 일시적 5xx(500/502/503/504) 뿐.**
      429·408·`TransportUnknown`·기타 4xx 는 재시도하지 않는다.
- [ ] 3.5 `retry_after()` — `Retry-After` 를 **120초 상한**으로 파싱해 에러에 실어 올린다(대기하지 않음).
- [ ] 3.6 `body_preview` 필드 — 4.5 에서 redaction 계층에 배선한다.

_Requirements: REQ-9, REQ-10, REQ-11_

## 4. redaction 단일 계층

- [ ] 4.1 모든 로깅·에러 메시지 생성 **이전에** 통과하는 단일 정제 함수.
- [ ] 4.2 **구조화 redaction 우선** — JSON 은 키 allowlist 기반, regex 는 보조.
      대상: Bearer/JWT/refresh_token/client_secret + **VIN + 좌표**.
- [ ] 4.3 `reqwest::Error` 의 URL·source chain 과 `Debug` 출력 경로를 포함한다.
- [ ] 4.4 **보장 범위를 명시한다** — 공개 `Error` 타입의 필드와 crate 가 생성하는 로그에 한한다.
      ※ 서드파티 타입의 임의 `Debug` 출력까지 강제하는 것은 완료 판정 불가이므로 목표에서 제외.
- [ ] 4.5 3.6 의 `body_preview` 를 이 계층에 배선한다.

_Requirements: REQ-16_

## 5. HTTP 계층

- [ ] 5.1 `Client` 인스턴스가 소유하는 장수 `reqwest::Client`(프로세스 전역 static 아님).
- [ ] 5.2 connect 10s / 요청 30s / `pool_max_idle_per_host(2)` / `pool_idle_timeout(90s)`.
      ※ 초기값이며 실측으로 조정. 동시성이 높아지면 idle 2 가 병목일 수 있다.
- [ ] 5.3 **재시도 체인 전체를 `tokio::time::timeout` 으로 감싼다**(`reqwest.timeout()` 은 체인을 못 막음).
      요청 30s < 체인 deadline 60s < `Retry-After` 상한 120s 의 예산 관계를 문서화한다.
- [ ] 5.4 응답 크기 상한 — `bytes_stream()` 누적 검사. JSON·PDF·오류 body **전부**. buffer-then-check 금지.
- [ ] 5.5 전송 실패 재시도 시 poisoned pool 탈출 경로(`pool_max_idle_per_host(0)` + `http1_only()`).

_Requirements: REQ-12, REQ-13_

## 6. 요청 파이프라인 (send / send_guarded / send_raw)

> 여기서 파이프라인이 **동작 가능한 상태**가 된다. 이후 편의 API 는 전부 이것에 위임한다.

- [ ] 6.1 `send<R: FleetGet>` — 순서: scope 선검사 → region base URL → auth → retry/deadline → GET → decode. endpoint의 `Pricing`을 끝까지 carry한다.
- [ ] 6.2 `send_guarded<R: VehicleEndpoint>` — 무과금인 `GET /vehicles/{vin}` 의 `state` 확인 후 online 일 때만 호출.
      `Guarded<T> { Executed(T), Skipped { connectivity } }`.
      **연결성 probe 자신이 다시 게이트를 타지 않도록 재귀를 차단한다.**
- [ ] 6.3 `send_raw` — **상대경로 전용 + 최종 URL same-origin 검증**. 절대 URL·origin 변경 차단.
- [ ] 6.4 게이트·리전·페이지·auth 재시도의 호출 수를 각각 계측하는 hook.
- [ ] 6.5 **중간 통합 스모크** — mock 서버 + 고정 base URL 로 `send` 한 경로를 끝까지 통과시킨다.
      ※ 이후 태스크가 이 위에 쌓이므로 여기서 회귀를 잡는다.

_Requirements: REQ-7, REQ-8, REQ-14_

## 7. 인증·토큰

- [ ] 7.1 토큰 파일 0600 + symlink 방어 + `.tmp` 쓰기 → fsync → rename → 디렉토리 fsync.
- [ ] 7.2 in-process `tokio::sync::Mutex` + 고정 `~/.tesla/tokens.lock` flock 2계층.
      **락 파일을 삭제하지 않는다**(flock 은 프로세스 종료 시 커널이 해제. 삭제하면 상호배제가 깨짐).
- [ ] 7.3 락 아래 재읽기 → 유효하면 `Adopted`(refresh 없이 반환) → 아니면 refresh 1회.
- [ ] 7.4 새 refresh_token 을 **락 보유 중 disk-first** 로 저장한 뒤 인메모리 갱신.
- [ ] 7.5 refresh 후 원 요청을 **1회만** 재시도.
- [ ] 7.6 flock 획득 타임아웃 값과 타임아웃 시 오류 의미를 정의한다(대기 중 refresh 지연 포함).
- [ ] 7.7 `authorize=auth.tesla.com`, `refresh=fleet-auth…` 고정. 401 을 무조건 refresh 로 해석하지 않고
      분류 후 1회 제한. POST 예외를 auth 모듈에만 격리(`#[allow]` + 사유 주석).
- [ ] 7.8 `TokenStore` trait — `get()` + **CAS 계약**. `FileTokenStore`/`MemoryTokenStore`.
      여러 호스트가 같은 토큰을 공유하는 구성은 명시적 non-goal.
- [ ] 7.9 기존 `~/.tesla/tokens.json` 스키마 호환 여부를 결정하고 문서화한다(호환/마이그레이션/범위 외).

_Requirements: REQ-15, REQ-17, REQ-18_

## 8. 리전·스코프 선검사

- [ ] 8.1 `/users/region` 으로 base URL 해석, `OnceCell` 캐시, 실패 시 NA 폴백.
      **`OnceCell` 은 리전 캐시 전용** — 토큰에는 쓰지 않는다.
- [ ] 8.2 NA 폴백이 잘못된 리전 계정에 미치는 영향을 로그로 남기고 캐시 무효화 경로를 둔다.
- [ ] 8.3 JWT `scp` 를 네트워크 없이 decode 해 대조. 불일치 시 `MissingScope`(과금 0).
- [ ] 8.4 **fail-open** — 파싱 실패·비 JWT 토큰이면 막지 않고 통과시킨다.
      서명·exp 를 검증하지 않으므로 **권한의 근거가 아니라 최적화**임을 문서화. 최종 판정은 서버 403.

_Requirements: REQ-8, REQ-20_

## 9. 엔드포인트 16개와 편의 API

- [ ] 9.1 vehicle 12개 · user 4개의 응답 타입. 확정 못 하는 Tesla 필드는 과도하게 고정하지 말고
      `Value` 또는 선택적 중첩 타입으로 둔다.
- [ ] 9.2 endpoint tag 16개 — 각 tag 에 path·response·scope·query·pricing 을 빠짐없이 명시.
      `vehicle_data`·`nearby_charging_sites` 2개만 `Data { usd_per_request: 0.002 }`, 나머지 14개는 `NotCharged`다.
      user endpoint는 user-context 토큰과 `user_data` 계열 스코프를 요구하고 3rd-party business 토큰과 비호환이며, `/users/region`은 여러 허용 스코프 중 하나를 사용한다.
- [ ] 9.3 **`vehicle_data` 위치 이중 opt-in 을 9.2 와 함께 처리한다.**
      위치 미요청 시 `location_data` query 와 `vehicle_location` 스코프를 **둘 다** 제외,
      요청 시 **둘 다** 포함. builder API 로 한쪽만 빠질 수 없게 만든다.
      ※ 스코프 메타데이터를 먼저 일괄 선언한 뒤 나중에 고치면 잘못된 공개 API 가 나간다.
- [ ] 9.4 vehicle endpoint 에 VIN 접근자 + `VehicleEndpoint` 구현.
- [ ] 9.5 `Client` 에 16개 편의 메서드 — 전부 6장 파이프라인에 위임. guarded 변형도 제공.

_Requirements: REQ-1, REQ-2, REQ-6_

## 10. 페이지네이션과 rate limit

- [ ] 10.1 `/vehicles` query `page`+`per_page`와 기본 페이지 100, `/invitations` query `page`+`per_page` 및 1..=25 검증, `Page<T>` 에 `pagination { previous, next, current, per_page, count, pages }` 노출.
- [ ] 10.2 기본 편의 메서드는 **한 페이지만** 가져온다.
- [ ] 10.3 `all_pages(max_pages)` — `max_pages > 0` 요구, 반복 page 감지, doc 에 호출 수와 rate limit 경고.
- [ ] 10.4 **클라이언트 측 rate limit 보호** — Realtime Data 60/분에 대한 로컬 백프레셔.
      ※ 수치가 문서에만 있고 구현이 없으면 429 를 맞고 나서야 알게 된다.
      계정·차량 단위 공유 제한이므로 로컬 카운터의 한계도 함께 문서화한다.
- [ ] 10.5 `nearby_charging_sites`는 page/per_page를 받지 않고 `count`·`radius`·`detail`만 표현하는 별도 `QuerySpec`을 사용한다.

_Requirements: REQ-6, REQ-13_

## 11. 비용 안전장치

- [ ] 11.1 기본값이 **유료 호출 전 무료 state 게이트(6.2) + 재시도 억제(3.4)** 를 보장하도록 배선.
- [ ] 11.2 `pricing()` 을 공개해 호출 전 비용을 확인할 수 있게 하고 2개 Data/14개 NotCharged 분류를 고정한다.
- [ ] 11.3 월 $10(=Data 5,000회) 내 운영 가정을 문서화하고, 이 가드가 **로컬 카운터**인지
      계정 실사용량인지 명시한다(여러 프로세스가 카운터를 공유하지 않음).
- [ ] 11.4 무과금 endpoint는 재시도해도 중복과금이 없음을 문서화하되, 기본 `max_attempts=1`, 재시도 allowlist, 60회/분 rate limit을 그대로 적용한다.

_Requirements: REQ-22_

## 12. 테스트

- [ ] 12.1 무과금 유닛·fixture 계약 테스트 — path/scope/pricing/gate/billing/retry/redaction/봉투/
      Content-Type/decode 오류. **0.8 의 mock 하니스 위에서 돈다.**
- [ ] 12.2 `billing_classification_matrix` — 3.2 판정표의 각 행과 14개 NotCharged 고정 판정을 테스트로 고정.
- [ ] 12.3 `retry_never_on_unknown_billing` — `TransportUnknown` 재시도 금지 단정.
- [ ] 12.4 `serde_absorbs_unknown_field_into_extra` / `serde_default_fills_missing_with_none`
      — serde 업그레이드로 `flatten` 동작이 바뀌면 감지한다.
- [ ] 12.5 락 동시성 — `concurrent_refresh_single_flight`, `cross_process_lock_adopts`,
      `lock_timeout_errors_without_unlink`.
      ※ multi-process 재현은 플랫폼 의존적이다. CI 에서 돌릴 OS·권한 전제를 명시하고,
      불가하면 `#[ignore]` + 수동 실행 런북으로 분리한다.
- [ ] 12.6 `redact_removes_vin_coords_tokens` — 4.4 의 보장 범위에 한해 검증.
- [ ] 12.7 불변식 I1~I5 테스트(I5 는 Data endpoint 408의 `billing()==Billed`와 NotCharged endpoint 408의 `billing()==NotBilled` 단정 포함).
- [ ] 12.8 `pricing_catalog_has_exactly_two_data_endpoints`와 `pagination_uses_page_per_page` 및 nearby 전용 query 테스트.

_Requirements: REQ-20_

## 13. 라이브 드리프트 캡처 (보호된 환경 전용)

- [ ] 13.1 주 1회 라이브 16개 캡처 → 정제 → `fixtures/live/<endpoint>/<state>.json`.
- [ ] 13.2 **구조 지문(fingerprint) diff** — 키셋만 비교하면 타입·nullability·enum·배열 구조 변화를 놓친다.
- [ ] 13.3 차량 상태(주차·주행·충전)별 캡처를 **시도**하되, 상태를 통제할 수 없으므로
      **부분 성공을 허용**하고 어떤 상태가 미수집인지 기록한다(태스크 실패로 치지 않는다).
- [ ] 13.4 정제기 자체 테스트. 원본 임시 파일 삭제, CI artifact 미업로드, 이슈 본문 정제.
- [ ] 13.5 드리프트 발견 시 이슈 생성 — 대상 트래커·자격증명·**중복 이슈 억제**·비밀 미포함 검증.
      ❗ 조용히 `Option` 붙이고 넘어가지 말 것.
- [ ] 13.6 실제 호출 수(게이트·리전·페이지 포함)를 계측해 **비용 추정치를 실측으로 갱신**한다.
      ※ "16개 endpoint 각 1회 중 유료 2회 ≈ 월 $0.02" 는 기본 경로 가정이며 부가 호출을 포함하지 않는다.

_Requirements: REQ-20_

## 14. 릴리스·정직성

- [ ] 14.1 릴리스 게이트 CI — `cargo clippy -- -D warnings`, `cargo semver-checks`,
      `cargo deny check`, `cargo test --locked`, MSRV job. (0.7 의 PR CI 와 구분)
- [ ] 14.2 **문서 정직성 검토** — crate 문서에 아래 세 가지를 명시한다.
      ① 라이브 검증 16개 vs 제외 12개(charging/energy/partner) 구분
      ② 스코프 선검사는 **권한 근거가 아니라 최적화**
      ③ fixture 계약 테스트는 **드리프트 감지가 아니다**
- [ ] 14.3 clippy `disallowed_methods` 만으로는 쓰기 금지가 닫히지 않는다는 **잔여 위협을 문서화**한다
      (다른 HTTP crate·`allow` 확산). 본진은 타입 시스템과 모듈 가시성임을 명시.
- [ ] 14.4 요금·rate limit 수치에 **출처 URL 과 조회 시각**을 붙이고 분기 재검증 주기를 적는다.

_Requirements: REQ-21, REQ-22_

---

## REQ 커버리지 확인

구현 완료 판정 전에 점검한다.

- [ ] `requirements.md` 의 모든 REQ-ID 가 이 문서에서 최소 1회 참조되는가
- [ ] 각 REQ 의 수용기준이 대응 태스크의 완료 조건과 어긋나지 않는가
- [ ] 완료 판정 기준이 없는 태스크가 남아 있지 않은가

## 순서 불변식 (되돌리지 말 것)

| 반드시 | 이유 |
|---|---|
| 3장(에러) → 4장(redaction) → 3.6 배선 | 임시 치환이 "단일 계층"을 깨뜨린다 |
| 6장(파이프라인) → 9장(편의 API) | 편의 메서드는 파이프라인에 위임. 역순이면 빅뱅 통합 |
| 9.2(스코프 선언) 와 9.3(위치 opt-in) 동시 | 먼저 일괄 선언하면 잘못된 공개 스코프가 나간다 |
| 0.8(mock 하니스) → 12장(테스트) | 하니스 없이 계약 테스트를 쓸 수 없다 |
| 0.7(PR CI) → 12장 | 테스트가 "작성만 되고 안 도는" 공백을 막는다 |

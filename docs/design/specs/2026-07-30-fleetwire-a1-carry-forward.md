# fleetwire A1 완료 — A2/A3 로 넘기는 항목

> A1(core 타입·에러·redaction·decode 게이트) 완료 시점의 이월 목록.
> 코드 최종 HEAD `8849426` (`<workspace>`, 16+1 커밋, 121 테스트, 최종 리뷰 판정 **Ship**).
> SDD 워크스페이스(ledger·태스크 리포트·리뷰 패키지)는 삭제됨 — 판단 근거가 필요한 항목만 여기 보존한다.

## A2(auth·토큰) 착수 전 반드시 결정할 것

**1. 미지 스코프 처리 — `Scope` 에 `#[serde(other)]` 가 없다.**
A1 fix wave 가 `Scope` 를 `rename_all = "snake_case"` 로 고쳐 와이어 인코딩은 맞췄다(그 전엔 `"VehicleDeviceData"` 를 뱉었고 serde 테스트가 0개라 안 보였다). 하지만 모델링된 variant 는 4개뿐이고, **실제 요청 scope 문자열에는 `offline_access` 가 있다** — refresh token 을 주는 바로 그 스코프다. Tesla 는 `vehicle_cmds`·`energy_device_data` 등도 발행한다. 지금 상태로 토큰 응답의 scope 문자열을 파싱하면 미지 스코프에서 실패한다. A2 의 첫 결정사항.

**2. `Auth` variant 의 pricing 은 이미 들어가 있다.**
`Auth { message: String, pricing: Option<Pricing> }`. A1 에서 발견된 실제 오보고(유료 `vehicle_data` 의 401 을 `NotBilled` 로 보고 — Tesla 는 $0.002 를 청구)를 닫으면서 오너 판정으로 확정한 모양이다. A2 의 토큰 갱신 실패는 엔드포인트 맥락이 없으므로 `pricing: None` 이 맞다.

**3. `MissingScope` 는 여전히 pricing 이 없다.**
네트워크 전 오류라 `NotBilled` 이 정확하다. 바꿀 필요 없음.

## A3(HTTP 전송) 로 넘어간 항목

**4. `redact_transport_error(reqwest::Error)` 미구현.**
REQ-18 이 요구하는 "`reqwest::Error` 의 URL·source chain·`Debug` 출력 정제" 는 A1 범위 밖으로 명시 지연됐다. `reqwest` 가 의존성에 들어오는 시점에 `redaction` 모듈 안에 만들 것 — 밖에서 `redact_text` 를 직접 부르는 형태는 금지(A1 이 `redact_json`/`redact_text` 를 private 으로 내린 이유다. `body_preview` 만이 안전한 진입점이라는 계약을 타입으로 강제한 상태).

**5. `body_preview` 의 전체 바디 비용.**
256바이트 프리뷰를 만들려고 멀티메가 바디를 통째로 파싱·재직렬화·regex 스캔한다. **순진한 사전 절단은 위험하다** — redaction 이 텍스트를 줄이므로 잘린 조각이 가시 창으로 이동할 수 있다. A3 가 REQ-13 의 bounded body stream 을 만들 때 거기서 상한을 걸 것.

**6. `Error::Api.message` 가 정보 0.**
현재 `status.canonical_reason()` 만 담아서 `HTTP 502 Bad Gateway: Bad Gateway` 로 렌더된다(`StatusCode` 의 Display 가 이미 reason 을 포함). Tesla 의 `error`/`error_description` 을 `body_preview` 경유로 채우는 게 A3 후속.

**7. `serde_path_to_error` 도입 검토.**
A1 의 `Decode.message` 는 `Category::Data` 일 때 값을 통째로 버리고 `invalid data at line N column M` 만 남긴다(바디 값이 그 카테고리에서만 보간되기 때문 — serde_json 1.0.151 소스로 검증). 대가로 `missing field \`token\`` / `duplicate field \`x\`` 처럼 **Rust 구조체 유래라 안전하고 스키마 드리프트 진단으로는 최고 가치인** 메시지까지 함께 잃는다. 같은 카테고리의 `unknown_field` 가 바디 유래 키를 에코하므로 카테고리만으로는 분리 불가. `serde_path_to_error` 가 정밀도를 회복시킨다.

**8. clippy `disallowed_methods` 가 없다.**
REQ-19 와 design §12 는 read-only 강제를 3계층으로 요구하는데, 현재 `[workspace.lints.rust] unsafe_code = "forbid"` 만 있고 `clippy.toml` 도 없다. tasks.md 0.4 가 shell(A1 이전)에 할당했으나 아무도 들고 있지 않다. 지금은 HTTP 의존성이 없어 안전하지만, **A3 가 바로 그 메커니즘이 이미 존재해야 하는 시점이다.** `reqwest::Client::{post,put,delete,request}` 경로는 A3 에서 추가(지금 미해결 경로를 넣으면 `-D warnings` 가 깨진다). 자매 레포의 `check-isolation.sh` 에 해당하는 것도 fleetwire 엔 없다.

## redaction 의 알려진 구멍 — 한 번의 설계 패스로 함께 처리할 것

A1 은 redaction 에 fix 라운드를 4회 썼고, 매번 **열거한 목록이 실제 형태에 뒤처져서** 샜다(`native_*` → `active_route_*` → `Access-Token`). 아래는 같은 뿌리를 공유하므로 **개별로 고치면 whack-a-mole 이 재발한다.** 비-REST 필드 형태(Fleet Telemetry 등)가 범위에 들어오는 시점에 한 번에 설계할 것.

- **camelCase 키**: `{"activeRouteLatitude":37.4419,"activeRouteDestination":"1234 Elm St"}` 는 좌표와 주소를 **둘 다** 통째로 흘린다. 넷 중 가장 나쁨.
- **destination 접미 앵커가 단방향**: `*_destination` 은 잡고 `destination_*` 은 못 잡는다. REST `vehicle_data` 엔 해당 필드가 없으나 Fleet **Telemetry** 는 `DestinationName`/`DestinationLocation` 을 쓴다.
- **점 표기 키**: `"drive_state.latitude":37.5665` 통과.
- **산문 속 좌표**: `{"msg":"vehicle at latitude 37.5665"}` 통과. 키 앵커 설계의 본질적 한계.
- **비문자열 destination 값**: 중첩 객체 안의 주소는 miss.
- 뿌리: `(?:[a-z0-9]+_)*` 접두 그룹이 밑줄로 끝나는 세그먼트를 요구한다 — 그게 `"flat"`/`"salon"` 오탐을 막아주는 정밀성의 대가다. 아마 `(?i)` 를 버리고 명시적 대소문자 교대로 가야 한다.
- **allowlist 커버리지 상한**: 정규화(`-`/`_`/공백 제거 + 소문자)는 구분자·대소문자·camelCase 철자만 덮는다. 프록시가 에코할 법한 접두형 — `X-Auth-Token`, `X-Access-Token`, `Proxy-Authorization`(그 `Basic <b64>` 값도 어느 regex 도 못 잡음) — 과 Tesla `vehicles` 응답의 복수형 `tokens` 배열은 여전히 통과. A1 지시가 "5키 유지" 였으므로 미스가 아니라 범위 밖.

## 원리상 닫히지 않는 것 (설계 변경 필요)

**9. `kv_secret` 멱등 가드의 잔여 우회.**
`code=[REDACTED]<secret>` 는 통과한다. 값 클래스가 `]` 를 제외하므로 2패스가 정확히 `[REDACTED` 를 캡처 → 멱등 가드 발동 → 뒤 문자열 무처치. **`caps[2]` 검사만으로는 폐쇄 불가**: redaction 이 단사 함수가 아니라, `code=[REDACTED]x` 가 `code=SECRET]x` 의 정상 1패스 결과인지 통째 주입인지 바이트로 구분할 수 없다. 진짜 폐쇄는 단일 패스 redaction 이거나, 첫 글자가 값 클래스 밖인 sentinel 이 필요하다.
⚠️ `redaction.rs:466-468` 주석이 현재 "no bypass surface" 로 **보증처럼 읽힌다** — 문구를 완화할 것.

## 사소하지만 1.0 프리즈 전에 정할 것

- **`ScopeExpr::missing()` 이름이 과대약속**: 미충족분이 아니라 요구 표현 전체를 돌려준다. doc 은 정정됐고 이름은 보류 — `unsatisfied` 등으로 개명하거나 실제 미충족 스코프를 반환하도록 바꿀 것. 아직 호출자 없음.
- **`Page<T>` doc 과 `non_exhaustive` 모순**: doc 은 "호출자가 수동 조립" 인데 non_exhaustive 가 다운스트림 구조체 리터럴을 막고 `Page::new` 도 없다. 생성자를 주거나 doc 을 "crate 의 decode 경로가 조립" 으로 고칠 것.
- **`Pagination` 에만 `flatten extra` 가 없다**: REQ-4 문구는 "every response model" 인데 design §6 과 brief 둘 다 빠뜨렸다. 무크래시 보장은 성립(`deny_unknown_fields` 부재). **규칙을 한 번 정해서 A3 의 모든 모델 타입에 동일 적용할 것.** `#[serde(default)]` 비대칭(VehicleSummary 는 명시, Pagination 은 serde 기본동작 의존)도 같이 결정.
- **`Pricing` 이 `f64` 위에 `PartialEq` 를 파생**: 공개 비교에서 부동소수 등치는 함정이고, 그래서 형제인 `Billing` 과 달리 `Eq`/`Hash` 가 안 된다. 정수(1/10 밀리센트)로 저장하거나 `usd_per_request()` 를 노출하고 파생을 뺄 것.
- **lib.rs re-export 비대칭**: `FleetGet`/`VehicleEndpoint` 는 재수출됐는데 **모든 호출이 반환하는** `VehicleSummary`/`Page`/`Pagination` 은 빠졌다.
- **`VehicleEndpoint` 는 구현체도 테스트도 0**: A3 의 첫 실제 엔드포인트 타입이 생길 때 `query()`/`vin()` 커버리지를 함께 넣을 것. sealed 컴파일 실패의 `trybuild` 테스트도 그때.
- **라이선스 스펙 불일치**: 스펙 §1·REQ-21 은 Apache-2.0, 레포는 듀얼 `MIT OR Apache-2.0`. 듀얼이 Rust 생태계 관행상 낫다 — **코드가 아니라 스펙을 고칠 것.**
- **acceptance 게이트 설계 결함**: T6 가 쓴 `grep -q 'deny_unknown_fields' src/` 는 코드와 주석을 구분하지 않아, 그 속성을 **왜 안 쓰는지** 실제 이름으로 문서화하면 게이트가 깨진다. A3 의 모든 모델 태스크가 이 게이트를 그대로 재사용할 예정이므로 먼저 고칠 것.

## A2/A3 가 물려받아야 할 테스트 관행

A1 최종 리뷰가 명시적으로 "A2/A3 의 템플릿" 으로 지목한 것:

- **기대값을 피검증 대상이 아닌 독립 출처에서 유도하라.** `decode` 의 serde 에러 테스트는 line/column 을 `serde_json` 에서 직접 뽑는다 — `decode` 가 계산한 값을 되풀이 단언하지 않는다.
- **`contains` 대신 전체 문자열 `assert_eq!`.** 이 브랜치의 이중 대괄호 버그(`[REDACTED]]`)가 green 아래 살아남은 이유가 `contains` 였다.
- **구조적 속성을 테스트하라.** `destination_pattern_is_suffix_anchored_not_enumerated` 처럼 "손으로 고른 필드 집합" 이 아니라 설계 속성 자체를 고정하는 테스트가 "수정에 맞춘 테스트" 사이클을 실제로 끝냈다.
- **입력은 assertion 이 통과하게 해주는 것이 아니라 약점을 드러내는 것으로 고르라.** A1 이 이걸 4번 틀렸다: round 1 은 `native_*` 를 뺀 페이로드, round 2 는 `active_route_*` 를 뺀 페이로드, round 3 은 값으로 `"Home"`, round 4 는 1000바이트 needle 을 256바이트 haystack 에서 찾기(산술적으로 실패 불가). 마지막 사례는 위 parked #3 에 남아 있다.

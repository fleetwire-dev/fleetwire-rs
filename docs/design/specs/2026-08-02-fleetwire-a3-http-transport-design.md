# fleetwire A3 — HTTP 전송 계층 설계 스펙

> 서브프로젝트 A(읽기 라이브러리)의 세 번째 조각. A1(코어 타입·에러·redaction·decode)과
> A2(토큰 저장·회전·프로세스간 single-flight)가 만든 자리에 **네트워크**를 넣는다.
>
> | | |
> |---|---|
> | 코드 레포 | `<workspace>` branch `main`, BASE `40858d8` (A2 완료 시점, 227 테스트) |
> | 상위 스펙 | `2026-07-24-fleetwire-read-client-design.md` (§5 HTTP·§6 에러/과금 확정분) |
> | 직전 이월 | `2026-07-31-fleetwire-a2-carry-forward.md` |
> | 범위 결정 | **1안 수직 슬라이스** — 전송 + `FleetGet` + 구조적으로 필요한 2 엔드포인트 |
> | 작성 | 2026-08-02 KST |

---

## 0. A3 가 끝났을 때 참인 것

1. `Client` 로 Tesla Fleet API 를 실제로 **읽을 수 있다**(공개 API, end-to-end).
2. 크레이트 전체에서 POST 는 **정확히 한 곳**(토큰 refresh)이고, 그 사실을 **타입·가시성이
   보장하고 clippy 가 거든다.** ⚠️ "3계층이 강제한다"는 표현은 쓰지 않는다 — clippy 는
   `Request::new(Method::POST, …)`·`RequestBuilder` 우회·`allow` 확산·다른 HTTP crate 추가를
   막지 못한다(§3). 강제는 1·2 계층이고 3은 실수 방지다.
3. **401·412 는 실측 증거로** 분류된다(§1). ⚠️ **408·5xx 는 증거가 없다** — 상위 스펙의
   문서 근거만 있고 실응답을 본 적이 없다(O10). 이 둘은 "증거 기반"이라 부르지 않는다.
4. 토큰 회전이 **한 프로세스 안에서는** 폭주할 수 없다. ⚠️ 프로세스 간 보장은 §6.4 의
   저장 위치 결정에 달려 있고, Tesla 측 재사용 탐지 정책은 여전히 미결(O7)이다.
5. **Bearer 토큰이 Tesla 호스트 밖으로 나가지 않는다** — 서버가 준 리전 URL 검증 + redirect 차단(§4.5).
6. 에러 메시지에 자격증명·차량 식별자·**계정 식별자**가 남지 않는다.

## 1. 이 설계를 떠받치는 실측 증거

⚠️ **아래는 2026-08-01 20:06~20:15 KST 실측이다.** 추정이 아니다. 재검증 주기: 분기 1회 또는 Tesla 공지 시.
전문·원본 응답은 **§16 부록**.

### E1. 401 은 세 형태로 오고, 두 개의 상류에서 나온다

| 케이스 | 요청 | status | content-type | body | `WWW-Authenticate` |
|---|---|---|---|---|---|
| A | `Bearer xxxxxxxxxxxx` | 401 | `application/json` | `{"error":"invalid authentication"}` | **없음** |
| E | 형태만 JWT(`h.p.sig`) | 401 | `application/json` | `{"error":"invalid bearer token"}` | **없음** |
| B | Authorization 헤더 없음 | 401 | **`text/html`** | **0바이트** | `Bearer realm="Doorkeeper", error="invalid_token", error_description="The access token is invalid"` |

`x-envoy-upstream-service-time` A/E=1ms vs B=5ms, B 에만 `x-request-id`·`pragma` → **다른 상류**(B = Doorkeeper, Rails OAuth gem).

**따라서 401 의 형태는 "토큰이 왜 나쁜가"가 아니라 "어느 계층이 거절했는가"를 반영한다.**

### E2. 리전을 틀리면 412 로 크게 실패한다

| 요청 | status | body |
|---|---|---|
| NA `/api/1/users/region` | 200 | `{"response":{"region":"na","fleet_api_base_url":"https://fleet-api.prd.na.vn.cloud.tesla.com"}}` |
| EU `/api/1/users/region` | **412** | `{"error":"Account <uuid> must be registered in the current region https://…eu…, please see …"}` |
| EU `/api/1/vehicles` | **412** | 동일 |

### E3. OIDC discovery 는 refresh 엔드포인트의 권위가 **아니다**

`/oauth2/v3/.well-known/openid-configuration` 은 두 호스트가 동일 문서를 주고,
`token_endpoint = https://auth.tesla.com/oauth2/v3/token`, `grant_types_supported = ["authorization_code"]` 뿐이다.
그런데 배포 클라이언트는 `fleet-auth.prd.vn.cloud.tesla.com` 으로 refresh 해서 **성공한다**.
실토큰 `iss` 는 `https://auth.tesla.com/oauth2/v3/nts` 로 discovery 의 `issuer` 와 **불일치**한다(`/nts` 접미).

### E4. 토큰 audience 에 NA·EU 가 둘 다 있다

`aud = ["…/oauth2/v3/userinfo", "https://fleet-api.prd.na…", "https://fleet-api.prd.eu…"]`
→ 리전 거절은 audience 검증이 아니라 **계정 등록 상태**로 이뤄진다. `aud` 로 리전을 유도할 수 없다.

### E5. 412 응답이 계정 UUID 를 평문으로 흘린다

`Account 801802ed-…-f69fc59b2ef0 must be registered…` — 현재 redaction 대상(Bearer/JWT/`refresh_token`/`client_secret`/VIN/좌표)에 **계정 식별자가 없다**.

### E6. ⚠️ A3 가 구현하는 두 엔드포인트는 **둘 다 무과금**이다

O1 증거 매트릭스(`2026-07-29-…-o1-endpoint-evidence-matrix.md`)의 최대 산출:
**16개 중 유료는 `vehicle_data`·`nearby_charging_sites` 2개뿐**이고 나머지 14개는
`pricing_category: null`("This endpoint is not charged.")이다.

| A3 대상 | 과금 |
|---|---|
| `GET /api/1/vehicles` | **무과금** |
| `GET /api/1/vehicles/{vin}` (state 게이트) | **무과금** — "게이트가 무료"라는 §10.1 의 전제 |
| `GET /api/1/users/region` | **무과금** |

**따라서 F3("status<500 = billed")은 유료 카테고리에 한정된 규칙이다.**
무과금 엔드포인트는 **status 불문 `Billing::NotBilled`** 이며, A1/A2 가 이미 이렇게 구현했다
(`Error::billing()` 이 각 variant 의 `pricing` 필드를 먼저 본다).

⚠️ 이 사실은 A3 의 비용 논증 전반을 바꾼다 — 401 복구·412 재해석·리전 폴백 실패는
**A3 가 내보내는 엔드포인트 범위에서는 돈이 들지 않는다.** 비용 논증은 A4 가 유료 2개를
들여올 때 실효가 생긴다. 아래 본문은 이 구분을 지켜서 쓴다.

---

## 2. 모듈 구조

⚠️ **A1 이 이미 만들어 둔 것을 다시 만들지 말 것** — 초안이 이 경계를 잘못 그었다.

| 이미 있음 (A1/A2) | 상태 |
|---|---|
| `endpoint.rs` — `FleetGet`(sealed) + `VehicleEndpoint{ fn vin() }` | ✅ 완성. **trait 을 새로 만들지 않는다** |
| `query.rs` — `QuerySpec{new,push,to_encoded}`, `encode_vin_segment()` | ✅ 완성 |
| `decode.rs` — `decode(status, content_type, body, pricing)` | ✅ **status-first** 게이트 완성. A3 는 호출자일 뿐 |
| `redaction.rs` — `body_preview(raw, max)` (`pub(crate)`, 유일 진입점) | ✅ |
| `auth/` — `TokenManager`, `TokenRefresher`, `Refreshed::new(a, r, secs)` | ✅ |
| `model.rs` — `VehicleSummary`, `Pagination`, `Page<T>` | ✅ |

A3 가 **새로 만드는** 것:

```
crates/fleetwire/src/
├── client.rs         # Client + 빌더. reqwest::Client 소유, 리전 캐시 보유
├── transport.rs      # 요청 실행: 타임아웃·크기상한·3xx 차단·헤더 파싱·재시도
├── auth/
│   └── refresh.rs    # TokenRefresher 의 reqwest 백엔드 — 크레이트 유일의 POST
└── api/
    ├── mod.rs        # 구체 엔드포인트 타입의 모듈 루트 (trait 은 endpoint.rs 소유)
    ├── users.rs      # GET /api/1/users/region
    └── vehicles.rs   # GET /api/1/vehicles, GET /api/1/vehicles/{vin}
```

신규 파일: `clippy.toml`(레포 루트). **reqwest 를 추가하는 커밋과 같은 커밋**에 들어간다(§3).

A3 가 **기존 파일을 수정**하는 것:
`decode.rs`(`retry_after` 채우기 · `message` 채우기 · `#[allow(dead_code)]` 제거) ·
`auth/mod.rs`(§5.1 세대 + §6 회전 간격) · `lib.rs`(신규 모듈 export) · `Cargo.toml`(reqwest·wiremock).

**`VehicleEndpoint` 는 이미 있다.** 검토에서 잡혔던 "`send_guarded` 제네릭 구현 불가 — trait 에 VIN 이
없다"는 A1 이 `fn vin(&self) -> &str` 로 이미 닫았다. A3 는 그 trait 을 **구현**하기만 한다.

---

## 3. 읽기 전용 3계층 강제 (REQ-19 / 상위 스펙 §3)

| 계층 | 수단 | 성격 |
|---|---|---|
| 1 | **타입 시스템** — 공개 API 에 POST 생성 경로 없음. `send_raw` 도 GET 고정 | 본진 |
| 2 | **모듈 가시성** — `reqwest` 를 `pub` 로 노출하지 않음 | 본진 |
| 3 | **clippy `disallowed_methods`** | 보조 |

```toml
# clippy.toml  (신규)
disallowed-methods = [
  { path = "reqwest::Client::post",    reason = "fleetwire 는 GET 전용" },
  { path = "reqwest::Client::put",     reason = "동일" },
  { path = "reqwest::Client::patch",   reason = "동일" },
  { path = "reqwest::Client::delete",  reason = "동일" },
  { path = "reqwest::Client::head",    reason = "동일" },
  { path = "reqwest::Client::request", reason = "메서드 임의 지정 우회 방지" },
]
```
```toml
# Cargo.toml — 린트를 켜야 효력이 있다
[workspace.lints.clippy]
disallowed_methods = "deny"
```

⚠️ **`clippy.toml` 은 reqwest 도입과 반드시 같은 커밋.** 나중에 붙이면 그 사이 커밋이 무방비다
(carry-forward #3). 현재 레포에 `clippy.toml` 이 **아예 없고** `unsafe_code = "forbid"` 하나뿐이다.

예외는 `auth/refresh.rs` 의 refresh POST **한 곳**만 `#[allow(clippy::disallowed_methods)]` + 사유 주석.

> clippy 는 보조 방어선이다 — 다른 HTTP crate 추가나 `allow` 확산으로 우회 가능하다.
> 이 사실을 크레이트 문서에 명시한다.

---

## 4. 리전 해석

### 4.1 캐시 — 세대(generation)를 가진 무효화 가능 구조

⚠️ **`OnceCell` 을 쓰지 않는다.** 초안은 `tokio::sync::OnceCell::get_or_try_init` 을 골랐으나
§4.3 이 "412 수신 시 캐시 무효화"를 요구한다 — **`OnceCell` 은 무효화가 불가능하다.**
설계 자기모순이었고 검토에서 적발됐다.

```rust
struct RegionCache {
    inner: tokio::sync::RwLock<Option<(u64 /* generation */, Url)>>,
}
```

- 읽기는 read guard 로 `(generation, url)` 을 함께 가져온다.
- 무효화는 **자기가 본 generation 을 제시할 때만** 성립한다(compare-and-clear).
  그래야 요청 A 의 412 가, 그 사이 요청 B 가 이미 갱신해 둔 새 리전을 지우지 않는다.
  이것은 §5 의 토큰 세대 규칙과 **같은 문제·같은 해법**이다.
- 실패는 캐시하지 않는다(값을 `Some` 으로 만들지 않으면 그만).

### 4.2 순서

1. 캐시에 있으면 사용.
2. 없으면 `GET {부트스트랩 호스트}/api/1/users/region` → `response.fleet_api_base_url` 을
   **§4.5 검증 통과 후** 캐시.
3. 해석 자체가 실패하면 **부트스트랩 호스트로 폴백**.

### 4.2.1 ⚠️ 부트스트랩 호스트는 설정 가능해야 한다

초안은 NA 를 무조건 폴백으로 삼았다. **EU 계정에서는 이것이 오진을 고정한다** — 리전 해석이
실패하면 NA 로 폴백하고, NA 는 412 를 주고, 412 복구는 다시 리전 해석을 시도하고, 그것도
실패하면 다시 NA 로 폴백한다.

→ `Client` 빌더에 **`bootstrap_region`** 을 노출하고 기본값만 NA 로 둔다.
   `region` 응답을 한 번이라도 성공적으로 받으면 그 값이 이긴다.

### 4.3 412 복구 — 폴백이 안전한 이유

E2 가 보인 대로 **리전이 틀리면 조용한 오작동이 아니라 412** 가 난다. 그래서 NA 폴백은 안전하다.

412 를 받으면: **리전 캐시 무효화 → 1회 재해석 → 원요청 1회 재전송.**
요청 컨텍스트의 `region_retry_used: bool` 로 루프를 차단한다(§5 의 401 정책과 동일한 모양).

⚠️ **412 메시지를 파싱해 올바른 리전을 유도하지 말 것.** 메시지가 담은 URL 은
*네가 보낸 리전*이지 *올바른 리전*이 아니다. `/users/region` 호출만이 권위다.

**412 의 비용.** E6 대로 `/users/region`·`/vehicles` 는 무과금이므로 **A3 범위에서 412 는 $0** 이다.
리전 캐시는 여기서 **지연·rate limit 장치**이지 비용 장치가 아니다. 비용 장치가 되는 건 A4 가
`vehicle_data`(유료)를 들여올 때다 — 그때는 리전을 틀린 412 가 실제로 과금된다.
⚠️ 그러니 **"리전 캐시는 비용 절감"이라고 doc 에 쓰지 말 것** — A3 시점에서 거짓이다.

### 4.4 auth 호스트 — discovery 를 따르지 않는다

refresh 는 `fleet-auth.prd.vn.cloud.tesla.com` 하드코딩을 유지한다. E3 근거를 `auth/refresh.rs`
rustdoc 에 **그대로 적는다**. 근거를 안 적으면 후속 기여자가 "정확성"을 이유로 discovery 를
도입해 refresh 를 깨뜨린다. 마찬가지로 **엄격한 OIDC `issuer` 검증을 넣지 않는다** — 실토큰의
`iss`(`…/oauth2/v3/nts`)가 discovery 의 `issuer`(`…/oauth2/v3`)와 달라 **자기 토큰을 거부**하게 된다.

이로써 상위 스펙 §10 의 **O6 이 해소된다**(I2 = discovery 부적합 확인, I3 = 412 로 안전 확인).

### 4.5 🔴 리전 URL 신뢰경계 — Bearer 토큰 유출 차단

**이것이 이 설계에서 가장 중요한 보안 항목이다.** `fleet_api_base_url` 은 **서버가 준 문자열**이고,
그 URL 로 **Bearer 토큰이 실려 나간다.** 검증 없이 쓰면 응답 변조·서버 오작동·DNS 오염이
곧 **자격증명 유출**이다. (검토에서 적발. 초안에 검증이 전혀 없었다.)

캐시에 넣기 **전에** 다음을 전부 통과해야 한다. 하나라도 실패하면 **거부하고 부트스트랩 호스트를 유지**한다:

| # | 검사 | 이유 |
|---|---|---|
| 1 | scheme == `https` | 평문 전송 차단 |
| 2 | host 가 **Tesla allowlist** 에 정확히 일치 | 임의 호스트 전송 차단 |
| 3 | userinfo(`user:pass@`) 없음 | 자격증명 스머글링·파서 혼동 차단 |
| 4 | port 미지정(또는 443) | 내부망 포트 스캔 차단 |
| 5 | path 가 비었거나 `/` | path prefix 주입 차단 |
| 6 | query·fragment 없음 | 동일 |

allowlist 는 **정확한 호스트 집합**으로 둔다 — `*.tesla.com` 같은 접미 매칭은
`evil-tesla.com`·`tesla.com.attacker.net` 류에 취약하다.

```
fleet-api.prd.na.vn.cloud.tesla.com
fleet-api.prd.eu.vn.cloud.tesla.com
fleet-api.prd.cn.vn.cloud.tesla.cn      # 중국 리전 (미검증 — O11)
```

⚠️ allowlist 는 **Tesla 가 리전을 추가하면 깨진다.** 그래서 빌더에
`allow_region_host(host)` 를 두어 사용자가 확장할 수 있게 하되, **기본값은 닫아 둔다.**
"열려 있는 편이 편하다"로 기본값을 뒤집지 말 것 — 실패 모드가 자격증명 유출이다.

### 4.6 🔴 redirect 는 차단한다

`reqwest` 는 기본으로 리다이렉트를 최대 10회 따라간다. 인증 요청에서 이는
**Authorization 헤더가 다른 호스트로 따라갈 수 있는 경로**다.

→ `redirect::Policy::none()` 을 **모든 요청에** 적용한다. Fleet API 는 정상 경로에서
리다이렉트를 쓰지 않으므로 잃는 것이 없다. 3xx 는 `Error::Api` 로 표면화해
**조용히 따라가지 않고 시끄럽게 실패**시킨다.

---

## 5. 401 정책

### 5.1 🔴 토큰 **세대**를 비교한다 — "stale 로 표시"만으로는 틀린다

초안은 "401 → 캐시 토큰을 stale 로 표시 → refresh" 였다. **동시 요청에서 이는 방금 갱신된
토큰을 죽인다**:

```
t0  요청 A 가 토큰 세대 N 으로 전송
t1  요청 B 가 refresh → 세대 N+1 저장
t2  A 가 (세대 N 에 대한) 401 을 받음
t3  A 가 "캐시를 stale 로 표시" → 멀쩡한 N+1 을 버리고 또 refresh  ← 잘못됨
```

→ **`access_token()` 이 `(generation, token)` 을 함께 반환**하고, 401 복구는
자기가 보낸 generation 이 **아직 current 일 때만** stale 처리한다.

```
401 수신 (내가 보낸 세대 = G)
 → auth_retry_used == true          ⇒ Error::Auth 로 종료
 → current_generation != G          ⇒ stale 처리 없이 새 토큰으로 즉시 1회 재전송
                                       (다른 요청이 이미 고쳐 놓았다)
 → current_generation == G          ⇒ 세대 G 를 stale 표시 → refresh 1회 → 1회 재전송
                                       (플래그 set)
```

이는 §4.1 의 리전 캐시 compare-and-clear 와 **같은 패턴**이다 — 공유 캐시를 무효화하려는
쪽은 자기가 본 버전을 제시해야 한다.

⚠️ 세대 카운터는 §6 의 `MIN_REFRESH_INTERVAL` 과 **다른 문제를 푼다.** 세대는 *정확성*
(누가 무엇을 무효화해도 되는가), 간격은 *폭주 억제*다. **60초 간격을 single-flight 정확성의
대체재로 쓰지 말 것.**

**왜 필요한가.** A2 는 `expires_at` 기준 **선제** refresh 를 하지만, 시계 오차나 서버측 선제 폐기로
"신선하다고 믿은" 토큰이 거부될 수 있다. 그 경우 유일한 복구는 refresh 다.

**왜 1회한인가.** 재시도분도 401 이면 refresh 로 고칠 수 있는 문제가 아니다. 최악 비용은
**요청 2회 + refresh 1회**로 유계다 — A3 의 두 엔드포인트는 무과금이라 **$0**,
A4 가 유료 엔드포인트를 들여오면 그때 $0.004 가 된다(E6).

⚠️ 이 플래그는 **한 요청 안의** 루프만 막는다. 요청이 바뀌면 다시 refresh 를 태울 수 있으므로
**호출 간** 폭주 차단은 §6 이 담당한다. 두 장치는 서로를 대체하지 않는다.

---

## 6. 회전 최소 간격 — `expires_in` 하한 (carry-forward #1 해소)

`TokenManager` 가 **마지막 refresh 성공 시각**을 기억한다(A2 의 `Clock` 주입 사용).

```
refresh 요청 도착
 → now - last_refresh_at >= MIN_REFRESH_INTERVAL  ⇒ 진행
 → 아니면 ⇒ 원인을 지목하는 Error 로 종료
```

**`MIN_REFRESH_INTERVAL = 60초` — `EXPIRY_SKEW` 와 같은 값.**
논리가 한 문장으로 닫힌다: **"신선하다고 볼 수 없는 수명은 회전도 할 수 없다."**

### 6.1 왜 캐시 반환이 아니라 에러인가

간격 내 재요청에 캐시된 토큰을 그냥 주면 **죽은 토큰을 전송**하게 된다. 이는 carry-forward 가
명시적으로 금지한 `max` 하한과 **같은 실패**다(상한 `min` 이 안전한 건 축소적이기 때문이고,
`max` 는 죽은 수명을 부풀린다). 에러는 소모를 **막고**, 로그는 **관측만** 한다.

### 6.2 왜 (a) 파서 hard error 가 아니라 이것인가

| 원인 | (a) 파서 hard error | (b) 회전 최소 간격 |
|---|---|---|
| `expires_in <= 60` | 막음 | 막음 |
| 시계 역행 | **못 막음** | 막음 |
| 401 refresh 루프(요청 간) | **못 막음** | 막음 |
| Tesla 가 단수명 토큰 도입 | **전면 실패** — 파싱 단계에서 모든 refresh 거부 | **부분 실패** + 진단 가능 |

마지막 행을 정확히 적으면: 서버가 30초 토큰을 준다고 할 때 (a)는 파싱에서 죽으므로 **100% 실패**고,
(b)는 발급 직후 30초 창의 호출은 캐시로 성공하고 그다음 30초만 에러가 되므로 **부분 실패**다.
(b)가 낫지만 **"멀쩡히 동작한다"는 뜻은 아니다** — 어느 쪽이든 그 시나리오는 사람이 개입해야 한다.

(b)가 (a)를 포함한다. 그래서 (b)만 채택한다 — 이중방어는 fail-closed 위험만 추가한다.

### 6.3 A2 모듈을 건드린다는 사실

이 변경은 A2 소유 `auth/mod.rs` 를 수정한다. carry-forward 가 이 항목을 명시적으로 A3 에
할당했으므로 범위 침범이 아니다. 다만 **A2 의 227 테스트가 전부 통과해야 한다** — 회귀 게이트.

⚠️ 이 간격은 §5 의 401 복구를 **막을 수 있다**. 401 이 refresh 5초 뒤에 왔고 **세대가 같다면**
다시 refresh 해도 소용없으므로 막는 것이 옳다. **세대가 다르면 §5.1 이 이미 refresh 없이
재전송하므로 간격에 걸리지 않는다** — 두 장치가 이렇게 맞물린다.

### 6.4 ⚠️ `last_refresh_at` 의 저장 위치 — 프로세스 간 일관성

A2 는 **프로세스 간** single-flight 를 flock 으로 이미 제공한다. 그런데 `last_refresh_at` 을
프로세스 메모리에만 두면, 새 프로세스는 간격을 모른 채 즉시 refresh 할 수 있다.

**결정: 메모리에 둔다. 파일에 쓰지 않는다.** 근거:

- 회전 최소 간격의 목적은 **한 프로세스의 폭주 억제**이지 전역 rate limiter 가 아니다.
  전역 보장이 필요하면 그건 flock 이 하는 일이고, A2 가 이미 한다.
- 토큰 파일에 필드를 추가하면 **포맷 변경**이라 A2 의 저장 계약과 227 테스트를 건드린다.
  A3 가 감당할 범위가 아니다.
- 프로세스가 계속 새로 뜨면서 매번 refresh 하는 상황은 간격이 아니라 **flock 이 잡는다**
  — 실제로 A2 가 그걸 위해 존재한다.

⚠️ 따라서 §0.4 를 "**한 프로세스 안에서** 폭주 불가"로 정확히 적는다.
프로세스 간 보장을 주장하지 않는다. 이 한계를 `MIN_REFRESH_INTERVAL` rustdoc 에 명시한다.

⚠️ **시계 역행은 명시적으로 처리한다.** `now - last_refresh_at` 을 뺄셈으로 쓰면 시계가
뒤로 가면 underflow 하거나 음수가 된다. `checked_duration_since` 를 쓰고, **`None`(=시계 역행)
은 "간격 미충족"으로 취급**한다(보수적 방향).

---

## 7. 에러 매핑 — 분류는 status, 문구는 메시지에만

**두 가지를 구분한다** — 초안이 뭉뚱그려 써서 검토가 모순으로 읽었다:

| | 입력 | 쓰이는 곳 |
|---|---|---|
| **분류**(어느 `Error` variant 인가) | **status 만** | 아래 표 |
| **메시지 추출**(`message` 를 뭘로 채우나) | body JSON `error` → `WWW-Authenticate` 의 `error_description` → `canonical_reason()` | §7.2 |
| **파싱 방식 결정**(JSON 을 시도할까) | content-type essence | §7.1 |

**body 문구를 분류에 쓰지 않는다** — E1 이 보인 대로 A 와 E 가 같은 401 인데 문구가 다르고
둘 다 상류 구현 세부다. 다만 **메시지에는 쓴다**(사람이 읽을 정보라서). 이 둘은 다른 일이다.

⚠️ **`billing()` 은 status 만으로 결정되지 않는다.** A1/A2 가 구현한 대로 각 variant 의
`pricing` 필드를 **먼저** 본다 — `Pricing::NotCharged` 면 status 불문 `NotBilled`(E6).
아래 표의 `billing()` 열은 **유료 카테고리 엔드포인트일 때**의 값이고,
A3 가 내보내는 두 엔드포인트에서는 **모든 행이 `NotBilled`** 다.

| status | 매핑 | `billing()` (유료 카테고리 한정) | `is_retryable()` |
|---|---|---|---|
| 200 | 정상 (봉투 해제 → decode) | — | — |
| **3xx** | **`Error::Api`** — redirect 를 따라가지 않는다(§4.6) | `Billed` | false |
| 401 | `Error::Auth { pricing: Some(..) }` | `Billed` | false (§5 가 처리) |
| **403** | **`Error::Api`** — 스코프 부족의 **최종 권위**(§10). `MissingScope` 로 바꾸지 **않는다** | `Billed` | false |
| 408 | `Error::VehicleOffline` | `Billed` | false |
| 412 | 리전 불일치 → §4.3 복구, 소진 시 `Error::Api` | `Billed` | false |
| 429 | `Error::Api` + `Retry-After` | `Billed` | false |
| 500·502·503·504 | `Error::Api` | `NotBilled` | **true** |
| 그 외 5xx (501·505…) | `Error::Api` | `NotBilled` | false |
| 그 외 4xx | `Error::Api` | `Billed` | false |

**403 을 `MissingScope` 로 바꾸지 않는 이유**: `MissingScope` 는 **네트워크 이전** 의
advisory 선검사 결과(💰 0)를 뜻한다(§10). 서버가 답한 403 을 같은 variant 로 표현하면
"돈이 안 든 사전 차단"과 "돈이 든 서버 거절"이 구분 불가능해진다. `billing()` 이 갈리므로
**타입으로도 갈라야 한다.**

**`Retry-After` 파싱**: delta-seconds 와 **HTTP-date 두 형식을 모두** 받는다(RFC 9110).
날짜 형식을 빠뜨리면 조용히 `None` 이 되어 상한이 무의미해진다. 상한 120초는 **파싱된 값을
자를 때** 적용한다.

**`Error::Auth` 의 `pricing` 은 `Option`** 이고 이 구분이 load-bearing 이다 —
`Some(..)` = 실제 엔드포인트가 답한 401, `None` = 엔드포인트에 닿기 전 실패
(로컬 토큰 없음, §6 의 회전 간격 차단 등). **§6 이 refresh 를 막아 낸 에러는 `None` 이어야 한다.**

**요청 수 상한.** §5(401 1회)와 §4.3(412 1회)의 플래그는 독립이므로 최악의 경우
Fleet 요청은 **원요청 + 401 재시도 + 412 재시도 = 3회**, 여기에 리전 해석 최대 2회와
refresh 최대 1회가 더해진다. 두 플래그가 서로를 재설정하지 않는다는 것을 테스트로 고정한다.

### 7.1 반드시 지킬 두 가지

**(1) content-type 을 exact match 로 비교하지 않는다.**
실측상 성공은 `application/json; charset=utf-8`, 412 는 `application/json`(charset 없음)이다.
상류마다 다르므로 **mime 파라미터를 분리**해 essence 만 비교한다.

**(2) 빈 body 로도 에러를 구성할 수 있어야 한다.**
401 케이스 B 는 `text/html` + **0바이트**다.

✅ **A1 의 `decode()` 가 이미 올바르다 — 초안이 이 점을 잘못 적었다.**
`decode()` 는 `if status >= 400` 를 **content-type 검사보다 먼저** 하므로,
401+`text/html`+빈 body 는 401 arm 에서 `Error::Auth` 가 되고 **`Decode` 로 새지 않는다.**
초안은 "에러 경로에서 성공 경로의 decode 를 재사용하지 말라"고 했으나 **불필요하고 틀렸다** —
`decode()` 가 곧 그 게이트다. A3 는 우회 경로를 만들지 말고 `(status, content_type, body, pricing)`
을 그대로 `decode()` 에 넘긴다.

⚠️ **다만 A3 가 자체 파싱을 앞세우면 이 보호가 무너진다.** "응답을 먼저 JSON 으로 읽고
그다음 status 를 본다" 는 순서는 금지다. 순서가 계약이다.

⚠️ **3xx 는 실제로 구멍이다.** `decode()` 의 `>= 400` 검사에 걸리지 않으므로 content-type 검사로
내려가고, 리다이렉트 바디는 JSON 이 아니라서 `Error::Decode` 가 된다. §4.6 이 redirect 를
차단하므로 3xx 는 **transport 계층에서 `Error::Api` 로 매핑**해 `decode()` 에 도달시키지 않는다.

### 7.2 `Error::Api.message` 채우기

현재 `status.canonical_reason()` 뿐이라 `HTTP 502 Bad Gateway: Bad Gateway` 로 정보가 0이다.
채우는 순서:

1. body 가 JSON 이고 `error` 가 있으면 그 값
2. 없고 `WWW-Authenticate` 가 있으면 그 헤더의 `error_description`
3. 둘 다 없으면 `canonical_reason()`

⚠️ carry-forward #8 은 "`error`/`error_description` 을 `body_preview` 경유로"라 했으나
**부분적으로 틀렸다** — 케이스 B 는 body 가 0바이트고 `error_description` 이 **헤더에만** 있다.
body 만 읽는 파서는 B 에서 아무것도 못 얻는다. 그래서 위 2단계가 필요하다.

모든 경로의 출력은 §8 의 redaction 을 통과한다.

---

## 8. redaction 확장

- `redact_transport_error(&reqwest::Error)` 를 **`redaction` 모듈 안에** 만든다.
  A1 이 `redact_json`/`redact_text` 를 private 으로 내린 이유가 "`body_preview` 만이 안전한
  진입점"을 **타입으로** 강제하려는 것이다. ⚠️ A2 에서 이 계약이 한 번 깨졌다
  (`Refreshed` 가 `#[derive(Debug)]` 로 두 토큰을 평문 출력 — 최종 리뷰가 적발).
  **모듈 경계를 스타일이 아니라 load-bearing 으로 취급한다.**
- **계정 UUID 를 redaction 대상에 추가한다** (E5 — 412 가 계정 UUID 를 평문으로 흘림).
  ⚠️ **UUID 를 무조건 전역 치환하지 않는다** — 그러면 `x-request-id` 같은 **진단용 식별자까지
  지워져** 장애 대응이 어려워진다. 문맥으로 좁힌다:
  **body 안에서 `Account <uuid>` 패턴**과 **알려진 계정 필드 키**만 치환하고,
  헤더의 `x-request-id` 는 **보존**한다. 그 판단(무엇을 남기고 무엇을 지웠는지)을 rustdoc 에 적는다.
- reqwest 의 **URL·source chain·`Debug` 출력**까지 포함한다. 근거: OSS 사용자는 에러 메시지를
  GitHub 이슈에 그대로 붙여넣는다.
- A3 가 새로 만드는 모든 타입은 `Debug` 를 **손으로 쓰거나** 자격증명 필드를 갖지 않는다.

---

## 9. 전송 정책

| 항목 | 값 | 근거 |
|---|---|---|
| connect timeout | 10s | 상위 스펙 §5 |
| 요청 timeout | 30s | 상위 스펙 §5 |
| **refresh 요청 timeout** | **8s** | ⚠️ `state_deadline`(10s)보다 **짧아야** 함 — carry-forward #4 |
| pool | `max_idle_per_host(2)` · `idle_timeout(90s)` | 상위 스펙 §5 |
| 재시도 기본 | **`max_attempts = 1`** (=재시도 없음) | 상위 스펙 §11 non-goal |
| 재시도 opt-in | `Client` 빌더 `max_attempts(n)` | 아래 §9.0 |
| 백오프 (opt-in 시에만) | exponential + jitter, cap 30s | 상위 스펙 §6 |
| 체인 총 deadline (opt-in 시에만) | **60s** | 아래 §9.2 |
| `Retry-After` | 120초 상한, delta-seconds + HTTP-date | §7 |
| 응답 크기 상한 | `bytes_stream()` **누적 검사** | buffer-then-check 금지 |

### 9.0 ⚠️ 재시도는 opt-in 이다 — 죽은 정책이 아니다

초안은 `max_attempts = 1`(재시도 없음)과 백오프 표·`is_retryable() == true` 를 나란히 적어
**"기본값에서 백오프와 retryable 매트릭스는 죽은 코드"** 라는 지적을 양쪽 모델에서 받았다.
정확한 지적이다. 명확히 한다:

- **기본값 `max_attempts = 1`** — 상위 스펙 §11 이 "재시도 기본 활성화"를 non-goal 로 둠.
- `is_retryable()` 은 기본값에서도 **의미가 있다** — 라이브러리가 재시도하지 않아도
  **호출자가 재시도해도 되는지 판단**하는 공개 술어이기 때문이다. `Billing` 과 짝이다.
- 백오프·체인 deadline 은 **`max_attempts > 1` 로 켰을 때만** 작동한다.
  기본값에서 이 코드가 실행되지 않는다는 사실을 **테스트로 고정**한다(T-k).

### 9.1 refresh timeout 이 8초여야 하는 이유

"유한하면 된다"가 아니다. A2 의 tokio state mutex 가 **refresh POST 를 가로질러 유지**되므로,
전송 timeout 이 `state_deadline`(10s)을 넘으면 **느리지만 성공하는 refresh 도중에** 프로세스 내
형제 호출자들이 `token state lock:` 오류를 받는다.

### 9.2 재시도 체인

재시도 체인 **전체**를 `tokio::time::timeout` 으로 감싼다 — `reqwest.timeout()` 은 개별 요청만
막고 체인은 못 막는다.

⚠️ **`TransportUnknown` 은 절대 재시도하지 않는다.** "전송 실패 = 무과금 = 재시도 안전"은
틀렸다. read timeout·connection reset 은 서버가 이미 처리·과금한 뒤일 수 있고, 재시도가 곧
**중복 과금**이다.

**이 규칙이 필요 이상으로 엄격하다는 점을 알고 유지한다.** 무과금 14개 엔드포인트에서는
`TransportUnknown` 을 재시도해도 중복 과금이 없다(rate limit 은 여전히 적용). 그럼에도
`is_retryable()` 을 status 만으로 판정하는 A1 구현을 유지하는 이유는 세 가지다 —
(1) 재시도 기본값이 어차피 off 라 실효 차이가 없고, (2) 과금 카테고리를 재시도 판정에
끌어들이면 "안전 규칙"이 요금표 변경에 연동돼 깨지기 쉬워지며, (3) `is_retryable()` 은
공개 API 라 의미를 나중에 좁히는 것이 breaking 이다. **이 판단을 rustdoc 에 남긴다** —
후속 기여자가 "무과금인데 왜 재시도 안 하지"로 되돌리지 않도록.

---

### 9.3 refresh POST 의 세부 (초안 누락)

크레이트 유일의 POST 이므로 세부를 확정해 둔다.

| 항목 | 값 |
|---|---|
| 엔드포인트 | `https://fleet-auth.prd.vn.cloud.tesla.com/oauth2/v3/token` (§4.4 — discovery 따르지 않음) |
| 본문 | `application/x-www-form-urlencoded`: `grant_type=refresh_token` · `client_id` · `refresh_token` |
| 클라이언트 인증 | **public client** — `client_secret` 을 보내지 않는다 |
| timeout | **8s** (§9.1) |
| redirect | 차단 (§4.6) |
| 응답 본문 상한 | 성공·오류 **둘 다** §9 의 크기 상한 적용 |
| 재시도 | **없음.** refresh 실패는 그대로 올린다 |

⚠️ **회전 저장 순서가 치명적이다.** Tesla 는 refresh 회전 시 이전 체인 전체를 무효화하므로,
**새 refresh_token 을 디스크에 원자적으로 쓰기 전에는 성공으로 취급하지 않는다.**
(A2 의 I1 "refresh 회전 시 즉시 저장, 실패 = 재-OAuth". A3 는 이 계약을 **바꾸지 않는다**.)

⚠️ **8초 타임아웃이 체인을 죽일 수 있다는 점을 문서화한다** — 서버는 회전을 완료했는데
응답이 유실되면, 우리가 가진 refresh_token 은 이미 무효다. 이 경우 복구는 재-OAuth 뿐이다.
이는 **타임아웃 값의 문제가 아니라 회전형 토큰의 구조적 위험**이고, 재시도로 악화되므로
위 표에서 refresh 재시도를 금지한다.

## 10. REQ-8 배선 (스코프 advisory)

A2 가 `observe_scopes` 를 구현·테스트했으나 `pub(crate)` + 호출자 없음 + `expect(dead_code)` 상태다.
A3 가 요청 파이프라인에 배선한다.

⚠️ **`Indeterminate` 와 `ContainsRequiredScope` 를 코드에서 동일하게 다룬다.**
호출부에서 갈라지면 fail-open 이 거기서 깨진다. (access token 이 항상 JWT 라는 보장이 없고,
서명 미검증 payload 는 권위가 없다 — 선검사는 **불필요한 유료 호출을 줄이는 최적화**이지
권한의 근거가 아니다. 최종 판정은 서버의 403 이다.)

`MissingScope` 의 doc 에 상위 스펙 §9 가 요구한 caveat 을 넣는다:
"검증되지 않은 토큰에서 관찰된 값" + "advisory 비용 가드이지 권한 판정이 아님".

### 10.1 호출 순서 (비용 낮은 검사부터)

```
1. required_scopes() vs 토큰 scp   → MissingScope   (💰 0, advisory)
2. [guarded] GET /vehicles/{vin}   → Skipped        (💰 state 조회분)
3. 실제 호출                        → 💰
4. 실패 시 is_retryable()           → NotBilled 인 것만 재시도
5. 모든 에러 메시지 → redaction
```

---

## 11. 테스트

**HTTP 테스트 서버 = `wiremock` (dev-dependency).** 사용자 의존성 트리에 들어가지 않는다.

### 11.1 반드시 있어야 할 테스트

| # | 무엇 | 왜 |
|---|---|---|
| T-a | 401 **세 형태 전부**(A/E/B) 재현 → 전부 `Error::Auth` | E1. 특히 B(`text/html`+0바이트)가 `Decode` 로 새지 않을 것 |
| T-b | 401 → refresh 1회 → 재전송 1회, **2번째 401 은 종료** | §5. 호출 카운터로 **기전**을 셀 것 |
| T-c | 412 → 리전 재해석 1회 → 재전송, 루프 없음 | §4.3 |
| T-d | 회전 간격 내 재요청이 **에러**로 끝나고 **stale 토큰이 전송되지 않음** | §6.1. wiremock 에 **요청이 아예 도달하지 않음**을 단언(요청 카운터=0). "에러가 났다"만 보면 stale 전송 후 실패와 구분 못 함 |
| T-d2 | 그 에러의 `pricing` 이 **`None`** 임 | §7. 엔드포인트에 닿지 않았으므로 `Billed` 로 세면 안 됨 |
| T-j | 401 재시도와 412 재시도가 **서로를 재설정하지 않음** — Fleet 요청 총 3회 상한 | §7 요청 수 상한 |
| **T-k** | 기본값에서 백오프·체인 deadline 경로가 **실행되지 않음** | §9.0 죽은 코드 아님을 고정 |
| **T-l** | 🔴 **악성 리전 URL 거부** — `http://`, 외부 호스트, `user:pass@`, path/query 부착, 비표준 포트 각각 | §4.5. 통과하면 자격증명 유출 |
| **T-m** | 🔴 **3xx 를 따라가지 않음** — redirect 응답에 Authorization 이 재전송되지 않음 | §4.6 |
| **T-n** | 🔴 **동시 401 에서 세대가 다르면 refresh 하지 않고 재전송** | §5.1. refresh 호출 카운터로 **기전**을 셀 것 |
| T-o | 412 캐시 무효화가 **자기가 본 generation 일 때만** 성립 | §4.1 compare-and-clear |
| T-p | 시계 역행(`checked_duration_since` → `None`)이 "간격 미충족"으로 처리됨 | §6.4 |
| T-q | `Retry-After` 의 **HTTP-date 형식**이 파싱됨 | §7 |
| T-r | 403 이 `Error::Api` 이고 `MissingScope` 가 **아님** | §7 |
| T-s | `x-request-id` 는 redaction 후에도 **남아 있음**, 계정 UUID 는 사라짐 | §8 |
| T-e | `application/json` vs `application/json; charset=utf-8` 양쪽 파싱 | §7.1(1) |
| T-f | 크기 상한 초과가 **누적 중** 끊김(buffer-then-check 아님) | §9 |
| T-g | 412 계정 UUID 가 에러 메시지에 **남지 않음** | E5 |
| T-h | `TransportUnknown` 이 재시도되지 않음 | §9.2 |
| T-i | A2 의 227 테스트 전부 통과 | §6.3 회귀 게이트 |

### 11.2 A2 가 확립한 테스트 규율 (A3 도 이어받음)

1. **X 를 금지하는 규칙에는 X 를 했을 때만 실패하는 입력이 필요하다.**
2. **자기 테스트를 mutation-test 하고 생존자를 정직하게 분류하라.** 죽일 수 없으면 무엇이
   필요한지와 그 비용을 대라 — **"안 하기로 했다"를 "할 수 없다"로 격상하지 말 것.**
3. **값이 아니라 기전을 단언하라.** A2 에서 두 번 물렸다(16-way 동시성 테스트가 Mutex 가 아니라
   flock 을 검증). 해법은 카운터로 기전을 세는 것.
4. **커버리지를 부풀리지 말 것.** 판별력 없는 입력은 제외하고, 예측한 실패가 안 나면
   near-duplicate 대신 **mutation 으로 기존 커버리지를 증명**하라.
5. **vacuous 하게 통과할 수 있으면 큰 소리로 말하라** — root 에서 무력화되는 테스트는 `eprintln!`.

---

## 12. 의존성 예산

| crate | 종류 | 비고 |
|---|---|---|
| `reqwest` | 정규 | `default-features = false` + `rustls-tls`·`json`·`stream` |
| `wiremock` | **dev** | 사용자 트리에 안 들어감 |

⚠️ **feature unification 은 예산 질문이다.** fleetwire 의 `default-features = false` 는 유지되지만
reqwest 가 `net`/`rt`/`io-util` 을 **같은 tokio 빌드에 병합**하므로 `mio`/`libc`/`socket2` 가
어차피 들어온다. **lockfile diff 에서 놀라지 말 것** — 이건 수용된 비용이지 결함이 아니다
(carry-forward #2).

⚠️ **`git add` 에 `Cargo.lock` 을 빠뜨리지 말 것.** 의존성이 바뀌면 `--locked` 게이트 3개가 전부
깨진다(A2 에서 두 번 걸렸다).

---

## 13. 명시적 비범위

- 나머지 14개 엔드포인트 → **A4**
- 페이지네이션 자동 순회 → A4 (opt-in 만)
- 만료 토큰 401 과 폐기 토큰 401 의 **구분** → 증거 없음(§14 O8). A3 는 둘 다 `Error::Auth`
- 명령(POST) 계층 · MCP 서버 · Fleet Telemetry · 클라우드
- 여러 호스트가 같은 토큰 파일을 공유하는 구성

## 14. 미결

| # | 항목 | 해소 시점 |
|---|---|---|
| O4 | `GET /vehicles/{vin}` 의 Pricing Category → 게이트 절감폭 | 사용량 대시보드 |
| O7 | Tesla 의 refresh token 재사용 탐지 정책 (근거가 타사 사례뿐) | 미해결 |
| **O8** | **만료 access token 의 401 형태** — A/E/B 는 전부 "유효한 적 없던 토큰" | A4 실호출 중 캡처 |
| **O9** | **무인증 요청의 과금 여부** — 계정 귀속이 안 되는 401 이 과금되는지 불명 | 사용량 대시보드 |
| **O10** | **408·5xx 의 실응답 증거 없음** — 문서 근거만 있고 본 적 없다. 특히 408 이 정말 `{"error":…}` 봉투 없이 오는지 | A4 실호출 중 캡처 |
| **O11** | **중국 리전 호스트 미검증** — §4.5 allowlist 의 `…tesla.cn` 항목은 문서 추정 | 증거 확보 전까지 allowlist 에서 뺄지 결정 |

## 15. A2 에서 그대로 이월된 미해결 (A3 범위 아님, 기록만)

- **`kv_secret` 멱등 가드 우회** — `code=[REDACTED]<secret>` 통과. redaction 이 단사가 아니라
  `caps` 검사만으로 폐쇄 불가. `SECURITY.md` 로 옮길 것.
- **`acquire` 가 락 경로 symlink 를 따라간다** — `tokens.lock` 이 다른 곳을 가리키면 두 holder 가
  두 inode 를 잡아 상호배제가 조용히 사라진다. 최소한 수용된 위협모델을 rustdoc 에 적을 것.

## 16. 부록 — 실측 원본

`o1-a3-evidence.md`(세션 scratchpad)에 요청·응답 헤더 전문 보존. O1 증거 매트릭스
(`2026-07-29-fleetwire-o1-endpoint-evidence-matrix.md`)에 401·412·region 행을 추가해야 한다.

**프로브 비용 = $0.** 실토큰 4회는 전부 무과금 엔드포인트였다(`/api/1/vehicles` ×2,
`/api/1/users/region` ×2 — E6). 무인증 4회는 계정 귀속이 안 되므로 별도(O9).
refresh 1회 발생. 토큰은 정상 회전됐고(access·refresh 둘 다) 파일 권한 0600 유지.

> ⚠️ 이 문서 초안은 이 프로브를 "$0.008" 로 적었다. F3(status<500=billed)을 엔드포인트
> 과금 카테고리와 무관하게 적용한 오류였다. **F3 은 유료 카테고리 한정 규칙이다**(E6).
> 같은 오류가 초안의 §4.3·§5 비용 논증에도 있었고 self-review 에서 정정했다.

---

## 17. 검증 이력

| 라운드 | 도구 | 잡아낸 것 |
|---|---|---|
| 실측 | curl(무인증 4 + 실토큰 4) | 401 3형태·412 리전거절·OIDC discovery 부적합·계정 UUID 유출·토큰 aud |
| self-review | 본인 | **과금 과대주장** — O1 의 "유료는 16중 2개" 를 반영 안 함(§4.3·§5·§16). `Error::Auth` 가 이미 `pricing: Option` 으로 이 문제를 푼 것을 못 봄 |

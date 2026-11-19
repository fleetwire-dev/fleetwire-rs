# fleetwire A2 설계 — 토큰 계층 (네트워크 없는 인증 기계)

> 작성 2026-07-31 · 서브프로젝트 A(읽기 라이브러리)의 2번째 사이클 · 선행 = A1(core 타입·에러·redaction·decode, HEAD `8849426`, 121 테스트, 최종 리뷰 Ship)
> SDD SSOT = `docs/design/specs/fleetwire-read-client/{requirements.md,design.md}` · 이월 = `2026-07-30-fleetwire-a1-carry-forward.md`

## 1. 한 줄 요약

A2 는 **네트워크를 전혀 타지 않는 토큰 기계**다 — 안전한 저장·회전, 프로세스 간 single-flight, 그리고 "요구 스코프가 없으면 과금 요청을 아예 보내지 않는다"는 advisory 선검사. 실제 refresh POST 는 주입된 trait 뒤에 있고 구현은 A3 가 준다.

## 2. 왜 이 경계인가

REQ-15(single-flight)·REQ-16(회전·저장)은 네트워크가 없어 A1 처럼 촘촘히 테스트할 수 있다. 반면 REQ-14 의 refresh POST 와 REQ-17 의 리전 해석은 HTTP 가 필요하고 그건 A3 다. 여기에 순서 문제가 겹친다 — refresh POST 는 설계 §12 가 "GET-only 정책의 **유일한 POST 예외**"로 규정한 지점인데, **read-only 강제의 3번째 계층(clippy `disallowed_methods`)이 아직 없다**(이월 8번). POST 능력을 들여오는 순간부터 그 가드가 있어야 한다.

네트워크 없는 A2 를 택하면 이 문제가 자연히 사라진다. A2 에는 HTTP 클라이언트가 아예 없으므로 REQ-19 의 3계층 강제는 **A3 에서 갖추면 되는 것이 맞고**, 갭이 아니라 정합한 순서가 된다.

### 범위

| | |
|---|---|
| **In** | `TokenSet` 저장/로드(REQ-16) · 고정 inode 락 기반 프로세스 간 single-flight(REQ-15) · 회전 · Adopted 판정 · JWT `scp` advisory 선검사(REQ-8) · `TokenRefresher` trait(구현 없음) · `Clock` 주입 |
| **Out** | refresh POST 구현 · 리전 해석(REQ-17) → **A3** · OAuth 초로그인 → **외부**(현행 `oauth_login.py`) · **401 분류기 → A3** |

**401 분류기를 미루는 이유**: REQ-14 는 "모든 401 을 무조건 refresh 로 해석하지 말 것"을 요구하는데, 분류하려면 Tesla 가 실제로 어떤 401 바디를 주는지 증거가 있어야 한다. O1 증거매트릭스는 엔드포인트 과금·스코프만 다뤘고 401 형태는 없다. **증거 없이 분류기를 쓰면 추측을 코드로 굳히는 것**이고, A1 이 네 라운드 데인 실패 모드가 정확히 그것이다. A2 는 어려운 부분(동시성·at-most-once 기계)을 만들고, 분류는 응답 형태가 확인되는 A3 에서 붙인다.

## 3. 실측 증거 (설계의 근거)

이 절의 값은 **이 머신의 실제 Tesla 토큰에서 읽은 것**이다(2026-07-31). 토큰 값 자체는 기록하지 않는다.

```
access_token : 1110자, 3세그먼트, "eyJ" 시작 → 진짜 JWT
헤더          : {"alg":"RS256","kid":"…","typ":"JWT"}
페이로드 클레임 : account_type, amr, aud, auth_time, azp, exp, iat, iss, locale, ou_code, scp, sub
scp          : ["offline_access","openid","vehicle_device_data","vehicle_location"]   ← 문자열 배열
iss          : https://auth.tesla.com/oauth2/v3/nts
aud          : 배열 (문자열 아님)
refresh_token: 67자, 1세그먼트, "NA_" 접두 → JWT 아님, 불투명 문자열
기존 파일 형식 : {access_token, expires_at, refresh_token}  (Python MCP 가 쓰던 3키)
```

여기서 나오는 설계 입력 5가지:

1. **`scp` 는 실재하고 문자열 배열이다.** REQ-8 은 추측이 아니었다.
2. **`offline_access` 가 실제로 들어 있다.** A1 의 `Scope` 는 4개(`Openid`·`VehicleDeviceData`·`VehicleLocation`·`UserData`)만 모델링돼 있으므로 **이 실제 토큰을 파싱하면 미지 스코프에 부딪힌다** → 미지 스코프를 무시하는 설계가 필수(§6).
3. **refresh token 은 JWT 가 아니다.** JWT 파싱은 access token 에만 적용한다.
4. **`exp` 가 토큰 안에 있다.** 만료 시각의 출처가 둘(토큰 응답 `expires_in` vs JWT `exp`)이라 어느 쪽을 신뢰할지 정해야 한다(§5).
5. **`aud` 는 배열이다.** 나중에 파싱하게 되면 문자열로 가정하는 순간 깨진다.

## 4. 파일 위치

기본값 **`~/.fleetwire/`**, 빌더로 오버라이드 가능.

```
~/.fleetwire/               0700
~/.fleetwire/tokens.json    0600
~/.fleetwire/tokens.lock    고정 inode — 어떤 경로에서도 삭제 금지
```

설계 §7.3/§7.4 는 `~/.tesla/` 를 쓰지만 **바꾼다**. 이 머신의 `~/.tesla/tokens.json` 은 기존 Python MCP 가 쓰고 있고, 두 클라이언트는 Tesla 앱이 다르다. Tesla 는 refresh token 회전 시 **이전 체인 전체를 무효화**하므로, 같은 파일을 보면 한쪽이 갱신하는 순간 다른 쪽이 죽고 서로를 계속 밀어낸다. 상용 VM 에선 안 겹치지만 로컬 테스트에서 즉시 겹친다. OSS 사용자가 다른 Tesla 도구와 `~/.tesla` 를 공유하는 사고도 같이 막는다.

lock 파일은 반드시 토큰과 **같은 디렉터리**에 둔다(락은 경로가 아니라 inode 기준).

## 5. 저장 계층 (REQ-16)

**외부 크레이트가 필요 없다.** `create_new(true)`(=`O_CREAT|O_EXCL`), `OpenOptionsExt::mode(0o600)`, `custom_flags(O_NOFOLLOW)`, `File::sync_all`, `fs::rename`, 디렉터리 fsync — 전부 `std::os::unix` 이고 `unsafe` 가 없다. 워크스페이스가 `unsafe_code = "forbid"` 이므로 raw `libc` 경로는 애초에 막혀 있다.

disk-first 저장 순서(설계 §7.4 그대로):

1. 디렉터리 안전성·regular file·소유권·symlink 부재 검사
2. 같은 디렉터리에 `O_CREAT|O_EXCL|O_NOFOLLOW`, mode 0600 임시 파일
3. 완전한 새 토큰 JSON 쓰기 → 파일 `fsync`
4. 원자적 `rename`
5. 부모 디렉터리 `fsync`
6. **성공 후에만** 메모리 access token 교체

새 refresh token 저장 실패는 치명적 `Auth` 오류다. 저장 전에 원 요청을 재시도하지 않는다.

### TokenSet

```rust
pub struct TokenSet {
    access_token: String,
    refresh_token: String,
    expires_at: u64,                     // unix secs — 안정적 JSON
    #[serde(flatten)] extra: Map<String, Value>,   // 신버전이 쓴 필드 보존
}
```

- **`Debug` 는 막지 않고 수동 구현**한다 — `access_token: [REDACTED]`. A1 은 `VehicleSummary` 에서 `Debug` 를 withhold 했지만, 토큰은 디버깅 수요가 확실해서 막으면 누군가 우회한다. 안전한 `Debug` 를 주는 쪽이 낫다.
- `extra` 로 미지 필드를 보존한다 — 신버전 fleetwire 가 쓴 파일을 구버전이 읽고 되쓸 때 필드를 잃지 않게(REQ-4 정신).
- **만료 시각의 출처**: 저장된 `expires_at` 을 신뢰한다. JWT `exp` 는 advisory 경로에서만 참고하고 만료 판정의 SSOT 로 쓰지 않는다 — 토큰이 JWT 가 아닐 수 있고, `exp` 는 검증되지 않은 값이기 때문이다.

## 6. 스코프 advisory 선검사 (REQ-8)

> 근거: 2026-07-31 리서치. 두 독립 조사가 같은 결론에 도달했다.

### 6.1 무엇을 쓰는가

**`base64` 크레이트 + `serde_json`.** 자가구현도 `jsonwebtoken` 도 쓰지 않는다.

- **자가구현 배제**: 40줄짜리에 흔히 숨는 버그 — URL-safe 알파벳(`-`/`_`) vs 표준(`+`/`/`) 혼동, 패딩 없는 입력의 `len%4==2|3` 오처리, `len%4==1`(불가능한 입력) 허용, 마지막 2/4비트의 미사용 비트 0 미검사(비정규 인코딩 허용), 출력 크기 계산 off-by-one, 경계 길이 panic. **실제 피해는 메모리 손상이 아니라 "잘못 디코딩한 결과를 스코프 없음으로 확정해 정상 요청을 막는 것"**이다. RFC 테스트 벡터 + property/fuzz 테스트 비용까지 넣으면 작은 크레이트가 더 싸다.
- **`jsonwebtoken` 배제**: 필요한 건 3분할·base64url 디코드·`scp` 역직렬화뿐인데 암호 알고리즘·키 처리·claim validation 전이 의존성이 감사 범위에 들어온다. 공급망 신호도 있다 — **CVE-2026-25537**(10.3.0 이전 claim validation type confusion). `insecure_disable_signature_validation` 같은 이름은 나중 유지보수자와 보안 감사자를 혼란시킨다.

`base64` 는 `URL_SAFE_NO_PAD` 알파벳을 **명시**해서 쓴다.

### 6.2 3상태 관찰 — fail-open 을 관례가 아니라 구조로

```rust
enum UnverifiedScopeObservation {
    ContainsRequiredScope,
    DoesNotContainRequiredScope,   // ← 유일하게 요청을 건너뛰는 값
    Indeterminate,
}

match observation {
    DoesNotContainRequiredScope => skip_request(),
    ContainsRequiredScope | Indeterminate => send_request(),
}
```

`bool` 을 반환하면 `false` 가 "권한 없음"인지 "파싱 실패"인지 "opaque 토큰"인지 구분되지 않고 authz 로 재사용되기 쉽다. `Option` 도 fail-open 이 **관례**에 머문다. 3상태는 fail-open 이 **타입**이 된다.

**`Indeterminate` 범위를 넓게 잡는다** — 세그먼트 ≠ 3 · base64url/JSON 파싱 실패 · `scp` 타입 불일치(우리 실측은 배열, 배열이 아니면 Indeterminate) · **`scp` 클레임 부재** · payload 크기 상한 초과 · 미지 발급자.

`scp` 클레임 부재를 "스코프 없음"으로 읽지 않는 것이 중요하다. 발급자 프로필이 "부재 = 스코프 없음"을 명시할 때만 그렇게 볼 수 있고, 우리는 그 명시를 갖고 있지 않다.

### 6.3 미지 스코프 — `#[serde(other)]` 가 아니라 `FromStr → Option`

실측 토큰의 `scp` 에 `offline_access` 가 있는데 A1 의 `Scope` 는 그걸 모델링하지 않았다. 미지 스코프는 **무시**한다(파싱 전체를 실패시키지 않는다). 요구 스코프는 항상 우리 엔드포인트 타입에서 오므로 알려진 값이고, 토큰이 들고 있는 미지 스코프를 버려도 판정이 틀리지 않는다.

`#[serde(other)]` 는 쓰지 않는다 — 서로 다른 미지 스코프를 한 variant 로 뭉개면서 공개 enum 을 오염시킨다. A1 fix wave 가 넣은 `rename_all = "snake_case"` 는 유지한다.

### 6.4 JWT 여부를 토큰 자체로 판정하지 않는다

`iss` 나 `alg` 를 읽어 "JWT 구나" 판단하는 것은 **검증되지 않은 입력으로 판단하는 것**이다. opaque 토큰이 우연히 JWT 처럼 디코딩될 수도 있고, 그걸 JWT 로 오인해 "스코프 없음"으로 요청을 막으면 fail-open 원칙이 깨진다. 따라서 **opt-in 생성자**로 호출자가 명시 선택한다.

```rust
// A2 의 실제 소유자는 TokenManager 다 (Client 는 A3/A4 에서 생긴다).
TokenManager::new(store, refresher)                              // 선검사 없음 — 형식 불명
TokenManager::new(store, refresher).with_unverified_jwt_scope_advisory()  // 명시 선택
```

A3/A4 가 `Client` 를 만들 때 이 선택을 그대로 위로 올린다(`ClientBuilder` 의 opt-in 메서드). **기본값은 항상 "선검사 없음"** 이다 — 형식을 모르는 토큰으로 요청을 막는 것이 이 설계가 피하려는 실패다.

### 6.5 오용 방지

- `decode_jwt`·`claims`·`has_scope` 같은 **범용 public API 를 만들지 않는다.** HTTP 요청 직전 비용 방지 로직에서만 쓰는 private 경로.
- 타입·함수명에 `Unverified`/`Advisory`/`Observation` 을 박는다.
- 오류는 `Unauthorized` 가 아니라 `RequestSkippedByScopeAdvisory` 로 구분한다.
- **토큰·payload·전체 JSON 을 로그에 남기지 않는다.** 파싱 오류도 토큰 내용을 포함하지 않는 정적 분류만 기록한다(A1 redaction 규율과 동일).

### 6.6 프레이밍 — 이것은 보안 경계가 아니다

공격자가 토큰을 통제하면 `scp` 에 뭐든 넣을 수 있다. 그러면 요청이 나가고 서버가 거부한다. 즉 이 선검사는 **adversarial cost-control 이 아니라 accidental misconfiguration 에 대한 비용 절약**이다. 문서에 이렇게 쓴다. 최종 권한 판정은 항상 Tesla 서버가 한다.

## 7. single-flight (REQ-15)

설계 §7.3 을 따르되 **async 안전하게** 바꾼다.

```
 1. fast path — 메모리 토큰이 유효(만료 skew 여유)하면 락 없이 즉시 반환
 2. tokio Mutex 획득 (await 구간 전체 보유)
 3. 락 안에서 재확인 — 다른 태스크가 이미 갱신했으면 반환
 4. tokens.lock 열기 (고정 inode, 삭제 절대 금지)
 5. try_lock 루프 → 실패 시 backoff sleep → deadline 초과 시 Auth 오류
 6. 디스크 재읽기 → access 유효? → yes: Adopted, POST 없음
 7. no → refresher.refresh() 정확히 1회
 8. disk-first 저장 (§5) — 실패하면 치명적 Auth 오류, 메모리 미갱신
 9. 저장 성공 후에만 메모리 교체
10. Drop 가드로 unlock
```

### 왜 `try_lock` + 재시도인가

flock 은 **블로킹 syscall** 이다. tokio Mutex 를 쥔 채 블로킹 락을 걸면 런타임 워커가 멈춘다. 그리고 설계 §7.3 의 "timeout 포함 `flock(LOCK_EX)`" 는 문자 그대로는 존재하지 않는다 — flock 에 timeout 인자가 없다. **논블로킹 `try_lock` + async 재시도 + deadline** 이 런타임을 막지 않으면서 REQ-15 의 "유한 timeout" 을 정직하게 충족하는 유일한 형태다. `spawn_blocking` 대안은 블로킹 락을 취소할 수 없어 더 나쁘다.

### 락 API — std, MSRV 상향

`std::fs::File::{try_lock, unlock}` 을 쓰고 락 관련 의존성을 0 으로 유지한다.

**실측(2026-07-31)**: 핀된 툴체인 1.97.1 에서 동작 ✅ / MSRV 1.85.0 에서는 `error[E0658]: use of unstable library feature 'file_lock'`(tracking issue #130994) ❌.

→ `rust-version` 을 `file_lock` 안정화 버전으로 올린다. **정확한 버전은 계획 T1 에서 확인**(추정 1.89.0, 미확인). CI MSRV 매트릭스 job 도 함께 수정한다. `rust-toolchain.toml` 채널 1.97.1 은 유지.

근거: 자격증명을 다루는 크레이트라 공급망 표면이 적을수록 좋고, std 구현은 영구히 유지보수되며, fleetwire 는 아직 미발행(`publish = false`)이라 사용자 0 — MSRV 상향 비용이 오늘은 정확히 0 이다.

### 테스트에서 유용한 사실

flock 은 **open file description** 단위다. 같은 파일을 한 프로세스에서 두 번 `open` 하면 서로 배타된다 → 실제 자식 프로세스를 띄우지 않고도 "형제 프로세스" 배제를 검증할 수 있다.

## 8. 주입 지점

```rust
pub trait TokenRefresher: Send + Sync {
    fn refresh<'a>(&'a self, refresh_token: &'a str)
        -> Pin<Box<dyn Future<Output = Result<Refreshed, Error>> + Send + 'a>>;
}

pub trait Clock: Send + Sync { fn now(&self) -> SystemTime; }
```

- **`Arc<dyn>` + 수동 박싱.** AFIT 은 1.75 부터 안정이지만 dyn-safe 가 아니다. 제네릭 `TokenManager<R>` 로 가면 `Client` 타입 시그니처까지 오염되고 빌더 API 가 지저분해진다. refresh 는 드물게 일어나므로 Box 한 번은 무의미하고, `async-trait` 크레이트를 안 들여도 된다.
- **`Clock` 주입**은 만료 판정을 sleep 없이 테스트하기 위한 것이다(grok-build 교훈).

## 9. 오류

A1 의 `Error::Auth { message, pricing }` 을 재사용하고 `pricing: None` 을 넣는다 — 인증 계층은 엔드포인트 맥락이 없다. A1 fix wave 에서 확정한 형태 그대로다.

**스코프 선검사가 요청을 건너뛴 경우는 A1 의 `Error::MissingScope { required, held }` 를 쓴다. 새 variant 를 만들지 않는다.** REQ-8 이 "불일치 시 네트워크 요청 없이 `MissingScope` 를 반환한다"고 명시하고 있고 A1 이 이미 그 variant 를 갖고 있다.

⚠️ 다만 §6.5 의 오용 방지 취지는 살려야 한다 — `MissingScope` 라는 이름이 권한 판정처럼 읽히기 때문이다. **variant 의 doc 주석에 "이것은 검증되지 않은 advisory 비용 가드이지 권한 판정이 아니다"를 명시**하고, `held` 필드에는 "검증되지 않은 토큰에서 관찰된 값"임을 적는다. 이름을 바꾸는 것은 REQ-8 이탈이므로 하지 않고, 문서로 해결한다.

`held` 에는 **파싱에 성공한 알려진 스코프만** 담는다(미지 스코프는 §6.3 대로 버려졌으므로). 그 사실도 doc 에 적어야 사용자가 `held` 를 토큰의 전체 스코프 목록으로 오해하지 않는다.

## 10. 플랫폼

**Unix 전용.** 0600·`O_NOFOLLOW`·flock 이 보안 모델의 핵심인데 Windows 에 대응물이 없다. Windows 는 명확한 컴파일 에러 + 문서화가 정직하다.

## 11. 새 의존성

| 크레이트 | 용도 | 근거 |
|---|---|---|
| `tokio` (`sync`, `time`) | REQ-15 가 `tokio::sync::Mutex` 를 명시적으로 요구 | 스펙 명시 |
| `base64` | JWT payload base64url 디코드 | §6.1 |

락은 std(§7), 저장은 std(§5) — 추가 의존성 없음. 둘 다 cargo-deny 라이선스 allowlist(MIT/Apache-2.0/BSD/ISC/Unicode-3.0/Zlib) 통과 여부를 계획 단계에서 고정 확인한다.

## 12. 테스트 전략

A1 이 4라운드 데인 실패 모드를 되풀이하지 않는 것이 목표다.

- **모킹 없이 실제 tempdir·실제 파일시스템.**
- **동시성**: tokio multi-thread 런타임에 N 태스크 → `refresher` 호출 카운트 **== 1**.
- **"형제 프로세스"**: 같은 락 파일 두 번 open → 배타 확인(§7).
- **symlink 공격**: 토큰 경로를 심볼릭 링크로 치환 → 거부되는지.
- **저장 실패 주입**: 읽기 전용 부모 디렉터리 → 메모리 미갱신 + `Auth` 오류.
- **deadline**: 락 경합 시 유한 시간 내 실패.
- **락 파일 불멸**: 정상·오류·panic 경로 어디서도 삭제되지 않음.
- **스코프 advisory**: 실측 `scp` 배열(§3)을 그대로 쓰고 `offline_access` 가 있어도 판정이 정확한지. `Indeterminate` 각 사유별로 하나씩.

### A1 이 남긴 규율 (최종 리뷰가 "A2/A3 템플릿"으로 지목)

- **기대값을 피검증 대상이 아닌 독립 출처에서 유도하라.**
- **`contains` 대신 전체 문자열 `assert_eq!`.**
- **구조적 속성을 테스트하라** — 손으로 고른 입력 집합이 아니라 설계 속성 자체를 고정.
- **입력은 assertion 이 통과하게 해주는 것이 아니라 약점을 드러내는 것으로 고르라.**

## 13. A3 로 넘기는 것

- refresh POST 구현(`TokenRefresher` 의 reqwest 백엔드) — **유일한 POST 예외**, `#[allow(clippy::disallowed_methods)]` 로 좁게 예외 처리
- clippy `disallowed_methods` 3계층 강제 활성화(REQ-19) — reqwest 도입과 **동시에**
- 401 분류기(§2)
- 리전 해석(REQ-17)
- `redact_transport_error(reqwest::Error)` — `redaction` 모듈 **안에** 만든다(A1 이 `redact_json`/`redact_text` 를 private 으로 내린 이유: `body_preview` 만이 안전한 진입점이라는 계약을 타입으로 강제)
- **스코프 provenance 개선**: OAuth 토큰 응답의 granted scope 가 JWT payload advisory 보다 나은 출처다. Tesla refresh 응답에 `scope` 가 들어오는지는 **증거가 없다**(현행 `tokens.json` 은 3키만 저장) → A3 가 실제 응답을 보고 결정. 지금 필드를 추가하면 생산자 없는 추측 코드가 된다.

## 14. 미해결 (계획 단계에서 확정)

> **✅ 1~2c 는 계획 작성 중 전부 확정됐다** (2026-07-31). 구현계획 `docs/design/plans/2026-07-31-fleetwire-a2-token-layer.md` 참조.

1. ~~`file_lock` 안정화 버전~~ → **1.89.0 확정.** 로컬 std 소스가 authoritative: `#[stable(feature = "file_lock", since = "1.89.0")]` (`try_lock`·`try_lock_shared`·`unlock` 모두 동일). `rust-version` 1.85.0 → 1.89.0, CI MSRV 매트릭스 동반 수정.
   부수 확인: `TryLockError` 는 `{ Error(io::Error), WouldBlock }` 2-variant 다 — 경합(재시도)과 실제 입출력 오류(중단)를 구분해 준다. 이 구분을 뭉개면 디스크 오류에서 deadline 까지 무의미하게 재시도한다.
2. ~~`base64` 라이선스·의존성~~ → **0.23.0 확정, 전이 의존성 0개.** ⚠️ **기본 feature 에 `simd-unsafe` 가 있다** — `default-features = false, features = ["std"]` 가 필수다. 자격증명 경로 의존성에 SIMD unsafe 코드를 기본으로 끌어들이지 않는다. `tokio` 도 `default-features = false` 로 `sync`+`time` 만(테스트용 `macros`/`rt` 는 dev-dependencies 로 분리).
2b. ~~만료 skew~~ → **60초 확정.** Tesla access token 수명이 8시간대라 약 0.2% 여유다.
2c. ~~락 재시도 파라미터~~ → **deadline 10초 / backoff 초기 5ms / 상한 200ms 확정.**
3. `scp` JSON 중복 키 방어 여부 — 중복 키는 파서 간 해석이 달라 "잘못된 DoesNotContain" 을 만들 수 있다. 다만 토큰은 사용자 자신의 것이라 적대적 입력이 주 위협은 아니다. A2 는 **`DoesNotContain` 판정이 배열의 성공적 파싱을 요구**하는 선에서 그치고, 중복 키 탐지는 만들지 않는다(문서화).

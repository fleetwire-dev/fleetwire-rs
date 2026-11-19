# fleetwire A2 완료 — A3 로 넘기는 항목

> A2(토큰 계층 — 저장·회전·프로세스간 single-flight·스코프 advisory) 완료 시점의 이월 목록.
> 코드 최종 HEAD `40858d8` (`<workspace>` main), A2 = 14커밋, 16파일 +4357/-26, 테스트 121→**227**, 최종 리뷰 판정 **Ship**.
> SDD 워크스페이스는 삭제됨 — 판단 근거가 필요한 항목만 여기 보존한다.

## A3 착수 전에 반드시 결정할 것

**1. `expires_in_secs` 의 하한은 주인이 없다.**
A2 가 **상한**(30일 clamp)은 소유한다 — A3 는 중복하지 말 것. **하한은 무주공산이다.** `is_fresh` 가 `expires_at > now + 60`(EXPIRY_SKEW) 이므로 서버가 `expires_in <= 60` 을 주면 토큰이 쓰이는 즉시 stale 이고 **매 `access_token()` 이 refresh POST + 체인 회전**을 태운다. `expires_in: 0` 은 `u64::MAX` 보다 훨씬 그럴듯한 입력이고 시계가 망가질 필요도 없다.
- ⚠️ **floor(`max`)를 쓰지 말 것.** 상한 clamp 가 안전한 건 `min` 이 **축소적**(`min(x,c) ≤ x`)이기 때문이다 — 일찍 갱신할 뿐 죽은 토큰을 보내지 않는다. `max` 는 진짜 30초짜리 수명을 120초로 부풀려 **이미 죽은 access token 을 60초 넘게 전송**한다. rate-limit 소모를 인증 실패로 바꾸는 나쁜 교환이다.
- 선택지 (a) 파서에서 `expires_in <= EXPIRY_SKEW` 를 **hard error** 로 승격, 또는 (b) REQ-12 아래 **회전 최소 간격**. ⚠️ (a)의 **경고 로그 형태는 소모를 관측만 가능케 하고 막지 못한다** — 로그 한 줄을 해결로 착각하지 말 것.
- 현재 동작은 `a_reported_lifetime_inside_the_skew_window_is_stale_on_arrival` 이 고정하고 있다(61=cached / 60·0=call_count 2).

**2. reqwest feature unification 은 예산 질문이다.**
fleetwire 의 `default-features = false` 는 유지되지만, reqwest 가 `net`/`rt`/`io-util` 을 **같은 tokio 빌드에 병합**하므로 `mio`/`libc`/`socket2` 가 어차피 들어온다. lockfile diff 에서 놀라지 말고 착수 시점에 명시적으로 정할 것.

**3. clippy `disallowed_methods` 는 reqwest 도입과 *같은 커밋*에 들어가야 한다.**
REQ-19 와 design §12 가 read-only 강제를 3계층으로 요구하는데 현재 `unsafe_code = "forbid"` 하나뿐이고 **`clippy.toml` 이 레포에 아예 없다.** 지금은 HTTP 의존성이 없어 안전할 뿐이다. reqwest 가 들어온 뒤에 붙이면 그 사이 커밋들이 무방비다.

**4. refresh 요청 timeout 은 `state_deadline`(10초)보다 **짧아야** 한다.**
"유한하면 된다"가 아니다. tokio state mutex 가 refresh POST 를 가로질러 유지되므로, A3 전송의 timeout 이 10초를 넘으면 **느리지만 성공하는 refresh 도중에** 프로세스 내 형제 호출자들이 `token state lock:` 오류를 받는다. 통상 경합은 안전하다(refresh 1회가 큐를 비우고 이후 호출자는 신선한 캐시에 걸리므로 대기는 왕복 1회분이지 N회분이 아니다).

## A3 가 구현할 것 (A2 가 자리를 비워둔 것)

**5. `TokenRefresher` 의 reqwest 백엔드 — 유일한 POST 예외.**
`Refreshed::new(access_token, refresh_token, expires_in_secs)` 로 만들면 된다(A2 최종 wave 에서 추가 — 그전엔 `#[non_exhaustive]` 에 생성자가 없어 **크레이트 밖에서 구현 불가**였다). `#[allow(clippy::disallowed_methods)]` 로 좁게 예외 처리할 것.

**6. `redact_transport_error(reqwest::Error)` 는 `redaction` 모듈 **안에** 만들 것.**
A1 이 `redact_json`/`redact_text` 를 private 으로 내린 이유가 "`body_preview` 만이 안전한 진입점"을 타입으로 강제하기 위해서다. ⚠️ A2 에서 그 계약이 **한 번 깨졌다** — `Refreshed` 가 `#[derive(Debug)]` 로 두 토큰을 그대로 찍었고 최종 리뷰가 잡았다. 모듈 경계를 스타일이 아니라 load-bearing 으로 취급할 것. 현재 hand-written `Debug` 는 `TokenSet`·`Refreshed` 둘이고 둘 다 destructure + `redaction::REDACTED` 를 쓴다.

**7. REQ-8 은 술어만 완성됐다 — 정책은 없다.**
`observe_scopes` 는 구현·테스트 완료지만 `pub(crate)` 이고 **호출자가 없으며** `expect(dead_code)` 로 연명 중이다. design §6.4 가 `with_unverified_jwt_scope_advisory()` 를 **A2 의** 모양으로 명명했으나 요청 파이프라인이 없어 만들지 않았다. 상태 문구는 "REQ-8 완료"가 아니라 **"REQ-8 술어만"**이어야 한다.
- ⚠️ 배선 시 **`Indeterminate` 와 `ContainsRequiredScope` 를 코드에서 동일하게 다룰 것** — 호출부에서 갈라지면 fail-open 이 거기서 깨진다.
- `MissingScope` 의 doc 에 design §9 가 요구한 caveat 이 아직 없다("검증되지 않은 토큰에서 관찰된 값" + "advisory 비용 가드이지 권한 판정이 아님").

**8. 401 분류기 · 리전 해석 · `Error::Api.message`.**
401 분류는 Tesla 의 실제 401 바디 증거가 있어야 한다(A2 는 증거 없이 쓰지 않으려고 미뤘다). `Error::Api.message` 는 현재 `status.canonical_reason()` 뿐이라 `HTTP 502 Bad Gateway: Bad Gateway` 로 렌더된다 — Tesla 의 `error`/`error_description` 을 `body_preview` 경유로 채울 것.

## 원리상 열려 있는 것 (설계 변경이 필요)

**9. `kv_secret` 멱등 가드의 잔여 우회 (A1 이월).**
`code=[REDACTED]<secret>` 는 통과한다. `caps[2]` 검사만으로는 폐쇄 불가 — redaction 이 단사가 아니라 `code=[REDACTED]x` 가 정상 1패스 결과인지 통째 주입인지 바이트로 구분할 수 없다. 단일 패스 redaction 이나 첫 글자가 값 클래스 밖인 sentinel 이 필요하다. `SECURITY.md` 로 옮길 것.

**10. `acquire` 는 락 경로의 symlink 를 따라간다.**
`store()` 는 토큰·임시 경로 양쪽에서 거부하는데 `acquire` 는 방어가 없다. `tokens.lock` 이 다른 곳을 가리키면 **두 holder 가 두 inode 를 잡아 상호배제가 조용히 사라진다** — 이 모듈이 존재하는 이유 자체가 무너진다. 실무상 `prepare_dir()` 의 0700 검사가 먼저 걸려 막히지만, `load()` 가 이미 쓰는 `symlink_metadata` → open → `ino`/`dev` 재확인 패턴이 ~6줄이면 된다. 최소한 수용된 위협모델을 `acquire` 의 rustdoc 에 적을 것(현재 리뷰 리포트에만 존재).

## 알아둘 것 (수정 불요)

- **`refresh_forbidden` 은 per-manager 다** — per-process 가 아니다. 같은 디렉터리에 `TokenManager` 를 하나 더 만들면 플래그가 깨끗하다. per-directory process-global 이 더 강한 형태.
- **`StoreError::NotFound` 가 일반 `Error::Auth` 로 표면화**된다 — "아직 로그인 안 함"은 이 라이브러리의 가장 흔한 상태인데 메시지 문자열 grep 으로만 판별된다. `Error` 가 `#[non_exhaustive]` 라 variant 추가는 non-breaking 이지만 **1.0 이전에 정할 것**.
- **`~/.fleetwire/` 기본 경로가 미구현** — design §4 가 규정했으나 `TokenManager::new(dir, …)` 는 오버라이드 절반만 있다. A3/A4 의 `Client` 빌더가 소유.
- **`tests/` 통합 테스트는 `cfg(test)` 없이 컴파일**되므로 `FixedClock` 사용 불가 — clock 주입하려면 `test-util` feature 가 필요하다.
- **`LOCK_DEADLINE` 10초가 공개 설정 불가**(테스트만 `with_lock_deadline`). `state_deadline` 도 동일.
- **stale 임시 파일이 청소되지 않는다** — `.tokens.json.tmp.<pid>.<ThreadId>` 는 자기 경로만 정리한다. 하드 크래시가 남긴 것은 0600 이라 노출은 없으나 자격증명 물질이 무한 축적된다.

## A2 가 확립한 테스트 규율 (A3 도 이어받을 것)

A1 은 redaction 하나에 fix 라운드 4회를 썼고 원인이 매번 같았다 — **테스트 입력을 구현에 맞춰 재단**했다. A2 는 이를 규율로 명문화해 매 brief 에 넣었고, 8태스크 fix 5회로 줄었으며 **한 태스크도 2회를 넘지 않았다.**

1. **X 를 금지하는 규칙에는 X 를 했을 때만 실패하는 입력이 필요하다.**
2. **자기 테스트를 mutation-test 하고 생존자를 정직하게 분류하라.** 죽일 수 없으면 **무엇이 필요한지와 그 비용**을 대라 — "안 하기로 했다"를 "할 수 없다"로 격상하지 말 것. 이 서브프로젝트에서 한 번 적발됐고 두 번 정정됐다.
3. **값이 아니라 기전을 단언하라.** A2 에서 두 번 물렸다 — 16-way 동시성 테스트가 Mutex 가 아니라 flock 을 검증하고 있었고, `..._served_from_memory_...` 가 이름만큼 증명하지 못했다. 해법은 카운터(`lock_attempts`)로 **기전을 세는 것**이었다.
4. **커버리지를 부풀리지 말 것.** T6 은 판별력 없는 입력(payload 안의 점)을 커버리지에서 **제외**했고, T8 은 brief 가 예측한 실패가 나지 않자 near-duplicate 를 넣는 대신 **mutation 으로 기존 커버리지를 증명**하고 그 자리에 진짜 새 테스트를 넣었다.
5. **vacuous 하게 통과할 수 있으면 큰 소리로 말할 것** — root 에서 무력화되는 테스트는 `eprintln!` 로 알린다. A2 에서 침묵하던 한 건이 최종 리뷰에 적발됐다(root CI 에서 poison 계약 전체가 무효화되는데 green).

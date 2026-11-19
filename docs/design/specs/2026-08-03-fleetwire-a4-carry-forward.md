# fleetwire A4 완료 — A5 로 넘기는 항목

> A4(엔드포인트 완성 · 페이지네이션 · 비용 게이트 · 쿼터 관측) 완료 시점의 이월 목록.
> 코드 최종 HEAD `7889451` (`<workspace>` main), A4 = **17커밋**, 테스트 316→**366**,
> 최종 whole-branch 리뷰(opus) 판정 **Ship**.

---

## 0. A4 가 만든 것

| | |
|---|---|
| 엔드포인트 | **16개 전부**(명시 impl 10 + 매크로 6). 유료는 정확히 2개 |
| 페이지네이션 | `pagination` 형제 키가 호출자에게 **도달**한다(A3 까지는 전량 폐기됐다) |
| 비용 3수단 | 타입 `Pricing` · 게이트 `send_guarded` · 관측 `Metered` |
| 408 | **실측 기반** 분류(문서 근거 아님) |
| read-only | POST 는 여전히 `auth/refresh.rs` 한 곳 |

---

## 1. A5 착수 전에 알아야 할 것

**1. `send_guarded` 와 `send_metered` 는 합성되지 않는다 — A5 의 API 결정.**
게이트는 유료 2개를 위한 것인데, **그 2개만** `ratelimit-*` 헤더를 낸다. 즉 비용을 아끼려고
게이팅하면 쿼터 잔량을 볼 수 없고, 쿼터를 보려면 게이트를 포기해야 한다. `lib.rs` 는 세 수단을
상호보완으로 제시하면서 **둘이 배타적이라는 사실을 말하지 않는다.** 최종 리뷰가 Important 로
잡았고 A5 로 이월됐다. `send_guarded` 가 `send` 대신 `send_inner` 를 쓰고 `Guarded<T>` 에
rate-limit 채널을 두는 것이 자연스러운 방향이나, **API 모양 결정이므로 설계 단계에서 정할 것.**

**2. 게이트는 무과금 엔드포인트에 쓰면 순손실이다.**
`R: VehicleEndpoint` 가 11개 구현을 받는데 9개가 무과금이다. `send_guarded(&GetVehicle::new(vin)?)`
는 **같은 요청을 두 번** 보낸다. doc 에 적혀 있으나 타입으로는 못 막는다 — 유료 엔드포인트만
받는 별도 trait 을 둘지 A5 가 판단할 것.

**3. 요청 상한**: `send` 는 `max_attempts + RECOVERY_BUDGET(2)`, `send_guarded` 는 그 **두 배**
(기본 6). 단 billed 는 최대 절반이라 **게이트가 ungated 보다 돈이 더 들 수는 없다**(최종 리뷰 검증).

**4. `Page.pagination` 은 `Option` 이다.** `None` = 서버가 안 줬다. 전 필드 `None` 인 `Some` 과
**다르다** — 기본값으로 채우면 호출자가 `next == None` 을 "다음 페이지 없음"으로 읽어 순회를
조기 종료한다.

**5. 응답 타입 다수가 `serde_json::Value` 다.** 실응답을 못 본 엔드포인트는 필드명을 추측하지
않았다. `#[serde(flatten)] extra` 가 전부 보존하므로 데이터 손실은 없다. 실응답 확보 시 타입화가
A5 의 일이다.

---

## 2. 미결 (O 항목)

| # | 항목 | 해소 조건 |
|---|---|---|
| O8 | 만료 access token 의 401 형태 | 만료 토큰 실측 |
| O9 | 무인증 요청의 과금 여부 | 사용량 대시보드 |
| O10 | **5xx 미측정**(408 절반은 A4 가 해소) | 5xx 실응답 캡처 |
| O11 | 중국 리전 호스트 — 기본 allowlist 에서 **의도적 제외** | 증거 확보 |
| O12 | `per_page` 기본값 — 매트릭스 100 vs 실측 1(차량 1대) | 차량 2대 이상 |
| O13 | 최상위 `count` 가 `pagination.count` 와 갈릴 수 있는가 | 다건 목록 |
| O14 | 에러 경로 rate-limit 미노출(`Metered` 는 성공 경로만) | A5 판단 |
| O15 | 408 을 status 만으로 분류 — 게이트웨이발 408 오분류 | 다른 408 관측 |
| O16 | 쿼터가 계정 공유인가 차량 전용인가 | 2대 이상/다중 앱 |
| O17 | 무과금 14개의 카테고리가 **매트릭스 전제**이지 실측 아님 | 대시보드 |
| O18 | rate-limit 창 종류·`reset` 경계 동작 | 창 경계 연속 측정 |
| O19 | `nearby_charging_sites` 의 `detail` wire form | 차량 online 시 |
| O20 | O1 의 스코프 서술 헤지(`users/orders`·`feature_config` = "user_data**류**") | 라이브 검증 |

## 3. Parked (최종 리뷰 잔여, load-bearing 아님)

**`client.rs:1018`** 이 `"(the probe never is [billed])"` 를 **사실로 단언**한다. 같은 크레이트가
O17 에서 이를 미결로 두고 `lib.rs` 세 곳에서 "강한 정황이지 증거 아님"으로 헤지한다.
돈 관점의 결론은 방향상 옳으나 문구가 크레이트 자신의 헤지 규율과 불일치한다.
**A5 첫 커밋에서 한 줄 수정 권장**: `"(the probe is unmetered and, on the O1 evidence, unbilled — O17)"`.

## 4. Deferred minor (최종 리뷰가 future work 로 triage)

- `Error::Auth` 에 `txid` 없음 — 유료 401 은 `Billed` 인데 상관관계 ID 를 버린다
- `Metered` 의 헤더별 파싱실패 커버리지가 `limit` 만(셋 다 같은 `num()` 클로저라 저위험)
- T9 `.expect` 불변식 — 향후 in-crate `VehicleEndpoint` 구현이 자기 `new` 에서 검증 안 한
  `vin()` 을 반환하면 조용히 깨진다. 현재 11개 전부 한 패턴(sealed trait 이 외부는 차단)
- `nearby_charging_sites` 쿼리 테스트가 집계형 — 적색 시 어느 파라미터인지 진단 어려움

---

## 5. A4 가 확립한 것 (A5 도 이어받을 것)

**게이트 5종** (⚠️ MSRV 는 `build` 가 아니라 `test --no-run`):

```bash
cargo fmt --all -- --check
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo test --locked
cargo +1.89.0 test --locked --no-run
cargo doc --no-deps --locked
```

**⚠️ mutation 위생 (A4 에서 두 번 데인 뒤 확립)**:
1. **구현을 먼저 커밋**하고 그 다음 mutation 을 돌린다 — T5 가 미커밋 상태에서
   `git checkout --` 를 돌려 작업을 통째로 날렸다.
2. 실파일을 손으로 되돌리지 말고 **git 이 복원을 보증**하게 한다 — T3 은 손복원이 한 arm 을
   놓쳐 **변이가 커밋에 살아남았고** 리뷰어가 잡았다.
3. mutation 라운드 후 `git status` 청결·`git diff` 공백을 **확인하고 보고**한다.

**⚠️ 계획의 mutation 표를 믿지 말 것**: A4 에서 **6번**(T1·T2·T3·T6·T7·T10) 계획이 지목한
"킬링 테스트"가 변이된 줄에 **도달하지 않았다**. 매번 구현자가 잡아 실제 테스트를 추가했다.
근본 원인은 표를 쓸 때 "지목한 테스트가 그 줄을 실행하는가"를 검증하지 않은 것.
**A5 계획 작성 시 mutation 마다 그 확인을 명시적으로 할 것.**

**검증수준 분리**: 실측한 것과 문서 근거뿐인 것을 크레이트 문서가 명확히 가른다.
특히 **게이트 무과금**은 `send_guarded` 전체가 그 위에 지어졌음에도 "강한 정황이지 증거 아님"을
끝까지 유지했다 — 최종 리뷰가 가장 드리프트하기 쉬운 지점으로 지목했고 통과했다.

**read-only 강제**: 본진은 **타입 시스템 + 모듈 가시성**, clippy 는 **보조**.
`lib.rs` 가 clippy 가 못 막는 4가지를 명시한다. **"3계층이 강제한다"고 쓰지 말 것.**

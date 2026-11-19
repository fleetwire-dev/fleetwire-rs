# O1 — fleetwire 엔드포인트 증거 매트릭스 (A 구현 착수 게이트)

> **날짜** 2026-07-29 · **게이트** A(읽기 라이브러리) 구현 착수 전 필수(설계 스펙 §7·SDD 미결 O1)
> **목적** 16개 엔드포인트의 스코프·**과금 카테고리**·content-type·페이지네이션을 **증거로 확정** — "검증 못 한 특성을 강타입 OSS로 내보내지 않는다"
> **소스** 내부(SDD REQ-1·design §4, [[fleet-api-pricing]] memory, 동작 중 MCP tesla_client.py) + Tesla 공식문서/**OpenAPI 임베드** deep research(2026-07-29). raw: scratchpad `oss-research/10-o1-tesla-billing.md`

## 🔴 핵심 발견 — 과금은 16개 중 2개뿐 (SDD 가정 정정)

Tesla OpenAPI 각 operation의 `security[].pricing_category` 필드가 권위 소스:
- **유료 Data(`device_data` ≈ $0.002)**: **`vehicle_data`, `nearby_charging_sites`** — 단 2개
- **무과금(`pricing_category: null` → docs "This endpoint is not charged.")**: 나머지 **14개** (vehicles 목록·vehicle 상태·mobile_enabled·recent_alerts·release_notes·service_data·drivers·invitations·fleet_telemetry_config·fleet_telemetry_errors·users/me·region·orders·feature_config)
- **상시 무료 "Discovery" 티어는 없음** — 2025-01-01 이전 discovery 무료기간은 종료. 현재 무료 = 무과금 엔드포인트(`null`) + 월 $10 크레딧.

> ⚠️ **SDD 정정 필요**: design §4 "모든 항목의 가격은 `Pricing::Data { usd_per_request: 0.002 }`" 는 **14개에 대해 틀림**. `Pricing`을 **per-endpoint**(`NotCharged` | `Data{0.002}`)로 바꿔야 함. 이것이 O1 게이트의 최대 산출.

## 증거 매트릭스 (16)

| # | 엔드포인트 | 스코프 | **과금** | content-type | 페이지네이션 | 근거 |
|---:|---|---|---|---|---|---|
| 1 | `GET /api/1/vehicles` | vehicle_device_data | **무과금**(null) | json | **page·per_page**(기본 100), resp `pagination{previous,next,current,per_page,count,pages}` | OpenAPI null·vehicle-endpoints |
| 2 | `GET /api/1/vehicles/{vin}` | vehicle_device_data | **무과금**(null) | json | 없음 | OpenAPI null. state 게이트=무료(권고) |
| 3 | `.../vehicle_data` | vehicle_device_data (+vehicle_location 위치 시) | **Data $0.002** | json | 없음, query `endpoints`(location_data 등) | OpenAPI device_data |
| 4 | `.../mobile_enabled` | vehicle_device_data | **무과금**(null) | json | 없음 | OpenAPI null |
| 5 | `.../nearby_charging_sites` | vehicle_device_data | **Data $0.002** | json | page 아님, query `count`·`radius`·`detail` | OpenAPI device_data |
| 6 | `.../recent_alerts` | vehicle_device_data | **무과금**(null) | json | 없음 | OpenAPI null |
| 7 | `.../release_notes` | vehicle_device_data | **무과금**(null) | json | 없음 | OpenAPI null |
| 8 | `.../service_data` | vehicle_device_data | **무과금**(null) | json | 없음 | OpenAPI null |
| 9 | `.../drivers` | vehicle_device_data | **무과금**(null) | json | 없음 | OpenAPI null |
| 10 | `.../invitations` | vehicle_device_data | **무과금**(null) | json | **page·per_page**(최대 25) | OpenAPI null |
| 11 | `.../fleet_telemetry_config` | vehicle_device_data | **무과금**(null) | json | 없음 | OpenAPI null |
| 12 | `.../fleet_telemetry_errors` | vehicle_device_data | **무과금**(null) | json | 없음(optional partner-client-id) | OpenAPI null |
| 13 | `GET /api/1/users/me` | **user_data**(openid 아님/부가) | **무과금**(null) | json | 없음 | OpenAPI null·user-endpoints |
| 14 | `/api/1/users/region` | 여러 스코프 중 하나 | **무과금**(null) | json | 없음 | OpenAPI null. `fleet_api_base_url` 반환 |
| 15 | `/api/1/users/orders` | user_data류 | **무과금**(null) | json | 없음 | OpenAPI null |
| 16 | `/api/1/users/feature_config` | user_data류 | **무과금**(null) | json | 없음 | OpenAPI null |

**공통**: 응답 봉투 `{"response": ...}` (MCP 실증), `Accept: application/json`. path param 현행 OpenAPI는 `{vin}`(docs는 id/tag 혼용). user 엔드포인트는 **user-context 토큰** 필수(3rd-party business 토큰 비호환).

## 전역 과금 규칙 (재확인, [[fleet-api-pricing]])
- Data $0.002·Commands $0.001·Wakes $0.02·Streaming $1/150k. 월 $10 크레딧(계정당).
- status **<500 = billed**(유료 카테고리 한정), **≥500 = not billed**. 408도 유료 카테고리면 과금 — **단 408은 vehicle_data 등 유료 경로에서만 의미** (무과금 14개는 408도 $0).
- rate limit: Realtime Data 60/분·Wakes 3/분·Commands 30/분 (디바이스+계정 공유).
- ⚠️ 정책이라 **분기 재검증**(다음 재확인 ~2026-10).

## SDD 정정 항목 (다음 단계 = A SDD 검증에서 반영)
1. **Pricing 모델 per-endpoint화** — `Pricing::{NotCharged, Data{usd_per_request:0.002}}`. design §4 "전부 Data" 정정. 유료=`vehicle_data`·`nearby_charging_sites` 2개만.
2. **Billing 판정** — 무과금 14개는 status 불문 `Billing::NotBilled`(고정). 유료 2개만 status<500=Billed 규칙 적용. `Error::billing()`이 엔드포인트 카테고리를 알아야 함.
3. **비용 게이트/재시도** — 무과금 엔드포인트는 재시도해도 중복과금 없음(단 rate limit은 여전). "state 게이트=무료"가 실증됨 → 게이팅 로직 근거 강화.
4. **user 스코프** — SDD "user 4개=openid"를 `user_data`(등 user-context 스코프)로 정정. region은 스코프 유연.
5. **페이지네이션** — `page`+`per_page`(offset 아님). vehicles 100·invitations 25. `nearby_charging_sites`는 page 아닌 `count`/`radius`/`detail` → 별도 QuerySpec.

## 게이트 상태
✅ **O1 증거 확보 완료** — 16개 전부 스코프·과금·content-type·페이지네이션 확정(공식 OpenAPI 근거). A 구현 착수 게이트 통과.
**단 선행 조건**: 위 SDD 정정 5건을 A SDD(design/requirements)에 반영 후 writing-plans로 진행(과금 모델이 타입·에러·게이트 설계를 좌우).

⚠️ **증거 한계**: pricing_category는 Tesla OpenAPI 임베드(webpack chunk) 파싱 기반 — 공식이나 UI expand-on-click 뒤에 있어 조사 도구가 추출. 분기 재검증 권장. 원한다면 동작 중 MCP로 `/vehicles`(무과금 예상) 실호출해 대시보드 청구 대조 가능(단 청구는 지연 반영).

## 참고
[[fleet-api-pricing]] · [설계 스펙](2026-07-24-fleetwire-read-client-design.md) §4·§7 · [SDD](fleetwire-read-client/) REQ-1·10 · [grok-build 교훈](2026-07-28-grokbuild-design-lessons.md)(RetryPolicy/Disposition·Billing)

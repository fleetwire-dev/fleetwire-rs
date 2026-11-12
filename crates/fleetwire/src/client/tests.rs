use super::*;
use crate::api::passthrough_endpoint;
use crate::api::users::{GetRegion, RegionInfo};
use crate::api::vehicles::{DataEndpoint, GetVehicleData, ListInvitations, ListVehicles};
use crate::model::{Invitation, Page, VehicleSummary};
use crate::{Refreshed, TokenRefresher};
use fleetwire_testkit::matchers::{header, method, path};
use fleetwire_testkit::{Mock, MockServer, ResponseTemplate};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

/// 봉투가 벗겨지고 타입이 나온다.
#[test]
fn send_unwraps_envelope() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/users/region"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            br#"{"response":{"region":"na","fleet_api_base_url":"https://fleet-api.prd.na.vn.cloud.tesla.com"}}"#.to_vec(),
            "application/json",
        ))
        .mount(&server);

    let c = test_client(&server);
    let got: RegionInfo = c.send(&GetRegion).unwrap();
    assert_eq!(got.region.as_deref(), Some("na"));
}

/// charset 이 붙어도 붙지 않아도 파싱된다(실측: 성공은 charset 있음, 412 는 없음).
#[test]
fn accepts_json_content_type_with_and_without_charset() {
    for ct in ["application/json", "application/json; charset=utf-8"] {
        let server = MockServer::start();
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(br#"{"response":{"region":"na"}}"#.to_vec(), ct),
            )
            .mount(&server);
        let c = test_client(&server);
        let got: RegionInfo = c.send(&GetRegion).unwrap();
        assert_eq!(got.region.as_deref(), Some("na"), "content-type={ct}");
    }
}

/// `send` 은 봉투 안쪽 배열/스칼라 어느 쪽도 다룬다 — `Vec<VehicleSummary>` 로
/// 시험해 `RegionInfo` 하나짜리 타입에서만 도는 우연을 배제한다.
#[test]
fn send_decodes_a_list_response_too() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/vehicles"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            br#"{"response":[{"vin":"5YJ3E1EA7KF000000"}]}"#.to_vec(),
            "application/json",
        ))
        .mount(&server);

    let c = test_client(&server);
    let got: Page<crate::model::VehicleSummary> = c
        .send(&crate::api::vehicles::ListVehicles {
            page: None,
            per_page: None,
        })
        .unwrap();
    assert_eq!(got.items.len(), 1);
    assert_eq!(got.items[0].vin.as_deref(), Some("5YJ3E1EA7KF000000"));
}

/// 실측 픽스처의 pagination 형제 키가 Page 에 도달한다.
#[test]
fn pagination_sibling_reaches_the_caller() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/vehicles"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(
                crate::test_support::FIXTURE_VEHICLES_PAGED
                    .as_bytes()
                    .to_vec(),
                "application/json",
            ),
        )
        .mount(&server);

    let c = test_client(&server);
    let page: Page<VehicleSummary> = c
        .send(&ListVehicles {
            page: None,
            per_page: None,
        })
        .unwrap();

    assert_eq!(page.items.len(), 1);
    let p = page.pagination.expect("서버가 pagination 을 줬다");
    assert_eq!(p.count, Some(1));
    assert_eq!(p.pages, Some(1));
    assert_eq!(p.current, Some(1));
}

/// pagination 이 없으면 None 이다 — 전 필드 None 인 Some 과 구분된다.
#[test]
fn absent_pagination_is_none_not_defaulted() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/vehicles"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            br#"{"response":[{"vin":"5YJ3E1EA1JF000001"}]}"#.to_vec(),
            "application/json",
        ))
        .mount(&server);

    let c = test_client(&server);
    let page: Page<VehicleSummary> = c
        .send(&ListVehicles {
            page: None,
            per_page: None,
        })
        .unwrap();

    assert_eq!(
        page.items.len(),
        1,
        "목록 자체는 유효하므로 실패시키지 않는다"
    );
    assert!(page.pagination.is_none(), "부재는 None 이어야 한다");
}

/// Task 6 리뷰 Important 조치: `ListInvitations` 은 `ListVehicles` 와 달리
/// `passthrough_endpoint!` 를 쓰지 않고 `assemble` 을 손으로 작성했다 —
/// Task 2 의 페이지네이션 플러밍(`Envelope`→`decode`→`send`→`assemble`)이
/// `ListVehicles` 전용이 아니라는 주장은 `send` 를 실제로 거쳐야 증명된다.
/// 직접 `assemble` 을 부르는 단위 테스트(`api::vehicles::tests::
/// invitations_assemble_preserves_items_and_pagination`)만으로는 전송
/// 경로 자체가 배선돼 있는지 알 수 없다 — items 만 확인하면 형제
/// `pagination` 키가 실제로 살아남았는지 증명하지 못하므로 둘 다 확인한다.
#[test]
fn invitations_pagination_sibling_reaches_the_caller_through_send() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/vehicles/5YJ3E1EA1JF000001/invitations"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            br#"{"response":[{"id":"inv-1"}],"pagination":{"count":30,"pages":2,"current":1,"next":2,"per_page":25}}"#.to_vec(),
            "application/json",
        ))
        .mount(&server);

    let c = test_client(&server);
    let page: Page<Invitation> = c
        .send(&ListInvitations::new("5YJ3E1EA1JF000001", None, None).unwrap())
        .unwrap();

    assert_eq!(page.items.len(), 1, "items 도 실려야 한다");
    let p = page
        .pagination
        .expect("형제 pagination 키가 send 를 거쳐 살아남아야 한다");
    assert_eq!(p.count, Some(30));
    assert_eq!(p.pages, Some(2));
    assert_eq!(p.next, Some(2));
}

/// pagination 이 없는 응답이면 `send` 를 거친 뒤에도 `None` 이어야
/// 한다(전 필드 `None` 인 `Some` 과는 다른 사실) — 이 구분이 조용히
/// 회귀할 가능성이 가장 높다는 리뷰 지적에 따라 추가.
#[test]
fn invitations_absent_pagination_is_none_through_send() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/vehicles/5YJ3E1EA1JF000001/invitations"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            br#"{"response":[{"id":"inv-1"}]}"#.to_vec(),
            "application/json",
        ))
        .mount(&server);

    let c = test_client(&server);
    let page: Page<Invitation> = c
        .send(&ListInvitations::new("5YJ3E1EA1JF000001", None, None).unwrap())
        .unwrap();

    assert_eq!(
        page.items.len(),
        1,
        "목록 자체는 유효하므로 실패시키지 않는다"
    );
    assert!(page.pagination.is_none(), "부재는 None 이어야 한다");
}

/// passthrough 엔드포인트는 pagination 이 와도 영향받지 않는다.
#[test]
fn passthrough_endpoint_ignores_a_pagination_sibling() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/users/region"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            br#"{"response":{"region":"na"},"pagination":{"count":7}}"#.to_vec(),
            "application/json",
        ))
        .mount(&server);

    let c = test_client(&server);
    let got: RegionInfo = c.send(&GetRegion).unwrap();
    assert_eq!(got.region.as_deref(), Some("na"));
}

/// page/per_page 가 쿼리에 실린다(요청 쪽 회귀 가드).
#[test]
fn list_vehicles_sends_pagination_params() {
    let q = ListVehicles {
        page: Some(3),
        per_page: Some(25),
    }
    .query()
    .to_encoded();
    assert!(
        q.contains("page=3") && q.contains("per_page=25"),
        "실제: {q}"
    );
}

/// Fix round 1 (Important 2): 손상된 VIN 은 `GetVehicle::new` 단계에서
/// 막힌다 — 그 결과로 `Client::send` 를 호출할 대상 자체가 없다. "에러가
/// 났다" 만으로는 "아무것도 안 나갔다" 를 증명하지 못하므로(리뷰 지적),
/// mock 서버에 아무 Mock 도 등록하지 않고 요청 기록으로 직접 확인한다 —
/// 무언가 요청이 갔다면 매칭되는 Mock 이 없어 wiremock 이 즉시 패닉하거나,
/// 최소한 `received_requests()` 가 비어 있지 않을 것이다.
#[test]
fn malformed_vin_never_reaches_the_wire() {
    let server = MockServer::start();
    // 의도적으로 어떤 Mock 도 mount 하지 않는다 — 요청이 나가면 즉시 드러난다.
    let _c = test_client(&server);

    // `.err().unwrap()` rather than `.unwrap_err()`: `GetVehicle` is
    // deliberately not `Debug` (it holds a VIN — see its doc), and
    // `unwrap_err()` would require that bound for its panic message.
    let err = crate::api::vehicles::GetVehicle::new("5YJ/../admin")
        .err()
        .unwrap();
    assert_eq!(err, crate::query::VinError::Unsafe);

    assert!(
        server.received_requests().unwrap().is_empty(),
        "생성이 실패했는데도 서버가 요청을 받았다"
    );
}

/// `send` 이 실제로 `Authorization: Bearer <token>` 을 붙이고, 정확히 한 번
/// 요청한다는 것을 **요청 자체**로 확인한다 — 값이 아니라 기전(규율 3).
#[test]
fn send_makes_exactly_one_request_carrying_the_bearer_token() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/users/region"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            br#"{"response":{"region":"na"}}"#.to_vec(),
            "application/json",
        ))
        .mount(&server);

    let c = test_client(&server);
    let _: RegionInfo = c.send(&GetRegion).unwrap();

    let reqs = server.received_requests().unwrap();
    assert_eq!(reqs.len(), 1, "정확히 한 번 요청해야 한다");
    let auth = reqs[0]
        .headers
        .get("authorization")
        .expect("Authorization 헤더가 있어야 한다")
        .to_str()
        .unwrap();
    assert!(auth.starts_with("Bearer "), "실제: {auth}");
}

/// 서버가 4xx 를 내면 `send` 이 그 상태에 맞는 `Error` 변종을 그대로 전파한다.
#[test]
fn send_propagates_a_4xx_as_an_error() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/users/region"))
        .respond_with(ResponseTemplate::new(401).set_body_raw(
            br#"{"error":"invalid authentication"}"#.to_vec(),
            "application/json",
        ))
        .mount(&server);

    let c = test_client(&server);
    let e = c.send(&GetRegion).unwrap_err();
    assert!(matches!(e, Error::Auth { .. }), "실제: {e:?}");
}

/// 이 태스크에서 요구한 `redirect::Policy::none()` 자체를 시험한다.
/// `transport.rs` 의 동명 성격 테스트는 그 파일이 직접 만든(하드코딩된)
/// reqwest::Client 로 정책을 시험하므로, `ClientBuilder::build()` 자신이
/// 그 정책 설정을 빠뜨려도 그 테스트는 계속 통과한다 — 이 테스트가 그
/// 간극(brief 이 이 태스크로 옮긴 요구사항)을 메운다.
#[test]
fn client_builder_does_not_follow_redirects() {
    let evil = MockServer::start();
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            br#"{"response":{"region":"evil"}}"#.to_vec(),
            "application/json",
        ))
        .mount(&evil);

    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/users/region"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", format!("{}/steal", evil.uri())),
        )
        .mount(&server);

    let c = test_client(&server);
    let r: Result<RegionInfo, _> = c.send(&GetRegion);

    assert!(r.is_err(), "redirect 를 따라가면 안 된다: {r:?}");
    assert!(
        evil.received_requests().unwrap().is_empty(),
        "🔴 reqwest 가 redirect 를 따라가 evil 호스트로 요청을 보냈다 \
         — ClientBuilder::build() 의 Policy::none() 이 빠졌다"
    );
}

// ------------------------------------------------------------ send_raw

/// **원문이 봉투째 그대로 나온다.** `send` 는 `Envelope<T>` 로 파싱해
/// `response` 멤버만 넘기고 모델에 없는 형제(`count` — O13)를 **말없이 버린다**.
/// 드리프트 감지는 그 버려지는 것을 보려고 존재한다.
#[test]
fn send_raw_returns_the_whole_body_including_unmodelled_siblings() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/vehicles"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            br#"{"response":[{"vin":"5YJ3E1EA7KF000000","tesla_added_this":7}],"count":1,"pagination":{"count":1}}"#.to_vec(),
            "application/json",
        ))
        .mount(&server);

    let c = test_client(&server);
    let got = c
        .send_raw(&ListVehicles {
            page: None,
            per_page: None,
        })
        .unwrap();

    // 봉투가 살아 있다.
    assert!(got.value.get("response").is_some(), "{:?}", got.value);
    // 모델에 없는 최상위 형제가 살아 있다 — 이것이 이 메서드의 존재 이유다.
    assert_eq!(got.value.get("count"), Some(&serde_json::json!(1)));
    assert!(got.value.get("pagination").is_some());
    // 그리고 Tesla 가 **추가한** 필드가 보인다.
    assert_eq!(
        got.value["response"][0]["tesla_added_this"],
        serde_json::json!(7)
    );

    // 비-vacuity — 같은 응답을 `send` 로 받으면 `count` 는 **사라진다**.
    //    이 대조가 없으면 위 단언은 "봉투를 안 벗긴다" 만 말하고,
    //    타입 경로가 정말로 정보를 잃는다는 것은 아무도 확인하지 않는다.
    let typed: Page<VehicleSummary> = c
        .send(&ListVehicles {
            page: None,
            per_page: None,
        })
        .unwrap();
    assert_eq!(typed.items.len(), 1);
    assert!(
        typed.items[0].extra.contains_key("tesla_added_this"),
        "관용적 타입이 새 필드를 흡수한다 — 그래서 typed 로는 드리프트를 못 본다"
    );
}

/// **4xx 가 `Value` 로 반환되면 안 된다.** `send_raw` 의 반환형이
/// `serde_json::Value` 라, API 에러 본문을 "성공한 데이터" 로 돌려주는 것이
/// 가장 그럴듯한 사고다. 상태 게이트가 본문을 보기 **전에** 걸린다.
#[test]
fn send_raw_turns_a_4xx_into_an_error_not_a_value() {
    for status in [400u16, 401, 403, 404, 408, 429, 500] {
        let server = MockServer::start();
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(status).set_body_raw(
                br#"{"response":null,"error":"nope"}"#.to_vec(),
                "application/json",
            ))
            .mount(&server);
        let c = test_client(&server);
        assert!(
            c.send_raw(&GetRegion).is_err(),
            "status={status} 가 Value 로 반환됐다"
        );
    }
}

/// 401/408 은 다른 에러로 분류된다 — `send` 와 **같은** 분류를 쓴다는 증거.
/// (408 은 잠든 차의 정상 응답이고 **과금된다**.)
#[test]
fn send_raw_classifies_errors_the_same_way_send_does() {
    for (status, want) in [(401u16, "auth"), (408, "offline"), (500, "api")] {
        let server = MockServer::start();
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(status)
                    .set_body_raw(br#"{"error":"x"}"#.to_vec(), "application/json"),
            )
            .mount(&server);
        let c = test_client(&server);
        let raw_err = c
            .send_raw(
                &GetVehicleData::new("5YJ3E1EA7KF000000", vec![DataEndpoint::ChargeState]).unwrap(),
            )
            .err()
            .expect("에러여야 한다");
        let kind = match &raw_err {
            Error::Auth { .. } => "auth",
            Error::VehicleOffline { .. } => "offline",
            Error::Api { .. } => "api",
            other => panic!("예상 밖 분류: {other:?}"),
        };
        assert_eq!(kind, want, "status={status}");
    }
}

/// **redirect 를 따라가지 않는다.** `send_raw` 가 `send_inner` 를 공유하므로
/// `Policy::none()` 이 그대로 적용된다 — 별도 경로를 만들었다면 여기서 깨진다.
#[test]
fn send_raw_does_not_follow_redirects() {
    let evil = MockServer::start();
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            br#"{"response":{"stolen":true}}"#.to_vec(),
            "application/json",
        ))
        .mount(&evil);

    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/users/region"))
        .respond_with(
            ResponseTemplate::new(302).insert_header("location", format!("{}/steal", evil.uri())),
        )
        .mount(&server);

    let c = test_client(&server);
    let r = c.send_raw(&GetRegion);
    assert!(r.is_err(), "3xx 가 Value 로 반환됐다");
    assert!(
        evil.received_requests().unwrap().is_empty(),
        "🔴 send_raw 가 redirect 를 따라가 evil 호스트로 요청을 보냈다"
    );
}

/// `ratelimit-*` 헤더가 `Metered.rate_limit` 로 실려 온다 — T21 의
/// rate-limit break 가 이 값을 쓴다.
#[test]
fn send_raw_carries_the_rate_limit_headers() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ratelimit-device-data-per-vehicle-limit", "60")
                .insert_header("ratelimit-device-data-per-vehicle-remaining", "57")
                .insert_header("ratelimit-device-data-per-vehicle-reset", "1785000000")
                .set_body_raw(br#"{"response":{}}"#.to_vec(), "application/json"),
        )
        .mount(&server);
    let c = test_client(&server);
    let got = c
        .send_raw(
            &GetVehicleData::new("5YJ3E1EA7KF000000", vec![DataEndpoint::ChargeState]).unwrap(),
        )
        .unwrap();
    let rl = got.rate_limit.expect("ratelimit 헤더가 실려오지 않았다");
    assert_eq!(rl.limit, 60);
    assert_eq!(rl.remaining, 57);

    // 비-vacuity: 헤더가 없는 응답은 `None` 이지 조작된 값이 아니다.
    let bare = MockServer::start();
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(br#"{"response":{}}"#.to_vec(), "application/json"),
        )
        .mount(&bare);
    let c2 = test_client(&bare);
    assert!(c2.send_raw(&GetRegion).unwrap().rate_limit.is_none());
}

/// content-type 이 JSON 이 아니면 거부한다 — 캡티브 포털 HTML 을 "본문" 으로
/// 삼키지 않는다.
#[test]
fn send_raw_refuses_a_non_json_content_type() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(b"<html>login</html>".to_vec(), "text/html"),
        )
        .mount(&server);
    let c = test_client(&server);
    assert!(matches!(
        c.send_raw(&GetRegion).err(),
        Some(Error::Decode { .. })
    ));
}

/// JSON 이 아니면 `Error::Decode` 다 — `send` 와 같은 실패 모양.
#[test]
fn send_raw_refuses_a_body_that_is_not_json() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(b"{not json".to_vec(), "application/json"),
        )
        .mount(&server);
    let c = test_client(&server);
    assert!(matches!(
        c.send_raw(&GetRegion).err(),
        Some(Error::Decode { .. })
    ));
}

/// **재시도하지 않는다.** 기본 `max_attempts = 1` 이 곧 "재시도 없음"이다.
///
/// **이 기본값의 근거를 "중복 과금 방지"로 적지 마라 — 두 번 틀렸다.**
/// 첫 판은 408 을 지목했고, 둘째 판은 같은 주장을 *재시도 가능한 5xx* 로
/// 옮겼다. 둘 다 거짓이다: [`Error::is_retryable`] 이 admit 하는 것은
/// `TransportNotSent` 와 `500|502|503|504` 뿐이고, 유료 엔드포인트에서
/// [`Error::billing`] 은 **그 전부를** [`crate::Billing::NotBilled`] 로
/// 판정한다(`RECOVERY_BUDGET` doc 의 PROBE-E: 유료 503 + `.max_attempts(3)`
/// → 3 요청, `billing == NotBilled`). 408 은 과금되지만 애초에 재시도 대상이
/// 아니다. 기본값이 1 인 진짜 이유는 **§9.2 의 판정이 Tesla 와의 계약이 아니라
/// 이 크레이트의 모델**이라는 잔여 불확실성이다.
///
/// 그래서 이 테스트는 408 이 아니라 **재시도 가능**인 503 을 써서 "안 보낸다"가
/// 우연이 아님을 보인다 — `default_does_not_retry_even_on_retryable_5xx` 가
/// `send` 에 대해 하는 것과 같은 형태다.
///
/// **`GetRegion` + [`Client::dispatches`] 를 쓰는 이유.** `GetRegion` 은 리전
/// 해석 자체이므로 선해석이 돌지 않고(`req_resolves_region`), 이 호출이 내는
/// 요청은 하나뿐이라 셈이 모호하지 않다. 그리고 세는 대상은 **클라이언트가
/// 내보낸 수**이지 서버가 받은 수가 아니다 — 그 필드의 doc 이 왜 그 경계를
/// 골랐는지, 그리고 T20 이 여기서 무엇을 잘못 진단했는지를 적어 두고 있다.
#[test]
fn send_raw_does_not_retry_by_default() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503).set_body_raw(b"{}".to_vec(), "application/json"))
        .mount(&server);

    let c = test_client(&server); // 기본 빌더 = max_attempts 1
    let e = c
        .send_raw(&GetRegion)
        .err()
        .expect("503 이 Value 로 반환됐다");
    assert!(
        e.is_retryable(),
        "503 은 재시도 **가능**하다고 판정돼야 한다 — 아니면 이 테스트는 공허하다"
    );
    assert_eq!(
        c.dispatches(),
        1,
        "판정이 가능이어도 기본값에서는 두 번째를 내보내지 않는다 — 유료 5xx 는 \
         §9.2 가 NotBilled 로 판정하므로 이 기본값이 막는 것은 '중복 과금'이 아니라 \
         '그 판정이 틀렸을 경우'다"
    );
}

/// scp 클레임을 가진 가짜 JWT 를 만든다(서명은 아무 값 — 검증하지 않으므로).
fn fake_jwt(scopes: &[&str]) -> String {
    use base64::Engine as _;
    let b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let payload = serde_json::json!({ "scp": scopes, "exp": 9_999_999_999u64 });
    format!(
        "{}.{}.sig",
        b64.encode(br#"{"alg":"RS256","typ":"JWT"}"#),
        b64.encode(serde_json::to_vec(&payload).unwrap()),
    )
}

/// 회전할 때마다 **다른** 토큰을 돌려주는 refresher + 호출 카운터.
///
/// 매번 다른 값인 것이 핵심이다(final fix wave / Important 1). 401 복구는
/// "회전이 일어났는가" 가 아니라 "재시도가 **다른 바이트**를 실어 보내는가" 로
/// 판정해야 하는데, 같은 값을 돌려주는 refresher 로는 고친 구현과 망가진
/// 구현(거절당한 토큰 재전송)이 전선 위에서 구별되지 않는다.
///
/// 값은 seed 토큰 뒤에 `-rot{n}` 을 붙여 만든다. JWT 의 **서명 세그먼트만**
/// 달라지므로 `scp` 클레임(따라서 스코프 선검사 결과)은 그대로다 — 이 픽스처를
/// 쓰는 스코프 테스트들이 회전 여부에 영향받지 않는다.
struct CountingRefresher {
    token: String,
    calls: Arc<AtomicUsize>,
}

impl TokenRefresher for CountingRefresher {
    fn refresh(&self, _refresh_token: &str) -> Result<Refreshed, Error> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(Refreshed::new(
            format!("{}-rot{n}", self.token),
            "refresh-token".into(),
            28_800,
        ))
    }
}

/// [`Client`] 와 **그 토큰 디렉터리 가드를 함께** 들고 다니는 픽스처.
///
/// 이 묶음이 존재하는 이유가 결함 하나다. 헬퍼 안에서 `TempDir` 를 지역
/// 변수로만 잡으면 헬퍼가 반환하는 순간 가드가 drop 되어 디렉터리가 지워지고,
/// 그것을 읽는 `Client` 는 뒤이어 `token store: token file not found` 로 죽는다
/// (실제로 이 모듈 51개 테스트가 그렇게 죽었다). 가드의 수명은 그것을 쓰는
/// `Client` 의 수명 **이상**이어야 하고, 그 제약을 타입으로 못박은 것이 이 구조체다.
///
/// `Deref<Target = Client>` 이므로 호출부는 `c.send(..)` 처럼 그대로 쓴다.
struct TestClient {
    client: Client,
    _dir: crate::test_support::TempDir,
}

impl std::ops::Deref for TestClient {
    type Target = Client;
    fn deref(&self) -> &Client {
        &self.client
    }
}

/// mock 서버를 부트스트랩 리전으로 삼은 빌더 + 토큰 디렉터리 가드.
///
/// 가드를 `_` 로 버리지 말 것 — 빌더가 만들 `Client` 가 읽어야 할 디렉터리가
/// 그 자리에서 사라진다. 이름 있는 바인딩(`let (b, _dir) = …`)으로 받아 테스트
/// 본문 끝까지 살려 둘 것.
fn test_client_builder(server: &MockServer) -> (ClientBuilder, crate::test_support::TempDir) {
    let (b, _, dir) = test_client_builder_with(
        server,
        Some(&["vehicle_device_data", "vehicle_location", "openid"]),
    );
    (b, dir)
}

fn test_client_builder_with(
    server: &MockServer,
    scopes: Option<&[&str]>,
) -> (
    ClientBuilder,
    Arc<AtomicUsize>,
    crate::test_support::TempDir,
) {
    let token = match scopes {
        Some(s) => fake_jwt(s),
        None => "opaque-not-a-jwt".to_owned(),
    };
    let calls = Arc::new(AtomicUsize::new(0));
    // **유효한** 토큰을 심는다 — 값은 `fake_jwt(scopes)`(또는 opaque 토큰)
    // 그대로이므로 이후 요청에 실리는 access token 과 스코프 선검사 입력이
    // 예전 판과 동일하다. `TokenManager` 는 온디스크 파일이 아예 없으면 첫
    // 호출이 `NotFound` 로 실패하므로(부트스트랩 로그인은 이 크레이트의 몫이
    // 아니다) 심는 것 자체는 그대로 필요하다.
    //
    // **왜 만료 토큰에서 유효 토큰으로 바꿨나** (final fix wave /
    // Important 1). 예전 판은 만료 토큰을 심어 첫 호출이 refresh POST 를
    // 태우게 했다. 그 부트스트랩 회전이 `last_refresh_at` 을 "방금" 으로
    // 찍어 버리므로, 401 복구가 강제하는 회전이 언제나
    // `MIN_REFRESH_INTERVAL`(60초)에 막힌다 — 즉 **고친 401 복구를 시험할 수
    // 없는** 픽스처다(고치기 전에는 복구가 디스크 채택으로 "성공" 해 버려서
    // 이 결함이 드러나지 않았다). 상용에서도 8시간짜리 토큰이 수명 도중
    // 401 을 맞는 것이 정상 시나리오이지 "회전 직후 401" 이 아니다. 유효
    // 토큰을 심으면 첫 호출은 **채택**(POST 0회, `last_refresh_at` 미설정)이
    // 되어 그 정상 시나리오를 그대로 모형화한다. 회전 억제 게이트 자체는
    // `auth` 계층 테스트가 전담한다.
    let dir = crate::test_support::temp_dir("client");
    crate::test_support::write_token(&dir, &token, "NA_SEED", crate::test_support::FAR_FUTURE);
    let tokens = TokenManager::new(
        dir.to_path_buf(),
        Arc::new(CountingRefresher {
            token,
            calls: Arc::clone(&calls),
        }),
    );
    let url: crate::Url = server.uri().parse().unwrap();
    let host = url.host_str().unwrap().to_owned();
    // T7: mock 서버 호스트를 allowlist 에 넣는다. `allow_region_host` 만으로는
    // 부족하다 — wiremock 은 임의 포트의 평문 HTTP 라 https·기본 포트 검사에
    // 걸린다. 그래서 origin 해치(`#[cfg(test)]`)를 함께 쓴다. 두 줄 모두
    // **테스트 한정**이고, `allow_region_host` 의 기본값은 닫혀 있다
    // (`TESLA_REGION_HOSTS` 만) — "편하니까" 그 기본값을 뒤집지 말 것.
    // 실패 모드가 자격증명 유출이다.
    let b = Client::builder(tokens)
        .bootstrap_region(url.clone())
        .allow_region_host(&host)
        .allow_region_origin_for_tests(&url)
        .request_timeout(std::time::Duration::from_secs(5));
    (b, calls, dir)
}

/// origin 해치 **없이** 부트스트랩만 mock 서버로 잡은 빌더 — 프로덕션
/// 신뢰경계(`validate_region_url`)를 그대로 통과해야 하는 채택/거부 테스트용.
fn strict_test_client(server: &MockServer) -> TestClient {
    let dir = crate::test_support::temp_dir("client-strict");
    crate::test_support::seed_expired_token(&dir);
    let tokens = TokenManager::new(
        dir.to_path_buf(),
        Arc::new(CountingRefresher {
            token: fake_jwt(&["openid"]),
            calls: Arc::new(AtomicUsize::new(0)),
        }),
    );
    let client = Client::builder(tokens)
        .bootstrap_region(server.uri().parse().unwrap())
        .request_timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    TestClient { client, _dir: dir }
}

fn test_client(server: &MockServer) -> TestClient {
    let (b, dir) = test_client_builder(server);
    TestClient {
        client: b.build().unwrap(),
        _dir: dir,
    }
}

/// 401 복구가 refresh 를 **몇 번** 태웠는지 세는 클라이언트.
fn test_client_counting_refreshes(server: &MockServer) -> (TestClient, Arc<AtomicUsize>) {
    let (b, calls, dir) = test_client_builder_with(
        server,
        Some(&["vehicle_device_data", "vehicle_location", "openid"]),
    );
    (
        TestClient {
            client: b.build().unwrap(),
            _dir: dir,
        },
        calls,
    )
}

/// T9 의 스코프 선검사(`observe_scopes`) 테스트가 쓴다 — 임의 스코프 집합을
/// 실은 JWT 를 발급하는 클라이언트.
fn test_client_with_scopes(server: &MockServer, scopes: &[&str]) -> TestClient {
    let (b, _, dir) = test_client_builder_with(server, Some(scopes));
    TestClient {
        client: b.build().unwrap(),
        _dir: dir,
    }
}

/// T9 의 opaque(비-JWT) 토큰 fail-open 테스트가 쓴다.
fn test_client_with_opaque_token(server: &MockServer) -> TestClient {
    let (b, _, dir) = test_client_builder_with(server, None);
    TestClient {
        client: b.build().unwrap(),
        _dir: dir,
    }
}

/// 부트스트랩이 **회전**으로 일어나는 클라이언트 — 즉 `last_refresh_at` 이
/// "방금" 으로 찍혀, 이후의 401 복구 회전이 `MIN_REFRESH_INTERVAL` 에 막히는
/// 구성이다.
///
/// 기본 픽스처(`test_client_builder_with`)는 유효 토큰을 심어 이 상황을 일부러
/// 피한다(그쪽 주석 참조). 회전 억제와 401 이 겹치는 경로를 시험하는
/// 테스트만 이 구성을 쓴다.
fn client_bootstrapped_by_a_rotation(server: &MockServer) -> TestClient {
    let dir = crate::test_support::temp_dir("client-must-rotate");
    crate::test_support::seed_expired_token(&dir);
    let tokens = TokenManager::new(
        dir.to_path_buf(),
        Arc::new(CountingRefresher {
            token: fake_jwt(&["vehicle_device_data", "vehicle_location", "openid"]),
            calls: Arc::new(AtomicUsize::new(0)),
        }),
    );
    let url: crate::Url = server.uri().parse().unwrap();
    let host = url.host_str().unwrap().to_owned();
    let client = Client::builder(tokens)
        .bootstrap_region(url.clone())
        .allow_region_host(&host)
        .allow_region_origin_for_tests(&url)
        .request_timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    TestClient { client, _dir: dir }
}

/// `GetRegion` 과 **모든 면에서 같지만 유료**인 테스트 전용 엔드포인트.
///
/// 이 크레이트가 오늘 노출하는 GET 3종은 전부 `Pricing::NotCharged` 라,
/// `billing()` 을 단언하는 테스트가 전부 공허해진다(`NotCharged` 는 어떤
/// 구현에서도 `NotBilled`). A4 의 `vehicle_data` 를 기다리는 대신 여기서
/// 유료 엔드포인트를 하나 세운다 — `FleetGet` 은 sealed 이므로 크레이트 안에서만
/// 가능하고, 경로를 `GetRegion` 과 같게 두어 사전 리전 해석이 끼어들지 않게
/// 한다(`req_resolves_region` 은 경로로 판정한다).
struct BilledLikeRegion;
impl crate::sealed::Sealed for BilledLikeRegion {}
impl FleetGet for BilledLikeRegion {
    passthrough_endpoint!(BilledLikeRegion, RegionInfo);
    fn required_scopes(&self) -> crate::scope::ScopeExpr {
        crate::scope::ScopeExpr::One(crate::scope::Scope::VehicleDeviceData)
    }
    fn pricing(&self) -> Pricing {
        Pricing::data()
    }
    fn path(&self) -> String {
        GetRegion.path()
    }
    fn query(&self) -> QuerySpec {
        QuerySpec::new()
    }
}

/// 다른 요청이 이미 세대를 올려 둔 상태를 만든다(§5.1 동시 401 시나리오).
fn bump_generation_out_of_band(c: &Client) {
    let (g, _) = c.tokens().access_token_with_generation().unwrap();
    c.tokens().invalidate_generation(g).unwrap();
    let _ = c.tokens().access_token_with_generation().unwrap();
}

fn default_allowlist() -> Vec<String> {
    TESLA_REGION_HOSTS
        .iter()
        .map(|s| (*s).to_string())
        .collect()
}

// --- 신뢰경계 ------------------------------------------------------

/// 악성 리전 URL 을 전부 거부한다. **하나라도 통과하면 자격증명 유출이다.**
///
/// brief 의 6종에 3종을 더했다 — brief 목록만으로는 "접미 매칭으로 바꾸는"
/// 뮤테이션(M2)이 **살아남는다**:
/// - `tesla.com.evil.net` 은 `ends_with("tesla.com")` 로도 거부되고,
///   `ends_with(<allowlist 항목>)` 로도 거부된다 → 어느 M2 변종도 못 죽인다.
/// - `evil-fleet-api.…tesla.com` 은 allowlist 항목을 **접미로** 포함하므로
///   `ends_with(a)` 변종을 죽인다.
/// - `evil.tesla.com` · `nottesla.com` 은 `ends_with("tesla.com")` 변종을
///   죽인다(뒤엣것은 점 경계조차 없다).
///
/// 규율 1조: X 를 금지하는 규칙엔 X 를 했을 때만 실패하는 입력이 필요하다.
#[test]
fn rejects_every_untrusted_region_url() {
    let allowed = default_allowlist();
    for bad in [
        "http://fleet-api.prd.na.vn.cloud.tesla.com", // 평문
        "https://evil.example",                       // 외부 호스트
        "https://fleet-api.prd.na.vn.cloud.tesla.com.evil.net", // 접미 추가
        "https://evil-fleet-api.prd.na.vn.cloud.tesla.com", // 접두 추가(ends_with 우회)
        "https://evil.tesla.com",                     // *.tesla.com 우회
        "https://nottesla.com",                       // 점 경계 없는 tesla.com 접미
        "https://user:pw@fleet-api.prd.na.vn.cloud.tesla.com", // userinfo
        "https://fleet-api.prd.na.vn.cloud.tesla.com:8443", // 비표준 포트
        "https://fleet-api.prd.na.vn.cloud.tesla.com/evil/path", // path 주입
        "https://fleet-api.prd.na.vn.cloud.tesla.com/?x=1", // query
        "https://fleet-api.prd.na.vn.cloud.tesla.com/#f", // fragment
        "not a url",                                  // 파싱 불가
        "",                                           // 빈 문자열
    ] {
        assert!(
            validate_region_url(bad, &allowed).is_err(),
            "통과하면 안 됨: {bad}"
        );
    }
}

/// Design invariant I3's first half — a client that never calls
/// [`ClientBuilder::bootstrap_region`] starts at the **NA** host.
///
/// A5 Task 1's reachability audit found [`NA_BOOTSTRAP`] entirely
/// unpinned: rewriting the constant to the EU host left all 366 tests
/// green, because every client test in this module supplies its own
/// bootstrap (a mock server) and so never exercises the default. The
/// escaping failure is quiet — an NA account would send its very first
/// `GetRegion` to Europe with a live Bearer token attached, which is the
/// wrong-region 412 that [`region_refusal`] exists to explain, arrived at
/// by a route no test watches.
///
/// Asserted through the built [`Client`] rather than against the constant
/// alone, so `unwrap_or_else`'s default stopping being applied fails this
/// as well as a changed constant would.
#[test]
fn the_default_bootstrap_host_is_na_when_the_builder_is_not_told_otherwise() {
    let dir = crate::test_support::temp_dir("client-default-bootstrap");
    crate::test_support::seed_expired_token(&dir);
    let tokens = TokenManager::new(
        dir.to_path_buf(),
        Arc::new(CountingRefresher {
            token: fake_jwt(&["openid"]),
            calls: Arc::new(AtomicUsize::new(0)),
        }),
    );
    let c = Client::builder(tokens).build().unwrap();
    let (_gen, base) = c.current_base();
    assert_eq!(
        base.host_str(),
        Some("fleet-api.prd.na.vn.cloud.tesla.com"),
        "the untouched default bootstrap must be the NA host"
    );
    assert_eq!(base.scheme(), "https");
    assert_ne!(base.host_str(), Some("fleet-api.prd.eu.vn.cloud.tesla.com"));
}

#[test]
fn accepts_legitimate_region_urls() {
    let allowed = default_allowlist();
    for ok in [
        "https://fleet-api.prd.na.vn.cloud.tesla.com",
        "https://fleet-api.prd.na.vn.cloud.tesla.com/",
        "https://fleet-api.prd.eu.vn.cloud.tesla.com",
        // 대소문자는 DNS 상 같은 호스트다 — `Url` 이 소문자화하므로 통과해야
        // 한다. 거부하면 정상 응답을 오탐으로 막는다.
        "https://FLEET-API.PRD.EU.VN.CLOUD.TESLA.COM",
        // 명시적 :443 은 스킴 기본 포트라 `port()` 가 None 이다.
        "https://fleet-api.prd.na.vn.cloud.tesla.com:443/",
    ] {
        assert!(
            validate_region_url(ok, &allowed).is_ok(),
            "거부하면 안 됨: {ok}"
        );
    }
}

/// 중국 리전은 기본 allowlist 에 **없다**(design O11 — 증거 없음).
/// 필요한 사용자는 `allow_region_host` 로 연다. 기본값이 열려 있는지
/// 닫혀 있는지를 값으로 고정한다 — 편의를 이유로 뒤집히면 실패한다.
#[test]
fn china_region_host_is_closed_by_default_and_opt_in_only() {
    const CN: &str = "https://fleet-api.prd.cn.vn.cloud.tesla.cn";
    let server = MockServer::start();

    assert!(
        validate_region_url(CN, &default_allowlist()).is_err(),
        "🔴 증거 없는 호스트가 기본 allowlist 에 들어 있다"
    );

    let (b, _dir) = test_client_builder(&server);
    let opened = b
        .allow_region_host("fleet-api.prd.cn.vn.cloud.tesla.cn")
        .build()
        .unwrap();
    assert!(opened.adopt_region_url(CN).is_ok());

    // 열어도 **호스트만** 열린다 — 평문 다운그레이드는 여전히 막힌다.
    assert!(
        opened
            .adopt_region_url("http://fleet-api.prd.cn.vn.cloud.tesla.cn")
            .is_err(),
        "🔴 allow_region_host 가 https 요구까지 풀어 버렸다"
    );
}

/// 프로덕션 경로 그대로: 검증을 통과한 리전 URL 은 채택되고 base 가 바뀐다.
/// (테스트용 origin 해치를 쓰지 **않는** 클라이언트다 — 실제
/// `TESLA_REGION_HOSTS` 항목으로만 통과시킨다. 채택된 호스트로 요청을
/// 보내지는 않으므로 네트워크에 나가지 않는다.)
#[test]
fn adopts_a_valid_region_url_and_switches_base() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/users/region"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            br#"{"response":{"region":"eu","fleet_api_base_url":"https://fleet-api.prd.eu.vn.cloud.tesla.com"}}"#.to_vec(),
            "application/json",
        ))
        .mount(&server);

    let c = strict_test_client(&server);
    let g0 = c.region_generation();
    let (_, before) = c.current_base();
    assert_eq!(before.host_str(), Some("127.0.0.1"), "처음엔 부트스트랩");

    let got = c.resolve_region().unwrap();
    assert_eq!(got.as_str(), "https://fleet-api.prd.eu.vn.cloud.tesla.com/");
    let (g1, after) = c.current_base();
    assert_eq!(
        after.host_str(),
        Some("fleet-api.prd.eu.vn.cloud.tesla.com"),
        "채택 후 base 가 바뀌어야 한다"
    );
    assert!(g1 > g0, "채택은 세대를 올린다");

    // 두 번째 호출은 캐시를 그대로 쓴다 — 재해석 요청이 더 나가지 않는다.
    c.resolve_region().unwrap();
    let region_calls = server
        .received_requests()
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/api/1/users/region")
        .count();
    assert_eq!(region_calls, 1, "이미 해석됐으면 다시 묻지 않는다");
}

/// 거부된 리전 URL 은 **채택되지 않고** 부트스트랩이 유지된다.
/// "에러가 났다" 가 아니라 base 가 안 바뀌었다는 것을 확인한다.
#[test]
fn refuses_an_untrusted_region_url_and_keeps_bootstrap() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/users/region"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            br#"{"response":{"region":"na","fleet_api_base_url":"https://evil.example"}}"#.to_vec(),
            "application/json",
        ))
        .mount(&server);

    let c = strict_test_client(&server);
    let g0 = c.region_generation();
    let e = c.resolve_region().unwrap_err();
    assert!(matches!(e, Error::Auth { .. }), "실제: {e:?}");

    let (g1, base) = c.current_base();
    assert_eq!(base.host_str(), Some("127.0.0.1"), "🔴 base 가 바뀌었다");
    assert_eq!(
        g1, g0,
        "거부는 세대를 올리지 않는다(실패를 캐시하지 않는다)"
    );
}

/// 리전 응답에 `fleet_api_base_url` 자체가 없으면 채택할 것이 없다.
#[test]
fn missing_fleet_api_base_url_is_not_adopted() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/users/region"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            br#"{"response":{"region":"na"}}"#.to_vec(),
            "application/json",
        ))
        .mount(&server);

    let c = strict_test_client(&server);
    assert!(c.resolve_region().is_err());
    assert_eq!(c.region_generation(), 0);
}

// --- 412 복구 ---------------------------------------------------------

/// fix round 1 (review Important 1): **부트스트랩이 아닌 리전의 계정은
/// 첫 호출에서 412 를 태우지 않는다.**
///
/// 두 개의 mock 서버로 기전을 직접 잰다 — A = 부트스트랩(리전 해석만
/// 답하고 `/vehicles` 는 412 로 거절), B = 실제 리전(200). 사전 해석이
/// 있으면 `/vehicles` 는 처음부터 B 로 가고 **A 는 `/vehicles` 요청을 단
/// 한 번도 받지 않는다**. 사전 해석을 빼면 A 가 412 를 한 번 받게 되므로
/// 이 단언이 죽는다(변이 M14).
///
/// A3 에서 그 412 는 $0 지만, design §4.3 대로 A4 가 `vehicle_data` 를
/// 들여오는 순간 **과금된다** — 게다가 무과금 리전 엔드포인트가 아니라
/// 호출자가 부른 그 엔드포인트에 떨어진다.
#[test]
fn resolves_region_before_the_first_request_so_no_412_is_burned() {
    let region_server = MockServer::start(); // B: 진짜 리전
    Mock::given(method("GET"))
        .and(path("/api/1/vehicles"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(br#"{"response":[]}"#.to_vec(), "application/json"),
        )
        .mount(&region_server);

    let boot = MockServer::start(); // A: 부트스트랩(틀린 리전)
    Mock::given(method("GET"))
        .and(path("/api/1/users/region"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"response":{{"region":"eu","fleet_api_base_url":"{}"}}}}"#,
                    region_server.uri()
                )
                .into_bytes(),
                "application/json",
            ),
        )
        .mount(&boot);
    Mock::given(method("GET"))
        .and(path("/api/1/vehicles"))
        .respond_with(ResponseTemplate::new(412).set_body_raw(
            br#"{"error":"must be registered in the current region"}"#.to_vec(),
            "application/json",
        ))
        .mount(&boot);

    let region_url: crate::Url = region_server.uri().parse().unwrap();
    let (b, _dir) = test_client_builder(&boot);
    let c = b
        .allow_region_origin_for_tests(&region_url)
        .build()
        .unwrap();

    let got: Page<crate::model::VehicleSummary> = c
        .send(&crate::api::vehicles::ListVehicles {
            page: None,
            per_page: None,
        })
        .unwrap();
    assert!(got.items.is_empty());

    let burned = boot
        .received_requests()
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/api/1/vehicles")
        .count();
    assert_eq!(
        burned, 0,
        "🔴 부트스트랩 호스트에 실 요청이 나갔다 — 사전 해석이 없으면 이 \
         412 가 A4 에서 과금된다"
    );
    let served = region_server
        .received_requests()
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/api/1/vehicles")
        .count();
    assert_eq!(served, 1, "실 요청은 해석된 리전으로 정확히 1회");
}

/// 412 → 리전 재해석 1회 → 재전송. **기전을 센다**(요청 수).
#[test]
fn region_412_triggers_one_reresolve_and_retry() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/vehicles"))
        .respond_with(ResponseTemplate::new(412).set_body_raw(
            br#"{"error":"must be registered in the current region"}"#.to_vec(),
            "application/json",
        ))
        .up_to_n_times(1)
        .mount(&server);
    Mock::given(method("GET"))
        .and(path("/api/1/vehicles"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(br#"{"response":[]}"#.to_vec(), "application/json"),
        )
        .mount(&server);
    Mock::given(method("GET"))
        .and(path("/api/1/users/region"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"response":{{"region":"na","fleet_api_base_url":"{}"}}}}"#,
                    server.uri()
                )
                .into_bytes(),
                "application/json",
            ),
        )
        .mount(&server);

    let c = test_client(&server);
    let got: Page<crate::model::VehicleSummary> = c
        .send(&crate::api::vehicles::ListVehicles {
            page: None,
            per_page: None,
        })
        .unwrap();
    assert!(got.items.is_empty());

    let reqs = server.received_requests().unwrap();
    let vehicles = reqs
        .iter()
        .filter(|r| r.url.path() == "/api/1/vehicles")
        .count();
    let region = reqs
        .iter()
        .filter(|r| r.url.path() == "/api/1/users/region")
        .count();
    assert_eq!(vehicles, 2, "원요청 + 412 재시도 = 2회여야 한다");
    // fix round 1: 사전 해석 1회 + 412 복구 재해석 1회 = 2회. 이것이 design
    // §7 이 잡아 둔 예산(최대 2회) 그대로이고, 사전 해석이 있어야만 도달한다.
    assert_eq!(region, 2, "사전 해석 1 + 412 재해석 1 = 정확히 2회");
    assert_eq!(
        reqs[0].url.path(),
        "/api/1/users/region",
        "리전 해석이 첫 요청보다 **먼저** 나가야 한다"
    );
}

/// `vehicle_data` 의 mock 경로 — 유료 복구 금지 테스트 두 짝이 함께 쓴다.
const PAID_VIN: &str = "5YJ3E1EA7JF000001";

fn paid_req() -> crate::api::vehicles::GetVehicleData {
    crate::api::vehicles::GetVehicleData::new(
        PAID_VIN,
        vec![crate::api::vehicles::DataEndpoint::ChargeState],
    )
    .expect("synthetic VIN is path-safe")
}

/// 해석 **가능한** 리전을 내는 mock — 유료/무료 짝 테스트의 공통 전제.
///
/// 이것이 없으면 유료 테스트가 **우연히** 통과한다. `resolve_region` 이
/// 실패하면 412 arm 의 `if …is_ok()` 가 false 라 재발사 자체가 일어나지 않기
/// 때문이다 — 즉 "유료라서 안 나갔다" 와 "리전을 못 풀어서 못 나갔다" 가
/// 구별되지 않는다. A5c-2a T23 의 첫 판 하네스가 정확히 그 상태였고, 리뷰가
/// 그것을 잡았다. 여기서는 리전이 **풀리는** 상태를 만들어 재발사 조건을
/// 실제로 성립시킨 뒤 유료만 막히는지를 본다.
fn mount_resolvable_region(server: &MockServer) {
    Mock::given(method("GET"))
        .and(path("/api/1/users/region"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"response":{{"region":"na","fleet_api_base_url":"{}"}}}}"#,
                    server.uri()
                )
                .into_bytes(),
                "application/json",
            ),
        )
        .mount(server);
}

/// **유료 412: 재발사는 없고, 복구는 일어난다** (T23 fix round 2).
///
/// # 이 테스트가 고정하는 두 가지 — 둘 다 있어야 한다
///
/// 1. **`send` 한 번에 유료 요청은 한 번.** 412 는 첫 요청이 서버에 도달해 답을
///    받았다는 증거이고(`Error::billing` → `Billed`), 재발사는 곧 두 번째 청구다.
/// 2. **그럼에도 자가 치유는 된다.** 첫 판(fix round 1)은 `may_recover` 자체를
///    `false` 로 만들어 `invalidate_region`/`resolve_region` 까지 건너뛰었고, 그래서
///    **유료만 부르는 소비자는 영원히 못 낫는다** — 매 호출이 같은 stale 리전으로
///    412 를 맞고 그 하나하나가 `Billed` 다. 중복 과금을 막으려다 무한 과금을 만든
///    셈이었다.
///
/// # 왜 mock 서버가 **둘**인가 — 첫 판의 픽스처는 공허했다
///
/// 한 서버에 `412`(`up_to_n_times(1)`) + `200` 을 얹으면 두 번째 `send` 는 **복구와
/// 무관하게** 200 을 받는다. mock 의 412 가 소진됐기 때문이다 — 즉 "복구가 됐다" 와
/// "mock 이 지쳤다" 가 구별되지 않는다. 실측으로 확인했다(fix round 2 작성 중):
/// 억제판에서도 `send2_ok=true` 였다.
///
/// 그래서 리전 전환을 **실제로** 만든다. `A` 는 언제나 412 를 내고 `B` 는 200 을 낸다.
/// `A` 의 리전 응답은 **첫 번째는 A, 그 다음부터는 B** 를 가리킨다. 복구가 일어나야만
/// 두 번째 `send` 의 base 가 `B` 로 바뀌므로, `send #2` 의 성공이 곧 복구의 증거다.
///
/// 무료 짝 `region_412_triggers_one_reresolve_and_retry` 는 한 번의 `send` 안에서
/// `vehicles == 2` 다. 두 테스트를 나란히 두는 것이 pricing 축의 대조군이다.
#[test]
fn a_412_on_a_paid_endpoint_is_not_re_fired() {
    let a = MockServer::start();
    let b = MockServer::start();
    let paid_path = format!("/api/1/vehicles/{PAID_VIN}/vehicle_data");

    // A: 유료는 **언제나** 412.
    Mock::given(method("GET"))
        .and(path(paid_path.clone()))
        .respond_with(ResponseTemplate::new(412).set_body_raw(
            br#"{"error":"must be registered in the current region"}"#.to_vec(),
            "application/json",
        ))
        .mount(&a);
    // A 의 리전 응답: 1회차는 A 자신, 이후는 B.
    let region_body = |uri: &str| {
        ResponseTemplate::new(200).set_body_raw(
            format!(r#"{{"response":{{"region":"na","fleet_api_base_url":"{uri}"}}}}"#)
                .into_bytes(),
            "application/json",
        )
    };
    Mock::given(method("GET"))
        .and(path("/api/1/users/region"))
        .respond_with(region_body(&a.uri()))
        .up_to_n_times(1)
        .mount(&a);
    Mock::given(method("GET"))
        .and(path("/api/1/users/region"))
        .respond_with(region_body(&b.uri()))
        .mount(&a);
    // B: 유료가 성공한다.
    Mock::given(method("GET"))
        .and(path(paid_path.clone()))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(br#"{"response":{}}"#.to_vec(), "application/json"),
        )
        .mount(&b);

    let b_url: crate::Url = b.uri().parse().unwrap();
    // 이 테스트는 mock 서버를 `b` 라는 이름으로 이미 쓰고 있다 — 빌더를 `b` 로
    // 받으면 그것을 가려 버린다.
    let (builder, _dir) = test_client_builder(&a);
    let c = builder
        .allow_region_host(b_url.host_str().unwrap())
        .allow_region_origin_for_tests(&b_url)
        .build()
        .unwrap();
    let count = |reqs: &[fleetwire_testkit::Request], p: &str| {
        reqs.iter().filter(|r| r.url.path() == p).count()
    };

    // ── send #1 — A 가 412. 재발사 없음. ─────────────────────────────────
    // `unwrap_err()` 를 쓸 수 없다 — `VehicleData` 는 의도적으로 `Debug` 가
    //    아니다(PII 가 패닉 메시지로 새는 것을 타입으로 막는다).
    let e = match c.send(&paid_req()) {
        Ok(_) => panic!("send #1 must fail: A always answers 412"),
        Err(e) => e,
    };
    assert!(
        matches!(&e, Error::Api { status, .. } if status.as_u16() == 412),
        "412 가 그대로 올라와야 한다: {e:?}"
    );
    assert_eq!(
        e.billing(),
        crate::Billing::Billed,
        "412 는 유료에서 청구된 것으로 판정된다 — 재발사가 곧 중복 청구인 이유"
    );
    assert_eq!(
        count(&a.received_requests().unwrap(), &paid_path),
        1,
        "🔴 send #1 에서 유료 요청이 두 번 나갔다 = 중복 과금"
    );

    // ── send #2 — 복구가 일어났으므로 base 가 B 로 바뀌어 성공해야 한다. ──
    assert!(
        c.send(&paid_req()).is_ok(),
        "🔴🔴 유료만 부르는 소비자가 자가 치유하지 못한다. 412 arm 이 \
         invalidate_region/resolve_region 을 건너뛰면 base 가 영원히 A 에 머물러 \
         매 호출이 412 를 맞고 그 하나하나가 Billed 다 — 중복 과금을 막으려다 \
         무한 과금이 된다"
    );

    let ra = a.received_requests().unwrap();
    let rb = b.received_requests().unwrap();
    assert_eq!(
        (count(&ra, &paid_path), count(&rb, &paid_path)),
        (1, 1),
        "A 에 1건(412) + B 에 1건(200) — 즉 **send 당 정확히 1건**, 재발사 0"
    );
    // 비-vacuity: 리전이 **실제로** 재해석됐다(사전 1 + 412 복구 1).
    //    1 이면 "리전을 못 풀어서 재발사가 없었다" 와 구별되지 않는다.
    assert_eq!(
        count(&ra, "/api/1/users/region"),
        2,
        "사전 해석 1 + 412 복구 재해석 1. 복구 자체는 유료에서도 일어나야 한다"
    );
}

/// **유료 401: 재발사는 없고, 회전은 일어난다** — 412 짝과 같은 논증.
///
/// 401 은 `Error::Auth { pricing: Some(Data) }` 이고 설계 §9.2 가 그것을 청구된
/// 것으로 판정한다. 그러나 `reject_generation` 은 **그대로 부른다** — 그것이 다음
/// 호출에 *다른 바이트*를 실어 보내는 유일한 수단이고, 그것까지 건너뛰면 유료
/// 소비자는 같은 stale 토큰으로 401 을 무한히 사게 된다(fix round 2 의 발단).
///
/// # 401 응답을 **토큰 값으로** 가른다 — 412 짝과 같은 비-vacuity 논증
///
/// `up_to_n_times(1)` 로 401 을 한 번만 내면 두 번째 `send` 는 회전과 무관하게
/// 200 을 받는다. 그래서 여기서는 **처음 심어 둔 토큰을 실은 요청만** 401 을 받고,
/// 다른 토큰이면 200 이다. 회전이 실제로 일어나야만 `send #2` 가 성공한다.
#[test]
fn a_401_on_a_paid_endpoint_is_not_re_fired() {
    let server = MockServer::start();
    let paid_path = format!("/api/1/vehicles/{PAID_VIN}/vehicle_data");
    mount_resolvable_region(&server);

    let (c, refresh_calls) = test_client_counting_refreshes(&server);
    // 부트스트랩은 디스크 채택이라 회전 0회 — 이 값이 "거절당할 토큰"이다.
    let (_, seed) = c.tokens().access_token_with_generation().unwrap();
    assert_eq!(
        refresh_calls.load(Ordering::SeqCst),
        0,
        "픽스처 전제: 부트스트랩은 채택이라 회전이 없어야 한다"
    );

    // 그 토큰을 실은 요청만 401. 다른 토큰이면 200.
    Mock::given(method("GET"))
        .and(path(paid_path.clone()))
        .and(header("authorization", format!("Bearer {seed}").as_str()))
        .respond_with(ResponseTemplate::new(401).set_body_raw(
            br#"{"error":"invalid authentication"}"#.to_vec(),
            "application/json",
        ))
        .mount(&server);
    Mock::given(method("GET"))
        .and(path(paid_path.clone()))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(br#"{"response":{}}"#.to_vec(), "application/json"),
        )
        .mount(&server);

    let count = |reqs: &[fleetwire_testkit::Request], p: &str| {
        reqs.iter().filter(|r| r.url.path() == p).count()
    };

    // ── send #1 — 401 을 그대로 받는다. 재발사 없음. ──────────────────────
    let e = match c.send(&paid_req()) {
        Ok(_) => panic!("send #1 must fail: the seeded token is refused"),
        Err(e) => e,
    };
    assert!(matches!(e, Error::Auth { .. }), "실제: {e:?}");
    assert_eq!(
        e.billing(),
        crate::Billing::Billed,
        "유료 401 은 설계 §9.2 가 청구로 판정한다"
    );
    assert_eq!(
        count(&server.received_requests().unwrap(), &paid_path),
        1,
        "🔴 send #1 에서 유료 요청이 두 번 나갔다 = 중복 과금"
    );

    // ── send #2 — 세대가 거절됐으므로 새 토큰으로 성공해야 한다. ────────
    assert!(
        c.send(&paid_req()).is_ok(),
        "🔴🔴 유료만 부르는 소비자가 자가 치유하지 못한다. 401 arm 이 \
         reject_generation 을 건너뛰면 매 호출이 같은 stale 토큰으로 401 을 맞고 \
         그 하나하나가 Billed 다"
    );

    let reqs = server.received_requests().unwrap();
    assert_eq!(
        count(&reqs, &paid_path),
        2,
        "두 번의 send 에 유료 요청 2건 — 즉 **send 당 정확히 1건**"
    );
    // 비-vacuity: 회전이 **실제로** 정확히 1회 일어났다.
    assert_eq!(
        refresh_calls.load(Ordering::SeqCst),
        1,
        "401 복구의 강제 회전은 유료에서도 정확히 1회 일어나야 한다"
    );
    // 그리고 전선 위의 토큰이 실제로 달라졌는가 — 회전 카운터만으로는
    // "회전은 했는데 같은 값을 다시 보냈다" 를 배제하지 못한다.
    let bearer = |i: usize| {
        reqs.iter()
            .filter(|r| r.url.path() == paid_path)
            .nth(i)
            .expect("유료 요청")
            .headers
            .get("authorization")
            .expect("Authorization 헤더")
            .to_str()
            .unwrap()
            .to_owned()
    };
    assert_eq!(bearer(0), format!("Bearer {seed}"), "테스트 전제");
    assert_ne!(
        bearer(0),
        bearer(1),
        "🔴 두 번째 요청이 방금 401 을 받은 그 토큰을 그대로 실었다"
    );
}

/// 412 가 계속 나도 무한루프가 아니다 — 요청 수 상한이 있다.
#[test]
fn repeated_412_terminates() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/vehicles"))
        .respond_with(ResponseTemplate::new(412).set_body_raw(
            br#"{"error":"must be registered in the current region"}"#.to_vec(),
            "application/json",
        ))
        .mount(&server);
    Mock::given(method("GET"))
        .and(path("/api/1/users/region"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"response":{{"fleet_api_base_url":"{}"}}}}"#,
                    server.uri()
                )
                .into_bytes(),
                "application/json",
            ),
        )
        .mount(&server);

    let c = test_client(&server);
    let e = c
        .send(&crate::api::vehicles::ListVehicles {
            page: None,
            per_page: None,
        })
        // `.err().unwrap()`: `VehicleSummary` 는 VIN 을 들고 있어 일부러
        // `Debug` 가 아니다 — `unwrap_err()` 는 그 바운드를 요구한다.
        .err()
        .unwrap();
    assert!(
        matches!(&e, Error::Api { status, .. } if status.as_u16() == 412),
        "실제: {e:?}"
    );

    let reqs = server.received_requests().unwrap();
    let vehicles = reqs
        .iter()
        .filter(|r| r.url.path() == "/api/1/vehicles")
        .count();
    assert_eq!(
        vehicles, 2,
        "412 재시도는 1회한 — 무한루프면 이 값이 폭발한다"
    );
}

/// 재해석이 **실패**하면 같은 호스트로 헛된 재전송을 하지 않는다 —
/// 원래의 412 를 그대로 올린다(A4 의 유료 엔드포인트에서 이 한 번이 곧 요금이다).
#[test]
fn failed_reresolution_does_not_retry() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/vehicles"))
        .respond_with(ResponseTemplate::new(412).set_body_raw(
            br#"{"error":"must be registered in the current region"}"#.to_vec(),
            "application/json",
        ))
        .mount(&server);
    // 재해석은 신뢰경계에 걸려 거부된다.
    Mock::given(method("GET"))
        .and(path("/api/1/users/region"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            br#"{"response":{"fleet_api_base_url":"https://evil.example"}}"#.to_vec(),
            "application/json",
        ))
        .mount(&server);

    let c = test_client(&server);
    let e = c
        .send(&crate::api::vehicles::ListVehicles {
            page: None,
            per_page: None,
        })
        // `.err().unwrap()`: `VehicleSummary` 는 VIN 을 들고 있어 일부러
        // `Debug` 가 아니다 — `unwrap_err()` 는 그 바운드를 요구한다.
        .err()
        .unwrap();
    assert!(
        matches!(&e, Error::Api { status, .. } if status.as_u16() == 412),
        "실제: {e:?}"
    );

    let reqs = server.received_requests().unwrap();
    let vehicles = reqs
        .iter()
        .filter(|r| r.url.path() == "/api/1/vehicles")
        .count();
    assert_eq!(vehicles, 1, "채택할 리전이 없으면 재전송하지 않는다");
}

/// fix round 1: 사전 해석이 **실패**해도 리전 해석 총량은 2회를 넘지 않는다.
///
/// 이 조합(사전 해석 실패 → 401 → 412)이 사전 해석을 재시도 루프 **안**에
/// 두는 변이(M15)를 죽이는 유일한 시나리오다: 캐시가 계속 비어 있으므로
/// 매 attempt 마다 사전 해석이 다시 돌고, 거기에 412 복구 해석까지 얹혀
/// §7 의 2회 예산을 넘긴다. 사전 해석이 **성공**하는 다른 테스트들은
/// 두 번째 attempt 에서 캐시가 차 있어 이 변이를 구별하지 못한다.
#[test]
fn region_resolution_stays_within_budget_when_preflight_fails() {
    let server = MockServer::start();
    // 리전 해석은 언제나 신뢰경계에 걸려 거부된다 → 캐시가 계속 비어 있다.
    Mock::given(method("GET"))
        .and(path("/api/1/users/region"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            br#"{"response":{"fleet_api_base_url":"https://evil.example"}}"#.to_vec(),
            "application/json",
        ))
        .mount(&server);
    Mock::given(method("GET"))
        .and(path("/api/1/vehicles"))
        .respond_with(ResponseTemplate::new(401).set_body_raw(
            br#"{"error":"invalid authentication"}"#.to_vec(),
            "application/json",
        ))
        .up_to_n_times(1)
        .mount(&server);
    Mock::given(method("GET"))
        .and(path("/api/1/vehicles"))
        .respond_with(ResponseTemplate::new(412).set_body_raw(
            br#"{"error":"must be registered"}"#.to_vec(),
            "application/json",
        ))
        .mount(&server);

    let c = test_client(&server);
    let _ = c.send(&crate::api::vehicles::ListVehicles {
        page: None,
        per_page: None,
    });

    let reqs = server.received_requests().unwrap();
    let region = reqs
        .iter()
        .filter(|r| r.url.path() == "/api/1/users/region")
        .count();
    let vehicles = reqs
        .iter()
        .filter(|r| r.url.path() == "/api/1/vehicles")
        .count();
    assert_eq!(region, 2, "사전 1 + 412 복구 1 = 2회 상한(§7)");
    assert_eq!(
        vehicles, 2,
        "원요청 + 401 재시도. 412 는 재해석 실패라 재전송 없음"
    );
}

/// 리전 캐시 무효화는 **자기가 본 generation 일 때만** 성립한다.
#[test]
fn stale_generation_cannot_clear_newer_region_cache() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/users/region"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"response":{{"fleet_api_base_url":"{}"}}}}"#,
                    server.uri()
                )
                .into_bytes(),
                "application/json",
            ),
        )
        .mount(&server);

    let c = test_client(&server);
    let g1 = c.region_generation(); // 아직 미해석
    c.resolve_region().unwrap(); // 해석 1회 → generation 상승
    let g2 = c.region_generation();
    assert!(g2 > g1);

    // 뒤늦게 도착한 412 가 **낡은** generation 으로 무효화를 시도한다.
    c.invalidate_region(g1);
    assert_eq!(
        c.region_generation(),
        g2,
        "낡은 세대의 무효화가 먹히면 안 된다"
    );
    // 캐시가 살아 있으므로 재해석 요청이 나가지 않는다.
    c.resolve_region().unwrap();

    let region_calls = server
        .received_requests()
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/api/1/users/region")
        .count();
    assert_eq!(
        region_calls, 1,
        "무효화가 잘못 먹으면 재해석이 한 번 더 일어난다"
    );
}

/// 현재 generation 으로 무효화하면 실제로 지워진다 — 위 테스트가
/// "무효화가 아무 때도 안 먹는다" 로도 통과하는 공허함을 막는다.
#[test]
fn current_generation_does_clear_the_region_cache() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/users/region"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"response":{{"fleet_api_base_url":"{}"}}}}"#,
                    server.uri()
                )
                .into_bytes(),
                "application/json",
            ),
        )
        .mount(&server);

    let c = test_client(&server);
    c.resolve_region().unwrap();
    let g = c.region_generation();
    c.invalidate_region(g);
    c.resolve_region().unwrap();

    let region_calls = server
        .received_requests()
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/api/1/users/region")
        .count();
    assert_eq!(
        region_calls, 2,
        "현재 세대의 무효화는 재해석을 강제해야 한다"
    );
}

// --- 401 복구 ---------------------------------------------------------

/// 401 → 회전 1회 강제 → **다른 토큰**으로 재전송 1회. 2번째 401 은 종료.
///
/// # 이 테스트는 무엇을 대체했는가 (final fix wave / Important 1)
///
/// 예전 판(`auth_401_invalidates_its_own_generation_then_gives_up`)은
/// **망가진 동작을 기대값으로 못박고 있었다**: "refresh 는 정확히 1회 —
/// 401 복구는 막 저장된 디스크 토큰을 **채택**하므로 POST 를 더 태우지
/// 않는다". 그 문장이 참이라는 것은 곧 재시도가 **방금 거절당한 바이트를
/// 그대로 다시 보냈다**는 뜻이다(401 은 `expires_at` 을 움직이지 않으므로
/// 같은 파일이 계속 `is_fresh` 다). 요청 1회와 요금 1건을 확실히 버리면서
/// 회복 가능성은 0인 경로였고, 설계 §5.1 이 약속한 복구가 아니었다.
///
/// # 판정은 **전선 위의 토큰 값**으로 한다
///
/// "오류가 났다/안 났다" 나 요청 수만으로는 부족하다 — 망가진 구현도 요청은
/// 2회 보낸다. 그래서 두 요청의 `Authorization` 헤더를 직접 비교한다:
///
/// - 거절 자체가 no-op 인 변이 → 캐시 적중 → **같은** 헤더 → 죽는다.
/// - 캐시만 비우는 변이(고치기 전 동작) → 디스크 채택 → **같은** 헤더 →
///   죽는다. `refresh_calls == 1` 단언도 함께 죽는다(0 이 된다).
/// - 거절이 세대 비교를 건너뛰는 변이는 여기서는 죽지 않는다 — 그쪽은
///   `concurrent_401_with_stale_generation_does_not_invalidate` 가 잡는
///   반대쪽 짝이다.
/// - 회전을 두 번 태우는 변이 → `refresh_calls == 2` → 죽는다.
#[test]
fn auth_401_forces_a_rotation_and_retries_with_a_different_token() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/users/region"))
        .respond_with(ResponseTemplate::new(401).set_body_raw(
            br#"{"error":"invalid authentication"}"#.to_vec(),
            "application/json",
        ))
        .mount(&server);

    let (c, refresh_calls) = test_client_counting_refreshes(&server);
    // 부트스트랩은 디스크 채택이다(픽스처가 유효 토큰을 심는다) — 회전 0회.
    let (gen_before, token_before) = c.tokens().access_token_with_generation().unwrap();
    assert_eq!(
        refresh_calls.load(Ordering::SeqCst),
        0,
        "픽스처 전제: 부트스트랩은 채택이라 회전이 없어야 한다"
    );

    let e = c.send(&GetRegion).unwrap_err();
    assert!(matches!(e, Error::Auth { .. }), "실제: {e:?}");

    let reqs = server.received_requests().unwrap();
    assert_eq!(reqs.len(), 2, "원요청 + 401 재시도 = 2회");
    let bearer = |i: usize| {
        reqs[i]
            .headers
            .get("authorization")
            .expect("Authorization 헤더")
            .to_str()
            .unwrap()
            .to_owned()
    };
    assert_eq!(
        bearer(0),
        format!("Bearer {token_before}"),
        "첫 요청은 거절당할 그 토큰을 실어야 한다(테스트 전제)"
    );
    assert_ne!(
        bearer(1),
        bearer(0),
        "🔴 재시도가 **방금 401 을 받은 그 토큰**을 그대로 다시 보냈다 — \
         복구가 아니라 낭비다(A4 에서는 요금과 rate limit 슬롯까지 낭비된다)"
    );
    assert_eq!(
        refresh_calls.load(Ordering::SeqCst),
        1,
        "401 복구는 회전을 **정확히 1회** 태워야 한다 \
         (0 = 거절이 채택으로 무력화됨, 2+ = 예산 초과)"
    );
    let gen_after = c.tokens().access_token_with_generation().unwrap().0;
    assert!(
        gen_after > gen_before,
        "캐시가 실제로 교체되지 않았다 (before={gen_before} after={gen_after})"
    );
}

/// 회전 억제(`MIN_REFRESH_INTERVAL`)가 복구를 막아도 **401 의 과금 사실**은
/// 보고된다 (final fix wave / Important 2).
///
/// # 왜 이 조합인가
///
/// 1번 반복이 진짜 401 을 받았다 → 유료(`Pricing::Data`) 엔드포인트였다면
/// 그 왕복은 **과금됐다**. 2번 반복의 토큰 계층이 회전 최소 간격에 막히면
/// `Error::Auth { pricing: None }` → `billing() == NotBilled` 다. 그걸 그대로
/// 돌려주면 (a) 지출을 적게 보고하고 (b) 실제로는 서버의 인증 거절이었던
/// 사건을 "token refresh suppressed" 로 설명한다. 그래서 **직전 응답 오류**를
/// 우선한다.
///
/// # 이 테스트의 전제가 **바뀌었다** (T23 fix round 2)
///
/// 예전 doc 은 *"Fleet 요청이 1회뿐 ⇒ 회전이 간격에 막혔다"* 고 적었다. 유료
/// 401 이 더 이상 **재발사하지 않으므로 그 추론은 이제 거짓**이다 — 1회인 이유는
/// pricing 이다. 리뷰어가 실측으로 잡았다: 이 테스트는 통과하지만 **다른 이유로**
/// 통과하고 있었고, `prior_response_error.unwrap_or(e)` 변이가 여기서 **생존**했다.
///
/// 그래서 역할을 둘로 갈랐다:
///
/// - **이 테스트** — *"유료 401 은 `Billed` 로, 서버의 문구 그대로 올라온다"*.
///   재발사가 없으므로 토큰 계층이 끼어들 자리 자체가 없다는 것을 고정한다.
/// - [`a_suppressed_recovery_prefers_the_401_over_the_token_layer_error`] —
///   `unwrap_or` 를 실제로 밟는 **무료** 경로. 그쪽이 변이를 죽인다.
///
/// # 공허하지 않은 이유
///
/// 이 크레이트의 실제 GET 3종은 전부 `NotCharged` 라 `billing()` 이 어느
/// 구현에서도 `NotBilled` 다 — 그걸로 단언하면 **공허**하다. 그래서 이
/// 테스트만을 위한 `Pricing::Data` 엔드포인트를 하나 세운다(`FleetGet` 은
/// sealed 이므로 크레이트 **안**에서만 가능하다).
#[test]
fn a_suppressed_recovery_refresh_still_reports_the_401s_billing() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/users/region"))
        .respond_with(ResponseTemplate::new(401).set_body_raw(
            br#"{"error":"invalid authentication"}"#.to_vec(),
            "application/json",
        ))
        .mount(&server);

    let c = client_bootstrapped_by_a_rotation(&server);
    let e = c.send(&BilledLikeRegion).unwrap_err();

    assert_eq!(
        server.received_requests().unwrap().len(),
        1,
        "유료 401 은 재발사하지 않는다 — 2회면 그것이 곧 중복 과금이다"
    );
    assert_eq!(
        e.billing(),
        crate::pricing::Billing::Billed,
        "🔴 토큰 계층 오류가 401 의 과금 사실을 지웠다 — 지출이 적게 보고된다: {e:?}"
    );
    let msg = e.to_string();
    assert!(
        msg.contains("invalid authentication"),
        "서버의 인증 거절이 아니라 다른 사건으로 설명되고 있다: {msg}"
    );
    assert!(
        !msg.contains("rotation interval"),
        "🔴 인증 거절을 '회전 억제' 로 잘못 설명한다: {msg}"
    );
}

/// **재시도의 토큰 계층 오류가 직전 401 을 덮어쓰면 안 된다** — `unwrap_or` 가
/// 하는 일 (T23 fix round 2 신설).
///
/// # 왜 새로 필요한가
///
/// `prior_response_error.unwrap_or(e)` 를 지키던 테스트는
/// [`a_suppressed_recovery_refresh_still_reports_the_401s_billing`] 하나뿐이었는데,
/// 그 테스트의 엔드포인트가 **유료**라 fix round 1/2 이후 401 복구 arm 의 `continue`
/// 를 밟지 않는다 → `unwrap_or` 줄에 **도달하지 않는다**. 리뷰어가
/// `unwrap_or(e)` → `e` 변이로 **생존**을 실증했다.
///
/// 이 테스트는 **무료** 엔드포인트로 그 줄을 실제로 밟는다: 401 → 세대 거절 →
/// 재시도 시도 → 2번째 반복의 토큰 계층이 `MIN_REFRESH_INTERVAL` 에 막혀 실패 →
/// 그때 돌려주는 것은 **서버의 401** 이어야 한다.
///
/// 무료라 `billing()` 은 양쪽 다 `NotBilled` 라 판정에 쓸 수 없다. 판정은
/// **메시지**로 한다 — 변이는 "rotation interval" 을 내고 원본은 "invalid
/// authentication" 을 낸다. 그 차이가 곧 *"인증 거절을 회전 억제로 잘못 설명한다"*
/// 는 운영자 오독이다.
#[test]
fn a_suppressed_recovery_prefers_the_401_over_the_token_layer_error() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/users/region"))
        .respond_with(ResponseTemplate::new(401).set_body_raw(
            br#"{"error":"invalid authentication"}"#.to_vec(),
            "application/json",
        ))
        .mount(&server);

    let c = client_bootstrapped_by_a_rotation(&server);
    // `GetRegion` — **무료**다. 그래서 401 복구 arm 이 `continue` 를 밟고
    //    `unwrap_or` 줄에 실제로 도달한다.
    let e = c.send(&GetRegion).unwrap_err();

    // 전제: 회전이 간격에 막혀 재시도가 전선에 나가지 못했다. (막히지 않았다면
    // 요청이 2회가 되고, 이 테스트가 겨냥한 경로가 아니게 된다.)
    assert_eq!(
        server.received_requests().unwrap().len(),
        1,
        "테스트 전제: 회전이 간격에 막혀 재시도가 나가지 못해야 한다"
    );
    let msg = e.to_string();
    assert!(
        msg.contains("invalid authentication"),
        "🔴 재시도의 토큰 계층 오류가 서버의 401 을 덮어썼다 — 운영자는 인증 거절을 \
         '회전 억제' 로 읽게 된다: {msg}"
    );
    assert!(
        !msg.contains("rotation interval"),
        "🔴 인증 거절을 '회전 억제' 로 잘못 설명한다: {msg}"
    );
}

/// 요청이 **도착했음을 신호로 알려 주는** 401 응답자.
///
/// fix round 1 (review Minor 4): 이전 판은 "send 가 80ms 안에 토큰을 잡는다"
/// 는 **시간 가정**에 의존했다. 여기서는 `respond` 가 호출된 시점 —
/// 즉 요청이 서버에 실제로 도착했고 따라서 **토큰이 이미 실려 나간 시점** —
/// 에 채널로 신호를 보낸다. 세대를 올리는 쪽은 그 신호를 기다리므로
/// "토큰 확보 → 세대 상승" 순서가 **인과적으로** 보장된다.
///
/// 남는 시간 의존은 하나뿐이고 성질이 다르다: 세대 상승이 응답 지연
/// 안에 끝나야 합니다. 응답은 신호를 보낸 뒤 지연으로 여유를 둡니다. 그 작업은 프로세스 내 뮤텍스 + 로컬 파일 I/O 뿐이라
/// 네트워크 대기가 없다.
struct SignalOnArrival {
    arrived: std::sync::Mutex<Option<std::sync::mpsc::SyncSender<()>>>,
}

impl fleetwire_testkit::Respond for SignalOnArrival {
    fn respond(&self, _req: &fleetwire_testkit::Request) -> ResponseTemplate {
        if let Some(tx) = self.arrived.lock().unwrap().take() {
            let _ = tx.send(());
        }
        ResponseTemplate::new(401)
            .set_delay(std::time::Duration::from_millis(500))
            .set_body_raw(
                br#"{"error":"invalid authentication"}"#.to_vec(),
                "application/json",
            )
    }
}

/// 세대가 다르면 **아무것도 무효화하지 않고** 즉시 재전송한다(design §5.1).
///
/// brief 원안은 `bump_generation_out_of_band` 를 send **이전**에 불렀는데,
/// 그러면 send 가 이미 오른 세대를 들고 나가므로 "세대 불일치" 분기 자체가
/// 실행되지 않는다 — 게다가 판정 지표였던 refresh 횟수는 어느 구현에서도
/// 움직이지 않는다(캐시를 지워도 디스크 토큰 **채택**으로 복구되어 refresh
/// POST 가 안 나간다). 즉 원안은 두 겹으로 공허했다.
///
/// 여기서는 (1) 요청 **도착 신호**를 받은 뒤에 세대를 올리고(순서가 인과적),
/// (2) 판정을 refresh 횟수가 아니라 **토큰 세대**로 한다 — 무조건 무효화하면
/// 재시도가 디스크를 채택하면서 세대가 한 번 더 오르므로 그 변이(M12)가 죽는다.
#[test]
fn concurrent_401_with_stale_generation_does_not_invalidate() {
    let server = MockServer::start();
    let (arrived_tx, arrived_rx) = std::sync::mpsc::sync_channel(1);
    Mock::given(method("GET"))
        .and(path("/api/1/users/region"))
        .respond_with(SignalOnArrival {
            arrived: std::sync::Mutex::new(Some(arrived_tx)),
        })
        .up_to_n_times(1)
        .mount(&server);
    Mock::given(method("GET"))
        .and(path("/api/1/users/region"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            br#"{"response":{"region":"na"}}"#.to_vec(),
            "application/json",
        ))
        .mount(&server);

    let (c, refresh_calls) = test_client_counting_refreshes(&server);
    // 첫 토큰을 확보해 두어 send 가 세대 G 로 출발하게 한다.
    let (g0, _) = c.tokens().access_token_with_generation().unwrap();
    let before_refreshes = refresh_calls.load(Ordering::SeqCst);

    let (got, gen_after_bump) = std::thread::scope(|sc| {
        let sending = sc.spawn(|| c.send(&GetRegion));
        let c_ref = &c;
        let racing = sc.spawn(move || {
            // 요청이 서버에 **도착한 뒤에** — 즉 토큰이 이미 실려 나간 뒤에 —
            // 다른 요청이 세대를 올립니다.
            arrived_rx.recv().expect("401 요청이 서버에 도착해야 한다");
            bump_generation_out_of_band(c_ref);
            c_ref.tokens().access_token_with_generation().unwrap().0
        });
        (sending.join().unwrap(), racing.join().unwrap())
    });

    // 이 한 줄이 **거짓 통과**를 막는다 (final fix wave / Minor 6).
    // 극단적인 스케줄링 지연으로 `send` 가 세대 상승보다 **먼저** 복구를
    // 끝내면, 시험하려던 "세대 불일치" 분기는 한 번도 실행되지 않은 채
    // 아래 두 단언이 나란히 통과한다(복구가 캐시 채택으로 세대를 올렸다면
    // `gen_now == gen_after_bump` 도 성립한다). 상승이 정확히 한 칸이어야
    // 한다고 못박으면, 굶주린 bump 와 정상 순서를 구별할 수 있다.
    assert_eq!(
        gen_after_bump,
        g0 + 1,
        "세대를 올린 것이 out-of-band bump 하나뿐이 아니다 — 401 복구가 \
         먼저 끼어들었다는 뜻이고, 그러면 이 테스트는 목표한 분기를 \
         한 번도 실행하지 않는다 (g0={g0} after_bump={gen_after_bump})"
    );
    assert_eq!(got.unwrap().region.as_deref(), Some("na"));
    let gen_now = c.tokens().access_token_with_generation().unwrap().0;
    assert_eq!(
        gen_now, gen_after_bump,
        "세대가 이미 올라갔으면 401 복구는 캐시를 **건드리면 안 된다** \
         — 무조건 무효화하면 재시도가 디스크를 채택하며 세대가 또 오른다"
    );
    assert_eq!(
        refresh_calls.load(Ordering::SeqCst),
        before_refreshes,
        "refresh POST 도 더 태우면 안 된다"
    );
}

/// 401 재시도와 412 재시도가 서로를 재설정하지 않는다 — Fleet 요청 총 3회 상한.
#[test]
fn auth_and_region_retries_do_not_reset_each_other() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/vehicles"))
        .respond_with(ResponseTemplate::new(401).set_body_raw(
            br#"{"error":"invalid authentication"}"#.to_vec(),
            "application/json",
        ))
        .up_to_n_times(1)
        .mount(&server);
    Mock::given(method("GET"))
        .and(path("/api/1/vehicles"))
        .respond_with(ResponseTemplate::new(412).set_body_raw(
            br#"{"error":"must be registered"}"#.to_vec(),
            "application/json",
        ))
        .mount(&server);
    Mock::given(method("GET"))
        .and(path("/api/1/users/region"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(
                format!(
                    r#"{{"response":{{"fleet_api_base_url":"{}"}}}}"#,
                    server.uri()
                )
                .into_bytes(),
                "application/json",
            ),
        )
        .mount(&server);

    let c = test_client(&server);
    let _ = c.send(&crate::api::vehicles::ListVehicles {
        page: None,
        per_page: None,
    });

    let reqs = server.received_requests().unwrap();
    let vehicles = reqs
        .iter()
        .filter(|r| r.url.path() == "/api/1/vehicles")
        .count();
    let region = reqs
        .iter()
        .filter(|r| r.url.path() == "/api/1/users/region")
        .count();
    assert_eq!(
        vehicles, 3,
        "원요청 + 401 재시도 + 412 재시도 = 3회 상한. \
         플래그가 서로를 재설정하면 이 값이 커진다"
    );
    // fix round 1: 최악 경로에서도 리전 해석은 2회를 넘지 않는다(§7 예산).
    // 사전 해석이 루프 **안**에 있으면 401 재시도가 한 번 더 태워 3회가 된다.
    assert_eq!(region, 2, "리전 해석은 최악에도 2회 — 사전 1 + 412 복구 1");
}

/// 412 바디의 계정 UUID 가 에러 메시지에 남지 않는다.
#[test]
fn account_uuid_is_redacted() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(412)
            .insert_header("x-request-id", "0aea2788-ca44-4391-95a3-774e13da5b5d")
            .set_body_raw(
                br#"{"error":"Account 00000000-1111-4222-8333-444444444444 must be registered in the current region"}"#.to_vec(),
                "application/json"))
        .mount(&server);

    let c = test_client(&server);
    let e = c.send(&GetRegion).unwrap_err();
    let rendered = format!("{e}{e:?}");
    assert!(
        !rendered.contains("00000000-1111-4222-8333-444444444444"),
        "🔴 계정 UUID 가 노출됐다: {rendered}"
    );
    assert!(
        rendered.contains("[REDACTED]"),
        "가려졌다는 표식이 남아야 한다: {rendered}"
    );
}

// --- T8: 재시도 opt-in --------------------------------------------------

/// 기본값에서는 5xx 여도 재시도하지 않는다(상위 스펙 §11 non-goal).
#[test]
fn default_does_not_retry_even_on_retryable_5xx() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503).set_body_raw(b"{}".to_vec(), "application/json"))
        .mount(&server);

    let c = test_client(&server); // 기본 빌더 = max_attempts 1
    let e = c.send(&GetRegion).unwrap_err();
    assert!(
        e.is_retryable(),
        "503 은 재시도 **가능**하다고 판정돼야 한다"
    );

    let reqs = server.received_requests().unwrap();
    assert_eq!(
        reqs.len(),
        1,
        "판정이 가능이어도 기본값에서는 보내지 않는다"
    );
}

/// max_attempts(3) 을 켜면 500·502·503·504 만 재시도한다.
#[test]
fn opt_in_retries_only_transient_5xx() {
    for (status, expected_sends) in [(503_u16, 3_usize), (501, 1)] {
        let server = MockServer::start();
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(status).set_body_raw(b"{}".to_vec(), "application/json"),
            )
            .mount(&server);

        let (b, _dir) = test_client_builder(&server);
        let c = b
            .max_attempts(3)
            .retry_deadline(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        let _ = c.send(&GetRegion);

        let reqs = server.received_requests().unwrap();
        assert_eq!(
            reqs.len(),
            expected_sends,
            "status={status} 는 {expected_sends}회 전송돼야 한다"
        );
    }
}

/// Fix round 1 (Important 1): 401/412 복구 예산은 **`send()` 호출
/// 전체에서 공유**되고, opt-in 재시도 시도마다 새로 주어지지 않는다.
///
/// 응답을 401 → 503 → 401 → 503 순으로 스크립트한다. **공유(고친)**
/// 구현: 1번 요청(401) → 복구 소비(`auth_retry_used=true`, 요금 없이
/// 즉시 재전송) → 2번 요청(503, 이제부터는 opt-in 재시도 판단) → 백오프 →
/// 3번 요청(다시 401 이지만 예산이 이미 소비돼 복구되지 않고 그대로
/// `Error::Auth` 로 끝남) — **총 3회**.
///
/// 고치기 **전**(각 opt-in 재시도가 자기만의 새 401/412 예산을 받는)
/// 구현이었다면: 1번 요청(401, 새 예산)→복구→2번 요청(503, 그 "시도"
/// 종료)→백오프→3번 요청(새 예산 리셋, 401 다시 복구)→4번 요청(503, 그
/// "시도" 종료)→백오프→5번 요청(스크립트 밖, catch-all) — **총 5회**.
/// 3 대 5 로 정확히 갈리므로, 이 값이 되돌아가면 여기서 바로 드러난다.
#[test]
fn recovery_budget_is_shared_across_the_whole_send_not_per_retry_attempt() {
    let server = MockServer::start();
    for status in [401_u16, 503, 401, 503] {
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(status)
                    .set_body_raw(br#"{"error":"x"}"#.to_vec(), "application/json"),
            )
            .up_to_n_times(1)
            .mount(&server);
    }
    // 스크립트를 넘어서는 요청을 위한 안전망(정상 경로라면 도달하지
    // 않아야 하고, 도달해도 wiremock 패닉이 아니라 아래 개수 단언이
    // 깔끔하게 실패하도록 무제한 catch-all 을 마지막에 둔다).
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503).set_body_raw(b"{}".to_vec(), "application/json"))
        .mount(&server);

    let (b, _dir) = test_client_builder(&server);
    let c = b
        .max_attempts(3)
        .retry_deadline(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    let e = c.send(&GetRegion).unwrap_err();

    assert!(matches!(e, Error::Auth { .. }), "실제: {e:?}");
    let reqs = server.received_requests().unwrap();
    assert_eq!(
        reqs.len(),
        3,
        "🔴 401 복구 예산이 재시도 시도마다 새로 주어지면(고치기 전 버그) 5 회가 \
         나간다 — 공유(고친) 구현은 정확히 3 회: 401 복구 1 + 503 재시도 1 + \
         두 번째 401(복구 불가, 종료)"
    );
}

/// `TransportUnknown` 은 켜도 재시도하지 않는다.
///
/// **이 변종에 한해서는 "재시도 = 중복 과금"이 실제로 성립한다** — 다만
/// 확정이 아니라 가능성으로. 유료 엔드포인트의 `TransportUnknown` 은
/// [`Error::billing`] 이 [`crate::Billing::Unknown`] 으로 판정하는 유일한 모양이고
/// (서버가 이미 처리·과금했는지 알 수 없다), 그래서 재시도는 **두 번째 청구일 수
/// 있다**. 재시도 가능한 5xx(§9.2 가 `NotBilled` 로 판정)와는 다른 근거이므로
/// 두 테스트의 이유를 섞어 쓰지 마라.
#[test]
fn transport_unknown_is_never_retried_even_when_enabled() {
    let server = MockServer::start();
    // 응답을 지연시켜 요청 timeout 을 유발한다 → TransportUnknown.
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(std::time::Duration::from_secs(30))
                .set_body_raw(b"{}".to_vec(), "application/json"),
        )
        .mount(&server);

    let (b, _dir) = test_client_builder(&server);
    let c = b
        .max_attempts(3)
        .request_timeout(std::time::Duration::from_millis(200))
        .build()
        .unwrap();
    let e = c.send(&GetRegion).unwrap_err();

    assert!(matches!(e, Error::TransportUnknown { .. }), "실제: {e:?}");
    assert!(!e.is_retryable(), "Unknown 은 재시도 금지 판정이어야 한다");
    let reqs = server.received_requests().unwrap();
    assert_eq!(reqs.len(), 1, "재시도를 켜도 Unknown 은 다시 보내지 않는다");
}

/// 백오프 경로가 기본값에서 실행되지 않는다 — "죽은 코드"가 아니라
/// "조건부 코드"임을 고정한다(design §9.0).
///
/// Fix round 1 (test-strength finding): 원래 판은 소요시간(<500ms)만
/// 쟀는데, 이는 **기전이 아니라 값**을 단언하는 셈이라 M1(기본값 1→3)이
/// 살아남았다 — 지수 백오프 초반 두 잠(≈100ms+200ms, jitter 로 더 줄어듦)
/// 합이 우연히 500ms 밑이라서다. 요청 수를 세면(규율 3) 기본값이 몇으로
/// 바뀌든 그 값과 정확히 일치해야 하므로 죽지 않는다 —
/// `default_does_not_retry_even_on_retryable_5xx` 와 같은 단언을 여기서도
/// 반복하는 이유는, "백오프 소요시간이 짧다"와 "재시도를 아예 안 한다"가
/// 서로 다른 코드 경로에 걸려 있어(전자는 `backoff_delay`+`sleep`, 후자는
/// `retries_used`+1 vs `max_attempts` 비교) 하나의 단언으로는 둘 다
/// 보증되지 않기 때문이다.
#[test]
fn backoff_path_is_not_entered_by_default() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503).set_body_raw(b"{}".to_vec(), "application/json"))
        .mount(&server);

    // 기본값에서는 백오프 대기가 없으므로 즉시 끝난다.
    let c = test_client(&server);
    let t0 = std::time::Instant::now();
    let _ = c.send(&GetRegion);
    assert!(
        t0.elapsed() < std::time::Duration::from_millis(500),
        "기본값에서 백오프를 타면 안 된다: {:?}",
        t0.elapsed()
    );

    // 기전 단언: 503 은 재시도 **가능** 판정이지만, 기본값에서는 몇 번을
    // 보냈는지로 직접 잰다 — M1(기본값 1→3)이 여기서 죽는다.
    let reqs = server.received_requests().unwrap();
    assert_eq!(
        reqs.len(),
        1,
        "기본값이 바뀌면(예: 1→3) 요청 수가 여기서 바로 드러나야 한다"
    );
}

/// 완전히 결정적인 체인 deadline 시험: deadline 을 `ZERO` 로 주면
/// **첫 실패 직후 재시도가 단 한 번도 없어야 한다** — 지수 백오프의
/// jitter 가 무엇을 뽑든 상관없다(`elapsed >= ZERO` 는 항상 참이므로
/// 첫 실패 시점에 이미 예산 소진). `max_attempts` 는 5 로 커 보이게 열어
/// 둬서, deadline 검사가 아니라면 재시도했을 것임을 분명히 한다.
///
/// Fix round 1(test-strength finding): 이 테스트가 바로 리뷰가 요구한
/// "요청 수가 deadline 이 허용하는 값과 정확히 일치"의 **완전 결정적**
/// 버전이다 — deadline 을 0 으로 주면 "허용하는 값"이 항상 정확히 1 이라
/// jitter 변동성이 끼어들 여지가 없다.
#[test]
fn zero_chain_deadline_permits_no_retries_at_all() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503).set_body_raw(b"{}".to_vec(), "application/json"))
        .mount(&server);

    let (b, _dir) = test_client_builder(&server);
    let c = b
        .max_attempts(5)
        .retry_deadline(std::time::Duration::ZERO)
        .build()
        .unwrap();

    let t0 = std::time::Instant::now();
    let e = c.send(&GetRegion).unwrap_err();
    let elapsed = t0.elapsed();

    assert!(matches!(&e, Error::Api { status, .. } if status.as_u16() == 503));
    // 결정적 단언: 요청 수 == 1 이 이 시험의 핵심이다. 아래 소요시간
    // 단언은 그 위에 얹는 보너스 sanity check 일 뿐(백오프에 진짜로
    // 들어갔다면 잡아낼 만큼만 느슨하게) — 요청 수가 이미 1 로 확정됐으면
    // 로컬 wiremock 왕복이 어쩌다 오래 걸리는 것(느린 CI/WSL) 자체는
    // 이 시험이 지키려는 성질과 무관하다.
    let reqs = server.received_requests().unwrap();
    assert_eq!(
        reqs.len(),
        1,
        "🔴 deadline=ZERO 인데 재시도가 나갔다 — deadline 검사가 우회됐다"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "1 회 왕복치고 비정상적으로 오래 걸렸다(백오프 진입 의심): {elapsed:?}"
    );
}

/// 체인 deadline 은 **백오프 잠까지 포함한** 전체 소요를 제한한다 —
/// per-attempt timeout 만으로는 못 잡는 것을 design §9.2·Task 4 의 교훈이
/// 지적한 바로 그 모양이다. [`zero_chain_deadline_permits_no_retries_at_all`]
/// 이 이 성질의 **완전 결정적** 증거이고, 이 테스트는 그와 별개로 "0 이
/// 아닌 deadline 도 실제로 재시도 도중에 끊는다"를 잰다.
///
/// Fix round 1(test-strength finding, **정정판**): 원래 판은 여기에
/// `reqs.len() < max_attempts` 도 같이 단언했는데, 실제로 돌려 보니
/// **첫 실행에서 바로 깨졌다** — `max_attempts(5)`·`retry_deadline(300ms)`
/// 조합에서 지수 백오프 4 회(100·200·400·800ms 기준값)의 uniform jitter
/// 합이 우연히 300ms 밑으로 들어오는 사건은, 내가 처음 추정했던
/// "1.6e-5 급 희귀 사건"이 아니라 **실측상 흔한 사건**이었다(가장 큰
/// 항 800ms·jitter 만 ≈0.37 이하면 이미 근접). 즉 seed 없는 진짜
/// wall-clock jitter 를 쓰는 한 요청 수 상한을 **엄격 부등식으로
/// 단언하는 것은 이 구현으로는 결정적으로 참일 수 없다** — 리뷰가 미리
/// 경고한 정확히 그 상황이라 값을 억지로 좁히는 대신 그 단언 자체를
/// 뺐다. 대신 결정적으로 성립하는 성질만 남긴다: 각 백오프 잠은 항상
/// `remaining`(=deadline 까지 남은 시간)으로 clamp 되므로 **잠에 쓴
/// 누적 시간은 jitter 가 무엇을 뽑든 `retry_deadline` 을 절대 넘지
/// 않는다** — 남는 오버런은 마지막 실제 네트워크 왕복(로컬 mock, 수 ms)
/// 뿐이라 `retry_deadline + 200ms` 슬랙은 jitter 와 무관하게 항상
/// 성립한다. "최소 1 회는 재시도했다"(`reqs.len() > 1`)도 남긴다 —
/// `retry_deadline`(300ms)이 로컬 왕복 1 회보다 넉넉히 크므로 이 또한
/// 사실상 결정적이다. `max_attempts` 를 5 로 열어 둔 것은 재시도
/// 예산이 아니라 deadline 이 실제 제한 요인임을 보이기 위해서다.
#[test]
fn nonzero_chain_deadline_cuts_off_mid_retry() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503).set_body_raw(b"{}".to_vec(), "application/json"))
        .mount(&server);

    let retry_deadline = std::time::Duration::from_millis(300);
    let (b, _dir) = test_client_builder(&server);
    let c = b
        .max_attempts(5)
        .retry_deadline(retry_deadline)
        .build()
        .unwrap();

    let t0 = std::time::Instant::now();
    let e = c.send(&GetRegion).unwrap_err();
    let elapsed = t0.elapsed();

    assert!(matches!(&e, Error::Api { status, .. } if status.as_u16() == 503));
    assert!(
        elapsed < retry_deadline + std::time::Duration::from_millis(200),
        "🔴 deadline 이 백오프 잠을 안 끊었다 — 전체 소요 {elapsed:?} (예산 {retry_deadline:?})"
    );

    let reqs = server.received_requests().unwrap();
    assert!(
        reqs.len() > 1,
        "적어도 한 번은 재시도(백오프를 최소 1회 통과)했어야 한다: {}건",
        reqs.len()
    );
}

// --- T9: 스코프 advisory 선검사 -----------------------------------------

/// 401 복구 뒤 갱신된 토큰에 명령 스코프가 생기면, 그 토큰으로는 아무것도 보내지 않습니다.
/// 서버에는 첫 요청만 닿아야 합니다.
#[test]
fn refreshed_token_with_command_scope_is_refused_before_the_retry() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/users/region"))
        .respond_with(ResponseTemplate::new(401).set_body_raw(
            br#"{"error":"invalid authentication"}"#.to_vec(),
            "application/json",
        ))
        .mount(&server);

    let dir = crate::test_support::temp_dir("client-widened");
    let seed = fake_jwt(&["vehicle_device_data", "openid"]);
    crate::test_support::write_token(&dir, &seed, "NA_SEED", crate::test_support::FAR_FUTURE);
    let tokens = TokenManager::new(
        dir.to_path_buf(),
        Arc::new(CountingRefresher {
            token: fake_jwt(&["vehicle_device_data", "openid", "vehicle_cmds"]),
            calls: Arc::new(AtomicUsize::new(0)),
        }),
    );
    let url: crate::Url = server.uri().parse().unwrap();
    let host = url.host_str().unwrap().to_owned();
    let client = Client::builder(tokens)
        .bootstrap_region(url.clone())
        .allow_region_host(&host)
        .allow_region_origin_for_tests(&url)
        .request_timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    let _guard = dir;

    let e = client.send(&GetRegion).unwrap_err();
    assert!(matches!(e, Error::CommandScopeOnReadToken), "실제: {e:?}");
    let reqs = server.received_requests().unwrap();
    assert_eq!(reqs.len(), 1, "갱신된 명령 스코프 토큰으로 요청이 나갔다");
    assert!(
        !reqs[0]
            .headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("-rot"),
        "첫 요청은 갱신 전 토큰이어야 한다"
    );
}

/// 명령 스코프가 든 읽기 토큰은 **요청 전에** 막힌다(서버 수신 0건).
#[test]
fn read_client_refuses_token_with_command_scope_before_sending() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(br#"{"response":[]}"#.to_vec(), "application/json"),
        )
        .mount(&server);

    let c = test_client_with_scopes(&server, &["openid", "vehicle_device_data", "vehicle_cmds"]);
    let result: Result<Page<crate::model::VehicleSummary>, Error> =
        c.send(&crate::api::vehicles::ListVehicles {
            page: None,
            per_page: None,
        });
    let e = match result {
        Err(e) => e,
        Ok(_) => panic!("명령 스코프 토큰이 통과했다 — CommandScopeOnReadToken 을 기대했다"),
    };
    assert!(matches!(e, Error::CommandScopeOnReadToken), "실제: {e:?}");
    assert!(matches!(e.billing(), crate::pricing::Billing::NotBilled));
    assert!(
        server.received_requests().unwrap().is_empty(),
        "명령 스코프 토큰으로 요청이 서버에 닿았다"
    );
}

/// 스코프가 확실히 없으면 **네트워크 이전에** 막는다(0).
/// "에러가 났다"로는 부족하다 — 서버 수신 요청이 0건임을 단언한다.
#[test]
fn missing_scope_blocks_before_any_request() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(br#"{"response":[]}"#.to_vec(), "application/json"),
        )
        .mount(&server);

    // scp 에 vehicle_device_data 가 **없는** JWT 를 심는다.
    let c = test_client_with_scopes(&server, &["openid", "offline_access"]);
    // `.unwrap_err()` 를 쓰지 않는다 — `VehicleSummary` 는 REQ-18 상
    // 의도적으로 `Debug` 를 구현하지 않으므로(모델 doc 참조), `Result<T,
    // E>::unwrap_err` 의 `T: Debug` 바운드에 걸린다. 수동 매치로 그 요구를
    // 피한다.
    let result: Result<Page<crate::model::VehicleSummary>, Error> =
        c.send(&crate::api::vehicles::ListVehicles {
            page: None,
            per_page: None,
        });
    let e = match result {
        Err(e) => e,
        Ok(_) => panic!("스코프 선검사가 통과시켰다 — MissingScope 를 기대했다"),
    };

    assert!(matches!(e, Error::MissingScope { .. }), "실제: {e:?}");
    assert!(matches!(e.billing(), crate::pricing::Billing::NotBilled));
    assert!(
        server.received_requests().unwrap().is_empty(),
        "선검사가 막았다면 요청이 나가면 안 된다"
    );
}

/// JWT 가 아니거나 파싱 불가면 **통과**시킨다(fail-open).
/// 서명 미검증 payload 는 권위가 없다 — 최종 판정은 서버의 403 이다.
#[test]
fn opaque_token_passes_through() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(br#"{"response":[]}"#.to_vec(), "application/json"),
        )
        .mount(&server);

    // JWT 가 아닌 불투명 토큰 — scp 를 관찰할 수 없다.
    let c = test_client_with_opaque_token(&server);
    let got: Page<crate::model::VehicleSummary> = c
        .send(&crate::api::vehicles::ListVehicles {
            page: None,
            per_page: None,
        })
        .unwrap();

    assert!(got.items.is_empty());
    // 총 요청 수가 아니라 **이 엔드포인트로 나간** 요청 수를 센다. 리전
    // 캐시가 비어 있으면 `send` 이 `/api/1/users/region` 사전해석을 먼저
    // 태우는데(§4.2, T7 에서 이미 다룸), 그건 이 테스트의 관심사가 아니다
    // — mock 이 경로를 가리지 않아 그 요청도 서버에 도달해 기록된다.
    let vehicle_hits = server
        .received_requests()
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/api/1/vehicles")
        .count();
    assert_eq!(
        vehicle_hits, 1,
        "관찰 불가는 **통과**여야 한다(fail-open) — 실제 엔드포인트에 정확히 1회 도달해야 한다"
    );
}

/// Indeterminate 와 ContainsRequiredScope 가 **같은 경로**를 탄다.
/// 호출부에서 갈라지면 fail-open 이 거기서 깨진다(design §10).
#[test]
fn indeterminate_and_contains_take_the_same_path() {
    for scopes in [
        None,                               // Indeterminate (JWT 아님)
        Some(&["vehicle_device_data"][..]), // ContainsRequiredScope
    ] {
        let server = MockServer::start();
        Mock::given(method("GET"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(br#"{"response":[]}"#.to_vec(), "application/json"),
            )
            .mount(&server);

        let c = match scopes {
            None => test_client_with_opaque_token(&server),
            Some(s) => test_client_with_scopes(&server, s),
        };
        let r: Result<Page<crate::model::VehicleSummary>, _> =
            c.send(&crate::api::vehicles::ListVehicles {
                page: None,
                per_page: None,
            });

        assert!(r.is_ok(), "두 경우 모두 통과해야 한다: scopes={scopes:?}");
        // 위와 같은 이유로 `/api/1/vehicles` 히트만 센다 — 리전 사전해석은
        // 별개 관심사다.
        let vehicle_hits = server
            .received_requests()
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == "/api/1/vehicles")
            .count();
        assert_eq!(
            vehicle_hits, 1,
            "두 경우 모두 실제 엔드포인트에 정확히 1회 나가야 한다: scopes={scopes:?}"
        );
    }
}

/// 403 은 `MissingScope` 가 아니라 `Error::Api` 다 — 과금 판정이 다르기 때문이다.
/// MissingScope 는 네트워크 이전 차단(0), 403 은 돈이 든 서버 거절이다.
#[test]
fn server_403_is_api_error_not_missing_scope() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(403).set_body_raw(
            br#"{"error":"insufficient scope"}"#.to_vec(),
            "application/json",
        ))
        .mount(&server);

    // 선검사는 통과시키되 서버가 거절하게 만든다.
    let c = test_client_with_scopes(&server, &["vehicle_device_data"]);
    // `.unwrap_err()` 를 쓰지 않는 이유는 위 테스트와 같다(`VehicleSummary`
    // 는 `Debug` 미구현, REQ-18).
    let result: Result<Page<crate::model::VehicleSummary>, Error> =
        c.send(&crate::api::vehicles::ListVehicles {
            page: None,
            per_page: None,
        });
    let e = match result {
        Err(e) => e,
        Ok(_) => panic!("서버 403 을 기대했다"),
    };

    assert!(
        matches!(&e, Error::Api { status, .. } if status.as_u16() == 403),
        "실제: {e:?}"
    );
    assert!(
        !matches!(e, Error::MissingScope { .. }),
        "서버 403 을 MissingScope 로 접으면 과금 판정이 뭉개진다"
    );
    // 위와 같은 이유로 `/api/1/vehicles` 히트만 센다 — 리전 사전해석은
    // 별개 관심사다.
    let vehicle_hits = server
        .received_requests()
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/api/1/vehicles")
        .count();
    assert_eq!(
        vehicle_hits, 1,
        "이 요청은 실제로 나갔다 — 무과금 엔드포인트라 $0 이지만 나간 건 사실이다"
    );
}

// --- T9: send_guarded (비용 게이트) -----------------------------------

/// offline 이면 유료 요청이 **0회** 나간다. 게이트의 존재 이유다.
#[test]
fn guard_skips_the_paid_call_when_offline() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/vehicles/5YJ3E1EA1JF000001"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(
                crate::test_support::FIXTURE_VEHICLE_STATE_OFFLINE
                    .as_bytes()
                    .to_vec(),
                "application/json",
            ),
        )
        .mount(&server);

    let c = test_client(&server);
    let req = GetVehicleData::new("5YJ3E1EA1JF000001", vec![DataEndpoint::ChargeState]).unwrap();
    let got = c.send_guarded(&req).unwrap();

    assert!(
        matches!(got.value, Guarded::Skipped { ref state } if state == "offline"),
        "{:?}",
        got.value
    );
    let paid = server
        .received_requests()
        .unwrap()
        .iter()
        .filter(|r| r.url.path().ends_with("/vehicle_data"))
        .count();
    assert_eq!(paid, 0, "게이트가 막았는데 유료 요청이 나갔다");
}

/// online 이면 실제로 호출한다.
#[test]
fn guard_executes_when_online() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/vehicles/5YJ3E1EA1JF000001"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            br#"{"response":{"state":"online"}}"#.to_vec(),
            "application/json",
        ))
        .mount(&server);
    Mock::given(method("GET"))
        .and(path("/api/1/vehicles/5YJ3E1EA1JF000001/vehicle_data"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            br#"{"response":{"vin":"5YJ3E1EA1JF000001"}}"#.to_vec(),
            "application/json",
        ))
        .mount(&server);

    let c = test_client(&server);
    let req = GetVehicleData::new("5YJ3E1EA1JF000001", vec![DataEndpoint::ChargeState]).unwrap();
    assert!(matches!(
        c.send_guarded(&req).unwrap().value,
        Guarded::Executed(_)
    ));
}

/// 게이트가 실패하면 fail-closed — 유료 호출을 강행하지 않는다.
#[test]
fn guard_failure_does_not_force_the_paid_call() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/vehicles/5YJ3E1EA1JF000001"))
        .respond_with(ResponseTemplate::new(503).set_body_raw(b"{}".to_vec(), "application/json"))
        .mount(&server);

    let c = test_client(&server);
    let req = GetVehicleData::new("5YJ3E1EA1JF000001", vec![DataEndpoint::ChargeState]).unwrap();
    assert!(c.send_guarded(&req).is_err());

    let paid = server
        .received_requests()
        .unwrap()
        .iter()
        .filter(|r| r.url.path().ends_with("/vehicle_data"))
        .count();
    assert_eq!(
        paid, 0,
        "게이트가 실패했는데 유료 요청이 나갔다 — fail-open 이다"
    );
}

/// backseat_token 이 게이트 응답에 있는데 에러·로그로 새면 안 된다.
#[test]
fn guard_does_not_leak_backseat_token() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/vehicles/5YJ3E1EA1JF000001"))
        .respond_with(
            ResponseTemplate::new(200).set_body_raw(
                crate::test_support::FIXTURE_VEHICLE_STATE_OFFLINE
                    .as_bytes()
                    .to_vec(),
                "application/json",
            ),
        )
        .mount(&server);

    let c = test_client(&server);
    let req = GetVehicleData::new("5YJ3E1EA1JF000001", vec![DataEndpoint::ChargeState]).unwrap();
    let got = c.send_guarded(&req).unwrap();
    assert!(
        !format!("{:?}", got.value).contains("SECRET-BACKSEAT"),
        "토큰이 샜다: {:?}",
        got.value
    );
}

/// A5 T2: 게이트를 통과한 유료 호출의 rate-limit 헤더가 호출자에게 도달한다.
/// A4 까지는 send_guarded 가 send 를 써서 이 값을 통째로 버렸다.
#[test]
fn a_guarded_paid_call_surfaces_its_rate_limit() {
    let server = MockServer::start();
    // probe: online
    Mock::given(method("GET"))
        .and(path("/api/1/vehicles/5YJ3E1EA1JF000001"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            br#"{"response":{"state":"online"}}"#.to_vec(),
            "application/json",
        ))
        .mount(&server);
    // 유료 호출: ratelimit 헤더 3종을 낸다
    Mock::given(method("GET"))
        .and(path("/api/1/vehicles/5YJ3E1EA1JF000001/vehicle_data"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ratelimit-device-data-per-vehicle-limit", "60")
                .insert_header("ratelimit-device-data-per-vehicle-remaining", "57")
                .insert_header("ratelimit-device-data-per-vehicle-reset", "42")
                .set_body_raw(br#"{"response":{}}"#.to_vec(), "application/json"),
        )
        .mount(&server);

    let c = test_client(&server);
    let req = GetVehicleData::new("5YJ3E1EA1JF000001", vec![DataEndpoint::ChargeState]).unwrap();
    let got = c.send_guarded(&req).unwrap();

    assert!(matches!(got.value, Guarded::Executed(_)));
    let rl = got.rate_limit.expect("게이트가 rate-limit 을 버렸다");
    assert_eq!(rl.limit, 60);
    assert_eq!(rl.remaining, 57);
    assert_eq!(rl.reset_raw, 42);
}

/// A5 T2: Skipped 경로는 **probe 의** rate-limit 을 낸다. 실측상 probe 는 헤더를 내지
/// 않으므로 None 이지만, 하드코딩된 None 이 아니라 probe 에서 온 값이어야 한다 —
/// Tesla 가 probe 에 헤더를 붙이기 시작하면 그것이 보여야 한다.
#[test]
fn a_skipped_gate_reports_the_probes_rate_limit_not_a_hardcoded_none() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/vehicles/5YJ3E1EA1JF000001"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ratelimit-device-data-per-vehicle-limit", "60")
                .insert_header("ratelimit-device-data-per-vehicle-remaining", "11")
                .insert_header("ratelimit-device-data-per-vehicle-reset", "9")
                .set_body_raw(
                    br#"{"response":{"state":"asleep"}}"#.to_vec(),
                    "application/json",
                ),
        )
        .mount(&server);

    let c = test_client(&server);
    let req = GetVehicleData::new("5YJ3E1EA1JF000001", vec![DataEndpoint::ChargeState]).unwrap();
    let got = c.send_guarded(&req).unwrap();

    assert!(matches!(got.value, Guarded::Skipped { ref state } if state == "asleep"));
    let rl = got.rate_limit.expect("probe 의 rate-limit 이 버려졌다");
    assert_eq!(rl.remaining, 11);
    // 기전 단언(규율 3조): 유료 경로로 요청이 나가지 않았다.
    let paid = server
        .received_requests()
        .unwrap()
        .iter()
        .filter(|r| r.url.path().ends_with("/vehicle_data"))
        .count();
    assert_eq!(paid, 0, "게이트가 Skipped 인데 유료 호출이 나갔다");
}

// --- A4 T10: RateLimit / Metered<T> ------------------------------------

/// 유료 엔드포인트의 rate-limit 헤더가 응답에 붙어서 온다.
#[test]
fn metered_surfaces_the_rate_limit_headers() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/vehicles/5YJ3E1EA1JF000001/vehicle_data"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ratelimit-device-data-per-vehicle-limit", "60")
                .insert_header("ratelimit-device-data-per-vehicle-remaining", "59")
                .insert_header("ratelimit-device-data-per-vehicle-reset", "57")
                .set_body_raw(
                    br#"{"response":{"vin":"5YJ3E1EA1JF000001"}}"#.to_vec(),
                    "application/json",
                ),
        )
        .mount(&server);

    let c = test_client(&server);
    let req = GetVehicleData::new("5YJ3E1EA1JF000001", vec![DataEndpoint::ChargeState]).unwrap();
    let m = c.send_metered(&req).unwrap();
    let rl = m.rate_limit.expect("유료 엔드포인트는 헤더를 준다");
    assert_eq!((rl.limit, rl.remaining, rl.reset_raw), (60, 59, 57));
}

/// 무과금 엔드포인트는 헤더가 없다 → None (실측 E4).
#[test]
fn metered_is_none_for_an_unmetered_endpoint() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/users/region"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            br#"{"response":{"region":"na"}}"#.to_vec(),
            "application/json",
        ))
        .mount(&server);

    let c = test_client(&server);
    let m = c.send_metered(&GetRegion).unwrap();
    assert!(m.rate_limit.is_none(), "무과금 응답에 rate limit 이 생겼다");
}

/// 헤더가 일부만 있거나 숫자가 아니면 None 이다 — 부분값을 지어내지 않는다.
#[test]
fn partial_or_unparsable_headers_yield_none() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/users/region"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ratelimit-device-data-per-vehicle-limit", "sixty")
                .insert_header("ratelimit-device-data-per-vehicle-remaining", "59")
                .set_body_raw(
                    br#"{"response":{"region":"na"}}"#.to_vec(),
                    "application/json",
                ),
        )
        .mount(&server);

    let c = test_client(&server);
    assert!(c.send_metered(&GetRegion).unwrap().rate_limit.is_none());
}

/// 한 `Client` 로 metered 호출 다음 unmetered 호출을 연달아 해도 두
/// 번째 결과에 첫 번째 값이 새면 안 된다 — 값이 응답 그 자체가 아니라
/// `Client` 위 어딘가의 공유 슬롯에 얹혀 있다면 이 순서에서 정확히
/// 깨진다. `Metered<T>` 를 응답과 함께 반환하는 설계(전역
/// `last_rate_limit()` 슬롯을 두지 않는 이유)가 실제로 지켜지는지
/// 확인하는 유일한 테스트 — 앞의 두 테스트는 매번 새 `Client` 를 만들어
/// 이 교차오염 경로를 아예 거치지 않는다.
#[test]
fn a_prior_metered_calls_headers_do_not_leak_into_a_later_unmetered_call() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .and(path("/api/1/vehicles/5YJ3E1EA1JF000001/vehicle_data"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("ratelimit-device-data-per-vehicle-limit", "60")
                .insert_header("ratelimit-device-data-per-vehicle-remaining", "59")
                .insert_header("ratelimit-device-data-per-vehicle-reset", "57")
                .set_body_raw(
                    br#"{"response":{"vin":"5YJ3E1EA1JF000001"}}"#.to_vec(),
                    "application/json",
                ),
        )
        .mount(&server);
    Mock::given(method("GET"))
        .and(path("/api/1/users/region"))
        .respond_with(ResponseTemplate::new(200).set_body_raw(
            br#"{"response":{"region":"na"}}"#.to_vec(),
            "application/json",
        ))
        .mount(&server);

    let c = test_client(&server);
    let req = GetVehicleData::new("5YJ3E1EA1JF000001", vec![DataEndpoint::ChargeState]).unwrap();

    let first = c.send_metered(&req).unwrap();
    assert!(first.rate_limit.is_some(), "첫 호출은 헤더를 받아야 한다");

    let second = c.send_metered(&GetRegion).unwrap();
    assert!(
        second.rate_limit.is_none(),
        "무과금 호출인데 직전 유료 호출의 rate limit 이 새어 나왔다"
    );
}

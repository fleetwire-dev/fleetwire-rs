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

/// The envelope is stripped and the typed value comes out.
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

/// Parses with or without a charset (measured: successes carry a charset, the 412 does not).
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

/// `send` handles both an array and a scalar inside the envelope — testing with
/// `Vec<VehicleSummary>` rules out the accident of it working only for the single-object
/// `RegionInfo` type.
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

/// The sibling pagination key of the measured fixture reaches Page.
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
    let p = page.pagination.expect("the server sent pagination");
    assert_eq!(p.count, Some(1));
    assert_eq!(p.pages, Some(1));
    assert_eq!(p.current, Some(1));
}

/// Without pagination it is None — distinct from a Some whose fields are all None.
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
        "the list itself is valid, so this must not fail"
    );
    assert!(page.pagination.is_none(), "absence must be None");
}

/// Follow-up to the Task 6 review (Important): unlike `ListVehicles`, `ListInvitations`
/// does not use `passthrough_endpoint!` and writes `assemble` by hand — so the claim that
/// Task 2's pagination plumbing (`Envelope`→`decode`→`send`→`assemble`) is not specific
/// to `ListVehicles` is proven only by actually going through `send`. The unit test that
/// calls `assemble` directly (`api::vehicles::tests::
/// invitations_assemble_preserves_items_and_pagination`) cannot by itself tell whether
/// the send path is wired up — and checking only items would not prove that the sibling
/// `pagination` key really survived, so both are checked.
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

    assert_eq!(page.items.len(), 1, "items must be carried too");
    let p = page
        .pagination
        .expect("the sibling pagination key must survive the trip through send");
    assert_eq!(p.count, Some(30));
    assert_eq!(p.pages, Some(2));
    assert_eq!(p.next, Some(2));
}

/// A response without pagination must still be `None` after going through `send` (a
/// different fact from a `Some` whose fields are all `None`) — added after the review
/// pointed out that this distinction is the one most likely to regress silently.
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
        "the list itself is valid, so this must not fail"
    );
    assert!(page.pagination.is_none(), "absence must be None");
}

/// A passthrough endpoint is unaffected even when pagination arrives.
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

/// page/per_page are carried in the query (regression guard on the request side).
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
        "actual: {q}"
    );
}

/// Fix round 1 (Important 2): a malformed VIN is stopped at `GetVehicle::new` — as a
/// result there is nothing left to call `Client::send` with. "An error happened" alone
/// does not prove "nothing went out" (review finding), so no Mock is registered on the
/// mock server and the request log is checked directly — had any request gone out,
/// `received_requests()` would not be empty.
#[test]
fn malformed_vin_never_reaches_the_wire() {
    let server = MockServer::start();
    // Deliberately mounts no Mock at all — a request going out shows up immediately.
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
        "the server received a request even though construction failed"
    );
}

/// Confirms from **the request itself** that `send` really attaches
/// `Authorization: Bearer <token>` and makes exactly one request — the mechanism, not the
/// value (discipline article 3).
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
    assert_eq!(reqs.len(), 1, "must make exactly one request");
    let auth = reqs[0]
        .headers
        .get("authorization")
        .expect("the Authorization header must be present")
        .to_str()
        .unwrap();
    assert!(auth.starts_with("Bearer "), "actual: {auth}");
}

/// When the server answers 4xx, `send` propagates the `Error` variant that matches that
/// status unchanged.
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
    assert!(matches!(e, Error::Auth { .. }), "actual: {e:?}");
}

/// Tests the no-redirect setting of the agent that `ClientBuilder::build()` really uses.
/// The test of the same nature in `transport.rs` exercises an agent that file builds on
/// its own, so it keeps passing even if `ClientBuilder::build()` itself drops the
/// setting — this test closes that gap (the requirement the brief moved into this task).
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

    assert!(r.is_err(), "must not follow the redirect: {r:?}");
    assert!(
        evil.received_requests().unwrap().is_empty(),
        "the client followed the redirect and sent a request to the evil host \
         — the agent ClientBuilder::build() uses follows redirects"
    );
}

// ------------------------------------------------------------ send_raw

/// **The raw body comes out whole, envelope included.** `send` parses into `Envelope<T>`,
/// hands over only the `response` member and **silently drops** siblings the model does
/// not have (`count` — O13). Drift detection exists to see exactly what gets dropped.
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

    // The envelope is intact.
    assert!(got.value.get("response").is_some(), "{:?}", got.value);
    // A top-level sibling the model does not have is intact — this is why the method
    // exists.
    assert_eq!(got.value.get("count"), Some(&serde_json::json!(1)));
    assert!(got.value.get("pagination").is_some());
    // And the field Tesla **added** is visible.
    assert_eq!(
        got.value["response"][0]["tesla_added_this"],
        serde_json::json!(7)
    );

    // Non-vacuity — receiving the same response through `send` makes `count` **vanish**.
    //    Without this contrast the assertions above only say "the envelope is not
    //    stripped", and nobody checks that the typed path really loses information.
    let typed: Page<VehicleSummary> = c
        .send(&ListVehicles {
            page: None,
            per_page: None,
        })
        .unwrap();
    assert_eq!(typed.items.len(), 1);
    assert!(
        typed.items[0].extra.contains_key("tesla_added_this"),
        "the tolerant type absorbs the new field — so typed cannot see drift"
    );
}

/// **A 4xx must not be returned as a `Value`.** Because `send_raw` returns
/// `serde_json::Value`, handing back an API error body as "successful data" is the most
/// plausible accident. The status gate fires **before** the body is looked at.
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
            "status={status} was returned as a Value"
        );
    }
}

/// 401 and 408 are classified as different errors — evidence that the **same**
/// classification as `send` is used. (A 408 is the normal answer of a sleeping car and it
/// **is billed**.)
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
            .expect("must be an error");
        let kind = match &raw_err {
            Error::Auth { .. } => "auth",
            Error::VehicleOffline { .. } => "offline",
            Error::Api { .. } => "api",
            other => panic!("unexpected classification: {other:?}"),
        };
        assert_eq!(kind, want, "status={status}");
    }
}

/// **Does not follow redirects.** `send_raw` shares `send_inner`, so the agent's
/// no-redirect setting applies unchanged — had a separate path been built, it would break
/// here.
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
    assert!(r.is_err(), "a 3xx was returned as a Value");
    assert!(
        evil.received_requests().unwrap().is_empty(),
        "🔴 send_raw followed the redirect and sent a request to the evil host"
    );
}

/// The `ratelimit-*` headers arrive as `Metered.rate_limit` — T21's rate-limit break
/// uses this value.
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
    let rl = got
        .rate_limit
        .expect("the ratelimit headers were not carried");
    assert_eq!(rl.limit, 60);
    assert_eq!(rl.remaining, 57);

    // Non-vacuity: a response without the headers is `None`, not a made-up value.
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

/// Refuses a content-type that is not JSON — a captive portal's HTML is not swallowed as
/// the "body".
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

/// A body that is not JSON is `Error::Decode` — the same failure shape as `send`.
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

/// **Does not retry.** The default `max_attempts = 1` simply means "no retries".
///
/// **Do not write "prevents double billing" as the rationale for this default — that has
/// been wrong twice.** The first version pointed at 408, and the second moved the same
/// claim onto the *retryable 5xx*. Both are false: [`Error::is_retryable`] admits only
/// `TransportNotSent` and `500|502|503|504`, and on a paid endpoint [`Error::billing`]
/// rules **every one of those** [`crate::Billing::NotBilled`] (PROBE-E in the
/// `RECOVERY_BUDGET` doc: paid 503 + `.max_attempts(3)` → 3 requests,
/// `billing == NotBilled`). A 408 is billed, but it is not a retry candidate in the first
/// place. The real reason the default is 1 is the residual uncertainty that **the §9.2
/// verdict is this crate's model, not a contract with Tesla**.
///
/// So this test uses not 408 but the **retryable** 503 to show that "does not send" is no
/// accident — the same shape as what `default_does_not_retry_even_on_retryable_5xx` does
/// for `send`.
///
/// **Why `GetRegion` + [`Client::dispatches`].** `GetRegion` is region resolution itself,
/// so the pre-flight resolution does not run (`req_resolves_region`), and this call makes
/// just one request, which keeps the count unambiguous. And what is counted is **the
/// number the client dispatched**, not the number the server received — that field's doc
/// records why that boundary was chosen, and what T20 misdiagnosed here.
#[test]
fn send_raw_does_not_retry_by_default() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503).set_body_raw(b"{}".to_vec(), "application/json"))
        .mount(&server);

    let c = test_client(&server); // default builder = max_attempts 1
    let e = c
        .send_raw(&GetRegion)
        .err()
        .expect("the 503 was returned as a Value");
    assert!(
        e.is_retryable(),
        "a 503 must be judged **retryable** — otherwise this test is vacuous"
    );
    assert_eq!(
        c.dispatches(),
        1,
        "even when judged retryable, the default dispatches no second request — a paid \
         5xx is ruled NotBilled by §9.2, so what this default guards against is not \
         'double billing' but 'that verdict being wrong'"
    );
}

/// Builds a fake JWT carrying an scp claim (the signature is any value — it is never
/// verified).
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

/// A refresher that returns a **different** token on every rotation, plus a call counter.
///
/// A different value each time is the point (final fix wave / Important 1). 401 recovery
/// has to be judged not by "did a rotation happen" but by "does the retry carry
/// **different bytes**", and with a refresher that returns the same value the fixed
/// implementation and the broken one (re-sending the refused token) are
/// indistinguishable on the wire.
///
/// The value is built by appending `-rot{n}` to the seed token. Only the JWT's
/// **signature segment** changes, so the `scp` claim (and hence the scope pre-check
/// result) stays the same — the scope tests using this fixture are unaffected by whether
/// a rotation happened.
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

/// A fixture that carries a [`Client`] **together with its token directory guard**.
///
/// One defect is why this bundle exists. If a helper holds the `TempDir` only as a local
/// variable, the guard is dropped the moment the helper returns and the directory is
/// deleted, and the `Client` reading it then dies with
/// `token store: token file not found` (51 tests in this module really did die that way).
/// The guard must live **at least as long as** the `Client` that uses it, and this struct
/// is that constraint pinned down in a type.
///
/// It is `Deref<Target = Client>`, so call sites keep writing `c.send(..)` as before.
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

/// A builder with the mock server as its bootstrap region, plus the token directory guard.
///
/// Do not discard the guard with `_` — the directory that the `Client` built by the
/// builder has to read would vanish on the spot. Take it with a named binding
/// (`let (b, _dir) = …`) and keep it alive to the end of the test body.
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
    // Seeds a **valid** token — its value is `fake_jwt(scopes)` (or the opaque token)
    // unchanged, so the access token carried on later requests and the input to the
    // scope pre-check are identical to the earlier version. `TokenManager` fails its
    // first call with `NotFound` when there is no on-disk file at all (bootstrap login
    // is not this crate's job), so the seeding itself is still needed.
    //
    // **Why the expired token was replaced with a valid one** (final fix wave /
    // Important 1). The earlier version seeded an expired token so that the first call
    // spent a refresh POST. That bootstrap rotation stamps `last_refresh_at` as "just
    // now", so the rotation that 401 recovery forces is always blocked by
    // `MIN_REFRESH_INTERVAL` (60 seconds) — in other words a fixture that **cannot test
    // the fixed 401 recovery** (before the fix, recovery "succeeded" through disk
    // adoption, which kept this defect hidden). In production too, the normal scenario
    // is an 8-hour token hitting a 401 partway through its life, not "a 401 right after
    // a rotation". Seeding a valid token makes the first call an **adoption** (0 POSTs,
    // `last_refresh_at` unset) and so models that normal scenario as it is. The
    // rotation-suppression gate itself is covered solely by the `auth` layer tests.
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
    // T7: puts the mock server's host on the allowlist. `allow_region_host` alone is
    // not enough — the mock server is plaintext HTTP on an arbitrary port, so it trips the
    // https and default-port checks. Hence the origin hatch (`#[cfg(test)]`) is used
    // along with it. Both lines are **test-only**, and the default of
    // `allow_region_host` is closed (`TESLA_REGION_HOSTS` only) — do not flip that
    // default "because it is convenient". The failure mode is credential exfiltration.
    let b = Client::builder(tokens)
        .bootstrap_region(url.clone())
        .allow_region_host(&host)
        .allow_region_origin_for_tests(&url)
        .request_timeout(std::time::Duration::from_secs(5));
    (b, calls, dir)
}

/// A builder with only the bootstrap pointed at the mock server, **without** the origin
/// hatch — for the adoption/refusal tests that must pass through the production trust
/// boundary (`validate_region_url`) as it is.
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

/// A client that counts **how many times** 401 recovery spent a refresh.
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

/// Used by T9's scope pre-check (`observe_scopes`) tests — a client that issues a JWT
/// carrying an arbitrary set of scopes.
fn test_client_with_scopes(server: &MockServer, scopes: &[&str]) -> TestClient {
    let (b, _, dir) = test_client_builder_with(server, Some(scopes));
    TestClient {
        client: b.build().unwrap(),
        _dir: dir,
    }
}

/// Used by T9's fail-open test for an opaque (non-JWT) token.
fn test_client_with_opaque_token(server: &MockServer) -> TestClient {
    let (b, _, dir) = test_client_builder_with(server, None);
    TestClient {
        client: b.build().unwrap(),
        _dir: dir,
    }
}

/// A client whose bootstrap happens through a **rotation** — that is, a setup where
/// `last_refresh_at` is stamped as "just now", so the later 401-recovery rotation is
/// blocked by `MIN_REFRESH_INTERVAL`.
///
/// The default fixture (`test_client_builder_with`) seeds a valid token to avoid this
/// situation on purpose (see the comment there). Only tests that exercise the path where
/// rotation suppression and a 401 overlap use this setup.
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

/// A test-only endpoint that is **the same as `GetRegion` in every respect, but paid**.
///
/// The three GETs this crate exposes today are all `Pricing::NotCharged`, which makes
/// every test asserting on `billing()` vacuous (`NotCharged` is `NotBilled` under any
/// implementation). Instead of waiting for A4's `vehicle_data`, a paid endpoint is set up
/// here — `FleetGet` is sealed, so this is possible only inside the crate, and the path
/// is kept the same as `GetRegion` so that pre-flight region resolution does not cut in
/// (`req_resolves_region` decides by path).
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

/// Creates the state where another request has already bumped the generation (the §5.1
/// concurrent-401 scenario).
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

// --- trust boundary ------------------------------------------------

/// Refuses every malicious region URL. **If even one passes, credentials leak.**
///
/// Three were added to the brief's six — with the brief's list alone, the mutation that
/// "switches to suffix matching" (M2) **survives**:
/// - `tesla.com.evil.net` is refused by `ends_with("tesla.com")` and also by
///   `ends_with(<allowlist entry>)` → it kills neither M2 variant.
/// - `evil-fleet-api.…tesla.com` contains an allowlist entry **as a suffix**, so it kills
///   the `ends_with(a)` variant.
/// - `evil.tesla.com` and `nottesla.com` kill the `ends_with("tesla.com")` variant (the
///   latter does not even have a dot boundary).
///
/// Discipline article 1: a rule that forbids X needs an input that fails only when X is
/// done.
#[test]
fn rejects_every_untrusted_region_url() {
    let allowed = default_allowlist();
    for bad in [
        "http://fleet-api.prd.na.vn.cloud.tesla.com", // plaintext
        "https://evil.example",                       // external host
        "https://fleet-api.prd.na.vn.cloud.tesla.com.evil.net", // suffix appended
        "https://evil-fleet-api.prd.na.vn.cloud.tesla.com", // prefix added (bypasses ends_with)
        "https://evil.tesla.com",                     // *.tesla.com bypass
        "https://nottesla.com",                       // tesla.com suffix without a dot boundary
        "https://user:pw@fleet-api.prd.na.vn.cloud.tesla.com", // userinfo
        "https://fleet-api.prd.na.vn.cloud.tesla.com:8443", // non-standard port
        "https://fleet-api.prd.na.vn.cloud.tesla.com/evil/path", // path injection
        "https://fleet-api.prd.na.vn.cloud.tesla.com/?x=1", // query
        "https://fleet-api.prd.na.vn.cloud.tesla.com/#f", // fragment
        "not a url",                                  // unparsable
        "",                                           // empty string
    ] {
        assert!(
            validate_region_url(bad, &allowed).is_err(),
            "must not pass: {bad}"
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
        // Letter case is the same host as far as DNS goes — `Url` lowercases it, so
        // this must pass. Refusing it would block a legitimate response as a false
        // positive.
        "https://FLEET-API.PRD.EU.VN.CLOUD.TESLA.COM",
        // An explicit :443 is the scheme's default port, so `port()` is None.
        "https://fleet-api.prd.na.vn.cloud.tesla.com:443/",
    ] {
        assert!(
            validate_region_url(ok, &allowed).is_ok(),
            "must not be refused: {ok}"
        );
    }
}

/// The China region is **not** on the default allowlist (design O11 — no evidence).
/// A user who needs it opens it with `allow_region_host`. Whether the default is open or
/// closed is pinned by value — it fails if flipped for the sake of convenience.
#[test]
fn china_region_host_is_closed_by_default_and_opt_in_only() {
    const CN: &str = "https://fleet-api.prd.cn.vn.cloud.tesla.cn";
    let server = MockServer::start();

    assert!(
        validate_region_url(CN, &default_allowlist()).is_err(),
        "🔴 a host without evidence is on the default allowlist"
    );

    let (b, _dir) = test_client_builder(&server);
    let opened = b
        .allow_region_host("fleet-api.prd.cn.vn.cloud.tesla.cn")
        .build()
        .unwrap();
    assert!(opened.adopt_region_url(CN).is_ok());

    // Opening it opens **the host only** — a plaintext downgrade is still blocked.
    assert!(
        opened
            .adopt_region_url("http://fleet-api.prd.cn.vn.cloud.tesla.cn")
            .is_err(),
        "🔴 allow_region_host also lifted the https requirement"
    );
}

/// The production path as it is: a region URL that passes validation is adopted and the
/// base changes. (This client does **not** use the test-only origin hatch — it gets
/// through only on a real `TESLA_REGION_HOSTS` entry. No request is sent to the adopted
/// host, so nothing goes out to the network.)
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
    assert_eq!(before.host_str(), Some("127.0.0.1"), "bootstrap at first");

    let got = c.resolve_region().unwrap();
    assert_eq!(got.as_str(), "https://fleet-api.prd.eu.vn.cloud.tesla.com/");
    let (g1, after) = c.current_base();
    assert_eq!(
        after.host_str(),
        Some("fleet-api.prd.eu.vn.cloud.tesla.com"),
        "the base must change after adoption"
    );
    assert!(g1 > g0, "adoption bumps the generation");

    // The second call uses the cache as is — no further re-resolution request goes out.
    c.resolve_region().unwrap();
    let region_calls = server
        .received_requests()
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/api/1/users/region")
        .count();
    assert_eq!(region_calls, 1, "once resolved, it does not ask again");
}

/// A refused region URL is **not adopted** and the bootstrap is kept.
/// What is checked is not "an error happened" but that the base did not change.
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
    assert!(matches!(e, Error::Auth { .. }), "actual: {e:?}");

    let (g1, base) = c.current_base();
    assert_eq!(base.host_str(), Some("127.0.0.1"), "🔴 the base changed");
    assert_eq!(
        g1, g0,
        "a refusal does not bump the generation (failures are not cached)"
    );
}

/// If the region response has no `fleet_api_base_url` at all, there is nothing to adopt.
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

// --- 412 recovery -----------------------------------------------------

/// fix round 1 (review Important 1): **an account in a non-bootstrap region does not
/// burn a 412 on its first call.**
///
/// The mechanism is measured directly with two mock servers — A = bootstrap (answers
/// only region resolution and refuses `/vehicles` with 412), B = the real region (200).
/// With pre-flight resolution, `/vehicles` goes to B from the start and **A never
/// receives a single `/vehicles` request**. Remove the pre-flight resolution and A gets
/// one 412, so this assertion dies (mutant M14).
///
/// In A3 that 412 costs $0, but per design §4.3 it **is billed** the moment A4 brings in
/// `vehicle_data` — and it lands not on the unbilled region endpoint but on the very
/// endpoint the caller asked for.
#[test]
fn resolves_region_before_the_first_request_so_no_412_is_burned() {
    let region_server = MockServer::start(); // B: the real region
    Mock::given(method("GET"))
        .and(path("/api/1/vehicles"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(br#"{"response":[]}"#.to_vec(), "application/json"),
        )
        .mount(&region_server);

    let boot = MockServer::start(); // A: bootstrap (the wrong region)
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
        "🔴 a real request went to the bootstrap host — without pre-flight resolution \
         this 412 is billed in A4"
    );
    let served = region_server
        .received_requests()
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/api/1/vehicles")
        .count();
    assert_eq!(
        served, 1,
        "the real request goes to the resolved region exactly once"
    );
}

/// 412 → one region re-resolution → re-send. **The mechanism is counted** (request
/// count).
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
    assert_eq!(vehicles, 2, "original request + 412 retry must be 2");
    // fix round 1: 1 pre-flight resolution + 1 re-resolution for 412 recovery = 2. This
    // is exactly the budget design §7 set aside (at most 2), and it is reached only with
    // the pre-flight resolution in place.
    assert_eq!(region, 2, "1 pre-flight + 1 412 re-resolution = exactly 2");
    assert_eq!(
        reqs[0].url.path(),
        "/api/1/users/region",
        "region resolution must go out **before** the first request"
    );
}

/// The mock path of `vehicle_data` — shared by the pair of tests that forbid paid
/// recovery.
const PAID_VIN: &str = "5YJ3E1EA7JF000001";

fn paid_req() -> crate::api::vehicles::GetVehicleData {
    crate::api::vehicles::GetVehicleData::new(
        PAID_VIN,
        vec![crate::api::vehicles::DataEndpoint::ChargeState],
    )
    .expect("synthetic VIN is path-safe")
}

/// A mock that serves a **resolvable** region — the shared precondition of the paid/free
/// test pair.
///
/// Without it the paid test passes **by accident**. When `resolve_region` fails, the 412
/// arm's `if …is_ok()` is false and no re-fire happens at all — so "it did not go out
/// because it is paid" and "it could not go out because the region did not resolve" are
/// indistinguishable. The first-version harness of A5c-2a T23 was in exactly that state,
/// and the review caught it. Here the region is made to **resolve**, so the re-fire
/// condition really holds, and then the test checks that only the paid one is blocked.
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

/// **Paid 412: no re-fire, yet recovery happens** (T23 fix round 2).
///
/// # Two things this test pins — both are required
///
/// 1. **One paid request per `send`.** A 412 is evidence that the first request reached
///    the server and got an answer (`Error::billing` → `Billed`), and a re-fire is simply
///    a second charge.
/// 2. **Self-healing still works.** The first version (fix round 1) made `may_recover`
///    itself `false` and so skipped even `invalidate_region`/`resolve_region`, which
///    meant **a consumer that calls only paid endpoints never heals** — every call hits a
///    412 on the same stale region and each one is `Billed`. Trying to prevent double
///    billing had in effect created unbounded billing.
///
/// # Why **two** mock servers — the first version's fixture was vacuous
///
/// Stack `412` (`up_to_n_times(1)`) + `200` on one server and the second `send` gets a
/// 200 **regardless of recovery**, because the mock's 412 has been used up — so
/// "recovery worked" and "the mock ran dry" are indistinguishable. This was confirmed by
/// measurement (while writing fix round 2): `send2_ok=true` even on the suppressed
/// version.
///
/// So a region switch is produced **for real**. `A` always answers 412 and `B` answers
/// 200. `A`'s region response points to **A the first time and B from then on**. The base
/// of the second `send` changes to `B` only if recovery happened, so the success of
/// `send #2` is itself the evidence of recovery.
///
/// The free counterpart `region_412_triggers_one_reresolve_and_retry` has
/// `vehicles == 2` within a single `send`. Keeping the two tests side by side is the
/// control group along the pricing axis.
#[test]
fn a_412_on_a_paid_endpoint_is_not_re_fired() {
    let a = MockServer::start();
    let b = MockServer::start();
    let paid_path = format!("/api/1/vehicles/{PAID_VIN}/vehicle_data");

    // A: the paid endpoint **always** answers 412.
    Mock::given(method("GET"))
        .and(path(paid_path.clone()))
        .respond_with(ResponseTemplate::new(412).set_body_raw(
            br#"{"error":"must be registered in the current region"}"#.to_vec(),
            "application/json",
        ))
        .mount(&a);
    // A's region response: A itself the first time, B afterwards.
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
    // B: the paid endpoint succeeds.
    Mock::given(method("GET"))
        .and(path(paid_path.clone()))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(br#"{"response":{}}"#.to_vec(), "application/json"),
        )
        .mount(&b);

    let b_url: crate::Url = b.uri().parse().unwrap();
    // This test already uses the name `b` for a mock server — binding the builder as `b`
    // would shadow it.
    let (builder, _dir) = test_client_builder(&a);
    let c = builder
        .allow_region_host(b_url.host_str().unwrap())
        .allow_region_origin_for_tests(&b_url)
        .build()
        .unwrap();
    let count = |reqs: &[fleetwire_testkit::Request], p: &str| {
        reqs.iter().filter(|r| r.url.path() == p).count()
    };

    // --- send #1: A answers 412. No re-fire. ------------------------------------------
    // `unwrap_err()` cannot be used — `VehicleData` is deliberately not `Debug` (the
    //    type keeps PII from leaking into a panic message).
    let e = match c.send(&paid_req()) {
        Ok(_) => panic!("send #1 must fail: A always answers 412"),
        Err(e) => e,
    };
    assert!(
        matches!(&e, Error::Api { status, .. } if status.as_u16() == 412),
        "the 412 must surface unchanged: {e:?}"
    );
    assert_eq!(
        e.billing(),
        crate::Billing::Billed,
        "a 412 on a paid endpoint is ruled billed — which is why a re-fire is a double charge"
    );
    assert_eq!(
        count(&a.received_requests().unwrap(), &paid_path),
        1,
        "🔴 send #1 sent the paid request twice = double billing"
    );

    // --- send #2: recovery happened, so the base moved to B and this must succeed. ----
    assert!(
        c.send(&paid_req()).is_ok(),
        "🔴🔴 a consumer that calls only paid endpoints cannot self-heal. If the 412 arm \
         skips invalidate_region/resolve_region, the base stays on A forever, every \
         call hits a 412 and each one is Billed — preventing double billing turns \
         into unbounded billing"
    );

    let ra = a.received_requests().unwrap();
    let rb = b.received_requests().unwrap();
    assert_eq!(
        (count(&ra, &paid_path), count(&rb, &paid_path)),
        (1, 1),
        "1 on A (412) + 1 on B (200) — that is, **exactly 1 per send**, 0 re-fires"
    );
    // Non-vacuity: the region **really was** re-resolved (1 pre-flight + 1 412 recovery).
    //    With 1 it would be indistinguishable from "no re-fire because the region did
    //    not resolve".
    assert_eq!(
        count(&ra, "/api/1/users/region"),
        2,
        "1 pre-flight + 1 412-recovery re-resolution. Recovery itself must happen on paid too"
    );
}

/// **Paid 401: no re-fire, yet the rotation happens** — the same argument as its 412
/// counterpart.
///
/// A 401 is `Error::Auth { pricing: Some(Data) }` and design §9.2 rules it billed. But
/// `reject_generation` is **still called** — it is the only means of carrying *different
/// bytes* on the next call, and skipping that too would leave a paid consumer buying 401s
/// forever with the same stale token (what set off fix round 2).
///
/// # 401 is answered **by token value** — the same non-vacuity argument as the 412 one
///
/// Answering 401 just once via `up_to_n_times(1)` would let the second `send` get a 200
/// regardless of rotation. So here **only a request carrying the originally seeded
/// token** gets a 401, and any other token gets a 200. `send #2` succeeds only if the
/// rotation really happened.
#[test]
fn a_401_on_a_paid_endpoint_is_not_re_fired() {
    let server = MockServer::start();
    let paid_path = format!("/api/1/vehicles/{PAID_VIN}/vehicle_data");
    mount_resolvable_region(&server);

    let (c, refresh_calls) = test_client_counting_refreshes(&server);
    // Bootstrap is a disk adoption, so 0 rotations — this value is "the token that will
    // be refused".
    let (_, seed) = c.tokens().access_token_with_generation().unwrap();
    assert_eq!(
        refresh_calls.load(Ordering::SeqCst),
        0,
        "fixture precondition: bootstrap is an adoption, so there must be no rotation"
    );

    // Only a request carrying that token gets a 401. Any other token gets a 200.
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

    // --- send #1: receives the 401 as is. No re-fire. ---------------------------------
    let e = match c.send(&paid_req()) {
        Ok(_) => panic!("send #1 must fail: the seeded token is refused"),
        Err(e) => e,
    };
    assert!(matches!(e, Error::Auth { .. }), "actual: {e:?}");
    assert_eq!(
        e.billing(),
        crate::Billing::Billed,
        "design §9.2 rules a paid 401 billed"
    );
    assert_eq!(
        count(&server.received_requests().unwrap(), &paid_path),
        1,
        "🔴 send #1 sent the paid request twice = double billing"
    );

    // --- send #2: the generation was rejected, so it must succeed with a new token. ---
    assert!(
        c.send(&paid_req()).is_ok(),
        "🔴🔴 a consumer that calls only paid endpoints cannot self-heal. If the 401 arm \
         skips reject_generation, every call hits a 401 with the same stale token and \
         each one is Billed"
    );

    let reqs = server.received_requests().unwrap();
    assert_eq!(
        count(&reqs, &paid_path),
        2,
        "2 paid requests over two sends — that is, **exactly 1 per send**"
    );
    // Non-vacuity: the rotation **really did** happen exactly once.
    assert_eq!(
        refresh_calls.load(Ordering::SeqCst),
        1,
        "the forced rotation of 401 recovery must happen exactly once on paid too"
    );
    // And did the token on the wire really change — the rotation counter alone cannot
    // rule out "it rotated but sent the same value again".
    let bearer = |i: usize| {
        reqs.iter()
            .filter(|r| r.url.path() == paid_path)
            .nth(i)
            .expect("paid request")
            .headers
            .get("authorization")
            .expect("Authorization header")
            .to_str()
            .unwrap()
            .to_owned()
    };
    assert_eq!(bearer(0), format!("Bearer {seed}"), "test precondition");
    assert_ne!(
        bearer(0),
        bearer(1),
        "🔴 the second request carried the very token that had just received the 401"
    );
}

/// A 412 that keeps coming is still not an infinite loop — the request count is capped.
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
        // `.err().unwrap()`: `VehicleSummary` holds a VIN and is deliberately not
        // `Debug` — `unwrap_err()` requires that bound.
        .err()
        .unwrap();
    assert!(
        matches!(&e, Error::Api { status, .. } if status.as_u16() == 412),
        "actual: {e:?}"
    );

    let reqs = server.received_requests().unwrap();
    let vehicles = reqs
        .iter()
        .filter(|r| r.url.path() == "/api/1/vehicles")
        .count();
    assert_eq!(
        vehicles, 2,
        "the 412 retry is limited to one — an infinite loop would blow this value up"
    );
}

/// When re-resolution **fails**, no futile re-send goes to the same host — the original
/// 412 is surfaced unchanged (on A4's paid endpoints this one request is itself a charge).
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
    // Re-resolution trips the trust boundary and is refused.
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
        // `.err().unwrap()`: `VehicleSummary` holds a VIN and is deliberately not
        // `Debug` — `unwrap_err()` requires that bound.
        .err()
        .unwrap();
    assert!(
        matches!(&e, Error::Api { status, .. } if status.as_u16() == 412),
        "actual: {e:?}"
    );

    let reqs = server.received_requests().unwrap();
    let vehicles = reqs
        .iter()
        .filter(|r| r.url.path() == "/api/1/vehicles")
        .count();
    assert_eq!(vehicles, 1, "with no region to adopt there is no re-send");
}

/// fix round 1: even when pre-flight resolution **fails**, the total number of region
/// resolutions does not exceed 2.
///
/// This combination (pre-flight resolution fails → 401 → 412) is the only scenario that
/// kills the mutant placing the pre-flight resolution **inside** the retry loop (M15):
/// the cache stays empty, so the pre-flight resolution runs again on every attempt, and
/// with the 412 recovery resolution on top it exceeds §7's budget of 2. The other tests,
/// where pre-flight resolution **succeeds**, have a filled cache by the second attempt
/// and cannot tell this mutant apart.
#[test]
fn region_resolution_stays_within_budget_when_preflight_fails() {
    let server = MockServer::start();
    // Region resolution always trips the trust boundary and is refused → the cache stays
    // empty.
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
    assert_eq!(region, 2, "1 pre-flight + 1 412 recovery = cap of 2 (§7)");
    assert_eq!(
        vehicles, 2,
        "original request + 401 retry. No re-send for the 412: re-resolution failed"
    );
}

/// Region cache invalidation takes effect **only for the generation the caller saw**.
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
    let g1 = c.region_generation(); // not resolved yet
    c.resolve_region().unwrap(); // one resolution → generation goes up
    let g2 = c.region_generation();
    assert!(g2 > g1);

    // A late-arriving 412 attempts invalidation with a **stale** generation.
    c.invalidate_region(g1);
    assert_eq!(
        c.region_generation(),
        g2,
        "invalidation by a stale generation must not take effect"
    );
    // The cache is intact, so no re-resolution request goes out.
    c.resolve_region().unwrap();

    let region_calls = server
        .received_requests()
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/api/1/users/region")
        .count();
    assert_eq!(
        region_calls, 1,
        "if the invalidation wrongly takes effect, one more re-resolution happens"
    );
}

/// Invalidating with the current generation really clears it — this closes the vacuity
/// where the test above would also pass with "invalidation never takes effect".
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
        "invalidation by the current generation must force a re-resolution"
    );
}

// --- 401 recovery -----------------------------------------------------

/// 401 → one forced rotation → one re-send with a **different token**. A second 401 ends
/// it.
///
/// # What this test replaced (final fix wave / Important 1)
///
/// The earlier version (`auth_401_invalidates_its_own_generation_then_gives_up`) **pinned
/// the broken behaviour as the expected value**: "refresh happens exactly once — 401
/// recovery **adopts** the disk token that was just saved, so it spends no further POST".
/// That sentence being true means the retry **re-sent the very bytes that had just been
/// refused** (a 401 does not move `expires_at`, so the same file stays `is_fresh`). It
/// was a path that reliably threw away one request and one charge with zero chance of
/// recovering, and it was not the recovery design §5.1 promised.
///
/// # The verdict is made on **the token value on the wire**
///
/// "An error happened or not" and the request count are not enough — the broken
/// implementation also sends 2 requests. So the `Authorization` headers of the two
/// requests are compared directly:
///
/// - A mutant where the rejection itself is a no-op → cache hit → the **same** header →
///   dies.
/// - A mutant that only clears the cache (the pre-fix behaviour) → disk adoption → the
///   **same** header → dies. The `refresh_calls == 1` assertion dies along with it (it
///   becomes 0).
/// - A mutant where the rejection skips the generation comparison does not die here —
///   that one is caught by the opposite counterpart,
///   `concurrent_401_with_stale_generation_does_not_invalidate`.
/// - A mutant that spends two rotations → `refresh_calls == 2` → dies.
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
    // Bootstrap is a disk adoption (the fixture seeds a valid token) — 0 rotations.
    let (gen_before, token_before) = c.tokens().access_token_with_generation().unwrap();
    assert_eq!(
        refresh_calls.load(Ordering::SeqCst),
        0,
        "fixture precondition: bootstrap is an adoption, so there must be no rotation"
    );

    let e = c.send(&GetRegion).unwrap_err();
    assert!(matches!(e, Error::Auth { .. }), "actual: {e:?}");

    let reqs = server.received_requests().unwrap();
    assert_eq!(reqs.len(), 2, "original request + 401 retry = 2");
    let bearer = |i: usize| {
        reqs[i]
            .headers
            .get("authorization")
            .expect("Authorization header")
            .to_str()
            .unwrap()
            .to_owned()
    };
    assert_eq!(
        bearer(0),
        format!("Bearer {token_before}"),
        "the first request must carry the token to be refused (test precondition)"
    );
    assert_ne!(
        bearer(1),
        bearer(0),
        "🔴 the retry re-sent **the very token that just received the 401** — that is \
         waste, not recovery (in A4 it also wastes a charge and a rate limit slot)"
    );
    assert_eq!(
        refresh_calls.load(Ordering::SeqCst),
        1,
        "401 recovery must spend **exactly one** rotation \
         (0 = the rejection was neutralised by adoption, 2+ = over budget)"
    );
    let gen_after = c.tokens().access_token_with_generation().unwrap().0;
    assert!(
        gen_after > gen_before,
        "the cache was not actually replaced (before={gen_before} after={gen_after})"
    );
}

/// Even when rotation suppression (`MIN_REFRESH_INTERVAL`) blocks recovery, **the fact
/// that the 401 was billed** is reported (final fix wave / Important 2).
///
/// # Why this combination
///
/// Iteration 1 received a real 401 → had it been a paid (`Pricing::Data`) endpoint, that
/// round trip **was billed**. If iteration 2's token layer is blocked by the minimum
/// rotation interval, the result is `Error::Auth { pricing: None }` →
/// `billing() == NotBilled`. Returning that as is would (a) under-report the spend and
/// (b) describe an event that was really the server refusing authentication as "token
/// refresh suppressed". So **the preceding response error** takes priority.
///
/// # This test's premise **has changed** (T23 fix round 2)
///
/// The earlier doc said *"only 1 Fleet request ⇒ the rotation was blocked by the
/// interval"*. A paid 401 **no longer re-fires, so that inference is now false** — the
/// reason for 1 is pricing. The reviewer caught it by measurement: this test passed, but
/// it was passing **for a different reason**, and the
/// `prior_response_error.unwrap_or(e)` mutant **survived** here.
///
/// So the role was split in two:
///
/// - **This test** — *"a paid 401 surfaces as `Billed`, with the server's wording
///   intact"*. It pins that, with no re-fire, there is no room at all for the token layer
///   to cut in.
/// - [`a_suppressed_recovery_prefers_the_401_over_the_token_layer_error`] — the **free**
///   path that really steps on `unwrap_or`. That one kills the mutant.
///
/// # Why it is not vacuous
///
/// The crate's three real GETs are all `NotCharged`, so `billing()` is `NotBilled` under
/// any implementation — asserting on that would be **vacuous**. So a `Pricing::Data`
/// endpoint is set up just for this test (`FleetGet` is sealed, so this is possible only
/// **inside** the crate).
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
        "a paid 401 is not re-fired — 2 would itself be double billing"
    );
    assert_eq!(
        e.billing(),
        crate::pricing::Billing::Billed,
        "🔴 the token layer error erased the 401's billing — spend is under-reported: {e:?}"
    );
    let msg = e.to_string();
    assert!(
        msg.contains("invalid authentication"),
        "described as some other event rather than the server's auth refusal: {msg}"
    );
    assert!(
        !msg.contains("rotation interval"),
        "🔴 the auth refusal is misdescribed as 'rotation suppression': {msg}"
    );
}

/// **The retry's token layer error must not overwrite the preceding 401** — what
/// `unwrap_or` does (added in T23 fix round 2).
///
/// # Why a new test was needed
///
/// The only test guarding `prior_response_error.unwrap_or(e)` was
/// [`a_suppressed_recovery_refresh_still_reports_the_401s_billing`], but that test's
/// endpoint is **paid**, so since fix round 1/2 it no longer steps on the `continue` of
/// the 401 recovery arm → it **never reaches** the `unwrap_or` line. The reviewer
/// demonstrated the **survival** with the `unwrap_or(e)` → `e` mutant.
///
/// This test really steps on that line with a **free** endpoint: 401 → generation
/// rejected → retry attempted → iteration 2's token layer is blocked by
/// `MIN_REFRESH_INTERVAL` and fails → what is returned then must be **the server's 401**.
///
/// Being free, `billing()` is `NotBilled` on both sides and cannot be used for the
/// verdict. The verdict is made on **the message** — the mutant yields "rotation
/// interval" and the original yields "invalid authentication". That difference is
/// exactly the operator misreading *"the auth refusal is misdescribed as rotation
/// suppression"*.
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
    // `GetRegion` — it is **free**. So the 401 recovery arm steps on `continue` and
    //    really reaches the `unwrap_or` line.
    let e = c.send(&GetRegion).unwrap_err();

    // Precondition: the rotation was blocked by the interval, so the retry never went
    // out on the wire. (Had it not been blocked, there would be 2 requests and this
    // would no longer be the path this test targets.)
    assert_eq!(
        server.received_requests().unwrap().len(),
        1,
        "test precondition: the interval must block the rotation so no retry goes out"
    );
    let msg = e.to_string();
    assert!(
        msg.contains("invalid authentication"),
        "🔴 the retry's token layer error overwrote the server's 401 — the operator \
         would read an auth refusal as 'rotation suppression': {msg}"
    );
    assert!(
        !msg.contains("rotation interval"),
        "🔴 the auth refusal is misdescribed as 'rotation suppression': {msg}"
    );
}

/// A 401 responder that **signals that the request has arrived**.
///
/// fix round 1 (review Minor 4): the previous version relied on the **timing
/// assumption** that "send grabs the token within 80ms". Here a signal is sent over a
/// channel at the moment `respond` is called — that is, when the request has really
/// arrived at the server and therefore **the token has already gone out**. The side that
/// bumps the generation waits for that signal, so the order "token obtained → generation
/// bumped" is guaranteed **causally**.
///
/// Only one timing dependence remains, and it is of a different nature: the generation
/// bump has to finish within the response delay. The response leaves that margin by
/// delaying after it sends the signal. That work is only an in-process mutex + local file
/// I/O, so it involves no waiting on the network.
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

/// When the generation differs, it re-sends immediately **without invalidating
/// anything** (design §5.1).
///
/// The brief's original draft called `bump_generation_out_of_band` **before** send, but
/// then send goes out holding the already-bumped generation, so the "generation
/// mismatch" branch itself never runs — and on top of that the refresh count, which was
/// the verdict metric, does not move under any implementation (even with the cache
/// cleared, recovery happens by **adopting** the disk token and no refresh POST goes
/// out). In other words the original draft was vacuous twice over.
///
/// Here (1) the generation is bumped after the request's **arrival signal** is received
/// (the order is causal), and (2) the verdict is made on the **token generation**, not
/// the refresh count — invalidating unconditionally makes the retry adopt the disk token
/// and bump the generation once more, so that mutant (M12) dies.
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
    // Obtain the first token up front so that send starts out at generation G.
    let (g0, _) = c.tokens().access_token_with_generation().unwrap();
    let before_refreshes = refresh_calls.load(Ordering::SeqCst);

    let (got, gen_after_bump) = std::thread::scope(|sc| {
        let sending = sc.spawn(|| c.send(&GetRegion));
        let c_ref = &c;
        let racing = sc.spawn(move || {
            // **After the request has arrived** at the server — that is, after the token
            // has already gone out — another request bumps the generation.
            arrived_rx
                .recv()
                .expect("the 401 request must arrive at the server");
            bump_generation_out_of_band(c_ref);
            c_ref.tokens().access_token_with_generation().unwrap().0
        });
        (sending.join().unwrap(), racing.join().unwrap())
    });

    // This one line prevents a **false pass** (final fix wave / Minor 6).
    // If an extreme scheduling delay lets `send` finish recovery **before** the
    // generation bump, the two assertions below both pass while the "generation
    // mismatch" branch under test never ran even once (if recovery bumped the generation
    // through cache adoption, `gen_now == gen_after_bump` holds as well). Pinning the
    // bump to exactly one step tells a starved bump apart from the normal order.
    assert_eq!(
        gen_after_bump,
        g0 + 1,
        "the out-of-band bump is not the only thing that bumped the generation — 401 \
         recovery cut in first, and then this test never runs the branch it targets \
         (g0={g0} after_bump={gen_after_bump})"
    );
    assert_eq!(got.unwrap().region.as_deref(), Some("na"));
    let gen_now = c.tokens().access_token_with_generation().unwrap().0;
    assert_eq!(
        gen_now, gen_after_bump,
        "once the generation has already been bumped, 401 recovery **must not touch** \
         the cache — invalidating unconditionally makes the retry adopt the disk token \
         and bump the generation again"
    );
    assert_eq!(
        refresh_calls.load(Ordering::SeqCst),
        before_refreshes,
        "no further refresh POST may be spent either"
    );
}

/// The 401 retry and the 412 retry do not reset each other — Fleet requests are capped at
/// 3 in total.
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
        "original request + 401 retry + 412 retry = cap of 3. \
         If the flags reset each other, this value grows"
    );
    // fix round 1: even on the worst path, region resolution does not exceed 2 (the §7
    // budget). With the pre-flight resolution **inside** the loop, the 401 retry spends
    // one more and it becomes 3.
    assert_eq!(
        region, 2,
        "region resolution is 2 even at worst — 1 pre-flight + 1 412 recovery"
    );
}

/// The account UUID in the 412 body does not remain in the error message.
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
        "🔴 the account UUID was exposed: {rendered}"
    );
    assert!(
        rendered.contains("[REDACTED]"),
        "a marker that it was redacted must remain: {rendered}"
    );
}

// --- T8: opt-in retry ---------------------------------------------------

/// By default there is no retry even on a 5xx (parent spec §11 non-goal).
#[test]
fn default_does_not_retry_even_on_retryable_5xx() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503).set_body_raw(b"{}".to_vec(), "application/json"))
        .mount(&server);

    let c = test_client(&server); // default builder = max_attempts 1
    let e = c.send(&GetRegion).unwrap_err();
    assert!(e.is_retryable(), "a 503 must be judged **retryable**");

    let reqs = server.received_requests().unwrap();
    assert_eq!(
        reqs.len(),
        1,
        "even when judged retryable, the default does not send again"
    );
}

/// With max_attempts(3) turned on, only 500, 502, 503 and 504 are retried.
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
            "status={status} must be sent {expected_sends} time(s)"
        );
    }
}

/// Fix round 1 (Important 1): the 401/412 recovery budget is **shared across the whole
/// `send()` call** and is not handed out afresh for each opt-in retry attempt.
///
/// The responses are scripted in the order 401 → 503 → 401 → 503. The **shared (fixed)**
/// implementation: request 1 (401) → recovery spent (`auth_retry_used=true`, immediate
/// re-send at no charge) → request 2 (503, from here on the opt-in retry decision) →
/// backoff → request 3 (401 again, but the budget is already spent, so there is no
/// recovery and it ends as `Error::Auth` as is) — **3 in total**.
///
/// Had it been the implementation from **before** the fix (each opt-in retry getting its
/// own fresh 401/412 budget): request 1 (401, fresh budget)→recovery→request 2 (503,
/// that "attempt" ends)→backoff→request 3 (budget reset afresh, 401 recovered
/// again)→request 4 (503, that "attempt" ends)→backoff→request 5 (off the script,
/// catch-all) — **5 in total**. The split is exactly 3 versus 5, so if this value ever
/// reverts it shows up right here.
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
    // A safety net for requests beyond the script (the normal path should never reach
    // it, and an unlimited catch-all is put last so that, if it is reached, the count
    // assertion below fails cleanly instead of on an unmatched request).
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

    assert!(matches!(e, Error::Auth { .. }), "actual: {e:?}");
    let reqs = server.received_requests().unwrap();
    assert_eq!(
        reqs.len(),
        3,
        "🔴 if the 401 recovery budget is handed out afresh per retry attempt (the \
         pre-fix bug), 5 go out — the shared (fixed) implementation sends exactly 3: \
         1 401 recovery + 1 503 retry + the second 401 (no recovery, ends)"
    );
}

/// `TransportUnknown` is not retried even when retries are turned on.
///
/// **For this variant alone, "retry = double billing" really does hold** — though as a
/// possibility, not a certainty. A `TransportUnknown` on a paid endpoint is the only
/// shape that [`Error::billing`] rules [`crate::Billing::Unknown`] (there is no knowing
/// whether the server already processed and billed it), so a retry **may be a second
/// charge**. That is a different rationale from the retryable 5xx (ruled `NotBilled` by
/// §9.2), so do not mix up the reasons for the two tests.
#[test]
fn transport_unknown_is_never_retried_even_when_enabled() {
    let server = MockServer::start();
    // Delay the response to trigger the request timeout → TransportUnknown.
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

    assert!(matches!(e, Error::TransportUnknown { .. }), "actual: {e:?}");
    assert!(!e.is_retryable(), "Unknown must be judged not retryable");
    let reqs = server.received_requests().unwrap();
    assert_eq!(
        reqs.len(),
        1,
        "even with retries on, Unknown is not sent again"
    );
}

/// The backoff path is not executed by default — this pins that it is "conditional
/// code", not "dead code" (design §9.0).
///
/// Fix round 1 (test-strength finding): the original version measured only the elapsed
/// time (<500ms), which amounts to asserting **a value, not the mechanism**, so M1
/// (default 1→3) survived — because the first two sleeps of exponential backoff
/// (≈100ms+200ms, shortened further by jitter) happen to add up to less than 500ms.
/// Counting the requests (discipline article 3) has to match the default exactly
/// whatever value it is changed to, so M1 cannot survive it — the reason the same
/// assertion as `default_does_not_retry_even_on_retryable_5xx` is repeated here is that
/// "the backoff takes little time" and "there is no retry at all" hang on different code
/// paths (the former on `backoff_delay`+`sleep`, the latter on the `retries_used`+1 vs
/// `max_attempts` comparison), so a single assertion does not guarantee both.
#[test]
fn backoff_path_is_not_entered_by_default() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503).set_body_raw(b"{}".to_vec(), "application/json"))
        .mount(&server);

    // By default there is no backoff wait, so it finishes immediately.
    let c = test_client(&server);
    let t0 = std::time::Instant::now();
    let _ = c.send(&GetRegion);
    assert!(
        t0.elapsed() < std::time::Duration::from_millis(500),
        "the default must not go through backoff: {:?}",
        t0.elapsed()
    );

    // Mechanism assertion: a 503 is judged **retryable**, but under the default it is
    // measured directly by how many were sent — M1 (default 1→3) dies here.
    let reqs = server.received_requests().unwrap();
    assert_eq!(
        reqs.len(),
        1,
        "if the default changes (e.g. 1→3), the request count must show it right here"
    );
}

/// A fully deterministic chain deadline test: with the deadline set to `ZERO`, **there
/// must not be a single retry after the first failure** — whatever the exponential
/// backoff's jitter draws (`elapsed >= ZERO` is always true, so the budget is already
/// exhausted at the first failure). `max_attempts` is left open at a visibly large 5 to
/// make it clear that it would have retried were it not for the deadline check.
///
/// Fix round 1 (test-strength finding): this test is the **fully deterministic** version
/// of what the review asked for, "the request count matches exactly the value the
/// deadline allows" — with the deadline at 0, "the value it allows" is always exactly 1,
/// leaving no room for jitter variability.
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
    // Deterministic assertion: request count == 1 is the heart of this test. The
    // elapsed-time assertion below is only a bonus sanity check on top of it (just loose
    // enough to catch a real entry into backoff) — once the request count is settled at
    // 1, a local mock round trip that happens to take long (slow CI/WSL) has nothing
    // to do with the property this test protects.
    let reqs = server.received_requests().unwrap();
    assert_eq!(
        reqs.len(),
        1,
        "🔴 a retry went out despite deadline=ZERO — the deadline check was bypassed"
    );
    assert!(
        elapsed < std::time::Duration::from_secs(3),
        "abnormally long for one round trip (backoff entry suspected): {elapsed:?}"
    );
}

/// The chain deadline limits the total elapsed time **including backoff sleeps** —
/// exactly the shape that the lessons of design §9.2 and Task 4 pointed out a
/// per-attempt timeout alone cannot catch.
/// [`zero_chain_deadline_permits_no_retries_at_all`] is the **fully deterministic**
/// evidence for this property, and this test separately measures that "a non-zero
/// deadline also really cuts off in the middle of retrying".
///
/// Fix round 1 (test-strength finding, **corrected version**): the original version also
/// asserted `reqs.len() < max_attempts` here, but when actually run it **broke on the
/// very first run** — with `max_attempts(5)` and `retry_deadline(300ms)`, the event of
/// the uniform jitter of four exponential backoffs (base values 100, 200, 400, 800ms)
/// happening to sum to under 300ms was not the "rare event on the order of 1.6e-5" I
/// first estimated but **a common event in measurement** (it is already close once the
/// jitter of the largest term, 800ms, alone is ≈0.37 or less). In other words, as long as
/// real unseeded wall-clock jitter is used, **asserting an upper bound on the request
/// count as a strict inequality cannot be deterministically true with this
/// implementation** — precisely the situation the review warned about in advance, so
/// instead of forcing the value narrower that assertion itself was removed. What stays
/// is only the property that holds deterministically: each backoff sleep is always
/// clamped to `remaining` (= the time left until the deadline), so **the cumulative time
/// spent sleeping never exceeds `retry_deadline`, whatever the jitter draws** — the only
/// remaining overrun is the last real network round trip. "It retried at least once"
/// (`reqs.len() > 1`) stays too — `retry_deadline` (300ms) is comfortably larger than one
/// local round trip, so this is practically deterministic as well.
///
/// Fix round 2 (2026-11-23, flaky under the verify VM): the slack was 200ms, and the last
/// round trip plus mock start-up overran it in 3 of 12 solo runs (total 502ms). The slack is
/// now 1s. To keep the negative control strong with the wider slack, `max_attempts` is 10:
/// without the deadline the nine full-jitter sleeps have base values summing to about 51s
/// (the last ones 6.4s, 12.8s, 25.6s), so finishing under 1.3s by chance needs all of the
/// large draws to be small at once (well under 1 in 1,000). The retry budget is still left
/// open so that the deadline, not `max_attempts`, is the limiting factor.
#[test]
fn nonzero_chain_deadline_cuts_off_mid_retry() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(503).set_body_raw(b"{}".to_vec(), "application/json"))
        .mount(&server);

    let retry_deadline = std::time::Duration::from_millis(300);
    let (b, _dir) = test_client_builder(&server);
    let c = b
        .max_attempts(10)
        .retry_deadline(retry_deadline)
        .build()
        .unwrap();

    let t0 = std::time::Instant::now();
    let e = c.send(&GetRegion).unwrap_err();
    let elapsed = t0.elapsed();

    assert!(matches!(&e, Error::Api { status, .. } if status.as_u16() == 503));
    assert!(
        elapsed < retry_deadline + std::time::Duration::from_secs(1),
        "🔴 deadline did not cut the backoff sleep — total {elapsed:?} (budget {retry_deadline:?})"
    );

    let reqs = server.received_requests().unwrap();
    assert!(
        reqs.len() > 1,
        "must have retried at least once (through backoff at least once): {} request(s)",
        reqs.len()
    );
}

// --- T9: advisory scope pre-check ---------------------------------------

/// If the token refreshed after 401 recovery has gained a command scope, nothing is sent
/// with that token. Only the first request may reach the server.
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
    assert!(matches!(e, Error::CommandScopeOnReadToken), "actual: {e:?}");
    let reqs = server.received_requests().unwrap();
    assert_eq!(
        reqs.len(),
        1,
        "a request went out with the refreshed command-scope token"
    );
    assert!(
        !reqs[0]
            .headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("-rot"),
        "the first request must carry the pre-refresh token"
    );
}

/// A read token carrying a command scope is blocked **before the request** (the server
/// receives 0).
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
        Ok(_) => panic!("command-scope token got through — expected CommandScopeOnReadToken"),
    };
    assert!(matches!(e, Error::CommandScopeOnReadToken), "actual: {e:?}");
    assert!(matches!(e.billing(), crate::pricing::Billing::NotBilled));
    assert!(
        server.received_requests().unwrap().is_empty(),
        "a request with the command-scope token reached the server"
    );
}

/// When the scope is definitely missing, it is blocked **before the network** (0).
/// "An error happened" is not enough — this asserts that the server received 0 requests.
#[test]
fn missing_scope_blocks_before_any_request() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(br#"{"response":[]}"#.to_vec(), "application/json"),
        )
        .mount(&server);

    // Seed a JWT whose scp does **not** contain vehicle_device_data.
    let c = test_client_with_scopes(&server, &["openid", "offline_access"]);
    // `.unwrap_err()` is not used — under REQ-18 `VehicleSummary` deliberately does not
    // implement `Debug` (see the model doc), so it trips the `T: Debug` bound of
    // `Result<T, E>::unwrap_err`. A manual match avoids that requirement.
    let result: Result<Page<crate::model::VehicleSummary>, Error> =
        c.send(&crate::api::vehicles::ListVehicles {
            page: None,
            per_page: None,
        });
    let e = match result {
        Err(e) => e,
        Ok(_) => panic!("the scope pre-check let it through — expected MissingScope"),
    };

    assert!(matches!(e, Error::MissingScope { .. }), "actual: {e:?}");
    assert!(matches!(e.billing(), crate::pricing::Billing::NotBilled));
    assert!(
        server.received_requests().unwrap().is_empty(),
        "if the pre-check blocked it, no request may go out"
    );
}

/// A token that is not a JWT or cannot be parsed is **let through** (fail-open).
/// A payload with an unverified signature has no authority — the final verdict is the
/// server's 403.
#[test]
fn opaque_token_passes_through() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_raw(br#"{"response":[]}"#.to_vec(), "application/json"),
        )
        .mount(&server);

    // An opaque token that is not a JWT — scp cannot be observed.
    let c = test_client_with_opaque_token(&server);
    let got: Page<crate::model::VehicleSummary> = c
        .send(&crate::api::vehicles::ListVehicles {
            page: None,
            per_page: None,
        })
        .unwrap();

    assert!(got.items.is_empty());
    // Counts the requests **that went to this endpoint**, not the total. When the region
    // cache is empty, `send` first spends a pre-flight resolution on
    // `/api/1/users/region` (§4.2, already covered in T7), but that is not this test's
    // concern — the mock does not filter by path, so that request also reaches the
    // server and is recorded.
    let vehicle_hits = server
        .received_requests()
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/api/1/vehicles")
        .count();
    assert_eq!(
        vehicle_hits, 1,
        "unobservable must **pass** (fail-open) — the real endpoint must be reached exactly once"
    );
}

/// Indeterminate and ContainsRequiredScope take **the same path**.
/// If they diverge at the call site, fail-open breaks right there (design §10).
#[test]
fn indeterminate_and_contains_take_the_same_path() {
    for scopes in [
        None,                               // Indeterminate (not a JWT)
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

        assert!(r.is_ok(), "both cases must pass: scopes={scopes:?}");
        // For the same reason as above, only `/api/1/vehicles` hits are counted —
        // pre-flight region resolution is a separate concern.
        let vehicle_hits = server
            .received_requests()
            .unwrap()
            .iter()
            .filter(|r| r.url.path() == "/api/1/vehicles")
            .count();
        assert_eq!(
            vehicle_hits, 1,
            "both cases must reach the real endpoint exactly once: scopes={scopes:?}"
        );
    }
}

/// A 403 is `Error::Api`, not `MissingScope` — because the billing verdict differs.
/// MissingScope is a block before the network (0); a 403 is a server refusal that cost
/// money.
#[test]
fn server_403_is_api_error_not_missing_scope() {
    let server = MockServer::start();
    Mock::given(method("GET"))
        .respond_with(ResponseTemplate::new(403).set_body_raw(
            br#"{"error":"insufficient scope"}"#.to_vec(),
            "application/json",
        ))
        .mount(&server);

    // Let the pre-check pass, but make the server refuse.
    let c = test_client_with_scopes(&server, &["vehicle_device_data"]);
    // `.unwrap_err()` is not used for the same reason as in the test above
    // (`VehicleSummary` does not implement `Debug`, REQ-18).
    let result: Result<Page<crate::model::VehicleSummary>, Error> =
        c.send(&crate::api::vehicles::ListVehicles {
            page: None,
            per_page: None,
        });
    let e = match result {
        Err(e) => e,
        Ok(_) => panic!("expected a server 403"),
    };

    assert!(
        matches!(&e, Error::Api { status, .. } if status.as_u16() == 403),
        "actual: {e:?}"
    );
    assert!(
        !matches!(e, Error::MissingScope { .. }),
        "folding a server 403 into MissingScope blurs the billing verdict"
    );
    // For the same reason as above, only `/api/1/vehicles` hits are counted — pre-flight
    // region resolution is a separate concern.
    let vehicle_hits = server
        .received_requests()
        .unwrap()
        .iter()
        .filter(|r| r.url.path() == "/api/1/vehicles")
        .count();
    assert_eq!(
        vehicle_hits, 1,
        "this request really went out — $0 on an unbilled endpoint, but it did go out"
    );
}

// --- T9: send_guarded (cost gate) --------------------------------------

/// When offline, **0** paid requests go out. This is why the gate exists.
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
    assert_eq!(paid, 0, "the gate blocked it, yet a paid request went out");
}

/// When online, the call is really made.
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

/// A failing gate is fail-closed — the paid call is not forced through.
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
        "the gate failed, yet a paid request went out — that is fail-open"
    );
}

/// backseat_token is in the gate response and must not leak into errors or logs.
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
        "the token leaked: {:?}",
        got.value
    );
}

/// A5 T2: the rate-limit headers of a paid call that passed the gate reach the caller.
/// Up to A4, send_guarded used send and so threw this value away entirely.
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
    // paid call: serves the three ratelimit headers
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
    let rl = got.rate_limit.expect("the gate dropped the rate-limit");
    assert_eq!(rl.limit, 60);
    assert_eq!(rl.remaining, 57);
    assert_eq!(rl.reset_raw, 42);
}

/// A5 T2: the Skipped path reports **the probe's** rate-limit. In measurement the probe
/// sends no headers, so it is None, but it must be the value that came from the probe and
/// not a hardcoded None — if Tesla starts attaching headers to the probe, they must show.
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
    let rl = got.rate_limit.expect("the probe's rate-limit was dropped");
    assert_eq!(rl.remaining, 11);
    // Mechanism assertion (discipline article 3): no request went out on the paid path.
    let paid = server
        .received_requests()
        .unwrap()
        .iter()
        .filter(|r| r.url.path().ends_with("/vehicle_data"))
        .count();
    assert_eq!(paid, 0, "the gate was Skipped, yet a paid call went out");
}

// --- A4 T10: RateLimit / Metered<T> ------------------------------------

/// A paid endpoint's rate-limit headers come attached to the response.
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
    let rl = m.rate_limit.expect("a paid endpoint sends the headers");
    assert_eq!((rl.limit, rl.remaining, rl.reset_raw), (60, 59, 57));
}

/// An unbilled endpoint has no headers → None (measurement E4).
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
    assert!(
        m.rate_limit.is_none(),
        "a rate limit appeared on an unbilled response"
    );
}

/// Headers that are only partly present or not numeric give None — no partial value is
/// made up.
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

/// Even when one `Client` makes a metered call followed right away by an unmetered one,
/// the first value must not leak into the second result — if the value rode on a shared
/// slot somewhere on the `Client` instead of on the response itself, it would break in
/// exactly this order. This is the only test that checks that the design of returning
/// `Metered<T>` together with the response (the reason there is no global
/// `last_rate_limit()` slot) is really upheld — the two preceding tests build a fresh
/// `Client` each time and never go through this cross-contamination path at all.
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
    assert!(
        first.rate_limit.is_some(),
        "the first call must receive the headers"
    );

    let second = c.send_metered(&GetRegion).unwrap();
    assert!(
        second.rate_limit.is_none(),
        "the previous paid call's rate limit leaked into an unbilled call"
    );
}

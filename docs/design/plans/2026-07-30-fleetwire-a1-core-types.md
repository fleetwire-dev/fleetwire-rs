# fleetwire A1 — core 타입·에러 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** fleetwire 읽기 라이브러리의 **네트워크 없는 순수 core**(엔드포인트 타입 계층·응답 모델·에러/과금 판정·redaction)를 `<workspace>` crate에 구현한다. 전부 유닛 테스트 가능, HTTP·auth·토큰은 이 사이클 아님(A2/A3).

**Architecture:** 기존 F shell(빈 lib)에 core 모듈을 채운다. reqwest/tokio 의존 없음 — 타입·serde 모델·에러 enum·redaction은 순수 로직. A2(auth)·A3(http)가 이 core 위에 올라온다.

**Tech Stack:** Rust edition 2024 · serde/serde_json(모델) · thiserror(에러) · http(StatusCode) · percent-encoding(VIN/query) · regex(redaction 보조). **reqwest·tokio 미도입.**

## Global Constraints

SDD `docs/design/specs/fleetwire-read-client/`(정정본, fleetapi 커밋 9c8df0b) + F shell 규약. 모든 태스크 암묵 적용.

- **레포**: `<workspace>` (기존 F shell 위, main, remote 없음). 모든 git/cargo는 여기서. fleetapi 레포 건드리지 말 것.
- **git author(D7)**: `Jeong Seung Hwan <4435353+draken1@users.noreply.github.com>` (이미 repo-local 설정됨).
- **불변식**: `#![forbid(unsafe_code)]` + `#![deny(missing_docs)]`(모든 pub 항목 문서화), `[lints] workspace=true`. edition 2024·MSRV 1.85.0·`publish=false` 유지.
- **읽기 전용**: 이 사이클엔 HTTP method 코드 없음(순수 타입). `reqwest`·`tokio` 의존 **추가 금지**(A3에서).
- **serde 정책(REQ-4)**: 응답 모델은 `Option<T>`+`#[serde(default)]`+`#[serde(flatten)] extra`. **`deny_unknown_fields` 절대 사용 금지**.
- **과금(O1/REQ-10)**: `Pricing::{NotCharged, Data{usd_per_request:0.002}}`. 유료는 `vehicle_data`·`nearby_charging_sites` 2개만, 14개 `NotCharged`. 무과금은 status 불문 `Billing::NotBilled`.
- **redaction(REQ-18)**: 모든 외부 표현(로그·에러·Debug) 전에 단일 계층. Bearer/JWT/refresh_token/client_secret→`[REDACTED]`, VIN→`[VIN]`, 좌표→`[COORD]`. JSON 키 allowlist 우선, regex 보조.
- **검증**: 각 태스크 종료 시 `cargo +stable test -p fleetwire`(해당) + `cargo +stable clippy --all-targets --all-features --locked -- -D warnings` + `cargo +stable fmt --all -- --check` 통과. 네트워크 테스트 없음(전부 순수).
- **커밋**: 한글 요약, 단순 커밋(fleetapi Co-Authored-By 규약 불필요).

---

## 파일 구조 (`<workspace>/crates/fleetwire/src/`)

| 파일 | 책임 | 태스크 |
|------|------|--------|
| `lib.rs` | 모듈 선언 + crate 범위 문서(16 GET·미검증 12 제외) | 1 |
| `sealed.rs` | `sealed::Sealed`(외부 구현 차단) | 5 |
| `scope.rs` | `Scope`, `ScopeExpr{All,Any,One}` + 충족 판정 | 2 |
| `pricing.rs` | `Pricing{NotCharged,Data}`, `Billing{Billed,NotBilled,Unknown}` | 3 |
| `query.rs` | `QuerySpec`(percent-encode·중복키), VIN path-segment 안전 인코딩 | 4 |
| `endpoint.rs` | `FleetGet`, `VehicleEndpoint` trait | 5 |
| `model.rs` | `Envelope<T>`, `VehicleSummary`, `Pagination`, `Page<T>` (serde 관대) | 6 |
| `error.rs` | `Error`(7 variant), `is_retryable`/`retry_after`/`billing` | 7 |
| `redaction.rs` | `Redactor`(redact_json/redact_text), body_preview 배선 | 8 |
| `decode.rs` | status+content-type 게이트 후 Envelope 해체 | 9 |

각 태스크 = SDD tasks.md 해당 sub-task(§1~§4) + design 해당 절. implementer는 brief로 SDD 근거를 받는다.

---

### Task 1: 의존성 + 모듈 골격 + crate 범위 문서

**Files:**
- Modify: `crates/fleetwire/Cargo.toml` (deps 추가)
- Modify: `crates/fleetwire/src/lib.rs` (모듈 선언 + crate 문서)
- Create: `src/{sealed,scope,pricing,query,endpoint,model,error,redaction,decode}.rs` (빈 스텁, `//!` 모듈 문서만)

**Interfaces:**
- Consumes: F shell의 빈 `lib.rs`(현재 pub 항목 0).
- Produces: 컴파일되는 모듈 골격. 이후 태스크가 각 모듈을 채운다.

- [ ] **Step 1: Cargo.toml에 deps 추가** (`crates/fleetwire/Cargo.toml` `[dependencies]`)

```toml
[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
http = "1"
percent-encoding = "2"
regex = "1"
```

- [ ] **Step 2: `src/lib.rs` 작성** (F shell의 lib.rs 교체)

```rust
//! # fleetwire
//!
//! **Unofficial, read-only** Rust client for the Tesla Fleet API.
//! Not affiliated with, endorsed by, or sponsored by Tesla, Inc.
//!
//! GET operations only — no vehicle-command or signing code. Fleet API calls
//! may be metered (`vehicle_data`, `nearby_charging_sites` are billed Data;
//! the other 14 supported endpoints are not charged).
//!
//! Scope: 16 live-verified GET endpoints (12 vehicle + 4 user). 12 unverified
//! endpoints (charging/energy/partner) are intentionally excluded until verified.
//!
//! Status: **work in progress** — A1 core types only; HTTP/auth not yet wired.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod sealed;
pub mod scope;
pub mod pricing;
pub mod query;
pub mod endpoint;
pub mod model;
pub mod error;
mod redaction;
mod decode;
```

- [ ] **Step 3: 9개 모듈 파일을 `//!` 문서만 있는 스텁으로 생성**

각 파일(예 `src/scope.rs`):
```rust
//! OAuth scopes and scope-satisfaction expressions.
```
(sealed.rs·decode.rs·redaction.rs는 `mod`(비공개), 나머지는 `pub mod`. 각자 한 줄 `//!`.)

- [ ] **Step 4: 컴파일·lint 검증**

Run:
```bash
cd <workspace>
cargo +stable build -p fleetwire --locked
cargo +stable clippy -p fleetwire --all-targets --all-features --locked -- -D warnings
cargo +stable fmt --all -- --check
```
Expected: 전부 exit 0. `deny(missing_docs)`가 빈 pub mod엔 안 걸림(mod 자체 `//!`로 충족).

- [ ] **Step 5: 커밋**
```bash
git add crates/fleetwire/Cargo.toml crates/fleetwire/src/
git commit -m "feat(a1): 의존성 + core 모듈 골격 + crate 범위 문서"
```

---

### Task 2: `Scope` · `ScopeExpr` (SDD 1.4)

**Files:** Modify `src/scope.rs` · Test: 같은 파일 `#[cfg(test)]`

**Interfaces:**
- Produces: `Scope`(enum, `vehicle_device_data`/`vehicle_location`/`user_data`/`openid` 등), `ScopeExpr::{All(Vec<ScopeExpr>), Any(Vec<ScopeExpr>), One(Scope)}`, `fn ScopeExpr::is_satisfied_by(&self, held: &[Scope]) -> bool`, `fn ScopeExpr::missing(&self, held: &[Scope]) -> Option<ScopeExpr>`.

- [ ] **Step 1: 실패 테스트 작성** (`src/scope.rs` 하단)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_requires_every_scope() {
        let e = ScopeExpr::All(vec![
            ScopeExpr::One(Scope::VehicleDeviceData),
            ScopeExpr::One(Scope::VehicleLocation),
        ]);
        assert!(!e.is_satisfied_by(&[Scope::VehicleDeviceData]));
        assert!(e.is_satisfied_by(&[Scope::VehicleDeviceData, Scope::VehicleLocation]));
    }
    #[test]
    fn any_requires_one() {
        let e = ScopeExpr::Any(vec![ScopeExpr::One(Scope::UserData), ScopeExpr::One(Scope::Openid)]);
        assert!(e.is_satisfied_by(&[Scope::Openid]));
        assert!(!e.is_satisfied_by(&[Scope::VehicleLocation]));
    }
}
```

- [ ] **Step 2: 실패 확인** — `cargo +stable test -p fleetwire scope 2>&1 | tail` → FAIL(미정의).
- [ ] **Step 3: 구현**

```rust
use serde::{Deserialize, Serialize};

/// A Tesla Fleet API OAuth scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Scope {
    /// `vehicle_device_data` — vehicle telemetry/data.
    VehicleDeviceData,
    /// `vehicle_location` — precise location (double opt-in).
    VehicleLocation,
    /// `user_data` — user-context endpoints.
    UserData,
    /// `openid` — base identity.
    Openid,
}

/// A boolean expression over required scopes.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ScopeExpr {
    /// All sub-expressions must hold.
    All(Vec<ScopeExpr>),
    /// At least one sub-expression must hold.
    Any(Vec<ScopeExpr>),
    /// A single required scope.
    One(Scope),
}

impl ScopeExpr {
    /// Whether `held` scopes satisfy this expression.
    pub fn is_satisfied_by(&self, held: &[Scope]) -> bool {
        match self {
            ScopeExpr::One(s) => held.contains(s),
            ScopeExpr::All(v) => v.iter().all(|e| e.is_satisfied_by(held)),
            ScopeExpr::Any(v) => v.iter().any(|e| e.is_satisfied_by(held)),
        }
    }
    /// Returns `Some(self)` when not satisfied, for advisory pre-checks.
    pub fn missing(&self, held: &[Scope]) -> Option<ScopeExpr> {
        if self.is_satisfied_by(held) { None } else { Some(self.clone()) }
    }
}
```

- [ ] **Step 4: 통과 확인** — `cargo +stable test -p fleetwire scope` → PASS.
- [ ] **Step 5: 커밋** — `git commit -am "feat(a1): Scope·ScopeExpr 충족 판정 (SDD 1.4)"`

---

### Task 3: `Pricing` · `Billing` (SDD 1.5 / 9.1, O1)

**Files:** Modify `src/pricing.rs` · Test 동파일

**Interfaces:**
- Produces: `Pricing::{NotCharged, Data { usd_per_request: f64 }}`, `Billing::{Billed, NotBilled, Unknown}`. `Pricing`는 `Copy+PartialEq+Debug`. `fn Pricing::data() -> Pricing`(= `Data{usd_per_request:0.002}` 상수 팩토리).

- [ ] **Step 1: 실패 테스트**
```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn data_is_two_tenths_cent() {
        match Pricing::data() { Pricing::Data { usd_per_request } => assert!((usd_per_request - 0.002).abs() < 1e-9), _ => panic!() }
    }
    #[test]
    fn not_charged_distinct() { assert_ne!(Pricing::NotCharged, Pricing::data()); }
}
```
- [ ] **Step 2: 실패 확인** — `cargo +stable test -p fleetwire pricing` → FAIL.
- [ ] **Step 3: 구현**
```rust
/// Per-endpoint usage-based pricing category (Tesla OpenAPI `pricing_category`).
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum Pricing {
    /// `pricing_category: null` — endpoint is not charged (14 of 16 GETs).
    NotCharged,
    /// Paid `device_data` request. `vehicle_data` and `nearby_charging_sites` only.
    Data {
        /// USD per request (currently 0.002).
        usd_per_request: f64,
    },
}
impl Pricing {
    /// The standard paid Data rate ($0.002/request).
    pub fn data() -> Pricing { Pricing::Data { usd_per_request: 0.002 } }
}

/// Whether a request outcome incurred a charge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Billing {
    /// A charge was incurred (status < 500 on a paid endpoint).
    Billed,
    /// No charge (all NotCharged endpoints; ≥500; confirmed not-sent).
    NotBilled,
    /// Charge status unknown (paid endpoint, server processing unclear).
    Unknown,
}
```
- [ ] **Step 4: 통과** — `cargo +stable test -p fleetwire pricing` → PASS.
- [ ] **Step 5: 커밋** — `git commit -am "feat(a1): Pricing(per-endpoint)·Billing 타입 (O1/SDD 1.5)"`

---

### Task 4: `QuerySpec` + VIN 안전 인코딩 (SDD 1.3, 1.6)

**Files:** Modify `src/query.rs` · Test 동파일

**Interfaces:**
- Produces: `QuerySpec`(내부 `Vec<(String,String)>`), `QuerySpec::new()`, `push(&mut self, key:&str, val:impl ToString)`, `to_encoded(&self) -> String`(percent-encode, `k=v&k2=v2`, 빈 경우 `""`). `fn encode_vin_segment(vin: &str) -> Result<String, VinError>` — `../`·`/`·scheme(`:`)·authority·NUL·백슬래시 차단, 통과 시 percent-encoded segment. `VinError`(thiserror).

- [ ] **Step 1: 실패 테스트**
```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn query_encodes_and_joins() {
        let mut q = QuerySpec::new();
        q.push("page", 2u64); q.push("per_page", 100u64);
        assert_eq!(q.to_encoded(), "page=2&per_page=100");
        let mut e = QuerySpec::new(); assert_eq!(e.to_encoded(), "");
    }
    #[test]
    fn query_percent_encodes_special() {
        let mut q = QuerySpec::new(); q.push("endpoints", "location_data;charge_state");
        assert!(q.to_encoded().contains("%3B")); // ';' encoded
    }
    #[test]
    fn vin_rejects_traversal_and_scheme() {
        assert!(encode_vin_segment("../secrets").is_err());
        assert!(encode_vin_segment("a/b").is_err());
        assert!(encode_vin_segment("http://evil").is_err());
        assert!(encode_vin_segment("5YJ3E1EA7KF000000").is_ok());
    }
}
```
- [ ] **Step 2: 실패 확인** — FAIL.
- [ ] **Step 3: 구현** (percent-encoding crate 사용, `NON_ALPHANUMERIC` 기반 form/segment set)
```rust
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};

const FORM: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'_').remove(b'.').remove(b'~');

/// Structured query params. Never string-concatenated by callers.
#[derive(Debug, Clone, Default)]
pub struct QuerySpec { pairs: Vec<(String, String)> }
impl QuerySpec {
    /// New empty spec.
    pub fn new() -> Self { Self::default() }
    /// Append a key/value pair (value stringified).
    pub fn push(&mut self, key: &str, val: impl ToString) { self.pairs.push((key.to_string(), val.to_string())); }
    /// Percent-encoded `k=v&...` (empty string when no pairs).
    pub fn to_encoded(&self) -> String {
        self.pairs.iter()
            .map(|(k, v)| format!("{}={}", utf8_percent_encode(k, FORM), utf8_percent_encode(v, FORM)))
            .collect::<Vec<_>>().join("&")
    }
}

/// Error encoding a VIN into a path segment.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum VinError {
    /// VIN contained a path-traversal, separator, scheme, or control character.
    #[error("unsafe VIN path segment")]
    Unsafe,
}

/// Encode a VIN as a single safe path segment (blocks traversal/scheme/authority).
pub fn encode_vin_segment(vin: &str) -> Result<String, VinError> {
    if vin.is_empty()
        || vin.contains('/') || vin.contains('\\') || vin.contains(':')
        || vin.contains('\0') || vin.contains("..") {
        return Err(VinError::Unsafe);
    }
    Ok(utf8_percent_encode(vin, FORM).to_string())
}
```
- [ ] **Step 4: 통과** — PASS.
- [ ] **Step 5: 커밋** — `git commit -am "feat(a1): QuerySpec 인코딩 + VIN path-segment 안전화 (SDD 1.3/1.6)"`

---

### Task 5: `Sealed` + `FleetGet` + `VehicleEndpoint` trait (SDD 1.1, 1.2)

**Files:** Modify `src/sealed.rs`, `src/endpoint.rs` · Test: `src/endpoint.rs` (테스트용 endpoint 구현으로 검증)

**Interfaces:**
- Consumes: `scope::ScopeExpr`, `pricing::Pricing`, `query::QuerySpec`.
- Produces: `sealed::Sealed`(crate-private marker), `pub trait FleetGet: sealed::Sealed + Send + Sync { type Response: serde::de::DeserializeOwned; fn required_scopes(&self)->ScopeExpr; fn pricing(&self)->Pricing; fn path(&self)->String; fn query(&self)->QuerySpec; }`, `pub trait VehicleEndpoint: FleetGet { fn vin(&self)->&str; }`. 외부 crate는 `Sealed` 미구현이라 `FleetGet` 구현 불가.

- [ ] **Step 1: 실패 테스트** (테스트 내부 dummy endpoint로 trait 계약 확인)
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{scope::{Scope, ScopeExpr}, pricing::Pricing, query::QuerySpec};
    struct DummyVehicles;
    impl crate::sealed::Sealed for DummyVehicles {}
    impl FleetGet for DummyVehicles {
        type Response = serde_json::Value;
        fn required_scopes(&self) -> ScopeExpr { ScopeExpr::One(Scope::VehicleDeviceData) }
        fn pricing(&self) -> Pricing { Pricing::NotCharged }
        fn path(&self) -> String { "/api/1/vehicles".into() }
        fn query(&self) -> QuerySpec { QuerySpec::new() }
    }
    #[test]
    fn fleetget_contract() {
        let d = DummyVehicles;
        assert_eq!(d.path(), "/api/1/vehicles");
        assert_eq!(d.pricing(), Pricing::NotCharged);
        assert!(d.required_scopes().is_satisfied_by(&[Scope::VehicleDeviceData]));
    }
}
```
- [ ] **Step 2: 실패 확인** — FAIL.
- [ ] **Step 3: 구현**

`src/sealed.rs`:
```rust
//! Sealed marker preventing external `FleetGet` implementations.
/// Crate-private supertrait; external crates cannot implement it.
pub trait Sealed {}
```
`src/endpoint.rs`:
```rust
use serde::de::DeserializeOwned;
use crate::{pricing::Pricing, query::QuerySpec, scope::ScopeExpr};

/// A strongly-typed read-only (GET) Fleet API endpoint.
pub trait FleetGet: crate::sealed::Sealed + Send + Sync {
    /// Deserialized response payload type.
    type Response: DeserializeOwned;
    /// Scopes this call requires (may depend on request values).
    fn required_scopes(&self) -> ScopeExpr;
    /// Usage-based pricing category for this endpoint.
    fn pricing(&self) -> Pricing;
    /// Relative API path (VIN encoded as a path segment).
    fn path(&self) -> String;
    /// Structured query parameters.
    fn query(&self) -> QuerySpec;
}

/// A `FleetGet` scoped to a specific vehicle (`{vin}` path).
pub trait VehicleEndpoint: FleetGet {
    /// The target vehicle VIN.
    fn vin(&self) -> &str;
}
```
- [ ] **Step 4: 통과** — PASS. (외부 구현 차단은 컴파일 계약 — `sealed`가 crate-private이므로 문서로 확인.)
- [ ] **Step 5: 커밋** — `git commit -am "feat(a1): Sealed·FleetGet·VehicleEndpoint trait (SDD 1.1/1.2)"`

---

### Task 6: 응답 모델 · Envelope · Page (SDD 2.1–2.3, REQ-4)

**Files:** Modify `src/model.rs` · Test 동파일

**Interfaces:**
- Produces: `pub(crate) struct Envelope<T> { pub response: T }`(Deserialize), `pub struct VehicleSummary`(id/vin/state Option+default, `#[serde(flatten)] extra`), `pub struct Pagination`(previous/next/current/per_page/count/pages: Option<u64>), `pub struct Page<T> { pub items: Vec<T>, pub pagination: Pagination }`. **어느 모델에도 `deny_unknown_fields` 없음.**

- [ ] **Step 1: 실패 테스트** (관대 파싱: extra 필드·누락·null)
```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tolerates_extra_and_missing() {
        let j = r#"{"vin":"5YJ...","new_field_tesla_added":42}"#;
        let v: VehicleSummary = serde_json::from_str(j).unwrap();
        assert_eq!(v.vin.as_deref(), Some("5YJ..."));
        assert!(v.state.is_none());              // 누락 필드 → None
        assert!(v.extra.contains_key("new_field_tesla_added")); // 미지 필드 보존
    }
    #[test]
    fn envelope_unwraps() {
        let j = r#"{"response":{"vin":"X"}}"#;
        let e: Envelope<VehicleSummary> = serde_json::from_str(j).unwrap();
        assert_eq!(e.response.vin.as_deref(), Some("X"));
    }
}
```
- [ ] **Step 2: 실패 확인** — FAIL.
- [ ] **Step 3: 구현** (design §6 verbatim: Envelope/VehicleSummary/Pagination/Page — 위 Interfaces 대로. `Page<T>`는 수동 구성이라 Deserialize 파생 안 함; Envelope·VehicleSummary·Pagination만 `#[derive(Deserialize)]`.)
- [ ] **Step 4: 통과** — PASS.
- [ ] **Step 5: `deny_unknown_fields` 부재 검증** — `! grep -rq 'deny_unknown_fields' crates/fleetwire/src/` (있으면 실패). 커밋 `git commit -am "feat(a1): 관대 응답 모델·Envelope·Page (SDD 2, REQ-4)"`

---

### Task 7: `Error` + 과금·재시도 판정 (SDD 3.1–3.5, 9.1–9.3)

**Files:** Modify `src/error.rs` · Test 동파일

**Interfaces:**
- Consumes: `pricing::{Pricing,Billing}`, `scope::{Scope,ScopeExpr}`, `http::StatusCode`.
- Produces: `pub enum Error`(design §9.1 7-variant verbatim: `TransportNotSent{message,pricing}`, `TransportUnknown{message,pricing}`, `Api{status,message,retry_after:Option<Duration>,body_preview,pricing}`, `VehicleOffline{pricing}`, `MissingScope{required:ScopeExpr,held:Vec<Scope>}`, `Decode{source,status,body_preview,pricing}`, `Auth(String)`) + `#[derive(thiserror::Error)]` + `#[non_exhaustive]`. 메서드: `fn is_retryable(&self)->bool`, `fn retry_after(&self)->Option<Duration>`(120s cap), `fn billing(&self)->Billing`(판정표 §9.2).

- [ ] **Step 1: 실패 테스트** (판정표 핵심 행)
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::{Pricing, Billing};
    use std::time::Duration;
    #[test]
    fn not_charged_never_billed() {
        let e = Error::Api { status: http::StatusCode::OK, message: "".into(), retry_after: None, body_preview: "".into(), pricing: Pricing::NotCharged };
        assert_eq!(e.billing(), Billing::NotBilled);
    }
    #[test]
    fn paid_success_billed_408_billed() {
        let ok = Error::Api { status: http::StatusCode::from_u16(408).unwrap(), message:"".into(), retry_after:None, body_preview:"".into(), pricing: Pricing::data() };
        assert_eq!(ok.billing(), Billing::Billed);         // Data 408 = billed
        assert!(!ok.is_retryable());                        // 408 재시도 금지
    }
    #[test]
    fn transport_unknown_paid_is_unknown_not_retryable() {
        let e = Error::TransportUnknown { message:"reset".into(), pricing: Pricing::data() };
        assert_eq!(e.billing(), Billing::Unknown);
        assert!(!e.is_retryable());
    }
    #[test]
    fn transport_not_sent_retryable_not_billed() {
        let e = Error::TransportNotSent { message:"dns".into(), pricing: Pricing::data() };
        assert_eq!(e.billing(), Billing::NotBilled);
        assert!(e.is_retryable());
    }
    #[test]
    fn retry_after_capped_120s() {
        let e = Error::Api { status: http::StatusCode::from_u16(429).unwrap(), message:"".into(), retry_after: Some(Duration::from_secs(999)), body_preview:"".into(), pricing: Pricing::NotCharged };
        assert_eq!(e.retry_after(), Some(Duration::from_secs(120)));
    }
}
```
- [ ] **Step 2: 실패 확인** — FAIL.
- [ ] **Step 3: 구현** — Error enum(위 Interfaces) + thiserror `#[error(...)]` 메시지는 redaction된 필드만 노출(원문 미노출). `billing()`: `NotCharged`→항상 `NotBilled`; `Data`에서 `TransportNotSent`→`NotBilled`, `TransportUnknown`→`Unknown`, `Api`(status<500)→`Billed`, `Api`(≥500)/`VehicleOffline`(무status)→판정표대로, `Decode`(status<500이면 Billed). `is_retryable()`: `TransportNotSent` 또는 `Api{status ∈ {500,502,503,504}}`만 true. `retry_after()`: `Api.retry_after`를 `min(_, 120s)`.
- [ ] **Step 4: 통과** — PASS(판정표 행 커버).
- [ ] **Step 5: 커밋** — `git commit -am "feat(a1): Error 7-variant + billing/retry 판정표 (SDD 3, 9)"`

---

### Task 8: redaction 단일 계층 (SDD 4.1–4.5, REQ-18)

**Files:** Modify `src/redaction.rs` · Test 동파일

**Interfaces:**
- Produces: `pub(crate) fn redact_text(s: &str) -> String`(Bearer/JWT/refresh_token/client_secret→`[REDACTED]`, VIN 패턴→`[VIN]`, 위경도 좌표→`[COORD]`), `pub(crate) fn redact_json(v: &serde_json::Value) -> serde_json::Value`(키 allowlist: `access_token`/`refresh_token`/`client_secret`/`authorization`/`token` 값→`[REDACTED]`, 재귀). `pub(crate) fn body_preview(raw: &str, max: usize) -> String`(redact 후 max 바이트 자름). ⚠️ `redact_transport_error(reqwest::Error)`는 A3(HTTP)에서 배선 — 이 태스크 범위 아님(주석으로 명시).

- [ ] **Step 1: 실패 테스트** (sentinel 값 + VIN/좌표)
```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn redacts_bearer_and_vin_and_coord() {
        let s = "Authorization: Bearer eyJABCDEF.tok.sig VIN 5YJ3E1EA7KF000000 at 37.5665,126.9780";
        let r = redact_text(s);
        assert!(!r.contains("eyJABCDEF")); assert!(r.contains("[REDACTED]"));
        assert!(!r.contains("5YJ3E1EA7KF000000")); assert!(r.contains("[VIN]"));
        assert!(r.contains("[COORD]"));
    }
    #[test]
    fn redact_json_keys() {
        let v = serde_json::json!({"access_token":"SECRET_XYZ","vin":"5YJ...","nested":{"refresh_token":"R"}});
        let out = redact_json(&v);
        assert_eq!(out["access_token"], serde_json::json!("[REDACTED]"));
        assert_eq!(out["nested"]["refresh_token"], serde_json::json!("[REDACTED]"));
    }
    #[test]
    fn body_preview_redacts_then_truncates() {
        let p = body_preview("access_token=SECRET_XYZ_LONG_VALUE", 12);
        assert!(!p.contains("SECRET_XYZ")); assert!(p.len() <= 12 || p.contains("[REDACTED]"));
    }
    #[test]
    fn sentinel_never_survives() {
        assert!(!redact_text("Bearer ACCESS_TOKEN_SHOULD_NOT_APPEAR").contains("ACCESS_TOKEN_SHOULD_NOT_APPEAR"));
    }
}
```
- [ ] **Step 2: 실패 확인** — FAIL.
- [ ] **Step 3: 구현** — `redact_json` 먼저(구조화 allowlist 재귀), `redact_text`는 보조 regex(`Bearer\s+\S+`, `eyJ[\w-]+\.[\w-]+\.[\w-]+`(JWT), `(refresh_token|client_secret|access_token)=\S+`, VIN `\b[A-HJ-NPR-Z0-9]{17}\b`, 좌표 `-?\d{1,3}\.\d{3,},\s*-?\d{1,3}\.\d{3,}`). regex는 `once_cell`/`std::sync::OnceLock`으로 컴파일 캐시(OnceLock은 redaction 상수 regex에만 — 토큰 저장 아님, REQ-15 무관).
- [ ] **Step 4: 통과** — PASS.
- [ ] **Step 5: 커밋** — `git commit -am "feat(a1): redaction 단일 계층(json allowlist+regex 보조)+body_preview (SDD 4)"`

---

### Task 9: decode 함수 — status+content-type 게이트 (SDD 2.4–2.5)

**Files:** Modify `src/decode.rs` · Test 동파일

**Interfaces:**
- Consumes: `error::Error`, `model::Envelope`, `redaction::body_preview`, `pricing::Pricing`, `http::StatusCode`.
- Produces: `pub(crate) fn decode<T: serde::de::DeserializeOwned>(status: StatusCode, content_type: Option<&str>, body: &[u8], pricing: Pricing) -> Result<T, Error>`. 순서: (1) status ≥400 → `Error::Api`/`VehicleOffline`(408)/`Auth`(401) 분기(body_preview redaction). (2) content-type이 json 아니면 `Error::Decode`(비 JSON 게이트웨이 응답). (3) `Envelope<T>` 파싱 실패 시 `Error::Decode`. (4) 성공 시 `response` 반환. **panic 없음**.

- [ ] **Step 1: 실패 테스트**
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::pricing::Pricing;
    #[test]
    fn ok_json_unwraps_envelope() {
        let r: serde_json::Value = decode(http::StatusCode::OK, Some("application/json"), br#"{"response":{"vin":"X"}}"#, Pricing::NotCharged).unwrap();
        assert_eq!(r["vin"], serde_json::json!("X"));
    }
    #[test]
    fn html_gateway_is_decode_error_not_panic() {
        let e = decode::<serde_json::Value>(http::StatusCode::BAD_GATEWAY, Some("text/html"), b"<html>502</html>", Pricing::NotCharged).unwrap_err();
        assert!(matches!(e, crate::error::Error::Api { .. }));
    }
    #[test]
    fn status_408_is_vehicle_offline() {
        let e = decode::<serde_json::Value>(http::StatusCode::from_u16(408).unwrap(), Some("application/json"), b"{}", Pricing::data()).unwrap_err();
        assert!(matches!(e, crate::error::Error::VehicleOffline { .. }));
    }
    #[test]
    fn non_json_200_is_decode_error() {
        let e = decode::<serde_json::Value>(http::StatusCode::OK, Some("application/pdf"), b"%PDF", Pricing::NotCharged).unwrap_err();
        assert!(matches!(e, crate::error::Error::Decode { .. }));
    }
}
```
- [ ] **Step 2: 실패 확인** — FAIL.
- [ ] **Step 3: 구현** — 위 순서. 401→`Auth`, 408→`VehicleOffline{pricing}`, 기타 ≥400→`Api{status,...,body_preview:body_preview(...),pricing}`. content-type에 `json` 미포함 & status<400 → `Decode`. Envelope 파싱: `serde_json::from_slice::<Envelope<T>>(body)` 실패 → `Decode{source: redact_text(err), status, body_preview, pricing}`.
- [ ] **Step 4: 통과** — PASS.
- [ ] **Step 5: 커밋** — `git commit -am "feat(a1): decode 게이트(status+content-type) → Envelope (SDD 2.4/2.5)"`

---

## Self-Review

**1. Spec coverage** — SDD §1(1.1~1.6)=T2·T3·T4·T5 ✓ / §2(2.1~2.5)=T6·T9 ✓ / §3(3.1~3.6)=T7(+body_preview는 T8) ✓ / §4(4.1~4.5)=T8 ✓ / §0 모듈골격·범위문서(0.2,0.9)=T1 ✓. 지연(A2/A3): 0.4 clippy disallowed(reqwest 도입 시)·§5 HTTP·§6 파이프라인·§7 auth·§8 리전·redact_transport_error — 명시적 out-of-scope.
**2. Placeholder scan** — 모든 태스크 actual 코드/테스트 포함. "TBD/적절히" 없음. design verbatim 참조는 타입 정의(이미 인라인)로 대체.
**3. Type consistency** — `Pricing::data()`·`Pricing::NotCharged`·`Billing`·`ScopeExpr::is_satisfied_by`·`Error` variant 필드명(message/pricing/status/retry_after/body_preview/required/held/source)·`Envelope.response`·`encode_vin_segment`·`QuerySpec::{new,push,to_encoded}`·`redact_text`/`redact_json`/`body_preview`/`decode` — 태스크 간 일치 확인.

## 참고
[A SDD(정정본)](../specs/fleetwire-read-client/) · [O1 매트릭스](../specs/2026-07-29-fleetwire-o1-endpoint-evidence-matrix.md) · [grok-build 교훈](../specs/2026-07-28-grokbuild-design-lessons.md)(Error 비-reqwest·RetryPolicy) · [F shell 설계](../specs/2026-07-28-fleetwire-oss-shell-scaffold-design.md)

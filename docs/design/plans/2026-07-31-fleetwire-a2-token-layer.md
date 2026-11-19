# fleetwire A2 (토큰 계층) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 네트워크를 타지 않는 토큰 계층을 만든다 — 안전한 저장·회전, 프로세스 간 single-flight refresh, 그리고 과금 요청을 미리 막는 스코프 advisory 선검사.

**Architecture:** 실제 refresh POST 는 `TokenRefresher` trait 뒤에 있고 구현은 A3 가 준다. 락은 `std::fs::File::try_lock`(MSRV 1.89.0) 을 논블로킹으로 쓰고 async 재시도로 감싼다 — flock 은 블로킹 syscall 이라 tokio Mutex 를 쥔 채 블로킹하면 런타임 워커가 멈추고, flock 에는 timeout 인자가 없어서 재시도 루프가 REQ-15 의 "유한 timeout" 을 만드는 유일한 방법이다. 저장은 `std::os::unix` 만으로 충분하다.

**Tech Stack:** Rust 2024 edition · MSRV 1.89.0 · `tokio` 1.53(sync,time) · `base64` 0.23(std, no-default) · 기존 `serde`/`serde_json`/`thiserror`/`http`

**설계 SSOT:** `docs/design/specs/2026-07-31-fleetwire-a2-token-layer-design.md`
**작업 레포:** `<workspace>` — ⚠️ `<docs repo>` 는 스펙·계획 보관처이므로 **코드 커밋 금지**.

## Global Constraints

- 워크스페이스 `[workspace.lints.rust] unsafe_code = "forbid"` — `unsafe` 블록 작성 금지. raw `libc` 경로가 애초에 막혀 있다.
- 크레이트 `#![deny(missing_docs)]` — 모든 pub item·pub 필드에 `///` 필요. private 모듈은 강제되지 않지만 크레이트 스타일대로 문서화한다.
- **Unix 전용.** 0600·`create_new`(=`O_CREAT|O_EXCL`)·`ino`/`dev` 재확인·flock 이 보안 모델의 핵심이고 Windows 에 대응물이 없다.
- **새 의존성은 이 둘뿐**: `tokio = { version = "1", default-features = false, features = ["sync", "time"] }`, `base64 = { version = "0.23", default-features = false, features = ["std"] }`. ⚠️ `base64` 의 기본 feature 에 **`simd-unsafe`** 가 있으므로 `default-features = false` 가 필수다.
- 락 파일 `tokens.lock` 은 **정상·오류·panic 어떤 경로에서도 삭제하지 않는다**(REQ-15). 삭제하면 기존 inode holder 와 새 inode holder 가 동시에 생겨 상호배제가 깨진다.
- **토큰 저장에 `OnceCell`/`OnceLock` 을 쓰지 않는다**(REQ-15). 토큰은 회전한다.
- **토큰·payload·전체 JSON 을 로그·에러 메시지에 남기지 않는다**(REQ-18). 파싱 오류도 토큰 내용을 포함하지 않는 정적 분류만 기록한다.
- 게이트: `cargo test -p fleetwire` · `cargo clippy --all-targets --all-features --locked -- -D warnings` · **lib-only clippy** · `cargo fmt --all -- --check` · `cargo +1.89.0 build -p fleetwire --locked` · `cargo doc --no-deps` (rustdoc `-D warnings`).
- 기존 121 테스트는 어느 태스크에서도 깨지지 않는다.

### 테스트 규율 (A1 최종 리뷰가 "A2/A3 템플릿" 으로 지목)

A1 은 redaction 하나에 fix 라운드 4회를 썼고, 매번 원인이 같았다 — **테스트 입력을 구현에 맞춰 재단**했다. 반복하지 말 것:

- **기대값을 피검증 대상이 아닌 독립 출처에서 유도하라.**
- **`contains` 대신 전체 문자열 `assert_eq!`.** A1 의 이중 대괄호 버그가 green 아래 살아남은 이유가 `contains` 였다.
- **구조적 속성을 테스트하라** — 손으로 고른 입력 집합이 아니라 설계 속성 자체를 고정.
- **입력은 assertion 이 통과하게 해주는 것이 아니라 약점을 드러내는 것으로 고르라.**

---

## File Structure

| 파일 | 책임 |
|---|---|
| `Cargo.toml` (워크스페이스) | `rust-version = "1.89.0"` |
| `.github/workflows/ci.yml` | MSRV 매트릭스 `"1.85.0"` → `"1.89.0"` |
| `crates/fleetwire/Cargo.toml` | `tokio`·`base64` 추가 |
| `crates/fleetwire/src/lib.rs` | `mod clock; mod auth;` 선언 + re-export |
| `crates/fleetwire/src/clock.rs` | `Clock` trait, `SystemClock` — 만료 판정을 sleep 없이 테스트하기 위한 주입점 |
| `crates/fleetwire/src/scope.rs` (수정) | `Scope::from_str` — 미지 스코프를 무시하는 파싱 |
| `crates/fleetwire/src/auth/mod.rs` | `TokenManager` 공개 API + single-flight 오케스트레이션, `TokenRefresher` trait |
| `crates/fleetwire/src/auth/store.rs` | `TokenSet` + 안전한 읽기/쓰기(0600·`create_new`·atomic+fsync·symlink 방어) |
| `crates/fleetwire/src/auth/lock.rs` | `LockGuard` — 고정 inode 락, `try_lock` + async 재시도 + deadline |
| `crates/fleetwire/src/auth/jwt.rs` | JWT payload advisory 파서 + `UnverifiedScopeObservation` 3상태 |

---

### Task 1: MSRV 상향 · 의존성 · 모듈 골격

**Files:**
- Modify: `Cargo.toml` (워크스페이스 루트, line 7)
- Modify: `.github/workflows/ci.yml` (line 24)
- Modify: `crates/fleetwire/Cargo.toml`
- Modify: `crates/fleetwire/src/lib.rs`
- Create: `crates/fleetwire/src/clock.rs`, `crates/fleetwire/src/auth/mod.rs`, `crates/fleetwire/src/auth/store.rs`, `crates/fleetwire/src/auth/lock.rs`, `crates/fleetwire/src/auth/jwt.rs`

**Interfaces:**
- Produces: `crate::clock` · `crate::auth{,::store,::lock,::jwt}` 모듈 경로. 이후 모든 태스크가 이 위에 올라간다.

**왜 MSRV 를 올리나:** `std::fs::File::{try_lock, unlock}` 은 로컬 std 소스 실측 기준 `#[stable(feature = "file_lock", since = "1.89.0")]` 이다. 1.85.0 에서는 `error[E0658]: use of unstable library feature 'file_lock'` 로 컴파일되지 않는다. 대안인 `fd-lock` 크레이트를 쓰면 `rustix` 전이 의존성이 자격증명 파일 락이라는 민감 경로에 들어온다. fleetwire 는 아직 `publish = false` 라 사용자가 0 이므로 MSRV 상향 비용이 오늘은 0 이다.

- [ ] **Step 1: MSRV 를 올리고 실패를 확인**

`Cargo.toml` (워크스페이스 루트) line 7:
```toml
rust-version = "1.89.0"
```

`.github/workflows/ci.yml` line 24:
```yaml
        rust: [stable, beta, "1.89.0"]
```

- [ ] **Step 2: 1.89.0 툴체인이 없으면 설치하고 락 API 가 실제로 존재하는지 확인**

Run:
```bash
rustup toolchain install 1.89.0 --profile minimal
cd <workspace>
cat > /tmp/locktest.rs <<'EOF'
fn main() {
    let f = std::fs::File::open("/etc/hostname").unwrap();
    match std::fs::File::try_lock(&f) {
        Ok(()) => { let _ = std::fs::File::unlock(&f); }
        Err(std::fs::TryLockError::WouldBlock) => {}
        Err(std::fs::TryLockError::Error(e)) => panic!("{e}"),
    }
    println!("file_lock OK");
}
EOF
rustc +1.89.0 --edition 2024 /tmp/locktest.rs -o /tmp/locktest && /tmp/locktest
```
Expected: `file_lock OK` (1.85.0 로 같은 명령을 돌리면 `E0658` 이 나온다 — 대조해 보면 MSRV 상향의 근거가 눈으로 확인된다)

- [ ] **Step 3: 의존성 추가**

`crates/fleetwire/Cargo.toml` 의 `[dependencies]` 에 추가:
```toml
tokio = { version = "1", default-features = false, features = ["sync", "time"] }
base64 = { version = "0.23", default-features = false, features = ["std"] }
```

⚠️ `base64` 의 기본 feature 는 `alloc` + `std` + **`simd-unsafe`** 다. `default-features = false` 를 빠뜨리면 SIMD unsafe 코드가 자격증명 경로 의존성에 딸려 들어온다. `default-features = false, features = ["std"]` 로 하면 `simd-unsafe` 가 꺼지고, base64 0.23.0 은 전이 의존성이 **0개**다.

`tokio` 도 `default-features = false` 로 `sync`/`time` 만 켠다 — `mio`/`libc`/`socket2` 를 끌어오지 않는다.

- [ ] **Step 4: 모듈 골격 생성**

`crates/fleetwire/src/clock.rs`:
```rust
//! 시간 주입 — 만료 판정을 실제 sleep 없이 테스트하기 위한 경계.
```

`crates/fleetwire/src/auth/mod.rs`:
```rust
//! 토큰 수명주기 — 저장·회전·프로세스 간 single-flight refresh.
//!
//! 이 모듈은 네트워크를 타지 않는다. 실제 refresh POST 는 [`TokenRefresher`]
//! 구현이 담당하며 그 구현은 서브프로젝트 A3(HTTP 전송)가 제공한다.

mod jwt;
mod lock;
mod store;
```

`crates/fleetwire/src/auth/store.rs`:
```rust
//! 토큰 파일의 안전한 읽기·쓰기 (0600, symlink 방어, atomic + fsync).
```

`crates/fleetwire/src/auth/lock.rs`:
```rust
//! 고정 inode 파일 락 — 프로세스 간 refresh 상호배제.
```

`crates/fleetwire/src/auth/jwt.rs`:
```rust
//! 검증하지 않는 JWT payload 관찰 — 스코프 advisory 선검사 전용.
```

`crates/fleetwire/src/lib.rs` 의 모듈 선언 블록에 추가(알파벳 순 유지 — `cargo fmt` 가 재정렬한다):
```rust
mod auth;
mod clock;
```

- [ ] **Step 5: 전 게이트 통과 확인**

Run:
```bash
cd <workspace>
cargo +1.89.0 build -p fleetwire --locked
cargo test -p fleetwire
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo clippy --locked -- -D warnings          # lib-only
cargo fmt --all -- --check
cargo deny check
```
Expected: 전부 통과, 테스트는 기존 **121 passed** 유지. `cargo deny check` 는 `base64`/`tokio` 의 라이선스가 allowlist(MIT/Apache-2.0/BSD-2/BSD-3/ISC/Unicode-3.0/Zlib)에 드는지 확인한다 — 실패하면 여기서 멈추고 보고할 것.

- [ ] **Step 6: 커밋**

```bash
git add Cargo.toml .github/workflows/ci.yml crates/fleetwire/Cargo.toml crates/fleetwire/src/
git commit -m "chore(a2): MSRV 1.89.0(std file_lock) + tokio/base64 + auth 모듈 골격"
```

---

### Task 2: `Scope::from_str` — 미지 스코프를 무시하는 파싱

**Files:**
- Modify: `crates/fleetwire/src/scope.rs`
- Test: 동파일 `#[cfg(test)] mod tests`

**Interfaces:**
- Produces: `impl std::str::FromStr for Scope`(`type Err = UnknownScope`), `pub struct UnknownScope`. Task 6 의 JWT 파서가 이걸로 `scp` 배열을 스코프로 변환한다.

**왜 필요한가:** 이 머신의 실제 Tesla 토큰에서 읽은 `scp` 는 `["offline_access","openid","vehicle_device_data","vehicle_location"]` 이다. `Scope` 는 4개 variant 만 모델링하는데 그중 `offline_access` 가 **없다**. 미지 스코프에서 파싱 전체를 실패시키면 실제 토큰이 통째로 `Indeterminate` 가 되어 선검사가 무용지물이 된다. 요구 스코프는 항상 우리 엔드포인트 타입에서 오므로 알려진 값이고, 토큰이 들고 있는 미지 스코프를 버려도 판정이 틀리지 않는다.

`#[serde(other)]` 는 쓰지 않는다 — 서로 다른 미지 스코프를 한 variant 로 뭉개면서 공개 enum 을 오염시킨다.

- [ ] **Step 1: 실패 테스트 작성**

`crates/fleetwire/src/scope.rs` 의 `mod tests` 에 추가:
```rust
    #[test]
    fn from_str_accepts_every_modeled_scope_in_wire_form() {
        use core::str::FromStr;
        // 기대값을 Scope 자신이 아니라 Tesla 와이어 문자열에서 독립적으로 유도한다.
        let pairs = [
            ("openid", Scope::Openid),
            ("user_data", Scope::UserData),
            ("vehicle_device_data", Scope::VehicleDeviceData),
            ("vehicle_location", Scope::VehicleLocation),
        ];
        for (wire, expected) in pairs {
            assert_eq!(Scope::from_str(wire), Ok(expected), "wire form {wire}");
        }
    }

    #[test]
    fn from_str_rejects_unknown_scope_without_panicking() {
        use core::str::FromStr;
        // 실측 토큰의 scp 에 실제로 들어 있는 값 — 모델링돼 있지 않다.
        assert!(Scope::from_str("offline_access").is_err());
        assert!(Scope::from_str("").is_err());
        assert!(Scope::from_str("VehicleDeviceData").is_err()); // PascalCase 는 와이어 형식이 아니다
    }
```

- [ ] **Step 2: 실패 확인**

Run: `cargo test -p fleetwire scope::tests::from_str`
Expected: FAIL — `no function or associated item named 'from_str' found` (컴파일 에러)

- [ ] **Step 3: 구현**

`crates/fleetwire/src/scope.rs` 에 추가:
```rust
/// 와이어 문자열이 모델링된 [`Scope`] 중 어느 것도 아닐 때.
///
/// Tesla 는 이 크레이트가 모델링하지 않는 스코프(예: `offline_access`)도 발급한다.
/// 이는 오류 상황이 아니라 **정상**이며, 호출자는 해당 항목을 버리면 된다.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct UnknownScope;

impl core::fmt::Display for UnknownScope {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // 스코프 문자열 자체를 담지 않는다 — 토큰 유래 값을 오류에 싣지 않기 위해서다.
        f.write_str("unrecognized scope")
    }
}

impl core::error::Error for UnknownScope {}

impl core::str::FromStr for Scope {
    type Err = UnknownScope;

    /// Tesla 와이어 형식(snake_case) 문자열을 [`Scope`] 로 변환한다.
    ///
    /// 대소문자를 정규화하지 않는다 — 와이어 형식은 소문자로 고정이고,
    /// 임의 정규화는 실제로 다른 스코프를 같은 것으로 접을 위험이 있다.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "openid" => Ok(Scope::Openid),
            "user_data" => Ok(Scope::UserData),
            "vehicle_device_data" => Ok(Scope::VehicleDeviceData),
            "vehicle_location" => Ok(Scope::VehicleLocation),
            _ => Err(UnknownScope),
        }
    }
}
```

⚠️ `match` 의 팔은 `scope.rs` 의 `#[serde(rename_all = "snake_case")]` 가 만드는 와이어 형식과 **정확히 일치**해야 한다. A1 fix wave 가 넣은 `scope_round_trips_through_wire_form` 테스트가 그 형식을 고정하고 있으니, 불일치가 생기면 그 테스트가 깨진다.

- [ ] **Step 4: 통과 확인**

Run: `cargo test -p fleetwire scope`
Expected: PASS — 신규 2개 + 기존 scope 테스트 전부

- [ ] **Step 5: 커밋**

```bash
git add crates/fleetwire/src/scope.rs
git commit -m "feat(a2): Scope::from_str — 미지 스코프를 버리는 와이어 파싱 (REQ-8)"
```

---

### Task 3: `Clock` 주입 + `TokenSet` + 만료 판정

**Files:**
- Modify: `crates/fleetwire/src/clock.rs`
- Modify: `crates/fleetwire/src/auth/store.rs`
- Test: 각 동파일

**Interfaces:**
- Produces:
  - `pub trait Clock: Send + Sync { fn now_unix(&self) -> u64; }`
  - `pub struct SystemClock;` (`impl Clock`)
  - `pub(crate) struct TokenSet { access_token: String, refresh_token: String, expires_at: u64, extra: Map<String, Value> }`
  - `impl TokenSet`: `pub(crate) fn access_token(&self) -> &str`, `pub(crate) fn refresh_token(&self) -> &str`, `pub(crate) fn is_fresh(&self, clock: &dyn Clock, skew: Duration) -> bool`
  - `pub(crate) const EXPIRY_SKEW: Duration` (60초)
- Consumes: 없음

**왜 `now_unix()` 인가:** `expires_at` 을 unix 초로 저장하므로(안정적 JSON) 시계도 같은 단위를 주는 편이 변환 실수를 없앤다. `SystemTime` 을 반환하면 매 비교마다 `duration_since(UNIX_EPOCH)` 의 `Result` 를 처리해야 하고, 그 오류 경로가 테스트되지 않은 채 남는다.

**만료 skew 60초:** 너무 작으면 요청이 나가는 도중에 토큰이 만료되고, 너무 크면 불필요한 refresh 로 회전을 촉발한다. Tesla access token 수명이 8시간대라 60초는 0.2% 의 여유다.

- [ ] **Step 1: 실패 테스트 작성**

`crates/fleetwire/src/clock.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_clock_is_within_a_second_of_std_time() {
        // 독립 출처(std)에서 기대값을 유도한다 — SystemClock 자신을 되풀이 단언하지 않는다.
        let std_now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock before unix epoch")
            .as_secs();
        let ours = SystemClock.now_unix();
        assert!(ours.abs_diff(std_now) <= 1, "ours={ours} std={std_now}");
    }

    #[test]
    fn fixed_clock_does_not_advance_on_its_own() {
        let c = FixedClock::new(1_700_000_000);
        assert_eq!(c.now_unix(), 1_700_000_000);
        assert_eq!(c.now_unix(), 1_700_000_000);
        c.advance(30);
        assert_eq!(c.now_unix(), 1_700_000_030);
    }
}
```

`crates/fleetwire/src/auth/store.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;

    fn token_set_expiring_at(expires_at: u64) -> TokenSet {
        serde_json::from_value(serde_json::json!({
            "access_token": "AT",
            "refresh_token": "NA_RT",
            "expires_at": expires_at,
        }))
        .expect("fixture must deserialize")
    }

    #[test]
    fn is_fresh_is_false_inside_the_skew_window() {
        let clock = FixedClock::new(1_000);
        // 만료가 30초 뒤 = skew(60초) 안쪽 → 미리 갱신해야 하므로 fresh 아님
        assert!(!token_set_expiring_at(1_030).is_fresh(&clock, EXPIRY_SKEW));
        // 만료가 90초 뒤 = skew 바깥 → fresh
        assert!(token_set_expiring_at(1_090).is_fresh(&clock, EXPIRY_SKEW));
    }

    #[test]
    fn is_fresh_is_false_for_already_expired_token_without_underflow() {
        let clock = FixedClock::new(2_000);
        // expires_at < now — u64 뺄셈이 언더플로하면 panic 하거나 거대한 값이 되어 fresh 로 오판한다.
        assert!(!token_set_expiring_at(1_000).is_fresh(&clock, EXPIRY_SKEW));
        assert!(!token_set_expiring_at(0).is_fresh(&clock, EXPIRY_SKEW));
    }

    #[test]
    fn debug_never_prints_token_material() {
        let ts = token_set_expiring_at(1_000);
        let rendered = format!("{ts:?}");
        assert!(!rendered.contains("AT"), "access token leaked: {rendered}");
        assert!(!rendered.contains("NA_RT"), "refresh token leaked: {rendered}");
        assert!(rendered.contains("[REDACTED]"), "no redaction marker: {rendered}");
    }

    #[test]
    fn unknown_fields_survive_a_round_trip() {
        // 신버전 fleetwire 가 쓴 파일을 구버전이 읽고 되쓸 때 필드를 잃지 않아야 한다.
        let json = r#"{"access_token":"AT","refresh_token":"NA_RT","expires_at":1000,"future_field":42}"#;
        let ts: TokenSet = serde_json::from_str(json).expect("must deserialize");
        let back = serde_json::to_value(&ts).expect("must serialize");
        assert_eq!(back["future_field"], serde_json::json!(42));
    }
}
```

- [ ] **Step 2: 실패 확인**

Run: `cargo test -p fleetwire clock:: auth::store::`
Expected: FAIL — `cannot find type 'SystemClock'`, `cannot find type 'TokenSet'` 등 컴파일 에러

- [ ] **Step 3: 구현**

`crates/fleetwire/src/clock.rs`:
```rust
//! 시간 주입 — 만료 판정을 실제 sleep 없이 테스트하기 위한 경계.

/// 현재 시각 공급자.
///
/// 토큰 만료 판정이 벽시계에 직접 의존하면 테스트가 실제 sleep 을 하거나
/// 시스템 시계를 건드려야 한다. 이 trait 이 그 결합을 끊는다.
pub trait Clock: Send + Sync {
    /// Unix epoch 이후 경과 초.
    fn now_unix(&self) -> u64;
}

/// 시스템 벽시계를 쓰는 기본 [`Clock`].
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix(&self) -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            // 시스템 시계가 epoch 이전이면 0 — panic 하지 않는다.
            // 0 은 "모든 토큰이 만료됨" 으로 읽혀 안전한 방향(refresh 시도)이다.
            .unwrap_or(0)
    }
}

#[cfg(test)]
pub(crate) use test_clock::FixedClock;

#[cfg(test)]
mod test_clock {
    use super::Clock;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// 테스트용 수동 시계.
    pub(crate) struct FixedClock(AtomicU64);

    impl FixedClock {
        pub(crate) fn new(now: u64) -> Self {
            Self(AtomicU64::new(now))
        }

        pub(crate) fn advance(&self, secs: u64) {
            self.0.fetch_add(secs, Ordering::SeqCst);
        }
    }

    impl Clock for FixedClock {
        fn now_unix(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }
}
```

`crates/fleetwire/src/auth/store.rs` 에 추가(모듈 doc 아래):
```rust
use crate::clock::Clock;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::time::Duration;

/// 토큰을 "곧 만료" 로 보는 여유분.
///
/// 요청이 나가는 도중에 만료되는 것을 막는다. Tesla access token 수명이
/// 8시간대이므로 60초는 약 0.2% 의 여유다.
pub(crate) const EXPIRY_SKEW: Duration = Duration::from_secs(60);

/// 디스크에 보관되는 토큰 한 벌.
///
/// `Debug` 는 파생하지 않고 손으로 구현한다 — 파생하면 access/refresh token 이
/// 그대로 찍힌다(REQ-18). 필드를 추가할 때 `Debug` 구현도 함께 갱신할 것.
#[derive(Serialize, Deserialize)]
pub(crate) struct TokenSet {
    access_token: String,
    refresh_token: String,
    /// Unix epoch 이후 경과 초. 안정적 JSON 을 위해 정수로 저장한다.
    expires_at: u64,
    /// 이 버전이 모르는 필드를 보존한다 — 신버전이 쓴 파일을 구버전이
    /// 읽고 되쓸 때 필드를 잃지 않기 위해서다.
    #[serde(flatten)]
    extra: Map<String, Value>,
}

impl core::fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TokenSet")
            .field("access_token", &"[REDACTED]")
            .field("refresh_token", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .field("extra_keys", &self.extra.len())
            .finish()
    }
}

impl TokenSet {
    /// 현재 access token.
    pub(crate) fn access_token(&self) -> &str {
        &self.access_token
    }

    /// 현재 refresh token.
    pub(crate) fn refresh_token(&self) -> &str {
        &self.refresh_token
    }

    /// `skew` 여유를 두고도 아직 유효한가.
    ///
    /// 이미 만료된 토큰에서 u64 언더플로가 나지 않도록 뺄셈이 아니라
    /// 덧셈으로 비교한다.
    pub(crate) fn is_fresh(&self, clock: &dyn Clock, skew: Duration) -> bool {
        self.expires_at > clock.now_unix().saturating_add(skew.as_secs())
    }
}
```

- [ ] **Step 4: 통과 확인**

Run: `cargo test -p fleetwire clock:: auth::store::`
Expected: PASS — 신규 6개

- [ ] **Step 5: 커밋**

```bash
git add crates/fleetwire/src/clock.rs crates/fleetwire/src/auth/store.rs
git commit -m "feat(a2): Clock 주입 + TokenSet(redact Debug·extra 보존·만료 skew)"
```

---

### Task 4: 안전한 토큰 파일 읽기·쓰기 (REQ-16)

**Files:**
- Modify: `crates/fleetwire/src/auth/store.rs`
- Test: 동파일

**Interfaces:**
- Consumes: `TokenSet` (Task 3)
- Produces:
  - `pub(crate) struct TokenStore { dir: PathBuf }`
  - `impl TokenStore`: `pub(crate) fn new(dir: PathBuf) -> Self`, `pub(crate) fn token_path(&self) -> PathBuf`, `pub(crate) fn lock_path(&self) -> PathBuf`, `pub(crate) fn load(&self) -> Result<TokenSet, StoreError>`, `pub(crate) fn store(&self, set: &TokenSet) -> Result<(), StoreError>`
  - `pub(crate) enum StoreError { NotFound, Symlink, BadPermissions, Io(std::io::Error), Malformed }`

**외부 크레이트가 필요 없다:** `create_new(true)`(=`O_CREAT|O_EXCL`), `OpenOptionsExt::mode(0o600)`, `MetadataExt::{ino,dev}`, `File::sync_all`, `fs::rename` — 전부 `std::os::unix` 이고 `unsafe` 가 없다.

**⚠️ `O_NOFOLLOW` 를 쓰지 않는 이유(pre-flight 스캔에서 정정):** `custom_flags(O_NOFOLLOW)` 를 쓰려면 상수값을 직접 박아야 하는데(std 에 상수가 없다) Linux 에서 아키텍처마다 값이 다르다(alpha·mips 등). 이식성 버그를 만들면서까지 쓸 이유가 없다 — **POSIX 가 `O_CREAT|O_EXCL` 은 경로가 심볼릭 링크면 링크 내용과 무관하게 `EEXIST` 로 실패한다고 규정**하므로, 임시 파일의 symlink 방어는 `create_new(true)` 만으로 이미 성립한다.

읽기 경로의 symlink 방어는 **TOCTOU 를 닫는 방식**으로 한다: `symlink_metadata` 로 링크를 거부한 뒤 파일을 열고, **연 핸들의 `ino`/`dev` 가 검사 때와 같은지 확인**한다. 검사와 열기 사이에 누가 파일을 바꿔치기했다면 여기서 걸린다. 권한 검사도 경로가 아니라 **연 핸들의 메타데이터**로 한다.

**저장 순서(설계 §7.4):** ①안전성 검사 → ②`O_CREAT|O_EXCL`(=`create_new(true)`) mode 0600 임시 파일 → ③쓰기 + 파일 `fsync` → ④원자적 `rename` → ⑤부모 디렉터리 `fsync`. (설계 §7.4 는 `O_NOFOLLOW` 도 적었으나 pre-flight 에서 제거 — 아래 T4 머리말 참조.)

- [ ] **Step 1: 실패 테스트 작성**

`crates/fleetwire/src/auth/store.rs` 의 `mod tests` 에 추가:
```rust
    use std::io::Write as _;
    use std::os::unix::fs::PermissionsExt as _;

    /// 프로세스 전역 상태(TMPDIR)를 건드리지 않는 임시 디렉터리.
    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let base = std::env::temp_dir().join(format!(
            "fleetwire-test-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&base).expect("temp dir");
        base
    }

    #[test]
    fn store_then_load_round_trips_and_file_is_0600() {
        let dir = temp_dir("roundtrip");
        let store = TokenStore::new(dir.clone());
        let ts = token_set_expiring_at(1_234);

        store.store(&ts).expect("store must succeed");
        let back = store.load().expect("load must succeed");

        assert_eq!(back.access_token(), "AT");
        assert_eq!(back.refresh_token(), "NA_RT");

        let mode = std::fs::metadata(store.token_path())
            .expect("token file must exist")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "token file mode must be 0600, got {mode:o}");
    }

    #[test]
    fn store_leaves_no_temp_file_behind() {
        let dir = temp_dir("notemp");
        let store = TokenStore::new(dir.clone());
        store.store(&token_set_expiring_at(1)).expect("store");

        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .expect("readdir")
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != "tokens.json")
            .collect();
        assert_eq!(leftovers, Vec::<String>::new(), "임시 파일이 남았다");
    }

    #[test]
    fn load_refuses_a_symlinked_token_file() {
        let dir = temp_dir("symlink");
        let real = dir.join("elsewhere.json");
        std::fs::write(&real, r#"{"access_token":"AT","refresh_token":"NA_RT","expires_at":1}"#)
            .expect("write decoy");
        let link = dir.join("tokens.json");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        let store = TokenStore::new(dir);
        assert!(
            matches!(store.load(), Err(StoreError::Symlink)),
            "symlink 을 통한 읽기를 거부해야 한다"
        );
    }

    #[test]
    fn load_reports_not_found_distinctly_from_io_error() {
        let store = TokenStore::new(temp_dir("missing"));
        assert!(matches!(store.load(), Err(StoreError::NotFound)));
    }

    #[test]
    fn load_reports_malformed_without_echoing_file_contents() {
        let dir = temp_dir("malformed");
        std::fs::write(dir.join("tokens.json"), "SECRET_NOT_JSON").expect("write");
        let err = TokenStore::new(dir).load().expect_err("must fail");
        assert!(matches!(err, StoreError::Malformed));
        let rendered = format!("{err}");
        assert!(
            !rendered.contains("SECRET_NOT_JSON"),
            "파일 내용이 오류에 새어나왔다: {rendered}"
        );
    }

    #[test]
    fn store_overwrites_atomically_leaving_previous_content_readable_until_swap() {
        let dir = temp_dir("atomic");
        let store = TokenStore::new(dir);
        store.store(&token_set_expiring_at(111)).expect("first");
        store.store(&token_set_expiring_at(222)).expect("second");

        let raw = std::fs::read_to_string(store.token_path()).expect("read");
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("valid json");
        assert_eq!(parsed["expires_at"], serde_json::json!(222));
    }
```

- [ ] **Step 2: 실패 확인**

Run: `cargo test -p fleetwire auth::store::`
Expected: FAIL — `cannot find type 'TokenStore'`

- [ ] **Step 3: 구현**

`crates/fleetwire/src/auth/store.rs` 에 추가:
```rust
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// 토큰 파일 이름.
pub(crate) const TOKEN_FILE: &str = "tokens.json";
/// 락 파일 이름. 토큰과 **같은 디렉터리**여야 한다(락은 경로가 아니라 inode 기준).
pub(crate) const LOCK_FILE: &str = "tokens.lock";

/// 토큰 저장소 접근 실패.
///
/// 어떤 variant 도 파일 내용이나 토큰 값을 담지 않는다(REQ-18).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub(crate) enum StoreError {
    /// 토큰 파일이 없다 — 아직 로그인하지 않았다는 뜻이다.
    #[error("token file not found")]
    NotFound,
    /// 토큰 경로가 symlink 다. 따라가지 않고 거부한다.
    #[error("token path is a symbolic link")]
    Symlink,
    /// 토큰 파일 권한이 0600 이 아니다.
    #[error("token file permissions are too permissive")]
    BadPermissions,
    /// 파일 내용이 유효한 토큰 JSON 이 아니다. 내용은 담지 않는다.
    #[error("token file is malformed")]
    Malformed,
    /// 그 밖의 입출력 오류.
    #[error("token store io error")]
    Io(#[from] std::io::Error),
}

/// 토큰 디렉터리 한 곳에 대한 안전한 읽기·쓰기.
#[derive(Debug, Clone)]
pub(crate) struct TokenStore {
    dir: PathBuf,
}

impl TokenStore {
    /// 주어진 디렉터리를 쓰는 저장소를 만든다. 디렉터리는 `store` 시점에 생성된다.
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// 토큰 파일 경로.
    pub(crate) fn token_path(&self) -> PathBuf {
        self.dir.join(TOKEN_FILE)
    }

    /// 락 파일 경로.
    pub(crate) fn lock_path(&self) -> PathBuf {
        self.dir.join(LOCK_FILE)
    }

    /// 토큰을 읽는다. symlink·과대 권한·손상 파일을 각각 구분해 거부한다.
    ///
    /// 검사와 열기 사이의 TOCTOU 를 닫기 위해, 연 핸들의 `ino`/`dev` 가 검사 때와
    /// 같은지 확인한다. 그 사이에 누가 경로를 바꿔치기했다면 여기서 걸린다.
    pub(crate) fn load(&self) -> Result<TokenSet, StoreError> {
        use std::io::Read as _;
        use std::os::unix::fs::MetadataExt as _;

        let path = self.token_path();
        let pre = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(StoreError::NotFound),
            Err(e) => return Err(StoreError::Io(e)),
        };
        // symlink_metadata 는 링크를 따라가지 않으므로 링크 자체를 판별할 수 있다.
        if pre.file_type().is_symlink() {
            return Err(StoreError::Symlink);
        }
        if !pre.file_type().is_file() {
            return Err(StoreError::Malformed);
        }

        let mut file = File::open(&path)?;
        // 열린 것이 방금 검사한 바로 그 파일인가.
        let opened = file.metadata()?;
        if opened.ino() != pre.ino() || opened.dev() != pre.dev() {
            return Err(StoreError::Symlink);
        }
        // 권한은 경로가 아니라 **연 핸들** 기준으로 본다.
        if opened.permissions().mode() & 0o077 != 0 {
            return Err(StoreError::BadPermissions);
        }

        let mut raw = String::new();
        file.read_to_string(&mut raw)?;
        serde_json::from_str(&raw).map_err(|_| StoreError::Malformed)
    }

    /// disk-first 저장: 임시 파일 → fsync → 원자적 rename → 부모 디렉터리 fsync.
    ///
    /// 이 함수가 `Ok` 를 돌려준 뒤에만 호출자가 메모리 토큰을 교체해야 한다.
    /// 저장 실패 상태로 새 토큰을 쓰면 다음 갱신에서 재-OAuth 가 필요해진다.
    pub(crate) fn store(&self, set: &TokenSet) -> Result<(), StoreError> {
        std::fs::create_dir_all(&self.dir)?;
        let final_path = self.token_path();

        if let Ok(meta) = std::fs::symlink_metadata(&final_path)
            && meta.file_type().is_symlink()
        {
            return Err(StoreError::Symlink);
        }

        let tmp_path = self.dir.join(format!(
            ".{TOKEN_FILE}.tmp.{}.{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        // 이전 실행이 남긴 잔여물이 있으면 O_EXCL 이 실패하므로 먼저 치운다.
        let _ = std::fs::remove_file(&tmp_path);

        let json = serde_json::to_vec(set).map_err(|_| StoreError::Malformed)?;
        {
            // create_new(true) = O_CREAT|O_EXCL. POSIX 상 경로가 symlink 면 링크
            // 내용과 무관하게 EEXIST 로 실패하므로, 이것만으로 임시 파일의
            // symlink 방어가 성립한다(O_NOFOLLOW 불필요 — 계획 T4 머리말 참조).
            let mut f = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp_path)?;
            f.write_all(&json)?;
            f.sync_all()?;
        }

        // rename 은 대상이 symlink 여도 **따라가지 않고** 링크 자체를 대체한다.
        // 위의 명시적 검사가 있으므로 여기 도달하면 대상은 symlink 가 아니다.
        std::fs::rename(&tmp_path, &final_path)?;
        fsync_dir(&self.dir)?;
        Ok(())
    }
}

/// 디렉터리 엔트리를 디스크에 확정한다. rename 이후 부모를 fsync 해야
/// 크래시 뒤에도 새 이름이 살아남는다.
fn fsync_dir(dir: &Path) -> std::io::Result<()> {
    File::open(dir)?.sync_all()
}
```

⚠️ `let ... && ...` (let-chain) 은 edition 2024 에서 안정이다. 컴파일이 안 되면 중첩 `if` 로 풀 것.

- [ ] **Step 4: 통과 확인**

Run: `cargo test -p fleetwire auth::store::`
Expected: PASS — 신규 6개 + Task 3 의 4개

- [ ] **Step 5: 커밋**

```bash
git add crates/fleetwire/src/auth/store.rs
git commit -m "feat(a2): 토큰 파일 안전 저장 — 0600·create_new·atomic+fsync·symlink 거부 (REQ-16)"
```

---

### Task 5: 고정 inode 락 (REQ-15 의 프로세스 간 절반)

**Files:**
- Modify: `crates/fleetwire/src/auth/lock.rs`
- Test: 동파일

**Interfaces:**
- Produces:
  - `pub(crate) struct LockGuard { file: std::fs::File }` — `Drop` 에서 `unlock`
  - `pub(crate) async fn acquire(path: &Path, deadline: Duration) -> Result<LockGuard, LockError>`
  - `pub(crate) enum LockError { Timeout, Io(std::io::Error) }`
  - `pub(crate) const LOCK_DEADLINE: Duration` (10초), `pub(crate) const LOCK_RETRY_INITIAL: Duration` (5ms), `pub(crate) const LOCK_RETRY_MAX: Duration` (200ms)

**왜 `try_lock` + 재시도인가:** flock 은 블로킹 syscall 이다. tokio Mutex 를 쥔 채 블로킹 락을 걸면 런타임 워커가 멈춘다. 그리고 flock 에는 **timeout 인자가 없다** — 설계 §7.3 의 "timeout 포함 flock" 은 문자 그대로는 존재하지 않는다. 논블로킹 `try_lock` + async 재시도 + deadline 이 런타임을 막지 않으면서 REQ-15 의 "유한 timeout" 을 만드는 유일한 형태다. `spawn_blocking` 은 블로킹 락을 취소할 수 없어 더 나쁘다.

`std::fs::TryLockError` 는 `WouldBlock`(경합 — 재시도)과 `Error(io::Error)`(실제 실패 — 중단)를 구분해 준다. 이 구분을 뭉개면 디스크 오류에서 deadline 까지 무의미하게 재시도한다.

**락 파일은 절대 삭제하지 않는다.** 삭제하면 기존 inode holder 와 새 inode holder 가 동시에 생겨 상호배제가 깨진다(설계 §7.3).

- [ ] **Step 1: 실패 테스트 작성**

`crates/fleetwire/src/auth/lock.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn temp_lock_path(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "fleetwire-lock-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir.join("tokens.lock")
    }

    #[tokio::test]
    async fn acquire_succeeds_on_a_fresh_path_and_creates_the_lock_file() {
        let path = temp_lock_path("fresh");
        let guard = acquire(&path, LOCK_DEADLINE).await.expect("must acquire");
        assert!(path.exists(), "락 파일이 생성돼야 한다");
        drop(guard);
        assert!(path.exists(), "락 해제 후에도 파일은 남아야 한다 (고정 inode)");
    }

    #[tokio::test]
    async fn a_second_holder_times_out_while_the_first_holds_it() {
        // flock 은 open file description 단위라, 같은 프로세스에서 두 번 열면
        // 서로 배타된다 → 실제 자식 프로세스 없이 "형제 프로세스" 를 재현한다.
        let path = temp_lock_path("contended");
        let first = acquire(&path, LOCK_DEADLINE).await.expect("first must acquire");

        let started = std::time::Instant::now();
        let second = acquire(&path, Duration::from_millis(300)).await;
        let elapsed = started.elapsed();

        assert!(matches!(second, Err(LockError::Timeout)), "두 번째는 타임아웃해야 한다");
        assert!(elapsed >= Duration::from_millis(300), "deadline 이전에 포기했다: {elapsed:?}");
        assert!(elapsed < Duration::from_secs(5), "deadline 을 크게 초과했다: {elapsed:?}");

        drop(first);
    }

    #[tokio::test]
    async fn the_lock_is_reacquirable_after_the_guard_drops() {
        let path = temp_lock_path("reacquire");
        drop(acquire(&path, LOCK_DEADLINE).await.expect("first"));
        // Drop 이 unlock 하지 않으면 여기서 타임아웃한다.
        let second = acquire(&path, Duration::from_millis(500)).await;
        assert!(second.is_ok(), "Drop 이 락을 풀지 않았다");
    }
}
```

- [ ] **Step 2: 실패 확인**

Run: `cargo test -p fleetwire auth::lock::`
Expected: FAIL — `cannot find function 'acquire'`. `tokio::test` 매크로가 없다는 에러가 나면 `tokio` 의 `macros`/`rt` feature 가 **dev-dependencies** 에만 필요하다는 뜻이다 — Step 3 에서 처리한다.

- [ ] **Step 3: 구현**

`crates/fleetwire/Cargo.toml` 에 dev-dependency 추가(런타임 의존성은 그대로 `sync`+`time` 만 유지):
```toml
[dev-dependencies]
tokio = { version = "1", default-features = false, features = ["macros", "rt", "rt-multi-thread", "time", "sync"] }
```

`crates/fleetwire/src/auth/lock.rs`:
```rust
//! 고정 inode 파일 락 — 프로세스 간 refresh 상호배제.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;
use std::time::{Duration, Instant};

/// 락 획득 전체 deadline.
pub(crate) const LOCK_DEADLINE: Duration = Duration::from_secs(10);
/// 첫 재시도 간격.
pub(crate) const LOCK_RETRY_INITIAL: Duration = Duration::from_millis(5);
/// 재시도 간격 상한.
pub(crate) const LOCK_RETRY_MAX: Duration = Duration::from_millis(200);

/// 락 획득 실패.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub(crate) enum LockError {
    /// deadline 안에 락을 얻지 못했다. 형제 프로세스가 오래 쥐고 있다는 뜻이다.
    #[error("timed out acquiring the token lock")]
    Timeout,
    /// 락 파일 자체에 대한 입출력 오류.
    #[error("token lock io error")]
    Io(#[from] std::io::Error),
}

/// 락을 쥔 동안 살아 있는 가드. `Drop` 에서 해제한다.
///
/// ⚠️ 이 타입은 락 **파일을 삭제하지 않는다**. 삭제하면 기존 inode 를 잡고 있는
/// 프로세스와 새 inode 를 잡는 프로세스가 동시에 생겨 상호배제가 깨진다.
#[derive(Debug)]
pub(crate) struct LockGuard {
    file: File,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        // 실패해도 할 수 있는 일이 없다. 프로세스가 죽으면 커널이 어차피 해제한다.
        let _ = File::unlock(&self.file);
    }
}

/// `deadline` 안에 배타 락을 얻는다.
///
/// 블로킹 `lock()` 대신 논블로킹 `try_lock()` 을 쓰고 async 로 재시도한다 —
/// 블로킹 syscall 은 tokio 워커 스레드를 멈추고, flock 에는 timeout 인자가 없다.
pub(crate) async fn acquire(path: &Path, deadline: Duration) -> Result<LockGuard, LockError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    // 고정 inode: 있으면 열고 없으면 만든다. truncate 하지 않는다(내용을 쓰지 않는다).
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;

    let started = Instant::now();
    let mut backoff = LOCK_RETRY_INITIAL;
    loop {
        match File::try_lock(&file) {
            Ok(()) => return Ok(LockGuard { file }),
            // 경합 — 재시도한다.
            Err(TryLockError::WouldBlock) => {}
            // 실제 입출력 오류 — 재시도해도 나아지지 않는다.
            Err(TryLockError::Error(e)) => return Err(LockError::Io(e)),
        }
        if started.elapsed() >= deadline {
            return Err(LockError::Timeout);
        }
        // 남은 시간을 넘겨 자지 않는다 — deadline 초과폭을 최소화한다.
        let remaining = deadline.saturating_sub(started.elapsed());
        tokio::time::sleep(backoff.min(remaining)).await;
        backoff = (backoff * 2).min(LOCK_RETRY_MAX);
    }
}
```

- [ ] **Step 4: 통과 확인**

Run: `cargo test -p fleetwire auth::lock::`
Expected: PASS — 신규 3개

- [ ] **Step 5: 커밋**

```bash
git add crates/fleetwire/Cargo.toml crates/fleetwire/src/auth/lock.rs
git commit -m "feat(a2): 고정 inode 락 — try_lock + async 재시도 + deadline (REQ-15)"
```

---

### Task 6: JWT scp advisory 관찰 (REQ-8)

**Files:**
- Modify: `crates/fleetwire/src/auth/jwt.rs`
- Test: 동파일

**Interfaces:**
- Consumes: `Scope::from_str` (Task 2), `crate::scope::ScopeExpr`
- Produces:
  - `pub(crate) enum UnverifiedScopeObservation { ContainsRequiredScope, DoesNotContainRequiredScope, Indeterminate }`
  - `pub(crate) fn observe_scopes(access_token: &str, required: &ScopeExpr) -> UnverifiedScopeObservation`
  - `pub(crate) const MAX_JWT_PAYLOAD_BYTES: usize` (8192)

**이것은 보안 경계가 아니다.** 공격자가 토큰을 통제하면 `scp` 에 뭐든 넣을 수 있고, 그러면 요청이 나가고 서버가 거부한다. 즉 **adversarial cost-control 이 아니라 accidental misconfiguration 에 대한 비용 절약**이다. 최종 권한 판정은 항상 Tesla 서버가 한다.

**3상태인 이유:** `bool` 은 `false` 가 "권한 없음" 인지 "파싱 실패" 인지 "opaque 토큰" 인지 구분하지 못하고 authz 로 재사용되기 쉽다. `Option` 도 fail-open 이 관례에 머문다. 3상태는 fail-open 이 **타입**이 된다 — `DoesNotContainRequiredScope` 만 요청을 건너뛴다.

**실측 근거(2026-07-31, 실제 Tesla 토큰):** `alg=RS256`, 3세그먼트, `scp` 는 문자열 **배열**이고 값은 `["offline_access","openid","vehicle_device_data","vehicle_location"]`. `offline_access` 는 `Scope` 에 모델링돼 있지 않다.

- [ ] **Step 1: 실패 테스트 작성**

`crates/fleetwire/src/auth/jwt.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::scope::{Scope, ScopeExpr};
    use base64::Engine as _;

    /// 테스트용 JWT 를 만든다. 서명은 검증하지 않으므로 아무 값이나 넣는다.
    fn jwt_with_payload(payload: &serde_json::Value) -> String {
        let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let header = engine.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
        let body = engine.encode(serde_json::to_vec(payload).expect("payload"));
        format!("{header}.{body}.not-a-real-signature")
    }

    fn required_device_data() -> ScopeExpr {
        ScopeExpr::One(Scope::VehicleDeviceData)
    }

    #[test]
    fn real_tesla_scp_shape_is_recognized_including_unmodeled_scopes() {
        // 2026-07-31 실측 토큰의 scp 그대로. offline_access 는 모델링돼 있지 않지만
        // 그것 때문에 판정이 Indeterminate 로 무너지면 안 된다.
        let token = jwt_with_payload(&serde_json::json!({
            "scp": ["offline_access", "openid", "vehicle_device_data", "vehicle_location"]
        }));
        assert_eq!(
            observe_scopes(&token, &required_device_data()),
            UnverifiedScopeObservation::ContainsRequiredScope
        );
    }

    #[test]
    fn missing_required_scope_is_the_only_path_that_skips_a_request() {
        let token = jwt_with_payload(&serde_json::json!({ "scp": ["openid"] }));
        assert_eq!(
            observe_scopes(&token, &required_device_data()),
            UnverifiedScopeObservation::DoesNotContainRequiredScope
        );
    }

    #[test]
    fn every_ambiguous_shape_is_indeterminate_not_a_skip() {
        let opaque = "NA_9f2b1c7d8e4a5b6c7d8e9f0a";
        let two_segments = "eyJhbGciOiJSUzI1NiJ9.eyJzY3AiOltdfQ";
        let bad_base64 = "eyJhbGciOiJSUzI1NiJ9.!!!not-base64!!!.sig";
        let not_json = {
            let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
            format!("eyJhbGciOiJSUzI1NiJ9.{}.sig", engine.encode(b"plain text"))
        };
        let scp_absent = jwt_with_payload(&serde_json::json!({ "sub": "abc" }));
        let scp_is_string = jwt_with_payload(&serde_json::json!({ "scp": "vehicle_device_data" }));
        let scp_has_non_strings = jwt_with_payload(&serde_json::json!({ "scp": [1, 2] }));

        for (label, token) in [
            ("opaque token", opaque.to_owned()),
            ("two segments", two_segments.to_owned()),
            ("undecodable payload", bad_base64.to_owned()),
            ("payload is not json", not_json),
            ("scp claim absent", scp_absent),
            ("scp is a string not an array", scp_is_string),
            ("scp holds non-strings", scp_has_non_strings),
        ] {
            assert_eq!(
                observe_scopes(&token, &required_device_data()),
                UnverifiedScopeObservation::Indeterminate,
                "{label} 은 Indeterminate 여야 한다 — 요청을 막으면 fail-open 이 깨진다"
            );
        }
    }

    #[test]
    fn an_oversized_payload_is_indeterminate_rather_than_allocated() {
        let huge = jwt_with_payload(&serde_json::json!({
            "scp": ["vehicle_device_data"],
            "padding": "A".repeat(MAX_JWT_PAYLOAD_BYTES * 2),
        }));
        assert_eq!(
            observe_scopes(&huge, &required_device_data()),
            UnverifiedScopeObservation::Indeterminate
        );
    }

    #[test]
    fn all_expression_requires_every_scope() {
        let token = jwt_with_payload(&serde_json::json!({
            "scp": ["vehicle_device_data"]
        }));
        // ⚠️ ScopeExpr::All 은 Vec<ScopeExpr> 를 받는다 — Vec<Scope> 가 아니다.
        let required = ScopeExpr::All(vec![
            ScopeExpr::One(Scope::VehicleDeviceData),
            ScopeExpr::One(Scope::VehicleLocation),
        ]);
        assert_eq!(
            observe_scopes(&token, &required),
            UnverifiedScopeObservation::DoesNotContainRequiredScope
        );
    }
}
```

- [ ] **Step 2: 실패 확인**

Run: `cargo test -p fleetwire auth::jwt::`
Expected: FAIL — `cannot find function 'observe_scopes'`

- [ ] **Step 3: 구현**

`crates/fleetwire/src/auth/jwt.rs`:
```rust
//! 검증하지 않는 JWT payload 관찰 — 스코프 advisory 선검사 전용.
//!
//! # 이것은 권한 판정이 아니다
//!
//! 여기서 읽는 값은 **서명이 검증되지 않았다**. 토큰을 통제하는 쪽은 `scp` 에
//! 무엇이든 넣을 수 있고, 그러면 요청이 그대로 나가서 서버가 거부한다. 따라서
//! 이 모듈은 **adversarial cost-control 이 아니라 accidental misconfiguration 에
//! 대한 비용 절약**이다. 최종 권한 판정은 언제나 Tesla 서버가 한다.
//!
//! 안전성은 단방향이다 — 스코프가 없다고 *관찰되면* 요청을 생략할 수 있지만,
//! 있다고 관찰되어도 권한이 있다고 결론 내리지 않는다.

use crate::scope::{Scope, ScopeExpr};
use base64::Engine as _;
use core::str::FromStr as _;

/// payload 디코드 상한. 이보다 크면 파싱하지 않고 [`UnverifiedScopeObservation::Indeterminate`].
pub(crate) const MAX_JWT_PAYLOAD_BYTES: usize = 8192;

/// 검증되지 않은 토큰에서 관찰한 스코프 충족 여부.
///
/// **`DoesNotContainRequiredScope` 만이 요청을 건너뛰게 한다.** 나머지 둘은
/// 모두 요청을 보낸다 — fail-open 을 관례가 아니라 타입으로 만든 것이다.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub(crate) enum UnverifiedScopeObservation {
    /// 요구 스코프가 관찰됐다. **권한이 있다는 뜻이 아니다.**
    ContainsRequiredScope,
    /// 요구 스코프가 확실히 없다. 유일하게 요청을 생략해도 되는 경우.
    DoesNotContainRequiredScope,
    /// 판단할 수 없다 — opaque 토큰, 파싱 실패, 예상 밖 구조 등. 요청을 보낸다.
    Indeterminate,
}

/// access token 의 `scp` 클레임을 검증 없이 관찰해 요구 스코프 충족 여부를 판정한다.
///
/// 토큰이 JWT 인지 여부를 이 함수가 **판정하지 않는다** — 호출자가 opt-in 으로
/// 선택했을 때만 호출된다. opaque 토큰이 우연히 JWT 처럼 디코딩될 수 있고,
/// 그것을 JWT 로 오인해 요청을 막으면 fail-open 원칙이 깨지기 때문이다.
pub(crate) fn observe_scopes(
    access_token: &str,
    required: &ScopeExpr,
) -> UnverifiedScopeObservation {
    use UnverifiedScopeObservation::Indeterminate;

    // compact JWS 는 정확히 3 세그먼트다. 그 외는 판단하지 않는다.
    let mut parts = access_token.split('.');
    let (Some(_header), Some(payload_b64), Some(_sig), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return Indeterminate;
    };

    // 디코드 **전에** 상한을 본다 — base64 는 3/4 로 줄어드니 넉넉히 잡아도 안전하다.
    if payload_b64.len() > MAX_JWT_PAYLOAD_BYTES {
        return Indeterminate;
    }

    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    let Ok(payload) = engine.decode(payload_b64) else {
        return Indeterminate;
    };
    if payload.len() > MAX_JWT_PAYLOAD_BYTES {
        return Indeterminate;
    }
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&payload) else {
        return Indeterminate;
    };

    // scp 는 문자열 배열이어야 한다(2026-07-31 실측). 그 외 형태는 판단하지 않는다.
    // 특히 **클레임 부재를 "스코프 없음" 으로 읽지 않는다** — 발급자 프로필이
    // "부재 = 스코프 없음" 을 명시할 때만 그렇게 볼 수 있고, 우리는 그 명시가 없다.
    let Some(items) = value.get("scp").and_then(serde_json::Value::as_array) else {
        return Indeterminate;
    };
    if items.iter().any(|v| !v.is_string()) {
        return Indeterminate;
    }

    // 모델링되지 않은 스코프(예: offline_access)는 버린다. 요구 스코프는 항상
    // 우리 엔드포인트 타입에서 오므로 알려진 값이고, 미지 항목을 버려도 판정이 틀리지 않는다.
    let held: Vec<Scope> = items
        .iter()
        .filter_map(serde_json::Value::as_str)
        .filter_map(|s| Scope::from_str(s).ok())
        .collect();

    if required.is_satisfied_by(&held) {
        UnverifiedScopeObservation::ContainsRequiredScope
    } else {
        UnverifiedScopeObservation::DoesNotContainRequiredScope
    }
}
```

- [ ] **Step 4: 통과 확인**

Run: `cargo test -p fleetwire auth::jwt::`
Expected: PASS — 신규 5개(그중 `every_ambiguous_shape_is_indeterminate_not_a_skip` 는 7가지 입력을 돈다)

- [ ] **Step 5: 커밋**

```bash
git add crates/fleetwire/src/auth/jwt.rs
git commit -m "feat(a2): JWT scp advisory 3상태 관찰 — fail-open 을 타입으로 (REQ-8)"
```

---

### Task 7: `TokenRefresher` + `TokenManager` single-flight (REQ-15/16)

**Files:**
- Modify: `crates/fleetwire/src/auth/mod.rs`
- Modify: `crates/fleetwire/src/lib.rs` (re-export)
- Test: `crates/fleetwire/src/auth/mod.rs` 동파일

**Interfaces:**
- Consumes: `TokenSet`·`TokenStore`·`StoreError`·`EXPIRY_SKEW` (T3/T4), `lock::{acquire, LockError, LOCK_DEADLINE}` (T5), `Clock`·`SystemClock` (T3)
- Produces:
  - `pub struct Refreshed { pub access_token: String, pub refresh_token: String, pub expires_in_secs: u64 }`
  - `pub trait TokenRefresher: Send + Sync { fn refresh<'a>(&'a self, refresh_token: &'a str) -> Pin<Box<dyn Future<Output = Result<Refreshed, crate::error::Error>> + Send + 'a>>; }`
  - `pub struct TokenManager` + `TokenManager::new(dir: PathBuf, refresher: Arc<dyn TokenRefresher>) -> Self`, `pub fn with_clock(self, clock: Arc<dyn Clock>) -> Self`, `pub async fn access_token(&self) -> Result<String, crate::error::Error>`

**왜 `Arc<dyn>` + 수동 박싱인가:** AFIT(async fn in trait)은 1.75 부터 안정이지만 dyn-safe 가 아니다. 제네릭 `TokenManager<R>` 로 가면 나중에 `Client` 타입 시그니처까지 오염되고 빌더 API 가 지저분해진다. refresh 는 드물게 일어나므로 Box 한 번은 무의미하고, `async-trait` 크레이트를 안 들여도 된다.

**오케스트레이션 순서(설계 §7.3):**
```
 1. fast path — 메모리 토큰이 fresh 하면 락 없이 반환
 2. tokio Mutex 획득 (await 구간 전체 보유)
 3. 락 안에서 재확인 — 다른 태스크가 이미 갱신했으면 반환
 4. tokens.lock 획득 (T5)
 5. 디스크 재읽기 → fresh? → yes: Adopted, POST 없음
 6. no → refresher.refresh() 정확히 1회
 7. disk-first 저장 (T4) — 실패하면 치명적 오류, 메모리 미갱신
 8. 저장 성공 후에만 메모리 교체
```

- [ ] **Step 1: 실패 테스트 작성**

`crates/fleetwire/src/auth/mod.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!(
            "fleetwire-mgr-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&d).expect("temp dir");
        d
    }

    /// 호출 횟수를 세는 refresher. single-flight 의 핵심 단언 대상이다.
    struct CountingRefresher {
        calls: AtomicUsize,
        new_access: String,
    }

    impl CountingRefresher {
        fn new(new_access: &str) -> Self {
            Self { calls: AtomicUsize::new(0), new_access: new_access.to_owned() }
        }
        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl TokenRefresher for CountingRefresher {
        fn refresh<'a>(
            &'a self,
            _refresh_token: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<Refreshed, crate::error::Error>> + Send + 'a>> {
            Box::pin(async move {
                self.calls.fetch_add(1, Ordering::SeqCst);
                // 실제 네트워크 지연을 흉내내 동시 진입을 벌린다.
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                Ok(Refreshed {
                    access_token: self.new_access.clone(),
                    refresh_token: "NA_ROTATED".to_owned(),
                    expires_in_secs: 28_800,
                })
            })
        }
    }

    fn seed_expired_token(dir: &std::path::Path) {
        std::fs::write(
            dir.join("tokens.json"),
            r#"{"access_token":"OLD","refresh_token":"NA_OLD","expires_at":1}"#,
        )
        .expect("seed");
        std::fs::set_permissions(
            dir.join("tokens.json"),
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .expect("chmod");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_callers_collapse_into_exactly_one_refresh() {
        let dir = temp_dir("singleflight");
        seed_expired_token(&dir);
        let refresher = Arc::new(CountingRefresher::new("NEW"));
        let mgr = Arc::new(TokenManager::new(dir, refresher.clone()));

        let mut handles = Vec::new();
        for _ in 0..16 {
            let m = Arc::clone(&mgr);
            handles.push(tokio::spawn(async move { m.access_token().await }));
        }
        for h in handles {
            assert_eq!(h.await.expect("join").expect("access_token"), "NEW");
        }
        assert_eq!(refresher.call_count(), 1, "refresh 가 single-flight 로 합쳐지지 않았다");
    }

    #[tokio::test]
    async fn a_fresh_token_on_disk_is_used_without_calling_refresh() {
        let dir = temp_dir("nofresh");
        let far_future = 4_000_000_000u64;
        std::fs::write(
            dir.join("tokens.json"),
            format!(
                r#"{{"access_token":"STILLGOOD","refresh_token":"NA_X","expires_at":{far_future}}}"#
            ),
        )
        .expect("seed");
        std::fs::set_permissions(
            dir.join("tokens.json"),
            std::os::unix::fs::PermissionsExt::from_mode(0o600),
        )
        .expect("chmod");

        let refresher = Arc::new(CountingRefresher::new("UNUSED"));
        let mgr = TokenManager::new(dir, refresher.clone());
        assert_eq!(mgr.access_token().await.expect("token"), "STILLGOOD");
        assert_eq!(refresher.call_count(), 0, "유효한 토큰인데 refresh 를 호출했다");
    }

    #[tokio::test]
    async fn the_rotated_refresh_token_is_persisted_before_the_new_access_token_is_returned() {
        let dir = temp_dir("rotation");
        seed_expired_token(&dir);
        let mgr = TokenManager::new(dir.clone(), Arc::new(CountingRefresher::new("NEW")));

        assert_eq!(mgr.access_token().await.expect("token"), "NEW");

        // 디스크가 이미 새 refresh token 을 갖고 있어야 한다 — 저장 전에 반환하면
        // 프로세스가 여기서 죽었을 때 회전된 토큰을 잃고 재-OAuth 가 필요해진다.
        let raw = std::fs::read_to_string(dir.join("tokens.json")).expect("read");
        let v: serde_json::Value = serde_json::from_str(&raw).expect("json");
        assert_eq!(v["refresh_token"], serde_json::json!("NA_ROTATED"));
        assert_eq!(v["access_token"], serde_json::json!("NEW"));
    }
}
```

- [ ] **Step 2: 실패 확인**

Run: `cargo test -p fleetwire auth::tests::`
Expected: FAIL — `cannot find type 'TokenManager'`

- [ ] **Step 3: 구현**

`crates/fleetwire/src/auth/mod.rs` (모듈 doc 아래 전체):
```rust
mod jwt;
mod lock;
mod store;

use crate::clock::{Clock, SystemClock};
use crate::error::Error;
use core::future::Future;
use core::pin::Pin;
use std::path::PathBuf;
use std::sync::Arc;
use store::{EXPIRY_SKEW, StoreError, TokenSet, TokenStore};

/// refresh 왕복이 돌려준 새 토큰 한 벌.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Refreshed {
    /// 새 access token.
    pub access_token: String,
    /// 새 refresh token. Tesla 는 회전 시 이전 체인 전체를 무효화한다.
    pub refresh_token: String,
    /// 지금부터 access token 이 만료될 때까지의 초.
    pub expires_in_secs: u64,
}

/// refresh 왕복 구현체.
///
/// A2 는 이 trait 을 **호출만** 한다 — 실제 HTTP POST 구현은 서브프로젝트 A3 가 준다.
/// 그래서 A2 전체가 네트워크 없이 테스트된다.
///
/// `async fn` 이 아니라 박싱된 future 를 돌려주는 이유: async fn in trait 은
/// dyn-safe 가 아니라서 `Arc<dyn TokenRefresher>` 로 담을 수 없다. refresh 는
/// 드물게 일어나므로 Box 한 번의 비용은 무의미하다.
pub trait TokenRefresher: Send + Sync {
    /// `refresh_token` 으로 새 토큰 한 벌을 받아온다.
    fn refresh<'a>(
        &'a self,
        refresh_token: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Refreshed, Error>> + Send + 'a>>;
}

/// 토큰 수명주기 관리자.
///
/// 프로세스 안에서는 `tokio::sync::Mutex`, 프로세스 사이에서는 고정 inode 파일
/// 락으로 refresh 를 single-flight 로 합친다(REQ-15).
pub struct TokenManager {
    store: TokenStore,
    refresher: Arc<dyn TokenRefresher>,
    clock: Arc<dyn Clock>,
    /// await 구간 전체에 걸쳐 보유하므로 std Mutex 가 아니라 tokio Mutex 여야 한다.
    /// ⚠️ 토큰은 회전하므로 `OnceCell`/`OnceLock` 에 넣지 않는다(REQ-15).
    cached: tokio::sync::Mutex<Option<TokenSet>>,
}

impl core::fmt::Debug for TokenManager {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // 캐시된 토큰을 찍지 않는다(REQ-18).
        f.debug_struct("TokenManager").finish_non_exhaustive()
    }
}

impl TokenManager {
    /// 토큰 디렉터리와 refresh 구현체로 관리자를 만든다.
    pub fn new(dir: PathBuf, refresher: Arc<dyn TokenRefresher>) -> Self {
        Self {
            store: TokenStore::new(dir),
            refresher,
            clock: Arc::new(SystemClock),
            cached: tokio::sync::Mutex::new(None),
        }
    }

    /// 시계를 교체한다(테스트용).
    #[must_use]
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// 유효한 access token 을 돌려준다. 필요하면 refresh 한다.
    pub async fn access_token(&self) -> Result<String, Error> {
        // 1. fast path — 캐시가 유효하면 파일 락을 건드리지 않는다.
        {
            let cached = self.cached.lock().await;
            if let Some(set) = cached.as_ref()
                && set.is_fresh(self.clock.as_ref(), EXPIRY_SKEW)
            {
                return Ok(set.access_token().to_owned());
            }
        }

        // 2. in-process single-flight — await 구간 전체에 걸쳐 보유한다.
        let mut cached = self.cached.lock().await;

        // 3. 락 안에서 재확인 — 기다리는 동안 다른 태스크가 갱신했을 수 있다.
        if let Some(set) = cached.as_ref()
            && set.is_fresh(self.clock.as_ref(), EXPIRY_SKEW)
        {
            return Ok(set.access_token().to_owned());
        }

        // 4. 프로세스 간 single-flight.
        let _guard = lock::acquire(&self.store.lock_path(), lock::LOCK_DEADLINE)
            .await
            .map_err(|e| Error::Auth { message: format!("token lock: {e}"), pricing: None })?;

        // 5. 락 아래에서 디스크를 다시 읽는다 — 형제 프로세스가 이미 갱신했을 수 있다.
        let on_disk = self
            .store
            .load()
            .map_err(|e| Error::Auth { message: format!("token store: {e}"), pricing: None })?;
        if on_disk.is_fresh(self.clock.as_ref(), EXPIRY_SKEW) {
            // Adopted — refresh POST 를 보내지 않는다.
            let token = on_disk.access_token().to_owned();
            *cached = Some(on_disk);
            return Ok(token);
        }

        // 6. 정확히 1회 refresh.
        let refreshed = self.refresher.refresh(on_disk.refresh_token()).await?;

        // 7. disk-first — 저장에 실패하면 새 토큰을 쓰지 않고 오류를 낸다.
        //    저장 못 한 회전 토큰을 쓰면 다음 갱신에서 재-OAuth 가 필요해진다.
        let expires_at = self.clock.now_unix().saturating_add(refreshed.expires_in_secs);
        let new_set = TokenSet::from_refreshed(&refreshed, expires_at);
        self.store.store(&new_set).map_err(|e| Error::Auth {
            message: format!("persisting rotated token: {e}"),
            pricing: None,
        })?;

        // 8. 저장 성공 후에만 메모리 교체.
        let token = new_set.access_token().to_owned();
        *cached = Some(new_set);
        Ok(token)
    }
}
```

`crates/fleetwire/src/auth/store.rs` 에 생성자를 추가:
```rust
impl TokenSet {
    /// refresh 결과로 새 토큰 한 벌을 만든다.
    pub(crate) fn from_refreshed(refreshed: &super::Refreshed, expires_at: u64) -> Self {
        Self {
            access_token: refreshed.access_token.clone(),
            refresh_token: refreshed.refresh_token.clone(),
            expires_at,
            extra: Map::new(),
        }
    }
}
```

`crates/fleetwire/src/lib.rs` 의 re-export 블록에 추가:
```rust
pub use auth::{Refreshed, TokenManager, TokenRefresher};
pub use clock::{Clock, SystemClock};
```

⚠️ `auth`·`clock` 모듈은 private(`mod auth;`)이지만 re-export 되는 타입은 public 이 되므로 **`#![deny(missing_docs)]` 가 적용된다** — 모든 pub item·pub 필드에 `///` 가 필요하다.

- [ ] **Step 4: 통과 확인**

Run:
```bash
cargo test -p fleetwire
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo clippy --locked -- -D warnings
cargo fmt --all -- --check
```
Expected: 전부 통과, 신규 3개 포함

- [ ] **Step 5: 커밋**

```bash
git add crates/fleetwire/src/auth/ crates/fleetwire/src/lib.rs
git commit -m "feat(a2): TokenManager single-flight + TokenRefresher 주입 (REQ-15/16)"
```

---

### Task 8: 적대적 테스트 하드닝 + 문서

**Files:**
- Modify: `crates/fleetwire/src/auth/mod.rs` (테스트 추가)
- Modify: `crates/fleetwire/src/auth/store.rs` (테스트 추가)
- Modify: `crates/fleetwire/src/lib.rs` (크레이트 doc 갱신)

**Interfaces:** 새 public API 없음. 기존 동작의 실패 경로를 고정한다.

**왜 별도 태스크인가:** A1 은 redaction 하나에 fix 라운드를 4회 썼고 매번 원인이 같았다 — **테스트를 구현에 맞춰 재단**했다. 구현이 이미 고정된 뒤에 "이 코드를 깨뜨리는 입력은 무엇인가" 만 묻는 태스크를 따로 두면 그 편향이 줄어든다. 리뷰어도 T7 의 구현은 승인하면서 이 태스크는 반려할 수 있다.

- [ ] **Step 1: 실패 주입 테스트 작성**

`crates/fleetwire/src/auth/mod.rs` 의 `mod tests` 에 추가:
```rust
    /// 항상 실패하는 refresher — 실패가 조용히 삼켜지지 않는지 본다.
    struct FailingRefresher;

    impl TokenRefresher for FailingRefresher {
        fn refresh<'a>(
            &'a self,
            _refresh_token: &'a str,
        ) -> Pin<Box<dyn Future<Output = Result<Refreshed, crate::error::Error>> + Send + 'a>> {
            Box::pin(async {
                Err(crate::error::Error::Auth {
                    message: "upstream refused".to_owned(),
                    pricing: None,
                })
            })
        }
    }

    #[tokio::test]
    async fn a_failing_refresh_does_not_poison_the_cache_with_a_partial_token() {
        let dir = temp_dir("failrefresh");
        seed_expired_token(&dir);
        let mgr = TokenManager::new(dir.clone(), Arc::new(FailingRefresher));

        assert!(mgr.access_token().await.is_err(), "실패가 전파돼야 한다");
        // 디스크는 손대지 않은 채여야 한다.
        let raw = std::fs::read_to_string(dir.join("tokens.json")).expect("read");
        let v: serde_json::Value = serde_json::from_str(&raw).expect("json");
        assert_eq!(v["access_token"], serde_json::json!("OLD"));
        assert_eq!(v["refresh_token"], serde_json::json!("NA_OLD"));

        // 두 번째 호출도 같은 오류여야 한다 — 깨진 상태가 캐시되면 안 된다.
        assert!(mgr.access_token().await.is_err());
    }

    #[tokio::test]
    async fn a_store_failure_prevents_the_new_token_from_being_returned() {
        let dir = temp_dir("storefail");
        seed_expired_token(&dir);
        let mgr = TokenManager::new(dir.clone(), Arc::new(CountingRefresher::new("NEW")));

        // 디렉터리를 읽기 전용으로 만들어 저장을 실패시킨다.
        let mut perms = std::fs::metadata(&dir).expect("meta").permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o500);
        std::fs::set_permissions(&dir, perms).expect("chmod dir");

        let result = mgr.access_token().await;

        // 정리 — 다른 테스트/CI 가 지우지 못하는 디렉터리를 남기지 않는다.
        let mut perms = std::fs::metadata(&dir).expect("meta").permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o700);
        std::fs::set_permissions(&dir, perms).expect("restore");

        assert!(
            result.is_err(),
            "저장에 실패했는데 새 토큰을 반환했다 — 회전 토큰을 잃고도 성공으로 보고한 셈이다"
        );
    }

    #[tokio::test]
    async fn the_lock_file_is_never_removed_on_any_path() {
        let dir = temp_dir("lockimmortal");
        seed_expired_token(&dir);
        let lock_path = dir.join("tokens.lock");

        // 성공 경로
        let mgr = TokenManager::new(dir.clone(), Arc::new(CountingRefresher::new("NEW")));
        mgr.access_token().await.expect("success path");
        assert!(lock_path.exists(), "성공 경로에서 락 파일이 사라졌다");

        // 실패 경로
        seed_expired_token(&dir);
        let mgr = TokenManager::new(dir.clone(), Arc::new(FailingRefresher));
        let _ = mgr.access_token().await;
        assert!(lock_path.exists(), "실패 경로에서 락 파일이 사라졌다");
    }

    #[tokio::test]
    async fn a_missing_token_file_is_a_clean_error_not_a_panic() {
        let dir = temp_dir("notokens");
        let mgr = TokenManager::new(dir, Arc::new(CountingRefresher::new("NEW")));
        assert!(mgr.access_token().await.is_err());
    }
```

`crates/fleetwire/src/auth/store.rs` 의 `mod tests` 에 추가:
```rust
    #[test]
    fn load_rejects_a_world_readable_token_file() {
        let dir = temp_dir("perms");
        let path = dir.join("tokens.json");
        std::fs::write(&path, r#"{"access_token":"AT","refresh_token":"NA_RT","expires_at":1}"#)
            .expect("write");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        assert!(matches!(TokenStore::new(dir).load(), Err(StoreError::BadPermissions)));
    }

    #[test]
    fn store_refuses_to_write_through_a_symlinked_token_path() {
        let dir = temp_dir("storesymlink");
        let target = dir.join("victim.json");
        std::fs::write(&target, "original").expect("write victim");
        std::os::unix::fs::symlink(&target, dir.join("tokens.json")).expect("symlink");

        let store = TokenStore::new(dir);
        assert!(matches!(store.store(&token_set_expiring_at(1)), Err(StoreError::Symlink)));
        assert_eq!(
            std::fs::read_to_string(&target).expect("read victim"),
            "original",
            "symlink 을 따라가 다른 파일을 덮어썼다"
        );
    }
```

- [ ] **Step 2: 실패 확인**

Run: `cargo test -p fleetwire auth::`
Expected: 일부 FAIL 가능. **여기서 실패하는 테스트는 구현의 실제 결함이다** — 테스트를 약화시키지 말고 구현을 고칠 것. 특히 `a_store_failure_prevents_the_new_token_from_being_returned` 와 `store_refuses_to_write_through_a_symlinked_token_path` 가 유력하다.

- [ ] **Step 3: 드러난 결함 수정**

발견되는 것에 따라 다르므로 코드를 미리 못 박지 않는다. 다만 **테스트를 완화하는 방향으로 고치지 말 것** — 각 단언은 설계 §5/§7 의 명시적 요구다:
- 저장 실패 시 새 토큰 반환 금지 → 설계 §5 "성공 후에만 메모리 교체"
- symlink 를 통한 쓰기 거부 → REQ-16 "symlink 공격 방어"
- 락 파일 불멸 → REQ-15 "정상·오류·프로세스 종료 어디에서도 삭제하지 않는다"

- [ ] **Step 4: 크레이트 문서 갱신**

`crates/fleetwire/src/lib.rs` 의 크레이트 doc 에서 상태 줄을 갱신:
```rust
//! Status: **work in progress** — A1 core types + A2 token layer. HTTP/전송은 아직 없다.
//! 토큰 refresh 는 [`TokenRefresher`] 구현이 필요하며, 그 구현은 서브프로젝트 A3 가 제공한다.
```

- [ ] **Step 5: 전 게이트 + 커밋**

Run:
```bash
cargo test -p fleetwire
cargo clippy --all-targets --all-features --locked -- -D warnings
cargo clippy --locked -- -D warnings
cargo fmt --all -- --check
cargo +1.89.0 build -p fleetwire --locked
cargo +1.89.0 test -p fleetwire --locked
cargo doc --no-deps -p fleetwire
cargo deny check
```
Expected: 전부 통과

```bash
git add crates/fleetwire/src/
git commit -m "test(a2): 적대적 실패 주입 — 저장실패·symlink·락 불멸·refresh 실패"
```

---

## Self-Review

**1. Spec coverage**

| 설계 절 | 태스크 |
|---|---|
| §2 범위·경계 | 전체 (401 분류기·리전·POST 는 명시적으로 A3) |
| §3 실측 증거 | T2(offline_access), T6(scp 배열 형태) |
| §4 파일 위치 | T4 (`TokenStore::new(dir)`, 기본값은 A3/A4 의 빌더가 지정) |
| §5 저장 계층 | T3(TokenSet·Debug·extra), T4(disk-first) |
| §6 스코프 advisory | T2(FromStr), T6(3상태 관찰) |
| §7 single-flight | T5(락), T7(오케스트레이션) |
| §8 주입 지점 | T3(Clock), T7(TokenRefresher) |
| §9 오류 | T7 (`Error::Auth{pricing:None}`) |
| §10 플랫폼 | T4 (`std::os::unix`) |
| §11 새 의존성 | T1 |
| §12 테스트 전략 | T5·T7·T8 |
| §14-1 file_lock 버전 | T1 — **확정 1.89.0** (로컬 std 소스 `#[stable(feature = "file_lock", since = "1.89.0")]`) |
| §14-2 base64 라이선스·의존성 | T1 — **확정** 0.23.0, 전이 의존성 0개, `default-features = false` 필수(`simd-unsafe` 회피) |
| §14-2b 만료 skew | T3 — **확정 60초** |
| §14-2c 락 재시도 파라미터 | T5 — **확정** deadline 10초 / 초기 5ms / 상한 200ms |
| §14-3 scp 중복 키 | T6 — 만들지 않음. `DoesNotContain` 은 배열의 성공적 파싱을 요구한다(문서화됨) |

**미커버(의도적)**: 설계 §6.4 의 opt-in 생성자(`with_unverified_jwt_scope_advisory`)는 **A3/A4 의 `Client` 빌더**에 붙는다. A2 에는 요청 파이프라인이 없어 선검사를 걸 지점이 없고, 지금 만들면 호출자 없는 API 가 된다. T6 이 판정 함수를 제공하고 배선은 파이프라인이 생길 때 한다.

**2. Placeholder scan** — "TBD"·"적절히"·"에러 처리 추가" 없음. T8 Step 3 만 코드를 미리 못 박지 않는데, 이는 "드러나는 결함에 따라 다름" 이 정직한 서술이고 대신 **무엇을 고치면 안 되는지**(테스트 완화 금지)와 각 단언의 스펙 근거를 명시했다.

**3. Type consistency** — `TokenSet`(T3) → `TokenStore::{load,store}`(T4) → `TokenManager`(T7) 이 같은 타입을 쓴다. `Clock::now_unix() -> u64` 가 `TokenSet::expires_at: u64` 및 `Refreshed::expires_in_secs: u64` 와 단위가 일치한다. `lock::acquire`(T5)의 `LOCK_DEADLINE` 을 T7 이 그대로 쓴다. `Scope::from_str`(T2)를 T6 이 쓴다. `Error::Auth { message, pricing }` 는 A1 이 fix wave 에서 확정한 형태 그대로다.

**A1 `scope.rs` 실제 시그니처 확인 완료**(2026-07-31, 소스 직독 — 구현자가 다시 확인할 필요 없다):

```rust
pub enum Scope { VehicleDeviceData, VehicleLocation, UserData, Openid }   // 4 variant
pub enum ScopeExpr {
    All(Vec<ScopeExpr>),   // ⚠️ Vec<Scope> 가 아니라 Vec<ScopeExpr>
    Any(Vec<ScopeExpr>),
    One(Scope),
}
impl ScopeExpr { pub fn is_satisfied_by(&self, held: &[Scope]) -> bool }
```

이 확인에서 계획의 버그를 하나 잡았다 — T6 테스트가 `ScopeExpr::All(vec![Scope::…])` 로 쓰여 있었고 그대로면 컴파일되지 않는다. `ScopeExpr::One(…)` 으로 감싸도록 수정했다.

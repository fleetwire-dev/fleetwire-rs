//! Fixtures shared across module test suites (`auth`, `client`), `#[cfg(test)]`-only.
//!
//! Extracted from `auth`'s own test fixtures (A2 Task 4 fix round) rather than
//! re-derived: `auth::tests` hit a umask trap there first — `create_dir_all`
//! and `std::fs::write` both apply the process umask to the mode they
//! request, so a 0700 directory or a 0600 file can silently come out wider
//! (0755 / 0644 under a common umask) and get rejected by `TokenStore`'s own
//! permission checks before the code under test ever runs (see
//! `auth::tests::temp_dir`'s doc, before this module existed, for the original
//! writeup). `client`'s Task 6+ fixtures need to drive a real `TokenManager`
//! against a real on-disk token directory, so they need the identical
//! umask-proof 0700-dir + 0600-token-file construction — moved here instead
//! of being re-derived (and re-broken) a second time in a different module.

use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

/// A private, process+thread-scoped temp directory, created 0700 directly and
/// **removed when the guard drops**.
///
/// Never build this with `create_dir_all`: `mkdir(2)` applies the process
/// umask to the requested mode, so 0700 can come out as 0755 (umask 022) or
/// 0775 (a shared-group umask 002) — and `TokenStore::prepare_dir` then
/// rejects the fixture itself as `BadDirPermissions` before any test logic
/// runs, so every test using it fails without ever reaching the code path it
/// meant to exercise.
///
/// 이 값은 **가드**다 — 살아 있는 동안만 디렉터리가 존재한다. 즉시 버리는 형태
/// (`let _ = temp_dir("x");` · `temp_dir("x").join(..)`)를 쓰면 테스트 도중 디렉터리가
/// 사라진다. 반드시 이름 있는 바인딩(`let dir = temp_dir("x");`)으로 받을 것.
///
/// `Deref<Target = Path>` 이므로 `dir.join(..)`·`&dir` 는 그대로 쓸 수 있다. 소유
/// `PathBuf` 가 필요한 자리에서는 `dir.to_path_buf()` — 가드 바인딩은 살려 둘 것.
pub(crate) struct TempDir(PathBuf);

impl std::ops::Deref for TempDir {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for TempDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    /// 지우기 **전에** 권한을 되돌린다 — 쓰기 금지 디렉터리를 만드는 테스트가
    /// 패닉으로 중단되면 0500 인 채로 여기 들어오고, 그러면 `remove_dir_all` 이
    /// 조용히 실패해 누수가 그대로 남는다(생성 경로에 있던 가드와 같은 것).
    ///
    /// 여기서 절대 패닉하지 않는다 — 패닉 언와인딩 중에 `Drop` 이 다시 패닉하면
    /// 이중 패닉으로 프로세스가 SIGABRT 로 죽어 실패한 테스트의 진단이 사라진다.
    /// 그래서 모든 실패를 `let _ =` 로 삼킨다.
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// [`TempDir`] 하나를 만든다.
///
/// 만들기 전에 지우고 다시 만든다. 이제는 `Drop` 이 정상 경로를 청소하므로 이
/// 사전 삭제가 잡는 것은 **좁다**: 같은 pid 를 재사용한 다른 실행이 SIGKILL·SIGSEGV
/// 로 죽어 `Drop` 조차 돌지 못하고 남긴 잔여물뿐이다.
///
/// 예전 doc 은 "pid/thread id 는 실행 간 재사용되므로 사전 삭제가 이전 실행의
/// 잔여물을 치운다" 고 적었는데 **그 논증은 틀렸다.** pid 는 `cargo test` 실행마다
/// 바뀌므로 이름이 거의 매칭되지 않고, 그래서 사전 삭제는 사실상 아무것도 치우지
/// 못한 채 (실행 × 태그 × 스레드)마다 디렉터리가 하나씩 영구 누적됐다.
///
/// **사고의 실측치는 여기 한 곳에만 적는다**(2026-08-13 KST, 다른 세션 계측).
/// `/tmp` 최상위 `fleetwire-*` 가 **77,890 개**(05:39 분류 실측). 이것은 하한이다 —
/// 그 시점엔 `systemd-tmpfiles` 가 이미 10분째 지우고 있었고, 부팅 시점 실제값은
/// **77,890 ~ 83,585** 구간, 단일 점으로는 **약 8만**. (최초 보고된 "약 97,000" 은
/// 뺄셈 오류로 **철회됐다** — 다른 크레이트의 가드 doc 이 그 수를 복제하고 있었기에
/// 여기로 모았다. 숫자를 다시 퍼뜨리지 말 것.) 그 결과 부팅 시 `systemd-tmpfiles` 가
/// 약 30건/초로 지우느라 `sysinit.target` 을 **76분** 막았고, 뒤따르던
/// `cron`·`docker`·`dbus` 가 전부 대기해 운영 cron 이 유실됐다.
///
/// **청소를 보장하는 것은 `Drop` 이다** — 사전 삭제가 아니다.
///
/// `Drop` 은 SIGKILL·강제중단에서는 돌지 않는다. 그 경로로 죽으면 그 실행분은
/// 그대로 남는다(사전 삭제가 pid 재사용 시에만 좁게 회수). 스위트가 정상 종료하는
/// 한 증가분은 0 이다 — 그것이 이 파일의 계약이다.
pub(crate) fn temp_dir(tag: &str) -> TempDir {
    let dir = std::env::temp_dir().join(format!(
        "fleetwire-test-support-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    // Restore permissions first: a test that deliberately makes a
    // write-protected directory can leave it 0500 if a previous run died
    // without unwinding, and `remove_dir_all` would then silently fail.
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&dir)
        .expect("temp dir");
    TempDir(dir)
}

/// Seeds a 0600 token file directly at `dir/`[`crate::auth::TOKEN_FILE`],
/// matching `auth::store::TokenStore`'s on-disk shape.
///
/// Fix round 1 (Minor 1): this used to hardcode the literal `"tokens.json"`
/// while `auth::tests::token_path` derived the same name from `TokenStore` —
/// exactly the split source of truth A2 warned about avoiding. Both now read
/// the one constant `auth::store` owns.
///
/// Never seed with `std::fs::write` alone: the process umask widens a
/// requested 0644-ish default further, and `TokenStore::load` stops at
/// `BadPermissions` before it ever parses the content.
pub(crate) fn write_token(dir: &Path, access: &str, refresh: &str, expires_at: u64) {
    let path = dir.join(crate::auth::TOKEN_FILE);
    std::fs::write(
        &path,
        format!(
            r#"{{"access_token":"{access}","refresh_token":"{refresh}","expires_at":{expires_at}}}"#
        ),
    )
    .expect("seed token file");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .expect("chmod token file");
}

/// An already-expired token (so the first `access_token*` call is forced
/// through the refresh/adopt path rather than served from cache).
pub(crate) fn seed_expired_token(dir: &Path) {
    write_token(dir, "OLD", "NA_OLD", 1);
}

/// An expiry far enough in the future that no test needs a refresh to reach it.
pub(crate) const FAR_FUTURE: u64 = 4_000_000_000;

// --- A4 live-captured fixtures (2026-08-03) ---------------------------------
//
// Real bytes from live measurements, shared across A4 task test suites.
// Do not reformat, re-indent, or "fix" these — later tasks assert against
// the precise strings, including an empty `error_description` and an
// embedded `txid`.

/// 408, vehicle offline. Uses the envelope, and `error_description` is an
/// **empty string**.
///
/// Consumed by A4 Task 3's `txid_survives_on_vehicle_offline` and
/// `txid_is_not_redacted`.
pub(crate) const FIXTURE_408_OFFLINE: &str = r#"{"response":null,"error":"vehicle unavailable: vehicle is offline or asleep","error_description":"","txid":"432e7faaa94cf732bdda8f82f2184eea"}"#;

/// List response. `pagination` and `count` are **sibling keys** of `response`.
pub(crate) const FIXTURE_VEHICLES_PAGED: &str = r#"{"response":[{"id":1,"vin":"5YJ3E1EA1JF000001","state":"offline"}],"pagination":{"previous":null,"next":null,"current":1,"per_page":1,"count":1,"pages":1},"count":1}"#;

/// State query for the cost gate. `backseat_token` rides along on a
/// successful response.
pub(crate) const FIXTURE_VEHICLE_STATE_OFFLINE: &str = r#"{"response":{"id":1,"vin":"5YJ3E1EA1JF000001","state":"offline","backseat_token":"SECRET-BACKSEAT","display_name":"","in_service":false}}"#;

/// `error` is an **empty string**, so the T3 fallback chain actually runs.
/// `FIXTURE_408_OFFLINE` alone never falls through, which would make that
/// test vacuous (design doc §8 T-b).
///
/// Consumed by A4 Task 3's `empty_error_is_skipped_and_description_wins`.
pub(crate) const FIXTURE_ERR_EMPTY_ERROR: &str = r#"{"response":null,"error":"","error_description":"the vehicle could not be reached","txid":"deadbeef"}"#;

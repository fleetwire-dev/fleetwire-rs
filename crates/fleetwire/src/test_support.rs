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
/// This value is a **guard** — the directory exists only while it is alive. A form that
/// drops it at once (`let _ = temp_dir("x");` or `temp_dir("x").join(..)`) makes the
/// directory disappear in the middle of the test. Always take it in a named binding
/// (`let dir = temp_dir("x");`).
///
/// It is `Deref<Target = Path>`, so `dir.join(..)` and `&dir` work as they are. Where an
/// owned `PathBuf` is needed, use `dir.to_path_buf()` — and keep the guard binding alive.
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
    /// Restores the permissions **before** removing — when a test that makes a
    /// write-protected directory is cut short by a panic, the directory arrives here still
    /// 0500, and then `remove_dir_all` fails silently and the leak stays as it is (the same
    /// guard that was on the creation path).
    ///
    /// Never panics here — if `Drop` panics again while a panic is unwinding, the double
    /// panic kills the process with SIGABRT and the failed test's diagnostics are lost.
    /// So every failure is swallowed with `let _ =`.
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Creates one [`TempDir`].
///
/// It removes the directory before creating it again. Now that `Drop` cleans up the normal
/// path, what this pre-delete catches is **narrow**: only the leftovers of another run that
/// reused the same pid and was killed by SIGKILL or SIGSEGV, so that not even `Drop` ran.
///
/// The old doc said "pid/thread id are reused across runs, so the pre-delete clears the
/// previous run's leftovers", but **that argument was wrong.** The pid changes on every
/// `cargo test` run, so the names almost never match, and so the pre-delete cleared
/// practically nothing while one directory per (run x tag x thread) piled up for good.
///
/// **The measured figures of the incident are written in this one place only**
/// (2026-08-13 KST, measured by another session). There were **77,890** `fleetwire-*`
/// entries at the top level of `/tmp` (measured by classification at 05:39). That is a
/// lower bound — by then `systemd-tmpfiles` had already been deleting for 10 minutes, and
/// the real value at boot lies in the range **77,890 to 83,585**, or **about 80,000** as a
/// single point. (The "about 97,000" first reported was **withdrawn** as a subtraction
/// error — guard docs in other crates were duplicating that number, so they were gathered
/// here. Do not spread the number again.) As a result, at boot `systemd-tmpfiles` blocked
/// `sysinit.target` for **76 minutes** while deleting at about 30 entries per second, and
/// `cron`, `docker` and `dbus` behind it all waited, so production cron runs were lost.
///
/// **What guarantees the cleanup is `Drop`** — not the pre-delete.
///
/// `Drop` does not run on SIGKILL or a forced abort. A run that dies that way leaves its
/// directories behind (the pre-delete reclaims them only narrowly, on pid reuse). As long
/// as the suite exits normally the increase is 0 — that is this file's contract.
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

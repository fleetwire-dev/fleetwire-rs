//! Safe reading and writing of the token file (0600, symlink defence, atomic + fsync).
//!
//! A2 Task 7 ([`super::TokenManager`]) calls this whole module, so every
//! `expect(dead_code)`/`allow(dead_code)` attribute that Tasks 3–5 had attached has been
//! removed — the purpose of using `expect` was exactly that "the compiler forces its removal
//! the moment the code comes alive".

use crate::clock::Clock;
use crate::redaction::REDACTED;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The margin within which a token is treated as "about to expire".
///
/// It keeps a token from expiring while a request is in flight. A Tesla access token lives
/// for about 8 hours, so 60 seconds is a margin of roughly 0.2% — large enough that no single
/// request will expire inside it, and not so large that it triggers needless refresh
/// rotations.
pub(crate) const EXPIRY_SKEW: Duration = Duration::from_secs(60);

/// One set of tokens as kept on disk.
///
/// `Debug` is implemented by hand rather than derived — a derived impl would print the
/// access/refresh tokens verbatim (REQ-18). Adding a field makes the destructuring in the
/// `Debug` impl fail to compile with `E0027`, so how to expose the new field has to be decided
/// on the spot (enforced by the compiler, not by convention).
#[derive(Serialize, Deserialize)]
pub(crate) struct TokenSet {
    access_token: String,
    refresh_token: String,
    /// Seconds elapsed since the Unix epoch. Stored as an integer for stable JSON.
    expires_at: u64,
    /// Preserves fields this version does not know — so that no field is lost when an older
    /// version reads and rewrites a file written by a newer version.
    #[serde(flatten)]
    extra: Map<String, Value>,
}

impl core::fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // 구조분해가 이 impl 의 안전장치다: 필드를 추가하면 여기서 `E0027`
        // (pattern does not mention field) 로 컴파일이 깨진다. `self.field` 로
        // 접근했다면 새 필드가 조용히 출력에서 빠지고 어떤 테스트도 실패하지 않는다.
        // `: _` 는 "값을 의도적으로 찍지 않는다" 는 표시다.
        let Self {
            access_token: _,
            refresh_token: _,
            expires_at,
            extra,
        } = self;
        // 마커는 크레이트 단일 리댁션 계층의 상수를 쓴다(REQ-18) — 여기서
        // 문자열을 다시 적으면 출처가 둘로 갈라진다.
        f.debug_struct("TokenSet")
            .field("access_token", &REDACTED)
            .field("refresh_token", &REDACTED)
            .field("expires_at", expires_at)
            // `extra` 는 개수만 — 신버전이 넣은 미지 필드가 비밀일 수 있다.
            .field("extra_keys", &extra.len())
            .finish()
    }
}

impl TokenSet {
    /// The current access token.
    pub(crate) fn access_token(&self) -> &str {
        &self.access_token
    }

    /// The current refresh token.
    pub(crate) fn refresh_token(&self) -> &str {
        &self.refresh_token
    }

    /// Whether the token is still valid even after allowing for the `skew` margin.
    ///
    /// `expires_at == now + skew` is **not fresh** — the boundary belongs to the refresh side
    /// (the comparison is `>`, not `>=`).
    ///
    /// To avoid a u64 underflow on an already expired token, the comparison is done by
    /// addition rather than subtraction, and the overflow on the other side is prevented with
    /// `saturating_add`.
    pub(crate) fn is_fresh(&self, clock: &dyn Clock, skew: Duration) -> bool {
        self.expires_at > clock.now_unix().saturating_add(skew.as_secs())
    }

    /// Builds a new set of tokens from a refresh result.
    ///
    /// `expires_at` is computed and passed in **by the caller**. Neither this type nor
    /// [`TokenStore::store`] does any time arithmetic — when `now + expires_in` meets a
    /// pre-epoch clock (`u64::MAX`) the result is a token that "never expires", so that
    /// computation lives in one place only, [`super::TokenManager::access_token`], which
    /// knows about the hazard.
    ///
    /// `extra` is left empty. Unknown fields exist to be preserved when rewriting **a file
    /// read from disk**; this is not the place to drag remnants of an old file into freshly
    /// issued tokens.
    pub(crate) fn from_refreshed(refreshed: &super::Refreshed, expires_at: u64) -> Self {
        Self {
            access_token: refreshed.access_token.clone(),
            refresh_token: refreshed.refresh_token.clone(),
            expires_at,
            extra: Map::new(),
        }
    }
}

/// The token file name.
pub(crate) const TOKEN_FILE: &str = "tokens.json";
/// The lock file name. It must be in the **same directory** as the tokens (the lock is keyed
/// on the inode, not the path).
pub(crate) const LOCK_FILE: &str = "tokens.lock";

/// A failure to access the token store.
///
/// No variant carries file contents or token values (REQ-18).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub(crate) enum StoreError {
    /// The token file does not exist — meaning the user has not logged in yet.
    #[error("token file not found")]
    NotFound,
    /// The token path is a symlink. It is refused rather than followed.
    #[error("token path is a symbolic link")]
    Symlink,
    /// The token file's permissions are not 0600.
    #[error("token file permissions are too permissive")]
    BadPermissions,
    /// The token **directory**'s permissions are not 0700.
    ///
    /// A check that looks at the file alone cannot prevent this — anyone who can write to
    /// the directory can swap the file out wholesale. See [`TokenStore::ensure_private_dir`].
    #[error("token directory permissions are too permissive")]
    BadDirPermissions,
    /// The file contents are not valid token JSON. The contents are not carried.
    #[error("token file is malformed")]
    Malformed,
    /// Any other I/O error.
    #[error("token store io error")]
    Io(#[from] std::io::Error),
}

/// Safe reading and writing for a single token directory.
#[derive(Debug, Clone)]
pub(crate) struct TokenStore {
    dir: PathBuf,
}

impl TokenStore {
    /// Creates a store that uses the given directory. The directory is created at `store` time.
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self { dir }
    }

    /// The token file path.
    pub(crate) fn token_path(&self) -> PathBuf {
        self.dir.join(TOKEN_FILE)
    }

    /// The lock file path.
    pub(crate) fn lock_path(&self) -> PathBuf {
        self.dir.join(LOCK_FILE)
    }

    /// The temporary file path private to this process and thread.
    ///
    /// It must be in the same directory for `rename` to be atomic (crossing a filesystem
    /// boundary fails with `EXDEV`). The pid and thread id are included so that it does not
    /// collide with other processes and threads using the same directory.
    fn temp_path(&self) -> PathBuf {
        self.dir.join(format!(
            ".{TOKEN_FILE}.tmp.{}.{:?}",
            std::process::id(),
            std::thread::current().id()
        ))
    }

    /// Reads the tokens, refusing a symlink, overly permissive permissions and a corrupt file
    /// as distinct errors.
    ///
    /// To close the TOCTOU window between the check and the open, it verifies that the
    /// `ino`/`dev` of the opened handle match those seen at check time. If someone swapped
    /// the path in between, it is caught here.
    ///
    /// **Error precedence (the order is intentional).** `BadDirPermissions` → `NotFound` →
    /// `Symlink` → (not a file) `Malformed` → `BadPermissions` → contents `Malformed`.
    /// What gets reported when two conditions overlap is not arbitrary:
    /// - If the directory is open to others it is refused **first**, because any check on a
    ///   file beneath it only means something on the premise that "nobody else can touch
    ///   this directory".
    /// - The path is a symlink and the target's permissions are also too permissive →
    ///   **`Symlink`**. The link is not followed, so there is no standing to look at the
    ///   target's permissions at all, and the link's own `lstat` mode is always 0777 on
    ///   Linux, so reporting `BadPermissions` would hide the real cause.
    /// - The permissions are too permissive and the contents are also corrupt →
    ///   **`BadPermissions`**. The reason is not "the contents are never loaded into memory"
    ///   (that is a side effect). A file that group or others could read is a file that
    ///   **may have been swapped**, and data already judged untrustworthy must not be parsed
    ///   and acted upon. Reporting `Malformed` would send the caller the wrong signal that
    ///   "only the contents need fixing" — the action actually required is a re-OAuth.
    pub(crate) fn load(&self) -> Result<TokenSet, StoreError> {
        use std::io::Read as _;
        use std::os::unix::fs::MetadataExt as _;

        Self::ensure_private_dir(&self.dir)?;
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

    /// Disk-first save: temporary file → fsync → atomic rename → fsync of the parent directory.
    ///
    /// The caller must replace the in-memory tokens only after this function has returned
    /// `Ok`. Using the new tokens after a failed save means the next refresh will need a
    /// re-OAuth.
    ///
    /// **`expires_at` is not computed here.** The [`TokenSet`] received is serialized as
    /// is. When `now + expires_in` meets the pre-epoch fallback of [`crate::clock`]
    /// (`u64::MAX`) it saturates and nails a token that "never expires" onto disk — the
    /// expiry computation lives in one place only, A2 Task 7, which knows about the hazard.
    /// Keep this function a pure serializer.
    pub(crate) fn store(&self, set: &TokenSet) -> Result<(), StoreError> {
        self.prepare_dir()?;
        let final_path = self.token_path();

        if let Ok(meta) = std::fs::symlink_metadata(&final_path)
            && meta.file_type().is_symlink()
        {
            return Err(StoreError::Symlink);
        }

        let tmp_path = self.temp_path();
        // 이전 실행이 남긴 잔여물이 있으면 O_EXCL 이 EEXIST 로 실패해 저장이
        // 영구히 막히므로 먼저 치운다. 다만 **symlink 는 치우지 않고 거부**한다 —
        // 우리가 만드는 것은 언제나 평범한 파일이므로, 임시 경로의 링크는
        // 잔여물이 아니라 공격이다. 조용히 지우고 진행하면 흔적만 지우는 셈이다.
        match std::fs::symlink_metadata(&tmp_path) {
            Ok(m) if m.file_type().is_symlink() => return Err(StoreError::Symlink),
            Ok(_) => drop(std::fs::remove_file(&tmp_path)),
            Err(_) => {}
        }

        let json = serde_json::to_vec(set).map_err(|_| StoreError::Malformed)?;

        // 실패하면 어느 단계에서 실패했든 임시 파일을 남기지 않는다. 성공 경로만
        // 정리하는 구현은 저장이 실패할 때마다 토큰 디렉터리에 `.tmp` 를 흘린다.
        let outcome = Self::write_then_swap(&tmp_path, &final_path, &json, &self.dir);
        if outcome.is_err() {
            // rename 이 성공한 뒤(fsync 단계)에 실패했다면 이미 없는 파일이다 — no-op.
            drop(std::fs::remove_file(&tmp_path));
        }
        outcome
    }

    /// **Creates** the token directory as 0700 and **verifies** its permissions.
    ///
    /// The two must go together.
    /// - Design §4: `~/.fleetwire/` is **0700**. `create_dir_all` cannot hold that line —
    ///   `mkdir(2)` applies the umask to the mode, so the result is 0755 under umask 022, and
    ///   0775 = **group-writable** on deployments and CI with a shared primary group and umask
    ///   002. `recursive(true)` applies the same mode to every ancestor it newly creates.
    /// - That mode applies only to **newly created** directories. A directory that already
    ///   existed as 0775 stays as it is, so this covers only half. The other half is
    ///   [`TokenStore::ensure_private_dir`].
    ///
    /// **Do not use the check-only counterpart (`ensure_private_dir` without creation).**
    /// It returns [`StoreError::NotFound`] when the directory does not exist yet, so it blocks
    /// outright any path on which the directory is missing, such as right after the first
    /// login. This is also the function [`super::TokenManager`] calls before taking the file
    /// lock.
    pub(crate) fn prepare_dir(&self) -> Result<(), StoreError> {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&self.dir)?;
        Self::ensure_private_dir(&self.dir)
    }

    /// Verifies that nobody but the owner can touch the token directory.
    ///
    /// **Every other defence in this file stands on this condition.** Anyone who can write to
    /// the directory can `unlink` `tokens.json` and put **a plain 0600 file of their own** in
    /// its place. The symlink check, the permission check and the `ino`/`dev` re-check then
    /// all pass — [`TokenStore::load`] has no uid check, so it accepts the tokens someone
    /// else planted as they are. File-level checks cannot prevent this in principle, so the
    /// premise itself is checked. This is the `~/.fleetwire/ 0700` line of design §4.
    ///
    /// Why the mask is `0o077` rather than `0o007` (write only): the design specifies 0700,
    /// and the same standard as the file-side check is used. Not fixing it automatically with
    /// `chmod` is also the same as on the file side — silently changing the permissions of
    /// someone else's directory would be the more surprising behaviour.
    ///
    /// **Limitation**: only `dir` itself is examined. Ancestor directories are not (`/tmp` is
    /// 1777, so if ancestors were examined no code using a temporary directory would pass).
    /// An attacker who can write to an ancestor can replace the directory wholesale — that
    /// threat is handled by the premise of home directory ownership.
    ///
    /// **A directory that is a symlink is refused** (final fix wave / triage 7). `metadata`
    /// follows links, so even when the token **directory** path is a symlink planted by
    /// someone else, it looks only at the target's permissions and lets it through — the file
    /// side is already guarded with `symlink_metadata`, and if only the directory side is
    /// left open the defence is half a defence. `symlink_metadata` is used to look at the
    /// link itself.
    fn ensure_private_dir(dir: &Path) -> Result<(), StoreError> {
        let meta = match std::fs::symlink_metadata(dir) {
            Ok(m) => m,
            // 디렉터리가 없다 = 아직 로그인 안 함. `load` 가 이 값을 그대로 쓴다.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(StoreError::NotFound),
            Err(e) => return Err(StoreError::Io(e)),
        };
        // 파일 쪽(`load`)과 같은 방침: 따라가지 않고 거부한다. symlink 의 mode 는
        // 보통 0777 이라 아래 권한 검사에도 걸리기는 하지만, 그러면 사용자는 진짜
        // 원인(링크) 대신 "권한이 헐겁다" 를 보고 헛되이 chmod 를 시도하게 된다.
        if meta.file_type().is_symlink() {
            return Err(StoreError::Symlink);
        }
        if !meta.is_dir() {
            return Err(StoreError::Io(std::io::Error::from(
                std::io::ErrorKind::NotADirectory,
            )));
        }
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(StoreError::BadDirPermissions);
        }
        Ok(())
    }

    /// Creates the temporary file → writes → `fsync` → atomic `rename` → `fsync` of the parent
    /// directory.
    ///
    /// On failure, cleaning up the temporary file is done by the caller
    /// ([`TokenStore::store`]).
    fn write_then_swap(
        tmp_path: &Path,
        final_path: &Path,
        json: &[u8],
        dir: &Path,
    ) -> Result<(), StoreError> {
        {
            // create_new(true) = O_CREAT|O_EXCL. POSIX 상 경로가 symlink 면 링크
            // 내용과 무관하게 EEXIST 로 실패하므로, 이것만으로 임시 파일의
            // symlink 방어가 성립한다(O_NOFOLLOW 불필요 — 계획 T4 머리말 참조).
            // 위의 명시적 검사를 지나 여기 오기까지의 경합 창을 닫는 것이 이 플래그다.
            //
            // `.mode(0o600)` 은 **생성 시점**에 적용된다 — 만들고 나서
            // `set_permissions` 로 조이면 그 사이에 남이 열 수 있는 창이 생긴다.
            let mut f = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(tmp_path)?;
            f.write_all(json)?;
            f.sync_all()?;
        }

        // rename 은 대상이 symlink 여도 **따라가지 않고** 링크 자체를 대체한다.
        // 위의 명시적 검사가 있으므로 여기 도달하면 대상은 symlink 가 아니다.
        std::fs::rename(tmp_path, final_path)?;
        fsync_dir(dir)?;
        Ok(())
    }
}

/// Commits the directory entry to disk. The parent must be fsynced after the rename for the
/// new name to survive a crash.
fn fsync_dir(dir: &Path) -> std::io::Result<()> {
    File::open(dir)?.sync_all()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::FixedClock;

    /// A temporary directory that does not touch process-global state (TMPDIR) — it exists
    /// **only while the guard is alive** and is removed wholesale in `Drop`.
    ///
    /// **It is created as 0700.** With `create_dir_all` it would be 0755 under umask 022 and
    /// [`TokenStore::ensure_private_dir`] would refuse the fixture itself — then no test
    /// would reach the code path it meant to exercise (exactly the same trap as the fixture
    /// in the original plan text failing to reach parsing because of the umask).
    ///
    /// With the immediately-dropped form (`TokenStore::new(temp_dir("x").join(..))`) the
    /// directory disappears before the test starts. Always bind it with
    /// `let dir = temp_dir("x");`, and if an owned `PathBuf` is needed, pass
    /// `dir.to_path_buf()` while keeping `dir` alive. Since it is `Deref<Target = Path>`,
    /// `&dir` and `dir.join(..)` can be used as they are.
    struct TempDir(std::path::PathBuf);

    impl std::ops::Deref for TempDir {
        type Target = std::path::Path;
        fn deref(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl AsRef<std::path::Path> for TempDir {
        fn as_ref(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl Drop for TempDir {
        /// Restores the permissions first — if the test that makes the directory unwritable
        /// (`a_failed_store_leaves_the_previous_token_file_intact`) is aborted by a panic, it
        /// arrives here still at 0500, and then `remove_dir_all` fails silently and the leak
        /// remains.
        ///
        /// This does not panic — a double panic during unwinding is a SIGABRT.
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// Creates one [`TempDir`].
    ///
    /// The up-front removal is kept, but what it catches is **narrow** — only the case where
    /// a run that reused the same pid could not even run `Drop`, for example because of
    /// SIGKILL.
    ///
    /// The earlier doc said "the pid and thread id are reused on the next run, so the
    /// up-front removal clears the leftovers of the previous run", but that was **wrong.**
    /// The pid changed on every run so the names almost never matched; as a result one
    /// directory per (run × tag × thread) accumulated permanently in `/tmp`, and it became an
    /// operational incident in which `systemd-tmpfiles` blocked `sysinit.target` for 76
    /// minutes at boot. What guarantees the cleanup is `Drop`.
    ///
    /// The measured figures of the incident are written in **one place** only, the
    /// `fleetwire::test_support::temp_dir` doc — do not duplicate them here (a number that had
    /// been corrected once was scattered across five places).
    fn temp_dir(tag: &str) -> TempDir {
        let base = std::env::temp_dir().join(format!(
            "fleetwire-test-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o700));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&base)
            .expect("temp dir");
        TempDir(base)
    }

    /// Creates a file with the given mode.
    ///
    /// Plain `std::fs::write` is subject to the umask (usually 022) and yields 0644, and then
    /// every fixture stops at [`StoreError::BadPermissions`] before parsing — this trap
    /// actually broke the tests in the original plan text.
    fn write_file_with_mode(path: &std::path::Path, contents: &str, mode: u32) {
        std::fs::write(path, contents).expect("write fixture");
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).expect("chmod");
    }

    /// One line of well-formed token JSON that this store accepts.
    const VALID_JSON: &str = r#"{"access_token":"AT","refresh_token":"NA_RT","expires_at":1}"#;

    /// The names of the entries in `dir` other than the files this store **legitimately
    /// leaves behind**.
    ///
    /// `LOCK_FILE` is excluded too. A2 Task 5 creates `tokens.lock` in the same directory as
    /// the tokens (`lock_path_sits_beside_the_token_file_in_the_same_directory` pins that it
    /// must), and if only the token file were filtered out, the three leftover assertions
    /// would blow up with mysterious failures the moment Task 5 started. This heads that off
    /// in advance.
    fn unexpected_entries(dir: &std::path::Path) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .expect("readdir")
            .filter_map(Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n != TOKEN_FILE && n != LOCK_FILE)
            .collect();
        names.sort();
        names
    }

    /// Verifies the mode of a file or directory created by the implementation, independently
    /// of the umask.
    ///
    /// Comparing the whole value, as in `assert_eq!(mode, 0o600)`, is wrong: `open(2)` and
    /// `mkdir(2)` apply the umask to the requested mode as well, so on a machine with umask
    /// 0277 a **correct** implementation produces 0400 and the test breaks for nothing. The
    /// two properties are examined separately.
    /// - `mode & 0o077 == 0` — the security property. The umask **cannot grant more**
    ///   permissions, so this assertion must hold regardless of the umask.
    /// - `mode & 0o700 == owner_bits` — the intent. When the umask shaves owner bits the
    ///   result gets tighter, not looser, so this one is checked as a `>=` relation.
    #[track_caller]
    fn assert_owner_only(mode: u32, owner_bits: u32, what: &str) {
        assert_eq!(
            mode & 0o077,
            0,
            "{what}: 소유자 외 권한 비트가 남아 있다 (mode {mode:o})"
        );
        assert_eq!(
            mode & 0o700 & !owner_bits,
            0,
            "{what}: 의도한 소유자 권한({owner_bits:o})보다 넓다 (mode {mode:o})"
        );
    }

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
    fn is_fresh_pins_the_exact_skew_boundary() {
        // 위 테스트(1_030 / 1_090)는 경계를 건드리지 않아 `>` 를 `>=` 로 바꿔도 통과한다.
        // 여기서 `now + skew` 정확히 그 지점을 못박는다.
        assert_eq!(
            EXPIRY_SKEW,
            Duration::from_secs(60),
            "아래 리터럴은 skew=60초를 전제한다"
        );
        let clock = FixedClock::new(1_000);
        // 경계 바로 앞 — 여전히 skew 창 안 → fresh 아님.
        assert!(!token_set_expiring_at(1_059).is_fresh(&clock, EXPIRY_SKEW));
        // 경계 정확히(`expires_at == now + skew`) — **fresh 아님**.
        // 비교가 `>` 이지 `>=` 가 아님을 고정한다. `>=` 로 바꾸면 이 줄만 깨진다.
        assert!(!token_set_expiring_at(1_060).is_fresh(&clock, EXPIRY_SKEW));
        // 경계 바로 뒤 — skew 바깥 → fresh.
        assert!(token_set_expiring_at(1_061).is_fresh(&clock, EXPIRY_SKEW));
    }

    #[test]
    fn is_fresh_uses_the_skew_argument_not_the_constant() {
        // 인자를 무시하고 EXPIRY_SKEW 를 직접 읽는 구현은 다른 모든 테스트를 통과한다.
        let clock = FixedClock::new(1_000);
        let ts = token_set_expiring_at(1_030);
        assert!(
            ts.is_fresh(&clock, Duration::ZERO),
            "skew 0 이면 30초 남은 토큰은 유효"
        );
        assert!(
            !ts.is_fresh(&clock, Duration::from_secs(3_600)),
            "skew 가 크면 같은 토큰이 만료 임박으로 뒤집혀야 한다"
        );
    }

    #[test]
    fn is_fresh_is_false_for_already_expired_token_without_underflow() {
        let clock = FixedClock::new(2_000);
        // expires_at < now — u64 뺄셈이 언더플로하면 panic 하거나 거대한 값이 되어 fresh 로 오판한다.
        assert!(!token_set_expiring_at(1_000).is_fresh(&clock, EXPIRY_SKEW));
        assert!(!token_set_expiring_at(0).is_fresh(&clock, EXPIRY_SKEW));
    }

    #[test]
    fn is_fresh_does_not_overflow_when_the_clock_is_near_u64_max() {
        // 언더플로의 반대 방향: `now + skew` 를 그냥 더하면 debug 빌드에서 패닉한다.
        let clock = FixedClock::new(u64::MAX - 1);
        assert!(!token_set_expiring_at(u64::MAX).is_fresh(&clock, EXPIRY_SKEW));
    }

    #[test]
    fn a_real_token_is_not_fresh_under_a_pre_epoch_clock() {
        // 값 자체가 아니라 **그 값의 결과**를 못박는다 — 원래 버그(0 으로 포화)를
        // 잡았을 성질이 바로 이것이다. 0 이면 실제 토큰이 영원히 fresh 로 읽혀
        // 죽은 access token 을 계속 실어 보낸다.
        let pre_epoch = std::time::UNIX_EPOCH - Duration::from_secs(1);
        let clock = FixedClock::new(crate::clock::secs_since_epoch(pre_epoch));
        assert!(
            !token_set_expiring_at(1_700_000_000).is_fresh(&clock, EXPIRY_SKEW),
            "epoch 이전 시계에서는 어떤 토큰도 fresh 가 아니어야 한다(=항상 refresh)"
        );
    }

    #[test]
    fn advancing_the_clock_flips_the_same_token_from_fresh_to_stale() {
        // 어떤 테스트도 시계를 움직이지 않으면 `now_unix()` 를 상수로 하드코딩한
        // 시계 구현이 만료 테스트를 전부 통과한다. 같은 TokenSet 하나로 전후를 본다.
        let clock = FixedClock::new(1_000);
        let ts = token_set_expiring_at(1_100);
        assert!(ts.is_fresh(&clock, EXPIRY_SKEW), "1_100 > 1_000+60 → fresh");
        clock.advance(50);
        assert!(
            !ts.is_fresh(&clock, EXPIRY_SKEW),
            "now=1_050 이면 1_100 > 1_110 이 거짓 → 같은 토큰이 stale 로 뒤집혀야 한다"
        );
    }

    #[test]
    fn accessors_do_not_swap_the_two_tokens() {
        // 두 접근자가 서로 바뀐 구현은 만료·Debug·직렬화 테스트를 전부 통과한다.
        let ts = token_set_expiring_at(1_000);
        assert_eq!(ts.access_token(), "AT");
        assert_eq!(ts.refresh_token(), "NA_RT");
    }

    #[test]
    fn debug_never_prints_token_material() {
        let ts = token_set_expiring_at(1_000);
        let rendered = format!("{ts:?}");
        assert!(!rendered.contains("AT"), "access token leaked: {rendered}");
        assert!(
            !rendered.contains("NA_RT"),
            "refresh token leaked: {rendered}"
        );
        assert!(
            rendered.contains("[REDACTED]"),
            "no redaction marker: {rendered}"
        );
    }

    #[test]
    fn debug_marker_is_the_crate_wide_redaction_const() {
        // REQ-18 단일 리댁션 계층: 마커를 이 파일에서 따로 하드코딩하면 출처가 둘로
        // 갈라져 드리프트한다. `redaction::REDACTED` 를 바꾸면 여기서 잡힌다.
        let rendered = format!("{:?}", token_set_expiring_at(1_000));
        assert!(
            rendered.contains(crate::redaction::REDACTED),
            "marker diverged from redaction::REDACTED: {rendered}"
        );
    }

    #[test]
    fn debug_does_not_leak_unknown_field_values() {
        // 두 토큰만 가리고 `extra` 를 통째로 찍는 Debug 는 위 테스트를 통과한다 —
        // 픽스처에 미지 필드가 없기 때문이다. 신버전이 쓴 파일에는 이 버전이 모르는
        // 비밀(예: id_token)이 들어있을 수 있다.
        let json = r#"{"access_token":"AT","refresh_token":"NA_RT","expires_at":1000,"id_token":"SECRET_ID_TOKEN_VALUE"}"#;
        let ts: TokenSet = serde_json::from_str(json).expect("must deserialize");
        let rendered = format!("{ts:?}");
        assert!(
            !rendered.contains("SECRET_ID_TOKEN_VALUE"),
            "unknown-field value leaked: {rendered}"
        );
        assert!(
            rendered.contains("extra_keys: 1"),
            "unknown-field count missing: {rendered}"
        );
    }

    #[test]
    fn unknown_fields_survive_a_round_trip() {
        // 신버전 fleetwire 가 쓴 파일을 구버전이 읽고 되쓸 때 필드를 잃지 않아야 한다.
        let json =
            r#"{"access_token":"AT","refresh_token":"NA_RT","expires_at":1000,"future_field":42}"#;
        let ts: TokenSet = serde_json::from_str(json).expect("must deserialize");
        let back = serde_json::to_value(&ts).expect("must serialize");
        assert_eq!(back["future_field"], serde_json::json!(42));
    }

    #[test]
    fn round_trip_preserves_known_fields_without_duplicating_them() {
        // 미지 필드만 보는 위 테스트는 알려진 필드가 사라지거나 `extra` 에 중복
        // 흡수되는 구현을 잡지 못한다.
        let json =
            r#"{"access_token":"AT","refresh_token":"NA_RT","expires_at":1000,"future_field":42}"#;
        let ts: TokenSet = serde_json::from_str(json).expect("must deserialize");
        assert_eq!(
            ts.extra.len(),
            1,
            "extra swallowed a known field: {:?}",
            ts.extra
        );

        let back = serde_json::to_value(&ts).expect("must serialize");
        let obj = back.as_object().expect("serializes to a JSON object");
        assert_eq!(obj.len(), 4, "field count changed on round trip: {obj:?}");
        assert_eq!(obj["access_token"], serde_json::json!("AT"));
        assert_eq!(obj["refresh_token"], serde_json::json!("NA_RT"));
        assert_eq!(obj["expires_at"], serde_json::json!(1_000));
        assert_eq!(obj["future_field"], serde_json::json!(42));
    }

    #[test]
    fn store_then_load_round_trips_and_file_is_0600() {
        let dir = temp_dir("roundtrip");
        let store = TokenStore::new(dir.to_path_buf());
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
        assert_owner_only(mode, 0o600, "token file");
    }

    #[test]
    fn store_leaves_no_temp_file_behind() {
        let dir = temp_dir("notemp");
        let store = TokenStore::new(dir.to_path_buf());
        store.store(&token_set_expiring_at(1)).expect("store");

        assert_eq!(
            unexpected_entries(&dir),
            Vec::<String>::new(),
            "임시 파일이 남았다"
        );
    }

    #[test]
    fn store_leaves_no_temp_file_behind_when_the_rename_fails() {
        // 성공 경로만 보는 위 테스트는, 실패할 때마다 `.tmp` 를 흘리는 구현을
        // 통과시킨다. rename 을 확실히 실패시키려면 최종 경로에 **디렉터리**를
        // 놓는다(EISDIR) — 임시 파일이 이미 만들어진 뒤에 실패하는 지점이다.
        let dir = temp_dir("notemp-fail");
        let store = TokenStore::new(dir.to_path_buf());
        std::fs::create_dir(store.token_path()).expect("occupy the final path with a dir");

        let err = store
            .store(&token_set_expiring_at(1))
            .expect_err("rename 이 실패해야 한다");
        assert!(matches!(err, StoreError::Io(_)), "예상 밖 오류: {err:?}");
        assert_eq!(
            unexpected_entries(&dir),
            Vec::<String>::new(),
            "실패 경로에서 임시 파일이 남았다"
        );
    }

    #[test]
    fn load_refuses_a_symlinked_token_file() {
        let dir = temp_dir("symlink");
        let real = dir.join("elsewhere.json");
        // 미끼를 0600 으로 둔다. 0644 로 두면 링크를 **따라가는** 구현도
        // 권한 검사에서 걸려 Err 가 되므로, 이 테스트가 symlink 검사가 아니라
        // 권한 검사를 시험하게 된다(계획서 원문의 취약점).
        write_file_with_mode(&real, VALID_JSON, 0o600);
        let link = dir.join(TOKEN_FILE);
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        let store = TokenStore::new(dir.to_path_buf());
        assert!(
            matches!(store.load(), Err(StoreError::Symlink)),
            "symlink 을 통한 읽기를 거부해야 한다"
        );
    }

    #[test]
    fn load_reports_symlink_not_bad_permissions_when_the_path_is_both() {
        // 우선순위 못박기: 경로가 symlink 이면서 대상 권한도 과대일 때 무엇을
        // 보고하는가. **Symlink 가 이긴다** — 링크는 따라가지 않으므로 대상의
        // 권한은 애초에 볼 자격이 없고, 링크 자신의 lstat mode 는 리눅스에서 늘
        // 0777 이라 BadPermissions 로 보고하면 진짜 원인을 가린다.
        let dir = temp_dir("symlink-badperm");
        let real = dir.join("elsewhere.json");
        write_file_with_mode(&real, VALID_JSON, 0o644);
        std::os::unix::fs::symlink(&real, dir.join(TOKEN_FILE)).expect("symlink");

        let err = TokenStore::new(dir.to_path_buf())
            .load()
            .expect_err("must fail");
        assert!(
            matches!(err, StoreError::Symlink),
            "symlink 이 권한보다 먼저 보고돼야 한다: {err:?}"
        );
    }

    #[test]
    fn load_rejects_group_or_world_readable_token_files() {
        // 계획서 원문에는 BadPermissions 를 확인하는 테스트가 아예 없었다 —
        // 권한 검사를 통째로 지운 구현이 전부 통과했다.
        for mode in [0o644, 0o640, 0o604, 0o660] {
            let dir = temp_dir(&format!("badperm-{mode:o}"));
            write_file_with_mode(&dir.join(TOKEN_FILE), VALID_JSON, mode);
            let err = TokenStore::new(dir.to_path_buf())
                .load()
                .expect_err("must fail");
            assert!(
                matches!(err, StoreError::BadPermissions),
                "mode {mode:o} 를 거부해야 한다: {err:?}"
            );
        }
        // 반대편: 0600 은 통과해야 한다(검사가 무조건 거부하는 구현 배제).
        let dir = temp_dir("badperm-ok");
        write_file_with_mode(&dir.join(TOKEN_FILE), VALID_JSON, 0o600);
        TokenStore::new(dir.to_path_buf())
            .load()
            .expect("0600 은 허용돼야 한다");
    }

    #[test]
    fn load_reports_not_found_distinctly_from_io_error() {
        // `TempDir` 은 가드다 — 바인딩을 살려 둬야 디렉터리가 단언 시점까지 존재한다.
        //    `TokenStore::new(temp_dir(..))` 처럼 즉시 버리면 "디렉터리가 있는데 파일만
        //    없다" 를 시험하려던 첫 단언이 몰래 "디렉터리도 없다" 로 바뀐다.
        let dir = temp_dir("missing");
        let store = TokenStore::new(dir.to_path_buf());
        assert!(matches!(store.load(), Err(StoreError::NotFound)));

        // 디렉터리 자체가 없을 때도 Io 가 아니라 NotFound 다(아직 로그인 안 함).
        let base = temp_dir("missing-dir");
        let nested = TokenStore::new(base.join("not").join("created"));
        assert!(matches!(nested.load(), Err(StoreError::NotFound)));
    }

    #[test]
    fn load_rejects_a_directory_at_the_token_path() {
        // `is_file()` 검사를 지우면 리눅스에서 디렉터리 열기는 성공하고
        // read_to_string 이 EISDIR 로 실패해 Io 가 된다 — 그 차이를 못박는다.
        let dir = temp_dir("token-is-dir");
        std::fs::create_dir(dir.join(TOKEN_FILE)).expect("mkdir");
        let err = TokenStore::new(dir.to_path_buf())
            .load()
            .expect_err("must fail");
        assert!(matches!(err, StoreError::Malformed), "{err:?}");
    }

    #[test]
    fn load_reports_malformed_without_echoing_file_contents() {
        let dir = temp_dir("malformed");
        // 0600 이어야 파싱 단계까지 도달한다 — write 만 하면 umask 때문에 0644 다.
        write_file_with_mode(&dir.join(TOKEN_FILE), "SECRET_NOT_JSON", 0o600);
        let err = TokenStore::new(dir.to_path_buf())
            .load()
            .expect_err("must fail");
        assert!(matches!(err, StoreError::Malformed), "{err:?}");
        // Display 와 Debug 를 **둘 다** 본다: Display 만 가리고 Debug 로 serde
        // 메시지를 실어 나르는 구현이 Display-only 단언을 통과한다.
        let display = format!("{err}");
        let debug = format!("{err:?}");
        assert!(
            !display.contains("SECRET_NOT_JSON"),
            "파일 내용이 오류에 새어나왔다: {display}"
        );
        assert!(
            !debug.contains("SECRET_NOT_JSON"),
            "파일 내용이 Debug 로 새어나왔다: {debug}"
        );
    }

    #[test]
    fn load_reports_malformed_without_echoing_token_material() {
        // 위 테스트의 픽스처는 토큰처럼 생기지 않았다. JSON 으로는 유효하지만
        // 스키마에 안 맞는 파일 — serde 메시지가 값을 실어 나르기 가장 쉬운 형태다.
        let dir = temp_dir("malformed-tokenish");
        write_file_with_mode(
            &dir.join(TOKEN_FILE),
            r#"{"access_token":"SECRET_ACCESS_VALUE","expires_at":"not-a-number"}"#,
            0o600,
        );
        let err = TokenStore::new(dir.to_path_buf())
            .load()
            .expect_err("must fail");
        assert!(matches!(err, StoreError::Malformed), "{err:?}");
        for rendered in [format!("{err}"), format!("{err:?}")] {
            assert!(
                !rendered.contains("SECRET_ACCESS_VALUE"),
                "토큰 값이 오류에 새어나왔다: {rendered}"
            );
        }
    }

    #[test]
    fn load_checks_permissions_before_parsing() {
        // 우선순위 못박기 #2: 손상 + 과대권한이 겹치면 BadPermissions 다.
        // 파일을 읽어 파싱한 **뒤에** 권한을 보는 구현은 이미 내용을 메모리에
        // 올린 셈이므로, 검사 순서 자체가 요구사항이다.
        let dir = temp_dir("perm-before-parse");
        write_file_with_mode(&dir.join(TOKEN_FILE), "SECRET_NOT_JSON", 0o644);
        let err = TokenStore::new(dir.to_path_buf())
            .load()
            .expect_err("must fail");
        assert!(
            matches!(err, StoreError::BadPermissions),
            "권한 검사가 파싱보다 먼저여야 한다: {err:?}"
        );
    }

    #[test]
    fn store_overwrites_atomically_leaving_previous_content_readable_until_swap() {
        let dir = temp_dir("atomic");
        let store = TokenStore::new(dir.to_path_buf());
        store.store(&token_set_expiring_at(111)).expect("first");
        store.store(&token_set_expiring_at(222)).expect("second");

        let raw = std::fs::read_to_string(store.token_path()).expect("read");
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("valid json");
        assert_eq!(parsed["expires_at"], serde_json::json!(222));
    }

    #[test]
    fn store_replaces_the_directory_entry_instead_of_writing_in_place() {
        // 위 테스트("나중 쓴 값이 보인다")는 최종 파일을 truncate 하고 제자리에
        // 덮어쓰는 **비원자적** 구현도 통과시킨다. 원자성의 관측 가능한 흔적은
        // "이전 inode 를 건드리지 않는다" 는 것이다 — rename 은 디렉터리 엔트리만
        // 바꾸므로, 옛 inode 를 가리키는 hard link 는 옛 내용을 그대로 본다.
        let dir = temp_dir("atomic-inode");
        let store = TokenStore::new(dir.to_path_buf());
        store.store(&token_set_expiring_at(111)).expect("first");

        let observer = dir.join("observer.json");
        std::fs::hard_link(store.token_path(), &observer).expect("hard link");

        store.store(&token_set_expiring_at(222)).expect("second");

        let now: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(store.token_path()).expect("read new"))
                .expect("json");
        let old: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&observer).expect("read old"))
                .expect("json");
        assert_eq!(now["expires_at"], serde_json::json!(222));
        assert_eq!(
            old["expires_at"],
            serde_json::json!(111),
            "옛 inode 가 덮어써졌다 — rename 이 아니라 제자리 쓰기다"
        );
    }

    #[test]
    fn store_refuses_a_symlinked_token_path_and_leaves_the_victim_untouched() {
        // rename 은 대상 symlink 를 따라가지 않으므로 피해자 파일이 망가지진
        // 않지만, 호출자는 "링크를 만났다" 는 사실을 알아야 한다. 두 가지를
        // 동시에 못박는다: 오류 종류 + 피해자 내용 불변.
        let dir = temp_dir("store-symlink");
        let victim = dir.join("victim.json");
        write_file_with_mode(&victim, "VICTIM_CONTENT", 0o600);
        std::os::unix::fs::symlink(&victim, dir.join(TOKEN_FILE)).expect("symlink");

        let store = TokenStore::new(dir.to_path_buf());
        let err = store
            .store(&token_set_expiring_at(1))
            .expect_err("symlink 대상 저장을 거부해야 한다");
        assert!(matches!(err, StoreError::Symlink), "{err:?}");
        assert_eq!(
            std::fs::read_to_string(&victim).expect("read victim"),
            "VICTIM_CONTENT",
            "심볼릭 링크를 통해 피해자 파일에 토큰을 썼다"
        );
    }

    #[test]
    fn store_refuses_a_symlink_planted_at_the_temp_path() {
        // 임시 경로도 공격 표면이다. 잔여물 정리를 `remove_file` 로 무조건
        // 하면 공격자가 심은 링크를 조용히 치우고 진행하게 된다 — 여기서는
        // 거부한다. 피해자 파일이 그대로인지도 함께 본다.
        let dir = temp_dir("store-tmp-symlink");
        let victim = dir.join("victim.json");
        write_file_with_mode(&victim, "VICTIM_CONTENT", 0o600);
        let store = TokenStore::new(dir.to_path_buf());
        std::os::unix::fs::symlink(&victim, store.temp_path()).expect("symlink");

        let err = store
            .store(&token_set_expiring_at(1))
            .expect_err("임시 경로의 symlink 를 거부해야 한다");
        assert!(matches!(err, StoreError::Symlink), "{err:?}");
        assert_eq!(
            std::fs::read_to_string(&victim).expect("read victim"),
            "VICTIM_CONTENT",
            "임시 경로의 심볼릭 링크를 따라 피해자 파일에 토큰을 썼다"
        );
    }

    #[test]
    fn store_clears_a_stale_regular_temp_file() {
        // 링크가 아닌 평범한 잔여물(이전 실행의 크래시)은 저장을 영구히
        // 막으면 안 된다 — O_EXCL 이 EEXIST 로 실패하기 때문이다.
        let dir = temp_dir("store-stale-tmp");
        let store = TokenStore::new(dir.to_path_buf());
        write_file_with_mode(&store.temp_path(), "STALE", 0o600);

        store.store(&token_set_expiring_at(7)).expect("store");
        assert_eq!(unexpected_entries(&dir), Vec::<String>::new());
    }

    #[test]
    fn store_creates_a_missing_token_directory() {
        // 가드 바인딩을 살려 둔다 — 즉시 버리면 조상 디렉터리째 사라져서
        //    `store` 가 만드는 것이 `nested/deep` 이 아니라 트리 전체가 된다.
        let root = temp_dir("mkdir");
        let store = TokenStore::new(root.join("nested").join("deep"));
        store.store(&token_set_expiring_at(1)).expect("store");
        store.load().expect("load back");
    }

    #[test]
    fn store_creates_the_token_directory_owner_only() {
        // 설계 §4 는 `~/.fleetwire/ 0700` 을 명시한다. `create_dir_all` 은 mode 에
        // umask 를 적용하므로 umask 022 에서 0755, umask 002 + 공유 primary group
        // 이면 0775(그룹 쓰기 가능)가 된다 — 이 파일의 symlink·TOCTOU 방어가 전부
        // "남이 이 디렉터리를 못 건드린다" 를 전제하므로 그 자체로 구멍이다.
        let root = temp_dir("mkdir-mode");
        // 조상까지 함께 만들어, recursive 가 **모든** 새 디렉터리에 mode 를 거는지 본다.
        let deep = root.join("nested").join("deep");
        let store = TokenStore::new(deep.clone());
        store.store(&token_set_expiring_at(1)).expect("store");

        for d in [root.join("nested"), deep] {
            let mode = std::fs::metadata(&d)
                .expect("dir must exist")
                .permissions()
                .mode()
                & 0o777;
            assert_owner_only(mode, 0o700, &format!("token dir {}", d.display()));
        }
    }

    #[test]
    fn store_refuses_a_group_or_world_accessible_token_directory() {
        // `DirBuilder::mode` 는 **새로 만드는** 디렉터리에만 걸린다. 이미 0775 로
        // 존재하던 `~/.fleetwire` 는 그대로이므로 절반밖에 못 막는다 — 나머지
        // 절반이 이 거부다. `set_permissions` 로 만들었으므로 umask 와 무관하다.
        for mode in [0o755, 0o775, 0o770, 0o707] {
            let dir = temp_dir(&format!("baddir-store-{mode:o}"));
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).expect("chmod");
            let err = TokenStore::new(dir.to_path_buf())
                .store(&token_set_expiring_at(1))
                .expect_err("열린 디렉터리에 토큰을 쓰면 안 된다");
            assert!(
                matches!(err, StoreError::BadDirPermissions),
                "mode {mode:o}: {err:?}"
            );
        }
    }

    #[test]
    fn load_refuses_a_group_or_world_writable_token_directory() {
        // 이것이 실제 공격이다: 디렉터리에 쓸 수 있는 사람은 tokens.json 을 지우고
        // **자기 소유의 평범한 0600 파일**을 놓을 수 있다. 그러면 symlink 검사도,
        // 권한 검사도, ino/dev 재검사도 전부 통과한다(uid 검사가 없다) — 조용한
        // 토큰 바꿔치기다. 파일 단위 검사로는 원리적으로 못 막으므로 전제를 본다.
        let dir = temp_dir("baddir-load");
        write_file_with_mode(&dir.join(TOKEN_FILE), VALID_JSON, 0o600);
        // 파일 자체는 흠잡을 데가 없다 — 이 상태로는 읽힌다.
        TokenStore::new(dir.to_path_buf())
            .load()
            .expect("0700 디렉터리 + 0600 파일은 읽혀야 한다");

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).expect("chmod");
        let err = TokenStore::new(dir.to_path_buf())
            .load()
            .expect_err("must fail");
        assert!(
            matches!(err, StoreError::BadDirPermissions),
            "완벽한 0600 파일이어도 디렉터리가 열려 있으면 거부해야 한다: {err:?}"
        );
    }

    #[test]
    fn a_symlinked_token_directory_is_refused_rather_than_followed() {
        // final fix wave / triage 7. `metadata` 는 링크를 따라가므로, 링크가 가리키는
        // 곳이 0700 이기만 하면 통과한다 — 즉 남이 `~/.fleetwire` 를 자기 디렉터리로
        // 향하는 symlink 로 바꿔 두면, 파일 쪽의 촘촘한 symlink 방어를 전부
        // 우회해서 토큰을 자기 디렉터리에 쓰게 만들 수 있다. 디렉터리 쪽도
        // `symlink_metadata` 로 링크 자체를 본다.
        let base = temp_dir("symlinkdir");
        let real = base.join("real");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&real)
            .expect("real dir");
        write_file_with_mode(&real.join(TOKEN_FILE), VALID_JSON, 0o600);
        // 대조군: 링크를 거치지 않으면 정상적으로 읽힌다.
        TokenStore::new(real.clone())
            .load()
            .expect("실제 0700 디렉터리는 읽혀야 한다");

        let link = base.join("link");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        let err = TokenStore::new(link.clone())
            .load()
            .expect_err("symlink 디렉터리를 따라갔다");
        assert!(matches!(err, StoreError::Symlink), "{err:?}");

        // 쓰기 쪽도 같은 가드를 지난다(`prepare_dir` → `ensure_private_dir`).
        let err = TokenStore::new(link)
            .store(&token_set_expiring_at(1))
            .expect_err("symlink 디렉터리에 토큰을 썼다");
        assert!(matches!(err, StoreError::Symlink), "{err:?}");
    }

    #[test]
    fn load_reports_bad_dir_permissions_before_not_found() {
        // 우선순위 못박기: 디렉터리가 열려 있으면 토큰 파일 유무보다 먼저 보고한다.
        // 파일이 없다는 사실 자체가 "방금 누가 지웠다" 일 수 있다.
        let dir = temp_dir("baddir-precedence");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).expect("chmod");
        let err = TokenStore::new(dir.to_path_buf())
            .load()
            .expect_err("must fail");
        assert!(matches!(err, StoreError::BadDirPermissions), "{err:?}");
    }

    #[test]
    fn store_writes_the_token_verbatim_without_recomputing_expiry() {
        // A2 Task 3 리뷰에서 넘어온 위험: epoch 이전 시계에서 `now + expires_in`
        // 은 u64::MAX 로 포화해 "영원히 만료되지 않는" 토큰을 남긴다. Task 4 의
        // `store` 는 시각 계산을 **하지 않는다** — 받은 것을 그대로 쓴다.
        // 미지 필드 보존도 같은 단언으로 못박는다(파일을 왕복해도 잃지 않는다).
        let dir = temp_dir("verbatim");
        let store = TokenStore::new(dir.to_path_buf());
        let ts: TokenSet = serde_json::from_str(
            r#"{"access_token":"AT","refresh_token":"NA_RT","expires_at":1234567890,"id_token":"KEEP_ME"}"#,
        )
        .expect("fixture");

        store.store(&ts).expect("store");
        let raw = std::fs::read_to_string(store.token_path()).expect("read");
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("json");
        assert_eq!(
            parsed["expires_at"],
            serde_json::json!(1_234_567_890_u64),
            "store 가 만료 시각을 다시 계산했다"
        );
        assert_eq!(
            parsed["id_token"],
            serde_json::json!("KEEP_ME"),
            "미지 필드가 파일 왕복에서 사라졌다"
        );
    }

    #[test]
    fn load_rejects_a_fifo_at_the_token_path_without_blocking_on_it() {
        // `load_rejects_a_directory_at_the_token_path` 는 `is_file()` 검사를 지운
        // 구현을 "오류 종류가 다르다"(Io) 로만 잡는다 — 디렉터리는 열리기라도 하기
        // 때문이다. FIFO 는 그보다 나쁘다: 쓰는 쪽이 없는 FIFO 를 O_RDONLY 로 열면
        // `open(2)` 이 **영원히 블록한다**. 검사가 열기보다 먼저여야 하는 진짜 이유가
        // 그것이고, 오류 종류가 아니라 "끝나기는 하는가" 로 못박는다.
        //
        // mkfifo(1) 이 없으면 조용히 건너뛴다(프로세스 전역 상태 미사용).
        let dir = temp_dir("fifo");
        let path = dir.join(TOKEN_FILE);
        let made = std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !made {
            eprintln!("skipped: mkfifo(1) unavailable — FIFO 경로를 만들 수 없다");
            return;
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");

        // 별도 스레드에서 돌려 hang 을 무한 대기가 아니라 실패로 만든다.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let outcome = TokenStore::new(dir.to_path_buf()).load();
            let _ = tx.send(matches!(outcome, Err(StoreError::Malformed)));
        });
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(is_malformed) => assert!(is_malformed, "FIFO 를 Malformed 로 거부해야 한다"),
            Err(_) => panic!("load 가 FIFO 를 열려다 블록했다 — 열기 전에 파일 종류를 봐야 한다"),
        }
    }

    #[test]
    fn a_failed_store_leaves_the_previous_token_file_intact() {
        // 저장이 실패했을 때 잃으면 안 되는 것은 새 토큰이 아니라 **옛 토큰**이다.
        // 최종 경로를 먼저 비우고 쓰는 구현(truncate 후 쓰기)은 실패할 때마다 멀쩡히
        // 쓰던 토큰까지 날려 재-OAuth 를 강제한다. 디렉터리를 읽기 전용으로 만들어
        // 임시 파일 생성 단계에서 실패시킨다.
        let dir = temp_dir("failed-store-keeps-old");
        let store = TokenStore::new(dir.to_path_buf());
        store.store(&token_set_expiring_at(111)).expect("seed");

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).expect("chmod");
        // root 로 돌리면 0500 에도 쓸 수 있어 실패를 연출할 수 없다 — 그 사실을
        // 조용히 넘기지 않고 명시적으로 건너뛴다.
        let denied = std::fs::File::create(dir.join(".write-probe")).is_err();
        if !denied {
            drop(std::fs::remove_file(dir.join(".write-probe")));
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
                .expect("restore");
            eprintln!("skipped: root 권한 — 읽기 전용 디렉터리에서도 쓰기가 성공한다");
            return;
        }

        let err = store
            .store(&token_set_expiring_at(222))
            .expect_err("읽기 전용 디렉터리에 저장이 성공하면 안 된다");
        assert!(matches!(err, StoreError::Io(_)), "예상 밖 오류: {err:?}");

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("restore");
        let back = store.load().expect("옛 토큰이 그대로 읽혀야 한다");
        assert_eq!(back.access_token(), "AT");
        assert_eq!(
            serde_json::to_value(&back).expect("json")["expires_at"],
            serde_json::json!(111),
            "저장에 실패했는데 옛 토큰 파일이 손상됐다"
        );
        assert_eq!(unexpected_entries(&dir), Vec::<String>::new());
    }

    #[test]
    fn lock_path_sits_beside_the_token_file_in_the_same_directory() {
        // Task 5 의 락은 **경로가 아니라 inode** 기준이라, 락 파일이 토큰과 다른
        // 디렉터리로 새면 프로세스 간 상호배제가 조용히 무력화된다.
        let dir = temp_dir("paths");
        let store = TokenStore::new(dir.to_path_buf());
        assert_eq!(store.token_path(), dir.join(TOKEN_FILE));
        assert_eq!(store.lock_path(), dir.join(LOCK_FILE));
        assert_eq!(store.lock_path().parent(), store.token_path().parent());
        assert_ne!(store.lock_path(), store.token_path());
    }
}

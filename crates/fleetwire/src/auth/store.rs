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
        // The destructuring is the safety device of this impl: adding a field breaks
        // compilation here with `E0027` (pattern does not mention field). Had the fields been
        // accessed as `self.field`, a new field would silently drop out of the output and no
        // test would fail. `: _` marks "the value is deliberately not printed".
        let Self {
            access_token: _,
            refresh_token: _,
            expires_at,
            extra,
        } = self;
        // The marker uses the constant of the crate's single redaction layer (REQ-18) —
        // writing the string again here would split the source into two.
        f.debug_struct("TokenSet")
            .field("access_token", &REDACTED)
            .field("refresh_token", &REDACTED)
            .field("expires_at", expires_at)
            // Only the count of `extra` — an unknown field put in by a newer version may be a
            // secret.
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
        // symlink_metadata does not follow links, so the link itself can be identified.
        if pre.file_type().is_symlink() {
            return Err(StoreError::Symlink);
        }
        if !pre.file_type().is_file() {
            return Err(StoreError::Malformed);
        }

        let mut file = File::open(&path)?;
        // Is what was opened the very file that was just checked?
        let opened = file.metadata()?;
        if opened.ino() != pre.ino() || opened.dev() != pre.dev() {
            return Err(StoreError::Symlink);
        }
        // Permissions are checked on the **opened handle**, not on the path.
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
        // If a previous run left something behind, O_EXCL would fail with EEXIST and saving
        // would be blocked permanently, so it is cleared first. However, **a symlink is
        // refused, not cleared** — what we create is always a plain file, so a link at the
        // temporary path is not a leftover but an attack. Silently deleting it and carrying on
        // would amount to erasing only the traces.
        match std::fs::symlink_metadata(&tmp_path) {
            Ok(m) if m.file_type().is_symlink() => return Err(StoreError::Symlink),
            Ok(_) => drop(std::fs::remove_file(&tmp_path)),
            Err(_) => {}
        }

        let json = serde_json::to_vec(set).map_err(|_| StoreError::Malformed)?;

        // On failure, whichever stage failed, no temporary file is left behind. An
        // implementation that cleans up only on the success path leaks a `.tmp` into the token
        // directory every time a save fails.
        let outcome = Self::write_then_swap(&tmp_path, &final_path, &json, &self.dir);
        if outcome.is_err() {
            // If it failed after the rename succeeded (at the fsync stage), the file is already
            // gone — a no-op.
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
            // No directory = not logged in yet. `load` uses this value as is.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(StoreError::NotFound),
            Err(e) => return Err(StoreError::Io(e)),
        };
        // The same policy as the file side (`load`): refuse rather than follow. The mode of a
        // symlink is usually 0777, so the permission check below would catch it as well, but
        // then the user would see "the permissions are too permissive" instead of the real
        // cause (the link) and try chmod in vain.
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
            // create_new(true) = O_CREAT|O_EXCL. Under POSIX, if the path is a symlink this
            // fails with EEXIST regardless of what the link points to, so this alone
            // establishes the symlink defence for the temporary file (O_NOFOLLOW is not
            // needed — see the preamble of plan T4). This flag is what closes the race window
            // between passing the explicit check above and arriving here.
            //
            // `.mode(0o600)` is applied **at creation time** — tightening with
            // `set_permissions` after creating would open a window in which someone else
            // could open the file.
            let mut f = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(tmp_path)?;
            f.write_all(json)?;
            f.sync_all()?;
        }

        // Even when the target is a symlink, rename **does not follow it** and replaces the
        // link itself. Because of the explicit check above, the target is not a symlink by the
        // time this is reached.
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
            "{what}: permission bits for others than the owner remain (mode {mode:o})"
        );
        assert_eq!(
            mode & 0o700 & !owner_bits,
            0,
            "{what}: wider than the intended owner permissions ({owner_bits:o}) (mode {mode:o})"
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
        // Expiry is 30 seconds away = inside the skew (60 seconds), so it must be refreshed
        // ahead of time and is not fresh.
        assert!(!token_set_expiring_at(1_030).is_fresh(&clock, EXPIRY_SKEW));
        // Expiry is 90 seconds away = outside the skew, so fresh.
        assert!(token_set_expiring_at(1_090).is_fresh(&clock, EXPIRY_SKEW));
    }

    #[test]
    fn is_fresh_pins_the_exact_skew_boundary() {
        // The test above (1_030 / 1_090) does not touch the boundary, so it passes even if `>`
        // is changed to `>=`. Here exactly the point `now + skew` is pinned.
        assert_eq!(
            EXPIRY_SKEW,
            Duration::from_secs(60),
            "the literals below assume skew=60 seconds"
        );
        let clock = FixedClock::new(1_000);
        // Just before the boundary — still inside the skew window, so not fresh.
        assert!(!token_set_expiring_at(1_059).is_fresh(&clock, EXPIRY_SKEW));
        // Exactly on the boundary (`expires_at == now + skew`) — **not fresh**.
        // This pins that the comparison is `>`, not `>=`. Changing it to `>=` breaks only this
        // line.
        assert!(!token_set_expiring_at(1_060).is_fresh(&clock, EXPIRY_SKEW));
        // Just after the boundary — outside the skew, so fresh.
        assert!(token_set_expiring_at(1_061).is_fresh(&clock, EXPIRY_SKEW));
    }

    #[test]
    fn is_fresh_uses_the_skew_argument_not_the_constant() {
        // An implementation that ignores the argument and reads EXPIRY_SKEW directly passes
        // every other test.
        let clock = FixedClock::new(1_000);
        let ts = token_set_expiring_at(1_030);
        assert!(
            ts.is_fresh(&clock, Duration::ZERO),
            "with skew 0 a token with 30 seconds left is valid"
        );
        assert!(
            !ts.is_fresh(&clock, Duration::from_secs(3_600)),
            "with a large skew the same token must flip to about-to-expire"
        );
    }

    #[test]
    fn is_fresh_is_false_for_already_expired_token_without_underflow() {
        let clock = FixedClock::new(2_000);
        // expires_at < now — if a u64 subtraction underflows, it either panics or becomes a
        // huge value and is misjudged as fresh.
        assert!(!token_set_expiring_at(1_000).is_fresh(&clock, EXPIRY_SKEW));
        assert!(!token_set_expiring_at(0).is_fresh(&clock, EXPIRY_SKEW));
    }

    #[test]
    fn is_fresh_does_not_overflow_when_the_clock_is_near_u64_max() {
        // The opposite direction of the underflow: adding `now + skew` naively panics in a
        // debug build.
        let clock = FixedClock::new(u64::MAX - 1);
        assert!(!token_set_expiring_at(u64::MAX).is_fresh(&clock, EXPIRY_SKEW));
    }

    #[test]
    fn a_real_token_is_not_fresh_under_a_pre_epoch_clock() {
        // This pins not the value itself but **the consequence of that value** — this is
        // exactly the property that would have caught the original bug (saturating to 0).
        // With 0 a real token reads as fresh forever and a dead access token keeps being sent.
        let pre_epoch = std::time::UNIX_EPOCH - Duration::from_secs(1);
        let clock = FixedClock::new(crate::clock::secs_since_epoch(pre_epoch));
        assert!(
            !token_set_expiring_at(1_700_000_000).is_fresh(&clock, EXPIRY_SKEW),
            "under a pre-epoch clock no token may be fresh (= always refresh)"
        );
    }

    #[test]
    fn advancing_the_clock_flips_the_same_token_from_fresh_to_stale() {
        // If no test moves the clock, a clock implementation that hardcodes `now_unix()` as a
        // constant passes every expiry test. The same single TokenSet is checked before and
        // after.
        let clock = FixedClock::new(1_000);
        let ts = token_set_expiring_at(1_100);
        assert!(ts.is_fresh(&clock, EXPIRY_SKEW), "1_100 > 1_000+60 → fresh");
        clock.advance(50);
        assert!(
            !ts.is_fresh(&clock, EXPIRY_SKEW),
            "with now=1_050, 1_100 > 1_110 is false, so the same token must flip to stale"
        );
    }

    #[test]
    fn accessors_do_not_swap_the_two_tokens() {
        // An implementation with the two accessors swapped passes every expiry, Debug and
        // serialization test.
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
        // REQ-18 single redaction layer: hardcoding the marker separately in this file would
        // split the source into two, which then drift apart. Changing `redaction::REDACTED`
        // is caught here.
        let rendered = format!("{:?}", token_set_expiring_at(1_000));
        assert!(
            rendered.contains(crate::redaction::REDACTED),
            "marker diverged from redaction::REDACTED: {rendered}"
        );
    }

    #[test]
    fn debug_does_not_leak_unknown_field_values() {
        // A Debug that hides only the two tokens and prints `extra` whole passes the tests
        // above — because the fixture has no unknown fields. A file written by a newer version
        // may contain a secret this version does not know (e.g. id_token).
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
        // When an older version reads and rewrites a file written by a newer fleetwire, no
        // field may be lost.
        let json =
            r#"{"access_token":"AT","refresh_token":"NA_RT","expires_at":1000,"future_field":42}"#;
        let ts: TokenSet = serde_json::from_str(json).expect("must deserialize");
        let back = serde_json::to_value(&ts).expect("must serialize");
        assert_eq!(back["future_field"], serde_json::json!(42));
    }

    #[test]
    fn round_trip_preserves_known_fields_without_duplicating_them() {
        // The test above, which looks only at the unknown field, does not catch an
        // implementation in which a known field disappears or is also absorbed into `extra` as
        // a duplicate.
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
            "a temporary file was left behind"
        );
    }

    #[test]
    fn store_leaves_no_temp_file_behind_when_the_rename_fails() {
        // The test above, which looks only at the success path, lets through an implementation
        // that leaks a `.tmp` on every failure. To make the rename fail for certain, a
        // **directory** is placed at the final path (EISDIR) — a point of failure that comes
        // after the temporary file has already been created.
        let dir = temp_dir("notemp-fail");
        let store = TokenStore::new(dir.to_path_buf());
        std::fs::create_dir(store.token_path()).expect("occupy the final path with a dir");

        let err = store
            .store(&token_set_expiring_at(1))
            .expect_err("the rename should fail");
        assert!(
            matches!(err, StoreError::Io(_)),
            "unexpected error: {err:?}"
        );
        assert_eq!(
            unexpected_entries(&dir),
            Vec::<String>::new(),
            "a temporary file was left behind on the failure path"
        );
    }

    #[test]
    fn load_refuses_a_symlinked_token_file() {
        let dir = temp_dir("symlink");
        let real = dir.join("elsewhere.json");
        // The bait is kept at 0600. At 0644 even an implementation that **follows** the link
        // would be caught by the permission check and return Err, so this test would be
        // testing the permission check rather than the symlink check (the weakness of the
        // original plan text).
        write_file_with_mode(&real, VALID_JSON, 0o600);
        let link = dir.join(TOKEN_FILE);
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        let store = TokenStore::new(dir.to_path_buf());
        assert!(
            matches!(store.load(), Err(StoreError::Symlink)),
            "reading through a symlink must be refused"
        );
    }

    #[test]
    fn load_reports_symlink_not_bad_permissions_when_the_path_is_both() {
        // Pinning the precedence: what is reported when the path is a symlink and the target's
        // permissions are also too permissive. **Symlink wins** — the link is not followed, so
        // there is no standing to look at the target's permissions at all, and the link's own
        // lstat mode is always 0777 on Linux, so reporting BadPermissions would hide the real
        // cause.
        let dir = temp_dir("symlink-badperm");
        let real = dir.join("elsewhere.json");
        write_file_with_mode(&real, VALID_JSON, 0o644);
        std::os::unix::fs::symlink(&real, dir.join(TOKEN_FILE)).expect("symlink");

        let err = TokenStore::new(dir.to_path_buf())
            .load()
            .expect_err("must fail");
        assert!(
            matches!(err, StoreError::Symlink),
            "the symlink must be reported before the permissions: {err:?}"
        );
    }

    #[test]
    fn load_rejects_group_or_world_readable_token_files() {
        // The original plan text had no test at all that checked BadPermissions — an
        // implementation with the permission check deleted wholesale passed everything.
        for mode in [0o644, 0o640, 0o604, 0o660] {
            let dir = temp_dir(&format!("badperm-{mode:o}"));
            write_file_with_mode(&dir.join(TOKEN_FILE), VALID_JSON, mode);
            let err = TokenStore::new(dir.to_path_buf())
                .load()
                .expect_err("must fail");
            assert!(
                matches!(err, StoreError::BadPermissions),
                "mode {mode:o} must be refused: {err:?}"
            );
        }
        // The other side: 0600 must pass (this rules out an implementation whose check
        // refuses unconditionally).
        let dir = temp_dir("badperm-ok");
        write_file_with_mode(&dir.join(TOKEN_FILE), VALID_JSON, 0o600);
        TokenStore::new(dir.to_path_buf())
            .load()
            .expect("0600 should be allowed");
    }

    #[test]
    fn load_reports_not_found_distinctly_from_io_error() {
        // `TempDir` is a guard — the binding has to be kept alive for the directory to exist
        // until the assertion. If it is discarded immediately, as in
        // `TokenStore::new(temp_dir(..))`, the first assertion, which meant to test "the
        // directory exists and only the file is missing", silently turns into "the directory
        // is missing too".
        let dir = temp_dir("missing");
        let store = TokenStore::new(dir.to_path_buf());
        assert!(matches!(store.load(), Err(StoreError::NotFound)));

        // When the directory itself is missing it is also NotFound, not Io (not logged in yet).
        let base = temp_dir("missing-dir");
        let nested = TokenStore::new(base.join("not").join("created"));
        assert!(matches!(nested.load(), Err(StoreError::NotFound)));
    }

    #[test]
    fn load_rejects_a_directory_at_the_token_path() {
        // If the `is_file()` check is deleted, opening a directory succeeds on Linux and
        // read_to_string fails with EISDIR, giving Io — this pins that difference.
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
        // It has to be 0600 to reach the parsing stage — a plain write gives 0644 because of
        // the umask.
        write_file_with_mode(&dir.join(TOKEN_FILE), "SECRET_NOT_JSON", 0o600);
        let err = TokenStore::new(dir.to_path_buf())
            .load()
            .expect_err("must fail");
        assert!(matches!(err, StoreError::Malformed), "{err:?}");
        // **Both** Display and Debug are checked: an implementation that hides only Display
        // and carries the serde message out through Debug passes a Display-only assertion.
        let display = format!("{err}");
        let debug = format!("{err:?}");
        assert!(
            !display.contains("SECRET_NOT_JSON"),
            "the file contents leaked into the error: {display}"
        );
        assert!(
            !debug.contains("SECRET_NOT_JSON"),
            "the file contents leaked through Debug: {debug}"
        );
    }

    #[test]
    fn load_reports_malformed_without_echoing_token_material() {
        // The fixture of the test above does not look like a token. This is a file that is
        // valid as JSON but does not fit the schema — the shape in which a serde message most
        // easily carries a value out.
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
                "the token value leaked into the error: {rendered}"
            );
        }
    }

    #[test]
    fn load_checks_permissions_before_parsing() {
        // Pinning the precedence #2: when corruption and overly permissive permissions overlap,
        // it is BadPermissions. An implementation that looks at the permissions **after**
        // reading and parsing the file has already loaded the contents into memory, so the
        // order of the checks is itself the requirement.
        let dir = temp_dir("perm-before-parse");
        write_file_with_mode(&dir.join(TOKEN_FILE), "SECRET_NOT_JSON", 0o644);
        let err = TokenStore::new(dir.to_path_buf())
            .load()
            .expect_err("must fail");
        assert!(
            matches!(err, StoreError::BadPermissions),
            "the permission check must come before parsing: {err:?}"
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
        // The test above ("the later-written value is visible") also lets through a
        // **non-atomic** implementation that truncates the final file and overwrites it in
        // place. The observable trace of atomicity is that "the previous inode is not touched"
        // — rename changes only the directory entry, so a hard link pointing at the old inode
        // still sees the old contents.
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
            "the old inode was overwritten — this is an in-place write, not a rename"
        );
    }

    #[test]
    fn store_refuses_a_symlinked_token_path_and_leaves_the_victim_untouched() {
        // rename does not follow a target symlink, so the victim file is not damaged, but the
        // caller has to know that "a link was encountered". Two things are pinned at once: the
        // error kind + the victim's contents being unchanged.
        let dir = temp_dir("store-symlink");
        let victim = dir.join("victim.json");
        write_file_with_mode(&victim, "VICTIM_CONTENT", 0o600);
        std::os::unix::fs::symlink(&victim, dir.join(TOKEN_FILE)).expect("symlink");

        let store = TokenStore::new(dir.to_path_buf());
        let err = store
            .store(&token_set_expiring_at(1))
            .expect_err("saving to a symlink target should be refused");
        assert!(matches!(err, StoreError::Symlink), "{err:?}");
        assert_eq!(
            std::fs::read_to_string(&victim).expect("read victim"),
            "VICTIM_CONTENT",
            "the token was written to the victim file through the symbolic link"
        );
    }

    #[test]
    fn store_refuses_a_symlink_planted_at_the_temp_path() {
        // The temporary path is an attack surface too. Clearing leftovers unconditionally with
        // `remove_file` would silently remove a link planted by an attacker and carry on —
        // here it is refused. That the victim file is unchanged is checked as well.
        let dir = temp_dir("store-tmp-symlink");
        let victim = dir.join("victim.json");
        write_file_with_mode(&victim, "VICTIM_CONTENT", 0o600);
        let store = TokenStore::new(dir.to_path_buf());
        std::os::unix::fs::symlink(&victim, store.temp_path()).expect("symlink");

        let err = store
            .store(&token_set_expiring_at(1))
            .expect_err("a symlink at the temporary path should be refused");
        assert!(matches!(err, StoreError::Symlink), "{err:?}");
        assert_eq!(
            std::fs::read_to_string(&victim).expect("read victim"),
            "VICTIM_CONTENT",
            "the token was written to the victim file by following the symbolic link at the \
             temporary path"
        );
    }

    #[test]
    fn store_clears_a_stale_regular_temp_file() {
        // A plain leftover that is not a link (a crash of a previous run) must not block saving
        // permanently — it would otherwise, because O_EXCL fails with EEXIST.
        let dir = temp_dir("store-stale-tmp");
        let store = TokenStore::new(dir.to_path_buf());
        write_file_with_mode(&store.temp_path(), "STALE", 0o600);

        store.store(&token_set_expiring_at(7)).expect("store");
        assert_eq!(unexpected_entries(&dir), Vec::<String>::new());
    }

    #[test]
    fn store_creates_a_missing_token_directory() {
        // The guard binding is kept alive — if it is discarded immediately the ancestor
        // directory disappears with it, and what `store` creates is then the whole tree rather
        // than `nested/deep`.
        let root = temp_dir("mkdir");
        let store = TokenStore::new(root.join("nested").join("deep"));
        store.store(&token_set_expiring_at(1)).expect("store");
        store.load().expect("load back");
    }

    #[test]
    fn store_creates_the_token_directory_owner_only() {
        // Design §4 specifies `~/.fleetwire/ 0700`. `create_dir_all` applies the umask to the
        // mode, so the result is 0755 under umask 022 and 0775 (group-writable) with umask 002
        // + a shared primary group — every symlink and TOCTOU defence in this file presumes
        // "nobody else can touch this directory", so that is a hole in itself.
        let root = temp_dir("mkdir-mode");
        // The ancestors are created along with it, to check that recursive applies the mode to
        // **every** new directory.
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
        // `DirBuilder::mode` applies only to **newly created** directories. A `~/.fleetwire`
        // that already existed as 0775 stays as it is, so that covers only half — the other
        // half is this refusal. The mode is set with `set_permissions`, so it does not depend
        // on the umask.
        for mode in [0o755, 0o775, 0o770, 0o707] {
            let dir = temp_dir(&format!("baddir-store-{mode:o}"));
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(mode)).expect("chmod");
            let err = TokenStore::new(dir.to_path_buf())
                .store(&token_set_expiring_at(1))
                .expect_err("tokens should not be written into an open directory");
            assert!(
                matches!(err, StoreError::BadDirPermissions),
                "mode {mode:o}: {err:?}"
            );
        }
    }

    #[test]
    fn load_refuses_a_group_or_world_writable_token_directory() {
        // This is the real attack: anyone who can write to the directory can delete tokens.json
        // and put **a plain 0600 file of their own** in its place. The symlink check, the
        // permission check and the ino/dev re-check then all pass (there is no uid check) — a
        // silent token swap. File-level checks cannot prevent this in principle, so the
        // premise is checked.
        let dir = temp_dir("baddir-load");
        write_file_with_mode(&dir.join(TOKEN_FILE), VALID_JSON, 0o600);
        // The file itself is flawless — in this state it is read.
        TokenStore::new(dir.to_path_buf())
            .load()
            .expect("a 0700 directory + a 0600 file should be readable");

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).expect("chmod");
        let err = TokenStore::new(dir.to_path_buf())
            .load()
            .expect_err("must fail");
        assert!(
            matches!(err, StoreError::BadDirPermissions),
            "even a perfect 0600 file must be refused when the directory is open: {err:?}"
        );
    }

    #[test]
    fn a_symlinked_token_directory_is_refused_rather_than_followed() {
        // final fix wave / triage 7. `metadata` follows links, so it passes as long as the
        // place the link points to is 0700 — that is, if someone else replaces `~/.fleetwire`
        // with a symlink to a directory of their own, they can bypass all of the tight symlink
        // defences on the file side and make the tokens be written into their directory. The
        // directory side also looks at the link itself with `symlink_metadata`.
        let base = temp_dir("symlinkdir");
        let real = base.join("real");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&real)
            .expect("real dir");
        write_file_with_mode(&real.join(TOKEN_FILE), VALID_JSON, 0o600);
        // Control: without going through the link it is read normally.
        TokenStore::new(real.clone())
            .load()
            .expect("the real 0700 directory should be readable");

        let link = base.join("link");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");

        let err = TokenStore::new(link.clone())
            .load()
            .expect_err("the symlinked directory was followed");
        assert!(matches!(err, StoreError::Symlink), "{err:?}");

        // The write side passes through the same guard (`prepare_dir` → `ensure_private_dir`).
        let err = TokenStore::new(link)
            .store(&token_set_expiring_at(1))
            .expect_err("the token was written into the symlinked directory");
        assert!(matches!(err, StoreError::Symlink), "{err:?}");
    }

    #[test]
    fn load_reports_bad_dir_permissions_before_not_found() {
        // Pinning the precedence: if the directory is open, that is reported before whether the
        // token file exists. The very fact that the file is missing may mean "someone just
        // deleted it".
        let dir = temp_dir("baddir-precedence");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).expect("chmod");
        let err = TokenStore::new(dir.to_path_buf())
            .load()
            .expect_err("must fail");
        assert!(matches!(err, StoreError::BadDirPermissions), "{err:?}");
    }

    #[test]
    fn store_writes_the_token_verbatim_without_recomputing_expiry() {
        // A hazard carried over from the A2 Task 3 review: under a pre-epoch clock
        // `now + expires_in` saturates to u64::MAX and leaves a token that "never expires".
        // The `store` of Task 4 **does no** time arithmetic — it writes what it received as is.
        // The preservation of unknown fields is pinned by the same assertions (they are not
        // lost on a round trip through the file).
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
            "store recomputed the expiry time"
        );
        assert_eq!(
            parsed["id_token"],
            serde_json::json!("KEEP_ME"),
            "the unknown field was lost on the round trip through the file"
        );
    }

    #[test]
    fn load_rejects_a_fifo_at_the_token_path_without_blocking_on_it() {
        // `load_rejects_a_directory_at_the_token_path` catches an implementation with the
        // `is_file()` check deleted only as "the error kind differs" (Io) — because a directory
        // can at least be opened. A FIFO is worse than that: opening a FIFO with no writer as
        // O_RDONLY makes `open(2)` **block forever**. That is the real reason the check has to
        // come before the open, and it is pinned not by the error kind but by "does it finish
        // at all".
        //
        // If mkfifo(1) is not available the test is silently skipped (no process-global state
        // is used).
        let dir = temp_dir("fifo");
        let path = dir.join(TOKEN_FILE);
        let made = std::process::Command::new("mkfifo")
            .arg(&path)
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !made {
            eprintln!("skipped: mkfifo(1) unavailable — cannot create the FIFO path");
            return;
        }
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");

        // It runs on a separate thread so that a hang becomes a failure rather than an endless
        // wait.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let outcome = TokenStore::new(dir.to_path_buf()).load();
            let _ = tx.send(matches!(outcome, Err(StoreError::Malformed)));
        });
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(is_malformed) => assert!(is_malformed, "a FIFO must be refused as Malformed"),
            Err(_) => panic!("load blocked opening the FIFO — check the file type before opening"),
        }
    }

    #[test]
    fn a_failed_store_leaves_the_previous_token_file_intact() {
        // What must not be lost when a save fails is not the new token but the **old token**.
        // An implementation that empties the final path first and then writes (truncate, then
        // write) wipes out even the perfectly good token in use on every failure and forces a
        // re-OAuth. The directory is made read-only so that the save fails at the stage of
        // creating the temporary file.
        let dir = temp_dir("failed-store-keeps-old");
        let store = TokenStore::new(dir.to_path_buf());
        store.store(&token_set_expiring_at(111)).expect("seed");

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).expect("chmod");
        // When run as root, even 0500 is writable and the failure cannot be staged — rather
        // than passing over that fact silently, the test is skipped explicitly.
        let denied = std::fs::File::create(dir.join(".write-probe")).is_err();
        if !denied {
            drop(std::fs::remove_file(dir.join(".write-probe")));
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
                .expect("restore");
            eprintln!("skipped: running as root — writes succeed even in a read-only directory");
            return;
        }

        let err = store
            .store(&token_set_expiring_at(222))
            .expect_err("saving into a read-only directory should not succeed");
        assert!(
            matches!(err, StoreError::Io(_)),
            "unexpected error: {err:?}"
        );

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("restore");
        let back = store
            .load()
            .expect("the old token should still be readable as it was");
        assert_eq!(back.access_token(), "AT");
        assert_eq!(
            serde_json::to_value(&back).expect("json")["expires_at"],
            serde_json::json!(111),
            "the save failed, yet the old token file was damaged"
        );
        assert_eq!(unexpected_entries(&dir), Vec::<String>::new());
    }

    #[test]
    fn lock_path_sits_beside_the_token_file_in_the_same_directory() {
        // The lock of Task 5 is keyed on **the inode, not the path**, so if the lock file
        // strays into a different directory from the tokens, cross-process mutual exclusion is
        // silently defeated.
        let dir = temp_dir("paths");
        let store = TokenStore::new(dir.to_path_buf());
        assert_eq!(store.token_path(), dir.join(TOKEN_FILE));
        assert_eq!(store.lock_path(), dir.join(LOCK_FILE));
        assert_eq!(store.lock_path().parent(), store.token_path().parent());
        assert_ne!(store.lock_path(), store.token_path());
    }
}

use super::*;
use std::cell::Cell;
use std::fs::TryLockError;
use std::os::unix::fs::PermissionsExt as _;
use std::time::{Duration, Instant};

/// 이 프로세스·스레드 전용 락 디렉터리 + 그 안의 락 경로를 함께 쥐는 가드.
///
/// 락 파일은 구현이 **절대 지우지 않는다**(REQ-15: inode 고정). 그래서 테스트가
/// 청소하지 않으면 그 파일과 디렉터리가 통째로 `/tmp` 에 남는다 — 이 가드가
/// 드롭될 때 디렉터리째 지운다.
///
/// `Deref<Target = Path>` 는 **락 파일 경로**를 준다 — `&path`·`path.exists()` 같은
/// 기존 호출부가 그대로 컴파일된다. 즉시 버리지 말 것
/// (`temp_lock_path("x").parent()` 같은 형태는 디렉터리를 그 자리에서 지운다).
struct TempLock {
    dir: std::path::PathBuf,
    file: std::path::PathBuf,
}

impl std::ops::Deref for TempLock {
    type Target = std::path::Path;
    fn deref(&self) -> &std::path::Path {
        &self.file
    }
}

impl AsRef<std::path::Path> for TempLock {
    fn as_ref(&self) -> &std::path::Path {
        &self.file
    }
}

impl Drop for TempLock {
    /// 패닉하지 않는다(언와인딩 중 이중 패닉 = SIGABRT). 권한을 먼저 되돌리는
    /// 것은 `store.rs` 픽스처와 같은 이유 — 0500 으로 남은 디렉터리에서
    /// `remove_dir_all` 이 조용히 실패하는 것을 막는다.
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700));
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// [`TempLock`] 하나를 만든다.
///
/// 예전 doc 은 "pid·thread id 는 **다음 실행**에서 재사용되므로 사전 삭제가
/// 이전 실행의 락 파일을 치운다" 고 적었으나 **틀렸다.** pid 는 `cargo test`
/// 실행마다 바뀌어 이름이 거의 매칭되지 않았고, 그래서 사전 삭제는 아무것도
/// 치우지 못한 채 디렉터리가 실행마다 누적됐다(부팅 76분 지연 사고 — 실측
/// 수치는 `crate::test_support::temp_dir` doc 한 곳에만 적는다).
/// "acquire 가 파일을 만든다" 단언을 실제로 지키는 것은 `Drop` 이다.
///
/// 반환값을 **이름으로 받을 것.** `temp_lock_path("x").parent()` 처럼 곧바로
/// 이어 쓰면 가드가 그 구문 끝에서 drop 되어 디렉터리가 사라지고, 뒤이어
/// `acquire` 가 같은 트리를 다시 만들어 아무도 지우지 않는 잔여물로 남는다
/// (`acquire_creates_a_missing_lock_directory_owner_only` 가 실제로 그랬다).
fn temp_lock_path(tag: &str) -> TempLock {
    let dir = std::env::temp_dir().join(format!(
        "fleetwire-lock-{tag}-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("temp dir");
    let file = dir.join("tokens.lock");
    TempLock { dir, file }
}

fn mode_of(path: &std::path::Path) -> u32 {
    std::fs::metadata(path)
        .expect("must exist")
        .permissions()
        .mode()
        & 0o777
}

fn inode_of(path: &std::path::Path) -> u64 {
    use std::os::unix::fs::MetadataExt as _;
    std::fs::metadata(path).expect("must exist").ino()
}

/// umask 와 무관하게 "소유자 외에는 아무 권한도 없다" 를 본다.
///
/// `assert_eq!(mode, 0o700)` 로 통째 비교하면 umask 0277 인 기계에서 **올바른**
/// 구현이 헛되이 깨진다 — umask 는 권한을 더 주지 못하므로 이 방향만 단언한다.
#[track_caller]
fn assert_owner_only(path: &std::path::Path, what: &str) {
    let mode = mode_of(path);
    assert_eq!(
        mode & 0o077,
        0,
        "{what}: 소유자 외 권한 비트가 남아 있다 (mode {mode:o})"
    );
}

#[test]
fn acquire_succeeds_on_a_fresh_path_and_creates_the_lock_file() {
    let path = temp_lock_path("fresh");
    assert!(!path.exists(), "픽스처가 락 파일을 미리 만들면 안 된다");
    let guard = acquire(&path, LOCK_DEADLINE).expect("must acquire");
    assert!(path.exists(), "락 파일이 생성돼야 한다");
    drop(guard);
    assert!(
        path.exists(),
        "락 해제 후에도 파일은 남아야 한다 (고정 inode)"
    );
}

#[test]
fn a_second_holder_times_out_while_the_first_holds_it() {
    // flock 은 open file description 단위라, 같은 프로세스에서 두 번 열면
    // 서로 배타된다 → 실제 자식 프로세스 없이 "형제 프로세스" 를 재현한다.
    let path = temp_lock_path("contended");
    let first = acquire(&path, LOCK_DEADLINE).expect("first must acquire");

    let started = Instant::now();
    let second = acquire(&path, Duration::from_millis(300));
    let elapsed = started.elapsed();

    assert!(
        matches!(second, Err(LockError::Timeout)),
        "두 번째는 타임아웃해야 한다"
    );
    assert!(
        elapsed >= Duration::from_millis(300),
        "deadline 이전에 포기했다: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "deadline 을 크게 초과했다: {elapsed:?}"
    );

    drop(first);
}

#[test]
fn the_lock_is_reacquirable_after_the_guard_drops() {
    let path = temp_lock_path("reacquire");
    drop(acquire(&path, LOCK_DEADLINE).expect("first"));
    // Drop 이 unlock 하지 않으면 여기서 타임아웃한다.
    let second = acquire(&path, Duration::from_millis(500));
    assert!(second.is_ok(), "Drop 이 락을 풀지 않았다");
}

#[test]
fn the_lock_file_keeps_its_inode_across_acquire_release_cycles() {
    // REQ-15 의 핵심: 락 파일을 지우면 안 된다. `path.exists()` 만 보는 단언은
    // "지우고 다시 만든다" 는 구현을 통과시키는데, 그 구현은 옛 inode 를 잡고
    // 있는 형제 프로세스와 새 inode 를 잡는 프로세스를 **동시에** 살려 둔다 —
    // 상호배제가 조용히 사라진다. 관측 가능한 흔적이 inode 다.
    let path = temp_lock_path("inode");
    let first = acquire(&path, LOCK_DEADLINE).expect("first");
    let ino = inode_of(&path);
    drop(first);
    assert_eq!(inode_of(&path), ino, "해제 경로에서 락 파일이 바뀌었다");

    let second = acquire(&path, LOCK_DEADLINE).expect("second");
    assert_eq!(inode_of(&path), ino, "재획득이 락 파일을 새로 만들었다");
    drop(second);
    assert_eq!(inode_of(&path), ino, "두 번째 해제에서 락 파일이 바뀌었다");
}

#[test]
fn a_timed_out_acquire_leaves_the_lock_file_in_place() {
    // 성공 경로만 보는 단언은 "실패하면 뒷정리한다" 는 구현을 통과시킨다.
    // 타임아웃한 쪽이 락 파일을 치우면 **락을 쥐고 있는 쪽의 inode** 가
    // 디렉터리에서 사라져, 다음 프로세스가 새 inode 를 만들어 동시에 들어온다.
    let path = temp_lock_path("timeout-keeps");
    let first = acquire(&path, LOCK_DEADLINE).expect("first");
    let ino = inode_of(&path);

    let second = acquire(&path, Duration::from_millis(50));
    assert!(
        matches!(second, Err(LockError::Timeout)),
        "타임아웃해야 한다"
    );
    assert!(path.exists(), "타임아웃이 락 파일을 지웠다");
    assert_eq!(inode_of(&path), ino, "타임아웃이 락 파일을 갈아치웠다");

    drop(first);
}

#[test]
fn the_lock_is_released_when_the_guard_drops_during_a_panic() {
    // 정상 drop 만 보는 테스트는 언와인딩 경로를 못 본다. 이 크레이트가
    // `panic = "abort"` 로 바뀌거나 해제를 명시적 `release()` 로 옮기면
    // 패닉한 프로세스가 락을 그대로 쥔 채 남아 형제가 deadline 까지 막힌다.
    let path = temp_lock_path("panic-unwind");
    let guard = acquire(&path, LOCK_DEADLINE).expect("first");

    // 패닉 훅은 프로세스 전역이라 건드리지 않는다(테스트는 병렬 실행된다).
    // 표준 훅이 stderr 에 남기는 메시지는 의도된 소음이다.
    let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _held = guard;
        panic!("intentional panic: lock must still be released while unwinding");
    }));
    assert!(caught.is_err(), "패닉이 잡혀야 한다");

    let second = acquire(&path, Duration::from_millis(500));
    assert!(second.is_ok(), "언와인딩 중 Drop 이 락을 풀지 않았다");
    assert!(path.exists(), "언와인딩 경로에서 락 파일이 사라졌다");
}

#[test]
fn a_zero_deadline_still_attempts_the_lock_once() {
    // deadline 을 **먼저** 보고 자는 구현(`if elapsed >= deadline` 을 루프
    // 머리에 둔 형태)은 비어 있는 락에서도 즉시 Timeout 을 낸다. 0 은 "시도
    // 하지 말라" 가 아니라 "재시도하지 말라" 다.
    let path = temp_lock_path("zero-free");
    let guard = acquire(&path, Duration::ZERO);
    assert!(
        guard.is_ok(),
        "비어 있는 락은 deadline 0 에서도 잡혀야 한다: {:?}",
        guard.err()
    );
}

#[test]
fn a_zero_deadline_makes_exactly_one_attempt_before_timing_out() {
    // 위 테스트의 반대편. 경합 중이면 정확히 한 번만 시도하고 포기한다 —
    // 0 을 무시하고 최소 한 번은 자는 구현을 배제한다.
    let attempts = Cell::new(0_u32);
    let started = Instant::now();
    let outcome = retry_until_locked(Duration::ZERO, || {
        attempts.set(attempts.get() + 1);
        Err(TryLockError::WouldBlock)
    });

    assert!(matches!(outcome, Err(LockError::Timeout)), "{outcome:?}");
    assert_eq!(attempts.get(), 1, "deadline 0 인데 재시도했다");
    assert!(
        started.elapsed() < Duration::from_millis(500),
        "deadline 0 인데 잤다: {:?}",
        started.elapsed()
    );
}

#[test]
fn a_real_lock_error_aborts_immediately_instead_of_retrying() {
    // `TryLockError::Error` 를 `WouldBlock` 과 뭉개면 디스크 오류에서
    // deadline 까지 무의미하게 재시도한다(REQ-15 의 유한성은 지키지만 10초를
    // 버린다). 시도 횟수로 그 구분을 못박는다.
    let attempts = Cell::new(0_u32);
    let started = Instant::now();
    let outcome = retry_until_locked(Duration::from_secs(10), || {
        attempts.set(attempts.get() + 1);
        Err(TryLockError::Error(std::io::Error::other(
            "simulated disk failure",
        )))
    });

    assert!(matches!(outcome, Err(LockError::Io(_))), "{outcome:?}");
    assert_eq!(attempts.get(), 1, "실제 입출력 오류에서 재시도했다");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "실제 오류인데 deadline 까지 기다렸다: {:?}",
        started.elapsed()
    );
}

#[test]
fn contention_is_retried_rather_than_treated_as_failure() {
    // 반대편: `WouldBlock` 을 즉시 실패로 바꾸는 구현을 배제한다.
    let attempts = Cell::new(0_u32);
    let outcome = retry_until_locked(Duration::from_secs(10), || {
        attempts.set(attempts.get() + 1);
        if attempts.get() < 4 {
            Err(TryLockError::WouldBlock)
        } else {
            Ok(())
        }
    });

    assert!(outcome.is_ok(), "경합은 재시도해야 한다: {outcome:?}");
    assert_eq!(attempts.get(), 4);
}

#[test]
fn the_retry_interval_backs_off_instead_of_spinning_at_the_initial_value() {
    // 백오프 증가를 지운 구현(항상 5ms)은 타이밍 단언을 전부 통과한다 —
    // 눈에 보이는 차이는 **시도 횟수**뿐이다. 500ms 경합에서 지수 백오프는
    // 5·10·20·40·80·160·200 → 8회 근방, 고정 5ms 는 100회 근방이다.
    // 두 상수가 뒤바뀐 구현(초기 200ms/상한 5ms)도 62회 근방으로 여기서 걸린다.
    //
    // 상한만 단언한다: 부하가 걸리면 각 sleep 이 **길어져** 시도가 줄어들 뿐
    // 늘지 않으므로, 이 방향은 느린 기계에서 헛되이 깨지지 않는다.
    let attempts = Cell::new(0_u32);
    let outcome = retry_until_locked(Duration::from_millis(500), || {
        attempts.set(attempts.get() + 1);
        Err(TryLockError::WouldBlock)
    });

    assert!(matches!(outcome, Err(LockError::Timeout)), "{outcome:?}");
    assert!(
        attempts.get() <= 25,
        "백오프가 자라지 않아 500ms 동안 {}회나 깨어났다",
        attempts.get()
    );
    // 반대편: 아예 재시도하지 않는 구현을 배제한다.
    assert!(
        attempts.get() >= 3,
        "재시도가 거의 없었다: {}",
        attempts.get()
    );
}

#[test]
fn a_freed_lock_is_picked_up_long_before_the_deadline() {
    // 재시도 간격이 실제로 짧은지 본다. 첫 간격을 deadline 만큼 크게 잡은
    // 구현은 위의 어떤 테스트도 깨지 않지만(타임아웃 테스트는 여전히 통과),
    // 형제가 락을 놓은 뒤에도 10초를 자게 만든다.
    let path = temp_lock_path("prompt");
    let first = acquire(&path, LOCK_DEADLINE).expect("first");
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(30));
        drop(first);
    });

    let started = Instant::now();
    acquire(&path, Duration::from_secs(5)).expect("풀린 락은 잡혀야 한다");
    let elapsed = started.elapsed();

    // 기대값은 ~40ms 다. 부하 걸린 기계를 위해 한참 넉넉한 상한만 단언한다.
    assert!(
        elapsed < Duration::from_millis(1_500),
        "락이 풀렸는데도 오래 잤다: {elapsed:?}"
    );
    releaser.join().expect("releaser");
}

#[test]
fn a_lock_held_for_a_while_is_still_picked_up_promptly_after_release() {
    // 상한(`LOCK_RETRY_MAX`)을 지운 구현 — `backoff *= 2` — 은 위의 어떤
    // 테스트도 깨지 않는다. 30ms 만에 풀리는 락은 백오프가 자라기 전에 잡히고,
    // 타임아웃 테스트는 `min(remaining)` 클램프 덕에 여전히 통과하며, 시도
    // 횟수는 **줄어들 뿐**이라 상한 단언에도 걸리지 않는다.
    //
    // 유일하게 보이는 차이는 "형제가 오래 쥐고 있다가 놓았을 때의 반응 지연"
    // 이다. 상한이 있으면 sleep 경계가 …515·715·915·1115·1315·1515ms 라
    // 1400ms 해제를 ~1515ms 에 잡지만, 상한이 없으면 635ms 에서 시작한
    // 2560ms 짜리 sleep 한복판이라 ~3835ms 까지 못 잡는다. 그 사이를 가른다.
    let path = temp_lock_path("cap");
    let first = acquire(&path, LOCK_DEADLINE).expect("first");
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(1_400));
        drop(first);
    });

    let started = Instant::now();
    acquire(&path, LOCK_DEADLINE).expect("풀린 락은 잡혀야 한다");
    let elapsed = started.elapsed();

    assert!(
        elapsed >= Duration::from_millis(1_400),
        "형제가 쥐고 있는 동안 잡혔다: {elapsed:?}"
    );
    // 정상 ~1.5초 / 상한 없는 구현 ~3.8초. 양쪽에 ~1초씩 여유를 두고 가른다.
    assert!(
        elapsed < Duration::from_millis(2_500),
        "백오프 상한이 없어 해제 후에도 오래 잤다: {elapsed:?}"
    );
    releaser.join().expect("releaser");
}

#[test]
fn acquiring_does_not_modify_the_lock_file_contents() {
    // 락 파일은 "지우지 않는다" 와 같은 이유로 **내용도 건드리지 않는다** —
    // 열 때 `truncate(true)` 를 쓰면 형제가 남긴 것을 조용히 날린다. 지금은
    // 아무것도 쓰지 않지만, 진단용 pid 같은 걸 넣는 순간 조용한 손실이 된다.
    let path = temp_lock_path("no-truncate");
    std::fs::write(&path, b"KEEP_ME").expect("seed the lock file");

    let guard = acquire(&path, LOCK_DEADLINE).expect("must acquire");
    assert_eq!(
        std::fs::read(&path).expect("read"),
        b"KEEP_ME",
        "acquire 가 락 파일 내용을 잘랐다"
    );
    drop(guard);
    assert_eq!(
        std::fs::read(&path).expect("read"),
        b"KEEP_ME",
        "해제가 락 파일 내용을 잘랐다"
    );
}

#[test]
fn the_retry_interval_grows_from_the_initial_toward_the_cap() {
    // 두 상수가 뒤바뀐 구현(초기 200ms / 상한 5ms)은 `min` 때문에 첫 간격 뒤로
    // 영원히 5ms 를 자며, 타이밍 테스트로는 잡히지 않는다.
    assert!(
        LOCK_RETRY_INITIAL < LOCK_RETRY_MAX,
        "초기 간격이 상한보다 크다"
    );
    assert!(LOCK_RETRY_MAX <= LOCK_DEADLINE, "상한이 deadline 보다 크다");
}

#[test]
fn an_unopenable_lock_path_is_reported_as_io_without_waiting() {
    // 락 경로에 디렉터리가 있으면 open 이 EISDIR 로 실패한다. 이건 경합이
    // 아니므로 deadline 까지 기다리면 안 된다.
    let path = temp_lock_path("eisdir");
    std::fs::create_dir(&path).expect("occupy the lock path with a dir");

    let started = Instant::now();
    let outcome = acquire(&path, LOCK_DEADLINE);
    assert!(matches!(outcome, Err(LockError::Io(_))), "{outcome:?}");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "열 수 없는 경로인데 deadline 까지 기다렸다: {:?}",
        started.elapsed()
    );
}

#[test]
fn acquire_creates_a_missing_lock_directory_owner_only() {
    // `create_dir_all` 은 mode 에 umask 를 적용해 umask 022 에서 0755 를
    // 만든다. 락 파일은 토큰과 **같은 디렉터리**이므로, 락을 먼저 잡는 경로가
    // 0755 로 디렉터리를 만들어 버리면 그 뒤의 모든 `TokenStore::store` 가
    // `BadDirPermissions` 로 영구히 실패한다(설계 §4 = 0700).
    // 가드를 **이름으로 붙잡는다.** `temp_lock_path("mkdir-mode").parent()` 처럼
    // 곧바로 이어 쓰면 `TempLock` 이 그 구문 끝에서 drop 되어 디렉터리가 사라지고,
    // 뒤이어 `acquire` 가 같은 트리를 다시 만들어 **아무도 지우지 않는 잔여물**로
    // 남는다(실측: 스위트 1회당 정확히 1개 누수 — 이 테스트 하나였다).
    let temp = temp_lock_path("mkdir-mode");
    let root = temp.parent().expect("parent").to_path_buf();
    let deep = root.join("nested").join("deep");
    let guard = acquire(&deep.join("tokens.lock"), LOCK_DEADLINE).expect("must acquire");

    assert_owner_only(&root.join("nested"), "lock dir (ancestor)");
    assert_owner_only(&deep, "lock dir");
    drop(guard);
}

#[test]
fn the_lock_file_is_created_owner_only() {
    let path = temp_lock_path("filemode");
    let guard = acquire(&path, LOCK_DEADLINE).expect("must acquire");
    assert_owner_only(&path, "lock file");
    drop(guard);
}

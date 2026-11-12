use super::*;
use crate::clock::FixedClock;
use crate::test_support::{FAR_FUTURE, TempDir, seed_expired_token, temp_dir, write_token};
use std::os::unix::fs::PermissionsExt as _;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};

/// 저장소가 실제로 쓰는 경로를 그대로 쓴다 — 테스트가 파일 이름을 다시 적으면
/// 출처가 둘로 갈라진다.
fn token_path(dir: &Path) -> PathBuf {
    TokenStore::new(dir.to_path_buf()).token_path()
}

fn lock_path(dir: &Path) -> PathBuf {
    TokenStore::new(dir.to_path_buf()).lock_path()
}

fn disk_json(dir: &Path) -> serde_json::Value {
    let raw = std::fs::read_to_string(token_path(dir)).expect("read token file");
    serde_json::from_str(&raw).expect("token file must be json")
}

/// umask 와 무관하게 "소유자 외에는 아무 권한도 없다" 를 본다.
#[track_caller]
fn assert_owner_only(path: &Path, what: &str) {
    let mode = std::fs::metadata(path)
        .expect("must exist")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(
        mode & 0o077,
        0,
        "{what}: 소유자 외 권한 비트가 남아 있다 (mode {mode:o})"
    );
}

/// 이 디렉터리에 파일을 만들 수 **없는가**.
///
/// root 로 테스트를 돌리면 0500 디렉터리에도 쓸 수 있어 "저장 실패" 를 연출할
/// 수 없다. 프로세스 전역 상태를 건드리지 않고 그 사실만 확인한다.
fn writes_are_denied(dir: &Path) -> bool {
    let probe = dir.join(".write-probe");
    match std::fs::File::create(&probe) {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            false
        }
        Err(_) => true,
    }
}

/// `cond` 가 참이 될 때까지 짧게 자며 기다린다(상한을 두고).
///
/// 경합 테스트에서 "승자가 이미 임계구역 안에 있다" 를 **시각이 아니라 사실**로
/// 확인하려고 쓴다. `sleep(n)` 으로 순서를 짐작하면 부하 걸린 기계에서 순서가
/// 뒤집히고, 그러면 시험하려던 경합이 아예 일어나지 않은 채 테스트가 통과한다.
/// 상한은 실패를 무한 hang 이 아니라 진단 가능한 패닉으로 만들기 위한 것이므로
/// 넉넉하게 잡는다.
fn wait_until(what: &str, mut cond: impl FnMut() -> bool) {
    let started = std::time::Instant::now();
    while !cond() {
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "조건이 성립하지 않았다: {what}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
}

/// 호출 횟수를 세는 refresher. single-flight 의 핵심 단언 대상이다.
struct CountingRefresher {
    calls: AtomicUsize,
    new_access: String,
    /// 왕복 지연. 임계구역을 얼마나 오래 붙잡을지가 곧 경합 창의 크기다.
    delay: Duration,
}

impl CountingRefresher {
    fn new(new_access: &str) -> Self {
        Self::with_delay(new_access, Duration::from_millis(50))
    }
    /// 임계구역을 오래 붙잡는 변형 — 뮤텍스/파일 락 대기를 시험할 때 쓴다.
    fn with_delay(new_access: &str, delay: Duration) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            new_access: new_access.to_owned(),
            delay,
        }
    }
    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl TokenRefresher for CountingRefresher {
    fn refresh(&self, _refresh_token: &str) -> Result<Refreshed, Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        // 실제 네트워크 지연을 흉내내 동시 진입을 벌린다.
        std::thread::sleep(self.delay);
        Ok(Refreshed::new(
            self.new_access.clone(),
            "NA_ROTATED".to_owned(),
            28_800,
        ))
    }
}

/// 항상 실패하는 refresher. 실패가 조용히 삼켜지지 않는지 본다.
///
/// `CountingRefresher` 와 같은 방식으로 호출 횟수를 센다 — "실패한 뒤에도
/// 다시 시도할 수 있는가" 가 이 타입으로 보려는 성질의 절반이기 때문이다.
struct FailingRefresher {
    calls: AtomicUsize,
}

impl FailingRefresher {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }
    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl TokenRefresher for FailingRefresher {
    fn refresh(&self, _refresh_token: &str) -> Result<Refreshed, Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(Error::Auth {
            message: "upstream refused".to_owned(),
            pricing: None,
        })
    }
}

/// 서버가 시킨 대로 만료 시각을 정하는 refresher — 수명 값을 시험한다.
struct LifetimeRefresher {
    calls: AtomicUsize,
    expires_in_secs: u64,
}

impl LifetimeRefresher {
    fn new(expires_in_secs: u64) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            expires_in_secs,
        }
    }
    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl TokenRefresher for LifetimeRefresher {
    fn refresh(&self, _refresh_token: &str) -> Result<Refreshed, Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(Refreshed {
            access_token: "NEW".to_owned(),
            refresh_token: "NA_ROTATED".to_owned(),
            expires_in_secs: self.expires_in_secs,
        })
    }
}

// --- 세대 + 회전 최소 간격 (A3 Task 5) 픽스처 -------------------------

/// 이 그룹의 테스트가 쓰는 기준 시각. 값 자체에 의미는 없다 — epoch 근처를
/// 피해 `checked_sub` 가 **역행 때문에만** `None` 이 되게 하려는 것뿐이다.
const T0: u64 = 1_700_000_000;

/// 세대·간격 테스트용 관리자.
///
/// A2 픽스처를 그대로 조립한다: `temp_dir`(0700) + `seed_expired_token`(0600)
/// 과 `LifetimeRefresher`(호출 카운터 내장) 와 `FixedClock`. 새로 만든 것은
/// 없다 — umask 함정은 A2 가 이미 밟았다.
///
/// `expires_in` 은 refresher 가 보고할 수명이다. 0 을 주면 A2 가 문서화한
/// "도착 즉시 stale" 상태가 되어 **매 호출이 회전하려 든다** — 회전 간격
/// 게이트를 시험하기에 정확히 맞는 입력이다.
fn interval_manager(
    tag: &str,
    expires_in: u64,
) -> (
    TokenManager,
    Arc<LifetimeRefresher>,
    Arc<FixedClock>,
    TempDir,
) {
    let dir = temp_dir(tag);
    seed_expired_token(&dir);
    let refresher = Arc::new(LifetimeRefresher::new(expires_in));
    let clock = Arc::new(FixedClock::new(T0));
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>)
        .with_clock(Arc::clone(&clock) as Arc<_>)
        // 락 경합을 시험하는 그룹이 아니다. 어떤 이유로든 락이 남아 있으면
        // 10초를 기다리는 대신 빨리 실패해서 오진을 드러낸다.
        .with_lock_deadline(Duration::from_secs(3));
    (mgr, refresher, clock, dir)
}

/// 회전 간격을 넘길 만큼은 살고, 그 뒤 한 번 더 회전해야 할 만큼은 짧은 수명.
///
/// `MIN_REFRESH_INTERVAL`(60초) 를 넘겨야 게이트가 열리고, `advance` 뒤에
/// 디스크 토큰이 stale 이어야 회전 경로에 닿는다. 100초가 그 둘을 동시에
/// 만족한다: `T0+100` 은 `T0` 에서 fresh 지만 `T0+61`(+skew 60)에서는 stale 이다.
const SHORT_BUT_USABLE: u64 = 100;

/// 느린 왕복 + **도착 즉시 stale** 인 토큰을 내는 refresher.
///
/// A2 에는 이 조합이 없다: `CountingRefresher` 는 느리지만 수명이 정상(28800초)
/// 이라 두 번째 호출이 캐시에 적중해 게이트에 닿지 않고, `LifetimeRefresher` 는
/// 수명은 고를 수 있으나 즉시 반환이라 임계구역을 붙잡지 못해 경합을 만들 수
/// 없다. 아래 `..._not_a_file_lock_timeout` 이 둘 **다** 필요로 한다.
struct SlowStaleRefresher {
    calls: AtomicUsize,
    delay: Duration,
}

impl SlowStaleRefresher {
    fn new(delay: Duration) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            delay,
        }
    }
    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl TokenRefresher for SlowStaleRefresher {
    fn refresh(&self, _refresh_token: &str) -> Result<Refreshed, Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(self.delay);
        // 수명 0 — 기록되는 순간 이미 stale 이라 다음 호출이 회전하려 든다.
        Ok(Refreshed::new("NEW".to_owned(), "NA_ROTATED".to_owned(), 0))
    }
}

// --- 세대 + 회전 최소 간격 (A3 Task 5) --------------------------------

#[test]
fn the_min_refresh_interval_is_the_expiry_skew_and_is_sixty_seconds() {
    // 설계가 닫히는 문장은 "신선하다고 볼 수 없는 수명은 회전도 할 수 없다" 다.
    // 두 상수가 갈라지면 그 문장이 거짓이 된다 — 구조적으로 묶는 대신(공개
    // 상수가 크레이트 내부 상수를 따라 조용히 움직이는 건 더 나쁘다) 여기서
    // 등가를 못박아, 어느 쪽을 움직이든 의식적인 결정이 되게 한다.
    assert_eq!(MIN_REFRESH_INTERVAL, EXPIRY_SKEW);
    // 등가만 보면 둘 다 0 으로 바꿔도 통과한다. 값 자체도 못박는다.
    assert_eq!(MIN_REFRESH_INTERVAL, Duration::from_secs(60));
}

#[test]
fn refresh_within_min_interval_errors_and_sends_nothing() {
    // A2 가 A3 에 넘긴 구멍(`MAX_TOKEN_LIFETIME` doc 의 "반대편에는 하한이 없다"):
    // 서버가 `expires_in = 0` 을 주면 새 토큰이 도착 즉시 stale 이라 **매 호출이**
    // 체인을 회전시킨다. 그 폭주를 여기서 막는다.
    let (mgr, refresher, _clock, _dir) = interval_manager("gen-interval", 0);

    assert_eq!(mgr.access_token().expect("first"), "NEW");
    let after_first = refresher.call_count();
    assert_eq!(after_first, 1, "1회차는 정상적으로 회전해야 한다");

    let err = mgr
        .access_token()
        .expect_err("간격 안에서는 캐시가 아니라 에러여야 한다");

    // ⚠️ "에러가 났다" 만으로는 부족하다 — POST 를 보내 놓고 에러를 낸 구현도
    //    같은 모양이다. 기구를 센다.
    assert_eq!(
        refresher.call_count(),
        after_first,
        "간격 안에서 refresh POST 를 한 번 더 태웠다"
    );
    let msg = err.to_string();
    assert!(msg.contains("refresh"), "원인을 지목해야 한다: {msg}");
    assert!(
        msg.contains("rotation interval"),
        "다른 이유(락·저장 등)로 실패한 것을 간격으로 오해하고 있다: {msg}"
    );
}

#[test]
fn min_interval_error_is_not_billed() {
    // 어떤 엔드포인트에도 닿지 않았다. `pricing: Some(..)` 로 만들면
    // `billing()` 이 이 실패를 지출로 세고, 요금 회계가 조용히 부풀려진다.
    let (mgr, _refresher, _clock, _dir) = interval_manager("gen-notbilled", 0);
    let _first = mgr.access_token().expect("first");
    let err = mgr.access_token().expect_err("간격에 막혀야 한다");

    assert!(
        matches!(&err, Error::Auth { pricing: None, .. }),
        "실제: {err:?}"
    );
    assert_eq!(err.billing(), crate::pricing::Billing::NotBilled);
}

#[test]
fn clock_going_backwards_counts_as_interval_not_met() {
    // 시계 역행에서 `now - last` 를 뺄셈으로 쓰면 debug 에서 패닉하고 release
    // 에서는 거대한 값으로 감싸 돌아 **간격을 무력화**한다. `checked_sub` 의
    // `None` 을 "간격 미충족" 으로 접는 것이 보수적 방향이다.
    let (mgr, refresher, clock, _dir) = interval_manager("gen-backwards", 0);
    assert_eq!(mgr.access_token().expect("first"), "NEW");
    let after_first = refresher.call_count();

    // ⚠️ 역행 폭을 30초로 잡는 이유(vacuous pass 방지). 디스크 토큰의 만료는
    //    `T0 + 0` 이고 신선도 판정은 `expires_at > now + 60` 이다. 1시간을
    //    되감으면 `T0 > T0 - 3600 + 60` 이 **참**이 되어 그 토큰이 도로
    //    "신선" 해지고, 호출은 게이트에 닿기도 전에 디스크 채택으로 성공한다 —
    //    회전이 없으니 카운터 단언은 통과하지만 시험하려던 코드는 한 줄도
    //    실행되지 않는다. 30초(< skew 60)면 디스크는 여전히 stale 이라 게이트가
    //    유일하게 남은 방어선이 된다.
    clock.set(T0 - 30);

    let err = mgr
        .access_token()
        .expect_err("시계 역행은 간격 미충족으로 봐야 한다");
    assert!(
        err.to_string().contains("rotation interval"),
        "역행이 게이트가 아닌 다른 경로에서 걸렸다(테스트가 헛돈다): {err}"
    );
    assert_eq!(
        refresher.call_count(),
        after_first,
        "시계 역행이 회전 억제를 무력화했다"
    );
}

#[test]
fn the_minimum_interval_boundary_admits_exactly_sixty_seconds() {
    // 경계 변이(`<` → `<=`)는 위 테스트들을 전부 통과한다 — 59초에서도 60초에서도
    // 막기 때문이다. 경계의 **양쪽**을 같은 테스트에서 본다.
    let (mgr, refresher, clock, _dir) = interval_manager("gen-boundary", SHORT_BUT_USABLE);
    let (g1, _tok) = mgr.access_token_with_generation().expect("first");
    assert_eq!(refresher.call_count(), 1);

    // 캐시를 비워 회전 경로로 보낸다(비우지 않으면 캐시 적중으로 게이트에 닿지 않는다).
    mgr.invalidate_generation(g1).expect("invalidate");

    // 59초 — 아직 막힌다.
    clock.advance(MIN_REFRESH_INTERVAL.as_secs() - 1);
    let err = mgr.access_token().expect_err("59초는 막혀야 한다");
    assert!(err.to_string().contains("rotation interval"), "{err}");
    assert_eq!(refresher.call_count(), 1, "59초에 회전했다");

    // 정확히 60초 — 열린다. `<` 가 `<=` 로 바뀌면 여기서 죽는다.
    clock.advance(1);
    assert_eq!(
        mgr.access_token().expect("정확히 60초는 허용돼야 한다"),
        "NEW"
    );
    assert_eq!(
        refresher.call_count(),
        2,
        "간격을 채웠는데 회전하지 못했다 — 게이트가 경계를 한 칸 넘게 잡았다"
    );
}

#[test]
fn generation_increases_on_each_refresh() {
    let (mgr, refresher, clock, _dir) = interval_manager("gen-increases", SHORT_BUT_USABLE);
    let (g1, _t1) = mgr.access_token_with_generation().expect("first");
    assert_eq!(refresher.call_count(), 1);

    mgr.invalidate_generation(g1).expect("invalidate");
    clock.advance(MIN_REFRESH_INTERVAL.as_secs() + 1);

    let (g2, _t2) = mgr.access_token_with_generation().expect("second");
    // 기구를 함께 본다 — 세대만 보면 "채택으로 올랐다" 와 구분되지 않는다.
    assert_eq!(
        refresher.call_count(),
        2,
        "두 번째가 회전이 아니라 다른 경로였다면 이 테스트는 이름값을 못 한다"
    );
    assert!(g2 > g1, "g1={g1} g2={g2}");
}

#[test]
fn generation_increases_when_a_siblings_rotation_is_adopted() {
    // 채택도 캐시를 **교체**한다. 여기서 세대를 올리지 않으면, 채택 직후 뒤늦게
    // 도착한 401 이 세대 일치로 방금 채택한 멀쩡한 토큰을 버린다 — 세대가
    // 막으려던 사고를 채택 경로에만 남겨 두는 셈이다.
    let (mgr, refresher, _clock, dir) = interval_manager("gen-adopt", SHORT_BUT_USABLE);
    let (g1, t1) = mgr.access_token_with_generation().expect("first");
    assert_eq!(refresher.call_count(), 1);

    mgr.invalidate_generation(g1).expect("invalidate");
    // 형제 프로세스가 회전시켜 둔 모양을 만든다 — 우리 refresher 는 쓰이지 않는다.
    write_token(&dir, "FROM_SIBLING", "NA_SIBLING", FAR_FUTURE);

    let (g2, t2) = mgr.access_token_with_generation().expect("adopt");
    assert_eq!(t2, "FROM_SIBLING", "형제의 토큰을 채택하지 않았다");
    assert_ne!(t2, t1);
    assert_eq!(
        refresher.call_count(),
        1,
        "채택 경로인데 회전 POST 를 태웠다"
    );
    assert!(
        g2 > g1,
        "캐시가 교체됐는데 세대가 그대로다 — 낡은 401 이 이 토큰을 죽일 수 있다 \
         (g1={g1} g2={g2})"
    );
}

#[test]
fn stale_generation_cannot_invalidate_newer_token() {
    // ⚠️ 이 비교가 없으면 동시 401 이 방금 갱신된 토큰을 죽인다: A 가 세대 N 으로
    //    보낸 요청의 401 이, B 가 이미 만든 N+1 을 버리게 한다.
    let (mgr, refresher, clock, _dir) = interval_manager("gen-stale", SHORT_BUT_USABLE);
    let (g1, _t1) = mgr.access_token_with_generation().expect("first");
    mgr.invalidate_generation(g1).expect("invalidate g1");
    clock.advance(MIN_REFRESH_INTERVAL.as_secs() + 1);

    let (g2, t2) = mgr.access_token_with_generation().expect("second");
    let calls_after_g2 = refresher.call_count();
    let locks_after_g2 = mgr.lock_attempts();
    assert_eq!(calls_after_g2, 2);

    // 뒤늦게 도착한 401 이 **낡은** 세대로 무효화를 시도한다.
    mgr.invalidate_generation(g1)
        .expect("낡은 세대의 무효화도 오류가 아니라 no-op 이다");

    let (g3, t3) = mgr.access_token_with_generation().expect("third");
    assert_eq!(g3, g2, "낡은 세대의 무효화가 먹혔다");
    assert_eq!(t3, t2, "멀쩡한 토큰이 버려졌다");
    assert_eq!(
        refresher.call_count(),
        calls_after_g2,
        "무효화가 먹혀서 회전이 한 번 더 일어났다"
    );
    // 세대 단언만으로는 부족한 변이가 있다(무조건 무효화 + 채택 경로). 기구를
    // 함께 본다 — 캐시가 살아 있었다면 파일 락에 갈 이유가 없다.
    assert_eq!(
        mgr.lock_attempts(),
        locks_after_g2,
        "캐시가 비워져 세 번째 호출이 파일 락까지 갔다"
    );
}

#[test]
fn a_blocked_second_caller_gets_the_interval_error_not_a_file_lock_timeout() {
    // ⚠️ **이 테스트가 무엇을 위해 있는가** (리뷰 fix round).
    //
    // "세대를 락을 놓은 뒤에 읽는" 구현은 borrow checker 가 막아 준다고 적었는데,
    // 그건 **순진한 형태**(놓은 가드를 다시 쓰는 것, E0382)에만 참이다. 다시 잡는
    // 형태는 멀쩡히 컴파일된다:
    //
    // ```ignore
    // drop(state);
    // let g = self.state.lock().generation;
    // Ok((g, token))
    // ```
    //
    // 이 형태는 세대를 임계구역 **밖에서** 읽을 뿐 아니라, tokio 뮤텍스를
    // **flock 보다 먼저** 놓아 락 순서를 뒤집는다. 그 역전이 관측 가능한 지점이
    // 여기다:
    //
    //   - 정상 구현: 승자가 뮤텍스와 flock 을 **함께** 놓는다. 패자는 뮤텍스를
    //     얻은 뒤 flock 도 곧바로 얻고, 디스크가 stale 이므로 8번 게이트에
    //     막혀 `rotation interval` 로 **즉시** 실패한다.
    //   - 재-lock 변이: 승자가 flock 을 쥔 채 뮤텍스만 놓는다. 패자가 뮤텍스를
    //     얻어 flock 으로 가고, 승자는 그 뮤텍스를 기다린다 — 서로를 기다리다
    //     패자가 `lock_deadline` 을 다 쓰고 `token lock:` 으로 실패한다.
    //
    // 그래서 **오류의 종류**가 변이를 가른다. 벽시계는 단언하지 않는다.
    let dir = temp_dir("relock-order");
    seed_expired_token(&dir);
    let refresher = Arc::new(SlowStaleRefresher::new(Duration::from_millis(300)));
    let mgr = Arc::new(
        TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>)
            .with_clock(Arc::new(FixedClock::new(T0)))
            // 변이가 걸렸을 때 10초를 실제로 기다리지 않기 위해서만 줄인다.
            // 승자의 왕복(300ms)보다 길어야 정상 구현이 이 값에 걸리지 않는다.
            .with_lock_deadline(Duration::from_millis(800)),
    );

    let winner = {
        let m = Arc::clone(&mgr);
        std::thread::spawn(move || m.access_token())
    };
    // 승자가 **임계구역 안에 들어간 것을 사실로** 확인한 뒤 패자를 출발시킨다.
    // sleep 으로 순서를 짐작하면 부하 걸린 기계에서 뒤집힌다(A2 와 같은 이유).
    wait_until("승자가 refresh 에 진입", || {
        refresher.call_count() >= 1
    });
    let loser = {
        let m = Arc::clone(&mgr);
        std::thread::spawn(move || m.access_token())
    };

    assert_eq!(winner.join().expect("join").expect("winner"), "NEW");
    let err = loser
        .join()
        .expect("join")
        .expect_err("패자는 간격에 막혀야 한다");
    let msg = err.to_string();
    assert!(
        msg.contains("rotation interval"),
        "간격이 아닌 이유로 실패했다: {msg}"
    );
    assert!(
        !msg.contains("token lock:"),
        "패자가 파일 락에서 굶었다 — 승자가 flock 보다 tokio 뮤텍스를 먼저 \
         놓았다는 뜻이고, 그건 세대를 임계구역 밖에서 읽는 구현의 부작용이다: {msg}"
    );
    assert_eq!(
        refresher.call_count(),
        1,
        "간격 안에서 두 번째 회전을 태웠다"
    );
}

#[test]
fn invalidating_the_current_generation_clears_the_cache() {
    // 반대편. 세대가 일치할 때 **아무것도 하지 않는** 구현(= 401 복구가 영영
    // 낡은 토큰을 다시 받는다)을 배제한다.
    let (mgr, refresher, clock, _dir) = interval_manager("gen-clears", SHORT_BUT_USABLE);
    let (g1, _t1) = mgr.access_token_with_generation().expect("first");
    let locks_after_g1 = mgr.lock_attempts();

    // 캐시가 아직 fresh 하므로, 무효화가 없었다면 다음 호출은 캐시 적중이다.
    mgr.invalidate_generation(g1).expect("invalidate");
    clock.advance(MIN_REFRESH_INTERVAL.as_secs() + 1);

    let (g2, _t2) = mgr.access_token_with_generation().expect("second");
    assert!(g2 > g1, "캐시가 비워지지 않았다 (g1={g1} g2={g2})");
    assert_eq!(refresher.call_count(), 2);
    assert!(
        mgr.lock_attempts() > locks_after_g1,
        "캐시가 그대로라 파일 락에 가지 않았다 — 무효화가 no-op 이다"
    );
}

// --- 401 거절(reject_generation) — A3 final fix wave -------------------

/// 회전할 때마다 **다른** 토큰을 내놓는 refresher.
///
/// `LifetimeRefresher`/`CountingRefresher` 는 언제나 같은 값(`"NEW"`)을
/// 돌려주므로 "거절 뒤에 정말 다른 토큰이 나왔는가" 를 구별할 수 없다 —
/// 거절이 무력화된 구현도 같은 문자열을 돌려주기 때문이다.
struct RotatingRefresher {
    calls: AtomicUsize,
}

impl RotatingRefresher {
    fn new() -> Self {
        Self {
            calls: AtomicUsize::new(0),
        }
    }
    fn call_count(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl TokenRefresher for RotatingRefresher {
    fn refresh(&self, _refresh_token: &str) -> Result<Refreshed, Error> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(Refreshed {
            access_token: format!("ROT{n}"),
            refresh_token: format!("NA_ROT{n}"),
            // 넉넉한 수명 — 이 그룹은 만료가 아니라 **거절**을 시험한다.
            // 짧은 수명을 주면 회전이 만료 때문에 일어났는지 거절 때문에
            // 일어났는지 갈리지 않는다.
            expires_in_secs: 28_800,
        })
    }
}

fn rotating_manager(
    tag: &str,
) -> (
    TokenManager,
    Arc<RotatingRefresher>,
    Arc<FixedClock>,
    TempDir,
) {
    let dir = temp_dir(tag);
    seed_expired_token(&dir);
    let refresher = Arc::new(RotatingRefresher::new());
    let clock = Arc::new(FixedClock::new(T0));
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>)
        .with_clock(Arc::clone(&clock) as Arc<_>)
        .with_lock_deadline(Duration::from_secs(3));
    (mgr, refresher, clock, dir)
}

/// 🔴 거절은 **`is_fresh` 를 넘어** 회전을 강제한다 — 정확히 1회.
///
/// 이 테스트가 `invalidating_the_current_generation_clears_the_cache` 와 다른
/// 점이 이 그룹의 존재 이유다. 무효화는 캐시만 비우므로, 캐시와 디스크가 **둘 다
/// 신선한** 이 상황에서는 다음 호출이 디스크를 그대로 채택한다 — 즉 서버가 방금
/// 거절한 그 바이트를 다시 내준다. 401 은 `expires_at` 을 움직이지 않으므로
/// 신선도만 보는 구현은 영영 회복하지 못한다.
///
/// 그래서 여기서는 (1) 회전 카운터가 실제로 올랐는지, (2) 나온 토큰이 **다른
/// 값**인지, (3) 그 다음 호출은 다시 캐시 적중인지(= 강제가 1회로 끝나는지)를
/// 함께 본다. `reject_generation` 을 `invalidate_generation` 으로 되돌리는
/// 변이는 (1)(2) 에서 죽고, "거절 표시를 영구 강제로 읽는" 변이는 (3) 에서 죽는다.
#[test]
fn rejecting_the_current_generation_forces_exactly_one_rotation_past_a_fresh_token() {
    let (mgr, refresher, clock, _dir) = rotating_manager("gen-reject-forces");
    let (g1, t1) = mgr.access_token_with_generation().expect("first");
    assert_eq!(refresher.call_count(), 1);
    assert_eq!(t1, "ROT1");
    // 전제: 캐시도 디스크도 신선하다. 무효화만으로는 회전이 일어나지 않는 상태다.

    mgr.reject_generation(g1).expect("reject");
    // 회전 억제 게이트를 연다 — 그 게이트 자체는
    // `a_rejection_does_not_bypass_the_minimum_rotation_interval` 이 본다.
    clock.advance(MIN_REFRESH_INTERVAL.as_secs() + 1);

    let (g2, t2) = mgr.access_token_with_generation().expect("second");
    assert_eq!(
        refresher.call_count(),
        2,
        "거절이 신선도 판정에 막혀 회전을 강제하지 못했다 — 재시도가 방금 \
         거절당한 토큰을 그대로 다시 실어 보낸다는 뜻이다"
    );
    assert_ne!(t2, t1, "같은 토큰이 다시 나왔다");
    assert!(g2 > g1, "g1={g1} g2={g2}");

    // 강제는 **1회**다. 세 번째 호출은 캐시 적중이어야 한다 — 기구(파일 락
    // 시도 횟수)로 본다. 값만 보면 "회전했는데 같은 값" 과 구별되지 않는다.
    let locks_after_g2 = mgr.lock_attempts();
    clock.advance(MIN_REFRESH_INTERVAL.as_secs() + 1);
    let (g3, t3) = mgr.access_token_with_generation().expect("third");
    assert_eq!(
        refresher.call_count(),
        2,
        "거절 표시가 남아 회전을 또 태웠다 — 강제는 1회여야 한다"
    );
    assert_eq!((g3, t3.as_str()), (g2, t2.as_str()));
    assert_eq!(
        mgr.lock_attempts(),
        locks_after_g2,
        "캐시 적중이 아니라 파일 락까지 갔다"
    );
}

/// 거절해도 형제 프로세스의 **다른** 토큰 채택은 그대로 살아 있다.
///
/// 거절을 "디스크 채택을 통째로 건너뛴다" 로 구현하면 이 경로가 죽는다 —
/// 형제가 이미 회전시켜 둔 멀쩡한 토큰을 두고 우리가 또 회전(= 체인 무효화
/// 위험 + 불필요한 POST)하게 된다. 값이 **같을 때만** 막아야 한다.
#[test]
fn a_rejected_token_still_yields_to_a_siblings_newer_token() {
    let (mgr, refresher, _clock, dir) = rotating_manager("gen-reject-adopt");
    let (g1, t1) = mgr.access_token_with_generation().expect("first");
    assert_eq!(refresher.call_count(), 1);

    mgr.reject_generation(g1).expect("reject");
    // 형제 프로세스가 회전시켜 둔 모양. 값이 우리 것과 **다르다**.
    write_token(&dir, "FROM_SIBLING", "NA_SIBLING", FAR_FUTURE);

    let (g2, t2) = mgr.access_token_with_generation().expect("adopt");
    assert_eq!(t2, "FROM_SIBLING", "형제의 새 토큰을 채택하지 않았다");
    assert_ne!(t2, t1);
    assert_eq!(
        refresher.call_count(),
        1,
        "채택할 수 있는 다른 토큰이 있는데 회전 POST 를 태웠다"
    );
    assert!(
        g2 > g1,
        "캐시가 교체됐는데 세대가 그대로다 (g1={g1} g2={g2})"
    );
}

/// 낡은 세대의 거절은 no-op 이다 — 방금 갱신된 토큰을 죽이지 않는다.
///
/// `invalidate_generation` 의 `stale_generation_cannot_invalidate_newer_token`
/// 과 같은 사고를 거절 쪽에서 막는다. 거절이 세대 비교를 건너뛰면 뒤늦게
/// 도착한 401 이 형제가 방금 만든 멀쩡한 토큰을 블랙리스트에 올려, 그 토큰을
/// 쓰는 요청까지 연쇄로 죽인다.
#[test]
fn a_stale_generation_cannot_reject_a_newer_token() {
    let (mgr, refresher, clock, _dir) = rotating_manager("gen-reject-stale");
    let (g1, _t1) = mgr.access_token_with_generation().expect("first");
    mgr.reject_generation(g1).expect("reject g1");
    clock.advance(MIN_REFRESH_INTERVAL.as_secs() + 1);
    let (g2, t2) = mgr.access_token_with_generation().expect("second");
    assert_eq!(refresher.call_count(), 2);
    let locks_after_g2 = mgr.lock_attempts();

    // 뒤늦게 도착한 401 이 **낡은** 세대로 거절을 시도한다.
    mgr.reject_generation(g1)
        .expect("낡은 세대의 거절도 오류가 아니라 no-op 이다");

    clock.advance(MIN_REFRESH_INTERVAL.as_secs() + 1);
    let (g3, t3) = mgr.access_token_with_generation().expect("third");
    assert_eq!(g3, g2, "낡은 세대의 거절이 먹혔다");
    assert_eq!(t3, t2, "멀쩡한 토큰이 버려졌다");
    assert_eq!(
        refresher.call_count(),
        2,
        "낡은 거절이 회전을 한 번 더 태웠다"
    );
    assert_eq!(
        mgr.lock_attempts(),
        locks_after_g2,
        "캐시가 버려져 파일 락까지 갔다 — 낡은 거절이 no-op 이 아니었다"
    );
}

/// 한 번 거절당한 바이트는 **나중에 디스크에서 되살아나도** 다시 나가지 않는다.
///
/// [`State::rejected_access_token`] 을 회전 성공 후에 지우지 **않는다**는 결정을
/// 못박는다. 변이 M7(회전 성공 시 표시 삭제)은 여기 말고는 어디서도 죽지
/// 않았다 — 다른 모든 테스트에서는 회전 직후의 새 토큰이 어차피 값이 달라
/// 게이트에 걸리지 않기 때문이다. 그래서 표시가 지워졌을 때에만 실패하는
/// 입력을 따로 만든다: 죽은 토큰이 **다시 디스크에 나타나는** 상황이다
/// (백업 복원, 또는 옛 파일을 들고 있던 형제 프로세스의 되쓰기).
#[test]
fn a_rejected_token_is_never_adopted_again_even_after_a_later_rotation() {
    let (mgr, refresher, clock, dir) = rotating_manager("gen-reject-sticky");
    let (g1, t1) = mgr.access_token_with_generation().expect("first");
    mgr.reject_generation(g1).expect("reject");
    clock.advance(MIN_REFRESH_INTERVAL.as_secs() + 1);
    let (g2, t2) = mgr.access_token_with_generation().expect("rotated");
    assert_eq!(refresher.call_count(), 2);
    assert_ne!(t2, t1);

    // 죽은 토큰이 디스크에 되살아난다 — 그리고 신선하다.
    write_token(&dir, &t1, "NA_RESURRECTED", FAR_FUTURE);
    // 캐시를 비워 디스크 경로로 보낸다(캐시 적중이면 디스크를 보지 않는다).
    mgr.invalidate_generation(g2).expect("invalidate");
    clock.advance(MIN_REFRESH_INTERVAL.as_secs() + 1);

    let (_g3, t3) = mgr.access_token_with_generation().expect("third");
    assert_ne!(
        t3, t1,
        "이미 서버가 거절한 토큰을 디스크에서 되살려 내보냈다"
    );
    assert_eq!(
        refresher.call_count(),
        3,
        "죽은 토큰을 채택하는 대신 회전했어야 한다"
    );
}

/// 거절은 회전 억제 게이트를 **우회하지 않는다**.
///
/// 강제되는 것은 "캐시/채택으로 빠져나가지 못한다" 뿐이고, 실제 회전은 여전히
/// `MIN_REFRESH_INTERVAL` 을 통과해야 한다. 거절이 게이트를 건너뛰도록 구현하면
/// 401 이 연달아 오는 동안 회전 폭주가 열린다.
///
/// 뒷부분이 함께 못박는 것: 간격에 막혀도 **거절 표시는 남는다**. 표시가
/// 지워지는 구현이면 간격이 지난 뒤의 호출이 디스크를 채택해 죽은 토큰으로
/// 되돌아간다.
#[test]
fn a_rejection_does_not_bypass_the_minimum_rotation_interval() {
    let (mgr, refresher, clock, _dir) = rotating_manager("gen-reject-interval");
    let (g1, t1) = mgr.access_token_with_generation().expect("first");
    assert_eq!(refresher.call_count(), 1);

    // 회전 직후다 — 간격이 아직 차지 않았다.
    mgr.reject_generation(g1).expect("reject");
    let err = mgr
        .access_token()
        .expect_err("간격 안의 강제 회전은 거절돼야 한다");
    assert!(
        err.to_string().contains("rotation interval"),
        "게이트가 아닌 다른 이유로 실패했다: {err}"
    );
    assert_eq!(
        refresher.call_count(),
        1,
        "거절이 회전 억제 게이트를 우회했다"
    );

    // 간격이 지나면 그때 회전한다 — 거절 표시가 살아 있어야 한다.
    clock.advance(MIN_REFRESH_INTERVAL.as_secs() + 1);
    let (_g2, t2) = mgr.access_token_with_generation().expect("second");
    assert_eq!(refresher.call_count(), 2, "간격이 찼는데 회전하지 않았다");
    assert_ne!(t2, t1, "거절 표시가 지워져 죽은 토큰으로 돌아갔다");
}

// --- single-flight ---------------------------------------------------

#[test]
fn concurrent_callers_collapse_into_exactly_one_refresh() {
    let dir = temp_dir("singleflight");
    seed_expired_token(&dir);
    // 갱신을 100ms 끌어 동시 진입을 확실히 벌립니다. 대기자는 깨우기로 풀려야 하고, 마감(20초)까지
    // 자면 아래 경과 시간 단언(10초)이 실패합니다(최종 검토 I-1). 마감과 기준을 멀리 둔 까닭은 전체 시험을 함께
    // 돌릴 때 부하로 2.27초가 걸린 일이 있어서입니다(2026-10-14, 깨우기를 놓쳤다면 마감까지 잤을 것).
    let refresher = Arc::new(CountingRefresher::with_delay(
        "NEW",
        Duration::from_millis(100),
    ));
    let mgr = Arc::new(
        TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>)
            .with_state_deadline(Duration::from_secs(20)),
    );

    let started = std::time::Instant::now();
    let mut handles = Vec::new();
    for _ in 0..16 {
        let m = Arc::clone(&mgr);
        handles.push(std::thread::spawn(move || m.access_token()));
    }
    for h in handles {
        assert_eq!(h.join().expect("join").expect("access_token"), "NEW");
    }
    assert_eq!(
        refresher.call_count(),
        1,
        "refresh 가 single-flight 로 합쳐지지 않았다"
    );
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "waiters slept until the state deadline instead of being woken: {:?}",
        started.elapsed()
    );
}

#[test]
fn concurrent_callers_are_served_from_memory_rather_than_each_taking_the_file_lock() {
    // 위 테스트만으로는 **어느 기구가** 합쳤는지 알 수 없다. 프로세스 간 파일
    // 락 하나만으로도 같은 프로세스의 동시 호출자가 합쳐지기 때문이다(뒤늦은
    // 호출자는 락을 기다렸다가 디스크에서 채택한다). 그래서 tokio Mutex 를
    // await 구간에서 놓아 버리는 변이가 위 테스트를 그대로 통과한다.
    //
    // 파일 락 deadline 을 0 으로 두면 그 차이가 드러난다. REQ-15 대로 in-process
    // single-flight 가 있으면 뒤늦은 호출자는 **메모리에서** 답을 받고 파일 락을
    // 아예 건드리지 않는다. 없으면 15개가 동시에 락을 시도해 전부 Timeout 이다.
    //
    // ⚠️ Task 7 리뷰가 잡은 구멍(M2 오분류): 반환값과 호출 횟수만 보면 **캐시
    // 확인을 지운 구현도 이 테스트를 통과한다.** 뒤늦은 호출자가 도착할 때쯤엔
    // 승자가 이미 `_guard` 를 놓았으므로 deadline 0 에서도 락이 잡히기 때문이다.
    // 같은 값을 다른 기구로 얻은 것이라 이름이 거짓말이 된다. 그래서 기구
    // 자체를 센다 — 락을 잡으러 간 횟수가 **정확히 1**(=승자뿐)이어야 한다.
    //
    // 프로세스 내 뮤텍스 deadline 은 이 시험의 대상이 아니므로 넉넉히(60초) 둡니다. 기본 10초에서는 `verify.sh`
    // 전체 시험 부하 중 승자의 갱신·저장(fsync)이 10초를 넘겨 뒤늦은 호출자 하나가 `token state lock:` 으로
    // 실패했습니다(2026-10-20, 크레이트 시험 15.18초, 단독 20/20 통과). 깃발이 풀리지 않는 결함은 60초 뒤에도 잡힙니다.
    let dir = temp_dir("memoryserved");
    seed_expired_token(&dir);
    let refresher = Arc::new(CountingRefresher::new("NEW"));
    let mgr = Arc::new(
        TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>)
            .with_lock_deadline(Duration::ZERO)
            .with_state_deadline(Duration::from_secs(60)),
    );

    let mut handles = Vec::new();
    for _ in 0..16 {
        let m = Arc::clone(&mgr);
        handles.push(std::thread::spawn(move || m.access_token()));
    }
    for h in handles {
        assert_eq!(
            h.join()
                .expect("join")
                .expect("모든 호출자가 메모리에서 답을 받아야 한다"),
            "NEW"
        );
    }
    assert_eq!(refresher.call_count(), 1);
    assert_eq!(
        mgr.lock_attempts(),
        1,
        "뒤늦은 호출자들이 메모리가 아니라 파일 락에서 답을 받았다"
    );
}

#[test]
fn two_managers_sharing_a_directory_collapse_into_one_refresh() {
    // REQ-15 AC2 의 **프로세스 간** 절반. 여기까지의 동시성 테스트는 전부
    // 관리자 **하나**를 썼고, 그러면 tokio 뮤텍스가 파일 락이 경합되기 전에
    // 모든 것을 직렬화해 버린다 — flock 의 "락 아래에서 디스크 재읽기 → 형제가
    // 갱신했으면 채택" 경로는 한 번도 실행되지 않았다. 유일한 경합 테스트
    // (`a_lock_timeout_never_refreshes`)는 맨 `lock::acquire` 로 락을 쥐므로
    // 패자가 **타임아웃**한다 — 채택 경로의 정반대다.
    //
    // 그래서 이 테스트가 없으면 다음 두 변이가 222개 테스트를 모두 통과한다.
    //   (a) 5번과 6번을 맞바꾼다(락보다 디스크를 **먼저** 읽는다). 실제 경합에서
    //       패자는 **낡은** 디스크를 읽고, 락을 기다린 뒤, 이미 회전된 refresh
    //       token 으로 다시 refresh 한다 — Tesla 가 체인 전체 무효화로 답하는
    //       바로 그 이중 refresh 다.
    //   (b) 디스크 재읽기 직후 `drop(_guard)`. 같은 결과다.
    //
    // 설계가 도구를 준다: flock 은 **open file description** 단위라, 한
    // 프로세스 안의 두 `TokenManager` 는 이 목적에서 형제 프로세스 둘이다.
    let dir = temp_dir("twomanagers");
    seed_expired_token(&dir);
    // ⚠️ refresher 를 **공유**한다 — 합쳐졌는지 보려면 합계가 필요하다.
    let refresher = Arc::new(CountingRefresher::new("NEW"));
    let winner = Arc::new(TokenManager::new(
        dir.to_path_buf(),
        Arc::clone(&refresher) as Arc<_>,
    ));
    let loser = Arc::new(TokenManager::new(
        dir.to_path_buf(),
        Arc::clone(&refresher) as Arc<_>,
    ));

    let w = Arc::clone(&winner);
    let first = std::thread::spawn(move || w.access_token());

    // 승자가 **파일 락을 쥔 채** refresh 안에 들어간 것을 확인한 뒤에 패자를
    // 출발시킨다. 둘을 그냥 동시에 던지면 어느 쪽이 락을 먼저 잡을지 모르고,
    // 위 변이들이 통과할 수도 있는 순서가 섞인다.
    wait_until("승자가 refresh 에 진입", || {
        refresher.call_count() >= 1
    });

    let l = Arc::clone(&loser);
    let second = std::thread::spawn(move || l.access_token());

    assert_eq!(first.join().expect("join").expect("winner"), "NEW");
    assert_eq!(
        second.join().expect("join").expect("loser"),
        "NEW",
        "패자가 형제의 새 토큰을 받지 못했다"
    );

    assert_eq!(
        refresher.call_count(),
        1,
        "두 관리자가 각각 회전했다 — 프로세스 간 single-flight 가 깨졌고, \
         두 번째 회전은 이미 무효화된 체인의 재사용이다"
    );
    // 패자가 "캐시에서 답했다" 가 아니라 "락까지 가서 채택했다" 임을 못박는다.
    // 패자는 방금 만든 관리자라 캐시가 비어 있으므로, 락에 가지 않았다면
    // 답을 얻을 길이 없다.
    assert_eq!(winner.lock_attempts(), 1, "승자가 락을 잡지 않았다");
    assert_eq!(
        loser.lock_attempts(),
        1,
        "패자가 파일 락을 잡으러 가지 않았다"
    );
    // 디스크에는 회전이 정확히 한 번 반영돼 있다.
    let v = disk_json(&dir);
    assert_eq!(v["access_token"], serde_json::json!("NEW"));
    assert_eq!(v["refresh_token"], serde_json::json!("NA_ROTATED"));
}

#[test]
fn a_contended_in_process_mutex_times_out_with_a_message_distinct_from_the_file_lock() {
    // REQ-15 AC3: "뮤텍스와 flock 획득에는 유한 timeout 또는 전체 작업 deadline".
    // flock 쪽에는 10초 deadline 이 있었지만 뮤텍스 쪽에는 없었다 — 대기 시간이
    // (큐 깊이) × (그 자체로 무한한 refresh) 였으므로 어느 disjunct 도 성립하지
    // 않았다.
    //
    // **전체 작업 deadline 은 일부러 두지 않는다.** 그것은 refresh POST 한복판을
    // 취소할 수 있다는 뜻이고, 그 순간 서버는 회전을 커밋했는데 우리는 새
    // refresh token 을 저장하지 못한다. 뮤텍스 대기는 어떤 POST 보다도 앞이라
    // 포기해도 잃을 상태가 없다 — 그래서 경계를 여기에 긋는다.
    let dir = temp_dir("statedeadline");
    seed_expired_token(&dir);
    // 임계구역(= refresh await)을 오래 붙잡는다.
    let refresher = Arc::new(CountingRefresher::with_delay("NEW", Duration::from_secs(1)));
    let mgr = Arc::new(
        TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>)
            .with_state_deadline(Duration::from_millis(50)),
    );

    let holder = {
        let m = Arc::clone(&mgr);
        std::thread::spawn(move || m.access_token())
    };
    wait_until("보유자가 refresh 에 진입", || {
        refresher.call_count() >= 1
    });

    let err = mgr
        .access_token()
        .expect_err("뮤텍스 대기가 무한하면 안 된다");
    let msg = err.to_string();
    assert!(msg.contains("token state lock"), "{msg}");
    // 두 대기를 오류 문장만으로 가를 수 있어야 한다 — 버그 리포트에는 대개
    // 이 한 줄만 실려 온다.
    assert!(
        !msg.contains("token lock:"),
        "프로세스 내 뮤텍스 대기가 파일 락 대기와 같은 문장으로 도착한다: {msg}"
    );
    assert_eq!(
        refresher.call_count(),
        1,
        "뮤텍스를 얻지 못했는데 refresh 했다"
    );

    // 보유자는 방해받지 않고 끝난다 — timeout 이 진행 중인 회전을 건드리지 않는다.
    assert_eq!(holder.join().expect("join").expect("holder"), "NEW");
}

// --- REQ-18: Refreshed 의 Debug -------------------------------------

#[test]
fn refreshed_debug_never_prints_token_material() {
    // C1. 이 타입은 크레이트 루트로 re-export 되고 access token 과 refresh
    // token 을 **둘 다** 들고 다니며, 만들어지는 자리는 전송 계층(A3)의 refresh
    // 응답 파서 직후다 — `#[derive(Debug)]` 이면 거기서 `tracing::debug!(
    // ?refreshed)` 한 줄이 살아 있는 refresh token 을 로그에 박는다.
    // `TokenSet` 이 받은 것과 같은 대우(구조분해 + REDACTED)를 여기서도 못박는다.
    let r = Refreshed::new(
        "AT_LEAKED_ACCESS".to_owned(),
        "NA_LEAKED_REFRESH".to_owned(),
        28_800,
    );
    let rendered = format!("{r:?}");
    assert!(
        !rendered.contains("AT_LEAKED_ACCESS"),
        "access token leaked: {rendered}"
    );
    assert!(
        !rendered.contains("NA_LEAKED_REFRESH"),
        "refresh token leaked: {rendered}"
    );
    // 마커는 크레이트 단일 리댁션 계층의 상수여야 한다 — 여기서 문자열을 다시
    // 적으면 출처가 둘로 갈라진다(`TokenSet` 과 같은 이유).
    assert!(
        rendered.contains(crate::redaction::REDACTED),
        "marker diverged from redaction::REDACTED: {rendered}"
    );
    // 반대편: 통째로 가려 진단이 불가능해지는 구현도 배제한다. 수명은 비밀이
    // 아니고, 이 타입에서 실제로 보고 싶은 유일한 값이다.
    assert!(
        rendered.contains("28800"),
        "expires_in_secs 까지 가렸다: {rendered}"
    );
}

#[test]
fn refreshed_new_assigns_the_fields_in_the_documented_order() {
    // 두 인자가 뒤바뀐 생성자는 Debug 테스트를 전부 통과한다(둘 다 가려지므로).
    // 그리고 그 실수는 refresh token 자리에 access token 을 저장해 체인을 잃는다.
    let r = Refreshed::new("ACCESS".to_owned(), "REFRESH".to_owned(), 7);
    assert_eq!(r.access_token, "ACCESS");
    assert_eq!(r.refresh_token, "REFRESH");
    assert_eq!(r.expires_in_secs, 7);
}

#[test]
fn a_fresh_token_on_disk_is_used_without_calling_refresh() {
    let dir = temp_dir("nofresh");
    write_token(&dir, "STILLGOOD", "NA_X", FAR_FUTURE);

    let refresher = Arc::new(CountingRefresher::new("UNUSED"));
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>);
    assert_eq!(mgr.access_token().expect("token"), "STILLGOOD");
    assert_eq!(
        refresher.call_count(),
        0,
        "유효한 토큰인데 refresh 를 호출했다"
    );
}

#[test]
fn a_cached_fresh_token_is_returned_without_touching_the_file_lock() {
    // 캐시 확인을 지운 구현은 위 두 테스트를 모두 통과한다 — 차이는 "파일
    // 락을 잡으러 가는가" 뿐이다. 락을 밖에서 쥐고 있으면 그 차이가 보인다.
    let dir = temp_dir("fastpath");
    write_token(&dir, "STILLGOOD", "NA_X", FAR_FUTURE);
    let refresher = Arc::new(CountingRefresher::new("UNUSED"));
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>)
        .with_lock_deadline(Duration::from_secs(3));

    // 첫 호출은 락을 잡고 디스크에서 채택한다(캐시가 비어 있으므로).
    assert_eq!(mgr.access_token().expect("first"), "STILLGOOD");
    assert_eq!(mgr.lock_attempts(), 1, "첫 호출은 락을 잡아야 한다");

    let held = lock::acquire(&lock_path(&dir), lock::LOCK_DEADLINE).expect("hold the lock");
    // 캐시 확인을 지운 구현은 여기서 deadline(3초)까지 기다린 뒤 Timeout 으로
    // 실패하므로 `expect` 가 잡는다. 벽시계 지연은 단언하지 않는다 — 오류
    // 종류만으로 이미 변이가 죽고, 부하 걸린 기계에서 흔들릴 이유만 는다.
    // 대신 **결정적인** 증거를 본다: 락을 잡으러 간 횟수가 늘지 않았다.
    assert_eq!(mgr.access_token().expect("cached"), "STILLGOOD");
    assert_eq!(
        mgr.lock_attempts(),
        1,
        "캐시가 유효한데 파일 락을 다시 잡으러 갔다"
    );
    assert_eq!(refresher.call_count(), 0);
    drop(held);
}

// --- 회전 보존 -------------------------------------------------------

#[test]
fn the_rotated_refresh_token_is_persisted_before_the_new_access_token_is_returned() {
    let dir = temp_dir("rotation");
    seed_expired_token(&dir);
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::new(CountingRefresher::new("NEW")));

    assert_eq!(mgr.access_token().expect("token"), "NEW");

    // 디스크가 이미 새 refresh token 을 갖고 있어야 한다 — 저장 전에 반환하면
    // 프로세스가 여기서 죽었을 때 회전된 토큰을 잃고 재-OAuth 가 필요해진다.
    let v = disk_json(&dir);
    assert_eq!(v["refresh_token"], serde_json::json!("NA_ROTATED"));
    assert_eq!(v["access_token"], serde_json::json!("NEW"));

    // REQ-18: 관리자의 Debug 가 토큰을 흘리지 않는다.
    let rendered = format!("{mgr:?}");
    assert!(!rendered.contains("NEW"), "access token leaked: {rendered}");
    assert!(
        !rendered.contains("NA_ROTATED"),
        "refresh token leaked: {rendered}"
    );
}

#[test]
fn expires_at_is_the_refresh_time_plus_the_reported_lifetime() {
    // `now` 만 쓰거나 `expires_in` 만 쓰는 구현, 혹은 시계 주입을 무시하고
    // 벽시계를 읽는 구현을 모두 배제한다.
    let dir = temp_dir("expiry");
    seed_expired_token(&dir);
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::new(CountingRefresher::new("NEW")))
        .with_clock(Arc::new(FixedClock::new(1_000_000)));

    assert_eq!(mgr.access_token().expect("token"), "NEW");
    assert_eq!(
        disk_json(&dir)["expires_at"],
        serde_json::json!(1_028_800_u64),
        "expires_at 이 now + expires_in 이 아니다"
    );
}

// --- 락 ---------------------------------------------------------------

#[test]
fn a_lock_timeout_never_refreshes() {
    // 타임아웃한 채로 진행하는 것이 곧 동시 refresh 이고, Tesla 는 거기에
    // 체인 전체 무효화로 답한다. 이 테스트가 그 경로를 막는다.
    let dir = temp_dir("locktimeout");
    seed_expired_token(&dir);
    let held = lock::acquire(&lock_path(&dir), lock::LOCK_DEADLINE).expect("hold the lock");

    let refresher = Arc::new(CountingRefresher::new("NEW"));
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>)
        .with_lock_deadline(Duration::from_millis(200));

    let err = mgr.access_token().expect_err("타임아웃해야 한다");
    assert!(err.to_string().contains("token lock"), "{err}");
    assert_eq!(
        refresher.call_count(),
        0,
        "락을 얻지 못했는데 refresh 했다 — 동시 refresh 경로"
    );
    assert_eq!(
        disk_json(&dir)["refresh_token"],
        serde_json::json!("NA_OLD"),
        "락 없이 디스크를 갱신했다"
    );
    drop(held);
}

// --- 디렉터리 준비 ----------------------------------------------------

#[test]
fn a_missing_token_directory_is_created_owner_only_instead_of_only_being_checked() {
    // 검사만 하는 가드(`ensure_private_dir` 단독)는 디렉터리가 없을 때
    // NotFound 를 내고 아무것도 만들지 않는다 — 첫 로그인 경로가 통째로 막힌다.
    let base = temp_dir("mkdir");
    let dir = base.join("nested").join("deep");
    let refresher = Arc::new(CountingRefresher::new("UNUSED"));
    let mgr = TokenManager::new(dir.clone(), Arc::clone(&refresher) as Arc<_>)
        .with_lock_deadline(Duration::from_millis(200));

    // 토큰 파일이 없으니 결과는 어차피 오류다. 보는 것은 **부수 효과**다.
    let err = mgr.access_token().expect_err("토큰이 없다");
    assert!(err.to_string().contains("token store"), "{err}");
    assert!(
        dir.is_dir(),
        "디렉터리를 만들지 않았다 — create-then-check 가 아니라 check-only 다"
    );
    assert_owner_only(&base.join("nested"), "token dir (ancestor)");
    assert_owner_only(&dir, "token dir");
    assert_eq!(refresher.call_count(), 0);
}

#[test]
fn an_over_permissive_directory_is_rejected_before_the_lock_is_acquired() {
    // 락부터 잡으러 가는 구현은 경합 중에 10초 Timeout 을 내고, 진짜 원인
    // (디렉터리 권한)은 영영 보이지 않는다. 락을 밖에서 쥔 채로 두 가지를
    // 동시에 본다: 오류의 종류와, 락을 아예 잡으러 가지 않았다는 사실.
    let dir = temp_dir("baddir");
    seed_expired_token(&dir);
    let held = lock::acquire(&lock_path(&dir), lock::LOCK_DEADLINE).expect("hold the lock");
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).expect("chmod");

    let refresher = Arc::new(CountingRefresher::new("NEW"));
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>)
        .with_lock_deadline(Duration::from_secs(3));

    let err = mgr
        .access_token()
        .expect_err("열린 디렉터리를 거부해야 한다");

    assert!(
        err.to_string().contains("token directory permissions"),
        "디렉터리 권한이 아니라 다른 이유로 실패했다: {err}"
    );
    // 벽시계 대신 기구를 본다 — 락을 잡으러 간 적이 **없어야** 한다.
    assert_eq!(mgr.lock_attempts(), 0, "디렉터리 검사보다 락이 먼저였다");
    assert_eq!(refresher.call_count(), 0);

    drop(held);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("restore");
}

#[test]
fn a_broken_clock_is_diagnosed_instead_of_timing_out_on_a_sibling_s_lock() {
    // 제약 2(오진 방지)를 시계에도 적용한 것. `now == u64::MAX` 면 어떤 토큰도
    // fresh 일 수 없으므로 이 검사 앞의 무엇도 결과를 바꾸지 못한다 — 그런데
    // 락을 먼저 잡으러 가면 형제가 쥐고 있을 때 사용자는 10초를 기다린 뒤
    // `token lock: timed out` 을 보고 시계를 의심하지 못한다.
    let dir = temp_dir("clock-before-lock");
    seed_expired_token(&dir);
    let held = lock::acquire(&lock_path(&dir), lock::LOCK_DEADLINE).expect("hold the lock");

    let pre_epoch = std::time::UNIX_EPOCH - Duration::from_secs(1);
    let refresher = Arc::new(CountingRefresher::new("NEW"));
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>)
        .with_lock_deadline(Duration::from_secs(3))
        .with_clock(Arc::new(FixedClock::new(crate::clock::secs_since_epoch(
            pre_epoch,
        ))));

    let err = mgr.access_token().expect_err("거부해야 한다");
    assert!(
        err.to_string().contains("unix epoch"),
        "시계가 아니라 락 경합으로 보고했다: {err}"
    );
    assert_eq!(mgr.lock_attempts(), 0, "시계 검사보다 락이 먼저였다");
    assert_eq!(refresher.call_count(), 0);
    drop(held);
}

// --- write-side epoch 위험 -------------------------------------------

#[test]
fn a_pre_epoch_clock_refuses_to_refresh_rather_than_minting_an_immortal_token() {
    // `saturating_add` 로 그냥 쓰면 expires_at = u64::MAX 가 되어 "영원히
    // 만료되지 않는" 토큰이 디스크에 박힌다. 시계를 고친 뒤에도 그 파일은
    // 계속 신뢰받는다 — 조용하고 영구적인 고장이다.
    let dir = temp_dir("preepoch");
    seed_expired_token(&dir);
    let pre_epoch = std::time::UNIX_EPOCH - Duration::from_secs(1);
    let refresher = Arc::new(CountingRefresher::new("NEW"));
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>).with_clock(
        Arc::new(FixedClock::new(crate::clock::secs_since_epoch(pre_epoch))),
    );

    let err = mgr.access_token().expect_err("거부해야 한다");
    assert!(err.to_string().contains("unix epoch"), "{err}");
    assert_eq!(
        refresher.call_count(),
        0,
        "만료 시각을 계산할 수 없는데 회전을 태웠다"
    );
    let v = disk_json(&dir);
    assert_eq!(v["refresh_token"], serde_json::json!("NA_OLD"));
    assert_eq!(v["expires_at"], serde_json::json!(1_u64));
}

#[test]
fn an_unrepresentable_expiry_is_recorded_as_expired_not_immortal() {
    // 시계가 u64::MAX 근처면 `now + expires_in` 이 표현 불가능하다.
    // `saturating_add` 는 여기서도 영구 토큰을 만든다. 회전은 보존하되
    // 만료 시각은 거짓말하지 않는 방향(= 이미 만료)으로 기록한다.
    //
    // ⚠️ 이 테스트가 못박는 `expires_at == now` 는 겉보기에
    // `MAX_TOKEN_LIFETIME` 상한이 막으려는 바로 그 붕괴(= 매 호출 회전)와 같다.
    // 그런데도 여기에 회전 횟수 상한을 요구하지 않는 이유는 **이 상태가 상용에서
    // 도달 불가능**하기 때문이다: 64비트 유닉스에서 `SystemTime` 의 epoch 이후
    // 초는 `i64::MAX`(≈ `u64::MAX / 2`)로 묶이므로, `SystemClock` 이 낼 수 있는
    // 어떤 `now` 에 대해서도 `now + 2_592_000`(clamp 후 최대치)은 넘치지 않는다.
    // 여기 오려면 `FixedClock` 처럼 값을 직접 주입해야 한다. 즉 이 경로는
    // "서버가 만들 수 있는 조건" 이 아니라 "구현 실수 방지용 안전망" 이고,
    // 서버가 자유롭게 만들 수 있는 위쪽(clamp)·아래쪽(하한 없음, 위험으로
    // 문서화됨)과는 성격이 다르다.
    let dir = temp_dir("nearmax");
    seed_expired_token(&dir);
    let now = u64::MAX - 5;
    let refresher = Arc::new(CountingRefresher::new("NEW"));
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>)
        .with_clock(Arc::new(FixedClock::new(now)));

    // refresh 뒤 `is_fresh` 를 다시 보고 반복하는 구현은 여기서 영원히 돌거나
    // 오류를 낸다. 정확히 한 번 갱신하고 새 토큰을 그대로 돌려줘야 한다.
    assert_eq!(mgr.access_token().expect("token"), "NEW");
    assert_eq!(refresher.call_count(), 1);

    let v = disk_json(&dir);
    assert_eq!(v["refresh_token"], serde_json::json!("NA_ROTATED"));
    assert_ne!(
        v["expires_at"],
        serde_json::json!(u64::MAX),
        "영원히 만료되지 않는 토큰을 디스크에 박았다"
    );
    assert_eq!(v["expires_at"], serde_json::json!(now));
}

// --- 저장 실패 계약 ---------------------------------------------------

#[test]
fn a_failed_persist_forbids_a_second_refresh_but_still_adopts_a_good_disk_token() {
    // 저장이 실패하면 디스크 상태를 알 수 없다(rename 뒤 fsync 가 실패할 수
    // 있다). 옛 refresh token 으로 다시 시도하는 것은 Tesla 가 체인 전체
    // 무효화로 답하는 바로 그 행동이다.
    let dir = temp_dir("persistfail");
    seed_expired_token(&dir);
    // 락 파일을 미리 만든다 — 쓰기 금지 디렉터리에서는 새로 만들 수 없다.
    drop(lock::acquire(&lock_path(&dir), lock::LOCK_DEADLINE).expect("pre-create the lock file"));
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).expect("chmod");
    if !writes_are_denied(&dir) {
        // root 로 돌리는 중 — 저장 실패를 연출할 수 없으므로 시험할 것이 없다.
        //
        // ⚠️ **반드시 알린다.** 조용히 return 하면 root 컨테이너 CI 에서 이
        // 테스트가 통째로 no-op 이 되고 — `refresh_forbidden` 계약과 poison
        // 이후 디스크 채택을 시험하는 **유일한** 테스트다 — CI 는 초록으로
        // 남는다. store.rs 의 같은 가드와 같은 방식으로 말한다.
        eprintln!(
            "skipped a_failed_persist_forbids_a_second_refresh_but_still_adopts_a_good_disk_token: \
             root 권한 — 읽기 전용 디렉터리에서도 쓰기가 성공한다"
        );
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("restore");
        return;
    }

    let refresher = Arc::new(CountingRefresher::new("NEW"));
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>)
        .with_lock_deadline(Duration::from_millis(500));

    let err = mgr.access_token().expect_err("저장이 실패해야 한다");
    assert!(err.to_string().contains("persist"), "{err}");
    for leaked in ["NA_OLD", "NA_ROTATED", "NEW"] {
        assert!(
            !err.to_string().contains(leaked),
            "오류 메시지에 토큰이 실렸다: {err}"
        );
    }
    assert_eq!(refresher.call_count(), 1);

    // 2회차 — 저장 못 한 회전 뒤에 옛 토큰으로 다시 시도하면 안 된다.
    let err2 = mgr.access_token().expect_err("금지돼야 한다");
    assert!(err2.to_string().contains("token chain"), "{err2}");
    // 사유는 저장 실패여야 합니다(panic 사유와 섞이지 않게, 최종 검토 M-3).
    assert!(
        err2.to_string().contains("could not be persisted"),
        "{err2}"
    );
    assert_eq!(
        refresher.call_count(),
        1,
        "저장하지 못한 회전 뒤에 옛 refresh token 으로 재시도했다"
    );

    // 그러나 형제 프로세스가 성공적으로 갱신해 두면 채택은 계속한다 —
    // 금지가 "무조건 오류" 로 퇴화하지 않았음을 본다.
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("restore");
    write_token(&dir, "FROM_DISK", "NA_SIBLING", FAR_FUTURE);
    assert_eq!(mgr.access_token().expect("adopt"), "FROM_DISK");
    assert_eq!(refresher.call_count(), 1);
}

#[test]
fn a_missing_token_file_is_reported_without_calling_refresh() {
    // 갱신할 refresh token 자체가 없다. 여기서 refresher 를 부르면 빈 문자열로
    // POST 하게 된다.
    let dir = temp_dir("notokenfile");
    let refresher = Arc::new(CountingRefresher::new("NEW"));
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>)
        .with_lock_deadline(Duration::from_millis(200));

    let err = mgr.access_token().expect_err("토큰이 없다");
    assert!(err.to_string().contains("token store"), "{err}");
    assert_eq!(refresher.call_count(), 0);
}

// --- refresh 실패 (저장 실패와 대칭이 아니다) --------------------------

#[test]
fn a_failing_refresh_leaves_the_cache_the_disk_and_the_poison_flag_untouched() {
    // 저장 실패와 **다르게** 다뤄야 하는 쪽. POST 가 실패했을 때 우리가 쥔
    // refresh token 은 손대지 않았으므로 재시도가 안전하다. 여기서까지
    // 금지해 버리면 지하주차장의 타임아웃 하나가 재-OAuth 를 부른다.
    //
    // 네 가지를 한꺼번에 못박는다: 오류 전파 / 디스크 불변 / 캐시 미오염 /
    // 재시도 허용. 특히 마지막이 저장 실패와의 비대칭을 고정한다.
    let dir = temp_dir("failrefresh");
    seed_expired_token(&dir);
    let refresher = Arc::new(FailingRefresher::new());
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>)
        .with_lock_deadline(Duration::from_millis(200));

    let err = mgr.access_token().expect_err("실패가 전파돼야 한다");
    assert!(err.to_string().contains("upstream refused"), "{err}");
    assert_eq!(refresher.call_count(), 1);

    // 디스크는 손대지 않은 채여야 한다 — 부분 저장은 회전을 잃는 길이다.
    let v = disk_json(&dir);
    assert_eq!(v["access_token"], serde_json::json!("OLD"));
    assert_eq!(v["refresh_token"], serde_json::json!("NA_OLD"));
    assert_eq!(v["expires_at"], serde_json::json!(1_u64));

    // 2회차도 같은 오류 — 깨진 토큰이 캐시되면 여기서 Ok 가 나온다.
    let err2 = mgr.access_token().expect_err("여전히 실패해야 한다");
    assert!(err2.to_string().contains("upstream refused"), "{err2}");
    assert!(
        !err2.to_string().contains("token chain"),
        "네트워크 실패를 저장 실패처럼 poison 했다: {err2}"
    );
    assert_eq!(
        refresher.call_count(),
        2,
        "일시적 refresh 실패 뒤에 재시도를 막았다 — 저장 실패와 같은 취급이다"
    );
}

// --- 서버가 준 수명 ---------------------------------------------------

#[test]
fn an_absurd_reported_lifetime_is_clamped_instead_of_collapsing_to_expired() {
    // 상한이 없으면 `now + u64::MAX` 가 넘쳐 `unwrap_or(now)` = **이미 만료**로
    // 접힌다. 그러면 이후 **모든** 호출이 refresh POST 를 태우고 체인을 한 번씩
    // 회전시킨다 — 호출당으로는 유한하지만 호출 수에 대해서는 무한하고, 상대는
    // Tesla 의 auth rate limit 이다. 두 번째 호출이 그 폭주를 잡는다.
    let dir = temp_dir("absurdlifetime");
    seed_expired_token(&dir);
    let now = 1_000_000_u64;
    let refresher = Arc::new(LifetimeRefresher::new(u64::MAX));
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>)
        .with_clock(Arc::new(FixedClock::new(now)));

    assert_eq!(mgr.access_token().expect("token"), "NEW");
    let expires_at = disk_json(&dir)["expires_at"].clone();
    assert_ne!(
        expires_at,
        serde_json::json!(now),
        "말도 안 되는 수명이 '이미 만료'로 접혔다 — 매 호출이 체인을 회전시킨다"
    );
    assert_ne!(
        expires_at,
        serde_json::json!(u64::MAX),
        "영원히 만료되지 않는 토큰을 디스크에 박았다"
    );
    assert_eq!(
        expires_at,
        serde_json::json!(now + MAX_TOKEN_LIFETIME.as_secs()),
        "상한이 아니라 다른 값으로 깎였다"
    );

    // 진짜 확인하려는 것: 두 번째 호출이 회전을 태우지 않는다.
    assert_eq!(mgr.access_token().expect("second"), "NEW");
    assert_eq!(
        refresher.call_count(),
        1,
        "두 번째 호출이 또 회전했다 — 상한이 폭주를 막지 못했다"
    );
}

#[test]
fn the_lifetime_cap_is_thirty_days() {
    // 위 테스트는 `MAX_TOKEN_LIFETIME` 자신에 대해 단언하므로 상수를 바꿔도
    // 통과한다(10년으로 올려도 verbatim 테스트까지 함께 통과한다). 값 자체를
    // 여기서 못박아, 상한을 움직이는 변경이 의식적인 결정이 되게 한다.
    assert_eq!(MAX_TOKEN_LIFETIME, Duration::from_secs(2_592_000));
}

#[test]
fn a_reported_lifetime_inside_the_skew_window_is_stale_on_arrival() {
    // 이름 그대로 — skew 창 안의 수명은 **여전히** 도착 즉시 stale 이다. 그 사실은
    // A3 이후에도 바뀌지 않았고, 이 테스트가 계속 그 경계를 못박는다.
    //
    // `is_fresh` 는 `expires_at > now + EXPIRY_SKEW`(60초)다. 따라서 서버가
    // `expires_in_secs <= 60` 을 주면 새 토큰이 기록되는 순간 이미 stale 이다.
    //
    // # A2 → A3 에서 무엇이 바뀌었나 (이 테스트의 유일한 변경점)
    //
    // A2 시절 이 테스트는 **알려진 미해결 위험을 못박는** 것이었다: 그 뒤 모든
    // 호출이 refresh POST 를 태우며 체인을 회전시켰고 — `MAX_TOKEN_LIFETIME`
    // 상한이 위쪽에서 막으려던 것과 **같은 피해**가 아래쪽에서, 시계 고장 없이
    // 일어났다 — A2 는 그것을 고치지 않고 "매 호출 회전" 을 그대로 기록해 두었다.
    //
    // A3 Task 5 가 `MIN_REFRESH_INTERVAL` 로 그 구멍을 닫았다. 그래서 아래
    // 루프의 마지막 블록만 "두 번째 호출이 또 회전한다" → "두 번째 호출이 간격에
    // 막힌다" 로 바뀌었다. 나머지는 한 줄도 손대지 않았다.
    //
    // ⚠️ **`max` 하한이 아니다** — A2 가 금지한 그 방향은 서버가 진심으로 말한 짧은
    // 수명을 무시하고 죽은 토큰을 실어 보낸다(근거는 `MAX_TOKEN_LIFETIME` 의 doc,
    // 그 논증은 지금도 유효하다). 이 테스트가 그 구별을 증명한다: 아래
    // `expires_at == now + expires_in` 단언이 **그대로 통과**하므로 수명 값은
    // 조작되지 않았고, 억제된 것은 회전의 **빈도**뿐이다.
    let now = 1_000_000_u64;

    // 경계 위 — 61초는 skew 바깥이므로 정상 캐시된다(대조군).
    {
        let dir = temp_dir("skewfloor-above");
        seed_expired_token(&dir);
        let refresher = Arc::new(LifetimeRefresher::new(EXPIRY_SKEW.as_secs() + 1));
        let mgr = TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>)
            .with_clock(Arc::new(FixedClock::new(now)));

        assert_eq!(mgr.access_token().expect("first"), "NEW");
        assert_eq!(mgr.access_token().expect("second"), "NEW");
        assert_eq!(
            refresher.call_count(),
            1,
            "skew 바깥(61초)인데 두 번째 호출이 또 회전했다"
        );
    }

    // 경계 위·아래 — 0초와 정확히 skew(60초) 둘 다 도착 즉시 stale.
    for expires_in in [0, EXPIRY_SKEW.as_secs()] {
        let dir = temp_dir(&format!("skewfloor-{expires_in}"));
        seed_expired_token(&dir);
        let refresher = Arc::new(LifetimeRefresher::new(expires_in));
        let mgr = TokenManager::new(dir.to_path_buf(), Arc::clone(&refresher) as Arc<_>)
            .with_clock(Arc::new(FixedClock::new(now)));

        assert_eq!(mgr.access_token().expect("first"), "NEW");
        // 회전은 정상적으로 보존됐다 — 값을 조작하지 않는다.
        let v = disk_json(&dir);
        assert_eq!(v["refresh_token"], serde_json::json!("NA_ROTATED"));
        assert_eq!(
            v["expires_at"],
            serde_json::json!(now + expires_in),
            "expires_in={expires_in}: 보고된 수명이 그대로 기록돼야 한다(하한 없음)"
        );

        // A3 Task 5 이후: 두 번째 호출은 **또 회전하지 않는다**.
        // MIN_REFRESH_INTERVAL(60초) 게이트가 막고, 캐시(=이미 stale)를
        // 돌려주는 대신 원인을 지목하는 에러를 낸다. A2 가 이 자리에 남긴
        // 인계 사항(`MAX_TOKEN_LIFETIME` doc: "안전한 해법은 … 회전 자체에
        // 최소 간격을 두거나 …")이 그대로 실행된 결과다.
        //
        // ⚠️ 위의 `expires_at == now + expires_in` 단언이 그대로 통과한다는
        //    점이 중요하다 — 금지된 `max` 하한이 아니라는 증거다. 수명 값은
        //    서버가 준 그대로이고, 억제된 것은 **회전의 빈도**뿐이다.
        //    그 단언을 지우면 이 테스트는 두 해법(안전한 간격 vs 금지된 하한)을
        //    구별하지 못하게 된다 — 절대 지우지 말 것.
        let err = mgr
            .access_token()
            .expect_err("expires_in={expires_in}: 간격 게이트가 막아야 한다");
        assert!(
            err.to_string().contains("rotation interval"),
            "expires_in={expires_in}: 간격이 아닌 다른 이유로 실패했다: {err}"
        );
        assert_eq!(
            refresher.call_count(),
            1,
            "expires_in={expires_in}: 간격 안에서 체인을 한 번 더 회전시켰다 — \
             A2 가 A3 에 넘긴 폭주가 그대로 남아 있다"
        );
    }
}

#[test]
fn a_normal_reported_lifetime_is_recorded_verbatim() {
    // 반대편. 무조건 상한을 쓰는 구현(=수명 인자 무시)을 배제한다.
    let dir = temp_dir("normallifetime");
    seed_expired_token(&dir);
    let now = 1_000_000_u64;
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::new(LifetimeRefresher::new(28_800)))
        .with_clock(Arc::new(FixedClock::new(now)));

    assert_eq!(mgr.access_token().expect("token"), "NEW");
    assert_eq!(
        disk_json(&dir)["expires_at"],
        serde_json::json!(now + 28_800),
        "정상 수명이 상한이나 다른 값으로 바뀌었다"
    );
}

// --- 락 파일 불멸 (REQ-15) --------------------------------------------

#[test]
fn the_lock_file_survives_every_path_through_access_token() {
    // REQ-15: 락 파일은 성공·오류 어디에서도 삭제되지 않는다. 지우면 옛 inode 를
    // 쥔 프로세스와 새 inode 를 쥐는 프로세스가 동시에 살아 상호배제가 조용히
    // 사라진다. inode 까지 보는 이유는 `exists()` 만으로는 "지우고 다시 만든다"
    // 는 구현을 통과시키기 때문이다(lock.rs 가 같은 이유로 inode 를 본다).
    use std::os::unix::fs::MetadataExt as _;
    let ino_of = |p: &Path| std::fs::metadata(p).expect("lock file must exist").ino();

    let dir = temp_dir("lockimmortal");
    let path = lock_path(&dir);

    // (a) 성공 경로.
    seed_expired_token(&dir);
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::new(CountingRefresher::new("NEW")));
    mgr.access_token().expect("success path");
    let ino = ino_of(&path);

    // (b) refresh 실패 경로.
    seed_expired_token(&dir);
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::new(FailingRefresher::new()));
    mgr.access_token().expect_err("refresh 가 실패해야 한다");
    assert_eq!(
        ino_of(&path),
        ino,
        "refresh 실패 경로가 락 파일을 갈아치웠다"
    );

    // (c) 토큰 파일이 사라진 경로(디스크 읽기 실패).
    std::fs::remove_file(token_path(&dir)).expect("remove token file");
    mgr.access_token().expect_err("토큰이 없다");
    assert_eq!(ino_of(&path), ino, "읽기 실패 경로가 락 파일을 갈아치웠다");
}

/// 갱신 중에 panic 하는 refresher 입니다(스펙 §0 I1).
struct PanickingRefresher;

impl TokenRefresher for PanickingRefresher {
    fn refresh(&self, _refresh_token: &str) -> Result<Refreshed, Error> {
        panic!("refresher blew up mid-rotation");
    }
}

/// 🔴 refresher 가 panic 해도 다른 호출자가 `refreshing` 깃발에 갇히지 않고, 더 회전하지 않아야 합니다.
///
/// panic 한 시점에 서버가 회전을 마쳤는지 모르므로 옛 refresh 토큰으로 다시 회전하면 체인을 확정적으로
/// 깹니다. 그래서 다음 호출은 `state_deadline` 안에 「회전 금지」 오류를 받아야 하고, 디스크의 토큰 파일은
/// 그대로여야 합니다. 음성 대조: `State::recover` 에서 `refresh_forbidden = true` 를 지우면 둘째 호출이
/// 다시 refresher 에 들어가 panic 합니다.
#[test]
fn a_panicking_refresher_forbids_refresh_and_does_not_block_others() {
    let dir = temp_dir("panicking-refresher");
    seed_expired_token(&dir);
    let before = disk_json(&dir);
    let mgr = TokenManager::new(dir.to_path_buf(), Arc::new(PanickingRefresher) as Arc<_>)
        .with_clock(Arc::new(FixedClock::new(T0)) as Arc<_>)
        .with_lock_deadline(Duration::from_secs(3))
        .with_state_deadline(Duration::from_secs(2));

    let first = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| mgr.access_token()));
    assert!(
        first.is_err(),
        "the refresher's panic should propagate to its caller"
    );

    let started = std::time::Instant::now();
    let second = mgr.access_token();
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "second caller waited out the state deadline: {:?}",
        started.elapsed()
    );
    let msg = second.expect_err("refresh must stay forbidden").to_string();
    // 저장 실패가 아니라 중단이 원인이라고 말해야 운영자가 디스크를 뒤지지 않습니다(최종 검토 M-3).
    assert!(msg.contains("was interrupted"), "실제: {msg}");
    assert!(!msg.contains("could not be persisted"), "실제: {msg}");
    assert_eq!(disk_json(&dir), before, "the token file must be untouched");
}

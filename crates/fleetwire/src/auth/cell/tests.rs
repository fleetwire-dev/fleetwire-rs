use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::*;

#[derive(Default)]
struct S {
    value: u32,
    recovered: u32,
}

impl Recover for S {
    fn recover(&mut self) {
        self.recovered += 1;
    }
}

#[test]
fn n_threads_share_one_refresh() {
    let cell = StateCell::new(S::default());
    let refreshes = AtomicUsize::new(0);
    let started = Instant::now();
    std::thread::scope(|s| {
        for _ in 0..8 {
            s.spawn(|| {
                let g = cell.lock_within(Duration::from_secs(5)).unwrap();
                if g.value == 0 {
                    let t = g.begin_refresh();
                    refreshes.fetch_add(1, Ordering::SeqCst);
                    std::thread::sleep(Duration::from_millis(100));
                    t.finish(|s| s.value = 1);
                }
            });
        }
    });
    // 음성 대조: lock_within 이 refreshing 을 기다리지 않으면 2 이상이 됩니다.
    assert_eq!(refreshes.load(Ordering::SeqCst), 1);
    // 기다리던 쪽은 `finish` 의 깨우기로 곧바로 풀려야 합니다. 음성 대조: `notify_all` 을 지우면
    // 대기자가 마감(5초)까지 자다가 깨어 이 단언이 실패합니다(최종 검토 I-1).
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "waiters were not woken by finish: {:?}",
        started.elapsed()
    );
}

#[test]
fn a_waiter_past_the_deadline_gets_the_lock_timeout_error() {
    let cell = StateCell::new(S::default());
    let t = cell
        .lock_within(Duration::from_secs(1))
        .unwrap()
        .begin_refresh();
    let start = Instant::now();
    assert!(cell.lock_within(Duration::from_millis(100)).is_err());
    assert!(start.elapsed() < Duration::from_millis(500));
    t.finish(|_| {});
}

#[test]
fn a_panicking_refresh_forbids_further_refresh_and_wakes_waiters() {
    let cell = StateCell::new(S::default());
    let started = std::sync::Barrier::new(2);
    std::thread::scope(|s| {
        s.spawn(|| {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let t = cell
                    .lock_within(Duration::from_secs(1))
                    .unwrap()
                    .begin_refresh();
                started.wait();
                std::thread::sleep(Duration::from_millis(50));
                let _keep = &t;
                panic!("refresher blew up");
            }));
        });
        s.spawn(|| {
            started.wait();
            let t = Instant::now();
            let g = cell
                .lock_within(Duration::from_secs(5))
                .expect("waiter should be woken, not stuck");
            assert_eq!(g.recovered, 1);
            // 음성 대조: Drop 의 `notify_all` 을 지우면 마감(5초)까지 잡니다(최종 검토 I-1).
            assert!(
                t.elapsed() < Duration::from_secs(2),
                "waiter was not woken by the dropped ticket: {:?}",
                t.elapsed()
            );
        });
    });
}

#[test]
fn poisoned_mutex_is_recovered_not_propagated() {
    let cell = StateCell::new(S::default());
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _g = cell.lock_within(Duration::from_secs(1)).unwrap();
        panic!("poison");
    }));
    let g = cell.lock_within(Duration::from_secs(1)).unwrap();
    assert_eq!(g.recovered, 1);
}

//! Time injection — the boundary that lets expiry decisions be tested without a real sleep.
//!
//! A2 Task 7 ([`crate::TokenManager`]) re-exports this module at the crate root and hooks it
//! into the refresh decision and the `expires_at` calculation, so every `dead_code` attribute
//! that Task 3 had attached is gone. That was exactly the purpose of using `expect` rather than
//! `allow` — the moment an item comes alive, the compiler forces the attribute's removal. (The
//! last module that still needs that convention is [`crate::auth::jwt`], and the three pitfalls
//! are now in that module's documentation.)

/// A source of the current time.
///
/// If token expiry decisions depend directly on the wall clock, tests have to really sleep or
/// touch the system clock. This trait breaks that coupling.
///
/// The `Send + Sync` supertraits exist so that a clock can be held in a shared client
/// (`Arc<dyn Clock>`) — [`crate::TokenManager`] uses it in that form.
pub trait Clock: Send + Sync {
    /// Returns the seconds elapsed since the Unix epoch.
    ///
    /// Why seconds rather than a `SystemTime`: `expires_at` is stored as unix seconds (stable
    /// JSON), so matching the unit removes the conversion needed on every comparison, together
    /// with its error path.
    fn now_unix(&self) -> u64;
}

/// The default [`Clock`], backed by the system wall clock.
///
/// It is `#[non_exhaustive]`, so it cannot be built with a unit literal outside the crate —
/// the external construction path is [`SystemClock::default()`](Default::default).
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix(&self) -> u64 {
        secs_since_epoch(std::time::SystemTime::now())
    }
}

/// Converts a `SystemTime` to seconds elapsed since the unix epoch, or [`u64::MAX`] if it is
/// before the epoch.
///
/// **Why `u64::MAX` and not 0 (Fix round 1).** Not panicking is not enough; what matters is
/// *in which direction* the value is wrong on failure. The expiry check is
/// `expires_at > now + skew`, so **the smaller `now` is, the fresher a token looks**.
/// Returning 0 would keep judging a real token (`expires_at ≈ 1.7e9`) fresh via
/// `1_700_000_000 > 60`, so for as long as the clock is before the epoch the client would
/// never refresh proactively and would send a dead access token on every call — the exact
/// opposite of the safe direction. With `u64::MAX`, `expires_at > u64::MAX` is false for every
/// token, so it always reads as "expired → refresh", and because the `saturating_add` in
/// `is_fresh` already saturates there, no overflow occurs either.
pub(crate) fn secs_since_epoch(t: std::time::SystemTime) -> u64 {
    t.duration_since(std::time::UNIX_EPOCH)
        .map_or(u64::MAX, |d| d.as_secs())
}

#[cfg(test)]
pub(crate) use test_clock::FixedClock;

#[cfg(test)]
mod test_clock {
    use super::Clock;
    use std::sync::atomic::{AtomicU64, Ordering};

    /// A manual clock for tests.
    ///
    /// It is test-only (`#[cfg(test)]`) — a mutable clock is not put into the public API.
    pub(crate) struct FixedClock(AtomicU64);

    impl FixedClock {
        pub(crate) fn new(now: u64) -> Self {
            Self(AtomicU64::new(now))
        }

        pub(crate) fn advance(&self, secs: u64) {
            self.0.fetch_add(secs, Ordering::SeqCst);
        }

        /// Moves the time to an arbitrary value — it can move **backwards** too.
        ///
        /// [`FixedClock::advance`] alone cannot stage a clock going backwards (`fetch_add`
        /// only goes forward). Going backwards is a condition the `MIN_REFRESH_INTERVAL`
        /// gate handles explicitly (`now.checked_sub(last) == None`), so reaching that path
        /// needs a seam that hands down an absolute time. A setter was added instead of
        /// changing `advance` to take an `i64` in order to leave the unsigned meaning of
        /// every existing call site as it is.
        pub(crate) fn set(&self, now: u64) {
            self.0.store(now, Ordering::SeqCst);
        }
    }

    impl Clock for FixedClock {
        fn now_unix(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }
}

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

    #[test]
    fn system_clock_is_constructible_without_a_unit_literal() {
        // `#[non_exhaustive]` 는 크레이트 밖에서 `SystemClock` 리터럴 생성을 막는다.
        // 밖에서 쓸 수 있는 생성 경로는 `Default` 뿐이므로 그것이 살아있는지 고정한다.
        let a = SystemClock::default().now_unix();
        let b = SystemClock.now_unix();
        assert!(a.abs_diff(b) <= 1, "default={a} literal={b}");
    }

    #[test]
    fn pre_epoch_time_saturates_high_not_low() {
        // 이 분기는 Fix round 1 이전까지 테스트가 없었고, 그래서 값이 뒤집힌 채
        // 살아남았다. `is_fresh` 는 `expires_at > now + skew` 이므로 **작은 now 가
        // 토큰을 더 신선해 보이게** 만든다 — 0 은 "항상 fresh"(죽은 토큰을 계속
        // 실어 보냄)라는 정반대 방향이다.
        let pre_epoch = std::time::UNIX_EPOCH - std::time::Duration::from_secs(1);
        assert_eq!(
            secs_since_epoch(pre_epoch),
            u64::MAX,
            "epoch 이전 시각은 u64::MAX(=모든 토큰 만료)로 포화해야 한다"
        );
    }

    #[test]
    fn secs_since_epoch_agrees_with_std_for_a_normal_time() {
        // 포화 분기만 보면 항상 u64::MAX 를 돌려주는 구현도 통과한다.
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        assert_eq!(secs_since_epoch(t), 1_700_000_000);
    }

    #[test]
    fn clock_is_shareable_across_threads_as_a_trait_object() {
        // `TokenManager` 는 시계를 공유 클라이언트에 담는다. 그걸 합법으로 만드는 건
        // `Clock` 의 `Send + Sync` 상위 trait 이며, 떼면 여기서 컴파일이 깨진다.
        let clock: std::sync::Arc<dyn Clock> = std::sync::Arc::new(SystemClock);
        let seen = std::thread::spawn(move || clock.now_unix())
            .join()
            .expect("clock thread panicked");
        assert!(seen > 1_700_000_000, "wall clock looks unset: {seen}");
    }
}

//! 429-learning backoff for the NOWPayments API.
//!
//! Ported from `gifthub-rust-payments/src/ratelimit.rs`, reduced to a single provider.
//! NOWPayments doesn't document a rate limit or return structured quota info, so this
//! honours the standard `Retry-After` header when one is sent and falls back to
//! exponential backoff when it isn't.
//!
//! State is in-memory only. A restart re-learns within a request or two, which is far
//! cheaper than persisting it — and the poller already rebuilds all of its state from
//! the database on every tick.

use std::time::{Duration, Instant};

/// Never back off longer than this, however many 429s arrive in a row.
const MAX_BACKOFF: Duration = Duration::from_secs(300);

/// Base delay for the first 429 that arrives without a `Retry-After`.
const BASE_BACKOFF_SECS: u64 = 2;

#[derive(Debug, Default)]
pub struct RateLimiter {
    blocked_until: Option<Instant>,
    consecutive_429s: u32,
}

impl RateLimiter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether we're currently clear to call the API.
    pub fn is_ready(&self) -> bool {
        match self.blocked_until {
            Some(deadline) => Instant::now() >= deadline,
            None => true,
        }
    }

    /// A call succeeded — clear the streak so the next 429 starts from the base delay
    /// again. Deliberately does not lift a still-future `blocked_until`: succeeding
    /// only means we were let through, not that the limit was raised.
    pub fn record_success(&mut self) {
        self.consecutive_429s = 0;
    }

    /// A 429 came back. Honours the provider's own `Retry-After` when present,
    /// otherwise backs off 2s, 4s, 8s… capped at `MAX_BACKOFF`. A fresh 429 while
    /// already backing off pushes the deadline further out rather than resetting it,
    /// so a sustained rate-limit storm doesn't thrash.
    pub fn record_429(&mut self, retry_after: Option<Duration>) {
        self.consecutive_429s = self.consecutive_429s.saturating_add(1);
        let backoff = retry_after.unwrap_or_else(|| {
            let secs = BASE_BACKOFF_SECS
                .saturating_pow(self.consecutive_429s.min(8))
                .min(MAX_BACKOFF.as_secs());
            Duration::from_secs(secs)
        });
        eprintln!(
            "⏳ NOWPayments rate limited — backing off {:?} (consecutive 429s: {})",
            backoff, self.consecutive_429s
        );
        self.blocked_until = Some(Instant::now() + backoff);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_by_default_and_blocked_after_429() {
        let mut rl = RateLimiter::new();
        assert!(rl.is_ready());
        rl.record_429(Some(Duration::from_secs(60)));
        assert!(!rl.is_ready());
    }

    #[test]
    fn honors_explicit_retry_after_exactly() {
        let mut rl = RateLimiter::new();
        rl.record_429(Some(Duration::from_millis(10)));
        std::thread::sleep(Duration::from_millis(25));
        assert!(rl.is_ready());
    }

    #[test]
    fn exponential_backoff_grows_without_explicit_retry_after() {
        let mut rl = RateLimiter::new();
        rl.record_429(None); // 2s
        let first = rl.blocked_until.unwrap();
        rl.record_429(None); // 4s — strictly later than the first deadline
        assert!(rl.blocked_until.unwrap() > first);
    }

    #[test]
    fn success_resets_the_streak_but_not_an_active_block() {
        let mut rl = RateLimiter::new();
        rl.record_429(Some(Duration::from_secs(60)));
        rl.record_success();
        assert!(!rl.is_ready(), "success must not lift a future deadline");
        assert_eq!(rl.consecutive_429s, 0);
    }

    #[test]
    fn backoff_is_capped() {
        let mut rl = RateLimiter::new();
        for _ in 0..20 {
            rl.record_429(None);
        }
        let remaining = rl.blocked_until.unwrap() - Instant::now();
        assert!(remaining <= MAX_BACKOFF, "backoff exceeded the cap");
    }
}

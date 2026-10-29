use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

// Simple per-user rate limiter using a token bucket: `MAX_REQUESTS` per
// `WINDOW_SECS` per user.

const MAX_REQUESTS: u32 = 15; // Max requests per window
const WINDOW_SECS: u64 = 60; // Window duration in seconds
const CLEANUP_THRESHOLD: usize = 1000; // Cleanup when map exceeds this size

struct RateLimitEntry {
    tokens: u32,
    last_update: Instant,
}

static RATE_LIMITER: LazyLock<Mutex<HashMap<i64, RateLimitEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Check if a user is rate limited. Returns Ok(()) if allowed, Err with seconds to wait if limited.
pub async fn check_rate_limit(user_id: i64) -> Result<(), u64> {
    let mut limiter = RATE_LIMITER.lock().await;
    let now = Instant::now();
    let window = Duration::from_secs(WINDOW_SECS);

    // Periodic cleanup of old entries to prevent memory bloat
    if limiter.len() > CLEANUP_THRESHOLD {
        limiter.retain(|_, entry| now.duration_since(entry.last_update) < window * 2);
    }

    let entry = limiter.entry(user_id).or_insert_with(|| RateLimitEntry {
        tokens: MAX_REQUESTS,
        last_update: now,
    });

    // Replenish tokens based on elapsed time
    let elapsed = now.duration_since(entry.last_update);
    let tokens_to_add =
        (elapsed.as_secs() as f64 / WINDOW_SECS as f64 * MAX_REQUESTS as f64) as u32;

    if tokens_to_add > 0 {
        entry.tokens = (entry.tokens + tokens_to_add).min(MAX_REQUESTS);
        entry.last_update = now;
    }

    // Check if user has tokens available
    if entry.tokens > 0 {
        entry.tokens -= 1;
        Ok(())
    } else {
        // Calculate seconds until next token
        let secs_per_token = WINDOW_SECS as f64 / MAX_REQUESTS as f64;
        let wait_secs = secs_per_token.ceil() as u64;
        Err(wait_secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_rate_limit_allows_initial_requests() {
        let result = check_rate_limit(99999).await;
        assert!(result.is_ok());
    }
}

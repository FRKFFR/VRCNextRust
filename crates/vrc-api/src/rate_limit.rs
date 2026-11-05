//! Shared token-bucket rate limiter for every VRChat REST call (RULES.md §3).
//!
//! One token per request, refilled at a steady rate and capped at a burst
//! size. `pause()` blocks *all* callers for a duration — used when the API
//! answers 429 with `Retry-After`. Backoff helpers add jitter so concurrent
//! clients do not retry in lockstep.

use std::sync::Mutex;
use std::time::Duration;

use tokio::time::{sleep, Instant};

/// Token-bucket rate limiter shared by all requests on a [`crate::Client`].
pub struct RateLimiter {
    rate_per_second: f64,
    burst: f64,
    state: Mutex<State>,
    rng: Mutex<u64>,
}

struct State {
    tokens: f64,
    last_refill: Instant,
    paused_until: Option<Instant>,
}

impl RateLimiter {
    /// `requests_per_second` tokens are refilled continuously, up to `burst`.
    pub fn new(requests_per_second: f64, burst: u32) -> Self {
        let seed = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.subsec_nanos() as u64)
            .unwrap_or(0x9e37_79b9_7f4a_7c15)
            | 1;
        Self {
            rate_per_second: requests_per_second.max(f64::MIN_POSITIVE),
            burst: (burst as f64).max(1.0),
            state: Mutex::new(State {
                tokens: (burst as f64).max(1.0),
                last_refill: Instant::now(),
                paused_until: None,
            }),
            rng: Mutex::new(seed),
        }
    }

    /// Wait until one request slot is available (and no global pause is in
    /// effect), then consume one token.
    pub async fn acquire(&self) {
        loop {
            let wait = {
                let mut state = self.lock();
                let now = Instant::now();
                let elapsed = now.saturating_duration_since(state.last_refill);
                state.tokens =
                    (state.tokens + elapsed.as_secs_f64() * self.rate_per_second).min(self.burst);
                state.last_refill = now;

                match state.paused_until {
                    Some(until) if until > now => until - now,
                    _ if state.tokens >= 1.0 => {
                        state.tokens -= 1.0;
                        return;
                    }
                    _ => Duration::from_secs_f64((1.0 - state.tokens) / self.rate_per_second),
                }
            };
            sleep(wait).await;
        }
    }

    /// Block all callers for `duration` (e.g. after a 429). Extends, never
    /// shortens, an existing pause.
    pub fn pause(&self, duration: Duration) {
        let until = Instant::now() + duration;
        let mut state = self.lock();
        if state.paused_until.is_none_or(|current| current < until) {
            state.paused_until = Some(until);
        }
    }

    /// Exponential backoff with jitter for attempt `n` (0-based): base 500 ms,
    /// doubled per attempt, capped at 30 s, scaled by a random factor in
    /// [1.0, 2.0).
    pub fn retry_delay(&self, attempt: u32) -> Duration {
        let base = Duration::from_millis(500).saturating_mul(1 << attempt.min(5));
        let base = base.min(Duration::from_secs(30));
        base.mul_f64(1.0 + self.next_f64())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// xorshift64* — decorrelates retries without a `rand` dependency.
    fn next_f64(&self) -> f64 {
        let mut rng = self.rng.lock().unwrap_or_else(|e| e.into_inner());
        let mut x = *rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        *rng = x;
        (x.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 11) as f64 / (1u64 << 53) as f64
    }
}

impl std::fmt::Debug for RateLimiter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RateLimiter")
            .field("rate_per_second", &self.rate_per_second)
            .field("burst", &self.burst)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn burst_then_steady_rate() {
        let limiter = RateLimiter::new(1.0, 3);
        let start = Instant::now();
        for _ in 0..3 {
            limiter.acquire().await;
        }
        assert_eq!(start.elapsed(), Duration::ZERO);
        limiter.acquire().await;
        assert!(start.elapsed() >= Duration::from_secs(1));
    }

    #[tokio::test(start_paused = true)]
    async fn pause_blocks_acquire() {
        let limiter = RateLimiter::new(1.0, 3);
        limiter.pause(Duration::from_secs(5));
        let start = Instant::now();
        limiter.acquire().await;
        assert!(start.elapsed() >= Duration::from_secs(5));
    }

    #[test]
    fn retry_delay_grows_and_stays_bounded() {
        let limiter = RateLimiter::new(1.0, 3);
        let first = limiter.retry_delay(0);
        let later = limiter.retry_delay(3);
        assert!(first >= Duration::from_millis(500) && first < Duration::from_secs(1));
        assert!(later >= Duration::from_secs(4) && later < Duration::from_secs(8));
        assert!(limiter.retry_delay(30) < Duration::from_secs(60));
    }
}

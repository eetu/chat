//! Per-key in-memory token-bucket rate limiter.
//!
//! Keyed by an opaque string — typically a `user_sub` for authenticated
//! routes or a client IP for the auth handshake. Buckets refill linearly
//! at `capacity / 60s`. Stale entries are reaped lazily once the map
//! grows past `SWEEP_THRESHOLD` so the limiter does not leak memory on a
//! long-running process facing many distinct IPs.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const SWEEP_THRESHOLD: usize = 1024;
const STALE_AFTER: Duration = Duration::from_secs(300);

#[derive(Clone, Copy)]
struct Bucket {
    tokens: f64,
    last: Instant,
}

pub struct RateLimiter {
    inner: Mutex<HashMap<String, Bucket>>,
    capacity: f64,
    refill_per_sec: f64,
}

impl RateLimiter {
    pub fn per_minute(rate: u32) -> Self {
        let cap = rate.max(1) as f64;
        Self {
            inner: Mutex::new(HashMap::new()),
            capacity: cap,
            refill_per_sec: cap / 60.0,
        }
    }

    /// Consume one token. Returns `true` if the request is allowed.
    pub fn check(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut map = self.inner.lock().expect("ratelimit lock poisoned");
        if map.len() > SWEEP_THRESHOLD {
            map.retain(|_, b| now.duration_since(b.last) < STALE_AFTER);
        }
        let bucket = map.entry(key.to_string()).or_insert(Bucket {
            tokens: self.capacity,
            last: now,
        });
        let elapsed = now.duration_since(bucket.last).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        bucket.last = now;
        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_up_to_capacity_then_blocks() {
        let rl = RateLimiter::per_minute(3);
        assert!(rl.check("user-a"));
        assert!(rl.check("user-a"));
        assert!(rl.check("user-a"));
        assert!(!rl.check("user-a"));
    }

    #[test]
    fn keys_are_isolated() {
        let rl = RateLimiter::per_minute(1);
        assert!(rl.check("user-a"));
        assert!(rl.check("user-b"));
        assert!(!rl.check("user-a"));
        assert!(!rl.check("user-b"));
    }

    #[test]
    fn refills_over_time() {
        let rl = RateLimiter {
            inner: Mutex::new(HashMap::new()),
            capacity: 1.0,
            refill_per_sec: 1000.0,
        };
        assert!(rl.check("k"));
        assert!(!rl.check("k"));
        std::thread::sleep(Duration::from_millis(5));
        assert!(rl.check("k"));
    }
}

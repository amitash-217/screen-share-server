use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Fixed-window counter keyed by string (IP or username).
pub struct RateLimiter {
    max: u32,
    window: Duration,
    hits: Mutex<HashMap<String, (u32, Instant)>>,
}

impl RateLimiter {
    pub fn new(max: u32, window: Duration) -> Self {
        Self { max, window, hits: Mutex::new(HashMap::new()) }
    }

    /// True when `key` has used up its attempts in the current window.
    pub fn blocked(&self, key: &str) -> bool {
        let hits = self.hits.lock().unwrap();
        matches!(hits.get(key), Some((n, start)) if *n >= self.max && start.elapsed() < self.window)
    }

    pub fn record(&self, key: &str) {
        let mut hits = self.hits.lock().unwrap();
        // Keep the map from growing without bound under a key-spraying attacker.
        if hits.len() > 10_000 {
            hits.retain(|_, (_, start)| start.elapsed() < self.window);
        }
        let entry = hits.entry(key.to_owned()).or_insert((0, Instant::now()));
        if entry.1.elapsed() >= self.window {
            *entry = (0, Instant::now());
        }
        entry.0 += 1;
    }

    pub fn clear(&self, key: &str) {
        self.hits.lock().unwrap().remove(key);
    }
}

pub struct Limiters {
    pub login_ip: RateLimiter,
    pub login_user: RateLimiter,
    pub signup_ip: RateLimiter,
    /// Join requests sent by one user; room codes are only 5 letters, so slow guessing down.
    pub join_request_user: RateLimiter,
}

impl Default for Limiters {
    fn default() -> Self {
        let quarter_hour = Duration::from_secs(15 * 60);
        Self {
            login_ip: RateLimiter::new(20, quarter_hour),
            login_user: RateLimiter::new(5, quarter_hour),
            signup_ip: RateLimiter::new(10, Duration::from_secs(60 * 60)),
            join_request_user: RateLimiter::new(10, Duration::from_secs(60)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_after_max_and_clears() {
        let rl = RateLimiter::new(2, Duration::from_secs(60));
        rl.record("a");
        assert!(!rl.blocked("a"));
        rl.record("a");
        assert!(rl.blocked("a"));
        assert!(!rl.blocked("b"));
        rl.clear("a");
        assert!(!rl.blocked("a"));
    }

    #[test]
    fn window_expiry_unblocks() {
        let rl = RateLimiter::new(1, Duration::from_millis(10));
        rl.record("a");
        assert!(rl.blocked("a"));
        std::thread::sleep(Duration::from_millis(20));
        assert!(!rl.blocked("a"));
    }
}

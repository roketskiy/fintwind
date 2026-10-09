//! Fixed-window rate limiters for auth attempts and per-connection frames.
//!
//! Deliberately trivial: a fixed window counter per key with a mutex. The
//! relay's job is to make brute force and flooding visibly expensive, not
//! to do smooth traffic shaping; a burst at a window boundary is acceptable
//! and the cap values already assume it.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

/// Hard bound on limiter keys. Registration is open by design, so the
/// key space is attacker-mintable; the map must not grow with it.
pub const RATE_LIMIT_MAX_KEYS: usize = 8192;

struct Window {
    /// Start of the current fixed window.
    started: Instant,
    count: u32,
}

/// Counts attempts per key inside a fixed window. Clone-friendly: the
/// state sits behind one mutex.
#[derive(Clone)]
pub struct RateLimiter {
    windows: Arc<Mutex<HashMap<String, Window>>>,
    limit: u32,
    window: Duration,
    max_keys: usize,
}

use std::sync::Arc;

impl RateLimiter {
    pub fn new(limit: u32, window: Duration) -> Self {
        Self::with_max_keys(limit, window, RATE_LIMIT_MAX_KEYS)
    }

    pub fn with_max_keys(limit: u32, window: Duration, max_keys: usize) -> Self {
        Self {
            windows: Arc::new(Mutex::new(HashMap::new())),
            limit,
            window,
            max_keys: max_keys.max(1),
        }
    }

    /// Record one attempt for `key` and report whether it is allowed.
    /// Allowed or not, the attempt counts — a rejected attempt still
    /// consumed the protected resource it targeted.
    pub fn admit(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut windows = self.windows.lock();
        if !windows.contains_key(key) {
            // A brand-new key must not push a live counter out of the
            // map: dropping expired entries first is free, and refusing
            // afterwards never resets anyone's window (an eviction-based
            // policy would hand an attacker a limit-bypass).
            if windows.len() >= self.max_keys {
                windows.retain(|_, window| now.duration_since(window.started) < self.window);
                if windows.len() >= self.max_keys {
                    return false;
                }
            }
        }
        let entry = windows.entry(key.to_owned()).or_insert(Window {
            started: now,
            count: 0,
        });
        if now.duration_since(entry.started) >= self.window {
            entry.started = now;
            entry.count = 0;
        }
        if entry.count >= self.limit {
            return false;
        }
        entry.count += 1;
        true
    }

    /// Drop entries whose window has closed. Called from the periodic
    /// sweep so a flood of one-shot keys cannot grow the map without
    /// bound; retaining only in-window entries is what keeps the bound
    /// self-healing inside one window.
    pub fn sweep(&self) {
        let now = Instant::now();
        self.windows
            .lock()
            .retain(|_, window| now.duration_since(window.started) < self.window);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ways limiting can fail: an unlimited flood passes, or a limit
    /// resets itself early and lets brute force continue.
    #[test]
    fn stops_at_limit_within_window() {
        let limiter = RateLimiter::new(3, Duration::from_secs(60));
        assert!(limiter.admit("k"));
        assert!(limiter.admit("k"));
        assert!(limiter.admit("k"));
        assert!(!limiter.admit("k"));
        // A different key has its own window.
        assert!(limiter.admit("other"));
    }

    #[test]
    fn closed_windows_are_swept() {
        let limiter = RateLimiter::new(1, Duration::from_millis(1));
        assert!(limiter.admit("k"));
        std::thread::sleep(Duration::from_millis(3));
        limiter.sweep();
        assert!(limiter.windows.lock().is_empty());
    }
}

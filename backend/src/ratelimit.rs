use std::{collections::HashMap, sync::Mutex, time::{Duration, Instant}};

/// In-memory sliding-window limiter (per process). Use Redis or similar if you run several instances.
#[derive(Default)]
pub struct RateLimiter {
    hits: Mutex<HashMap<String, Vec<Instant>>>,
}

impl RateLimiter {
    /// Allow at most `max` hits per `window` for `key`.
    pub fn allow(&self, key: &str, max: usize, window: Duration) -> bool {
        let mut m = self.hits.lock().unwrap_or_else(|e| e.into_inner());
        let t = Instant::now();
        if m.len() > 20_000 {
            m.retain(|_, v| v.last().is_some_and(|x| t.duration_since(*x) < Duration::from_secs(3600)));
        }
        let v = m.entry(key.to_string()).or_default();
        v.retain(|x| t.duration_since(*x) < window);
        if v.len() >= max { return false; }
        v.push(t);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window() {
        let l = RateLimiter::default();
        for _ in 0..3 { assert!(l.allow("k", 3, Duration::from_secs(60))); }
        assert!(!l.allow("k", 3, Duration::from_secs(60)));
        assert!(l.allow("other", 3, Duration::from_secs(60)));
    }
}

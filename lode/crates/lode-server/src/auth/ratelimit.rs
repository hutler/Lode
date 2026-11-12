//! In-memory limit on failed logins, per name and per client address.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;

/// Limits on failed logins.
#[derive(Debug, Clone, Copy)]
pub struct LoginPolicy {
    /// Failures for one name within the window that lock that name.
    pub max_failures_per_name: u32,
    /// Failures from one address within the window that lock that address.
    pub max_failures_per_addr: u32,
    /// Window length, and how long a lockout lasts from the first counted failure.
    pub window_ms: i64,
}

impl Default for LoginPolicy {
    fn default() -> Self {
        Self {
            max_failures_per_name: 5,
            max_failures_per_addr: 20,
            window_ms: 15 * 60 * 1000,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct Window {
    start: i64,
    failures: u32,
}

/// Tracks failed logins. Entries expire with their window; memory stays bounded by the
/// number of distinct names and addresses seen within one window.
#[derive(Debug, Default)]
pub struct LoginLimiter {
    policy: LoginPolicy,
    by_name: Mutex<HashMap<String, Window>>,
    by_addr: Mutex<HashMap<IpAddr, Window>>,
}

impl LoginLimiter {
    /// A limiter with this policy.
    pub fn new(policy: LoginPolicy) -> Self {
        Self {
            policy,
            by_name: Mutex::default(),
            by_addr: Mutex::default(),
        }
    }

    /// If the name or address is locked, the milliseconds until it unlocks.
    pub fn locked_for(&self, name: &str, addr: Option<IpAddr>, now: i64) -> Option<i64> {
        let w = self.policy.window_ms;
        let by_name = lock(&self.by_name)
            .get(name)
            .copied()
            .filter(|e| now - e.start < w && e.failures >= self.policy.max_failures_per_name)
            .map(|e| e.start + w - now);
        let by_addr = addr.and_then(|a| {
            lock(&self.by_addr)
                .get(&a)
                .copied()
                .filter(|e| now - e.start < w && e.failures >= self.policy.max_failures_per_addr)
                .map(|e| e.start + w - now)
        });
        by_name.max(by_addr)
    }

    /// Records a failed attempt.
    pub fn record_failure(&self, name: &str, addr: Option<IpAddr>, now: i64) {
        let w = self.policy.window_ms;
        {
            let mut map = lock(&self.by_name);
            map.retain(|_, e| now - e.start < w);
            bump(map.entry(name.to_owned()).or_insert(Window {
                start: now,
                failures: 0,
            }));
        }
        if let Some(a) = addr {
            let mut map = lock(&self.by_addr);
            map.retain(|_, e| now - e.start < w);
            bump(map.entry(a).or_insert(Window {
                start: now,
                failures: 0,
            }));
        }
    }

    /// Clears the name's failures after a successful login. The address keeps its count, so
    /// one valid account cannot be used to reset guessing against others.
    pub fn record_success(&self, name: &str) {
        lock(&self.by_name).remove(name);
    }
}

fn bump(e: &mut Window) {
    e.failures = e.failures.saturating_add(1);
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // The maps hold counters only; a panic while holding the lock cannot leave them in a
    // state worse than a missed count, so a poisoned lock is recovered.
    m.lock().unwrap_or_else(|p| p.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locks_after_max_failures_and_recovers_after_window() {
        let l = LoginLimiter::new(LoginPolicy {
            max_failures_per_name: 3,
            max_failures_per_addr: 100,
            window_ms: 1000,
        });
        for t in 0..3 {
            assert!(l.locked_for("ana", None, t).is_none());
            l.record_failure("ana", None, t);
        }
        assert_eq!(l.locked_for("ana", None, 10), Some(990));
        assert!(l.locked_for("bob", None, 10).is_none());
        assert!(l.locked_for("ana", None, 1000).is_none());
    }

    #[test]
    fn address_limit_spans_names() {
        let addr: IpAddr = [10, 0, 0, 1].into();
        let other: IpAddr = [10, 0, 0, 2].into();
        let l = LoginLimiter::new(LoginPolicy {
            max_failures_per_name: 100,
            max_failures_per_addr: 2,
            window_ms: 1000,
        });
        l.record_failure("a", Some(addr), 0);
        l.record_failure("b", Some(addr), 1);
        assert!(l.locked_for("c", Some(addr), 2).is_some());
        assert!(l.locked_for("c", Some(other), 2).is_none());
        l.record_success("c");
        assert!(l.locked_for("c", Some(addr), 2).is_some());
    }
}

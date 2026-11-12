//! When and how long to wait before trying a request again.

use std::time::Duration;

/// Retry schedule for idempotent requests and commits: exponential backoff between attempts.
/// 4xx responses are never retried.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Attempts in total, the first one included. `1` disables retries.
    pub max_attempts: u32,
    /// Wait before the second attempt; doubled before each further one.
    pub initial_delay: Duration,
    /// Longest single wait, also the cap on a server's `Retry-After`.
    pub max_delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            max_attempts: 4,
            initial_delay: Duration::from_millis(250),
            max_delay: Duration::from_secs(8),
        }
    }
}

impl RetryPolicy {
    /// One attempt only.
    pub fn none() -> Self {
        Self {
            max_attempts: 1,
            ..Self::default()
        }
    }

    /// The wait after failed attempt number `attempt` (1-based). A `Retry-After` from the
    /// server wins when given, within `max_delay`.
    pub(crate) fn delay(&self, attempt: u32, retry_after: Option<Duration>) -> Duration {
        if let Some(d) = retry_after {
            return d.min(self.max_delay);
        }
        let shift = attempt.saturating_sub(1).min(16);
        self.initial_delay
            .saturating_mul(1u32 << shift)
            .min(self.max_delay)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_up_to_the_cap() {
        let p = RetryPolicy {
            max_attempts: 10,
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(500),
        };
        let waits: Vec<u128> = (1..=5).map(|a| p.delay(a, None).as_millis()).collect();
        assert_eq!(waits, vec![100, 200, 400, 500, 500]);
        assert_eq!(p.delay(1, Some(Duration::from_secs(60))).as_millis(), 500);
        assert_eq!(p.delay(1, Some(Duration::from_millis(30))).as_millis(), 30);
    }
}

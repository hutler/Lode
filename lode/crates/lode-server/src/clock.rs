//! Injected time source, so tests can control expiry, rate limits and retries.

use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

use time::OffsetDateTime;

/// A source of the current time.
pub trait Clock: Send + Sync + 'static {
    /// The current time in UTC.
    fn now(&self) -> OffsetDateTime;

    /// The current time in milliseconds since the Unix epoch, the unit the metadata store uses.
    fn now_ms(&self) -> i64 {
        let ns = self.now().unix_timestamp_nanos() / 1_000_000;
        i64::try_from(ns).unwrap_or(i64::MAX)
    }
}

/// Shared handle to a clock.
pub type SharedClock = Arc<dyn Clock>;

/// The real wall clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> OffsetDateTime {
        OffsetDateTime::now_utc()
    }
}

/// A clock that only moves when told to. For tests.
#[derive(Debug, Clone)]
pub struct ManualClock {
    ms: Arc<AtomicI64>,
}

impl ManualClock {
    /// A clock set to `ms` milliseconds since the epoch.
    pub fn new(ms: i64) -> Self {
        Self {
            ms: Arc::new(AtomicI64::new(ms)),
        }
    }

    /// Moves the clock forward by `ms` milliseconds.
    pub fn advance_ms(&self, ms: i64) {
        self.ms.fetch_add(ms, Ordering::SeqCst);
    }

    /// Sets the clock.
    pub fn set_ms(&self, ms: i64) {
        self.ms.store(ms, Ordering::SeqCst);
    }
}

impl Clock for ManualClock {
    fn now(&self) -> OffsetDateTime {
        let ms = self.ms.load(Ordering::SeqCst);
        OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
            .unwrap_or(OffsetDateTime::UNIX_EPOCH)
    }

    fn now_ms(&self) -> i64 {
        self.ms.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_moves_only_when_told() {
        let c = ManualClock::new(1_000);
        assert_eq!(c.now_ms(), 1_000);
        c.advance_ms(500);
        assert_eq!(c.now_ms(), 1_500);
        assert_eq!(c.now().unix_timestamp(), 1);
        c.set_ms(60_000);
        assert_eq!(c.now().unix_timestamp(), 60);
    }
}

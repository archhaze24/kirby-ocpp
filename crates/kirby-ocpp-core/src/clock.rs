//! Wall-clock abstraction.
//!
//! [`Clock`] supplies **wall-clock time only** (timestamps put on the wire and
//! in persisted state, reservation expiry, clock-aligned meter values). It is
//! injected into the station so tests control "what time is it" without
//! touching the OS clock.
//!
//! Timers (heartbeat, reconnect backoff, call timeouts, retry delays) are
//! **not** part of this trait: they use `tokio::time` directly. Tests of pure
//! scheduling logic (backoff, debounce, periodic meter values) may use
//! `tokio::time::pause()`/`advance()`; tests that involve real sockets must use
//! real, short timeouts instead, because paused time and socket I/O do not mix.

use std::fmt::Debug;
use std::sync::{Arc, Mutex, PoisonError};

use chrono::{DateTime, Duration, Utc};

/// Source of the current wall-clock time.
pub trait Clock: Send + Sync + Debug + 'static {
    /// Current time in UTC.
    fn now_utc(&self) -> DateTime<Utc>;
}

/// The real system clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_utc(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// A clock that only moves when told to. Cheap to clone; clones share time.
#[derive(Debug, Clone)]
pub struct ManualClock {
    now: Arc<Mutex<DateTime<Utc>>>,
}

impl ManualClock {
    /// Creates a clock frozen at `start`.
    pub fn new(start: DateTime<Utc>) -> Self {
        Self {
            now: Arc::new(Mutex::new(start)),
        }
    }

    /// Moves the clock forward (or backward for a negative `by`).
    pub fn advance(&self, by: Duration) {
        let mut now = self.now.lock().unwrap_or_else(PoisonError::into_inner);
        *now += by;
    }

    /// Jumps the clock to an absolute time.
    pub fn set(&self, to: DateTime<Utc>) {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner) = to;
    }
}

impl Clock for ManualClock {
    fn now_utc(&self) -> DateTime<Utc> {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::*;

    #[test]
    fn manual_clock_is_frozen_until_advanced_and_clones_share_time() {
        let start = Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap();
        let clock = ManualClock::new(start);
        let clone = clock.clone();
        assert_eq!(clock.now_utc(), start);
        assert_eq!(clock.now_utc(), start);

        clone.advance(Duration::seconds(90));
        assert_eq!(clock.now_utc(), start + Duration::seconds(90));

        clock.set(start);
        assert_eq!(clone.now_utc(), start);
    }

    #[test]
    fn system_clock_is_close_to_utc_now() {
        let delta = (SystemClock.now_utc() - Utc::now()).num_seconds().abs();
        assert!(delta <= 1);
    }
}

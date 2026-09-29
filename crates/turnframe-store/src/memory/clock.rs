//! The clock a store stamps its own columns with.
//!
//! A store owns a few timestamps the caller does not supply: when a phase
//! marker was last written, when a journal outcome was recorded, when a commit
//! landed. Reading the wall clock for those makes tests that involve expiry or
//! ordering flaky, so the in-memory store takes its time from a [`Clock`].
//!
//! Use [`SystemClock`] in production and [`ManualClock`] in tests: nothing
//! moves until the test moves it, so "the card expired" is an assertion rather
//! than a sleep.

use std::fmt;
use std::sync::Mutex;

use chrono::{DateTime, TimeDelta, Utc};

/// Source of the current time for a store.
///
/// Implementations must be cheap and must never block: a store calls
/// [`Clock::now`] while it holds its internal lock.
pub trait Clock: Send + Sync + fmt::Debug {
    /// The current instant, in UTC.
    fn now(&self) -> DateTime<Utc>;
}

/// The wall clock.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// A clock that only moves when a test moves it.
///
/// ```rust
/// use chrono::{TimeDelta, Utc};
/// use turnframe_store::memory::{Clock, ManualClock};
///
/// let clock = ManualClock::new(Utc::now());
/// let start = clock.now();
/// clock.advance(TimeDelta::seconds(30));
/// assert_eq!(clock.now(), start + TimeDelta::seconds(30));
/// ```
#[derive(Debug)]
pub struct ManualClock {
    now: Mutex<DateTime<Utc>>,
}

impl ManualClock {
    /// A clock stopped at `start`.
    #[must_use]
    pub fn new(start: DateTime<Utc>) -> Self {
        Self {
            now: Mutex::new(start),
        }
    }

    /// A clock stopped at the Unix epoch, for tests that only care about order.
    #[must_use]
    pub fn epoch() -> Self {
        Self::new(DateTime::<Utc>::UNIX_EPOCH)
    }

    /// Moves the clock to `instant`, forwards or backwards.
    ///
    /// A poisoned lock is ignored: the clock is a test fixture and losing its
    /// value cannot corrupt anything a store relies on.
    pub fn set(&self, instant: DateTime<Utc>) {
        match self.now.lock() {
            Ok(mut guard) => *guard = instant,
            Err(poisoned) => *poisoned.into_inner() = instant,
        }
    }

    /// Moves the clock forward by `delta`.
    pub fn advance(&self, delta: TimeDelta) {
        let next = self.now() + delta;
        self.set(next);
    }
}

impl Default for ManualClock {
    fn default() -> Self {
        Self::epoch()
    }
}

impl Clock for ManualClock {
    fn now(&self) -> DateTime<Utc> {
        match self.now.lock() {
            Ok(guard) => *guard,
            Err(poisoned) => *poisoned.into_inner(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_is_stopped_until_moved() {
        let clock = ManualClock::epoch();
        assert_eq!(clock.now(), DateTime::<Utc>::UNIX_EPOCH);
        assert_eq!(clock.now(), clock.now());
        clock.advance(TimeDelta::hours(2));
        assert_eq!(
            clock.now(),
            DateTime::<Utc>::UNIX_EPOCH + TimeDelta::hours(2)
        );
        clock.set(DateTime::<Utc>::UNIX_EPOCH);
        assert_eq!(clock.now(), DateTime::<Utc>::UNIX_EPOCH);
    }

    #[test]
    fn system_clock_moves_forward() {
        let clock = SystemClock;
        assert!(clock.now() <= clock.now());
    }
}

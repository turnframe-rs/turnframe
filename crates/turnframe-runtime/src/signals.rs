//! Measuring a stage of the turn in the unit its [`Signal`] is recorded in (§28).
//!
//! The provider signals of every model call are the tasks engine's
//! (`turnframe_tasks`); labels anywhere are keys, purposes and closed codes, never text.

use std::time::{Duration, Instant};

use turnframe_core::observe::{Observer, Signal, SignalLabels};

/// Measures one stage, in the unit the signal it feeds is recorded in.
///
/// It reads [`Instant`] rather than the runtime's [`TurnClock`](crate::orchestrator::TurnClock)
/// on purpose: the clock is stopped in tests and may be stepped by a scenario,
/// and a latency histogram that reports whatever a fixture decided is worse
/// than none.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Stage {
    /// When the stage was entered.
    started: Instant,
}

impl Stage {
    /// Starts measuring.
    pub(crate) fn enter() -> Self {
        Self {
            started: Instant::now(),
        }
    }

    /// How long the stage has been running.
    pub(crate) fn elapsed(self) -> Duration {
        self.started.elapsed()
    }

    /// Reports the stage under `signal`, with `labels`.
    pub(crate) fn observe(self, observer: &dyn Observer, signal: Signal, labels: &SignalLabels) {
        observer.observe_duration(&signal, self.elapsed(), labels);
    }
}

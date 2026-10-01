//! What a turn's model calls may spend, and which bound stopped them.
//!
//! A call is reserved before it is sent, so parallel tasks cannot overshoot a
//! bound; tokens are counted from what providers report, after each call. A
//! bound that is reached stops new calls and is reported, never a partial effect:
//! what the caller does with an unfinished task is its own decision.

use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::{Semaphore, SemaphorePermit};
use turnframe_core::replay::BudgetReport;

/// The limits of one phase of a turn. Every bound is optional except parallelism.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[non_exhaustive]
pub struct Budget {
    /// Model calls, each reserved before it is sent.
    pub max_model_calls: Option<u32>,
    /// Dependent calls in a row; a repair, a vote round and an escalation count one.
    pub max_chain_depth: Option<u8>,
    /// Calls in flight at once.
    pub max_parallel: usize,
    /// Prompt tokens as providers report them; no call starts once reached.
    pub max_prompt_tokens: Option<u64>,
    /// Wall clock of the whole phase, in seconds.
    pub max_wall_clock_secs: Option<u64>,
    /// Deadline of one call, in seconds.
    pub per_call_timeout_secs: u64,
}

impl Budget {
    /// Understanding a message: 32 calls, depth 8, 6 in flight, 100k tokens, 30 s.
    #[must_use]
    pub const fn understanding() -> Self {
        Self {
            max_model_calls: Some(32),
            max_chain_depth: Some(8),
            max_parallel: 6,
            max_prompt_tokens: Some(100_000),
            max_wall_clock_secs: Some(30),
            per_call_timeout_secs: 20,
        }
    }

    /// Writing the reply: 12 calls, depth 4, 4 in flight, 40k tokens, 20 s.
    #[must_use]
    pub const fn narration() -> Self {
        Self {
            max_model_calls: Some(12),
            max_chain_depth: Some(4),
            max_parallel: 4,
            max_prompt_tokens: Some(40_000),
            max_wall_clock_secs: Some(20),
            per_call_timeout_secs: 20,
        }
    }

    /// No bound but parallelism and the per-call deadline: an explicit choice.
    #[must_use]
    pub const fn unbounded() -> Self {
        Self {
            max_model_calls: None,
            max_chain_depth: None,
            max_parallel: 6,
            max_prompt_tokens: None,
            max_wall_clock_secs: None,
            per_call_timeout_secs: 60,
        }
    }
}

impl Default for Budget {
    fn default() -> Self {
        Self::understanding()
    }
}

/// The bound that stopped a call.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum BudgetBound {
    /// `max_model_calls`.
    ModelCalls,
    /// `max_chain_depth`.
    ChainDepth,
    /// `max_prompt_tokens`.
    PromptTokens,
    /// `max_wall_clock_secs`.
    WallClock,
}

impl BudgetBound {
    /// Stable label, for records and metrics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ModelCalls => "model_calls",
            Self::ChainDepth => "chain_depth",
            Self::PromptTokens => "prompt_tokens",
            Self::WallClock => "wall_clock",
        }
    }
}

/// What one phase has spent so far. Shared by every task the phase runs.
#[derive(Debug)]
pub struct BudgetTracker {
    budget: Budget,
    started: Instant,
    calls: AtomicU32,
    tokens: AtomicU64,
    max_depth: AtomicU8,
    exhausted: Mutex<Option<BudgetBound>>,
    permits: Semaphore,
}

impl BudgetTracker {
    /// A tracker starting now.
    #[must_use]
    pub fn new(budget: Budget) -> Self {
        Self {
            budget,
            started: Instant::now(),
            calls: AtomicU32::new(0),
            tokens: AtomicU64::new(0),
            max_depth: AtomicU8::new(0),
            exhausted: Mutex::new(None),
            permits: Semaphore::new(budget.max_parallel.max(1)),
        }
    }

    /// The limits in force.
    #[must_use]
    pub const fn budget(&self) -> &Budget {
        &self.budget
    }

    /// Reserves one call at `depth`.
    ///
    /// # Errors
    ///
    /// The [`BudgetBound`] that forbids it; the bound is also remembered for the report.
    pub fn reserve(&self, depth: u8) -> Result<(), BudgetBound> {
        let refused = self.first_bound(depth);
        if let Some(bound) = refused {
            self.exhaust(bound);
            return Err(bound);
        }
        let limit = self.budget.max_model_calls.unwrap_or(u32::MAX);
        // Rust 1.99 renames this `try_update`, which the 1.88 MSRV does not have.
        #[allow(deprecated)]
        let reserved = self
            .calls
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |calls| {
                (calls < limit).then_some(calls + 1)
            });
        if reserved.is_err() {
            self.exhaust(BudgetBound::ModelCalls);
            return Err(BudgetBound::ModelCalls);
        }
        self.max_depth.fetch_max(depth, Ordering::SeqCst);
        Ok(())
    }

    fn first_bound(&self, depth: u8) -> Option<BudgetBound> {
        if self.remaining_wall_clock() == Some(Duration::ZERO) {
            return Some(BudgetBound::WallClock);
        }
        if self.budget.max_chain_depth.is_some_and(|max| depth > max) {
            return Some(BudgetBound::ChainDepth);
        }
        if self
            .budget
            .max_prompt_tokens
            .is_some_and(|max| self.tokens.load(Ordering::SeqCst) >= max)
        {
            return Some(BudgetBound::PromptTokens);
        }
        None
    }

    fn exhaust(&self, bound: BudgetBound) {
        let mut exhausted = self
            .exhausted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        exhausted.get_or_insert(bound);
    }

    /// Waits for a slot among the calls in flight.
    pub async fn permit(&self) -> Option<SemaphorePermit<'_>> {
        self.permits.acquire().await.ok()
    }

    /// Counts the prompt tokens a provider reported.
    pub fn record_tokens(&self, tokens: u64) {
        self.tokens.fetch_add(tokens, Ordering::SeqCst);
    }

    /// Time left on the wall clock, when it is bounded.
    #[must_use]
    pub fn remaining_wall_clock(&self) -> Option<Duration> {
        self.budget
            .max_wall_clock_secs
            .map(|secs| Duration::from_secs(secs).saturating_sub(self.started.elapsed()))
    }

    /// The deadline of the next call: the per-call timeout, cut to the time left.
    #[must_use]
    pub fn call_timeout(&self, requested: Option<Duration>) -> Duration {
        let per_call = Duration::from_secs(self.budget.per_call_timeout_secs);
        let wanted = requested.map_or(per_call, |requested| requested.min(per_call));
        match self.remaining_wall_clock() {
            Some(left) => wanted.min(left),
            None => wanted,
        }
    }

    /// The first bound reached, if any.
    #[must_use]
    pub fn exhausted(&self) -> Option<BudgetBound> {
        *self
            .exhausted
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// What was spent, for the replay record.
    #[must_use]
    pub fn report(&self) -> BudgetReport {
        let mut report = BudgetReport::default();
        report.model_calls = self.calls.load(Ordering::SeqCst);
        report.prompt_tokens = self.tokens.load(Ordering::SeqCst);
        report.max_depth = self.max_depth.load(Ordering::SeqCst);
        report.exhausted = self.exhausted().map(|bound| bound.as_str().to_owned());
        report
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calls_are_refused_once_the_bound_is_reached_and_the_bound_is_reported() {
        let tracker = BudgetTracker::new(Budget {
            max_model_calls: Some(2),
            ..Budget::understanding()
        });
        assert!(tracker.reserve(1).is_ok());
        assert!(tracker.reserve(2).is_ok());
        assert_eq!(tracker.reserve(1), Err(BudgetBound::ModelCalls));
        let report = tracker.report();
        assert_eq!(report.model_calls, 2);
        assert_eq!(report.max_depth, 2);
        assert_eq!(report.exhausted.as_deref(), Some("model_calls"));
    }

    #[test]
    fn a_chain_too_deep_is_refused_before_a_call_is_counted() {
        let tracker = BudgetTracker::new(Budget::understanding());
        assert_eq!(tracker.reserve(9), Err(BudgetBound::ChainDepth));
        assert_eq!(tracker.report().model_calls, 0);
    }

    #[test]
    fn reported_tokens_stop_the_next_call() {
        let tracker = BudgetTracker::new(Budget {
            max_prompt_tokens: Some(100),
            ..Budget::understanding()
        });
        tracker.record_tokens(100);
        assert_eq!(tracker.reserve(1), Err(BudgetBound::PromptTokens));
    }

    #[test]
    fn unbounded_is_a_choice_that_still_keeps_a_deadline() {
        let tracker = BudgetTracker::new(Budget::unbounded());
        for _ in 0..100 {
            assert!(tracker.reserve(200).is_ok());
        }
        assert_eq!(tracker.call_timeout(None), Duration::from_secs(60));
    }
}

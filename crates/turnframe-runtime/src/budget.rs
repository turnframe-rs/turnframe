//! The resource budget of the sandboxed autonomous mode (spec §11.1).
//!
//! [`OrchestrationMode::SandboxedAutonomous`](crate::config::OrchestrationMode::SandboxedAutonomous)
//! is the one mode where the model drives writes nobody reviewed, and the
//! [`ResourceBudget`] it carries is what keeps that from being open-ended. This
//! module is the enforcement: it measures what a turn has spent and answers one
//! question — *may the turn keep going?* — with the name of the bound that ran
//! out when the answer is no.
//!
//! # Three bounds, one rule
//!
//! | Bound | Measured as | Exhausted when |
//! | --- | --- | --- |
//! | [`max_model_calls`](ResourceBudget::max_model_calls) | one [`ProviderAttempt`] per call the turn made, retries and fallbacks included | `spent >= max` |
//! | [`max_prompt_tokens`](ResourceBudget::max_prompt_tokens) | the prompt tokens the provider reported for every successful attempt | `spent >= max` |
//! | [`max_wall_clock`](ResourceBudget::max_wall_clock) | the runtime clock, from the moment the turn was accepted | `elapsed >= max` |
//!
//! The comparison is `>=`, not `>`, and that is the fail-closed reading: a
//! budget of eight calls means a turn may make eight, so a turn that has made
//! eight has nothing left to spend and stops before asking for a ninth. An
//! attempt that failed still counts — it cost the same call.
//!
//! # Where it is checked, and what happens
//!
//! [`Orchestrator::handle_turn`](crate::orchestrator::Orchestrator::handle_turn)
//! checks the budget after interpretation and again before anything executes.
//! Both are **before** any effect, so an exhausted budget fails the turn
//! outright: [`BudgetLimit::into_error`] names the bound in a typed
//! [`PolicyError::BudgetExhausted`], nothing was journaled and nothing can have
//! happened.
//!
//! After the commit the answer is different, and deliberately so. The effects
//! are real by then, and §23.1 says a turn that committed keeps its effects and
//! regenerates its wording; failing it to save a token budget would throw away
//! the only truthful account of what just happened. So a budget that runs out
//! post-commit stops the *model calls* — narration and answers — and the turn
//! is delivered with its receipts, its notices and an explicit
//! [`budget_exhausted`](crate::compose::notice::BUDGET_EXHAUSTED) notice.
//!
//! ```
//! use std::time::Duration;
//! use turnframe_runtime::budget::{BudgetLimit, BudgetSpend, TurnBudget};
//! use turnframe_runtime::config::ResourceBudget;
//!
//! let budget = ResourceBudget::conservative().with_max_model_calls(2);
//! let started = chrono::DateTime::UNIX_EPOCH;
//! let turn = TurnBudget::new(budget, started);
//!
//! let spent = BudgetSpend::none().with_model_calls(2);
//! assert_eq!(turn.exhausted(spent, started), Some(BudgetLimit::ModelCalls));
//! assert_eq!(turn.exhausted(BudgetSpend::none(), started), None);
//! ```

use chrono::{DateTime, Utc};
use turnframe_core::error::{OrchestratorError, PolicyError};
use turnframe_provider::fallback::{AttemptOutcome, ProviderAttempt};

use crate::config::ResourceBudget;

/// Which bound of a [`ResourceBudget`] ran out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum BudgetLimit {
    /// The turn made as many model calls as it was allowed.
    ModelCalls,
    /// The turn sent as many prompt tokens as it was allowed.
    PromptTokens,
    /// The turn took as long as it was allowed.
    WallClock,
}

impl BudgetLimit {
    /// Every bound, in the order they are checked.
    pub const ALL: [Self; 3] = [Self::ModelCalls, Self::PromptTokens, Self::WallClock];

    /// Stable snake-case name, safe on the wire and in a metric label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ModelCalls => "model_calls",
            Self::PromptTokens => "prompt_tokens",
            Self::WallClock => "wall_clock",
        }
    }

    /// The typed failure a turn stopped by this bound returns.
    ///
    /// It is a [`PolicyError`] rather than an internal failure because that is
    /// what it is: a configured limit refused to let the turn continue, and the
    /// caller is entitled to know which one.
    #[must_use]
    pub fn into_error(self) -> OrchestratorError {
        OrchestratorError::Policy(PolicyError::BudgetExhausted {
            limit: self.as_str().to_owned(),
        })
    }
}

impl std::fmt::Display for BudgetLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a turn has spent so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct BudgetSpend {
    /// Model calls made, retries and fallbacks included.
    pub model_calls: u64,
    /// Prompt tokens the providers reported.
    pub prompt_tokens: u64,
}

impl BudgetSpend {
    /// Nothing spent yet.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            model_calls: 0,
            prompt_tokens: 0,
        }
    }

    /// Returns a copy with another call count.
    #[must_use]
    pub const fn with_model_calls(mut self, model_calls: u64) -> Self {
        self.model_calls = model_calls;
        self
    }

    /// Returns a copy with another token count.
    #[must_use]
    pub const fn with_prompt_tokens(mut self, prompt_tokens: u64) -> Self {
        self.prompt_tokens = prompt_tokens;
        self
    }

    /// Measures a turn's attempt trail.
    ///
    /// Every attempt is one model call, whatever became of it: a retry after a
    /// timeout cost the same call as the answer that followed it. Tokens are
    /// counted only where a provider reported them, because a provider that
    /// reports nothing has told us nothing, and inventing an estimate would
    /// make the bound a guess.
    ///
    /// A [cancelled](AttemptOutcome::Cancelled) attempt is not counted: the
    /// caller went away before the provider was asked to do anything.
    #[must_use]
    pub fn of(attempts: &[ProviderAttempt]) -> Self {
        let mut spend = Self::none();
        for attempt in attempts {
            if attempt.outcome == AttemptOutcome::Cancelled {
                continue;
            }
            spend.model_calls = spend.model_calls.saturating_add(1);
            spend.prompt_tokens = spend
                .prompt_tokens
                .saturating_add(attempt.input_tokens.unwrap_or(0));
        }
        spend
    }
}

/// One turn's budget, measured against the clock it started on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnBudget {
    budget: ResourceBudget,
    started_at: DateTime<Utc>,
}

impl TurnBudget {
    /// Binds `budget` to the instant the turn was accepted.
    #[must_use]
    pub const fn new(budget: ResourceBudget, started_at: DateTime<Utc>) -> Self {
        Self { budget, started_at }
    }

    /// The budget in force.
    #[must_use]
    pub const fn budget(&self) -> &ResourceBudget {
        &self.budget
    }

    /// When the turn started.
    #[must_use]
    pub const fn started_at(&self) -> DateTime<Utc> {
        self.started_at
    }

    /// The first bound that is exhausted at `now`, if any.
    ///
    /// The order is fixed — calls, then tokens, then the clock — so the same
    /// turn always reports the same bound, and a replay of it says the same
    /// thing.
    #[must_use]
    pub fn exhausted(&self, spent: BudgetSpend, now: DateTime<Utc>) -> Option<BudgetLimit> {
        if spent.model_calls >= u64::from(self.budget.max_model_calls) {
            return Some(BudgetLimit::ModelCalls);
        }
        if spent.prompt_tokens >= self.budget.max_prompt_tokens {
            return Some(BudgetLimit::PromptTokens);
        }
        let elapsed = (now - self.started_at).to_std().unwrap_or_default();
        if elapsed >= self.budget.max_wall_clock {
            return Some(BudgetLimit::WallClock);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use turnframe_provider::fallback::FallbackStage;
    use turnframe_provider::ids::{AttemptNumber, ModelRef, RequestId};
    use turnframe_provider::purpose::ModelPurpose;

    use super::*;

    fn attempt(outcome: AttemptOutcome, input_tokens: Option<u64>) -> ProviderAttempt {
        ProviderAttempt {
            attempt: AttemptNumber::FIRST,
            request_id: RequestId::nil(),
            purpose: ModelPurpose::Extract,
            stage: FallbackStage::PreCommit,
            model: ModelRef::new("p", "m"),
            outcome,
            class: None,
            latency: Duration::ZERO,
            input_tokens,
            output_tokens: None,
            temperature: None,
            finish_reasons: Vec::new(),
        }
    }

    fn started() -> DateTime<Utc> {
        DateTime::from_timestamp(1_700_000_000, 0).expect("a valid fixed instant")
    }

    #[test]
    fn every_attempt_is_a_call_and_only_reported_tokens_count() {
        let spend = BudgetSpend::of(&[
            attempt(
                AttemptOutcome::Retried {
                    code: "timeout".to_owned(),
                },
                None,
            ),
            attempt(AttemptOutcome::Succeeded, Some(120)),
            attempt(AttemptOutcome::Cancelled, Some(999)),
        ]);
        assert_eq!(
            spend.model_calls, 2,
            "a retry cost a call; a cancel did not"
        );
        assert_eq!(spend.prompt_tokens, 120);
        assert_eq!(BudgetSpend::of(&[]), BudgetSpend::none());
    }

    #[test]
    fn each_bound_reports_itself_and_the_order_is_fixed() {
        let budget = ResourceBudget::conservative()
            .with_max_model_calls(4)
            .with_max_prompt_tokens(100)
            .with_max_wall_clock(Duration::from_secs(30));
        let turn = TurnBudget::new(budget, started());
        assert_eq!(turn.budget(), &budget);
        assert_eq!(turn.started_at(), started());

        assert_eq!(turn.exhausted(BudgetSpend::none(), started()), None);
        assert_eq!(
            turn.exhausted(BudgetSpend::none().with_model_calls(4), started()),
            Some(BudgetLimit::ModelCalls)
        );
        assert_eq!(
            turn.exhausted(BudgetSpend::none().with_prompt_tokens(100), started()),
            Some(BudgetLimit::PromptTokens)
        );
        assert_eq!(
            turn.exhausted(
                BudgetSpend::none(),
                started() + chrono::Duration::seconds(30)
            ),
            Some(BudgetLimit::WallClock)
        );
        // Everything at once still names the first bound in the fixed order.
        assert_eq!(
            turn.exhausted(
                BudgetSpend::none()
                    .with_model_calls(9)
                    .with_prompt_tokens(999),
                started() + chrono::Duration::seconds(99)
            ),
            Some(BudgetLimit::ModelCalls)
        );
        // A clock that went backwards is not an exhausted budget.
        assert_eq!(
            turn.exhausted(
                BudgetSpend::none(),
                started() - chrono::Duration::seconds(5)
            ),
            None
        );
    }

    #[test]
    fn a_limit_names_itself_in_the_error_it_raises() {
        for limit in BudgetLimit::ALL {
            let error = limit.into_error();
            assert!(
                matches!(
                    &error,
                    OrchestratorError::Policy(PolicyError::BudgetExhausted { limit: name })
                        if name == limit.as_str()
                ),
                "{limit} did not name itself: {error}"
            );
            assert_eq!(limit.to_string(), limit.as_str());
        }
    }
}

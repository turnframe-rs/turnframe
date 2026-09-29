//! Retry and fallback across candidates (spec §20.7, invariant I17).
//!
//! [`execute_with_fallback`] walks the candidates a router produced, retries within one where
//! the error class allows it, moves on where it does not, and records every attempt. Where in
//! the turn the call happens decides whether moving on is safe, and nothing in a
//! [`ModelRequest`] reveals it, so [`FallbackStage`] is a required positional argument:
//!
//! * [`FallbackStage::PreCommit`]: understanding a message, before any command executes;
//!   retries and fallback are free, because nothing has happened yet.
//! * [`FallbackStage::PostCommitNarration`]: the reply, after the commit, whose facts the
//!   events already fix. A critical purpose passed here is refused outright (I17).
//!
//! A [`FallbackOutcome`] carries one [`ModelResponse`] from one provider: nothing is ever
//! merged across candidates, because an answer half from one model and half from another is
//! an answer nobody reviewed.

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{ProviderDetail, ProviderError, ProviderErrorKind, RetryClass};
use crate::ids::{AttemptNumber, ModelRef, RequestId};
use crate::purpose::ModelPurpose;
use crate::request::ModelRequest;
use crate::response::ModelResponse;
use crate::router::{Clock, ProviderCandidate, SystemClock};

/// Where in the turn a call is being made (invariant I17).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackStage {
    /// Before any command executes.
    PreCommit,
    /// After events are committed, for answering and narration only.
    PostCommitNarration,
}

impl FallbackStage {
    /// Stable snake-case label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PreCommit => "pre_commit",
            Self::PostCommitNarration => "post_commit_narration",
        }
    }

    /// Returns `true` when `purpose` may run at this stage.
    ///
    /// A [critical](ModelPurpose::is_critical) purpose — one whose output can
    /// become commands or reads — may only run before commit.
    #[must_use]
    pub const fn admits(self, purpose: ModelPurpose) -> bool {
        match self {
            Self::PreCommit => true,
            Self::PostCommitNarration => !purpose.is_critical(),
        }
    }
}

impl fmt::Display for FallbackStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Waits between attempts. Injectable so tests do not sleep.
#[async_trait::async_trait]
pub trait Sleeper: Send + Sync + fmt::Debug {
    /// Waits for `duration`.
    async fn sleep(&self, duration: Duration);
}

/// Sleeps on the Tokio timer.
#[derive(Debug, Clone, Copy, Default)]
pub struct TokioSleeper;

#[async_trait::async_trait]
impl Sleeper for TokioSleeper {
    async fn sleep(&self, duration: Duration) {
        if !duration.is_zero() {
            tokio::time::sleep(duration).await;
        }
    }
}

/// How hard to try one candidate before moving on (spec §20.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Attempts per candidate, including the first. `1` disables retries.
    pub max_attempts: u32,
    /// Delay before the second attempt.
    pub initial_backoff: Duration,
    /// Ceiling on the computed delay.
    pub max_backoff: Duration,
    /// Multiplier applied per additional attempt.
    pub backoff_multiplier: u32,
    /// Whether to spread delays. **Off by default**: a fixed schedule makes a
    /// replayed turn reproduce the same timing, and the jitter that is
    /// available is derived from the request id rather than from randomness,
    /// so it stays reproducible too.
    pub jitter: bool,
    /// Whether a provider's own `Retry-After` overrides the computed backoff.
    pub honour_retry_after: bool,
    /// Ceiling on a honoured `Retry-After`, so a provider cannot park a turn.
    pub max_retry_after: Duration,
}

impl RetryPolicy {
    /// Three attempts, 200 ms growing by four, capped at five seconds, no
    /// jitter, honouring `Retry-After` up to thirty seconds.
    pub const DEFAULT: Self = Self {
        max_attempts: 3,
        initial_backoff: Duration::from_millis(200),
        max_backoff: Duration::from_secs(5),
        backoff_multiplier: 4,
        jitter: false,
        honour_retry_after: true,
        max_retry_after: Duration::from_secs(30),
    };

    /// One attempt per candidate: fall back rather than retry.
    pub const NO_RETRY: Self = Self {
        max_attempts: 1,
        ..Self::DEFAULT
    };

    /// The delay before attempt number `attempt` of a candidate.
    ///
    /// `attempt` is 1-based, so [`AttemptNumber::FIRST`] has no delay.
    #[must_use]
    pub fn backoff_for(&self, attempt: AttemptNumber) -> Duration {
        let step = attempt.get().saturating_sub(1);
        if step == 0 {
            return Duration::ZERO;
        }
        let factor = self.backoff_multiplier.saturating_pow(step - 1);
        self.initial_backoff
            .saturating_mul(factor)
            .min(self.max_backoff)
    }

    /// The delay to wait before `attempt`, given what went wrong last time.
    ///
    /// A [`RetryAfter`](RetryClass::RetryAfter) failure that carried a delay
    /// wins over the computed backoff when
    /// [`honour_retry_after`](Self::honour_retry_after) is set, capped by
    /// [`max_retry_after`](Self::max_retry_after). Jitter, when enabled, is
    /// derived from `request_id` so the same turn replays with the same
    /// timing.
    #[must_use]
    pub fn delay_for(
        &self,
        attempt: AttemptNumber,
        previous: &ProviderError,
        request_id: RequestId,
    ) -> Duration {
        if self.honour_retry_after
            && let Some(asked) = previous.retry_after()
        {
            return asked.min(self.max_retry_after);
        }
        let base = self.backoff_for(attempt);
        if !self.jitter || base.is_zero() {
            return base;
        }
        // Deterministic spread in [50%, 100%] of the computed backoff, seeded
        // by the stable request id and the attempt number.
        let seed = request_id.as_uuid().as_u128() ^ u128::from(attempt.get());
        let permille = 500 + u64::try_from(seed % 501).unwrap_or(0);
        Duration::from_nanos(
            u64::try_from(base.as_nanos().saturating_mul(u128::from(permille)) / 1000)
                .unwrap_or(u64::MAX),
        )
    }
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// How one attempt ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AttemptOutcome {
    /// The provider answered.
    Succeeded,
    /// It failed and the same provider was tried again.
    Retried {
        /// The failure family.
        code: String,
    },
    /// It failed and routing moved to the next candidate.
    FellBack {
        /// The failure family.
        code: String,
    },
    /// It failed and the stage was abandoned.
    Failed {
        /// The failure family.
        code: String,
    },
    /// The caller cancelled.
    Cancelled,
}

impl AttemptOutcome {
    /// Stable snake-case label.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Retried { .. } => "retried",
            Self::FellBack { .. } => "fell_back",
            Self::Failed { .. } => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Maps onto the outcome the core replay record stores.
    #[must_use]
    pub fn to_core_outcome(&self) -> turnframe_core::replay::ProviderAttemptOutcome {
        use turnframe_core::replay::ProviderAttemptOutcome as Core;
        match self {
            Self::Succeeded => Core::Succeeded,
            Self::Retried { code } | Self::Failed { code } => Core::Failed { code: code.clone() },
            Self::FellBack { code } => Core::FellBack { code: code.clone() },
            Self::Cancelled => Core::Cancelled,
        }
    }
}

/// One recorded model call (spec §20.7: "record every provider attempt").
///
/// `Eq` is deliberately absent: the record carries the requested temperature,
/// and a float has no total equality.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderAttempt {
    /// 1-based position within the whole stage, across candidates.
    pub attempt: AttemptNumber,
    /// The stable id sent on every attempt of the logical call.
    pub request_id: RequestId,
    /// Why the call was made.
    pub purpose: ModelPurpose,
    /// Where in the turn it happened.
    pub stage: FallbackStage,
    /// Which profile was called.
    pub model: ModelRef,
    /// How it ended.
    pub outcome: AttemptOutcome,
    /// How the failure was classified, when it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub class: Option<RetryClass>,
    /// Wall-clock duration of the attempt.
    pub latency: Duration,
    /// Tokens reported, when the attempt succeeded and the provider reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    /// Tokens generated, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// Sampling temperature the request asked for, when it set one.
    ///
    /// Recorded as sent, so an audit reads the value that produced the answer
    /// rather than a default filled in later.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Finish reasons the provider reported, verbatim.
    ///
    /// Not normalized: providers spell them differently, and the difference is
    /// often exactly what the audit is being read for.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub finish_reasons: Vec<String>,
}

impl ProviderAttempt {
    /// Converts into the record the core replay log stores (I20).
    #[must_use]
    pub fn to_core_record(&self) -> turnframe_core::replay::ProviderAttemptRecord {
        turnframe_core::replay::ProviderAttemptRecord {
            attempt: self.attempt.get(),
            purpose: self.purpose.as_str().to_owned(),
            provider_key: self.model.provider.clone(),
            model_key: self.model.model.clone(),
            request_id: self.request_id.to_string(),
            prompt_version: None,
            // Set by the runtime, which is the layer that knows which prompt
            // source produced the instructions this call carried.
            prompt_ref: None,
            outcome: self.outcome.to_core_outcome(),
            latency_ms: u64::try_from(self.latency.as_millis()).ok(),
            input_tokens: self.input_tokens,
            output_tokens: self.output_tokens,
            temperature: self.temperature,
            finish_reasons: self.finish_reasons.clone(),
        }
    }
}

/// A successful stage, with the trail that got there.
#[derive(Debug, Clone, PartialEq)]
pub struct FallbackOutcome {
    /// The answer, from exactly one provider. Never assembled from several.
    pub response: ModelResponse,
    /// Every attempt, in order, the successful one last.
    pub attempts: Vec<ProviderAttempt>,
}

impl FallbackOutcome {
    /// The profile that answered.
    #[must_use]
    pub fn served_by(&self) -> ModelRef {
        self.response.reference()
    }

    /// Returns `true` when more than one profile was called.
    #[must_use]
    pub fn fell_back(&self) -> bool {
        self.attempts
            .iter()
            .any(|attempt| matches!(attempt.outcome, AttemptOutcome::FellBack { .. }))
    }
}

/// A stage that ran out of candidates.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{error} after {} attempt(s)", attempts.len())]
pub struct FallbackFailure {
    /// The failure of the last attempt, or the reason nothing was attempted.
    pub error: ProviderError,
    /// Every attempt, in order.
    pub attempts: Vec<ProviderAttempt>,
}

impl FallbackFailure {
    /// A failure with no attempt behind it (an empty candidate list, a stage
    /// that refused the purpose).
    #[must_use]
    pub fn unattempted(error: ProviderError) -> Self {
        Self {
            error,
            attempts: Vec::new(),
        }
    }
}

/// Knobs that are not the stage.
///
/// The stage is deliberately absent: it is a positional argument of
/// [`execute_with_fallback`] so it cannot be defaulted away.
#[derive(Clone)]
pub struct FallbackOptions {
    /// How hard to try each candidate.
    pub retry: RetryPolicy,
    /// Whether the walk may move past the first candidate at all. Setting it
    /// to `false` restricts a stage to one profile without changing the
    /// router's output.
    pub allow_provider_fallback: bool,
    /// Who waits between attempts.
    pub sleeper: Arc<dyn Sleeper>,
    /// Where attempt latencies come from.
    pub clock: Arc<dyn Clock>,
}

impl FallbackOptions {
    /// The default policy, sleeping on the Tokio timer and timing on the wall
    /// clock.
    #[must_use]
    pub fn new() -> Self {
        Self {
            retry: RetryPolicy::DEFAULT,
            allow_provider_fallback: true,
            sleeper: Arc::new(TokioSleeper),
            clock: Arc::new(SystemClock),
        }
    }

    /// Sets the retry policy.
    #[must_use]
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }

    /// Restricts the stage to the first candidate.
    #[must_use]
    pub fn without_provider_fallback(mut self) -> Self {
        self.allow_provider_fallback = false;
        self
    }

    /// Injects a sleeper.
    #[must_use]
    pub fn with_sleeper<S: Sleeper + 'static>(mut self, sleeper: Arc<S>) -> Self {
        self.sleeper = sleeper;
        self
    }

    /// Injects a clock.
    #[must_use]
    pub fn with_clock<C: Clock + 'static>(mut self, clock: Arc<C>) -> Self {
        self.clock = clock;
        self
    }
}

impl Default for FallbackOptions {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for FallbackOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FallbackOptions")
            .field("retry", &self.retry)
            .field("allow_provider_fallback", &self.allow_provider_fallback)
            .finish_non_exhaustive()
    }
}

/// Runs `request` against `candidates` until one answers.
///
/// Each candidate gets up to [`RetryPolicy::max_attempts`]: `Retry` and `RetryAfter` wait, a
/// `Fallback` moves on, `Fatal` abandons the stage; every attempt reuses the request id.
///
/// # Errors
///
/// [`FallbackFailure`], with the last error and the whole trail, when there is no
/// candidate, the stage refuses the request's purpose (I17), or every candidate failed.
///
/// ```
/// use std::sync::Arc;
/// use turnframe_provider::prelude::*;
/// use turnframe_provider::testing::{ImmediateSleeper, StaticProvider};
///
/// # futures::executor::block_on(async {
/// let flaky = Arc::new(
///     StaticProvider::new("a", "m").failing_once(ProviderError::server(Some(503))),
/// );
/// let candidates = vec![StaticProvider::candidate(Arc::clone(&flaky))];
/// let options = FallbackOptions::new().with_sleeper(Arc::new(ImmediateSleeper::new()));
///
/// let outcome = execute_with_fallback(
///     &candidates,
///     &ModelRequest::new(ModelPurpose::Acknowledge),
///     FallbackStage::PreCommit,
///     &options,
/// )
/// .await
/// .unwrap();
///
/// assert_eq!(outcome.attempts.len(), 2, "one failure, then one success");
/// assert!(!outcome.fell_back(), "the same provider answered");
/// # });
/// ```
// The failure deliberately carries the whole attempt trail (spec §20.7:
// "record every provider attempt"), which puts the `Err` variant over clippy's
// size threshold. Boxing it would move an allocation onto a path that runs once
// per stage, to save copying a struct that is returned once per stage.
#[allow(clippy::result_large_err)]
pub async fn execute_with_fallback(
    candidates: &[ProviderCandidate],
    request: &ModelRequest,
    stage: FallbackStage,
    options: &FallbackOptions,
) -> Result<FallbackOutcome, FallbackFailure> {
    if !stage.admits(request.purpose) {
        // I17: re-running a mutation plan after effects may have committed is
        // the failure this whole module exists to prevent.
        return Err(FallbackFailure::unattempted(
            ProviderError::invalid_request("critical_purpose_after_commit"),
        ));
    }
    if candidates.is_empty() {
        return Err(FallbackFailure::unattempted(ProviderError::other(
            "no_candidate",
        )));
    }

    let usable = if options.allow_provider_fallback {
        candidates
    } else {
        &candidates[..1]
    };

    let mut attempts: Vec<ProviderAttempt> = Vec::new();
    let mut counter = AttemptNumber::FIRST;
    let mut last_error = ProviderError::other("no_candidate");

    for (index, candidate) in usable.iter().enumerate() {
        let model = candidate.reference();
        let is_last_candidate = index + 1 == usable.len();
        let mut within = AttemptNumber::FIRST;

        loop {
            let started = options.clock.now();
            let result = candidate.provider.generate(request.clone()).await;
            let latency = elapsed_since(options.clock.now(), started);

            match result {
                Ok(response) => {
                    tracing::debug!(
                        request_id = %request.request_id,
                        purpose = request.purpose.as_str(),
                        stage = stage.as_str(),
                        provider = model.provider.as_str(),
                        model = model.model.as_str(),
                        attempt = counter.get(),
                        latency_ms = u64::try_from(latency.as_millis()).unwrap_or(u64::MAX),
                        "provider attempt succeeded"
                    );
                    attempts.push(ProviderAttempt {
                        attempt: counter,
                        request_id: request.request_id,
                        purpose: request.purpose,
                        stage,
                        model,
                        outcome: AttemptOutcome::Succeeded,
                        class: None,
                        latency,
                        input_tokens: Some(response.usage.input),
                        output_tokens: Some(response.usage.output),
                        temperature: request.temperature,
                        finish_reasons: vec![response.finish.as_str().to_owned()],
                    });
                    return Ok(FallbackOutcome { response, attempts });
                }
                Err(error) => {
                    let error = error.with_model(&model);
                    let class = error.retry_class();
                    let code = error.kind().as_str().to_owned();
                    let has_attempts_left = within.get() < options.retry.max_attempts;
                    let will_retry = class.allows_same_provider() && has_attempts_left;
                    let will_fall_back =
                        !will_retry && class.allows_another_candidate() && !is_last_candidate;

                    let outcome = if matches!(error.kind(), ProviderErrorKind::Cancelled) {
                        AttemptOutcome::Cancelled
                    } else if will_retry {
                        AttemptOutcome::Retried { code }
                    } else if will_fall_back {
                        AttemptOutcome::FellBack { code }
                    } else {
                        AttemptOutcome::Failed { code }
                    };
                    tracing::warn!(
                        request_id = %request.request_id,
                        purpose = request.purpose.as_str(),
                        stage = stage.as_str(),
                        provider = model.provider.as_str(),
                        model = model.model.as_str(),
                        attempt = counter.get(),
                        error = error.kind().as_str(),
                        class = class.as_str(),
                        outcome = outcome.as_str(),
                        // The endpoint's own sentence, sanitized by the
                        // adapter. Without it a malformed request this library
                        // sent and a provider that is genuinely down are the
                        // same line, and only one of them is anybody's bug to
                        // fix.
                        detail = error.detail().map(ProviderDetail::as_str),
                        "provider attempt failed"
                    );
                    attempts.push(ProviderAttempt {
                        attempt: counter,
                        request_id: request.request_id,
                        purpose: request.purpose,
                        stage,
                        model: model.clone(),
                        outcome,
                        class: Some(class),
                        latency,
                        input_tokens: None,
                        output_tokens: None,
                        temperature: request.temperature,
                        finish_reasons: Vec::new(),
                    });
                    counter = counter.next();

                    if will_retry {
                        within = within.next();
                        let delay = options.retry.delay_for(within, &error, request.request_id);
                        options.sleeper.sleep(delay).await;
                        continue;
                    }
                    last_error = error;
                    if class == RetryClass::Fatal {
                        // Neither another try nor another provider can help.
                        return Err(FallbackFailure {
                            error: last_error,
                            attempts,
                        });
                    }
                    break;
                }
            }
        }
    }

    Err(FallbackFailure {
        error: last_error,
        attempts,
    })
}

/// Duration between two clock readings, floored at zero.
fn elapsed_since(
    now: chrono::DateTime<chrono::Utc>,
    started: chrono::DateTime<chrono::Utc>,
) -> Duration {
    (now - started).to_std().unwrap_or(Duration::ZERO)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::ProviderCapabilities;
    use crate::error::RetryClass;
    use crate::provider::ModelProvider;
    use crate::response::FinishReason;
    use crate::testing::{ImmediateSleeper, ManualClock, StaticProvider};
    use serde_json::json;

    fn options(sleeper: &Arc<ImmediateSleeper>) -> FallbackOptions {
        FallbackOptions::new()
            .with_sleeper(Arc::clone(sleeper))
            .with_clock(Arc::new(ManualClock::at_epoch()))
    }

    fn request(purpose: ModelPurpose) -> ModelRequest {
        ModelRequest::new(purpose).with_request_id(RequestId::nil())
    }

    #[tokio::test]
    async fn a_retryable_failure_retries_the_same_provider() {
        let provider = Arc::new(
            StaticProvider::new("a", "m")
                .failing(vec![
                    ProviderError::server(Some(503)),
                    ProviderError::timeout(),
                ])
                .answering_text("ok"),
        );
        let sleeper = Arc::new(ImmediateSleeper::new());
        let outcome = execute_with_fallback(
            &[StaticProvider::candidate(Arc::clone(&provider))],
            &request(ModelPurpose::Acknowledge),
            FallbackStage::PreCommit,
            &options(&sleeper),
        )
        .await
        .unwrap();

        assert_eq!(outcome.attempts.len(), 3);
        assert!(matches!(
            outcome.attempts[0].outcome,
            AttemptOutcome::Retried { .. }
        ));
        assert!(matches!(
            outcome.attempts[2].outcome,
            AttemptOutcome::Succeeded
        ));
        assert!(!outcome.fell_back());
        assert_eq!(outcome.served_by(), ModelRef::new("a", "m"));
        assert_eq!(sleeper.slept().len(), 2, "one wait per retry");
    }

    #[tokio::test]
    async fn exhausting_a_candidate_moves_to_the_next_one() {
        let broken =
            Arc::new(StaticProvider::new("a", "m").always_failing(ProviderError::timeout()));
        let good = Arc::new(StaticProvider::new("b", "m").answering_text("ciao"));
        let sleeper = Arc::new(ImmediateSleeper::new());
        let outcome = execute_with_fallback(
            &[
                StaticProvider::candidate(Arc::clone(&broken)),
                StaticProvider::candidate(Arc::clone(&good)),
            ],
            &request(ModelPurpose::Acknowledge),
            FallbackStage::PreCommit,
            &options(&sleeper),
        )
        .await
        .unwrap();

        assert_eq!(
            broken.call_count(),
            3,
            "max_attempts on the first candidate"
        );
        assert_eq!(good.call_count(), 1);
        assert_eq!(outcome.attempts.len(), 4);
        assert!(matches!(
            outcome.attempts[2].outcome,
            AttemptOutcome::FellBack { .. }
        ));
        assert!(outcome.fell_back());
        assert_eq!(outcome.response.text(), "ciao");
        assert_eq!(outcome.served_by(), ModelRef::new("b", "m"));
    }

    #[tokio::test]
    async fn a_fallback_class_moves_on_without_retrying() {
        let unauthorized =
            Arc::new(StaticProvider::new("a", "m").always_failing(ProviderError::authentication()));
        let good = Arc::new(StaticProvider::new("b", "m").answering_text("ok"));
        let sleeper = Arc::new(ImmediateSleeper::new());
        let outcome = execute_with_fallback(
            &[
                StaticProvider::candidate(Arc::clone(&unauthorized)),
                StaticProvider::candidate(Arc::clone(&good)),
            ],
            &request(ModelPurpose::Acknowledge),
            FallbackStage::PreCommit,
            &options(&sleeper),
        )
        .await
        .unwrap();
        assert_eq!(
            unauthorized.call_count(),
            1,
            "no point retrying bad credentials"
        );
        assert_eq!(outcome.attempts.len(), 2);
        assert!(sleeper.slept().is_empty());
    }

    #[tokio::test]
    async fn a_fatal_class_abandons_the_stage_at_once() {
        let refusing =
            Arc::new(StaticProvider::new("a", "m").always_failing(ProviderError::refusal()));
        let good = Arc::new(StaticProvider::new("b", "m").answering_text("ok"));
        let sleeper = Arc::new(ImmediateSleeper::new());
        let failure = execute_with_fallback(
            &[
                StaticProvider::candidate(Arc::clone(&refusing)),
                StaticProvider::candidate(Arc::clone(&good)),
            ],
            &request(ModelPurpose::Acknowledge),
            FallbackStage::PreCommit,
            &options(&sleeper),
        )
        .await
        .unwrap_err();

        assert_eq!(refusing.call_count(), 1);
        assert_eq!(good.call_count(), 0, "a refusal is not shopped around");
        assert_eq!(failure.attempts.len(), 1);
        assert_eq!(failure.error.retry_class(), RetryClass::Fatal);
        assert!(failure.to_string().contains("1 attempt"), "{failure}");
    }

    #[tokio::test]
    async fn a_critical_purpose_is_refused_after_commit() {
        let provider = Arc::new(StaticProvider::new("a", "m").answering_text("ok"));
        let sleeper = Arc::new(ImmediateSleeper::new());
        for purpose in [ModelPurpose::Extract, ModelPurpose::Investigate] {
            let failure = execute_with_fallback(
                &[StaticProvider::candidate(Arc::clone(&provider))],
                &request(purpose),
                FallbackStage::PostCommitNarration,
                &options(&sleeper),
            )
            .await
            .unwrap_err();
            assert!(failure.attempts.is_empty());
            assert_eq!(provider.call_count(), 0, "the model was never called");
            assert!(
                failure
                    .error
                    .code()
                    .is_some_and(|code| code.as_str() == "critical_purpose_after_commit")
            );
        }
        // Narration is exactly what the post-commit stage is for.
        assert!(
            execute_with_fallback(
                &[StaticProvider::candidate(Arc::clone(&provider))],
                &request(ModelPurpose::Acknowledge),
                FallbackStage::PostCommitNarration,
                &options(&sleeper),
            )
            .await
            .is_ok()
        );
    }

    #[tokio::test]
    async fn partial_outputs_from_different_providers_are_never_merged() {
        // `a` produces half a plan before failing; `b` produces a whole one.
        let half = Arc::new(
            StaticProvider::new("a", "m")
                .replying_once_json(json!({"acts": ["only_from_a"]}))
                .always_failing(ProviderError::server(None)),
        );
        let whole =
            Arc::new(StaticProvider::new("b", "m").answering_json(json!({"acts": ["from_b"]})));
        let sleeper = Arc::new(ImmediateSleeper::new());

        // Drain `a`'s single good answer so the next call fails.
        let _ = half.generate(request(ModelPurpose::Extract)).await.unwrap();

        let outcome = execute_with_fallback(
            &[
                StaticProvider::candidate(Arc::clone(&half)),
                StaticProvider::candidate(Arc::clone(&whole)),
            ],
            &request(ModelPurpose::Extract),
            FallbackStage::PreCommit,
            &options(&sleeper),
        )
        .await
        .unwrap();

        assert_eq!(outcome.response.provider.as_str(), "b");
        assert_eq!(outcome.response.content.len(), 1);
        assert_eq!(
            outcome.response.text(),
            json!({"acts": ["from_b"]}).to_string()
        );
        assert!(
            !outcome.response.text().contains("only_from_a"),
            "no content crossed providers"
        );
    }

    #[tokio::test]
    async fn the_request_id_is_stable_across_every_attempt() {
        let broken =
            Arc::new(StaticProvider::new("a", "m").always_failing(ProviderError::timeout()));
        let good = Arc::new(StaticProvider::new("b", "m").answering_text("ok"));
        let sleeper = Arc::new(ImmediateSleeper::new());
        let request = request(ModelPurpose::Acknowledge);
        let outcome = execute_with_fallback(
            &[
                StaticProvider::candidate(Arc::clone(&broken)),
                StaticProvider::candidate(Arc::clone(&good)),
            ],
            &request,
            FallbackStage::PreCommit,
            &options(&sleeper),
        )
        .await
        .unwrap();

        for attempt in &outcome.attempts {
            assert_eq!(attempt.request_id, request.request_id);
        }
        for seen in broken.calls() {
            assert_eq!(seen.request_id, request.request_id);
        }
        assert_eq!(outcome.response.request_id, request.request_id);
    }

    #[tokio::test]
    async fn attempts_convert_into_core_replay_records() {
        let broken =
            Arc::new(StaticProvider::new("a", "m").always_failing(ProviderError::timeout()));
        let good = Arc::new(
            StaticProvider::new("b", "m")
                .answering_text("ok")
                .with_usage(crate::response::TokenUsage::new(12, 3)),
        );
        let sleeper = Arc::new(ImmediateSleeper::new());
        let outcome = execute_with_fallback(
            &[
                StaticProvider::candidate(broken),
                StaticProvider::candidate(good),
            ],
            &request(ModelPurpose::Extract),
            FallbackStage::PreCommit,
            &options(&sleeper),
        )
        .await
        .unwrap();

        let records: Vec<_> = outcome
            .attempts
            .iter()
            .map(ProviderAttempt::to_core_record)
            .collect();
        assert_eq!(records.len(), 4);
        assert_eq!(records[0].attempt, 1);
        assert_eq!(records[0].provider_key.as_str(), "a");
        assert_eq!(records[0].purpose, "extract");
        assert_eq!(
            records[2].outcome,
            turnframe_core::replay::ProviderAttemptOutcome::FellBack {
                code: "timeout".to_owned()
            }
        );
        assert_eq!(
            records[3].outcome,
            turnframe_core::replay::ProviderAttemptOutcome::Succeeded
        );
        assert_eq!(records[3].input_tokens, Some(12));
        assert_eq!(records[3].output_tokens, Some(3));
        // Every record names the same logical call.
        assert!(
            records
                .iter()
                .all(|r| r.request_id == RequestId::nil().to_string())
        );
    }

    #[tokio::test]
    async fn an_empty_candidate_list_fails_without_attempting() {
        let sleeper = Arc::new(ImmediateSleeper::new());
        let failure = execute_with_fallback(
            &[],
            &request(ModelPurpose::Acknowledge),
            FallbackStage::PreCommit,
            &options(&sleeper),
        )
        .await
        .unwrap_err();
        assert!(failure.attempts.is_empty());
        assert_eq!(
            failure.error.code().map(|c| c.as_str().to_owned()),
            Some("no_candidate".to_owned())
        );
    }

    #[tokio::test]
    async fn provider_fallback_can_be_switched_off() {
        let broken =
            Arc::new(StaticProvider::new("a", "m").always_failing(ProviderError::authentication()));
        let good = Arc::new(StaticProvider::new("b", "m").answering_text("ok"));
        let sleeper = Arc::new(ImmediateSleeper::new());
        let failure = execute_with_fallback(
            &[
                StaticProvider::candidate(Arc::clone(&broken)),
                StaticProvider::candidate(Arc::clone(&good)),
            ],
            &request(ModelPurpose::Acknowledge),
            FallbackStage::PreCommit,
            &options(&sleeper).without_provider_fallback(),
        )
        .await
        .unwrap_err();
        assert_eq!(good.call_count(), 0);
        assert_eq!(failure.attempts.len(), 1);
    }

    #[tokio::test]
    async fn retry_after_is_honoured_and_capped() {
        let provider = Arc::new(
            StaticProvider::new("a", "m")
                .failing_once(ProviderError::rate_limited(Some(Duration::from_secs(2))))
                .answering_text("ok"),
        );
        let sleeper = Arc::new(ImmediateSleeper::new());
        let outcome = execute_with_fallback(
            &[StaticProvider::candidate(Arc::clone(&provider))],
            &request(ModelPurpose::Acknowledge),
            FallbackStage::PreCommit,
            &options(&sleeper),
        )
        .await
        .unwrap();
        assert!(outcome.attempts[0].class == Some(RetryClass::RetryAfter));
        assert_eq!(sleeper.slept(), vec![Duration::from_secs(2)]);

        let capped = RetryPolicy {
            max_retry_after: Duration::from_millis(500),
            ..RetryPolicy::DEFAULT
        };
        assert_eq!(
            capped.delay_for(
                AttemptNumber(2),
                &ProviderError::rate_limited(Some(Duration::from_secs(90))),
                RequestId::nil()
            ),
            Duration::from_millis(500)
        );
    }

    #[test]
    fn backoff_grows_and_is_capped_with_jitter_off_by_default() {
        let policy = RetryPolicy::DEFAULT;
        assert!(!policy.jitter, "determinism is the default");
        assert_eq!(policy.backoff_for(AttemptNumber::FIRST), Duration::ZERO);
        assert_eq!(
            policy.backoff_for(AttemptNumber(2)),
            Duration::from_millis(200)
        );
        assert_eq!(
            policy.backoff_for(AttemptNumber(3)),
            Duration::from_millis(800)
        );
        assert_eq!(policy.backoff_for(AttemptNumber(9)), policy.max_backoff);
        assert_eq!(RetryPolicy::NO_RETRY.max_attempts, 1);
        assert_eq!(RetryPolicy::default(), RetryPolicy::DEFAULT);
    }

    #[test]
    fn jitter_is_derived_from_the_request_id_so_a_replay_repeats_it() {
        let policy = RetryPolicy {
            jitter: true,
            honour_retry_after: false,
            ..RetryPolicy::DEFAULT
        };
        let error = ProviderError::timeout();
        let id = RequestId::nil();
        let first = policy.delay_for(AttemptNumber(2), &error, id);
        let again = policy.delay_for(AttemptNumber(2), &error, id);
        assert_eq!(first, again, "same call, same schedule");
        let base = policy.backoff_for(AttemptNumber(2));
        assert!(first >= base / 2 && first <= base, "{first:?} vs {base:?}");
    }

    #[test]
    fn the_stage_decides_which_purposes_may_run() {
        assert!(FallbackStage::PreCommit.admits(ModelPurpose::Extract));
        assert!(FallbackStage::PreCommit.admits(ModelPurpose::Acknowledge));
        assert!(!FallbackStage::PostCommitNarration.admits(ModelPurpose::Extract));
        assert!(!FallbackStage::PostCommitNarration.admits(ModelPurpose::Investigate));
        assert!(FallbackStage::PostCommitNarration.admits(ModelPurpose::Answer));
        assert_eq!(FallbackStage::PreCommit.to_string(), "pre_commit");
    }

    #[tokio::test]
    async fn a_cancelled_attempt_is_recorded_as_cancelled() {
        let provider =
            Arc::new(StaticProvider::new("a", "m").always_failing(ProviderError::cancelled()));
        let sleeper = Arc::new(ImmediateSleeper::new());
        let failure = execute_with_fallback(
            &[StaticProvider::candidate(provider)],
            &request(ModelPurpose::Acknowledge),
            FallbackStage::PreCommit,
            &options(&sleeper),
        )
        .await
        .unwrap_err();
        assert_eq!(failure.attempts[0].outcome, AttemptOutcome::Cancelled);
        assert_eq!(
            failure.attempts[0].to_core_record().outcome,
            turnframe_core::replay::ProviderAttemptOutcome::Cancelled
        );
    }

    #[test]
    fn options_render_without_leaking_their_collaborators() {
        let rendered = format!("{:?}", FallbackOptions::new());
        assert!(rendered.contains("FallbackOptions"));
        assert!(rendered.contains("allow_provider_fallback"));
    }

    #[tokio::test]
    async fn capabilities_of_a_candidate_are_the_profiles_own() {
        let provider = Arc::new(
            StaticProvider::new("a", "m")
                .with_capabilities(ProviderCapabilities::minimal().with_streaming(true))
                .answering_text("ok"),
        );
        let candidate = StaticProvider::candidate(provider);
        assert!(candidate.profile.capabilities.streaming);
        assert!(candidate.healthy);
        let response = candidate
            .provider
            .generate(request(ModelPurpose::Acknowledge))
            .await
            .unwrap();
        assert_eq!(response.finish, FinishReason::Stop);
    }
}

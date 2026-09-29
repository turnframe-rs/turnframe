//! Test doubles for the provider layer, always compiled.
//!
//! These live in the library rather than behind `#[cfg(test)]` because the
//! runtime, the store and the evaluation crates all need to drive a turn
//! without a network: a scripted [`StaticProvider`], a [`ManualClock`] that
//! makes health cooldowns deterministic, and an [`ImmediateSleeper`] that
//! records a backoff instead of waiting it out.
//!
//! They are doubles, not simulators. [`StaticProvider`] answers from a script
//! and declares whatever capabilities the test gives it — including dishonest
//! ones, which is exactly what a test of the no-silent-downgrade rule needs.
//!
//! ```
//! use std::sync::Arc;
//! use turnframe_provider::prelude::*;
//! use turnframe_provider::testing::StaticProvider;
//!
//! # futures::executor::block_on(async {
//! let provider = StaticProvider::new("fake", "model-1")
//!     .failing_once(ProviderError::server(Some(503)))
//!     .answering_text("done");
//!
//! assert!(provider.generate(ModelRequest::new(ModelPurpose::Acknowledge)).await.is_err());
//! let response = provider.generate(ModelRequest::new(ModelPurpose::Acknowledge)).await.unwrap();
//! assert_eq!(response.text(), "done");
//! assert_eq!(provider.call_count(), 2);
//! # });
//! ```

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};

use crate::capabilities::{MicroCents, ModelProfile, ProviderCapabilities};
use crate::error::ProviderError;
use crate::fallback::Sleeper;
use crate::ids::{CallId, ModelKey, ProviderKey};
use crate::provider::ModelProvider;
use crate::request::{ModelRequest, ToolCall};
use crate::response::{FinishReason, ModelResponse, TokenUsage};
use crate::router::{Clock, ProviderCandidate};
use crate::stream::{ModelStream, StreamEvent};

/// A clock a test moves by hand.
///
/// Health cooldowns and attempt latencies read the clock through
/// [`Clock`], so a test can jump a minute forward without sleeping a minute.
pub struct ManualClock {
    now: Mutex<DateTime<Utc>>,
}

impl ManualClock {
    /// A clock stopped at the Unix epoch.
    #[must_use]
    pub fn at_epoch() -> Self {
        Self::at(Utc.timestamp_opt(0, 0).single().unwrap_or_default())
    }

    /// A clock stopped at `instant`.
    #[must_use]
    pub fn at(instant: DateTime<Utc>) -> Self {
        Self {
            now: Mutex::new(instant),
        }
    }

    /// Moves the clock forward.
    pub fn advance(&self, by: Duration) {
        let delta =
            chrono::Duration::from_std(by).unwrap_or_else(|_| chrono::Duration::seconds(i64::MAX));
        let mut now = self.now.lock().unwrap_or_else(PoisonError::into_inner);
        *now += delta;
    }

    /// Moves the clock to `instant`.
    pub fn set(&self, instant: DateTime<Utc>) {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner) = instant;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> DateTime<Utc> {
        *self.now.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Default for ManualClock {
    fn default() -> Self {
        Self::at_epoch()
    }
}

impl fmt::Debug for ManualClock {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ManualClock")
            .field("now", &self.now().to_rfc3339())
            .finish()
    }
}

/// A sleeper that records what it was asked to wait and returns at once.
///
/// It makes a fallback test assert the *schedule* — that a `Retry-After` was
/// honoured, that a backoff grew — without the test taking that long.
#[derive(Default)]
pub struct ImmediateSleeper {
    slept: Mutex<Vec<Duration>>,
}

impl ImmediateSleeper {
    /// A sleeper with an empty log.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Every duration it was asked to wait, in order.
    #[must_use]
    pub fn slept(&self) -> Vec<Duration> {
        self.slept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The sum of the recorded waits.
    #[must_use]
    pub fn total(&self) -> Duration {
        self.slept().into_iter().sum()
    }

    /// Forgets the log.
    pub fn clear(&self) {
        self.slept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }
}

#[async_trait]
impl Sleeper for ImmediateSleeper {
    async fn sleep(&self, duration: Duration) {
        self.slept
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(duration);
    }
}

impl fmt::Debug for ImmediateSleeper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImmediateSleeper")
            .field("slept", &self.slept())
            .finish()
    }
}

/// One scripted outcome of a [`StaticProvider`] call.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ScriptStep {
    /// Answer with this text.
    Text(String),
    /// Answer with this JSON, serialized into a text part — the shape a
    /// prompt-only or JSON-mode transport produces.
    Json(serde_json::Value),
    /// Answer with a single tool call carrying this payload — the shape a
    /// native function-schema transport produces.
    ToolCall {
        /// Call id echoed back.
        id: CallId,
        /// Tool name.
        name: String,
        /// Arguments.
        arguments: serde_json::Value,
    },
    /// Answer with exactly these stream events. Used by
    /// [`ModelProvider::stream`]; a [`generate`](ModelProvider::generate) that
    /// meets this step reassembles them.
    Stream(Vec<StreamEvent>),
    /// Fail with this error.
    Fail(ProviderError),
}

/// A [`ModelProvider`] that answers from a script.
///
/// Calls consume the queued [`ScriptStep`]s in order; once the queue is empty,
/// every further call gets the *default* step. That split is what lets a test
/// say "fail twice, then succeed for ever" or "answer once, then break".
pub struct StaticProvider {
    profile: ModelProfile,
    script: Mutex<VecDeque<ScriptStep>>,
    default_step: Mutex<ScriptStep>,
    usage: TokenUsage,
    latency: Duration,
    calls: Mutex<Vec<ModelRequest>>,
}

impl StaticProvider {
    /// A provider that answers with empty text for ever.
    #[must_use]
    pub fn new(provider: impl Into<ProviderKey>, model: impl Into<ModelKey>) -> Self {
        Self {
            profile: ModelProfile::new(provider, model, ProviderCapabilities::minimal()),
            script: Mutex::new(VecDeque::new()),
            default_step: Mutex::new(ScriptStep::Text(String::new())),
            usage: TokenUsage::none(),
            latency: Duration::ZERO,
            calls: Mutex::new(Vec::new()),
        }
    }

    /// Declares capabilities. They may be dishonest on purpose: a test of the
    /// no-silent-downgrade rule needs a profile that claims more than it does.
    #[must_use]
    pub fn with_capabilities(mut self, capabilities: ProviderCapabilities) -> Self {
        self.profile.capabilities = capabilities;
        self
    }

    /// Declares a region, so residency routing can be exercised.
    #[must_use]
    pub fn with_profile_region(mut self, region: impl Into<String>) -> Self {
        self.profile.region = Some(region.into());
        self
    }

    /// Declares per-million prices, so cost ceilings can be exercised.
    #[must_use]
    pub fn with_profile_cost(mut self, input: MicroCents, output: MicroCents) -> Self {
        self.profile.cost_per_million_input = Some(input);
        self.profile.cost_per_million_output = Some(output);
        self
    }

    /// Adds a tag to the profile.
    #[must_use]
    pub fn with_profile_tag(mut self, tag: impl Into<String>) -> Self {
        self.profile.tags.push(tag.into());
        self
    }

    /// Reports this usage on every successful answer.
    #[must_use]
    pub fn with_usage(mut self, usage: TokenUsage) -> Self {
        self.usage = usage;
        self
    }

    /// Reports this latency on every successful answer.
    #[must_use]
    pub fn with_latency(mut self, latency: Duration) -> Self {
        self.latency = latency;
        self
    }

    /// Queues one step.
    #[must_use]
    pub fn push(self, step: ScriptStep) -> Self {
        self.script
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(step);
        self
    }

    /// Queues one failure.
    #[must_use]
    pub fn failing_once(self, error: ProviderError) -> Self {
        self.push(ScriptStep::Fail(error))
    }

    /// Queues several failures, in order.
    #[must_use]
    pub fn failing<I: IntoIterator<Item = ProviderError>>(mut self, errors: I) -> Self {
        for error in errors {
            self = self.push(ScriptStep::Fail(error));
        }
        self
    }

    /// Queues one text answer.
    #[must_use]
    pub fn replying_once(self, text: impl Into<String>) -> Self {
        self.push(ScriptStep::Text(text.into()))
    }

    /// Queues one JSON answer.
    #[must_use]
    pub fn replying_once_json(self, value: serde_json::Value) -> Self {
        self.push(ScriptStep::Json(value))
    }

    /// Sets the step used once the queue is empty.
    #[must_use]
    pub fn defaulting_to(self, step: ScriptStep) -> Self {
        *self
            .default_step
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = step;
        self
    }

    /// Answers with this text once the queue is empty.
    #[must_use]
    pub fn answering_text(self, text: impl Into<String>) -> Self {
        self.defaulting_to(ScriptStep::Text(text.into()))
    }

    /// Answers with this JSON once the queue is empty.
    #[must_use]
    pub fn answering_json(self, value: serde_json::Value) -> Self {
        self.defaulting_to(ScriptStep::Json(value))
    }

    /// Answers with a single tool call once the queue is empty.
    #[must_use]
    pub fn answering_tool_call(
        self,
        id: impl Into<CallId>,
        name: impl Into<String>,
        arguments: serde_json::Value,
    ) -> Self {
        self.defaulting_to(ScriptStep::ToolCall {
            id: id.into(),
            name: name.into(),
            arguments,
        })
    }

    /// Answers with these stream events once the queue is empty.
    #[must_use]
    pub fn answering_stream(self, events: Vec<StreamEvent>) -> Self {
        self.defaulting_to(ScriptStep::Stream(events))
    }

    /// Fails with this error once the queue is empty.
    #[must_use]
    pub fn always_failing(self, error: ProviderError) -> Self {
        self.defaulting_to(ScriptStep::Fail(error))
    }

    /// Every request it received, in order.
    #[must_use]
    pub fn calls(&self) -> Vec<ModelRequest> {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// How many calls it received.
    #[must_use]
    pub fn call_count(&self) -> usize {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// The most recent request, when there was one.
    #[must_use]
    pub fn last_call(&self) -> Option<ModelRequest> {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .last()
            .cloned()
    }

    /// How many steps are still queued.
    #[must_use]
    pub fn remaining_steps(&self) -> usize {
        self.script
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .len()
    }

    /// Forgets the recorded calls.
    pub fn clear_calls(&self) {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
    }

    /// Wraps a provider as a routing candidate that is healthy and uses its own
    /// profile — the shortest path from a double to
    /// [`execute_with_fallback`](crate::fallback::execute_with_fallback).
    #[must_use]
    pub fn candidate(provider: Arc<Self>) -> ProviderCandidate {
        let profile = provider.profile.clone();
        ProviderCandidate {
            provider,
            profile,
            healthy: true,
        }
    }

    /// Records the call and takes the next step.
    fn take_step(&self, request: &ModelRequest) -> ScriptStep {
        self.calls
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request.clone());
        let queued = self
            .script
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .pop_front();
        queued.unwrap_or_else(|| {
            self.default_step
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
        })
    }

    /// Turns a step into a response.
    fn respond(
        &self,
        request: &ModelRequest,
        step: ScriptStep,
    ) -> Result<ModelResponse, ProviderError> {
        let base = ModelResponse::new(
            request.request_id,
            self.profile.provider.clone(),
            self.profile.model.clone(),
        )
        .with_usage(self.usage)
        .with_latency(self.latency);
        match step {
            ScriptStep::Text(text) => Ok(base.with_text(text)),
            ScriptStep::Json(value) => Ok(base.with_text(value.to_string())),
            ScriptStep::ToolCall {
                id,
                name,
                arguments,
            } => Ok(base
                .with_tool_call(ToolCall::new(id, name, arguments))
                .with_finish(FinishReason::ToolCalls)),
            ScriptStep::Stream(events) => {
                let mut accumulator = crate::stream::StreamAccumulator::new(
                    request.request_id,
                    self.profile.provider.clone(),
                    self.profile.model.clone(),
                );
                for event in events {
                    accumulator.push(event)?;
                }
                accumulator.finish()
            }
            ScriptStep::Fail(error) => Err(error.with_model(&self.profile.reference())),
        }
    }

    /// Renders a step as stream events, so `stream` and `generate` agree.
    fn events_for(&self, step: &ScriptStep, response: &ModelResponse) -> Vec<StreamEvent> {
        if let ScriptStep::Stream(events) = step {
            return events.clone();
        }
        let mut events = Vec::new();
        let text = response.text();
        if !text.is_empty() {
            events.push(StreamEvent::text(text));
        }
        for call in response.tool_calls() {
            events.push(StreamEvent::tool_call_start(
                call.id.clone(),
                call.name.clone(),
            ));
            events.push(StreamEvent::tool_call_delta(
                call.id.clone(),
                call.arguments.to_string(),
            ));
            events.push(StreamEvent::tool_call_end(call.id.clone()));
        }
        if !response.usage.is_unreported() {
            events.push(StreamEvent::Usage {
                usage: response.usage,
            });
        }
        events.push(StreamEvent::Finish {
            reason: response.finish,
        });
        events
    }
}

#[async_trait]
impl ModelProvider for StaticProvider {
    fn provider_key(&self) -> ProviderKey {
        self.profile.provider.clone()
    }

    fn model_key(&self) -> ModelKey {
        self.profile.model.clone()
    }

    fn capabilities(&self) -> ProviderCapabilities {
        self.profile.capabilities.clone()
    }

    fn profile(&self) -> ModelProfile {
        self.profile.clone()
    }

    async fn generate(&self, request: ModelRequest) -> Result<ModelResponse, ProviderError> {
        let step = self.take_step(&request);
        self.respond(&request, step)
    }

    async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ProviderError> {
        if !self.profile.capabilities.streaming {
            // The double stays honest: a profile that declares no streaming
            // does not get a stream synthesized for it.
            return Err(
                ProviderError::unsupported("streaming").with_model(&self.profile.reference())
            );
        }
        let step = self.take_step(&request);
        if let ScriptStep::Fail(error) = step {
            return Err(error.with_model(&self.profile.reference()));
        }
        let response = self.respond(&request, step.clone())?;
        Ok(ModelStream::from_events(self.events_for(&step, &response)))
    }
}

impl fmt::Debug for StaticProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StaticProvider")
            .field("model", &self.profile.reference().to_string())
            .field("queued", &self.remaining_steps())
            .field("calls", &self.call_count())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::StructuredOutputCapability;
    use crate::ids::RequestId;
    use crate::purpose::ModelPurpose;
    use crate::stream::reconstruct;
    use serde_json::json;

    fn request() -> ModelRequest {
        ModelRequest::new(ModelPurpose::Acknowledge).with_request_id(RequestId::nil())
    }

    #[tokio::test]
    async fn the_queue_runs_out_into_the_default() {
        let provider = StaticProvider::new("fake", "m")
            .failing(vec![ProviderError::timeout(), ProviderError::server(None)])
            .answering_text("finally");
        assert_eq!(provider.remaining_steps(), 2);
        assert!(provider.generate(request()).await.is_err());
        assert!(provider.generate(request()).await.is_err());
        for _ in 0..3 {
            assert_eq!(
                provider.generate(request()).await.unwrap().text(),
                "finally"
            );
        }
        assert_eq!(provider.call_count(), 5);
        assert_eq!(provider.remaining_steps(), 0);
        provider.clear_calls();
        assert_eq!(provider.call_count(), 0);
    }

    #[tokio::test]
    async fn a_queued_answer_can_precede_a_permanent_failure() {
        let provider = StaticProvider::new("fake", "m")
            .replying_once_json(json!({"acts": []}))
            .always_failing(ProviderError::server(Some(500)));
        assert_eq!(
            provider
                .generate(request())
                .await
                .unwrap()
                .single_json()
                .unwrap(),
            json!({"acts": []})
        );
        assert!(provider.generate(request()).await.is_err());
        assert!(provider.generate(request()).await.is_err());
    }

    #[tokio::test]
    async fn failures_are_labelled_with_the_profile() {
        let provider = StaticProvider::new("fake", "m").always_failing(ProviderError::timeout());
        let error = provider.generate(request()).await.unwrap_err();
        assert_eq!(error.provider().map(ProviderKey::as_str), Some("fake"));
        assert_eq!(error.model().map(ModelKey::as_str), Some("m"));
    }

    #[tokio::test]
    async fn calls_are_recorded_verbatim() {
        let provider = StaticProvider::new("fake", "m").answering_text("ok");
        let request = request().with_system("be brief");
        provider.generate(request.clone()).await.unwrap();
        assert_eq!(provider.calls(), vec![request.clone()]);
        assert_eq!(provider.last_call(), Some(request));
    }

    #[tokio::test]
    async fn streaming_is_refused_unless_declared_and_then_reassembles_equal() {
        let silent = StaticProvider::new("fake", "m").answering_text("ok");
        assert!(silent.stream(request()).await.is_err());

        let streaming = StaticProvider::new("fake", "m")
            .with_capabilities(ProviderCapabilities::minimal().with_streaming(true))
            .answering_tool_call("c1", "plan", json!({"n": 1}));
        let whole = streaming.generate(request()).await.unwrap();
        let stream = streaming.stream(request()).await.unwrap();
        let rebuilt = reconstruct(
            stream,
            crate::stream::StreamAccumulator::new(RequestId::nil(), "fake", "m"),
        )
        .await
        .unwrap();
        assert_eq!(rebuilt.content, whole.content);
        assert_eq!(rebuilt.finish, whole.finish);
    }

    #[tokio::test]
    async fn a_scripted_stream_is_replayed_as_written() {
        let provider = StaticProvider::new("fake", "m")
            .with_capabilities(ProviderCapabilities::minimal().with_streaming(true))
            .answering_stream(vec![
                StreamEvent::text("ciao "),
                StreamEvent::text("mondo"),
                StreamEvent::Finish {
                    reason: FinishReason::Stop,
                },
            ]);
        let items = provider
            .stream(request())
            .await
            .unwrap()
            .collect_items()
            .await;
        assert_eq!(items.len(), 3);
        // `generate` reassembles the same script.
        assert_eq!(
            provider.generate(request()).await.unwrap().text(),
            "ciao mondo"
        );
    }

    #[test]
    fn the_profile_carries_what_routing_reads() {
        let provider = StaticProvider::new("fake", "m")
            .with_capabilities(
                ProviderCapabilities::minimal()
                    .with_structured_output(StructuredOutputCapability::NativeJsonSchema),
            )
            .with_profile_region("eu")
            .with_profile_cost(MicroCents::from_cents(1), MicroCents::from_cents(2))
            .with_profile_tag("cheap");
        let profile = provider.profile();
        assert_eq!(profile.region.as_deref(), Some("eu"));
        assert_eq!(profile.tags, vec!["cheap".to_owned()]);
        assert_eq!(
            profile.max_cost_per_million(),
            Some(MicroCents::from_cents(2))
        );
        let candidate = StaticProvider::candidate(Arc::new(provider));
        assert!(candidate.healthy);
        assert_eq!(candidate.reference().to_string(), "fake/m");
    }

    #[test]
    fn the_manual_clock_only_moves_when_told() {
        let clock = ManualClock::at_epoch();
        let start = clock.now();
        assert_eq!(start.timestamp(), 0);
        clock.advance(Duration::from_secs(90));
        assert_eq!(clock.now().timestamp(), 90);
        clock.set(start);
        assert_eq!(clock.now(), start);
        assert!(format!("{clock:?}").contains("ManualClock"));
        assert_eq!(ManualClock::default().now().timestamp(), 0);
    }

    #[tokio::test]
    async fn the_sleeper_records_instead_of_waiting() {
        let sleeper = ImmediateSleeper::new();
        sleeper.sleep(Duration::from_secs(3)).await;
        sleeper.sleep(Duration::from_secs(4)).await;
        assert_eq!(
            sleeper.slept(),
            vec![Duration::from_secs(3), Duration::from_secs(4)]
        );
        assert_eq!(sleeper.total(), Duration::from_secs(7));
        assert!(format!("{sleeper:?}").contains("ImmediateSleeper"));
        sleeper.clear();
        assert!(sleeper.slept().is_empty());
    }

    #[tokio::test]
    async fn the_double_renders_its_state() {
        let provider = StaticProvider::new("fake", "m").failing_once(ProviderError::timeout());
        let rendered = format!("{provider:?}");
        assert!(rendered.contains("fake/m"), "{rendered}");
        assert!(rendered.contains("queued"), "{rendered}");
    }
}

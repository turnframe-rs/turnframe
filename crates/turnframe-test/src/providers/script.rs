//! A [`ModelProvider`] whose whole behaviour is a script the test writes.
//!
//! See the [module documentation](crate::providers) for why the kit ships this
//! next to [`StaticProvider`](turnframe_provider::testing::StaticProvider).

use std::collections::VecDeque;
use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use turnframe_provider::capabilities::{
    MicroCents, ModelProfile, ProviderCapabilities, StructuredOutputCapability,
    ToolCallingCapability,
};
use turnframe_provider::error::ProviderError;
use turnframe_provider::ids::{CallId, ModelKey, ProviderKey};
use turnframe_provider::provider::ModelProvider;
use turnframe_provider::purpose::ModelPurpose;
use turnframe_provider::request::{ContentPart, ModelRequest, Role, ToolCall};
use turnframe_provider::response::{FinishReason, ModelResponse, TokenUsage};
use turnframe_provider::router::ProviderCandidate;
use turnframe_provider::stream::{ModelStream, StreamAccumulator, StreamEvent};

/// Error code of the failure returned for a call the script did not
/// anticipate.
pub const UNEXPECTED_CALL_CODE: &str = "turnframe.test.unexpected_call";

/// Error code of the failure returned when a call arrives for another purpose
/// than the next step expected.
pub const WRONG_PURPOSE_CODE: &str = "turnframe.test.wrong_purpose";

/// Error code of the failure returned when a scripted value cannot be
/// serialized. Only reachable with a custom `Serialize` that fails.
pub const UNSERIALIZABLE_REPLY_CODE: &str = "turnframe.test.unserializable_reply";

/// What one scripted call answers with.
///
/// The variants cover the answers a turn has to survive: the plan you meant,
/// the plan you did not mean, a body that is not JSON at all, a refusal, and
/// the three transport failures whose handling differs (timeout, rate limit,
/// anything else).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ScriptedReply {
    /// An arbitrary JSON document, for the shapes no typed value can express —
    /// an act with a field the schema forbids, a plan missing a required field.
    Json(serde_json::Value),
    /// Prose.
    Text(String),
    /// A single tool call, the shape a native function-schema transport
    /// produces.
    ToolCall {
        /// Call id echoed back to the runtime.
        id: CallId,
        /// Tool name.
        name: String,
        /// Arguments, still untrusted.
        arguments: serde_json::Value,
    },
    /// A body that is not JSON. Sent verbatim, so the test decides *how*
    /// broken it is.
    MalformedJson(String),
    /// The model declined. A semantic outcome
    /// ([`FinishReason::Refusal`]), not a transport failure — the difference a
    /// runtime must not lose.
    Refusal(String),
    /// The call did not answer in time.
    Timeout,
    /// The provider refused the call and asked the caller to wait.
    RateLimited {
        /// The `Retry-After` hint, when the provider gave one.
        retry_after: Option<Duration>,
    },
    /// Any other normalized failure.
    Fail(ProviderError),
    /// Exactly these stream events, in this order.
    Stream(Vec<StreamEvent>),
    /// This text, delivered in these chunks. Equivalent to a [`Self::Stream`]
    /// of one text delta per chunk followed by a stop, and shorter to write.
    Chunks(Vec<String>),
    /// A document built from the request's output schema, for a task whose schema
    /// depends on its input.
    FromSchema(fn(&serde_json::Value) -> serde_json::Value),
}

impl ScriptedReply {
    /// Prose.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }

    /// A refusal.
    #[must_use]
    pub fn refusal(text: impl Into<String>) -> Self {
        Self::Refusal(text.into())
    }

    /// What the acknowledge and answer tasks write: a document holding `text`.
    #[must_use]
    pub fn written(text: impl Into<String>) -> Self {
        Self::Json(serde_json::json!({ "text": text.into() }))
    }

    /// An answer the answer task writes.
    #[must_use]
    pub fn answer(text: impl Into<String>) -> Self {
        Self::Json(serde_json::json!({ "kind": "answered", "text": text.into() }))
    }

    /// The answer task saying the facts do not answer the question.
    #[must_use]
    pub fn cannot_answer(reason: impl Into<String>) -> Self {
        Self::Json(serde_json::json!({ "kind": "cannot_answer", "text": reason.into() }))
    }

    /// A review that passes the reply: each check it was asked answered in its favour.
    #[must_use]
    pub fn review_passes() -> Self {
        Self::FromSchema(|schema| review(schema, false))
    }

    /// A review that finds the reply claims what its material does not hold.
    #[must_use]
    pub fn review_fails() -> Self {
        Self::FromSchema(|schema| review(schema, true))
    }

    /// A rate limit with a `Retry-After` hint in seconds.
    #[must_use]
    pub fn rate_limited_after(seconds: u64) -> Self {
        Self::RateLimited {
            retry_after: Some(Duration::from_secs(seconds)),
        }
    }

    /// A text answer split into chunks.
    #[must_use]
    pub fn chunks<I, S>(chunks: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self::Chunks(chunks.into_iter().map(Into::into).collect())
    }

    /// Stable snake-case label of the variant, safe in a failure message: it
    /// never carries the scripted value.
    #[must_use]
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Json(_) => "json",
            Self::Text(_) => "text",
            Self::ToolCall { .. } => "tool_call",
            Self::MalformedJson(_) => "malformed_json",
            Self::Refusal(_) => "refusal",
            Self::Timeout => "timeout",
            Self::RateLimited { .. } => "rate_limited",
            Self::Fail(_) => "fail",
            Self::Stream(_) => "stream",
            Self::Chunks(_) => "chunks",
            Self::FromSchema(_) => "from_schema",
        }
    }
}

/// A review answering every check its schema asks: the reply asks its ask, and claims
/// beyond its material only when `claims`.
fn review(schema: &serde_json::Value, claims: bool) -> serde_json::Value {
    let mut document = serde_json::Map::new();
    let checks = schema["properties"]
        .as_object()
        .into_iter()
        .flat_map(|p| p.keys());
    for check in checks {
        let answer = match check.as_str() {
            "reasoning" => serde_json::Value::from("Judged against the material."),
            "asks_the_ask" => serde_json::Value::from(true),
            "claims_beyond_material" => serde_json::Value::from(claims),
            _ => serde_json::Value::from(false),
        };
        document.insert(check.clone(), answer);
    }
    serde_json::Value::Object(document)
}

/// One entry of a script: what to answer, and what the call is expected to be.
#[derive(Debug, Clone)]
pub struct ScriptStep {
    /// The answer.
    pub reply: ScriptedReply,
    /// Purpose the call must carry. `None` accepts any purpose.
    pub expected_purpose: Option<ModelPurpose>,
}

impl ScriptStep {
    /// A step that accepts any call.
    #[must_use]
    pub fn new(reply: ScriptedReply) -> Self {
        Self {
            reply,
            expected_purpose: None,
        }
    }

    /// Restricts the step to one purpose.
    #[must_use]
    pub fn expecting(mut self, purpose: ModelPurpose) -> Self {
        self.expected_purpose = Some(purpose);
        self
    }
}

/// One request the provider received.
///
/// The point of recording is to assert what the runtime *sent*, not only what
/// it did with the answer: the catalog reaches a model as the output schema
/// and the declared tools, so [`schema_mentions`](Self::schema_mentions) and
/// [`tool_names`](Self::tool_names) are how a test checks that a workflow's
/// operations were actually offered.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedCall {
    /// Zero-based position in the call sequence.
    pub index: usize,
    /// `true` when the call arrived through
    /// [`ModelProvider::stream`], `false` through
    /// [`generate`](ModelProvider::generate).
    pub streamed: bool,
    /// The request, verbatim.
    pub request: ModelRequest,
}

impl RecordedCall {
    /// Why the call was made.
    #[must_use]
    pub fn purpose(&self) -> ModelPurpose {
        self.request.purpose
    }

    /// The JSON Schema the call demanded, when it demanded one.
    #[must_use]
    pub fn schema(&self) -> Option<&serde_json::Value> {
        self.request.output.schema()
    }

    /// Name the schema was labelled with.
    #[must_use]
    pub fn schema_name(&self) -> Option<&str> {
        match &self.request.output {
            turnframe_provider::request::OutputSpec::Json { name, .. } => Some(name),
            _ => None,
        }
    }

    /// Returns `true` when `needle` occurs anywhere in the serialized schema.
    ///
    /// This is how a test asks "was `trip.set_name` in the catalog this
    /// call offered?" without depending on how the schema is shaped.
    #[must_use]
    pub fn schema_mentions(&self, needle: &str) -> bool {
        self.schema()
            .is_some_and(|schema| schema.to_string().contains(needle))
    }

    /// Names of the read-only tools the call declared, in order.
    #[must_use]
    pub fn tool_names(&self) -> Vec<&str> {
        self.request
            .tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect()
    }

    /// Every message as `(role, flattened text)`, in order.
    #[must_use]
    pub fn messages(&self) -> Vec<(Role, String)> {
        self.request
            .messages
            .iter()
            .map(|message| (message.role, message.text()))
            .collect()
    }

    /// The concatenated text of every user message.
    #[must_use]
    pub fn user_text(&self) -> String {
        self.request
            .messages
            .iter()
            .filter(|message| message.role == Role::User)
            .map(turnframe_provider::request::Message::text)
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Returns `true` when `needle` occurs in the system prompt or in any
    /// message of the call.
    #[must_use]
    pub fn prompt_mentions(&self, needle: &str) -> bool {
        if self
            .request
            .system
            .as_deref()
            .is_some_and(|system| system.contains(needle))
        {
            return true;
        }
        self.request
            .messages
            .iter()
            .any(|message| message.text().contains(needle))
    }
}

/// A call the script did not anticipate, or a step it never reached.
///
/// Every variant names positions, purposes and labels — never a prompt, a
/// scripted value or anything a user typed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ScriptViolation {
    /// A call arrived after the script ran out of steps.
    #[error("call {call_index} ({purpose}) arrived after the script ran out of steps")]
    UnexpectedCall {
        /// Position of the call.
        call_index: usize,
        /// Purpose the call carried.
        purpose: &'static str,
    },
    /// A call arrived for another purpose than the next step expected.
    #[error("call {call_index} carried purpose {found}, but the next step expected {expected}")]
    WrongPurpose {
        /// Position of the call.
        call_index: usize,
        /// Purpose the step expected.
        expected: &'static str,
        /// Purpose the call carried.
        found: &'static str,
    },
    /// The script still had steps when it was verified.
    #[error("{remaining} scripted step(s) were never reached; the next one would answer {next}")]
    StepsUnused {
        /// How many steps are left.
        remaining: usize,
        /// Label of the first unused reply.
        next: &'static str,
    },
}

/// A [`ModelProvider`] that answers from a script and refuses to improvise.
///
/// Steps are consumed in order. When the script runs out, the provider does
/// **not** fall back to a default answer: it records a
/// [`ScriptViolation::UnexpectedCall`] and fails the call with
/// [`UNEXPECTED_CALL_CODE`], so a runtime that calls a model one more time than
/// the test believed cannot pass silently.
///
/// ```
/// use turnframe_provider::prelude::*;
/// use turnframe_test::providers::{ScriptedProvider, ScriptedReply};
///
/// # futures::executor::block_on(async {
/// let provider = ScriptedProvider::builder("fake", "m")
///     .reply(ScriptedReply::text("una frase"))
///     .build();
///
/// let request = ModelRequest::new(ModelPurpose::Acknowledge).with_message(Message::user("ciao"));
/// assert_eq!(provider.generate(request.clone()).await.unwrap().text(), "una frase");
///
/// // The second call was never scripted.
/// assert!(provider.generate(request).await.is_err());
/// assert!(provider.verify().is_err());
/// # });
/// ```
pub struct ScriptedProvider {
    profile: ModelProfile,
    steps: Mutex<VecDeque<ScriptStep>>,
    calls: Mutex<Vec<RecordedCall>>,
    violations: Mutex<Vec<ScriptViolation>>,
    usage: TokenUsage,
    latency: Duration,
}

/// Collects steps and capabilities for a [`ScriptedProvider`].
#[derive(Debug)]
pub struct ScriptedProviderBuilder {
    profile: ModelProfile,
    steps: VecDeque<ScriptStep>,
    usage: TokenUsage,
    latency: Duration,
}

impl ScriptedProviderBuilder {
    /// Declares capabilities, replacing the defaults.
    ///
    /// They may be dishonest on purpose: a test of the no-silent-downgrade rule
    /// needs a profile that claims more than it delivers.
    #[must_use]
    pub fn capabilities(mut self, capabilities: ProviderCapabilities) -> Self {
        self.profile.capabilities = capabilities;
        self
    }

    /// Declares streaming support, which [`ModelProvider::stream`] refuses
    /// without.
    #[must_use]
    pub fn streaming(mut self) -> Self {
        self.profile.capabilities = self.profile.capabilities.with_streaming(true);
        self
    }

    /// Declares a structured-output transport.
    #[must_use]
    pub fn structured_output(mut self, capability: StructuredOutputCapability) -> Self {
        self.profile.capabilities = self.profile.capabilities.with_structured_output(capability);
        self
    }

    /// Declares a region, so residency routing can be exercised.
    #[must_use]
    pub fn region(mut self, region: impl Into<String>) -> Self {
        self.profile.region = Some(region.into());
        self
    }

    /// Declares per-million prices, so cost ceilings can be exercised.
    #[must_use]
    pub fn cost(mut self, input: MicroCents, output: MicroCents) -> Self {
        self.profile.cost_per_million_input = Some(input);
        self.profile.cost_per_million_output = Some(output);
        self
    }

    /// Reports this usage on every successful answer.
    #[must_use]
    pub fn usage(mut self, usage: TokenUsage) -> Self {
        self.usage = usage;
        self
    }

    /// Reports this latency on every successful answer.
    #[must_use]
    pub fn latency(mut self, latency: Duration) -> Self {
        self.latency = latency;
        self
    }

    /// Appends one step.
    #[must_use]
    pub fn step(mut self, step: ScriptStep) -> Self {
        self.steps.push_back(step);
        self
    }

    /// Appends a reply that accepts any purpose.
    #[must_use]
    pub fn reply(self, reply: ScriptedReply) -> Self {
        self.step(ScriptStep::new(reply))
    }

    /// Appends a reply that only accepts calls made for `purpose`.
    #[must_use]
    pub fn reply_to(self, purpose: ModelPurpose, reply: ScriptedReply) -> Self {
        self.step(ScriptStep::new(reply).expecting(purpose))
    }

    /// Appends prose.
    #[must_use]
    pub fn text(self, text: impl Into<String>) -> Self {
        self.reply(ScriptedReply::text(text))
    }

    /// Appends the acknowledgement a turn writes, `text`, and the review that passes it.
    #[must_use]
    pub fn acknowledging(self, text: impl Into<String>) -> Self {
        self.reply_to(ModelPurpose::Acknowledge, ScriptedReply::written(text))
            .reply_to(ModelPurpose::Review, ScriptedReply::review_passes())
    }

    /// Appends one answer, `text`.
    #[must_use]
    pub fn answering(self, text: impl Into<String>) -> Self {
        self.reply_to(ModelPurpose::Answer, ScriptedReply::answer(text))
    }

    /// Appends a body that is not JSON.
    #[must_use]
    pub fn malformed_json(self, body: impl Into<String>) -> Self {
        self.reply(ScriptedReply::MalformedJson(body.into()))
    }

    /// Appends a refusal.
    #[must_use]
    pub fn refusing(self, text: impl Into<String>) -> Self {
        self.reply(ScriptedReply::refusal(text))
    }

    /// Appends a timeout.
    #[must_use]
    pub fn timing_out(self) -> Self {
        self.reply(ScriptedReply::Timeout)
    }

    /// Appends a rate limit.
    #[must_use]
    pub fn rate_limited(self, retry_after: Option<Duration>) -> Self {
        self.reply(ScriptedReply::RateLimited { retry_after })
    }

    /// Appends a normalized failure.
    #[must_use]
    pub fn failing(self, error: ProviderError) -> Self {
        self.reply(ScriptedReply::Fail(error))
    }

    /// Appends a stream delivered in these chunks.
    #[must_use]
    pub fn streaming_chunks<I, S>(self, chunks: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.streaming().reply(ScriptedReply::chunks(chunks))
    }

    /// Builds the provider.
    #[must_use]
    pub fn build(self) -> ScriptedProvider {
        ScriptedProvider {
            profile: self.profile,
            steps: Mutex::new(self.steps),
            calls: Mutex::new(Vec::new()),
            violations: Mutex::new(Vec::new()),
            usage: self.usage,
            latency: self.latency,
        }
    }

    /// Builds the provider behind an [`Arc`], the shape routing and fallback
    /// take.
    #[must_use]
    pub fn build_shared(self) -> Arc<ScriptedProvider> {
        Arc::new(self.build())
    }
}

impl ScriptedProvider {
    /// A builder for a provider that declares native JSON Schema output,
    /// parallel tool calling and no streaming.
    ///
    /// Those are the capabilities a mutation-capable interpretation stage
    /// requires (spec §20.4), which is what most tests need; call
    /// [`capabilities`](ScriptedProviderBuilder::capabilities) to say something
    /// else.
    #[must_use]
    pub fn builder(
        provider: impl Into<ProviderKey>,
        model: impl Into<ModelKey>,
    ) -> ScriptedProviderBuilder {
        ScriptedProviderBuilder {
            profile: ModelProfile::new(
                provider,
                model,
                ProviderCapabilities::minimal()
                    .with_structured_output(StructuredOutputCapability::NativeJsonSchema)
                    .with_tool_calling(ToolCallingCapability::Parallel)
                    .with_preserves_call_ids(true),
            ),
            steps: VecDeque::new(),
            usage: TokenUsage::none(),
            latency: Duration::ZERO,
        }
    }

    /// The one-line common case: a provider that acknowledges one turn with `text`,
    /// passes its review, and refuses everything else.
    #[must_use]
    pub fn narrating(text: impl Into<String>) -> Self {
        Self::builder("scripted", "narrator-1")
            .acknowledging(text)
            .build()
    }

    /// The routing profile, as configured.
    #[must_use]
    pub fn profile_ref(&self) -> &ModelProfile {
        &self.profile
    }

    /// Wraps a shared provider as a healthy routing candidate.
    #[must_use]
    pub fn candidate(provider: Arc<Self>) -> ProviderCandidate {
        let profile = provider.profile.clone();
        ProviderCandidate {
            provider,
            profile,
            healthy: true,
        }
    }

    /// Every request received, in order.
    #[must_use]
    pub fn calls(&self) -> Vec<RecordedCall> {
        self.lock(&self.calls).clone()
    }

    /// How many requests were received.
    #[must_use]
    pub fn call_count(&self) -> usize {
        self.lock(&self.calls).len()
    }

    /// The `index`-th request received.
    #[must_use]
    pub fn nth_call(&self, index: usize) -> Option<RecordedCall> {
        self.lock(&self.calls).get(index).cloned()
    }

    /// The most recent request received.
    #[must_use]
    pub fn last_call(&self) -> Option<RecordedCall> {
        self.lock(&self.calls).last().cloned()
    }

    /// Every request made for one purpose, in order.
    #[must_use]
    pub fn calls_for(&self, purpose: ModelPurpose) -> Vec<RecordedCall> {
        self.lock(&self.calls)
            .iter()
            .filter(|call| call.request.purpose == purpose)
            .cloned()
            .collect()
    }

    /// How many steps are still unused.
    #[must_use]
    pub fn remaining_steps(&self) -> usize {
        self.lock(&self.steps).len()
    }

    /// Every violation the script observed, in order.
    #[must_use]
    pub fn violations(&self) -> Vec<ScriptViolation> {
        self.lock(&self.violations).clone()
    }

    /// Checks that the script was followed exactly: no unanticipated call and
    /// no unused step.
    ///
    /// A call the script did not anticipate already failed at the time it was
    /// made; this is how the *test* learns about it even when the code under
    /// test swallowed the error.
    ///
    /// # Errors
    ///
    /// The first [`ScriptViolation`] observed, or
    /// [`ScriptViolation::StepsUnused`] when steps remain.
    pub fn verify(&self) -> Result<(), ScriptViolation> {
        if let Some(violation) = self.lock(&self.violations).first() {
            return Err(violation.clone());
        }
        let steps = self.lock(&self.steps);
        match steps.front() {
            None => Ok(()),
            Some(next) => Err(ScriptViolation::StepsUnused {
                remaining: steps.len(),
                next: next.reply.label(),
            }),
        }
    }

    /// Appends a step to a provider already in use, for a turn whose script
    /// depends on identifiers an earlier turn produced.
    pub fn push(&self, step: ScriptStep) {
        self.lock(&self.steps).push_back(step);
    }

    /// Forgets the recorded calls and violations, keeping the remaining script.
    pub fn clear_calls(&self) {
        self.lock(&self.calls).clear();
        self.lock(&self.violations).clear();
    }

    /// Locks, recovering from poisoning: a test that already failed must not
    /// cascade into unrelated failures.
    fn lock<'a, T>(&self, target: &'a Mutex<T>) -> MutexGuard<'a, T> {
        target.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn record_violation(&self, violation: ScriptViolation) -> ProviderError {
        let error = match &violation {
            ScriptViolation::WrongPurpose { .. } => ProviderError::other(WRONG_PURPOSE_CODE),
            _ => ProviderError::other(UNEXPECTED_CALL_CODE),
        };
        self.lock(&self.violations).push(violation);
        error.with_model(&self.profile.reference())
    }

    /// Records the call and takes the next step, or explains why there is none.
    fn take_step(
        &self,
        request: &ModelRequest,
        streamed: bool,
    ) -> Result<ScriptStep, ProviderError> {
        let index = {
            let mut calls = self.lock(&self.calls);
            let index = calls.len();
            calls.push(RecordedCall {
                index,
                streamed,
                request: request.clone(),
            });
            index
        };
        let Some(step) = self.lock(&self.steps).pop_front() else {
            return Err(self.record_violation(ScriptViolation::UnexpectedCall {
                call_index: index,
                purpose: request.purpose.as_str(),
            }));
        };
        if let Some(expected) = step.expected_purpose
            && expected != request.purpose
        {
            return Err(self.record_violation(ScriptViolation::WrongPurpose {
                call_index: index,
                expected: expected.as_str(),
                found: request.purpose.as_str(),
            }));
        }
        Ok(step)
    }

    /// Turns a reply into a response.
    fn respond(
        &self,
        request: &ModelRequest,
        reply: &ScriptedReply,
    ) -> Result<ModelResponse, ProviderError> {
        let base = ModelResponse::new(
            request.request_id,
            self.profile.provider.clone(),
            self.profile.model.clone(),
        )
        .with_usage(self.usage)
        .with_latency(self.latency);
        match reply {
            ScriptedReply::Json(value) => Ok(base.with_text(value.to_string())),
            ScriptedReply::FromSchema(build) => {
                let schema = request.output.schema().cloned().unwrap_or_default();
                Ok(base.with_text(build(&schema).to_string()))
            }
            ScriptedReply::Text(text) => Ok(base.with_text(text.clone())),
            ScriptedReply::ToolCall {
                id,
                name,
                arguments,
            } => Ok(base
                .with_tool_call(ToolCall::new(id.clone(), name.clone(), arguments.clone()))
                .with_finish(FinishReason::ToolCalls)),
            ScriptedReply::MalformedJson(body) => Ok(base.with_text(body.clone())),
            ScriptedReply::Refusal(text) => Ok(base
                .with_text(text.clone())
                .with_finish(FinishReason::Refusal)),
            ScriptedReply::Timeout => Err(self.label(ProviderError::timeout())),
            ScriptedReply::RateLimited { retry_after } => {
                Err(self.label(ProviderError::rate_limited(*retry_after)))
            }
            ScriptedReply::Fail(error) => Err(self.label(error.clone())),
            ScriptedReply::Stream(events) => self.reassemble(request, events.clone()),
            ScriptedReply::Chunks(chunks) => self.reassemble(request, chunk_events(chunks)),
        }
    }

    /// Renders a reply as the events a stream would deliver.
    fn events_for(&self, reply: &ScriptedReply, response: &ModelResponse) -> Vec<StreamEvent> {
        match reply {
            ScriptedReply::Stream(events) => return events.clone(),
            ScriptedReply::Chunks(chunks) => return chunk_events(chunks),
            _ => {}
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

    /// Rebuilds the whole answer from scripted events, so `generate` and
    /// `stream` never disagree about the same step.
    fn reassemble(
        &self,
        request: &ModelRequest,
        events: Vec<StreamEvent>,
    ) -> Result<ModelResponse, ProviderError> {
        let mut accumulator = StreamAccumulator::new(
            request.request_id,
            self.profile.provider.clone(),
            self.profile.model.clone(),
        )
        .with_latency(self.latency);
        for event in events {
            accumulator.push(event)?;
        }
        accumulator.finish()
    }

    fn label(&self, error: ProviderError) -> ProviderError {
        error.with_model(&self.profile.reference())
    }
}

/// One text delta per chunk, then a stop.
fn chunk_events(chunks: &[String]) -> Vec<StreamEvent> {
    let mut events: Vec<StreamEvent> = chunks.iter().map(StreamEvent::text).collect();
    events.push(StreamEvent::Finish {
        reason: FinishReason::Stop,
    });
    events
}

#[async_trait]
impl ModelProvider for ScriptedProvider {
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
        let step = self.take_step(&request, false)?;
        self.respond(&request, &step.reply)
    }

    async fn stream(&self, request: ModelRequest) -> Result<ModelStream, ProviderError> {
        if !self.profile.capabilities.streaming {
            // The double stays honest: a profile that declares no streaming
            // does not get a stream synthesized for it.
            return Err(self.label(ProviderError::unsupported("streaming")));
        }
        let step = self.take_step(&request, true)?;
        let response = self.respond(&request, &step.reply)?;
        Ok(ModelStream::from_events(
            self.events_for(&step.reply, &response),
        ))
    }
}

impl fmt::Debug for ScriptedProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScriptedProvider")
            .field("model", &self.profile.reference().to_string())
            .field("remaining_steps", &self.remaining_steps())
            .field("calls", &self.call_count())
            .field("violations", &self.lock(&self.violations).len())
            .finish_non_exhaustive()
    }
}

/// Concatenated text of every part of a response, for a test that only cares
/// about the prose.
#[must_use]
pub fn response_text(response: &ModelResponse) -> String {
    response
        .content
        .iter()
        .filter_map(ContentPart::as_text)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use turnframe_provider::ids::RequestId;
    use turnframe_provider::request::{Message, ModelRequest, OutputSpec, ToolSpec};
    use turnframe_provider::stream::reconstruct;

    fn interpret() -> ModelRequest {
        ModelRequest::new(ModelPurpose::Extract)
            .with_request_id(RequestId::nil())
            .with_system("Propose acts for trip.set_name.")
            .with_message(Message::user("cambia l'oggetto"))
            .with_output(OutputSpec::json(
                "user_turn_plan",
                serde_json::json!({"operations": ["trip.set_name"]}),
            ))
            .with_tools(vec![ToolSpec::new(
                "case.get",
                "load a case",
                serde_json::json!({}),
            )])
    }

    #[tokio::test]
    async fn the_script_is_consumed_in_order() {
        let provider = ScriptedProvider::builder("fake", "m")
            .text("first")
            .reply(ScriptedReply::Json(serde_json::json!({"acts": []})))
            .build();
        let first = provider.generate(interpret()).await.unwrap();
        assert_eq!(first.text(), "first");
        let second = provider.generate(interpret()).await.unwrap();
        assert_eq!(second.text(), r#"{"acts":[]}"#);
        assert_eq!(provider.remaining_steps(), 0);
        assert!(provider.verify().is_ok());
    }

    #[tokio::test]
    async fn a_call_the_script_did_not_anticipate_fails_loudly() {
        let provider = ScriptedProvider::builder("fake", "m").text("once").build();
        assert!(provider.generate(interpret()).await.is_ok());

        let error = provider.generate(interpret()).await.unwrap_err();
        assert_eq!(
            error
                .code()
                .map(turnframe_provider::error::ErrorCode::as_str),
            Some(UNEXPECTED_CALL_CODE)
        );
        assert_eq!(
            provider.verify().unwrap_err(),
            ScriptViolation::UnexpectedCall {
                call_index: 1,
                purpose: "extract",
            }
        );
    }

    #[tokio::test]
    async fn a_step_bound_to_a_purpose_refuses_another_one() {
        let provider = ScriptedProvider::builder("fake", "m")
            .reply_to(ModelPurpose::Extract, ScriptedReply::text("{}"))
            .build();
        let error = provider
            .generate(ModelRequest::new(ModelPurpose::Acknowledge))
            .await
            .unwrap_err();
        assert_eq!(
            error
                .code()
                .map(turnframe_provider::error::ErrorCode::as_str),
            Some(WRONG_PURPOSE_CODE)
        );
        assert!(matches!(
            provider.verify().unwrap_err(),
            ScriptViolation::WrongPurpose { .. }
        ));
    }

    #[test]
    fn an_unused_step_is_a_violation() {
        let provider = ScriptedProvider::builder("fake", "m")
            .text("never asked for")
            .build();
        assert_eq!(
            provider.verify().unwrap_err(),
            ScriptViolation::StepsUnused {
                remaining: 1,
                next: "text",
            }
        );
    }

    #[tokio::test]
    async fn every_request_is_recorded_with_what_was_sent() {
        let provider = ScriptedProvider::builder("fake", "m").text("{}").build();
        provider.generate(interpret()).await.unwrap();

        let call = provider.last_call().unwrap();
        assert_eq!(call.index, 0);
        assert!(!call.streamed);
        assert_eq!(call.purpose(), ModelPurpose::Extract);
        assert_eq!(call.schema_name(), Some("user_turn_plan"));
        assert!(call.schema_mentions("trip.set_name"));
        assert!(!call.schema_mentions("trip.rebook"));
        assert_eq!(call.tool_names(), vec!["case.get"]);
        assert_eq!(call.user_text(), "cambia l'oggetto");
        assert_eq!(
            call.messages(),
            vec![(Role::User, "cambia l'oggetto".to_owned())]
        );
        assert!(call.prompt_mentions("trip.set_name"));
        assert_eq!(provider.calls_for(ModelPurpose::Extract).len(), 1);
        assert_eq!(provider.nth_call(0), Some(call));
    }

    #[tokio::test]
    async fn the_transport_failures_keep_their_families() {
        let provider = ScriptedProvider::builder("fake", "m")
            .timing_out()
            .rate_limited(Some(Duration::from_secs(3)))
            .refusing("non posso")
            .build();
        let timeout = provider
            .generate(ModelRequest::new(ModelPurpose::Acknowledge))
            .await
            .unwrap_err();
        assert_eq!(
            timeout.retry_class(),
            turnframe_provider::error::RetryClass::Retry
        );
        let limited = provider
            .generate(ModelRequest::new(ModelPurpose::Acknowledge))
            .await
            .unwrap_err();
        assert_eq!(limited.retry_after(), Some(Duration::from_secs(3)));
        assert_eq!(limited.model().map(ModelKey::as_str), Some("m"));

        // A refusal is a response, not a transport failure.
        let refusal = provider
            .generate(ModelRequest::new(ModelPurpose::Acknowledge))
            .await
            .unwrap();
        assert_eq!(refusal.finish, FinishReason::Refusal);
        assert!(!refusal.finish.is_complete());
        assert_eq!(response_text(&refusal), "non posso");
    }

    #[tokio::test]
    async fn malformed_json_is_delivered_verbatim() {
        let provider = ScriptedProvider::builder("fake", "m")
            .malformed_json("{\"acts\": [")
            .build();
        let response = provider
            .generate(ModelRequest::new(ModelPurpose::Extract))
            .await
            .unwrap();
        assert!(serde_json::from_str::<serde_json::Value>(&response.text()).is_err());
    }

    #[tokio::test]
    async fn chunks_stream_and_reassemble_to_the_same_answer() {
        let provider = ScriptedProvider::builder("fake", "m")
            .streaming_chunks(["Ho preparato ", "la modifica."])
            .streaming_chunks(["Ho preparato ", "la modifica."])
            .build();
        let request =
            ModelRequest::new(ModelPurpose::Acknowledge).with_request_id(RequestId::nil());

        let items = provider
            .stream(request.clone())
            .await
            .unwrap()
            .collect_items()
            .await;
        assert_eq!(items.len(), 3, "two deltas and a finish");

        let whole = provider.generate(request).await.unwrap();
        assert_eq!(whole.text(), "Ho preparato la modifica.");
        assert!(provider.calls()[0].streamed);
        assert!(!provider.calls()[1].streamed);
    }

    #[tokio::test]
    async fn streaming_is_refused_unless_declared() {
        let provider = ScriptedProvider::builder("fake", "m").text("x").build();
        let error = provider
            .stream(ModelRequest::new(ModelPurpose::Acknowledge))
            .await
            .unwrap_err();
        assert!(matches!(
            error.kind(),
            turnframe_provider::error::ProviderErrorKind::Unsupported { .. }
        ));
        // The refused call consumed no step.
        assert_eq!(provider.remaining_steps(), 1);
    }

    #[tokio::test]
    async fn a_scripted_stream_reconstructs_into_the_generated_answer() {
        let provider = ScriptedProvider::builder("fake", "m")
            .streaming()
            .reply(ScriptedReply::Stream(vec![
                StreamEvent::text("ciao "),
                StreamEvent::text("mondo"),
                StreamEvent::Finish {
                    reason: FinishReason::Stop,
                },
            ]))
            .build();
        let stream = provider
            .stream(ModelRequest::new(ModelPurpose::Acknowledge).with_request_id(RequestId::nil()))
            .await
            .unwrap();
        let rebuilt = reconstruct(
            stream,
            StreamAccumulator::new(RequestId::nil(), "fake", "m"),
        )
        .await
        .unwrap();
        assert_eq!(rebuilt.text(), "ciao mondo");
    }

    #[test]
    fn the_double_renders_its_state_without_the_script() {
        let provider = ScriptedProvider::builder("fake", "m")
            .text("secret")
            .build();
        let rendered = format!("{provider:?}");
        assert!(rendered.contains("fake/m"), "{rendered}");
        assert!(!rendered.contains("secret"), "{rendered}");
    }

    #[test]
    fn the_one_line_constructors_declare_what_they_need() {
        let narrating = ScriptedProvider::narrating("x");
        assert!(narrating.capabilities().structured_output.enforces_schema());
        let candidate = ScriptedProvider::candidate(Arc::new(narrating));
        assert!(candidate.healthy);
        assert_eq!(candidate.reference().to_string(), "scripted/narrator-1");
        assert_eq!(
            ScriptedProvider::narrating("x")
                .profile_ref()
                .model
                .as_str(),
            "narrator-1"
        );
    }

    #[tokio::test]
    async fn recorded_calls_survive_a_clear() {
        let provider = ScriptedProvider::builder("fake", "m")
            .text("a")
            .text("b")
            .build();
        provider
            .generate(ModelRequest::new(ModelPurpose::Acknowledge))
            .await
            .unwrap();
        assert_eq!(provider.call_count(), 1);
        provider.clear_calls();
        assert_eq!(provider.call_count(), 0);
        assert!(provider.violations().is_empty());
        assert_eq!(provider.remaining_steps(), 1);
    }
}

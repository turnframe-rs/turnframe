//! Structured tracing: the end-to-end trace of one turn (spec §26.1).
//!
//! A metric says how often something happened; a trace says which turn it
//! happened to. This module carries the stable identifiers spec §26.1 asks for
//! — turn, conversation, workflow key and version, case id and revision,
//! interaction id, provider, model, attempt, plan hash, command id, event ids,
//! response block ids and outbox id — as structured `tracing` fields.
//!
//! Two rules hold everywhere here:
//!
//! * **Identifiers and codes only.** Nothing the user wrote, nothing a case
//!   contains and no provider payload is ever a field. [`TurnIdentifiers`] has
//!   no free-text member, so there is no place to put one.
//! * **The account id is never logged raw.** [`account_hash`] logs a truncated
//!   digest instead, which correlates a tenant's turns without naming the
//!   tenant (spec §25.5).

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use tracing::field::Empty;
use turnframe_core::hash::{Digest, digest_hex};
use turnframe_core::ids::{
    AccountId, BlockId, CaseId, CaseRevision, CommandId, ConversationId, EventId, InteractionId,
    OutboxId, TurnId, WorkflowKey, WorkflowVersion,
};
use turnframe_core::observe::{Observer, Signal, SignalLabels};
use turnframe_core::replay::{ProviderAttemptRecord, ReplayRecord};

use crate::{attrs, opt_str};

/// Tracing target of every event and span this crate emits.
pub const TRACE_TARGET: &str = "turnframe";

/// Domain separator for the account digest, so the same account id hashed for
/// another purpose does not produce the same value.
const ACCOUNT_HASH_DOMAIN: &str = "turnframe.account";

/// Number of hexadecimal characters kept from the account digest. Sixteen
/// characters (64 bits) keep collisions negligible at any realistic tenant
/// count while staying short enough to read in a log line.
pub const ACCOUNT_HASH_LEN: usize = 16;

/// One stage of the turn pipeline, used as the name of a child span.
///
/// The set is the deterministic path of the architecture, so a trace of a turn
/// reads as the pipeline it went through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum PipelineStage {
    /// Projecting persisted state into workflow views.
    Projection,
    /// The small model tasks that understand the message.
    Understanding,
    /// Server-side target resolution.
    TargetResolution,
    /// Whole-turn reduction.
    Reduction,
    /// Policy evaluation over the planned commands.
    Policy,
    /// Command execution and commit.
    Execution,
    /// Dispatch of an external side effect.
    ExternalDispatch,
    /// Composition of the response blocks.
    Composition,
    /// The tasks that write and review the reply.
    Narration,
    /// Persistence of the turn and its replay record.
    Persistence,
    /// Reconciliation of an unknown external outcome.
    Reconciliation,
}

impl PipelineStage {
    /// Every stage, in pipeline order.
    pub const ALL: [Self; 11] = [
        Self::Projection,
        Self::Understanding,
        Self::TargetResolution,
        Self::Reduction,
        Self::Policy,
        Self::Execution,
        Self::ExternalDispatch,
        Self::Composition,
        Self::Narration,
        Self::Persistence,
        Self::Reconciliation,
    ];

    /// The stage name as it appears on the span.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Projection => "projection",
            Self::Understanding => "understanding",
            Self::TargetResolution => "target_resolution",
            Self::Reduction => "reduction",
            Self::Policy => "policy",
            Self::Execution => "execution",
            Self::ExternalDispatch => "external_dispatch",
            Self::Composition => "composition",
            Self::Narration => "narration",
            Self::Persistence => "persistence",
            Self::Reconciliation => "reconciliation",
        }
    }
}

impl std::fmt::Display for PipelineStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Returns a short, domain-separated digest of an account id.
///
/// The raw tenant identifier never reaches a log line; the digest is stable for
/// the life of the account, so every turn of one tenant still groups together
/// during an investigation.
#[must_use]
pub fn account_hash(account_id: &AccountId) -> String {
    let mut material =
        String::with_capacity(ACCOUNT_HASH_DOMAIN.len() + 1 + account_id.as_str().len());
    material.push_str(ACCOUNT_HASH_DOMAIN);
    material.push('\0');
    material.push_str(account_id.as_str());
    let mut digest = digest_hex(material.as_bytes());
    digest.truncate(ACCOUNT_HASH_LEN);
    digest
}

/// The stable identifiers of one turn (spec §26.1).
///
/// Every member is an identifier, a version label or a digest. Build one with
/// [`TurnIdentifiers::of_turn`] and fill in what a stage knows, or derive a
/// whole one from a persisted [`ReplayRecord`] with
/// [`TurnIdentifiers::from_replay`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct TurnIdentifiers {
    /// The turn.
    pub turn_id: Option<TurnId>,
    /// The conversation the turn belongs to.
    pub conversation_id: Option<ConversationId>,
    /// Digest of the tenant, from [`account_hash`]. Never the raw account id.
    pub account_hash: Option<String>,
    /// Workflow in force.
    pub workflow: Option<WorkflowKey>,
    /// Version of that workflow.
    pub workflow_version: Option<WorkflowVersion>,
    /// Case the turn acted on.
    pub case_id: Option<CaseId>,
    /// Revision that case was loaded at.
    pub case_revision: Option<CaseRevision>,
    /// Interaction created or answered.
    pub interaction_id: Option<InteractionId>,
    /// Provider key of the model call.
    pub provider: Option<String>,
    /// Model key of the model call.
    pub model: Option<String>,
    /// Attempt within the stage.
    pub attempt: Option<String>,
    /// Hash of the accepted, normalized plan.
    pub plan_hash: Option<Digest>,
    /// Command the event concerns.
    pub command_id: Option<CommandId>,
    /// Events committed by the turn.
    pub event_ids: Vec<EventId>,
    /// Response block ids, in order.
    pub block_ids: Vec<BlockId>,
    /// Outbox row of an external side effect.
    pub outbox_id: Option<OutboxId>,
}

impl TurnIdentifiers {
    /// The identifiers every stage of a turn knows.
    #[must_use]
    pub fn of_turn(
        turn_id: TurnId,
        conversation_id: ConversationId,
        account_id: &AccountId,
    ) -> Self {
        Self {
            turn_id: Some(turn_id),
            conversation_id: Some(conversation_id),
            account_hash: Some(account_hash(account_id)),
            ..Self::default()
        }
    }

    /// Everything spec §26.1 asks for that a persisted replay record holds.
    ///
    /// The workflow, case and interaction taken are the first of each list and
    /// the provider attempt the last, which is the one a failure concerns; the
    /// full lists stay in the replay record itself.
    #[must_use]
    pub fn from_replay(record: &ReplayRecord) -> Self {
        let workflow_version = record.workflow_versions.first();
        let case = record.loaded_cases.first();
        let attempt = record.provider_attempts.last();
        let command = record
            .command_outcomes
            .first()
            .map(|outcome| outcome.command_ref.command_id);

        Self {
            turn_id: Some(record.turn_id),
            conversation_id: Some(record.conversation_id),
            account_hash: Some(account_hash(&record.account_id)),
            workflow: workflow_version.map(|entry| entry.key.clone()),
            workflow_version: workflow_version.map(|entry| entry.version.clone()),
            case_id: case.map(|case| case.case_id.clone()),
            case_revision: case.map(|case| case.expected_revision),
            interaction_id: record.interactions_created.first().copied(),
            provider: attempt.map(|attempt| attempt.provider_key.to_string()),
            model: attempt.map(|attempt| attempt.model_key.to_string()),
            attempt: attempt.map(|attempt| attempt.attempt.to_string()),
            plan_hash: record.plan_hash.clone(),
            command_id: command,
            event_ids: record.event_ids.clone(),
            block_ids: record.response_block_ids.clone(),
            outbox_id: record.outbox_ids.first().copied(),
        }
    }

    /// Renders the identifiers as `(field name, value)` pairs in a stable
    /// order, omitting what is not known.
    ///
    /// This is the exact set a subscriber sees; it is a plain function so a
    /// test can assert on it without standing up a subscriber.
    #[must_use]
    pub fn fields(&self) -> Vec<(&'static str, String)> {
        let mut fields: Vec<(&'static str, String)> = Vec::new();
        let mut push = |name: &'static str, value: Option<String>| {
            if let Some(value) = value {
                fields.push((name, value));
            }
        };

        push(field::TURN_ID, self.turn_id.map(|id| id.to_string()));
        push(
            field::CONVERSATION_ID,
            self.conversation_id.map(|id| id.to_string()),
        );
        push(field::ACCOUNT_HASH, self.account_hash.clone());
        push(field::WORKFLOW, opt_str(&self.workflow).map(str::to_owned));
        push(
            field::WORKFLOW_VERSION,
            opt_str(&self.workflow_version).map(str::to_owned),
        );
        push(field::CASE_ID, opt_str(&self.case_id).map(str::to_owned));
        push(
            field::CASE_REVISION,
            self.case_revision.map(|revision| revision.to_string()),
        );
        push(
            field::INTERACTION_ID,
            self.interaction_id.map(|id| id.to_string()),
        );
        push(field::PROVIDER, opt_str(&self.provider).map(str::to_owned));
        push(field::MODEL, opt_str(&self.model).map(str::to_owned));
        push(field::ATTEMPT, opt_str(&self.attempt).map(str::to_owned));
        push(
            field::PLAN_HASH,
            self.plan_hash.as_ref().map(|hash| hash.as_str().to_owned()),
        );
        push(field::COMMAND_ID, self.command_id.map(|id| id.to_string()));
        push(field::EVENT_IDS, join_ids(&self.event_ids));
        push(field::BLOCK_IDS, join_ids(&self.block_ids));
        push(field::OUTBOX_ID, self.outbox_id.map(|id| id.to_string()));
        fields
    }
}

/// Joins a list of identifiers into one comma-separated field value, or `None`
/// when the list is empty.
fn join_ids<T: ToString>(ids: &[T]) -> Option<String> {
    if ids.is_empty() {
        return None;
    }
    Some(
        ids.iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(","),
    )
}

/// Names of the structured fields this module emits (spec §26.1).
pub mod field {
    /// The turn.
    pub const TURN_ID: &str = "turn_id";
    /// The conversation.
    pub const CONVERSATION_ID: &str = "conversation_id";
    /// Digest of the tenant.
    pub const ACCOUNT_HASH: &str = "account_hash";
    /// Workflow key.
    pub const WORKFLOW: &str = "workflow";
    /// Workflow version.
    pub const WORKFLOW_VERSION: &str = "workflow_version";
    /// Case identifier.
    pub const CASE_ID: &str = "case_id";
    /// Case revision.
    pub const CASE_REVISION: &str = "case_revision";
    /// Interaction identifier.
    pub const INTERACTION_ID: &str = "interaction_id";
    /// Provider key.
    pub const PROVIDER: &str = "provider";
    /// Model key.
    pub const MODEL: &str = "model";
    /// Provider attempt.
    pub const ATTEMPT: &str = "attempt";
    /// Hash of the accepted plan.
    pub const PLAN_HASH: &str = "plan_hash";
    /// Command identifier.
    pub const COMMAND_ID: &str = "command_id";
    /// Committed event identifiers.
    pub const EVENT_IDS: &str = "event_ids";
    /// Response block identifiers.
    pub const BLOCK_IDS: &str = "block_ids";
    /// Outbox row identifier.
    pub const OUTBOX_ID: &str = "outbox_id";
    /// The signal a telemetry event reports.
    pub const SIGNAL: &str = "signal";
    /// The pipeline stage a span covers.
    pub const STAGE: &str = "stage";
    /// Measured duration of a latency signal, in milliseconds.
    pub const DURATION_MS: &str = "duration_ms";
    /// Risk class of a command.
    pub const RISK: &str = "risk";
    /// Kind of an interaction.
    pub const INTERACTION: &str = "interaction";
    /// Normalized request purpose.
    pub const PURPOSE: &str = "purpose";
    /// The effort of the turn.
    pub const EFFORT: &str = "effort";
    /// Stable failure or rejection code.
    pub const ERROR_CODE: &str = "error_code";
}

/// Opens the span that covers one whole turn.
///
/// The account id is hashed rather than logged, so a tenant is correlatable but
/// not identifiable from the logs alone.
///
/// ```rust
/// use turnframe_core::ids::{AccountId, ConversationId, TurnId};
/// use turnframe_telemetry::tracing::turn_span;
///
/// let span = turn_span(TurnId::nil(), ConversationId::nil(), &AccountId::from("acct-1"));
/// let _entered = span.enter();
/// ```
#[must_use]
pub fn turn_span(
    turn_id: TurnId,
    conversation_id: ConversationId,
    account_id: &AccountId,
) -> ::tracing::Span {
    ::tracing::span!(
        target: TRACE_TARGET,
        ::tracing::Level::INFO,
        "turnframe.turn",
        turn_id = %turn_id,
        conversation_id = %conversation_id,
        account_hash = %account_hash(account_id),
        "session.id" = Empty,
        "user.id" = Empty,
        tags = Empty,
        "deployment.environment.name" = Empty,
        "service.version" = Empty,
    )
}

/// Opens a child span for one stage of the pipeline.
///
/// ```rust
/// use turnframe_core::ids::TurnId;
/// use turnframe_telemetry::tracing::{PipelineStage, stage_span};
///
/// let span = stage_span(PipelineStage::Understanding, TurnId::nil());
/// let _entered = span.enter();
/// ```
#[must_use]
pub fn stage_span(stage: PipelineStage, turn_id: TurnId) -> ::tracing::Span {
    ::tracing::span!(
        target: TRACE_TARGET,
        ::tracing::Level::DEBUG,
        "turnframe.stage",
        stage = stage.as_str(),
        turn_id = %turn_id,
        "session.id" = Empty,
        "user.id" = Empty,
        tags = Empty,
        "deployment.environment.name" = Empty,
        "service.version" = Empty,
    )
}

/// One provider call, described with the OpenTelemetry GenAI semantic
/// conventions (`gen_ai.*`).
///
/// These are the attributes every LLM observability backend already reads —
/// Langfuse, Datadog LLM Observability, Phoenix, Braintrust — so a Turnframe
/// application shows up in them as a model call without any vendor adapter.
/// The keys live in [`crate::attrs`] so a provider crate and an application
/// spell them identically.
///
/// Token accounting is the part that is easy to get wrong, so it is explicit
/// here: [`ProviderCall::input_tokens`] is always recorded **net of cached
/// tokens**. A consumer that reads only that field sees the uncached prompt
/// cost; one that adds [`ProviderCall::input_cached_tokens`] sees the whole
/// prompt. Neither double counts. Use
/// [`ProviderCall::with_cached_usage`] when the provider reports a total and a
/// cached figure, and [`ProviderCall::with_usage`] when it already reports the
/// net one.
///
/// No member holds prompt or completion text: content is user data and travels
/// only through [`ContentRecorder`], which is off by default.
#[derive(Debug, Clone, Default, PartialEq)]
#[non_exhaustive]
pub struct ProviderCall {
    /// Provider key, recorded as `gen_ai.system`.
    pub system: Option<String>,
    /// Normalized request purpose, recorded as `gen_ai.operation.name`.
    pub operation: Option<String>,
    /// Model asked for, recorded as `gen_ai.request.model`.
    pub request_model: Option<String>,
    /// Sampling temperature, recorded as `gen_ai.request.temperature`.
    pub temperature: Option<f64>,
    /// The provider's identifier for the response.
    pub response_id: Option<String>,
    /// Model that actually answered, which is not always the one asked for.
    pub response_model: Option<String>,
    /// Why generation stopped.
    pub finish_reasons: Vec<String>,
    /// Input tokens billed, **net of `input_cached_tokens`**.
    pub input_tokens: Option<u64>,
    /// Output tokens generated.
    pub output_tokens: Option<u64>,
    /// Input tokens served from the provider's prompt cache.
    pub input_cached_tokens: Option<u64>,
}

impl ProviderCall {
    /// A call to `system` for `operation` with `model`.
    #[must_use]
    pub fn new(
        system: impl Into<String>,
        operation: impl Into<String>,
        request_model: impl Into<String>,
    ) -> Self {
        Self {
            system: Some(system.into()),
            operation: Some(operation.into()),
            request_model: Some(request_model.into()),
            ..Self::default()
        }
    }

    /// Everything a persisted provider attempt already knows (spec §20.7).
    #[must_use]
    pub fn from_attempt(attempt: &ProviderAttemptRecord) -> Self {
        Self {
            system: Some(attempt.provider_key.to_string()),
            operation: Some(attempt.purpose.to_string()),
            request_model: Some(attempt.model_key.to_string()),
            temperature: attempt.temperature.map(f64::from),
            response_id: Some(attempt.request_id.to_string()),
            response_model: Some(attempt.model_key.to_string()),
            finish_reasons: attempt.finish_reasons.clone(),
            input_tokens: attempt.input_tokens,
            output_tokens: attempt.output_tokens,
            input_cached_tokens: None,
        }
    }

    /// Sets the requested sampling temperature.
    #[must_use]
    pub fn with_temperature(mut self, temperature: f64) -> Self {
        self.temperature = Some(temperature);
        self
    }

    /// Sets the response identifier and the model that answered.
    #[must_use]
    pub fn with_response(
        mut self,
        response_id: impl Into<String>,
        response_model: impl Into<String>,
    ) -> Self {
        self.response_id = Some(response_id.into());
        self.response_model = Some(response_model.into());
        self
    }

    /// Adds a finish reason.
    #[must_use]
    pub fn with_finish_reason(mut self, reason: impl Into<String>) -> Self {
        self.finish_reasons.push(reason.into());
        self
    }

    /// Records usage the provider already reports net of its prompt cache.
    #[must_use]
    pub fn with_usage(mut self, input_tokens: u64, output_tokens: u64) -> Self {
        self.input_tokens = Some(input_tokens);
        self.output_tokens = Some(output_tokens);
        self
    }

    /// Records usage a provider reports as a **total** prompt size plus a
    /// cached figure, and stores the input tokens net of the cache.
    ///
    /// ```rust
    /// use turnframe_telemetry::tracing::ProviderCall;
    ///
    /// let call = ProviderCall::new("openai", "extract", "gpt-x")
    ///     .with_cached_usage(1_000, 800, 120);
    ///
    /// // 200 uncached + 800 cached = the 1_000 the provider reported.
    /// assert_eq!(call.input_tokens, Some(200));
    /// assert_eq!(call.input_cached_tokens, Some(800));
    /// assert_eq!(call.output_tokens, Some(120));
    /// ```
    #[must_use]
    pub fn with_cached_usage(
        mut self,
        total_input_tokens: u64,
        cached_tokens: u64,
        output_tokens: u64,
    ) -> Self {
        self.input_tokens = Some(total_input_tokens.saturating_sub(cached_tokens));
        self.input_cached_tokens = Some(cached_tokens);
        self.output_tokens = Some(output_tokens);
        self
    }

    /// The total prompt size the provider saw: net input plus cached.
    #[must_use]
    pub fn total_input_tokens(&self) -> Option<u64> {
        match (self.input_tokens, self.input_cached_tokens) {
            (None, None) => None,
            (input, cached) => Some(
                input
                    .unwrap_or_default()
                    .saturating_add(cached.unwrap_or_default()),
            ),
        }
    }

    /// The call as `(attribute key, value)` pairs in a stable order, omitting
    /// what is unknown.
    ///
    /// This is the exact set the span carries; it is a plain function so a test
    /// can assert on it without standing up a subscriber, and an adopter can
    /// rename the keys for a backend that wants its own spelling.
    #[must_use]
    pub fn attributes(&self) -> Vec<(&'static str, String)> {
        let mut out: Vec<(&'static str, String)> = Vec::new();
        let mut push = |key: &'static str, value: Option<String>| {
            if let Some(value) = value {
                out.push((key, value));
            }
        };
        push(attrs::GEN_AI_SYSTEM, self.system.clone());
        push(attrs::GEN_AI_OPERATION_NAME, self.operation.clone());
        push(attrs::GEN_AI_REQUEST_MODEL, self.request_model.clone());
        push(
            attrs::GEN_AI_REQUEST_TEMPERATURE,
            self.temperature.map(|value| value.to_string()),
        );
        push(attrs::GEN_AI_RESPONSE_ID, self.response_id.clone());
        push(attrs::GEN_AI_RESPONSE_MODEL, self.response_model.clone());
        push(
            attrs::GEN_AI_RESPONSE_FINISH_REASONS,
            if self.finish_reasons.is_empty() {
                None
            } else {
                Some(self.finish_reasons.join(","))
            },
        );
        push(
            attrs::GEN_AI_USAGE_INPUT_TOKENS,
            self.input_tokens.map(|value| value.to_string()),
        );
        push(
            attrs::GEN_AI_USAGE_OUTPUT_TOKENS,
            self.output_tokens.map(|value| value.to_string()),
        );
        push(
            attrs::GEN_AI_USAGE_INPUT_CACHED_TOKENS,
            self.input_cached_tokens.map(|value| value.to_string()),
        );
        out
    }
}

/// Opens the span of one provider call, carrying the GenAI semantic
/// conventions of [`crate::attrs`].
///
/// The span also declares the trace-grouping fields and the two content fields
/// empty, so [`TraceGrouping::stamp`] and [`ContentRecorder`] can fill them in
/// on the same span.
///
/// ```rust
/// use turnframe_telemetry::tracing::{ProviderCall, provider_call_span};
///
/// let call = ProviderCall::new("openai", "extract", "gpt-x")
///     .with_cached_usage(1_000, 800, 120);
/// let span = provider_call_span(&call);
/// let _entered = span.enter();
/// ```
#[must_use]
pub fn provider_call_span(call: &ProviderCall) -> ::tracing::Span {
    ::tracing::span!(
        target: TRACE_TARGET,
        ::tracing::Level::INFO,
        "gen_ai.client.operation",
        "gen_ai.system" = call.system.as_deref(),
        "gen_ai.operation.name" = call.operation.as_deref(),
        "gen_ai.request.model" = call.request_model.as_deref(),
        "gen_ai.request.temperature" = call.temperature,
        "gen_ai.response.id" = call.response_id.as_deref(),
        "gen_ai.response.model" = call.response_model.as_deref(),
        "gen_ai.response.finish_reasons" = (!call.finish_reasons.is_empty())
            .then(|| call.finish_reasons.join(",")),
        "gen_ai.usage.input_tokens" = call.input_tokens,
        "gen_ai.usage.output_tokens" = call.output_tokens,
        "gen_ai.usage.input_cached_tokens" = call.input_cached_tokens,
        "gen_ai.input.messages" = Empty,
        "gen_ai.output.messages" = Empty,
        "session.id" = Empty,
        "user.id" = Empty,
        tags = Empty,
        "deployment.environment.name" = Empty,
        "service.version" = Empty,
    )
}

/// Vendor-neutral grouping of spans: which session, which end user, which
/// tags, which environment, which release.
///
/// LLM observability backends filter at the level of the individual span, not
/// only at the trace root, so the grouping has to reach every span rather than
/// sit on the first one. Two ways to make that happen:
///
/// * Without the `otel` feature, [`TraceGrouping::stamp`] fills the grouping
///   fields — which every span this crate opens declares empty — on whichever
///   span you hand it, and [`TraceGrouping::scope_span`] opens a parent span
///   that carries them for a subscriber that flattens ancestors.
/// * With the `otel` feature, [`crate::otel::attach_grouping`] puts the same
///   values in OpenTelemetry baggage on the current context, where a
///   baggage-copying span processor in the application's SDK setup stamps them
///   onto every span that starts underneath. The processor itself lives in the
///   adopter's code because it needs `opentelemetry_sdk`, which this crate does
///   not depend on; [`crate::otel::grouping_from_baggage`] gives it the
///   key/values to copy.
///
/// The end-user reference is always a digest — build it with
/// [`TraceGrouping::with_account`], which hashes the account id, or supply your
/// own digest with [`TraceGrouping::with_end_user_hash`]. There is no
/// constructor that takes a raw account or user identifier.
///
/// A backend that insists on its own attribute names is a renaming function
/// over [`TraceGrouping::fields`] in the adopter's code, not something this
/// crate hardcodes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct TraceGrouping {
    /// The session, recorded as `session.id`. Turnframe uses the conversation.
    pub session_id: Option<String>,
    /// Digest of the end user, recorded as `user.id`. Never a raw identifier.
    pub end_user_hash: Option<String>,
    /// Free-form grouping labels the application chose.
    pub tags: Vec<String>,
    /// Deployment environment, e.g. `production`.
    pub environment: Option<String>,
    /// Release or build of the running service.
    pub release: Option<String>,
}

impl TraceGrouping {
    /// Groups spans by conversation, which is the session a backend shows.
    #[must_use]
    pub fn for_conversation(conversation_id: ConversationId) -> Self {
        Self {
            session_id: Some(conversation_id.to_string()),
            ..Self::default()
        }
    }

    /// Adds the end user as the digest of an account id. The raw id is hashed
    /// by [`account_hash`] and never stored on the grouping.
    #[must_use]
    pub fn with_account(mut self, account_id: &AccountId) -> Self {
        self.end_user_hash = Some(account_hash(account_id));
        self
    }

    /// Adds an end-user reference the caller has already hashed.
    #[must_use]
    pub fn with_end_user_hash(mut self, hash: impl Into<String>) -> Self {
        self.end_user_hash = Some(hash.into());
        self
    }

    /// Adds a grouping label.
    #[must_use]
    pub fn with_tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }

    /// Sets the deployment environment.
    #[must_use]
    pub fn with_environment(mut self, environment: impl Into<String>) -> Self {
        self.environment = Some(environment.into());
        self
    }

    /// Sets the release or build.
    #[must_use]
    pub fn with_release(mut self, release: impl Into<String>) -> Self {
        self.release = Some(release.into());
        self
    }

    /// The grouping as `(attribute key, value)` pairs in a stable order,
    /// omitting what is unset. Testable without a subscriber, and the set an
    /// adopter renames for a backend with its own spelling.
    #[must_use]
    pub fn fields(&self) -> Vec<(&'static str, String)> {
        let mut out: Vec<(&'static str, String)> = Vec::new();
        let mut push = |key: &'static str, value: Option<String>| {
            if let Some(value) = value {
                out.push((key, value));
            }
        };
        push(attrs::SESSION_ID, self.session_id.clone());
        push(attrs::USER_ID, self.end_user_hash.clone());
        push(
            attrs::TAGS,
            if self.tags.is_empty() {
                None
            } else {
                Some(self.tags.join(","))
            },
        );
        push(attrs::DEPLOYMENT_ENVIRONMENT, self.environment.clone());
        push(attrs::SERVICE_VERSION, self.release.clone());
        out
    }

    /// Records the grouping on `span`.
    ///
    /// Every span this crate opens declares the grouping fields empty, so this
    /// fills them in. Recording on a span that did not declare them is a no-op
    /// rather than an error.
    pub fn stamp(&self, span: &::tracing::Span) {
        for (key, value) in self.fields() {
            span.record(key, value.as_str());
        }
    }

    /// Records the grouping on the span that is currently entered.
    pub fn stamp_current(&self) {
        self.stamp(&::tracing::Span::current());
    }

    /// Opens a span that carries the grouping and becomes the parent of every
    /// span opened while it is entered.
    ///
    /// ```rust
    /// use turnframe_core::ids::{AccountId, ConversationId};
    /// use turnframe_telemetry::tracing::TraceGrouping;
    ///
    /// let grouping = TraceGrouping::for_conversation(ConversationId::nil())
    ///     .with_account(&AccountId::from("acct-1"))
    ///     .with_environment("production")
    ///     .with_release("v0.1.0")
    ///     .with_tag("trip");
    ///
    /// let span = grouping.scope_span();
    /// let _entered = span.enter();
    /// ```
    #[must_use]
    pub fn scope_span(&self) -> ::tracing::Span {
        let span = ::tracing::span!(
            target: TRACE_TARGET,
            ::tracing::Level::INFO,
            "turnframe.grouping",
            "session.id" = Empty,
            "user.id" = Empty,
            tags = Empty,
            "deployment.environment.name" = Empty,
            "service.version" = Empty,
        );
        self.stamp(&span);
        span
    }
}

/// Which half of a conversation a piece of content is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
#[non_exhaustive]
pub enum ContentRole {
    /// The prompt sent to the model.
    Input,
    /// The completion the model returned.
    Output,
}

impl ContentRole {
    /// The attribute key this role is recorded under.
    #[must_use]
    pub const fn attribute(self) -> &'static str {
        match self {
            Self::Input => attrs::GEN_AI_INPUT_MESSAGES,
            Self::Output => attrs::GEN_AI_OUTPUT_MESSAGES,
        }
    }
}

/// Decides what, if anything, of a prompt or completion may be recorded.
///
/// The hook is mandatory: content recording cannot be switched on without one,
/// so there is no configuration in which raw user text reaches the backend
/// unexamined. Returning `None` drops the content entirely.
pub trait ContentRedactor: Send + Sync + fmt::Debug {
    /// Returns the text that may be recorded, or `None` to record nothing.
    fn redact(&self, role: ContentRole, text: &str) -> Option<String>;
}

/// A redactor that records nothing. Useful as an explicit "not yet" while the
/// real one is being written.
#[derive(Debug, Clone, Copy, Default)]
pub struct DropAllContent;

impl ContentRedactor for DropAllContent {
    fn redact(&self, _role: ContentRole, _text: &str) -> Option<String> {
        None
    }
}

/// Records prompt and completion text on a provider-call span.
///
/// **Disabled by default, and for a reason.** Prompts and completions are user
/// data: enabling this sends what the user typed, and what the model said back,
/// to whatever tracing backend is configured. That is a decision about data
/// residency, retention and consent, not a debugging convenience, so it takes
/// an explicit [`ContentRecorder::enabled`] call and a
/// [`ContentRedactor`] to switch on (spec §25.5).
///
/// ```rust
/// use std::sync::Arc;
///
/// use turnframe_telemetry::tracing::{ContentRecorder, ContentRole, DropAllContent};
///
/// // The default records nothing at all.
/// let off = ContentRecorder::disabled();
/// assert!(!off.is_enabled());
/// assert_eq!(off.rendered(ContentRole::Input, "withdraw trip 17"), None);
///
/// // Even switched on, everything goes through the redaction hook.
/// let on = ContentRecorder::enabled(Arc::new(DropAllContent));
/// assert!(on.is_enabled());
/// assert_eq!(on.rendered(ContentRole::Input, "withdraw trip 17"), None);
/// ```
#[derive(Clone)]
pub struct ContentRecorder {
    enabled: bool,
    redactor: Arc<dyn ContentRedactor>,
}

impl ContentRecorder {
    /// Records nothing. The default, and what an application gets unless it
    /// deliberately asks for something else.
    #[must_use]
    pub fn disabled() -> Self {
        Self {
            enabled: false,
            redactor: Arc::new(DropAllContent),
        }
    }

    /// Records content, every piece of it through `redactor` first.
    #[must_use]
    pub fn enabled(redactor: Arc<dyn ContentRedactor>) -> Self {
        Self {
            enabled: true,
            redactor,
        }
    }

    /// Whether content recording is switched on.
    #[must_use]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// What would be recorded for this text: `None` when recording is off or
    /// the redactor dropped it. Exposed so an application can test its own
    /// redaction hook without a subscriber.
    #[must_use]
    pub fn rendered(&self, role: ContentRole, text: &str) -> Option<String> {
        if !self.enabled {
            return None;
        }
        self.redactor.redact(role, text)
    }

    /// Records content on `span`, if recording is on and the redactor allows
    /// it. The span must be one this crate opened, which declares the two
    /// content fields empty.
    pub fn record(&self, span: &::tracing::Span, role: ContentRole, text: &str) {
        if let Some(rendered) = self.rendered(role, text) {
            span.record(role.attribute(), rendered.as_str());
        }
    }
}

impl Default for ContentRecorder {
    fn default() -> Self {
        Self::disabled()
    }
}

impl fmt::Debug for ContentRecorder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ContentRecorder")
            .field("enabled", &self.enabled)
            .field("redactor", &self.redactor)
            .finish()
    }
}

/// Emits an event carrying every known identifier of a turn (spec §26.1).
///
/// Use it at the end of a turn, or when a replay record is written, so one log
/// line ties the turn to its plan, its commands, its events and its blocks.
pub fn record_turn(ids: &TurnIdentifiers) {
    ::tracing::event!(
        target: TRACE_TARGET,
        ::tracing::Level::INFO,
        turn_id = ids.turn_id.map(|id| id.to_string()),
        conversation_id = ids.conversation_id.map(|id| id.to_string()),
        account_hash = ids.account_hash.as_deref(),
        workflow = opt_str(&ids.workflow),
        workflow_version = opt_str(&ids.workflow_version),
        case_id = opt_str(&ids.case_id),
        case_revision = ids.case_revision.map(|revision| revision.value()),
        interaction_id = ids.interaction_id.map(|id| id.to_string()),
        provider = opt_str(&ids.provider),
        model = opt_str(&ids.model),
        attempt = opt_str(&ids.attempt),
        plan_hash = ids.plan_hash.as_ref().map(Digest::as_str),
        command_id = ids.command_id.map(|id| id.to_string()),
        event_ids = join_ids(&ids.event_ids),
        block_ids = join_ids(&ids.block_ids),
        outbox_id = ids.outbox_id.map(|id| id.to_string()),
        "turn recorded",
    );
}

/// The label fields of a signal, as `(field name, value)` pairs.
///
/// Only the typed members of [`SignalLabels`] appear; there is no free-text
/// member to leak. Exposed as a plain function so the extraction can be tested
/// without a subscriber.
#[must_use]
pub fn signal_fields(labels: &SignalLabels) -> Vec<(&'static str, String)> {
    let mut fields: Vec<(&'static str, String)> = Vec::new();
    let mut push = |name: &'static str, value: Option<String>| {
        if let Some(value) = value {
            fields.push((name, value));
        }
    };
    push(
        field::WORKFLOW,
        opt_str(&labels.workflow).map(str::to_owned),
    );
    push(
        field::PROVIDER,
        opt_str(&labels.provider).map(str::to_owned),
    );
    push(field::MODEL, opt_str(&labels.model).map(str::to_owned));
    push(field::PURPOSE, opt_str(&labels.purpose).map(str::to_owned));
    push(
        field::RISK,
        labels.risk.as_ref().and_then(crate::enum_label),
    );
    push(
        field::INTERACTION,
        labels.interaction.as_ref().and_then(crate::enum_label),
    );
    push(
        field::ERROR_CODE,
        opt_str(&labels.error_code).map(str::to_owned),
    );
    push(
        field::EFFORT,
        labels.effort.map(|effort| effort.as_str().to_owned()),
    );
    fields
}

/// An [`Observer`] that emits one structured `tracing` event per signal.
///
/// Signals that report a safety-integrity problem
/// ([`Signal::is_safety_signal`]) are emitted at `WARN`, everything else at
/// `DEBUG`: a claim violation or a stale interaction should surface without a
/// filter change, while the ordinary flow of a busy system should not.
///
/// The event carries the signal name, the typed labels and, for a latency
/// signal, the measured duration in milliseconds. It never carries text.
///
/// ```rust
/// use turnframe_core::ids::WorkflowKey;
/// use turnframe_core::observe::{Observer, Signal, SignalLabels};
/// use turnframe_telemetry::TracingObserver;
///
/// let observer = TracingObserver::new();
/// observer.observe_labeled(
///     &Signal::WorkflowInvariantViolation,
///     &SignalLabels::workflow(WorkflowKey::from("trip")).with_error_code("two_open_drafts"),
/// );
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct TracingObserver;

impl TracingObserver {
    /// Builds the observer. Events go to whichever subscriber the application
    /// installed, so there is nothing to configure here.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    fn emit(signal: Signal, labels: &SignalLabels, duration: Option<Duration>) {
        let duration_ms = duration.map(|value| value.as_secs_f64() * 1_000.0);
        if signal.is_safety_signal() {
            ::tracing::event!(
                target: TRACE_TARGET,
                ::tracing::Level::WARN,
                signal = signal.name(),
                workflow = opt_str(&labels.workflow),
                provider = opt_str(&labels.provider),
                model = opt_str(&labels.model),
                purpose = opt_str(&labels.purpose),
                risk = labels.risk.as_ref().and_then(crate::enum_label),
                interaction = labels.interaction.as_ref().and_then(crate::enum_label),
                error_code = opt_str(&labels.error_code),
                effort = labels.effort.map(|effort| effort.as_str()),
                duration_ms = duration_ms,
                "turnframe safety signal",
            );
        } else {
            ::tracing::event!(
                target: TRACE_TARGET,
                ::tracing::Level::DEBUG,
                signal = signal.name(),
                workflow = opt_str(&labels.workflow),
                provider = opt_str(&labels.provider),
                model = opt_str(&labels.model),
                purpose = opt_str(&labels.purpose),
                risk = labels.risk.as_ref().and_then(crate::enum_label),
                interaction = labels.interaction.as_ref().and_then(crate::enum_label),
                error_code = opt_str(&labels.error_code),
                effort = labels.effort.map(|effort| effort.as_str()),
                duration_ms = duration_ms,
                "turnframe signal",
            );
        }
    }
}

impl Observer for TracingObserver {
    fn observe(&self, signal: &Signal) {
        Self::emit(*signal, &SignalLabels::none(), None);
    }

    fn observe_labeled(&self, signal: &Signal, labels: &SignalLabels) {
        Self::emit(*signal, labels, None);
    }

    fn observe_duration(&self, signal: &Signal, duration: Duration, labels: &SignalLabels) {
        Self::emit(*signal, labels, Some(duration));
    }
}

#[cfg(test)]
mod tests {
    use chrono::DateTime;
    use turnframe_core::case::CaseRef;
    use turnframe_core::command::RiskClass;
    use turnframe_core::ids::{ModelKey, ProviderKey};
    use turnframe_core::interaction::InteractionKind;
    use turnframe_core::replay::{ProviderAttemptOutcome, WorkflowVersionRecord};

    use super::*;

    fn replay() -> ReplayRecord {
        let now = DateTime::from_timestamp(1_700_000_000, 0).expect("valid timestamp");
        let mut record = ReplayRecord::received(
            TurnId::nil(),
            ConversationId::nil(),
            AccountId::from("acct-1"),
            now,
        );
        record.workflow_versions.push(WorkflowVersionRecord {
            key: WorkflowKey::from("trip"),
            version: WorkflowVersion::from("3"),
        });
        record.loaded_cases.push(CaseRef::new(
            WorkflowKey::from("trip"),
            CaseId::from("trip-7"),
            CaseRevision(4),
        ));
        record.interactions_created.push(InteractionId::nil());
        record.plan_hash = Some(Digest(String::from("abc123")));
        record.event_ids.push(EventId::nil());
        record.response_block_ids.push(BlockId::from("b1"));
        record.response_block_ids.push(BlockId::from("b2"));
        record
    }

    #[test]
    fn account_hash_is_stable_short_and_hides_the_raw_id() {
        let account = AccountId::from("acct-1");
        let hash = account_hash(&account);
        assert_eq!(hash.len(), ACCOUNT_HASH_LEN);
        assert!(hash.chars().all(|c| c.is_ascii_hexdigit()));
        assert_eq!(hash, account_hash(&account));
        assert!(!hash.contains("acct"));
        assert_ne!(hash, account_hash(&AccountId::from("acct-2")));
    }

    #[test]
    fn account_hash_is_domain_separated() {
        // The digest is not the plain digest of the identifier.
        let plain = digest_hex(b"acct-1")[..ACCOUNT_HASH_LEN].to_owned();
        assert_ne!(account_hash(&AccountId::from("acct-1")), plain);
    }

    #[test]
    fn identifiers_from_replay_carry_the_stable_ids_of_26_1() {
        let ids = TurnIdentifiers::from_replay(&replay());
        assert_eq!(ids.turn_id, Some(TurnId::nil()));
        assert_eq!(ids.conversation_id, Some(ConversationId::nil()));
        assert_eq!(ids.workflow.as_ref().map(WorkflowKey::as_str), Some("trip"));
        assert_eq!(
            ids.workflow_version.as_ref().map(WorkflowVersion::as_str),
            Some("3")
        );
        assert_eq!(ids.case_id.as_ref().map(CaseId::as_str), Some("trip-7"));
        assert_eq!(ids.case_revision, Some(CaseRevision(4)));
        assert_eq!(ids.interaction_id, Some(InteractionId::nil()));
        assert_eq!(ids.plan_hash.as_ref().map(Digest::as_str), Some("abc123"));
        assert_eq!(ids.event_ids.len(), 1);
        assert_eq!(ids.block_ids.len(), 2);
        assert_eq!(
            ids.account_hash,
            Some(account_hash(&AccountId::from("acct-1")))
        );
    }

    #[test]
    fn fields_are_ordered_and_omit_what_is_unknown() {
        let ids = TurnIdentifiers::from_replay(&replay());
        let fields = ids.fields();
        let names: Vec<&str> = fields.iter().map(|(name, _)| *name).collect();
        assert_eq!(
            names,
            vec![
                field::TURN_ID,
                field::CONVERSATION_ID,
                field::ACCOUNT_HASH,
                field::WORKFLOW,
                field::WORKFLOW_VERSION,
                field::CASE_ID,
                field::CASE_REVISION,
                field::INTERACTION_ID,
                field::PLAN_HASH,
                field::EVENT_IDS,
                field::BLOCK_IDS,
            ]
        );
        let by_name = |name: &str| {
            fields
                .iter()
                .find(|(field, _)| *field == name)
                .map(|(_, value)| value.clone())
        };
        assert_eq!(by_name(field::CASE_ID).as_deref(), Some("trip-7"));
        assert_eq!(by_name(field::CASE_REVISION).as_deref(), Some("4"));
        assert_eq!(by_name(field::BLOCK_IDS).as_deref(), Some("b1,b2"));
    }

    #[test]
    fn fields_of_an_empty_identifier_set_are_empty() {
        assert!(TurnIdentifiers::default().fields().is_empty());
    }

    #[test]
    fn of_turn_hashes_the_account() {
        let ids = TurnIdentifiers::of_turn(
            TurnId::nil(),
            ConversationId::nil(),
            &AccountId::from("acct-9"),
        );
        let account = ids.account_hash.clone().expect("hashed");
        assert_eq!(account, account_hash(&AccountId::from("acct-9")));
        let rendered = ids.fields();
        assert!(rendered.iter().all(|(_, value)| value != "acct-9"));
    }

    #[test]
    fn signal_fields_render_only_typed_labels() {
        let labels = SignalLabels::workflow(WorkflowKey::from("trip"))
            .with_provider("openai")
            .with_model("gpt-x")
            .with_purpose("extract")
            .with_risk(RiskClass::Destructive)
            .with_interaction(InteractionKind::ConfirmCommand)
            .with_error_code("rate_limited");
        assert_eq!(
            signal_fields(&labels),
            vec![
                (field::WORKFLOW, String::from("trip")),
                (field::PROVIDER, String::from("openai")),
                (field::MODEL, String::from("gpt-x")),
                (field::PURPOSE, String::from("extract")),
                (field::RISK, String::from("destructive")),
                (field::INTERACTION, String::from("confirm_command")),
                (field::ERROR_CODE, String::from("rate_limited")),
            ]
        );
        assert!(signal_fields(&SignalLabels::none()).is_empty());
    }

    #[test]
    fn signal_fields_carry_the_turns_effort() {
        let labels = SignalLabels::none().with_effort(turnframe_core::effort::Effort::Low);
        assert_eq!(
            signal_fields(&labels),
            vec![(field::EFFORT, String::from("low"))]
        );
    }

    #[test]
    fn join_ids_is_empty_for_no_ids() {
        assert_eq!(join_ids::<BlockId>(&[]), None);
        assert_eq!(
            join_ids(&[BlockId::from("a"), BlockId::from("b")]).as_deref(),
            Some("a,b")
        );
    }

    #[test]
    fn stage_names_are_distinct() {
        let mut names: Vec<&str> = PipelineStage::ALL.iter().map(|s| s.as_str()).collect();
        names.sort_unstable();
        let total = names.len();
        names.dedup();
        assert_eq!(names.len(), total);
        assert_eq!(PipelineStage::Understanding.to_string(), "understanding");
    }

    #[test]
    fn provider_call_attributes_use_the_genai_keys_in_order() {
        let call = ProviderCall::new("openai", "extract", "gpt-x")
            .with_temperature(0.2)
            .with_response("resp-1", "gpt-x-2026-05")
            .with_finish_reason("stop")
            .with_finish_reason("length")
            .with_cached_usage(1_000, 800, 120);

        assert_eq!(
            call.attributes(),
            vec![
                (attrs::GEN_AI_SYSTEM, String::from("openai")),
                (attrs::GEN_AI_OPERATION_NAME, String::from("extract")),
                (attrs::GEN_AI_REQUEST_MODEL, String::from("gpt-x")),
                (attrs::GEN_AI_REQUEST_TEMPERATURE, String::from("0.2")),
                (attrs::GEN_AI_RESPONSE_ID, String::from("resp-1")),
                (attrs::GEN_AI_RESPONSE_MODEL, String::from("gpt-x-2026-05")),
                (
                    attrs::GEN_AI_RESPONSE_FINISH_REASONS,
                    String::from("stop,length")
                ),
                (attrs::GEN_AI_USAGE_INPUT_TOKENS, String::from("200")),
                (attrs::GEN_AI_USAGE_OUTPUT_TOKENS, String::from("120")),
                (attrs::GEN_AI_USAGE_INPUT_CACHED_TOKENS, String::from("800")),
            ]
        );
    }

    #[test]
    fn input_tokens_are_net_of_cached_tokens() {
        let call = ProviderCall::default().with_cached_usage(1_000, 800, 10);
        assert_eq!(call.input_tokens, Some(200));
        assert_eq!(call.input_cached_tokens, Some(800));
        // Net plus cached is the total the provider reported: no double count.
        assert_eq!(call.total_input_tokens(), Some(1_000));

        // A cache figure larger than the total cannot underflow.
        let odd = ProviderCall::default().with_cached_usage(10, 40, 1);
        assert_eq!(odd.input_tokens, Some(0));

        // Usage reported already net leaves the cache unset.
        let plain = ProviderCall::default().with_usage(300, 20);
        assert_eq!(plain.input_tokens, Some(300));
        assert_eq!(plain.input_cached_tokens, None);
        assert_eq!(plain.total_input_tokens(), Some(300));
        assert_eq!(ProviderCall::default().total_input_tokens(), None);
    }

    #[test]
    fn a_provider_call_carries_no_content() {
        let call = ProviderCall::new("openai", "extract", "gpt-x");
        let keys: Vec<&str> = call.attributes().into_iter().map(|(key, _)| key).collect();
        assert!(!keys.contains(&attrs::GEN_AI_INPUT_MESSAGES));
        assert!(!keys.contains(&attrs::GEN_AI_OUTPUT_MESSAGES));
    }

    #[test]
    fn a_provider_call_can_be_built_from_a_persisted_attempt() {
        let mut record = replay();
        record.provider_attempts.push(ProviderAttemptRecord {
            attempt: 2,
            purpose: String::from("extract"),
            provider_key: ProviderKey::from("openai"),
            model_key: ModelKey::from("gpt-x"),
            request_id: String::from("req-1"),
            prompt_version: None,
            prompt_ref: None,
            outcome: ProviderAttemptOutcome::Succeeded,
            latency_ms: Some(120),
            input_tokens: Some(300),
            output_tokens: Some(40),
            temperature: Some(0.2),
            finish_reasons: vec![String::from("stop")],
        });
        let attempt = record.provider_attempts.last().expect("attempt");
        let call = ProviderCall::from_attempt(attempt);
        assert_eq!(call.system.as_deref(), Some("openai"));
        assert_eq!(call.operation.as_deref(), Some("extract"));
        assert_eq!(call.request_model.as_deref(), Some("gpt-x"));
        assert_eq!(call.response_id.as_deref(), Some("req-1"));
        assert_eq!(call.input_tokens, Some(300));
        assert_eq!(call.output_tokens, Some(40));
        // The persisted attempt now carries what the request asked for and what
        // the provider answered, so the span needs no second source.
        assert!(call.temperature.is_some_and(|t| (t - 0.2).abs() < 1e-6));
        assert_eq!(call.finish_reasons, vec![String::from("stop")]);
        let attributes = call.attributes();
        assert!(
            attributes
                .iter()
                .any(|(key, _)| *key == crate::attrs::GEN_AI_REQUEST_TEMPERATURE)
        );
        assert!(
            attributes
                .iter()
                .any(|(key, _)| *key == crate::attrs::GEN_AI_RESPONSE_FINISH_REASONS)
        );

        let ids = TurnIdentifiers::from_replay(&record);
        assert_eq!(ids.attempt.as_deref(), Some("2"));
        assert_eq!(ids.provider.as_deref(), Some("openai"));
    }

    #[test]
    fn the_outbox_identifier_comes_from_the_replay_record() {
        let mut record = replay();
        assert_eq!(
            TurnIdentifiers::from_replay(&record).outbox_id,
            None,
            "a turn with no external effect names no outbox row"
        );

        let enqueued = OutboxId::new();
        record.outbox_ids.push(enqueued);
        let ids = TurnIdentifiers::from_replay(&record);
        assert_eq!(ids.outbox_id, Some(enqueued));
        assert!(
            ids.fields()
                .iter()
                .any(|(key, value)| *key == field::OUTBOX_ID && value == &enqueued.to_string())
        );
    }

    #[test]
    fn trace_grouping_fields_are_vendor_neutral_and_hashed() {
        let grouping = TraceGrouping::for_conversation(ConversationId::nil())
            .with_account(&AccountId::from("acct-1"))
            .with_environment("production")
            .with_release("v0.1.0")
            .with_tag("trip")
            .with_tag("beta");

        assert_eq!(
            grouping.fields(),
            vec![
                (attrs::SESSION_ID, ConversationId::nil().to_string()),
                (attrs::USER_ID, account_hash(&AccountId::from("acct-1"))),
                (attrs::TAGS, String::from("trip,beta")),
                (attrs::DEPLOYMENT_ENVIRONMENT, String::from("production")),
                (attrs::SERVICE_VERSION, String::from("v0.1.0")),
            ]
        );
        // The raw tenant identifier is nowhere in the grouping.
        assert!(grouping.fields().iter().all(|(_, value)| value != "acct-1"));
        assert_eq!(
            grouping.end_user_hash.as_deref(),
            Some(account_hash(&AccountId::from("acct-1")).as_str())
        );
    }

    #[test]
    fn an_empty_grouping_stamps_nothing() {
        assert!(TraceGrouping::default().fields().is_empty());
    }

    #[test]
    fn a_grouping_can_take_an_already_hashed_end_user() {
        let grouping = TraceGrouping::default().with_end_user_hash("deadbeefdeadbeef");
        assert_eq!(
            grouping.fields(),
            vec![(attrs::USER_ID, String::from("deadbeefdeadbeef"))]
        );
    }

    #[test]
    fn stamping_a_grouping_is_harmless_without_a_subscriber() {
        let grouping = TraceGrouping::for_conversation(ConversationId::nil()).with_tag("trip");
        let span = grouping.scope_span();
        let _entered = span.enter();
        grouping.stamp_current();
        grouping.stamp(&turn_span(
            TurnId::nil(),
            ConversationId::nil(),
            &AccountId::from("acct-1"),
        ));
        grouping.stamp(&stage_span(PipelineStage::Understanding, TurnId::nil()));
        grouping.stamp(&provider_call_span(&ProviderCall::new(
            "openai", "extract", "gpt-x",
        )));
    }

    #[test]
    fn content_recording_is_off_by_default() {
        let recorder = ContentRecorder::default();
        assert!(!recorder.is_enabled());
        assert_eq!(
            recorder.rendered(ContentRole::Input, "withdraw trip 17"),
            None
        );
        assert_eq!(recorder.rendered(ContentRole::Output, "done"), None);
        assert!(!ContentRecorder::disabled().is_enabled());
        assert!(format!("{recorder:?}").contains("enabled: false"));
    }

    /// A redactor that keeps only the length, to prove the hook is consulted.
    #[derive(Debug)]
    struct LengthOnly;

    impl ContentRedactor for LengthOnly {
        fn redact(&self, role: ContentRole, text: &str) -> Option<String> {
            match role {
                ContentRole::Input => Some(format!("{} chars", text.chars().count())),
                _ => None,
            }
        }
    }

    #[test]
    fn enabled_content_still_goes_through_the_redactor() {
        let recorder = ContentRecorder::enabled(Arc::new(LengthOnly));
        assert!(recorder.is_enabled());
        assert_eq!(
            recorder
                .rendered(ContentRole::Input, "withdraw trip 17")
                .as_deref(),
            Some("16 chars")
        );
        // The redactor dropped the completion entirely.
        assert_eq!(recorder.rendered(ContentRole::Output, "done"), None);

        // And a redactor that drops everything records nothing even when on.
        let strict = ContentRecorder::enabled(Arc::new(DropAllContent));
        assert!(strict.is_enabled());
        assert_eq!(
            strict.rendered(ContentRole::Input, "withdraw trip 17"),
            None
        );

        let span = provider_call_span(&ProviderCall::new("openai", "extract", "gpt-x"));
        recorder.record(&span, ContentRole::Input, "withdraw trip 17");
        recorder.record(&span, ContentRole::Output, "done");
    }

    #[test]
    fn content_roles_map_to_the_genai_keys() {
        assert_eq!(ContentRole::Input.attribute(), attrs::GEN_AI_INPUT_MESSAGES);
        assert_eq!(
            ContentRole::Output.attribute(),
            attrs::GEN_AI_OUTPUT_MESSAGES
        );
    }

    #[test]
    fn observers_accept_every_signal_without_a_subscriber() {
        let observer = TracingObserver::new();
        for signal in Signal::ALL {
            observer.observe(&signal);
            observer.observe_labeled(&signal, &SignalLabels::none());
            observer.observe_duration(&signal, Duration::from_millis(1), &SignalLabels::none());
        }
        record_turn(&TurnIdentifiers::from_replay(&replay()));
    }
}

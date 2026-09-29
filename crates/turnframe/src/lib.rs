//! `turnframe`: deterministic conversational workflows for Rust, built around the Flow Map
//! architecture. It is the crate an application installs, with the `turnframe-*` family
//! behind feature flags. The model proposes meaning, deterministic code decides effects, and
//! committed events decide claims; [`Orchestrator`] runs one [`TurnInput`] through all of it,
//! and the [architecture guide](https://github.com/turnframe-rs/turnframe/blob/main/docs/architecture.md)
//! explains what an adopter writes.
//!
//! ```toml
//! [dependencies]
//! turnframe = { version = "0.1", features = ["openai", "postgres", "telemetry"] }
//! ```
//!
//! | Module | What belongs there |
//! |---|---|
//! | [`flow`] | workflow definitions, the pure projector, the view, the registry, case references |
//! | [`turn`] | one user turn: the input, target resolution, reduction, the replay record |
//! | [`understanding`] | what a turn was understood to say: units, acts, questions, constraints |
//! | [`understand`] | the understanding pipeline: its input, tasks, settings and streamed steps |
//! | [`tasks`] | the engine that runs small verified model tasks under budgets and profiles |
//! | [`interaction`] | durable cards: payloads, options, status, responses and their validation |
//! | [`command`] | typed commands, envelopes, origins, risk and confirmation policy |
//! | [`event`] | the claim ledger: committed events, external statuses, operational receipts |
//! | [`response`] | the ordered blocks a turn returns, and the claim guard over them |
//! | [`provider`] | the provider-neutral model layer, capability routing, fallback, and each enabled adapter |
//! | [`store`] | the persistence traits, the in-memory implementation, the store conformance suite |
//! | [`runtime`] | the turn pipeline itself: orchestration, configuration, and every stage of it |
//! | [`prompt`] | where prompt text comes from, and which prompt text produced a turn |
//! | [`error`] | the typed failure family every layer speaks |
//! | [`ids`] | newtype identifiers and versions |
//! | [`observe`] | the metric and tracing signals a runtime emits |
//! | [`locale`] | locales and server-authored localized copy |
//! | [`schema`] | canonical hashing and JSON Schema fingerprints |
//! | [`telemetry`], [`testing`], [`evaluation`] | with their feature, below; none is on by default, and none changes the runtime's safety semantics |
//!
//! | Feature | What it turns on |
//! |---|---|
//! | `openai` | `provider::openai`: OpenAI, Azure OpenAI and OpenAI-compatible endpoints |
//! | `anthropic` | `provider::anthropic`: the Anthropic Messages API |
//! | `gemini` | `provider::gemini`: Google Gemini and Vertex AI |
//! | `bedrock` | `provider::bedrock`: AWS Bedrock Converse |
//! | `ollama` | `provider::ollama`: a local Ollama daemon |
//! | `all-providers` | every adapter above |
//! | `postgres` | `store::postgres`: the PostgreSQL reference store, its migrations and its expected-revision transactions |
//! | `prompts` | the prompt sources in [`prompt`]: prompts compiled in from your own repository, and a bounded cache. No network |
//! | `langfuse` | `prompts`, plus `prompt::langfuse`: prompts fetched from a Langfuse project over the Langfuse v4 API. A runtime dependency on a remote service |
//! | `telemetry` | [`telemetry`]: the `turnframe.*` metrics observer, tracing spans and the dashboard description |
//! | `otel` | `telemetry`, plus the OpenTelemetry bridge and the baggage-copying span processor |
//! | `test-kit` | [`testing`]: scripted providers and tasks, fake stores, workflow exploration and three sample domains |
//! | `eval` | [`evaluation`]: the model evaluation harness |
//! | `full` | `all-providers`, `postgres`, `prompts`, `telemetry`, `test-kit` and `eval` |
//!
#![cfg_attr(feature = "test-kit", doc = include_str!("quickstart.md"))]
#![cfg_attr(
    not(feature = "test-kit"),
    doc = "\n# A minimal turn, end to end\n\nThe runnable end-to-end example needs the `test-kit` feature, which supplies \
           the in-memory stores, the scripted provider and the sample domains it \
           uses. It is on <https://docs.rs/turnframe>, and the programs under \
           [`examples/`](https://github.com/turnframe-rs/turnframe/tree/main/examples) \
           are longer versions of it."
)]
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

/// The crate README, compiled as a doc-test so its example cannot rot.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
mod readme {}

pub mod flow {
    //! Workflow definitions, the pure projector, the view it returns, the
    //! registry that hosts several workflows at once, and the case references
    //! everything is addressed by.

    pub use turnframe_core::case::{CaseKey, CaseRef, Versioned};
    pub use turnframe_core::flow::*;
}

pub mod turn {
    //! One user turn: what arrives, how targets resolve, how the whole turn
    //! reduces to commands, and the record that can explain it afterwards.

    pub use turnframe_core::turn::*;
    pub use turnframe_core::{knowledge, plan, read, reduce, replay, target};
}

pub mod interaction {
    //! Durable cards: immutable payloads, server-owned option semantics,
    //! compare-and-set resolution, staleness and expiry.

    pub use turnframe_core::interaction::*;
}

pub mod command {
    //! Typed commands and the envelope that carries one: who asked, which case,
    //! which expected revision, which idempotency key, and which origin — plus
    //! the risk and confirmation policy that decides whether it may run.

    pub use turnframe_core::command::*;
    pub use turnframe_core::policy;
}

pub mod event {
    //! The claim ledger: committed events, the regulated external statuses that
    //! must never collapse into "done", and the receipts rendered from them.

    pub use turnframe_core::event::*;
}

pub mod response {
    //! The ordered, typed blocks a turn returns, and the claim guard that
    //! refuses an operational claim no committed event backs.

    pub use turnframe_core::response::*;
}

pub mod error {
    //! The typed failure family: what went wrong, whether it may be retried, and
    //! whether an effect may already exist.

    pub use turnframe_core::error::*;
}

pub mod ids {
    //! Newtype identifiers, revisions and versions. Nothing here is a bare
    //! `String` or `Uuid` at a call site.

    pub use turnframe_core::ids::*;
}

pub mod observe {
    //! The signals a runtime emits, so metrics and traces are a contract rather
    //! than a grep over log lines.

    pub use turnframe_core::observe::*;
}

pub mod locale {
    //! Locales and server-authored localized copy, which is what receipts,
    //! notices and card labels are made of.

    pub use turnframe_core::locale::*;
}

pub mod schema {
    //! Canonical hashing and JSON Schema fingerprints: how a payload, a plan or
    //! a configuration is identified by content.

    pub use turnframe_core::hash::*;
    pub use turnframe_core::schema::*;
}

pub mod case {
    //! Case references and versioned values: how a record is named, and how a
    //! value is carried with the revision it was read at.

    pub use turnframe_core::case::*;
}

pub mod plan {
    //! Which records an operation may aim at, whether it changes anything, and the
    //! limits a turn's understanding is held to.

    pub use turnframe_core::plan::*;
}

pub mod understanding {
    //! What a turn was understood to say: its units, the acts they ask for with
    //! their arguments and the words each came from, questions, constraints, a
    //! typed card answer, disputes, and what could not be understood.
    //!
    //! Everything here is a *reading*. It is target-resolved, checked and
    //! reduced before any of it can become an effect.

    pub use turnframe_core::understanding::*;
}

pub mod understand {
    //! The understanding pipeline: segment, cover, route, locate, extract, verify
    //! and frame, each a small model task that code checks, plus the steps it
    //! publishes while it runs.

    pub use turnframe_understand::*;
}

pub mod tasks {
    //! The engine that runs one small, schema-bound model task: profiles, repairs,
    //! votes, escalation, budgets, and the record of every call.

    pub use turnframe_tasks::*;
}

pub mod policy {
    //! The deterministic decision: given a command's policy and its origin,
    //! whether the command may run now, needs confirming first, or is refused.

    pub use turnframe_core::policy::*;
}

pub mod reduce {
    //! The whole-turn reduction: one pass over the accepted plan that decides
    //! every act together rather than each on its own.

    pub use turnframe_core::reduce::*;
}

pub mod target {
    //! Deterministic target resolution. The model never sees a record
    //! identifier; it gets an opaque per-turn handle, and the map back lives
    //! here and stays on the server.

    pub use turnframe_core::target::*;
}

pub mod read {
    //! The read-only tool contract. A read tool queries and never mutates, and
    //! every result carries where it came from and how far it is trusted.

    pub use turnframe_core::read::*;
}

pub mod knowledge {
    //! The knowledge retrieval contract. Retrieved content is evidence for an
    //! answer and never authorization for a command.

    pub use turnframe_core::knowledge::*;
}

pub mod operation {
    //! What a workflow offers to do: operations, their arguments and examples, and the
    //! values a model understands and code computes, such as dates and money.

    pub use turnframe_core::operation::*;
}

pub mod replay {
    //! Replay records and turn phases: enough of a turn kept to reconstruct why
    //! it produced the commands and the response it did.

    pub use turnframe_core::replay::*;
}

pub mod effort {
    //! How much judgment a turn buys: `low`, `medium` or `high`. More model calls behind
    //! each step, never more authority.

    pub use turnframe_core::effort::*;
}

pub mod runtime {
    //! The turn pipeline itself, one module per stage, so a trace, a phase
    //! marker and a replay record can all point at the same place.
    //!
    //! Every public module of `turnframe-runtime` appears here. That is a rule
    //! rather than a coincidence, and
    //! `crates/turnframe/tests/facade_surface.rs` fails when a new one does not
    //! join them: a module missing from this list is not merely inconvenient,
    //! it is unusable in the ways that matter. An adopter can still *call* what
    //! they cannot name — `orchestrator.planner()` binds a value and every
    //! method on it works — but no signature, struct field or `impl` block can
    //! mention the type, which is most of what an integration needs to write.

    pub use turnframe_runtime::{
        attachments, budget, compose, config, conversation, copy, dispatch, divergence, effort,
        execute, interactions, orchestrator, planning, policy, recover, reduce, resolve, resume,
        stream, trace,
    };
}

pub mod provider {
    //! The provider-neutral model layer: normalized requests and responses,
    //! capabilities declared per provider-model pair, capability-first routing,
    //! bounded fallback, and the conformance suite every adapter passes.
    //!
    //! Each enabled provider feature adds its adapter as a submodule.

    pub use turnframe_provider::*;

    /// OpenAI, Azure OpenAI and OpenAI-compatible endpoints, selected by
    /// endpoint profile.
    #[cfg(feature = "openai")]
    pub mod openai {
        pub use turnframe_provider_openai::*;
    }

    /// The Anthropic Messages API.
    #[cfg(feature = "anthropic")]
    pub mod anthropic {
        pub use turnframe_provider_anthropic::*;
    }

    /// Google Gemini and Vertex AI behind one adapter.
    #[cfg(feature = "gemini")]
    pub mod gemini {
        pub use turnframe_provider_gemini::*;
    }

    /// AWS Bedrock Converse.
    #[cfg(feature = "bedrock")]
    pub mod bedrock {
        pub use turnframe_provider_bedrock::*;
    }

    /// A local Ollama daemon.
    #[cfg(feature = "ollama")]
    pub mod ollama {
        pub use turnframe_provider_ollama::*;
    }
}

pub mod prompt {
    //! Where prompt text comes from, and which prompt text produced a turn.
    //!
    //! The contract — the source trait, the selector, the error family and the
    //! reference a replay record stores — is always here. The `prompts` feature
    //! adds the sources this workspace ships.
    //!
    //! **The recommended production setup is prompts served from the adopter's
    //! own repository**, compiled into the binary with
    //! `FilePromptSource` and its `prompt_dir!` macro. The reason is not convenience: a turn's meaning
    //! must not change because somebody edited a registry entry while the
    //! system was running, and a registry outage must not stop the assistant
    //! from answering.
    //!
    //! An adopter who does use the registry should **pin a version** rather
    //! than track a moving label, so a registry edit cannot silently change a
    //! running system's behaviour — and because the pinned version is what the
    //! replay record cites.

    pub use turnframe_core::prompt::*;

    #[cfg(feature = "prompts")]
    pub use turnframe_prompt::{
        CachedPromptSource, Clock, FilePromptSource, ManualClock, PromptFile, SystemClock,
        VERSION_HEX_LEN, cache, files, prompt_dir, version_of,
    };

    /// A source backed by a Langfuse project, over the Langfuse v4 public API.
    ///
    /// Enabling it introduces a runtime dependency on a remote service; read
    /// the module documentation before choosing it.
    #[cfg(feature = "langfuse")]
    pub use turnframe_prompt::langfuse;
}

pub mod store {
    //! What must be durable, and with which rules: seven object-safe traits, a
    //! deterministic in-memory implementation, and a conformance suite that
    //! proves an implementation right without reading its code.

    pub use turnframe_store::*;

    /// The PostgreSQL reference store: migrations, the indexes that enforce the
    /// invariants, and expected-revision transactions.
    #[cfg(feature = "postgres")]
    pub mod postgres {
        pub use turnframe_store_postgres::*;
    }
}

/// Metrics, tracing spans and the dashboard description a running Turnframe
/// application reports through.
#[cfg(feature = "telemetry")]
pub mod telemetry {
    pub use turnframe_telemetry::*;
}

/// The test kit: scripted providers, fake stores with failure injection,
/// bounded workflow exploration, replay assertions, and two complete sample
/// domains (a trip, a traveler and an expense claim) to build examples and tests against.
#[cfg(feature = "test-kit")]
pub mod testing {
    pub use turnframe_test::*;
}

/// The model evaluation harness: corpora, execution samples kept separate from
/// judge votes, and deterministic assertions over a turn.
#[cfg(feature = "eval")]
pub mod evaluation {
    pub use turnframe_eval::*;
}

// ---------------------------------------------------------------------------
// The working vocabulary, at the root.
// ---------------------------------------------------------------------------

pub use turnframe_core::case::{CaseKey, CaseRef, Versioned};
pub use turnframe_core::command::{
    AtomicityScope, ClaimMode, CommandBatch, CommandEnvelope, CommandOrigin, CommandPolicy,
    ConfirmationPolicy, IdempotencyKey, ResolutionChannel, RiskClass, origin_satisfies,
};
pub use turnframe_core::error::{
    DomainRejection, ExecutionError, InvariantViolation, OrchestratorError, RejectionCode,
    RevisionConflict, StoreError,
};
pub use turnframe_core::event::{
    Commit, CommittedEvent, EventRedaction, EventRef, ExternalStatus, OperationalReceipt,
    ReceiptEvent, ReceiptSeverity, RedactedEvent,
};
pub use turnframe_core::flow::{
    InteractionRequirement, ObligationId, PhaseOwnership, ViewOf, WorkflowDefinition,
    WorkflowExecutor, WorkflowNotice, WorkflowRegistry, WorkflowRegistryBuilder, WorkflowView,
    check_view,
};
/// Re-exported at the root rather than only under [`schema`] because
/// [`CommandOrigin`] carries one: an origin cannot be constructed without
/// naming it, and a type needed to build a root-level enum belongs beside it.
pub use turnframe_core::hash::Digest;
pub use turnframe_core::ids::{
    AccountId, CaseId, CaseRevision, ConversationId, EventId, InteractionId, OperationKey,
    OptionId, RedactionAuthority, TargetToken, TurnId, UserId, WorkflowKey, WorkflowVersion,
};
pub use turnframe_core::interaction::{
    ActionClass, Interaction, InteractionKind, InteractionOption, InteractionPayload,
    InteractionSpec, InteractionStatus, InteractionView, ReviewDiffEntry, StoredInteractionAction,
};
pub use turnframe_core::locale::{Locale, LocalizedText};
pub use turnframe_core::operation::{ArgumentSpec, OperationSpec};
pub use turnframe_core::plan::{ActMutability, TargetPolicy};
pub use turnframe_core::policy::{PolicyDecision, PolicySnapshot};
pub use turnframe_core::reduce::{PlannedAct, PlannedActResult, ReductionPlan};
pub use turnframe_core::response::{AssistantTurn, ResponseBlock, ServerNotice};
pub use turnframe_core::turn::{ActorContext, InteractionResponse, TurnInput};
pub use turnframe_core::understanding::{
    ActId, ActTarget, ConstraintKind, Understanding, UnderstoodAct,
};
pub use turnframe_runtime::config::{OrchestrationMode, OrchestratorConfig};
pub use turnframe_runtime::orchestrator::{
    BuildError, CaseCandidate, CaseDirectory, Orchestrator, OrchestratorBuilder,
    StaticCaseDirectory, TurnConsequences,
};

/// The set an application reaches for in almost every file.
///
/// ```
/// use turnframe::prelude::*;
///
/// // The vocabulary is in scope: a case reference, a policy, a risk class.
/// let case = CaseRef::new("trip", "trip-1", CaseRevision(3));
/// assert_eq!(case.key(), CaseKey::new("trip", "trip-1"));
/// assert_eq!(CommandPolicy::low_risk().risk, RiskClass::ReversibleLowRisk);
/// ```
pub mod prelude {
    pub use crate::{
        AccountId, ActionClass, ActorContext, AssistantTurn, AtomicityScope, CaseId, CaseKey,
        CaseRef, CaseRevision, ClaimMode, CommandBatch, CommandEnvelope, CommandOrigin,
        CommandPolicy, ConfirmationPolicy, ConversationId, Digest, DomainRejection, ExecutionError,
        IdempotencyKey, Interaction, InteractionId, InteractionKind, InteractionResponse,
        InteractionSpec, InteractionStatus, InteractionView, Locale, LocalizedText, OperationSpec,
        OperationalReceipt, Orchestrator, OrchestratorBuilder, OrchestratorConfig,
        OrchestratorError, PolicySnapshot, RejectionCode, ResolutionChannel, ResponseBlock,
        RiskClass, ServerNotice, TargetPolicy, TurnId, TurnInput, UserId, Versioned,
        WorkflowDefinition, WorkflowExecutor, WorkflowKey, WorkflowRegistry, WorkflowView,
    };
}

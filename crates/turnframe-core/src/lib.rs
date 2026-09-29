//! `turnframe-core`: the contract every Turnframe crate is written against.
//!
//! **The model proposes meaning, deterministic code decides effects, committed
//! events decide claims.** Persisted case state is authoritative and the
//! transcript is history; a [`flow::WorkflowDefinition`] purely projects state
//! into a view; an [`understanding::Understanding`] says what a turn asks, with the
//! words behind every value; a [`reduce::TurnReducer`] gives every act an explicit
//! result; commands travel with a trusted origin, a policy and a
//! stable idempotency key; execution commits events, and every operational claim
//! derives from them ([`response::claim_guard`]). The [architecture guide] walks
//! it step by step.
//!
//! [architecture guide]: https://github.com/turnframe-rs/turnframe/blob/main/docs/architecture.md
//!
//! It has no async runtime, no HTTP client, no database driver and no provider
//! wire types: types, pure functions and traits only. Runtime, stores, providers
//! and the test kit are sibling crates.
//!
//! # Modules
//!
//! * [`ids`], [`case`], [`locale`], [`hash`], [`schema`]: identity, versions,
//!   copy, canonical hashing, schema fingerprints.
//! * [`flow`]: the Flow Map projector, invariants and the erased registry.
//! * [`turn`], [`plan`], [`target`], [`reduce`], [`policy`], [`command`]: the
//!   deterministic path from input to executable commands.
//! * [`interaction`], [`event`], [`response`]: durable cards, the claim ledger
//!   and the ordered response.
//! * [`read`], [`knowledge`]: read-only context acquisition.
//! * [`understanding`]: what a turn was understood to say, the reducer's input.
//! * [`replay`], [`observe`], [`error`]: audit, metrics and the typed error family.
//! * [`prompt`]: where prompt text comes from, and which prompt text produced
//!   a turn. The trait and the reference live here; every concrete source
//!   lives in `turnframe-prompt`, which this crate does not depend on.

#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

/// The crate README, compiled as a doc-test so its example cannot rot.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
mod readme {}

pub mod case;
pub mod command;
pub mod effort;
pub mod error;
pub mod event;
pub mod flow;
pub mod hash;
pub mod ids;
pub mod interaction;
pub mod knowledge;
pub mod locale;
pub mod observe;
pub mod operation;
pub mod plan;
pub mod policy;
pub mod prompt;
pub mod read;
pub mod reduce;
pub mod replay;
pub mod response;
pub mod schema;
pub mod target;
pub mod turn;
pub mod understanding;

/// The most used items, for `use turnframe_core::prelude::*`.
pub mod prelude {
    pub use crate::case::{CaseKey, CaseRef, Versioned};
    pub use crate::command::{
        AtomicityScope, ClaimMode, CommandBatch, CommandEnvelope, CommandOrigin, CommandPolicy,
        ConfirmationPolicy, IdempotencyKey, ResolutionChannel, RiskClass, origin_satisfies,
    };
    pub use crate::error::{
        DomainRejection, ErasedCallError, ErasureError, ErrorClassification, ExecutionError,
        HashError, InteractionSpecError, InvariantViolation, OrchestratorError, RejectionCode,
        RevisionConflict, StoreError,
    };
    pub use crate::event::{
        Commit, CommittedEvent, EventRedaction, EventRef, ExternalStatus, OperationalReceipt,
        ReceiptEvent, ReceiptSeverity, RedactedEvent,
    };
    pub use crate::flow::{
        ErasedExecutor, ErasedObligation, ErasedWorkflow, ErasedWorkflowView,
        InteractionRequirement, ObligationId, PhaseOwnership, RegisteredWorkflow,
        TypedWorkflowAdapter, ViewOf, WorkflowDefinition, WorkflowExecutor, WorkflowNotice,
        WorkflowRegistry, WorkflowRegistryBuilder, WorkflowView, check_erased_view, check_view,
    };
    pub use crate::hash::Digest;
    pub use crate::ids::{
        AccountId, AttemptId, BatchId, BlockId, CaseId, CaseRevision, CommandId, ConversationId,
        EventId, InteractionId, ModelKey, OperationKey, OptionId, ProviderKey, ReceiptId,
        RedactionAuthority, TargetToken, TurnId, UserId, WorkflowKey, WorkflowVersion,
    };
    pub use crate::interaction::{
        AcceptedResponse, ActionClass, FieldValue, FreeformPolicy, Interaction, InteractionKind,
        InteractionOption, InteractionPayload, InteractionSpec, InteractionStatus, InteractionView,
        ReviewDiffEntry, StoredInteractionAction, TextResolutionPolicy, validate_response,
    };
    pub use crate::locale::{Locale, LocalizedText};
    pub use crate::operation::{ArgumentSpec, OperationCatalog, OperationSpec};
    pub use crate::plan::limits::PlanLimits;
    pub use crate::plan::{ActAvailability, ActMutability, AnswerBasis, TargetPolicy};
    pub use crate::policy::{PolicyDecision, PolicySnapshot};
    pub use crate::prompt::{
        LoadedPrompt, PromptError, PromptName, PromptRef, PromptSelector, PromptSource,
        PromptVersion,
    };
    pub use crate::reduce::{
        AnswerTask, CommandRef, PlannedAct, PlannedActResult, ReductionContext, ReductionPlan,
        TurnReducer,
    };
    pub use crate::response::{AssistantTurn, ResponseBlock, ServerNotice};
    pub use crate::schema::schema_for;
    pub use crate::target::{ResolvedAct, ResolvedActKind, TargetResolution, TargetTokenMap};
    pub use crate::turn::{ActorContext, InteractionResponse, TurnInput, TurnLimits};
    pub use crate::understanding::{
        ActId, ActStatus, ActTarget, Understanding, UnderstoodAct, UnitId,
    };
}

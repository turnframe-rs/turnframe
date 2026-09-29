//! The typed error family of spec §24.
//!
//! Every error carries identifiers and codes only. `Display` output is safe to
//! log: it never includes secrets, free user text, quotes or payloads.
//!
//! [`OrchestratorError`] is the top-level sum type the runtime returns.
//! [`ErrorClassification`] answers the five operational questions of §24 for it:
//! is it retryable, may an effect have happened, which user-safe message key
//! applies, how severe is it, and does it require a reconciliation job.

use serde::{Deserialize, Serialize};

use crate::case::CaseRef;
use crate::command::RiskClass;
use crate::ids::{
    CaseRevision, CommandId, ConversationId, InteractionId, ModelKey, OperationKey, OptionId,
    ProviderKey, TargetToken, WorkflowKey, WorkflowVersion, string_id,
};
use crate::interaction::{InteractionKind, InteractionRejection, InteractionStatus};
use crate::locale::LocalizedText;
use crate::plan::limits::PlanLimitError;
use crate::reduce::CommandRef;
use crate::understanding::ActId;

pub use crate::event::UnknownOutcome;
pub use crate::hash::HashError;

string_id! {
    /// Application-defined rejection code (e.g. `"trip.traveler_missing"`).
    RejectionCode
}

/// The rejection code a workflow uses when `compile_act` does not recognise an
/// operation at all.
///
/// # Why this one string is reserved
///
/// A workflow declares its operations in `interpretation_catalog` and turns
/// them into commands in `compile_act`. The two are different functions and
/// nothing relates them, so an operation added to one and forgotten in the
/// other fails as far downstream as a mistake can: the catalogue offers it, the
/// interpreter proposes it correctly, and the user is told his request could
/// not be carried out — on a sentence that was understood perfectly.
///
/// A workflow that returns *this* code says which of the two it is, and two
/// things follow. The state explorer walks every reachable state, asks each
/// state's catalogue whether `compile_act` knows its operations, and reports
/// the ones that come back with it — the drift becomes a test failure in the
/// workflow's own suite instead of a sentence a user reads three times. And the
/// runtime treats it as its own refusal rather than the domain's, because a
/// catalogue and a compiler that disagree is a defect in the deployment and not
/// an outcome for the user.
///
/// A workflow that uses its own code keeps today's behaviour exactly: its words
/// reach the user and nothing checks the drift. The check sees what the
/// workflow declares, and this constant is the declaration.
pub const UNKNOWN_OPERATION: &str = "turnframe.operation.unknown";

/// A domain refused an act or a command (spec §8.2).
///
/// `details` is application-defined structured data for the UI; it must not be
/// interpolated into logs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("domain rejected the request with code {code} (message key {message_key})")]
pub struct DomainRejection {
    /// Stable machine-readable code.
    pub code: RejectionCode,
    /// Key of the user-facing message in the application's copy catalog.
    pub message_key: String,
    /// Structured details (field names, limits...).
    ///
    /// Boxed for the same reason [`Self::explanation`] is: a rejection travels
    /// in the `Err` half of every compile and validate call, so the size of the
    /// unhappy path is the size of every call's return value. A JSON document
    /// is the largest thing here and the least often read.
    #[serde(default)]
    pub details: Box<serde_json::Value>,
    /// Why, in words the user can read.
    ///
    /// [`Self::message_key`] names copy in the application's own catalog, which
    /// is the right shape when an application has one and no use to the runtime
    /// when it does not: composition has to write a sentence and cannot resolve
    /// a key it knows nothing about. So a rejection may carry its own copy, the
    /// way a receipt does.
    ///
    /// When it does, the turn tells the user the act was refused and why.
    /// When it does not, the turn still says an act was refused, in the
    /// runtime's own words, because the alternative is what this field was
    /// added to fix: an assistant that cannot report a failure says something
    /// unrelated instead, and the user concludes the write succeeded.
    ///
    /// Boxed because a `DomainRejection` travels inside the `Err` half of every
    /// compile and validate call, and copy is much larger than a code: putting
    /// it inline widens the error variant that every one of those returns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<Box<LocalizedText>>,
    /// The argument the rejection is about, as a JSON Pointer into the act's arguments.
    ///
    /// Present, the value can be asked for again; absent, the act is refused as it stands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub argument: Option<String>,
}

impl DomainRejection {
    /// Builds a rejection without details.
    #[must_use]
    pub fn new(code: impl Into<RejectionCode>, message_key: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            explanation: None,
            argument: None,
            message_key: message_key.into(),
            details: Box::new(serde_json::Value::Null),
        }
    }

    /// Attaches structured details.
    #[must_use]
    pub fn with_details(mut self, details: serde_json::Value) -> Self {
        self.details = Box::new(details);
        self
    }

    /// Says the rejection is about one argument, which the user may give again.
    #[must_use]
    pub fn on_argument(mut self, pointer: impl Into<String>) -> Self {
        self.argument = Some(pointer.into());
        self
    }

    /// Attaches the sentence the user should read.
    ///
    /// Write it the way you write a receipt's body: server-authored copy, in
    /// the languages the workflow answers in. It reaches the user as a notice
    /// whether or not a model runs, and the narrator is shown it so its prose
    /// does not contradict what the notice says.
    #[must_use]
    pub fn with_explanation(mut self, explanation: LocalizedText) -> Self {
        self.explanation = Some(Box::new(explanation));
        self
    }
}

/// A command targeted a revision that is no longer current (I13).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error(
    "revision conflict on {}/{}: expected {}, current {current_revision}",
    expected.workflow, expected.case_id, expected.expected_revision
)]
pub struct RevisionConflict {
    /// The reference the command was planned against.
    pub expected: CaseRef,
    /// The revision actually found.
    pub current_revision: CaseRevision,
}

/// Errors raised by persistence adapters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[non_exhaustive]
pub enum StoreError {
    /// The requested record does not exist for this account. Never reveals
    /// whether it exists for another tenant.
    #[error("record not found")]
    NotFound,
    /// A uniqueness or compare-and-swap constraint failed.
    #[error("store constraint conflict")]
    Conflict,
    /// The store could not be reached.
    #[error("store unavailable")]
    Unavailable,
    /// The operation timed out; a write may or may not have landed.
    #[error("store operation timed out")]
    Timeout,
    /// A stored payload could not be (de)serialized.
    #[error("stored payload could not be serialized or deserialized")]
    Serialization,
    /// The stored data violates an invariant the adapter relies on.
    #[error("stored data is corrupt")]
    Corrupt,
    /// Adapter-specific failure identified by a stable code.
    #[error("store failure {code}")]
    Other {
        /// Stable adapter-defined code.
        code: String,
    },
}

/// Errors raised while executing a command batch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[non_exhaustive]
pub enum ExecutionError {
    /// The expected revision was stale.
    #[error(transparent)]
    RevisionConflict(RevisionConflict),
    /// The domain refused the command.
    #[error(transparent)]
    Rejected(DomainRejection),
    /// Persistence failed.
    #[error(transparent)]
    Store(StoreError),
    /// An external effect was attempted and its outcome is unknown (I15).
    #[error(transparent)]
    OutcomeUnknown(UnknownOutcome),
    /// The idempotency key was seen before with a different command payload.
    #[error("idempotency key reused with a different command {command_id}")]
    IdempotencyMismatch {
        /// The offending command.
        command_id: CommandId,
    },
    /// The batch mixed cases while the scope required a single case.
    #[error("batch scope violation")]
    ScopeViolation,
    /// Execution exceeded its time budget; a commit may have happened.
    #[error("execution timed out")]
    Timeout,
    /// The erased boundary could not convert a command, event or state.
    #[error(transparent)]
    Erasure(ErasureError),
    /// Executor-specific failure identified by a stable code.
    #[error("execution failure {code}")]
    Other {
        /// Stable executor-defined code.
        code: String,
    },
}

impl From<ErasureError> for ExecutionError {
    fn from(value: ErasureError) -> Self {
        Self::Erasure(value)
    }
}

/// A projection violated one of the Flow Map invariants (spec §8.4).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("invariant violation on {}/{}: {kind}", case_ref.workflow, case_ref.case_id)]
pub struct InvariantViolation {
    /// The projected case.
    pub case_ref: CaseRef,
    /// Which rule was broken.
    pub kind: InvariantViolationKind,
}

/// The individual Flow Map invariants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum InvariantViolationKind {
    /// An outcome is present while obligations remain open.
    #[error("outcome present while {obligation_count} obligations remain")]
    OutcomeWithObligations {
        /// Number of open obligations.
        obligation_count: usize,
    },
    /// A user-owned phase has no blocking interaction requirement (I6).
    #[error("user-owned phase without a blocking interaction")]
    MissingBlockingInteraction,
    /// A terminal phase declares a blocking interaction.
    #[error("blocking interaction on a terminal phase")]
    BlockingInteractionOnTerminalPhase,
    /// A system- or external-owned phase declares a blocking interaction.
    #[error("blocking interaction on a phase not owned by the user")]
    BlockingInteractionOnNonUserPhase,
    /// The requirement in `blocking_interaction` is flagged non-blocking.
    #[error("blocking_interaction slot holds a non-blocking requirement")]
    NonBlockingRequirementInBlockingSlot,
    /// A terminal phase has no outcome.
    #[error("terminal phase without outcome")]
    TerminalPhaseWithoutOutcome,
    /// A non-terminal phase carries an outcome.
    #[error("outcome present on a non-terminal phase")]
    OutcomeOnNonTerminalPhase,
    /// Two obligations serialize to the same stable identifier.
    #[error("duplicate obligation id {obligation_id}")]
    DuplicateObligation {
        /// The repeated identifier (canonical JSON of the obligation).
        obligation_id: String,
    },
    /// An obligation could not be serialized to derive its identifier.
    #[error("obligation could not be serialized")]
    UnserializableObligation,
    /// The blocking requirement carries a payload the user could not answer.
    #[error("blocking interaction cannot be answered: {error}")]
    UnanswerableBlockingInteraction {
        /// Why the card is unanswerable.
        error: InteractionSpecError,
    },
}

/// Target resolution failed for an act (spec §12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TargetError {
    /// Several authorized cases match; a selection interaction is required.
    #[error("ambiguous target with {candidate_count} candidates")]
    Ambiguous {
        /// Number of candidates.
        candidate_count: usize,
    },
    /// The token was issued this turn but the case is gone.
    #[error("target {token} missing")]
    Missing {
        /// The token.
        token: TargetToken,
    },
    /// The token is unknown or belongs to another tenant (indistinguishable).
    #[error("target {token} unauthorized")]
    Unauthorized {
        /// The token.
        token: TargetToken,
    },
    /// The case moved past the revision the token was issued at.
    #[error("target {token} stale: issued at {issued_revision}, current {current_revision}")]
    Stale {
        /// The token.
        token: TargetToken,
        /// Revision at issue time.
        issued_revision: CaseRevision,
        /// Revision now.
        current_revision: CaseRevision,
    },
    /// A `Mention` target could not be matched to any candidate.
    #[error("mention could not be resolved for workflow {workflow}")]
    MentionUnresolved {
        /// Workflow named by the mention.
        workflow: WorkflowKey,
    },
    /// `ActiveInteraction` was used but no active blocking interaction exists.
    #[error("no active interaction to target")]
    NoActiveInteraction,
    /// The act's target policy forbids the proposed target kind.
    #[error("target kind not allowed by the policy of operation {operation}")]
    PolicyMismatch {
        /// The operation whose policy was violated.
        operation: OperationKey,
    },
}

/// The whole-turn reducer could not produce a plan (spec §13).
///
/// The wire form is adjacently tagged (`{"kind": ..., "detail": ...}`): several
/// variants wrap another tagged error, and an internal tag would write two
/// `kind` keys into one object, so the outer variant was lost on the way back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ReductionError {
    /// Plan limits were exceeded.
    #[error(transparent)]
    Limits(PlanLimitError),
    /// An act names an operation not present in the catalog.
    #[error("act {act} uses unknown operation {operation}")]
    UnknownOperation {
        /// The act.
        act: ActId,
        /// The operation.
        operation: OperationKey,
    },
    /// An act's arguments do not satisfy the operation's input schema.
    #[error("act {act} has invalid arguments")]
    InvalidArguments {
        /// The act.
        act: ActId,
        /// Validation detail.
        error: SchemaValidationError,
    },
    /// An act references a case view that is not in the context.
    #[error("act {act} references a case that is not loaded")]
    CaseNotLoaded {
        /// The act.
        act: ActId,
    },
    /// The produced plan is inconsistent (missing act results, dangling refs).
    #[error("reduction plan inconsistent: {detail}")]
    InconsistentPlan {
        /// Stable description of the inconsistency (no user text).
        detail: String,
    },
    /// Hashing the plan failed.
    #[error("plan hash could not be computed")]
    Hash,
    /// Two act definitions were offered under the same operation key, so one
    /// would silently shadow the other.
    #[error("duplicate operation {operation} in the act catalog")]
    DuplicateOperation {
        /// The repeated key.
        operation: OperationKey,
    },
    /// Two acts contradict each other and no precedence rule applies.
    #[error("acts {first} and {second} contradict without a precedence rule")]
    Contradiction {
        /// The first act.
        first: ActId,
        /// The second act.
        second: ActId,
    },
    /// The turn compiled more commands than the runtime is configured to
    /// execute.
    ///
    /// The whole turn is refused rather than a prefix executed: a plan that
    /// runs half of what the user asked for is exactly the silent partial
    /// application I10 and I11 exist to prevent.
    #[error("turn compiled {actual} commands, more than the limit of {limit}")]
    CommandBudgetExceeded {
        /// The configured maximum.
        limit: usize,
        /// How many commands the turn compiled.
        actual: usize,
    },
}

impl From<PlanLimitError> for ReductionError {
    fn from(value: PlanLimitError) -> Self {
        Self::Limits(value)
    }
}

/// A JSON Schema validation failure described by paths only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("schema violation at {instance_path} (schema {schema_path})")]
pub struct SchemaValidationError {
    /// JSON pointer into the validated instance.
    pub instance_path: String,
    /// JSON pointer into the schema.
    pub schema_path: String,
}

/// Validating a JSON value against a schema failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SchemaCheckError {
    /// The schema itself could not be compiled.
    #[error("schema could not be compiled")]
    InvalidSchema,
    /// The instance violates the schema.
    #[error(transparent)]
    Violation(SchemaValidationError),
}

/// A card as specified could not be answered, or claims an authority it may
/// not have (spec §15.2, §15.7, I6).
///
/// Refusing at creation is deliberate: a persisted card with no usable option
/// blocks its case forever, and a high-risk card that accepts typed text turns
/// an inference into a confirmation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum InteractionSpecError {
    /// Two options share an identifier, so the answer would be ambiguous.
    #[error("duplicate option id {option_id}")]
    DuplicateOptionId {
        /// The repeated identifier.
        option_id: OptionId,
    },
    /// The card carries fewer options than its kind needs.
    #[error("{interaction_kind:?} card needs at least {required} options, found {found}")]
    NotEnoughOptions {
        /// The kind.
        interaction_kind: InteractionKind,
        /// Options the kind requires.
        required: usize,
        /// Options found.
        found: usize,
    },
    /// No option of a confirming card actually authorizes commands.
    #[error("{interaction_kind:?} card has no option that authorizes commands")]
    MissingAuthorizingOption {
        /// The kind.
        interaction_kind: InteractionKind,
    },
    /// No option lets the user decline, so refusing is impossible.
    #[error("{interaction_kind:?} card has no declining option")]
    MissingDeclineOption {
        /// The kind.
        interaction_kind: InteractionKind,
    },
    /// A review card shows no diff.
    #[error("review card has no diff entries")]
    MissingReviewEntries,
    /// A free-form card has no prompt.
    #[error("freeform card has no prompt")]
    MissingFreeformPrompt,
    /// A free-form card has no option that accepts the required text.
    #[error("freeform card has no option requiring free text")]
    MissingFreeformOption,
    /// The kind cannot be answered through the input protocol.
    #[error("{interaction_kind:?} cards cannot be persisted")]
    UnsupportedKind {
        /// The kind.
        interaction_kind: InteractionKind,
    },
    /// Typed text may not resolve this card.
    #[error("{interaction_kind:?} card confirming {confirms_risk:?} may not be resolved from text")]
    TextResolutionNotAllowed {
        /// The kind.
        interaction_kind: InteractionKind,
        /// What the card confirms.
        confirms_risk: RiskClass,
    },
}

/// Interaction lifecycle errors (spec §15).
///
/// The wire form is adjacently tagged (`{"kind": ..., "detail": ...}`): several
/// variants wrap another tagged error, and an internal tag would write two
/// `kind` keys into one object, so the outer variant was lost on the way back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
#[non_exhaustive]
pub enum InteractionError {
    /// A client response was rejected by [`crate::interaction::validate_response`].
    #[error(transparent)]
    Rejected(InteractionRejection),
    /// An illegal status transition was attempted.
    #[error("illegal interaction transition {from:?} -> {to:?} on {interaction_id}")]
    InvalidTransition {
        /// The interaction.
        interaction_id: InteractionId,
        /// Current status.
        from: InteractionStatus,
        /// Requested status.
        to: InteractionStatus,
    },
    /// A second active blocking interaction was requested for the same case (I5).
    #[error("case already has an active blocking interaction {existing}")]
    BlockingConflict {
        /// The interaction already active.
        existing: InteractionId,
    },
    /// The stored payload hash does not match the payload.
    #[error("payload hash mismatch on {interaction_id}")]
    PayloadHashMismatch {
        /// The interaction.
        interaction_id: InteractionId,
    },
    /// The interaction must be persisted before it can be referenced.
    #[error("interaction not persisted")]
    NotPersisted,
    /// The card was refused before it could be created.
    #[error(transparent)]
    InvalidSpec(InteractionSpecError),
    /// The time to live cannot be applied to the creation instant.
    #[error("interaction time to live is out of range")]
    InvalidTtl,
    /// The payload could not be hashed.
    #[error("interaction payload could not be hashed")]
    Hash,
}

impl From<InteractionRejection> for InteractionError {
    fn from(value: InteractionRejection) -> Self {
        Self::Rejected(value)
    }
}

impl From<InteractionSpecError> for InteractionError {
    fn from(value: InteractionSpecError) -> Self {
        Self::InvalidSpec(value)
    }
}

/// Policy evaluation refused a command (spec §14.3, I12).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PolicyError {
    /// The command origin is not trusted enough for the policy.
    #[error("command {command_ref} requires a trusted origin")]
    UntrustedOrigin {
        /// The command.
        command_ref: CommandRef,
    },
    /// The risk class is forbidden in the current configuration (e.g. sandbox).
    #[error("command {command_ref} has a forbidden risk class")]
    ForbiddenRiskClass {
        /// The command.
        command_ref: CommandRef,
    },
    /// The policy engine denied the command with a reason key.
    #[error("command {command_ref} denied ({reason_key})")]
    Denied {
        /// The command.
        command_ref: CommandRef,
        /// Key of the user-facing reason.
        reason_key: String,
    },
    /// The policy source could not be consulted; the runtime fails closed.
    #[error("policy source unavailable")]
    Unavailable,
    /// The turn spent the resource budget its orchestration mode allows
    /// (spec §11.1), and stopped rather than continuing on credit.
    ///
    /// `limit` is the stable snake-case name of the bound that ran out, so an
    /// operator can tell "the model was called too often" from "the turn took
    /// too long" without parsing a sentence. The runtime that raises it owns
    /// the vocabulary of names; the library only carries it.
    #[error("resource budget exhausted ({limit})")]
    BudgetExhausted {
        /// Stable snake-case name of the bound that ran out.
        limit: String,
    },
}

/// The actor is not allowed to do what the turn asks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum AuthorizationError {
    /// The conversation does not belong to the actor's account.
    #[error("conversation {conversation_id} not accessible")]
    ConversationNotAccessible {
        /// The conversation.
        conversation_id: ConversationId,
    },
    /// The actor lacks a role or permission.
    #[error("forbidden ({reason_key})")]
    Forbidden {
        /// Key of the user-facing reason.
        reason_key: String,
    },
    /// The actor's account does not match the record's account.
    #[error("account mismatch")]
    AccountMismatch,
}

/// The turn input is malformed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum InvalidInputError {
    /// Neither text, nor interaction response, nor attachments were supplied.
    #[error("turn carries no text, interaction response or attachment")]
    EmptyTurn,
    /// The text exceeds the configured maximum.
    #[error("text exceeds {max_bytes} bytes")]
    TextTooLong {
        /// The limit.
        max_bytes: usize,
    },
    /// Too many attachments.
    #[error("more than {max} attachments")]
    TooManyAttachments {
        /// The limit.
        max: usize,
    },
    /// The locale tag is empty.
    #[error("empty locale")]
    EmptyLocale,
    /// The account identifier is empty.
    #[error("empty account id")]
    EmptyAccount,
}

/// Normalized description of a provider failure.
///
/// The provider crate maps its rich error into this so the core error family
/// stays free of wire types. Never contains raw provider bodies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("provider {provider_key} failed: {code:?}")]
pub struct ProviderFailure {
    /// Provider identifier.
    pub provider_key: ProviderKey,
    /// Model identifier when known.
    pub model_key: Option<ModelKey>,
    /// Normalized failure code.
    pub code: ProviderFailureCode,
    /// Whether the provider crate considers it retryable.
    pub retryable: bool,
    /// The endpoint's own sentence about the refusal, sanitized by the adapter.
    ///
    /// A code says which family a failure belongs to; it cannot say what was
    /// wrong with the request. For the one family where that is the caller's
    /// own bug — a malformed request — a provider that is down and a function
    /// schema missing `properties` were indistinguishable from outside, and
    /// telling them apart took a proxy reading a body this library had already
    /// read and discarded.
    ///
    /// Absent when the adapter had nothing, and never rendered to a user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Normalized provider failure categories (spec §20.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProviderFailureCode {
    /// The request timed out.
    Timeout,
    /// The provider rate-limited the request.
    RateLimited,
    /// Authentication failed.
    Authentication,
    /// A credential that was valid has expired.
    ///
    /// Distinct from [`Authentication`](Self::Authentication) because a bad key
    /// stays bad while an expired token becomes valid again after a refresh:
    /// providers that issue short-lived credentials (Vertex AI bearer tokens,
    /// Bedrock session credentials) fail this way as a matter of course, and a
    /// replay record that could not tell the two apart would make a refresh
    /// gap indistinguishable from a misconfiguration.
    CredentialExpired,
    /// The account's quota or credit balance is exhausted.
    ///
    /// Distinct from [`RateLimited`](Self::RateLimited) because a rate limit
    /// clears by waiting and a quota does not: it clears when a window resets
    /// or a human tops the account up.
    QuotaExhausted,
    /// The prompt exceeded the context window.
    ContextOverflow,
    /// The response could not be parsed.
    Malformed,
    /// The model refused.
    Refusal,
    /// The provider/model lacks a required capability (no silent downgrade).
    CapabilityMismatch,
    /// The request was cancelled.
    Cancelled,
    /// The provider returned a server error.
    ServerError,
    /// Anything else.
    Other,
}

/// Errors raised by the type-erasure boundary of the workflow registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErasureError {
    /// A stored state could not be deserialized into the workflow's state type.
    #[error("state of workflow {workflow} could not be deserialized")]
    StateDeserialization {
        /// The workflow.
        workflow: WorkflowKey,
    },
    /// A JSON command could not be deserialized into the workflow's command type.
    #[error("command of workflow {workflow} could not be deserialized")]
    CommandDeserialization {
        /// The workflow.
        workflow: WorkflowKey,
    },
    /// A JSON event could not be deserialized into the workflow's event type.
    #[error("event of workflow {workflow} could not be deserialized")]
    EventDeserialization {
        /// The workflow.
        workflow: WorkflowKey,
    },
    /// A typed value could not be serialized at the boundary.
    #[error("value of workflow {workflow} could not be serialized")]
    Serialization {
        /// The workflow.
        workflow: WorkflowKey,
    },
    /// An operation's declaration cannot be used.
    #[error("workflow {workflow} declares an unusable operation: {reason}")]
    InvalidOperation {
        /// The workflow.
        workflow: WorkflowKey,
        /// What is wrong with the declaration.
        reason: String,
    },
    /// The registry has no workflow with this key.
    #[error("unknown workflow {workflow}")]
    UnknownWorkflow {
        /// The key.
        workflow: WorkflowKey,
    },
    /// Two definitions were registered under the same key.
    #[error("duplicate workflow {workflow}")]
    DuplicateWorkflow {
        /// The key.
        workflow: WorkflowKey,
    },
    /// An erased call was asked to compile or build against a case other than
    /// the one the act resolved to, or at another revision.
    #[error("workflow {workflow} call targets a different case than the act resolved to")]
    CaseMismatch {
        /// The workflow.
        workflow: WorkflowKey,
    },
    /// A stored record names a version different from the registered one.
    #[error("workflow {workflow} version mismatch: registered {registered}, found {found}")]
    VersionMismatch {
        /// The key.
        workflow: WorkflowKey,
        /// Version in the registry.
        registered: WorkflowVersion,
        /// Version found on the record.
        found: WorkflowVersion,
    },
}

/// A domain call through the erased boundary failed either because of the
/// boundary itself or because the domain rejected it.
///
/// The wire form is adjacently tagged (`{"kind": ..., "detail": ...}`): several
/// variants wrap another tagged error, and an internal tag would write two
/// `kind` keys into one object, so the outer variant was lost on the way back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ErasedCallError {
    /// The (de)serialization boundary failed.
    #[error(transparent)]
    Erasure(ErasureError),
    /// The domain rejected the call.
    ///
    /// Boxed because this variant travels in the `Err` half of every erased
    /// compile and validate call, and a rejection carrying its own copy is much
    /// larger than the success it displaces.
    #[error(transparent)]
    Rejected(Box<DomainRejection>),
    /// The domain built a card that cannot be answered.
    #[error(transparent)]
    InvalidSpec(InteractionSpecError),
}

impl From<ErasureError> for ErasedCallError {
    fn from(value: ErasureError) -> Self {
        Self::Erasure(value)
    }
}

impl From<InteractionSpecError> for ErasedCallError {
    fn from(value: InteractionSpecError) -> Self {
        Self::InvalidSpec(value)
    }
}

impl From<DomainRejection> for ErasedCallError {
    fn from(value: DomainRejection) -> Self {
        Self::Rejected(Box::new(value))
    }
}

/// Operational severity of an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorSeverity {
    /// Expected in normal operation (a clarification, a rejected act).
    Info,
    /// Worth watching but not an outage.
    Warning,
    /// Something failed that should not fail.
    Error,
    /// A safety boundary was breached or an invariant is broken.
    Critical,
}

/// Answers the classification questions of spec §24 for an error.
pub trait ErrorClassification {
    /// May the whole operation be retried without further analysis?
    fn retryable(&self) -> bool;
    /// May a side effect have happened despite the error?
    fn effect_may_have_happened(&self) -> bool;
    /// Key of a user-safe message in the application's copy catalog.
    fn user_message_key(&self) -> &'static str;
    /// Operational severity.
    fn severity(&self) -> ErrorSeverity;
    /// Must a reconciliation job run before the case is trusted again?
    fn reconciliation_required(&self) -> bool;
}

/// Top-level error of a turn (spec §24).
///
/// The wire form is adjacently tagged (`{"kind": ..., "detail": ...}`): several
/// variants wrap another tagged error, and an internal tag would write two
/// `kind` keys into one object, so the outer variant was lost on the way back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(tag = "kind", content = "detail", rename_all = "snake_case")]
#[non_exhaustive]
pub enum OrchestratorError {
    /// The turn input is malformed.
    #[error(transparent)]
    InvalidInput(InvalidInputError),
    /// The actor is not allowed.
    #[error(transparent)]
    Unauthorized(AuthorizationError),
    /// A model provider failed.
    #[error(transparent)]
    Provider(ProviderFailure),
    /// Target resolution failed.
    #[error(transparent)]
    Target(TargetError),
    /// Reduction failed.
    #[error(transparent)]
    Reduction(ReductionError),
    /// Interaction lifecycle error.
    #[error(transparent)]
    Interaction(InteractionError),
    /// Policy refused.
    #[error(transparent)]
    Policy(PolicyError),
    /// A stale revision was detected.
    #[error(transparent)]
    RevisionConflict(RevisionConflict),
    /// The domain rejected.
    #[error(transparent)]
    DomainRejected(DomainRejection),
    /// Execution failed.
    #[error(transparent)]
    Execution(ExecutionError),
    /// An external effect has an unknown outcome.
    #[error(transparent)]
    ExternalOutcomeUnknown(UnknownOutcome),
    /// Persistence failed.
    #[error(transparent)]
    Store(StoreError),
    /// A Flow Map invariant was violated.
    #[error(transparent)]
    InvariantViolation(InvariantViolation),
    /// The erasure boundary failed.
    #[error(transparent)]
    Erasure(ErasureError),
    /// A library invariant failed: canonical hashing or an internal
    /// serialization the library itself produced. Never caused by user input.
    #[error("internal failure {code}")]
    Internal {
        /// Stable code, one of the constants in [`internal_code`].
        code: String,
    },
}

/// Stable codes used by [`OrchestratorError::Internal`].
pub mod internal_code {
    /// Canonical JSON hashing failed (payload hash, plan hash, fingerprint).
    pub const HASH: &str = "hash";
}

impl From<HashError> for OrchestratorError {
    fn from(_: HashError) -> Self {
        // The source carries a `serde_json::Error`, which cannot be serialized
        // into the (serializable) error family, so only the stable code travels.
        Self::Internal {
            code: internal_code::HASH.to_owned(),
        }
    }
}

macro_rules! orchestrator_from {
    ($($variant:ident($ty:ty)),* $(,)?) => {
        $(
            impl From<$ty> for OrchestratorError {
                fn from(value: $ty) -> Self {
                    Self::$variant(value)
                }
            }
        )*
    };
}

orchestrator_from! {
    InvalidInput(InvalidInputError),
    Unauthorized(AuthorizationError),
    Provider(ProviderFailure),
    Target(TargetError),
    Reduction(ReductionError),
    Interaction(InteractionError),
    Policy(PolicyError),
    RevisionConflict(RevisionConflict),
    DomainRejected(DomainRejection),
    Execution(ExecutionError),
    ExternalOutcomeUnknown(UnknownOutcome),
    Store(StoreError),
    InvariantViolation(InvariantViolation),
    Erasure(ErasureError),
}

impl ErrorClassification for StoreError {
    fn retryable(&self) -> bool {
        matches!(self, Self::Unavailable | Self::Timeout)
    }

    fn effect_may_have_happened(&self) -> bool {
        matches!(self, Self::Timeout)
    }

    fn user_message_key(&self) -> &'static str {
        match self {
            Self::NotFound => "turnframe.error.not_found",
            _ => "turnframe.error.temporary",
        }
    }

    fn severity(&self) -> ErrorSeverity {
        match self {
            Self::NotFound | Self::Conflict => ErrorSeverity::Warning,
            Self::Corrupt => ErrorSeverity::Critical,
            _ => ErrorSeverity::Error,
        }
    }

    fn reconciliation_required(&self) -> bool {
        matches!(self, Self::Timeout | Self::Corrupt)
    }
}

impl ErrorClassification for ExecutionError {
    fn retryable(&self) -> bool {
        match self {
            Self::Store(store) => store.retryable(),
            _ => false,
        }
    }

    fn effect_may_have_happened(&self) -> bool {
        match self {
            Self::OutcomeUnknown(_) | Self::Timeout => true,
            Self::Store(store) => store.effect_may_have_happened(),
            _ => false,
        }
    }

    fn user_message_key(&self) -> &'static str {
        match self {
            Self::RevisionConflict(_) => "turnframe.error.revision_conflict",
            Self::Rejected(_) => "turnframe.error.domain_rejected",
            Self::OutcomeUnknown(_) | Self::Timeout => "turnframe.error.verification_in_progress",
            _ => "turnframe.error.temporary",
        }
    }

    fn severity(&self) -> ErrorSeverity {
        match self {
            Self::RevisionConflict(_) | Self::Rejected(_) => ErrorSeverity::Warning,
            Self::IdempotencyMismatch { .. } | Self::ScopeViolation | Self::Erasure(_) => {
                ErrorSeverity::Critical
            }
            Self::Store(store) => store.severity(),
            _ => ErrorSeverity::Error,
        }
    }

    fn reconciliation_required(&self) -> bool {
        match self {
            Self::OutcomeUnknown(_) | Self::Timeout => true,
            Self::Store(store) => store.reconciliation_required(),
            _ => false,
        }
    }
}

impl ErrorClassification for ReductionError {
    fn retryable(&self) -> bool {
        false
    }

    fn effect_may_have_happened(&self) -> bool {
        false
    }

    fn user_message_key(&self) -> &'static str {
        match self {
            Self::Limits(_)
            | Self::UnknownOperation { .. }
            | Self::InvalidArguments { .. }
            | Self::CaseNotLoaded { .. }
            | Self::Contradiction { .. }
            | Self::CommandBudgetExceeded { .. } => "turnframe.error.not_understood",
            Self::InconsistentPlan { .. } | Self::Hash | Self::DuplicateOperation { .. } => {
                "turnframe.error.internal"
            }
        }
    }

    fn severity(&self) -> ErrorSeverity {
        match self {
            Self::Limits(_)
            | Self::UnknownOperation { .. }
            | Self::InvalidArguments { .. }
            | Self::CaseNotLoaded { .. }
            | Self::Contradiction { .. }
            | Self::CommandBudgetExceeded { .. } => ErrorSeverity::Error,
            // Library or domain defects, not user behaviour.
            Self::InconsistentPlan { .. } | Self::Hash | Self::DuplicateOperation { .. } => {
                ErrorSeverity::Critical
            }
        }
    }

    fn reconciliation_required(&self) -> bool {
        false
    }
}

impl ErrorClassification for OrchestratorError {
    fn retryable(&self) -> bool {
        match self {
            Self::Provider(failure) => failure.retryable,
            Self::RevisionConflict(_) => true,
            Self::Reduction(inner) => inner.retryable(),
            Self::Execution(inner) => inner.retryable(),
            Self::Store(inner) => inner.retryable(),
            _ => false,
        }
    }

    fn effect_may_have_happened(&self) -> bool {
        match self {
            Self::ExternalOutcomeUnknown(_) => true,
            Self::Execution(inner) => inner.effect_may_have_happened(),
            Self::Store(inner) => inner.effect_may_have_happened(),
            _ => false,
        }
    }

    fn user_message_key(&self) -> &'static str {
        match self {
            Self::InvalidInput(_) => "turnframe.error.invalid_input",
            Self::Unauthorized(_) => "turnframe.error.unauthorized",
            Self::Provider(_) => "turnframe.error.assistant_unavailable",
            Self::Target(_) => "turnframe.error.target",
            Self::Reduction(inner) => inner.user_message_key(),
            Self::Interaction(_) => "turnframe.error.interaction",
            Self::Policy(_) => "turnframe.error.policy",
            Self::RevisionConflict(_) => "turnframe.error.revision_conflict",
            Self::DomainRejected(_) => "turnframe.error.domain_rejected",
            Self::Execution(inner) => inner.user_message_key(),
            Self::ExternalOutcomeUnknown(_) => "turnframe.error.verification_in_progress",
            Self::Store(inner) => inner.user_message_key(),
            Self::InvariantViolation(_) | Self::Erasure(_) | Self::Internal { .. } => {
                "turnframe.error.internal"
            }
        }
    }

    fn severity(&self) -> ErrorSeverity {
        match self {
            Self::Target(_) | Self::DomainRejected(_) => ErrorSeverity::Info,
            Self::InvalidInput(_)
            | Self::Unauthorized(_)
            | Self::Interaction(_)
            | Self::Policy(_)
            | Self::RevisionConflict(_) => ErrorSeverity::Warning,
            Self::Provider(_) | Self::ExternalOutcomeUnknown(_) => ErrorSeverity::Error,
            Self::Reduction(inner) => inner.severity(),
            Self::Execution(inner) => inner.severity(),
            Self::Store(inner) => inner.severity(),
            Self::InvariantViolation(_) | Self::Erasure(_) | Self::Internal { .. } => {
                ErrorSeverity::Critical
            }
        }
    }

    fn reconciliation_required(&self) -> bool {
        match self {
            Self::ExternalOutcomeUnknown(_) => true,
            Self::Execution(inner) => inner.reconciliation_required(),
            Self::Store(inner) => inner.reconciliation_required(),
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::UnknownOutcome;
    use crate::ids::AttemptId;

    /// Every variant of the top-level error, so the classification table is
    /// exercised as a whole rather than in three samples.
    fn every_orchestrator_error() -> Vec<OrchestratorError> {
        vec![
            OrchestratorError::InvalidInput(InvalidInputError::EmptyTurn),
            OrchestratorError::Unauthorized(AuthorizationError::AccountMismatch),
            OrchestratorError::Provider(ProviderFailure {
                provider_key: crate::ids::ProviderKey::from("p"),
                model_key: None,
                code: ProviderFailureCode::Timeout,
                retryable: true,
                detail: None,
            }),
            OrchestratorError::Target(TargetError::NoActiveInteraction),
            OrchestratorError::Reduction(ReductionError::Limits(PlanLimitError {
                kind: crate::plan::limits::PlanLimitKind::Acts,
                limit: 1,
                actual: 2,
            })),
            OrchestratorError::Reduction(ReductionError::InconsistentPlan { detail: "d".into() }),
            OrchestratorError::Interaction(InteractionError::NotPersisted),
            OrchestratorError::Policy(PolicyError::Unavailable),
            OrchestratorError::RevisionConflict(RevisionConflict {
                expected: CaseRef::new("w", "c", CaseRevision(1)),
                current_revision: CaseRevision(2),
            }),
            OrchestratorError::DomainRejected(DomainRejection::new("c", "k")),
            OrchestratorError::Execution(ExecutionError::Timeout),
            OrchestratorError::ExternalOutcomeUnknown(UnknownOutcome {
                attempt_id: AttemptId::from("a1"),
                remote_ref: None,
                reason: "timeout".into(),
            }),
            OrchestratorError::Store(StoreError::Unavailable),
            OrchestratorError::InvariantViolation(InvariantViolation {
                case_ref: CaseRef::new("w", "c", CaseRevision(1)),
                kind: InvariantViolationKind::TerminalPhaseWithoutOutcome,
            }),
            OrchestratorError::Erasure(ErasureError::UnknownWorkflow {
                workflow: WorkflowKey::from("w"),
            }),
            OrchestratorError::Internal {
                code: internal_code::HASH.to_owned(),
            },
        ]
    }

    #[test]
    fn every_variant_is_classified_and_safe_to_log() {
        for error in every_orchestrator_error() {
            let key = error.user_message_key();
            assert!(key.starts_with("turnframe.error."), "{error:?} -> {key}");
            // A defect of the library or the domain is never sold to the user
            // as a language problem, and never quietly retried.
            if error.severity() == ErrorSeverity::Critical {
                assert!(!error.retryable(), "{error:?}");
            }
            let rendered = error.to_string();
            assert!(!rendered.is_empty());
            let json = serde_json::to_value(&error).unwrap();
            assert_eq!(
                serde_json::from_value::<OrchestratorError>(json).unwrap(),
                error
            );
        }
    }

    #[test]
    fn a_structurally_broken_plan_is_a_defect_not_a_language_problem() {
        let broken = OrchestratorError::Reduction(ReductionError::InconsistentPlan {
            detail: "dangling command reference".into(),
        });
        assert_eq!(broken.severity(), ErrorSeverity::Critical);
        assert!(!broken.retryable());
    }

    #[test]
    fn hashing_failures_reach_the_orchestrator_error() {
        // A map with non-string keys cannot be canonical JSON.
        let unserializable: std::collections::BTreeMap<(u8, u8), u8> =
            [((1, 2), 3)].into_iter().collect();
        let err: OrchestratorError = crate::hash::canonical_digest(&unserializable)
            .unwrap_err()
            .into();
        assert_eq!(
            err,
            OrchestratorError::Internal {
                code: internal_code::HASH.to_owned()
            }
        );
        assert_eq!(err.severity(), ErrorSeverity::Critical);
        assert!(!err.retryable());
        assert_eq!(err.user_message_key(), "turnframe.error.internal");
    }

    #[test]
    fn external_unknown_is_classified_for_reconciliation() {
        let err = OrchestratorError::ExternalOutcomeUnknown(UnknownOutcome {
            attempt_id: "a1".into(),
            remote_ref: None,
            reason: "timeout".into(),
        });
        assert!(!err.retryable());
        assert!(err.effect_may_have_happened());
        assert!(err.reconciliation_required());
        assert_eq!(
            err.user_message_key(),
            "turnframe.error.verification_in_progress"
        );
    }

    #[test]
    fn display_carries_ids_only() {
        let err = OrchestratorError::Reduction(ReductionError::CaseNotLoaded {
            act: ActId::new(crate::understanding::UnitId(2), 1),
        });
        assert_eq!(
            err.to_string(),
            "act u2.a1 references a case that is not loaded"
        );
    }

    #[test]
    fn revision_conflict_is_retryable_without_effect() {
        let err = OrchestratorError::RevisionConflict(RevisionConflict {
            expected: CaseRef::new("trip", "i1", CaseRevision(3)),
            current_revision: CaseRevision(4),
        });
        assert!(err.retryable());
        assert!(!err.effect_may_have_happened());
        assert_eq!(err.severity(), ErrorSeverity::Warning);
    }
}

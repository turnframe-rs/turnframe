//! Flow Map V2: the deterministic workflow projector (spec §8).
//!
//! A [`WorkflowDefinition`] projects persisted state into a [`WorkflowView`]:
//! exactly one lifecycle phase, zero or more parameterized obligations, at most
//! one blocking [`InteractionRequirement`], informational notices and an
//! outcome that is present only when the workflow is complete. Projection is
//! pure (I2): same version + same state ⇒ same view, no I/O.
//!
//! Execution lives in a separate [`WorkflowExecutor`] because applications
//! mutate SQL rows, call services or fold event-sourced aggregates.
//!
//! [`registry`] erases the generics at the boundary so the runtime can host
//! several workflows without knowing their concrete types; [`invariants`]
//! checks the §8.4 rules on any view.

pub mod invariants;
pub mod registry;

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::case::{CaseRef, Versioned};
use crate::command::RiskClass;
use crate::error::{DomainRejection, ExecutionError, HashError, StoreError};
use crate::ids::{AccountId, CaseId, WorkflowKey, WorkflowVersion};
use crate::interaction::{
    InteractionKind, InteractionPayload, InteractionSpec, TextResolutionPolicy,
};
use crate::locale::{Locale, LocalizedText};
use crate::operation::{GlossaryTerm, OperationSpec};
use crate::response::NoticeSeverity;
use crate::target::ResolvedAct;

pub use crate::command::{CommandBatch, CommandPolicy};
pub use crate::event::{
    Commit, CommittedEvent, EventRedaction, OperationalReceipt, ReceiptEvent, RedactedEvent,
};
pub use invariants::{check_erased_view, check_view};
pub use registry::{
    CaseLoaderHandle, ErasedCaseLoader, ErasedExecutor, ErasedObligation, ErasedWorkflow,
    ErasedWorkflowView, RegisteredWorkflow, TypedWorkflowAdapter, WorkflowDefinitions,
    WorkflowReadRegistry, WorkflowRegistry, WorkflowRegistryBuilder,
};

/// Who must act for the case to leave its current phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PhaseOwnership {
    /// The user must answer a blocking interaction (I6).
    User,
    /// The system acts (a job, a policy).
    System,
    /// An external party acts (an airline, an intermediary, a recipient).
    External,
    /// Nothing more happens; the outcome is present.
    Terminal,
}

/// Stable identifier of an obligation: the canonical JSON of its value.
///
/// Two obligations with the same identifier in one view are a map defect
/// (spec §8.4). Parameterized obligations therefore carry their entity ids.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ObligationId(pub String);

impl ObligationId {
    /// Derives the identifier of an obligation.
    pub fn of<O: Serialize + ?Sized>(obligation: &O) -> Result<Self, HashError> {
        crate::hash::canonical_json(obligation).map(Self)
    }

    /// Borrows the canonical JSON.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ObligationId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// An informational, non-blocking element of a view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowNotice {
    /// Stable code (e.g. `"trip.traveler_missing_email"`).
    pub code: String,
    /// Severity.
    pub severity: NoticeSeverity,
    /// Copy.
    pub text: LocalizedText,
}

/// What a user-owned phase requires from the user (I6).
///
/// The requirement is data; the workflow turns it into a full
/// [`InteractionSpec`] through [`WorkflowDefinition::build_interaction`], where
/// it can consult state. When the projection can already describe the whole
/// card, it may set `payload` and rely on the default implementation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractionRequirement {
    /// Stable key per phase (e.g. `"send_confirmation"`).
    pub key: String,
    /// Shape of the interaction.
    pub kind: InteractionKind,
    /// Whether it owns unqualified answers for the case (I5).
    pub blocking: bool,
    /// Whether a revision change leaves it valid.
    pub revision_independent: bool,
    /// Whether typed text may resolve it.
    pub text_resolution: TextResolutionPolicy,
    /// Highest risk class an answer to this card authorizes. Conservative by
    /// default, so a requirement that forgets it cannot be resolved from text.
    #[serde(default = "RiskClass::conservative")]
    pub confirms_risk: RiskClass,
    /// Time to live.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_in: Option<Duration>,
    /// Full payload when the projection can describe it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<InteractionPayload>,
}

impl InteractionRequirement {
    /// A blocking, revision-bound requirement with `Never` text resolution.
    #[must_use]
    pub fn blocking(key: impl Into<String>, kind: InteractionKind) -> Self {
        Self {
            key: key.into(),
            kind,
            blocking: true,
            revision_independent: false,
            text_resolution: TextResolutionPolicy::Never,
            confirms_risk: RiskClass::conservative(),
            expires_in: None,
            payload: None,
        }
    }

    /// A non-blocking requirement: a card the case offers without owning the
    /// answers to everything else.
    ///
    /// The flag existed and could not be set, which made every declared card
    /// blocking whether or not the question wanted one. A blocking card owns
    /// unqualified answers for the whole case (I5), so putting one beside a
    /// question whose ordinary answer is a value leaves the user reading
    /// buttons that cannot say what they came to say.
    ///
    /// The other half of that problem is answered by
    /// the claim the act declares, which lets the refusal be
    /// spoken instead of pressed. This is the smaller lever, kept because a
    /// field nobody can write is not a field.
    #[must_use]
    pub fn non_blocking(key: impl Into<String>, kind: InteractionKind) -> Self {
        Self {
            blocking: false,
            ..Self::blocking(key, kind)
        }
    }

    /// Declares the highest risk class an answer authorizes. Lowering it is
    /// what makes a card resolvable from typed text (spec §15.7).
    #[must_use]
    pub fn with_confirms_risk(mut self, risk: RiskClass) -> Self {
        self.confirms_risk = risk;
        self
    }

    /// Attaches a full payload.
    #[must_use]
    pub fn with_payload(mut self, payload: InteractionPayload) -> Self {
        self.payload = Some(payload);
        self
    }

    /// Sets the text resolution policy.
    #[must_use]
    pub fn with_text_resolution(mut self, policy: TextResolutionPolicy) -> Self {
        self.text_resolution = policy;
        self
    }

    /// Builds a spec for `case_ref`, using `payload` or a title-only payload
    /// named after the key.
    ///
    /// A title-only payload is answerable for no kind, so a requirement without
    /// a payload must be completed by
    /// [`WorkflowDefinition::build_interaction`]; the spec is validated there.
    #[must_use]
    pub fn to_spec(&self, case_ref: CaseRef) -> InteractionSpec {
        let payload = self
            .payload
            .clone()
            .unwrap_or_else(|| InteractionPayload::new(self.key.clone()));
        InteractionSpec {
            key: self.key.clone(),
            case_ref,
            kind: self.kind,
            blocking: self.blocking,
            payload,
            expires_in: self.expires_in,
            text_resolution: self.text_resolution.clone(),
            confirms_risk: self.confirms_risk,
            binds_to_revision: !self.revision_independent,
        }
    }
}

/// The pure projection of a case (spec §8.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowView<P, O, T> {
    /// The case and the revision it was projected at.
    pub case_ref: CaseRef,
    /// Version of the definition that produced the view.
    pub workflow_version: WorkflowVersion,
    /// Exactly one lifecycle phase (I3).
    pub phase: P,
    /// Every currently open obligation, possibly parameterized (I4), **in the
    /// order the workflow wants them asked for**.
    ///
    /// A set is the truth about what is open and is what an index wants; it
    /// cannot say which one is being asked. Told to pick "the most useful one"
    /// a writer picks, and a collection flow was asked for its bank account
    /// first and its beneficiary second — the last question of the flow — and
    /// then read all six out as a menu. So the list is ordered by declaration
    /// and the writer is told to ask for the first.
    ///
    /// Ordering rather than a single "current" obligation, because some
    /// domains genuinely have several open at once — a proposal reviewed in one
    /// answer, three extras with no payer that one message can close two
    /// of — and a shape that forced exactly one would make those unsayable.
    pub obligations: Vec<O>,
    /// Zero or one blocking interaction requirement (I5, I6).
    #[serde(default = "Option::default", skip_serializing_if = "Option::is_none")]
    pub blocking_interaction: Option<InteractionRequirement>,
    /// Informational, non-blocking elements.
    #[serde(default = "Vec::new")]
    pub notices: Vec<WorkflowNotice>,
    /// Present only when the workflow is complete.
    #[serde(default = "Option::default", skip_serializing_if = "Option::is_none")]
    pub outcome: Option<T>,
}

impl<P, O, T> WorkflowView<P, O, T> {
    /// A view with a phase and nothing else.
    #[must_use]
    pub fn new(case_ref: CaseRef, workflow_version: WorkflowVersion, phase: P) -> Self {
        Self {
            case_ref,
            workflow_version,
            phase,
            obligations: Vec::new(),
            blocking_interaction: None,
            notices: Vec::new(),
            outcome: None,
        }
    }

    /// Adds obligations.
    #[must_use]
    pub fn with_obligations(mut self, obligations: impl IntoIterator<Item = O>) -> Self {
        self.obligations.extend(obligations);
        self
    }

    /// Sets the blocking requirement.
    #[must_use]
    pub fn with_blocking_interaction(mut self, requirement: InteractionRequirement) -> Self {
        self.blocking_interaction = Some(requirement);
        self
    }

    /// Adds a notice.
    #[must_use]
    pub fn with_notice(mut self, notice: WorkflowNotice) -> Self {
        self.notices.push(notice);
        self
    }

    /// Sets the outcome.
    #[must_use]
    pub fn with_outcome(mut self, outcome: T) -> Self {
        self.outcome = Some(outcome);
        self
    }

    /// Returns `true` when an outcome is present.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.outcome.is_some()
    }

    /// Returns `true` when obligations remain.
    #[must_use]
    pub fn has_obligations(&self) -> bool {
        !self.obligations.is_empty()
    }
}

impl<P: Serialize, O: Serialize, T: Serialize> WorkflowView<P, O, T> {
    /// Erases the generics into canonical JSON, attaching the phase ownership
    /// the definition declares.
    pub fn erase(&self, ownership: PhaseOwnership) -> Result<ErasedWorkflowView, HashError> {
        let mut obligations = Vec::with_capacity(self.obligations.len());
        for obligation in &self.obligations {
            obligations.push(ErasedObligation {
                id: ObligationId::of(obligation)?,
                value: crate::hash::canonical_value(obligation)?,
                // Filled by the registry, which holds the definition: see
                // `ErasedObligation::sentence`.
                sentence: None,
                act: None,
            });
        }
        Ok(ErasedWorkflowView {
            case_ref: self.case_ref.clone(),
            workflow_version: self.workflow_version.clone(),
            phase: crate::hash::canonical_value(&self.phase)?,
            phase_ownership: ownership,
            obligations,
            blocking_interaction: self.blocking_interaction.clone(),
            notices: self.notices.clone(),
            outcome: self
                .outcome
                .as_ref()
                .map(crate::hash::canonical_value)
                .transpose()?,
            // Filled by the registry, which is the layer that still holds the
            // state: erasing a view has already dropped it.
            state: Vec::new(),
        })
    }
}

/// One value a case holds, as the stage that ANSWERS may state it.
///
/// # Why a workflow declares this and the runtime does not read it
///
/// The composer knows what every case still NEEDS — obligations travel on the
/// view and reach the writer as facts — and nothing at all about what it
/// already HAS. So a question the user asks about their own record («what did
/// you save as the company name?») arrives at the answering stage with the list
/// of missing fields, the sources, and no answer in it. What comes back is
/// «nothing is set», in good faith, over a record that holds the value.
///
/// The state itself cannot be handed over wholesale: it is the workflow's own
/// type, it may carry values a person must not be told back, and a JSON dump is
/// not a fact. So the workflow says which of its values are sayable and under
/// which name, and the runtime turns each into a
/// [`NarratableFact::StateValue`](crate::response::NarratableFact::StateValue)
/// — the variant that has existed for exactly this and that nothing filled.
///
/// Returning nothing, the default, keeps the previous behaviour.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateField {
    /// The path the workflow names it with.
    pub field: String,
    /// The value, as the writer may state it.
    pub value: serde_json::Value,
    /// Whether it tells this record apart from others of its workflow, such as a
    /// number or a counterpart's name. Identifying fields are what a record is shown
    /// by when the model has to choose among several.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub identifying: bool,
}

impl StateField {
    /// A field named `field` holding `value`.
    #[must_use]
    pub fn new(field: impl Into<String>, value: serde_json::Value) -> Self {
        Self {
            field: field.into(),
            value,
            identifying: false,
        }
    }

    /// Marks the field as telling this record apart from others.
    #[must_use]
    pub const fn identifying(mut self) -> Self {
        self.identifying = true;
        self
    }
}

/// Shorthand for the view type of a definition.
pub type ViewOf<W> = WorkflowView<
    <W as WorkflowDefinition>::Phase,
    <W as WorkflowDefinition>::Obligation,
    <W as WorkflowDefinition>::Outcome,
>;

/// What must already be true of **another** case before this workflow may be
/// started.
///
/// # Why a declaration
///
/// A projector is pure and cannot read another case, and the executor is the wrong place
/// for a domain rule about when a case may exist: «a traveler is registered only while a
/// trip is open» belongs to neither. The workflow declares what it needs; the runtime,
/// which has the other cases in hand already, decides whether it is there.
///
/// # What it costs when it is not met
///
/// The start act is not offered at all, so it cannot be proposed and cannot be
/// refused later. The [`reason`](Self::reason) travels in the catalogue instead,
/// which is the honest limit of this shape: with no act there is no refusal to
/// attach a notice to, so whether the user hears *why* depends on the sentence
/// the writing stage produces. A deployment that needs the reason guaranteed
/// should keep an operation that refuses it rather than one that is absent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartPrecondition {
    /// The workflow the required case belongs to.
    pub workflow: WorkflowKey,
    /// The phases that satisfy it, as that workflow's phase serializes.
    pub phases: Vec<serde_json::Value>,
    /// Why, in words a user can read, when it is not met.
    pub reason: LocalizedText,
}

impl StartPrecondition {
    /// Requires a case of `workflow` to be in one of `phases`, already
    /// serialized.
    ///
    /// The infallible primitive. Prefer [`Self::requires`], which takes the
    /// other workflow's own phase type so the compiler checks the spelling.
    #[must_use]
    pub fn new(
        workflow: impl Into<WorkflowKey>,
        phases: impl IntoIterator<Item = serde_json::Value>,
        reason: LocalizedText,
    ) -> Self {
        Self {
            workflow: workflow.into(),
            phases: phases.into_iter().collect(),
            reason,
        }
    }

    /// Requires a case of `workflow` to be in one of `phases`.
    ///
    /// The phases are serialized here, so an adopter names them with the other
    /// workflow's own type and the compiler checks the spelling.
    ///
    /// # Errors
    ///
    /// [`HashError`] when a phase does not serialize, which is the workflow's
    /// own type failing to round-trip.
    pub fn requires<P: Serialize>(
        workflow: impl Into<WorkflowKey>,
        phases: &[P],
        reason: LocalizedText,
    ) -> Result<Self, HashError> {
        let mut serialized = Vec::with_capacity(phases.len());
        for phase in phases {
            serialized.push(crate::hash::canonical_value(phase)?);
        }
        Ok(Self {
            workflow: workflow.into(),
            phases: serialized,
            reason,
        })
    }

    /// Whether `view` is a case that satisfies this precondition.
    #[must_use]
    pub fn satisfied_by(&self, view: &ErasedWorkflowView) -> bool {
        view.case_ref.workflow == self.workflow && self.phases.contains(&view.phase)
    }
}

/// What starting this workflow means when the account already has a case of it.
///
/// `StartWorkflow` is the runtime's own door: no catalogue entry, no target. What
/// *start* means differs by workflow: a second trip is ordinary, a second profile of the
/// same traveler is not, and only the domain knows which.
///
/// Under [`ResumesOpenCase`](Self::ResumesOpenCase) the act reaches the case
/// the turn can already see. Seeing one it resolves to it, so `compile_act`
/// will be asked to start an open case and the honest answers are no commands
/// or a rejection; seeing several the act is refused, because the door carries
/// no target and a selection card would come back with the same question;
/// seeing none it mints as before.
///
/// The limit is the directory's: a case the turn did not load cannot be seen. An
/// operation aimed at a new record is governed by
/// [`WorkflowDefinition::may_open_beside`] instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StartBehaviour {
    /// Every start opens a new record.
    ///
    /// The default, and what every workflow did before this existed.
    #[default]
    OpensNewCase,
    /// A start reaches the case that is already open, when the turn sees one.
    ResumesOpenCase,
}

impl StartBehaviour {
    /// Whether a start should reach a case the turn already has.
    #[must_use]
    pub const fn resumes(self) -> bool {
        matches!(self, Self::ResumesOpenCase)
    }
}

/// What a confirmation the **policy engine** raised is about, in the domain's
/// own words.
///
/// # The card nobody could write
///
/// An operation whose confirmation policy demands a click gets a card from the
/// policy engine, built from `ConfirmationCopy`: a title and two labels, all
/// per-*kind*. So every act in a deployment that needs a click and declares no
/// card of its own draws the same box. Asked to delete a draft, a user got
/// "Confermi?" over Cancel and Confirm, with nothing anywhere naming what was
/// about to be deleted.
///
/// It is the right card for the policy — a destructive act must be a click, and
/// it was — and the wrong card for a person, who is being asked to confirm
/// something the card does not name.
///
/// A workflow can already describe a card its **projection** declares, through
/// `build_interaction`. That door is shut for a confirmation the engine raises,
/// because there is no requirement to hang it on. This is the same door on that
/// path: the engine is the only layer that knows a confirmation is *needed*, and
/// the domain is the only one that knows what it is *about*.
///
/// Returning `None` keeps the generic box, so a workflow that says nothing sees
/// no change.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ConfirmationSubject {
    /// Replaces the per-kind title, when the domain has a better question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<LocalizedText>,
    /// Sets the body, which the generic card has none of.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<LocalizedText>,
}

impl ConfirmationSubject {
    /// A confirmation that asks its own question.
    #[must_use]
    pub fn asking(title: LocalizedText) -> Self {
        Self {
            title: Some(title),
            body: None,
        }
    }

    /// A confirmation that keeps the generic question and explains underneath.
    #[must_use]
    pub fn describing(body: LocalizedText) -> Self {
        Self {
            title: None,
            body: Some(body),
        }
    }

    /// Adds a body to a question.
    #[must_use]
    pub fn with_body(mut self, body: LocalizedText) -> Self {
        self.body = Some(body);
        self
    }
}

/// Which of the two writing stages a briefing is addressed to.
///
/// They are told apart because they must be. The transition acknowledges what
/// happened and asks for what is still open; the answer stage answers what the
/// user asked. A string written for one of them and delivered to both is how
/// the runtime's own care — keeping every question away from the transition —
/// was undone by an adopter sentence it had no way to notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WritingStage {
    /// The acknowledgement, which is also the stage that asks.
    Transition,
    /// The block that answers one question the user asked.
    Answer,
}

/// One value a field accepts, as the workflow names it to a user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnumeratedValue {
    /// Stable identifier, as the domain stores it.
    pub id: String,
    /// What a person calls it, in the languages the workflow answers in.
    pub label: LocalizedText,
}

impl EnumeratedValue {
    /// A value with an id and a label.
    #[must_use]
    pub fn new(id: impl Into<String>, label: LocalizedText) -> Self {
        Self {
            id: id.into(),
            label,
        }
    }
}

crate::ids::string_id! {
    /// A field or concept a question may be about, as a workflow names it:
    /// `proposed.company.registered_address`.
    QuestionReference
}

/// Every value one subject accepts, and nothing else.
///
/// Declared per view, so a workflow whose accepted values depend on the phase
/// or on the record declares what is true now rather than what is true in
/// general. See
/// [`WorkflowDefinition::enumerations`] for what the runtime does with it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainEnumeration {
    /// The field or concept, in the vocabulary a plan's questions use.
    pub subject: QuestionReference,
    /// The complete set. A partial one would be worse than none: it would make
    /// the runtime state as exhaustive a list that is not.
    pub values: Vec<EnumeratedValue>,
    /// A sentence to put before the values, in the workflow's own words.
    ///
    /// Optional, and server-authored like a receipt's body. The runtime writes
    /// no sentence of its own here, because a sentence about a domain is the
    /// domain's to write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preamble: Option<LocalizedText>,
}

impl DomainEnumeration {
    /// A subject and the values it accepts.
    #[must_use]
    pub fn new(subject: impl Into<QuestionReference>, values: Vec<EnumeratedValue>) -> Self {
        Self {
            subject: subject.into(),
            values,
            preamble: None,
        }
    }

    /// Adds the sentence that introduces the values.
    #[must_use]
    pub fn with_preamble(mut self, preamble: LocalizedText) -> Self {
        self.preamble = Some(preamble);
        self
    }
}

/// How much of a workflow's guidance reaches the model, per case.
///
/// # Why nothing is bounded by default
///
/// The right number depends on the context window of the model a deployment
/// runs and on what else that deployment puts in a turn. A workflow knows
/// neither, and neither does the library, so a shipped number would be a guess
/// made once on behalf of everybody — and it would be a guess that silently
/// deleted the end of a workflow's guidance for anyone whose situation it did
/// not fit. Nothing is bounded until a deployment says so, and there is no
/// ceiling on what it may say: a limit an adopter cannot raise is not a
/// configurable limit.
///
/// # Why cutting is right here and wrong elsewhere
///
/// This is the adopter's own text on its way *into* a prompt, not a model's
/// answer on its way out to a user. Shortening what a deployment wrote costs
/// the model some guidance and is visible in what was sent; shortening what a
/// model wrote hands a person half a sentence. And the cut is marked, so a
/// briefing that arrives as a prefix says so rather than reading as a complete
/// rule that happens to be shorter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct BriefingBudget {
    max_bytes: Option<usize>,
}

impl BriefingBudget {
    /// The default: no bound at all.
    #[must_use]
    pub const fn conservative() -> Self {
        Self { max_bytes: None }
    }

    /// A budget of `max_bytes`, taken as given.
    #[must_use]
    pub const fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes: Some(max_bytes),
        }
    }

    /// The bound in force, or `None` when the briefing is not bounded.
    #[must_use]
    pub const fn max_bytes(self) -> Option<usize> {
        self.max_bytes
    }

    /// Cuts `text` to the budget, marking the cut. Returns it unchanged when
    /// no budget is set, which is what ships.
    ///
    /// The marker matters more than the limit. A model shown a prefix with no
    /// sign that it is one will follow half a rule as though it were the whole
    /// rule, which is worse than never having been told the rule.
    #[must_use]
    pub fn apply(self, text: &str) -> String {
        let Some(max_bytes) = self.max_bytes else {
            return text.to_owned();
        };
        if text.len() <= max_bytes {
            return text.to_owned();
        }
        let mut end = max_bytes;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}… [briefing truncated]", &text[..end])
    }
}

/// A workflow: pure projection plus deterministic compilation and policy
/// (spec §8.2).
pub trait WorkflowDefinition: Send + Sync + 'static {
    /// Persisted case state.
    type State: Clone + Send + Sync + Serialize + serde::de::DeserializeOwned + 'static;
    /// Lifecycle phase.
    type Phase: Clone + Send + Sync + Serialize + serde::de::DeserializeOwned + Eq + 'static;
    /// Open obligation, possibly parameterized.
    type Obligation: Clone
        + Send
        + Sync
        + Serialize
        + serde::de::DeserializeOwned
        + Eq
        + std::hash::Hash
        + 'static;
    /// Typed command.
    type Command: Clone + Send + Sync + Serialize + serde::de::DeserializeOwned + 'static;
    /// Typed domain event.
    type Event: Clone + Send + Sync + Serialize + serde::de::DeserializeOwned + 'static;
    /// Terminal outcome.
    type Outcome: Clone + Send + Sync + Serialize + serde::de::DeserializeOwned + Eq + 'static;

    /// Stable key.
    fn key(&self) -> WorkflowKey;

    /// Version; must change when projection semantics change (spec §8.4).
    fn version(&self) -> WorkflowVersion;

    /// Who must act in a phase. Drives the §8.4 invariants.
    fn phase_ownership(&self, phase: &Self::Phase) -> PhaseOwnership;

    /// Pure projection (I2).
    ///
    /// An absent state means the case does not exist **yet**, and never that it
    /// no longer does: a case's identity outlives its content, so removal is a
    /// status the state carries and never an absence. Its one use is a case the
    /// application's case directory has offered but that has not been created,
    /// which is projected to a pre-draft phase and never to a terminal one. A
    /// projector that gives an absent state a terminal phase or an outcome is
    /// reported by the state explorer in the test kit.
    fn project(&self, case_ref: CaseRef, state: Option<&Self::State>) -> ViewOf<Self>;

    /// What this case HOLDS, for the stage that answers questions.
    ///
    /// The obligations on the view say what a case still needs; this says what
    /// it already has. Without it the answering stage is handed a list of
    /// missing fields and no values, and «what did you record as the company
    /// name?» comes back as «nothing», truthfully as far as the brief goes,
    /// over a record that holds one.
    ///
    /// Takes the state rather than the view for the same reason
    /// [`Self::compile_act`] does: the view is a projection and a projection
    /// drops the values.
    ///
    /// Declare only what a person may be told back. A value under an
    /// obligation is fine — it is theirs, they gave it — and a value the domain
    /// holds for its own bookkeeping is not.
    ///
    /// The default is empty, which is the previous behaviour.
    fn narratable_state(&self, state: Option<&Self::State>) -> Vec<StateField> {
        let _ = state;
        Vec::new()
    }

    /// One line saying what the workflow is for, shown when a message is split into
    /// requests. `None`, the default, shows the workflow's key alone.
    fn summary(&self) -> Option<String> {
        None
    }

    /// Terms this workflow's users say, and what they mean here.
    fn glossary(&self) -> Vec<GlossaryTerm> {
        Vec::new()
    }

    /// What one record of this workflow is called, in each language its users speak
    /// («traveler», «viaggiatore»), for the sentences the runtime writes about one. `None`,
    /// the default, uses the workflow's key.
    fn noun(&self) -> Option<crate::locale::LocalizedText> {
        None
    }

    /// The operations offered in this view, with their arguments, labels and examples.
    fn operations(&self, view: &ViewOf<Self>) -> Vec<OperationSpec>;

    /// Guidance for understanding a turn about a record in *this* view. `None`, the
    /// default, is ordinary.
    ///
    /// It is instructions, not data: never interpolate text a user wrote. It varies by
    /// view, not by record contents, which keeps it reviewable.
    fn briefing(&self, view: &ViewOf<Self>) -> Option<String> {
        let _ = view;
        None
    }

    /// One obligation, in words a person would recognise.
    ///
    /// The stage that writes is handed obligations as the domain's own serialized
    /// values, which a `match` needs and a sentence does not. Returning `None`, the
    /// default, lets the value travel as it is, and a structured one such as
    /// `{"fill":{"row":1}}` is then guessed at. Say it as the question it is, «row 1
    /// has no A: what is it?»: localized copy for a reader, saying what is missing
    /// rather than what to do about it.
    fn obligation_sentence(&self, obligation: &Self::Obligation) -> Option<LocalizedText> {
        let _ = obligation;
        None
    }

    /// The act that answers `obligation`, and the values it already knows.
    ///
    /// Asked «row 1 has no A: what is it?», the user answers «X»: the answer is A, and
    /// the row is the obligation's. With an act named here the reply's question carries
    /// it, so a bare answer completes it. `None`, the default, leaves the answer to be
    /// routed as any other message.
    fn obligation_act(
        &self,
        state: Option<&Self::State>,
        obligation: &Self::Obligation,
    ) -> Option<ObligationAct> {
        let _ = (state, obligation);
        None
    }

    /// What must already be true of another case before this workflow may be
    /// started.
    ///
    /// Every precondition must hold, or the workflow's "no case yet" operations
    /// are absent from the catalogue and the model cannot propose starting it.
    /// A declaration and not a read: see [`StartPrecondition`], which also says
    /// what this shape cannot guarantee.
    ///
    /// The default is empty, which requires nothing and changes nothing.
    fn start_preconditions(&self) -> Vec<StartPrecondition> {
        Vec::new()
    }

    /// What a confirmation the policy engine raises is about.
    ///
    /// Called when an act compiles to commands that policy says need a click,
    /// with the state and the act that produced them — which is everything
    /// needed to write "Delete the record for Mario Rossi?" where the engine
    /// would otherwise draw its per-kind box. See [`ConfirmationSubject`].
    ///
    /// The default is `None`, which keeps that box exactly as it was.
    fn confirmation_subject(
        &self,
        state: Option<&Self::State>,
        view: &ViewOf<Self>,
        act: &ResolvedAct,
    ) -> Option<ConfirmationSubject> {
        let _ = (state, view, act);
        None
    }

    /// What starting this workflow means when a case of it is already open.
    ///
    /// See [`StartBehaviour`], which carries the whole argument. The default
    /// mints a new case every time, which is what this door did before the
    /// declaration existed.
    fn start_behaviour(&self) -> StartBehaviour {
        StartBehaviour::OpensNewCase
    }

    /// Whether a new case may be opened while `open` ones are: an operation aimed at a
    /// new record, the door [`StartBehaviour`] does not govern.
    ///
    /// `open` holds every case of this workflow the turn can address, plus any it
    /// already minted, projected with no state. Only the domain knows whether a second
    /// one is reasonable (an unfinished draft beside another is not; one waiting to be
    /// sent may be), and neither `compile_act` nor `validate_command` sees the others.
    /// A refusal rejects the act with the domain's own sentence and the rest of the
    /// turn stands. The default admits everything.
    fn may_open_beside(&self, open: &[ViewOf<Self>]) -> Result<(), DomainRejection> {
        let _ = open;
        Ok(())
    }

    /// What this workflow wants said while it **acknowledges** the turn and
    /// asks for what is still open.
    ///
    /// # Why there are two of these and not one
    ///
    /// There was one, and it went to both writing stages. The runtime goes to
    /// real lengths to keep a question away from this stage — the questions are
    /// not in its brief and the words they are made of are cut out of the
    /// message it is shown — because a model handed a question answers it, and
    /// the user reads the same explanation twice. Then one adopter string
    /// reached both stages and put the instruction straight back, and the
    /// duplicate came out again.
    ///
    /// The guarantee has to be whole or it is not one. So a workflow addresses
    /// each stage by name: what to say while acknowledging is not what to say
    /// while answering, and a workflow with something for one and nothing for
    /// the other says exactly that by leaving the other at `None`.
    ///
    /// This is the stage that asks, so guidance about *what to ask for and in
    /// what order* belongs here.
    ///
    /// Like [`Self::briefing`] it takes the view and not the state, so the
    /// guidance varies exactly as much as the projection does and can never
    /// carry a user's words into a prompt. The composer sees the view projected
    /// **after** the turn committed, so what it is briefed about is the case as
    /// it now stands.
    fn transition_briefing(&self, view: &ViewOf<Self>) -> Option<String> {
        let _ = view;
        None
    }

    /// What this workflow wants said while it **answers** a question the user
    /// asked.
    ///
    /// The other half of [`Self::transition_briefing`], and deliberately a
    /// different string: guidance about how to explain a domain concept has no
    /// business reaching the stage whose job is to acknowledge and ask.
    fn answer_briefing(&self, view: &ViewOf<Self>) -> Option<String> {
        let _ = view;
        None
    }

    /// The complete sets of values this workflow accepts, for the fields where
    /// there is one.
    ///
    /// # The claim nothing was checking
    ///
    /// The claim guard verifies that prose does not say an action happened
    /// when it did not. "These are the values this field accepts" is not a
    /// claim about an action; it is a claim about the domain, it is exactly as
    /// harmful when false, and it went out unchecked. A workflow accepting two
    /// legal forms was asked which forms exist and answered with three, the
    /// third being a plausible name for nothing. A user who takes that advice
    /// types a value the domain will refuse.
    ///
    /// A firmer prompt does not fix it. A sentence naming three plausible
    /// things is what a language model produces when nothing decides how many
    /// there are, and the values already existed in the workflow — as prose in
    /// a briefing, which is to say as a suggestion.
    ///
    /// # What the runtime does with it
    ///
    /// A question whose references name an enumerated subject, and whose basis
    /// is general domain knowledge — "which forms are there", not "which one
    /// does this record have" — is answered from this declaration and no model
    /// is asked. The values reach the user as structured data carrying the
    /// workflow's own labels, so there is no sentence for a third value to
    /// appear in. Guarding prose after the fact was the alternative, and it
    /// cannot be done: reading an answer cannot tell an invented value from a
    /// real one, which is why the declaration answers instead of checking.
    ///
    /// It also tells the runtime that such a question *is* answerable, which
    /// is what stops it being dropped as the assistant's own next step: the
    /// field a flow is collecting is the field a user asks about, and without
    /// a declared answer the two are indistinguishable.
    ///
    /// The default is empty, which declares nothing and changes nothing.
    fn enumerations(&self, view: &ViewOf<Self>) -> Vec<DomainEnumeration> {
        let _ = view;
        Vec::new()
    }

    /// What the user may do next once the case owes nothing, each a sentence in the
    /// workflow's words: the reply offers them when the case needs nothing more.
    ///
    /// The default is empty, which offers nothing.
    fn next_steps(&self, view: &ViewOf<Self>) -> Vec<crate::locale::LocalizedText> {
        let _ = view;
        Vec::new()
    }

    /// The documents this case has, for the turn to put in front of the user.
    ///
    /// # The type that nothing could fill
    ///
    /// [`ArtifactRef`](crate::event::ArtifactRef),
    /// [`ArtifactView`](crate::response::ArtifactView) and
    /// [`OperationalReceipt::artifact_refs`](crate::event::OperationalReceipt::artifact_refs)
    /// all existed, and no workflow could fill any of them. The only hook that
    /// came close is [`Self::receipts`], which is handed the events and nothing
    /// else — and whether a case has a document is a question about the state
    /// those events folded into, not about the events. A list of "line added"
    /// cannot answer it, so every implementation ended at an empty vector.
    ///
    /// What that cost is concrete: a user was asked to authorise the
    /// irreversible transmission of a document they had never seen, because the
    /// preview that used to sit in the conversation had nowhere to come from.
    ///
    /// # Why the view and not the receipt
    ///
    /// A receipt exists only where something committed. A document does not stop
    /// existing on a turn that writes nothing — the user asks a question, or
    /// refuses a card and the requirement stays down — and on those turns there
    /// is no receipt to hang it on. An artifact outlives the turn that produced
    /// it, so it is declared from the projection, like everything else that is
    /// true of a case rather than of a moment.
    ///
    /// # What the runtime does with it
    ///
    /// One [`ResponseBlock::Artifact`](crate::response::ResponseBlock::Artifact)
    /// per declaration, for the cases **the turn was about**. A case the turn
    /// merely loaded does not put a document in the reply, for the same reason
    /// its briefing does not: a conversation about one record is not an occasion
    /// to show another.
    ///
    /// Re-declaring the same artifact after an edit is not a duplicate and is
    /// not suppressed. It is the same document at a later revision, the turn
    /// order says which is which, and what a surface does with the earlier ones
    /// is a rendering decision the runtime has no business taking.
    ///
    /// The default is empty, which declares nothing and changes nothing.
    fn artifacts(&self, view: &ViewOf<Self>) -> Vec<crate::event::ArtifactRef> {
        let _ = view;
        Vec::new()
    }

    /// Compiles a resolved act into typed commands (spec §21.2).
    fn compile_act(
        &self,
        state: Option<&Self::State>,
        view: &ViewOf<Self>,
        act: &ResolvedAct,
    ) -> Result<Vec<Self::Command>, DomainRejection>;

    /// Why an act that compiled nothing changed nothing, in the reader's own
    /// words.
    ///
    /// Called only when [`compile_act`](Self::compile_act) returned no commands
    /// at all. That is a legitimate answer — the state the act asks for is the
    /// state the case is already in — but the runtime cannot say which of a
    /// workflow's several reasons it was, and the writing stage is handed the
    /// operation's name and nothing else.
    ///
    /// # What that costs when nobody implements it
    ///
    /// One workflow compiles nothing for two quite different reasons: the
    /// singleton whose start was proposed a second time, and the write that
    /// tells a field what it already says. Given only «this act changed
    /// nothing», a writer invents a reason, and the one it reaches for is a
    /// refusal: asked to correct a value and told nothing changed, it answered
    /// «I cannot do that here» — which was not true, and left the user with no
    /// idea what to say next. The sentence a person needed was «that is already
    /// the value; tell me what you want instead», and only the workflow knows
    /// it.
    ///
    /// This is the same channel [`DomainRejection`] gives a refusal, for the
    /// same reason: a workflow that wrote a sentence knows more than the
    /// runtime does.
    ///
    /// # The default
    ///
    /// `None`, which is exactly what every workflow said before this existed —
    /// the fact reaches the writing stage naming the operation and no more.
    fn nothing_changed(
        &self,
        state: Option<&Self::State>,
        act: &ResolvedAct,
    ) -> Option<LocalizedText> {
        let _ = (state, act);
        None
    }

    /// Policy of a command. Unknown commands must default to
    /// [`CommandPolicy::conservative`].
    fn command_policy(&self, state: Option<&Self::State>, command: &Self::Command)
    -> CommandPolicy;

    /// Deterministic validation before execution.
    fn validate_command(
        &self,
        state: Option<&Self::State>,
        command: &Self::Command,
    ) -> Result<(), DomainRejection>;

    /// Renders receipts from committed events (spec §17.3).
    ///
    /// The events arrive as [`ReceiptEvent`]s, not bare payloads, because a
    /// receipt must cite the [`EventId`](crate::ids::EventId)s that authorize
    /// its claim (I16): a `Success` receipt with no event ids is refused by
    /// [`claim_guard::verify`](crate::response::claim_guard::verify). Derive the
    /// receipt id with
    /// [`ReceiptId::derive`](crate::ids::ReceiptId::derive) so a replayed turn
    /// renders the same receipts.
    ///
    /// # The redacted case
    ///
    /// [`ReceiptEvent::Redacted`] means the event happened and its payload was
    /// erased (see the [`event`](crate::event) module). The event is still in
    /// the ledger, at its position, with its identity and its type, so a
    /// receipt rendered over it is still backed and still passes the claim
    /// guard — but it cannot say what changed, and it must not read as though
    /// it could. Write copy that is true of an erased event: that this step is
    /// on record and its detail is gone. Rendering nothing at all is worse than
    /// it looks, because a turn that quietly drops a receipt reads as a turn in
    /// which nothing happened.
    fn receipts(
        &self,
        events: &[ReceiptEvent<Self::Event>],
        locale: &Locale,
    ) -> Vec<OperationalReceipt>;

    /// Turns a requirement of the view into a full interaction spec.
    ///
    /// The default uses the requirement's payload, so a requirement that
    /// carries none must be completed here: the engine validates the result
    /// ([`InteractionSpec::validate`]) and refuses a card nobody could answer.
    fn build_interaction(
        &self,
        state: Option<&Self::State>,
        view: &ViewOf<Self>,
        requirement: &InteractionRequirement,
    ) -> Result<InteractionSpec, DomainRejection> {
        let _ = state;
        Ok(requirement.to_spec(view.case_ref.clone()))
    }
}

/// Loads and mutates cases of one workflow (spec §8.2).
#[async_trait::async_trait]
pub trait WorkflowExecutor<W: WorkflowDefinition>: Send + Sync {
    /// Loads a case for an account. `None` value means the case does not exist;
    /// its revision is then [`crate::ids::CaseRevision::ZERO`]. An executor
    /// must never delete a case to express completion, because a completed case
    /// keeps its row and moves to a terminal status, so an absent state always
    /// means the case has not been created yet.
    async fn load(
        &self,
        account: &AccountId,
        case_id: &CaseId,
    ) -> Result<Versioned<Option<W::State>>, StoreError>;

    /// Executes a batch under its atomicity scope with revision and
    /// idempotency checks (I13, I14).
    async fn execute(
        &self,
        batch: CommandBatch<W::Command>,
    ) -> Result<Commit<W::State, W::Event>, ExecutionError>;
}

/// The read-only half of [`WorkflowExecutor`]: loading a case, and nothing
/// else (spec §8.2).
///
/// It exists for callers that must be unable to mutate a case — the plan-only
/// turn path of `turnframe-runtime` above all, which needs the state a turn is
/// planned against and must not be able to execute a batch. A guarantee that
/// says "this code simply never calls `execute`" is not a guarantee; being
/// handed a value that has no `execute` is.
///
/// **There is nothing to implement.** Every [`WorkflowExecutor`] is a
/// `CaseLoader` through the blanket implementation below, so an adopter writes
/// exactly what they write today. The method is called `load_case` rather than
/// `load` so that a type which is both never makes a call site ambiguous.
///
/// ```rust
/// # use turnframe_core::flow::{CaseLoader, WorkflowDefinition, WorkflowExecutor};
/// /// Accepts any executor, but can only read through it.
/// fn planning_only<W: WorkflowDefinition, E: WorkflowExecutor<W>>(executor: E) -> impl CaseLoader<W> {
///     executor
/// }
/// ```
#[async_trait::async_trait]
pub trait CaseLoader<W: WorkflowDefinition>: Send + Sync {
    /// Loads a case for an account. `None` value means the case does not exist;
    /// its revision is then [`crate::ids::CaseRevision::ZERO`]. See
    /// [`WorkflowExecutor::load`] for what an absent state does and does not
    /// mean.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the case could not be read.
    async fn load_case(
        &self,
        account: &AccountId,
        case_id: &CaseId,
    ) -> Result<Versioned<Option<W::State>>, StoreError>;
}

/// Every executor loads.
#[async_trait::async_trait]
impl<W, E> CaseLoader<W> for E
where
    W: WorkflowDefinition,
    E: WorkflowExecutor<W> + ?Sized,
{
    async fn load_case(
        &self,
        account: &AccountId,
        case_id: &CaseId,
    ) -> Result<Versioned<Option<W::State>>, StoreError> {
        self.load(account, case_id).await
    }
}

/// A shared executor is an executor.
///
/// [`WorkflowRegistryBuilder::register`] takes the executor by value, so an
/// application that also holds its own handle on it — to seed a case, to read a
/// revision back, to share one connection pool between two workflows — would
/// otherwise have to wrap the `Arc` in a newtype just to re-implement two
/// forwarding methods. This impl is that newtype, written once.
#[async_trait::async_trait]
impl<W, E> WorkflowExecutor<W> for std::sync::Arc<E>
where
    W: WorkflowDefinition,
    E: WorkflowExecutor<W> + ?Sized,
{
    async fn load(
        &self,
        account: &AccountId,
        case_id: &CaseId,
    ) -> Result<Versioned<Option<W::State>>, StoreError> {
        (**self).load(account, case_id).await
    }

    async fn execute(
        &self,
        batch: CommandBatch<W::Command>,
    ) -> Result<Commit<W::State, W::Event>, ExecutionError> {
        (**self).execute(batch).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ids::CaseRevision;

    #[test]
    fn requirement_to_spec_defaults() {
        let req = InteractionRequirement::blocking("send", InteractionKind::ConfirmCommand);
        let spec = req.to_spec(CaseRef::new("trip", "i1", CaseRevision(1)));
        assert_eq!(spec.key, "send");
        assert!(spec.blocking);
        assert!(spec.binds_to_revision);
        assert_eq!(spec.payload.title.default, "send");
    }

    #[test]
    fn erase_produces_stable_obligation_ids() {
        #[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
        enum Ob {
            Line { id: u32 },
        }
        let view: WorkflowView<&str, Ob, ()> = WorkflowView::new(
            CaseRef::new("w", "c", CaseRevision(1)),
            WorkflowVersion::from("1"),
            "collecting",
        )
        .with_obligations([Ob::Line { id: 2 }, Ob::Line { id: 1 }]);
        let erased = view.erase(PhaseOwnership::System).unwrap();
        assert_eq!(erased.obligations[0].id.as_str(), r#"{"Line":{"id":2}}"#);
        assert_eq!(erased.phase, serde_json::json!("collecting"));
        assert!(erased.outcome.is_none());
    }
}

/// The act that answers an obligation: its operation, the values the obligation fixes,
/// and the values the answer gives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ObligationAct {
    /// The operation.
    pub operation: crate::ids::OperationKey,
    /// The arguments the obligation fixes, by name.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub given: std::collections::BTreeMap<String, serde_json::Value>,
    /// The arguments the answer gives.
    pub asks: Vec<String>,
}

impl ObligationAct {
    /// An act of `operation` asking for `asks`, with nothing fixed yet.
    #[must_use]
    pub fn new(
        operation: impl Into<crate::ids::OperationKey>,
        asks: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            operation: operation.into(),
            given: std::collections::BTreeMap::new(),
            asks: asks.into_iter().map(Into::into).collect(),
        }
    }

    /// Fixes argument `name` to `value`.
    #[must_use]
    pub fn given(mut self, name: impl Into<String>, value: serde_json::Value) -> Self {
        self.given.insert(name.into(), value);
        self
    }
}

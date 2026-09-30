//! Type erasure and the workflow registry (spec §8.3).
//!
//! Domain authors implement the typed [`WorkflowDefinition`] and
//! [`WorkflowExecutor`]. The runtime talks to [`ErasedWorkflow`] and
//! [`ErasedExecutor`], where state, commands and events are `serde_json::Value`.
//! [`TypedWorkflowAdapter`] is the only place where the conversion happens:
//! it deserializes at entry, calls the typed code, and serializes at exit.
//! Because projection is pure, erased methods that need a view take the state
//! and re-project instead of trusting a caller-supplied view.

use std::fmt;
use std::sync::Arc;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::case::{CaseRef, Versioned};
use crate::command::{CommandBatch, CommandPolicy};
use crate::error::{ErasedCallError, ErasureError, ExecutionError, StoreError};
use crate::event::{ArtifactRef, Commit, CommittedEvent, OperationalReceipt, ReceiptEvent};
use crate::flow::{
    ConfirmationSubject, DomainEnumeration, InteractionRequirement, ObligationId, PhaseOwnership,
    StartBehaviour, StartPrecondition, ViewOf, WorkflowDefinition, WorkflowExecutor,
    WorkflowNotice, WorkflowView, WritingStage,
};
use crate::ids::{AccountId, CaseId, WorkflowKey, WorkflowVersion};
use crate::interaction::InteractionSpec;
use crate::locale::Locale;
use crate::operation::{GlossaryTerm, OperationSpec};
use crate::target::ResolvedAct;

/// An obligation in erased form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErasedObligation {
    /// Stable identifier (canonical JSON).
    pub id: ObligationId,
    /// Canonical JSON value.
    pub value: serde_json::Value,
    /// The same thing in words, when the workflow says it.
    ///
    /// Filled at projection, the one place holding both the obligation and the
    /// definition — the same seam
    /// [`ErasedWorkflowView::state`](Self::value) is filled at. See
    /// [`WorkflowDefinition::obligation_sentence`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sentence: Option<crate::locale::LocalizedText>,
    /// The act that answers it, when the workflow names one.
    /// See [`WorkflowDefinition::obligation_act`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub act: Option<super::ObligationAct>,
}

/// A [`WorkflowView`] with phase, obligations and
/// outcome as canonical JSON, plus the phase ownership the definition declared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErasedWorkflowView {
    /// The case and the revision it was projected at.
    pub case_ref: CaseRef,
    /// Version of the definition.
    pub workflow_version: WorkflowVersion,
    /// The phase, canonical JSON.
    pub phase: serde_json::Value,
    /// Who must act.
    pub phase_ownership: PhaseOwnership,
    /// Open obligations.
    pub obligations: Vec<ErasedObligation>,
    /// Blocking requirement, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocking_interaction: Option<InteractionRequirement>,
    /// Notices.
    #[serde(default)]
    pub notices: Vec<WorkflowNotice>,
    /// Outcome, canonical JSON, present only when complete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outcome: Option<serde_json::Value>,
    /// What the case HOLDS, as the workflow says it may be stated.
    ///
    /// The counterpart of [`Self::obligations`], which say what it still needs.
    /// Filled from
    /// [`WorkflowDefinition::narratable_state`]
    /// at projection, because that is the one place holding both the state and
    /// the view. Empty for a workflow that declares none, which is the default.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub state: Vec<crate::flow::StateField>,
}

impl ErasedWorkflowView {
    /// Returns `true` when an outcome is present.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.outcome.is_some()
    }

    /// Returns `true` when the phase is user-owned.
    #[must_use]
    pub fn is_user_owned(&self) -> bool {
        self.phase_ownership == PhaseOwnership::User
    }

    /// Identifiers of the open obligations.
    #[must_use]
    pub fn obligation_ids(&self) -> Vec<&ObligationId> {
        self.obligations.iter().map(|o| &o.id).collect()
    }
}

/// Object-safe view of a workflow definition (spec §8.3).
///
/// State, commands and events cross this boundary as JSON. Methods that need a
/// view take `(case_ref, state)` and re-project internally.
pub trait ErasedWorkflow: Send + Sync {
    /// Stable key.
    fn key(&self) -> WorkflowKey;

    /// Version.
    fn version(&self) -> WorkflowVersion;

    /// Projects a case.
    fn project(
        &self,
        case_ref: CaseRef,
        state: Option<&serde_json::Value>,
    ) -> Result<ErasedWorkflowView, ErasureError>;

    /// The operations offered for this case, validated and stamped with this workflow.
    fn operations(
        &self,
        case_ref: CaseRef,
        state: Option<&serde_json::Value>,
    ) -> Result<Vec<OperationSpec>, ErasureError>;
    /// One line saying what the workflow is for.
    fn summary(&self) -> Option<String>;
    /// Terms this workflow's users say, and what they mean here.
    fn glossary(&self) -> Vec<GlossaryTerm>;
    /// What one record of this workflow is called. See [`WorkflowDefinition::noun`].
    fn noun(&self) -> Option<crate::locale::LocalizedText> {
        None
    }
    /// Every operation a record may offer. See [`WorkflowDefinition::record_operations`].
    fn record_operations(&self) -> Vec<OperationSpec> {
        Vec::new()
    }

    /// Guidance for understanding a turn about this case, as the workflow wrote it,
    /// untruncated: the deployment's [`BriefingBudget`](crate::flow::BriefingBudget)
    /// applies where it is shown.
    fn briefing(
        &self,
        case_ref: CaseRef,
        state: Option<&serde_json::Value>,
    ) -> Result<Option<String>, ErasureError>;

    /// What must be true of another case before this workflow may be started.
    ///
    /// Needs no erasure: a precondition is plain data, so this is the typed
    /// method verbatim. See
    /// [`crate::flow::WorkflowDefinition::start_preconditions`].
    fn start_preconditions(&self) -> Vec<StartPrecondition>;

    /// What starting this workflow means when a case of it is already open.
    ///
    /// Needs no erasure either: the answer is a property of the workflow, not
    /// of any case. See
    /// [`crate::flow::WorkflowDefinition::start_behaviour`].
    fn start_behaviour(&self) -> StartBehaviour;

    /// What a confirmation the policy engine raises is about.
    ///
    /// Takes the state and projects it, the way
    /// [`Self::briefing`] does, because the typed method is a function of both.
    /// See [`crate::flow::WorkflowDefinition::confirmation_subject`].
    fn confirmation_subject(
        &self,
        case_ref: CaseRef,
        state: Option<&serde_json::Value>,
        act: &ResolvedAct,
    ) -> Result<Option<ConfirmationSubject>, ErasureError>;

    /// The documents the case `view` describes has.
    ///
    /// See [`crate::flow::WorkflowDefinition::artifacts`].
    fn artifacts(&self, view: &ErasedWorkflowView) -> Result<Vec<ArtifactRef>, ErasureError>;

    /// Whether a new case may be opened while `open` ones are.
    ///
    /// Erased from views rather than from states, because the caller is the
    /// reducer and the reducer holds views. A refusal travels as
    /// [`ErasedCallError::Rejected`], the way the other two domain decisions at
    /// this boundary do. See
    /// [`crate::flow::WorkflowDefinition::may_open_beside`].
    fn may_open_beside(&self, open: &[ErasedWorkflowView]) -> Result<(), ErasedCallError>;

    /// What this workflow wants said in the phase `view` is in.
    ///
    /// Erased from the view rather than from the state, because the composer
    /// holds views and not states: it speaks about the case as it stands after
    /// the turn committed, which is a projection somebody else already made.
    /// See [`crate::flow::WorkflowDefinition::transition_briefing`].
    fn narration_briefing(
        &self,
        stage: WritingStage,
        view: &ErasedWorkflowView,
    ) -> Result<Option<String>, ErasureError>;

    /// The complete value sets this workflow accepts, for the case in `state`.
    ///
    /// Erased the way [`Self::briefing`] is, and for the same reason: the
    /// declaration is a function of the view, and the view is a function of the
    /// state. See [`crate::flow::WorkflowDefinition::enumerations`].
    fn enumerations(
        &self,
        case_ref: CaseRef,
        state: Option<&serde_json::Value>,
    ) -> Result<Vec<DomainEnumeration>, ErasureError>;

    /// One obligation in words, erased. See
    /// [`WorkflowDefinition::obligation_sentence`].
    fn obligation_sentence(
        &self,
        obligation: &serde_json::Value,
    ) -> Result<Option<crate::locale::LocalizedText>, ErasureError>;

    /// What the user may do next once the case owes nothing, erased. See
    /// [`WorkflowDefinition::next_steps`].
    fn next_steps(
        &self,
        case_ref: CaseRef,
        state: Option<&serde_json::Value>,
    ) -> Result<Vec<super::NextStep>, ErasureError> {
        let _ = (case_ref, state);
        Ok(Vec::new())
    }

    /// Compiles a resolved act into erased commands.
    fn compile_act(
        &self,
        case_ref: CaseRef,
        state: Option<&serde_json::Value>,
        act: &ResolvedAct,
    ) -> Result<Vec<serde_json::Value>, ErasedCallError>;

    /// Why an act that compiled nothing changed nothing.
    ///
    /// See [`crate::flow::WorkflowDefinition::nothing_changed`]. Defaulted to
    /// `None` so an erasure written before this existed still compiles, and
    /// asked only after [`compile_act`](Self::compile_act) has already answered
    /// on the same state.
    fn nothing_changed(
        &self,
        state: Option<&serde_json::Value>,
        act: &ResolvedAct,
    ) -> Option<crate::locale::LocalizedText> {
        let _ = (state, act);
        None
    }

    /// Policy of an erased command.
    fn command_policy(
        &self,
        state: Option<&serde_json::Value>,
        command: &serde_json::Value,
    ) -> Result<CommandPolicy, ErasureError>;

    /// Validates an erased command.
    fn validate_command(
        &self,
        state: Option<&serde_json::Value>,
        command: &serde_json::Value,
    ) -> Result<(), ErasedCallError>;

    /// The erased state an erased command leaves, when the workflow can tell.
    fn state_after(
        &self,
        state: Option<&serde_json::Value>,
        command: &serde_json::Value,
    ) -> Result<Option<serde_json::Value>, ErasureError> {
        let _ = (state, command);
        Ok(None)
    }

    /// Renders receipts from erased ledger events.
    ///
    /// A [`ReceiptEvent::Redacted`] entry carries no payload to deserialize, so
    /// an erased event reaches the domain as an erased event rather than as a
    /// deserialization failure.
    fn receipts(
        &self,
        events: &[ReceiptEvent<serde_json::Value>],
        locale: &Locale,
    ) -> Result<Vec<OperationalReceipt>, ErasureError>;

    /// Builds the interaction spec for a requirement of the projected view.
    ///
    /// The returned spec must belong to `case_ref` and must be answerable, so
    /// neither a card bound to another case nor one nobody could answer reaches
    /// persistence.
    fn build_interaction(
        &self,
        case_ref: CaseRef,
        state: Option<&serde_json::Value>,
        requirement: &InteractionRequirement,
    ) -> Result<InteractionSpec, ErasedCallError>;
}

/// The read-only half of [`ErasedExecutor`]: loading a case, and nothing else.
///
/// This is the erased counterpart of [`CaseLoader`](crate::flow::CaseLoader),
/// and the same reasoning applies: a caller that must be unable to mutate a
/// case is handed a value that has no `execute` method rather than a value it
/// is asked not to use. [`WorkflowRegistry::read_only`] projects a whole
/// registry into definitions plus loaders for exactly this purpose.
///
/// **There is nothing to implement.** Every [`ErasedExecutor`] is an
/// `ErasedCaseLoader` through the blanket implementation below, and
/// [`CaseLoaderHandle`] turns an `Arc<dyn ErasedExecutor>` into an owned
/// loader.
#[async_trait::async_trait]
pub trait ErasedCaseLoader: Send + Sync {
    /// Loads a case for an account as JSON.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the case could not be read.
    async fn load_case(
        &self,
        account: &AccountId,
        case_id: &CaseId,
    ) -> Result<Versioned<Option<serde_json::Value>>, StoreError>;
}

/// Every erased executor loads.
#[async_trait::async_trait]
impl<E: ErasedExecutor + ?Sized> ErasedCaseLoader for E {
    async fn load_case(
        &self,
        account: &AccountId,
        case_id: &CaseId,
    ) -> Result<Versioned<Option<serde_json::Value>>, StoreError> {
        self.load(account, case_id).await
    }
}

/// The loading half of one executor, as a value of its own.
///
/// `Arc<dyn ErasedExecutor>` cannot be coerced to `Arc<dyn ErasedCaseLoader>`
/// — the two are unrelated trait objects — so this handle is the bridge. It
/// holds the executor and exposes exactly one method, which is what makes it
/// safe to give away: a holder can read a case and has no name for anything
/// else.
#[derive(Clone)]
pub struct CaseLoaderHandle {
    executor: Arc<dyn ErasedExecutor>,
}

impl CaseLoaderHandle {
    /// The loading half of `executor`.
    #[must_use]
    pub fn new(executor: Arc<dyn ErasedExecutor>) -> Self {
        Self { executor }
    }
}

impl fmt::Debug for CaseLoaderHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CaseLoaderHandle").finish_non_exhaustive()
    }
}

#[async_trait::async_trait]
impl ErasedCaseLoader for CaseLoaderHandle {
    async fn load_case(
        &self,
        account: &AccountId,
        case_id: &CaseId,
    ) -> Result<Versioned<Option<serde_json::Value>>, StoreError> {
        self.executor.load(account, case_id).await
    }
}

/// Object-safe executor working on erased state, commands and events.
#[async_trait::async_trait]
pub trait ErasedExecutor: Send + Sync {
    /// Loads a case for an account as JSON.
    async fn load(
        &self,
        account: &AccountId,
        case_id: &CaseId,
    ) -> Result<Versioned<Option<serde_json::Value>>, StoreError>;

    /// Executes an erased batch.
    async fn execute(
        &self,
        batch: CommandBatch<serde_json::Value>,
    ) -> Result<Commit<serde_json::Value, serde_json::Value>, ExecutionError>;
}

/// Wraps a typed definition and executor, converting only at the boundary.
pub struct TypedWorkflowAdapter<W, E> {
    definition: W,
    executor: E,
}

impl<W, E> TypedWorkflowAdapter<W, E> {
    /// Pairs a definition with its executor.
    #[must_use]
    pub const fn new(definition: W, executor: E) -> Self {
        Self {
            definition,
            executor,
        }
    }

    /// The typed definition.
    #[must_use]
    pub const fn definition(&self) -> &W {
        &self.definition
    }

    /// The typed executor.
    #[must_use]
    pub const fn executor(&self) -> &E {
        &self.executor
    }
}

impl<W: WorkflowDefinition, E> fmt::Debug for TypedWorkflowAdapter<W, E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TypedWorkflowAdapter")
            .field("workflow", &self.definition.key())
            .field("version", &self.definition.version())
            .finish_non_exhaustive()
    }
}

impl<W: WorkflowDefinition, E> TypedWorkflowAdapter<W, E> {
    fn state(&self, value: Option<&serde_json::Value>) -> Result<Option<W::State>, ErasureError> {
        value
            .map(|v| {
                W::State::deserialize(v).map_err(|_| ErasureError::StateDeserialization {
                    workflow: self.definition.key(),
                })
            })
            .transpose()
    }

    fn command(&self, value: &serde_json::Value) -> Result<W::Command, ErasureError> {
        W::Command::deserialize(value).map_err(|_| ErasureError::CommandDeserialization {
            workflow: self.definition.key(),
        })
    }

    fn serialize<T: Serialize>(&self, value: &T) -> Result<serde_json::Value, ErasureError> {
        serde_json::to_value(value).map_err(|_| ErasureError::Serialization {
            workflow: self.definition.key(),
        })
    }

    fn typed_view(&self, case_ref: CaseRef, state: Option<&W::State>) -> ViewOf<W> {
        self.definition.project(case_ref, state)
    }

    /// Reads an erased view back into the typed one that produced it.
    ///
    /// A view that does not round-trip is the workflow's own phase, obligation
    /// or outcome failing to deserialize into itself, which is the same defect
    /// the state and command boundaries report rather than paper over.
    fn typed_view_from(&self, view: &ErasedWorkflowView) -> Result<ViewOf<W>, ErasureError> {
        let mismatch = || ErasureError::StateDeserialization {
            workflow: self.definition.key(),
        };
        let phase: W::Phase = serde_json::from_value(view.phase.clone()).map_err(|_| mismatch())?;
        let mut obligations = Vec::with_capacity(view.obligations.len());
        for obligation in &view.obligations {
            obligations.push(
                serde_json::from_value::<W::Obligation>(obligation.value.clone())
                    .map_err(|_| mismatch())?,
            );
        }
        let outcome = view
            .outcome
            .clone()
            .map(serde_json::from_value::<W::Outcome>)
            .transpose()
            .map_err(|_| mismatch())?;
        Ok(WorkflowView {
            case_ref: view.case_ref.clone(),
            workflow_version: view.workflow_version.clone(),
            phase,
            obligations,
            blocking_interaction: view.blocking_interaction.clone(),
            notices: view.notices.clone(),
            outcome,
        })
    }

    /// The state an erased call re-projects must be the very case the act
    /// resolved to, at the revision it resolved at (I13, spec §12.2). Without
    /// this an act aimed at one trip could be compiled against another
    /// trip's state.
    fn check_same_case(&self, projected: &CaseRef, resolved: &CaseRef) -> Result<(), ErasureError> {
        if projected.same_case(resolved)
            && projected.expected_revision == resolved.expected_revision
        {
            Ok(())
        } else {
            Err(ErasureError::CaseMismatch {
                workflow: self.definition.key(),
            })
        }
    }
}

impl<W: WorkflowDefinition, E: Send + Sync> ErasedWorkflow for TypedWorkflowAdapter<W, E> {
    fn key(&self) -> WorkflowKey {
        self.definition.key()
    }

    fn version(&self) -> WorkflowVersion {
        self.definition.version()
    }

    fn project(
        &self,
        case_ref: CaseRef,
        state: Option<&serde_json::Value>,
    ) -> Result<ErasedWorkflowView, ErasureError> {
        let state = self.state(state)?;
        let view = self.typed_view(case_ref, state.as_ref());
        let ownership = self.definition.phase_ownership(&view.phase);
        let mut erased = view
            .erase(ownership)
            .map_err(|_| ErasureError::Serialization {
                workflow: self.definition.key(),
            })?;
        // Here and not on the typed view, because this is the only place that
        // holds both: the view is a projection and a projection drops the
        // values, so what a case HOLDS has to be asked of the state.
        erased.state = self.definition.narratable_state(state.as_ref());
        // The words for each obligation, from the same place and for the same
        // reason: this is the only seam holding the typed obligation and the
        // definition at once.
        for obligation in &mut erased.obligations {
            if let Ok(typed) = serde_json::from_value::<W::Obligation>(obligation.value.clone()) {
                obligation.sentence = self.definition.obligation_sentence(&typed);
                obligation.act = self.definition.obligation_act(state.as_ref(), &typed);
            }
        }
        Ok(erased)
    }

    fn operations(
        &self,
        case_ref: CaseRef,
        state: Option<&serde_json::Value>,
    ) -> Result<Vec<OperationSpec>, ErasureError> {
        let state = self.state(state)?;
        let view = self.typed_view(case_ref, state.as_ref());
        let workflow = self.definition.key();
        let mut operations = self.definition.operations(&view);
        for operation in &mut operations {
            operation.workflow = workflow.clone();
            operation
                .validate()
                .map_err(|error| ErasureError::InvalidOperation {
                    workflow: workflow.clone(),
                    reason: error.to_string(),
                })?;
        }
        Ok(operations)
    }

    fn summary(&self) -> Option<String> {
        self.definition.summary()
    }

    fn glossary(&self) -> Vec<GlossaryTerm> {
        self.definition.glossary()
    }

    fn noun(&self) -> Option<crate::locale::LocalizedText> {
        self.definition.noun()
    }

    fn record_operations(&self) -> Vec<OperationSpec> {
        self.definition.record_operations()
    }

    fn briefing(
        &self,
        case_ref: CaseRef,
        state: Option<&serde_json::Value>,
    ) -> Result<Option<String>, ErasureError> {
        let state = self.state(state)?;
        let view = self.typed_view(case_ref, state.as_ref());
        Ok(self.definition.briefing(&view))
    }

    fn start_preconditions(&self) -> Vec<StartPrecondition> {
        self.definition.start_preconditions()
    }

    fn start_behaviour(&self) -> StartBehaviour {
        self.definition.start_behaviour()
    }

    fn confirmation_subject(
        &self,
        case_ref: CaseRef,
        state: Option<&serde_json::Value>,
        act: &ResolvedAct,
    ) -> Result<Option<ConfirmationSubject>, ErasureError> {
        let state = self.state(state)?;
        let view = self.typed_view(case_ref, state.as_ref());
        Ok(self
            .definition
            .confirmation_subject(state.as_ref(), &view, act))
    }

    fn artifacts(&self, view: &ErasedWorkflowView) -> Result<Vec<ArtifactRef>, ErasureError> {
        let typed = self.typed_view_from(view)?;
        Ok(self.definition.artifacts(&typed))
    }

    fn may_open_beside(&self, open: &[ErasedWorkflowView]) -> Result<(), ErasedCallError> {
        let typed: Vec<ViewOf<W>> = open
            .iter()
            .map(|view| self.typed_view_from(view))
            .collect::<Result<_, _>>()?;
        self.definition
            .may_open_beside(&typed)
            .map_err(ErasedCallError::from)
    }

    fn narration_briefing(
        &self,
        stage: WritingStage,
        view: &ErasedWorkflowView,
    ) -> Result<Option<String>, ErasureError> {
        let typed = self.typed_view_from(view)?;
        Ok(match stage {
            WritingStage::Transition => self.definition.transition_briefing(&typed),
            WritingStage::Answer => self.definition.answer_briefing(&typed),
        })
    }

    fn enumerations(
        &self,
        case_ref: CaseRef,
        state: Option<&serde_json::Value>,
    ) -> Result<Vec<DomainEnumeration>, ErasureError> {
        let state = self.state(state)?;
        let view = self.typed_view(case_ref, state.as_ref());
        Ok(self.definition.enumerations(&view))
    }

    fn next_steps(
        &self,
        case_ref: CaseRef,
        state: Option<&serde_json::Value>,
    ) -> Result<Vec<super::NextStep>, ErasureError> {
        let state = self.state(state)?;
        let view = self.typed_view(case_ref, state.as_ref());
        Ok(self.definition.next_steps(state.as_ref(), &view))
    }

    fn obligation_sentence(
        &self,
        obligation: &serde_json::Value,
    ) -> Result<Option<crate::locale::LocalizedText>, ErasureError> {
        let typed: W::Obligation = serde_json::from_value(obligation.clone()).map_err(|_| {
            ErasureError::StateDeserialization {
                workflow: self.definition.key(),
            }
        })?;
        Ok(self.definition.obligation_sentence(&typed))
    }

    fn compile_act(
        &self,
        case_ref: CaseRef,
        state: Option<&serde_json::Value>,
        act: &ResolvedAct,
    ) -> Result<Vec<serde_json::Value>, ErasedCallError> {
        self.check_same_case(&case_ref, &act.case_ref)?;
        let state = self.state(state)?;
        let view = self.typed_view(case_ref, state.as_ref());
        let commands = self.definition.compile_act(state.as_ref(), &view, act)?;
        commands
            .iter()
            .map(|c| self.serialize(c).map_err(ErasedCallError::from))
            .collect()
    }

    fn nothing_changed(
        &self,
        state: Option<&serde_json::Value>,
        act: &ResolvedAct,
    ) -> Option<crate::locale::LocalizedText> {
        // A state that does not deserialize answers `None` rather than failing
        // the turn: this is asked only after `compile_act` has succeeded on the
        // same state, so it cannot happen, and the worst a wrong answer here
        // could do is leave the writing stage with the operation's name — which
        // is what it had before this channel existed.
        let state = self.state(state).ok()?;
        self.definition.nothing_changed(state.as_ref(), act)
    }

    fn command_policy(
        &self,
        state: Option<&serde_json::Value>,
        command: &serde_json::Value,
    ) -> Result<CommandPolicy, ErasureError> {
        let state = self.state(state)?;
        let command = self.command(command)?;
        Ok(self.definition.command_policy(state.as_ref(), &command))
    }

    fn validate_command(
        &self,
        state: Option<&serde_json::Value>,
        command: &serde_json::Value,
    ) -> Result<(), ErasedCallError> {
        let state = self.state(state)?;
        let command = self.command(command)?;
        self.definition
            .validate_command(state.as_ref(), &command)
            .map_err(ErasedCallError::from)
    }

    fn state_after(
        &self,
        state: Option<&serde_json::Value>,
        command: &serde_json::Value,
    ) -> Result<Option<serde_json::Value>, ErasureError> {
        let state = self.state(state)?;
        let command = self.command(command)?;
        self.definition
            .state_after(state.as_ref(), &command)
            .map(|next| self.serialize(&next))
            .transpose()
    }

    fn receipts(
        &self,
        events: &[ReceiptEvent<serde_json::Value>],
        locale: &Locale,
    ) -> Result<Vec<OperationalReceipt>, ErasureError> {
        // Deserialized from the borrowed JSON: identity and timestamp are kept
        // so the domain can cite the event ids its receipts rest on (I16). An
        // erased payload has nothing to deserialize and passes through as
        // `Redacted`, which is why erasure never looks like a broken domain.
        let events = events
            .iter()
            .map(|event| {
                event.try_map_payload_ref(|payload| {
                    W::Event::deserialize(payload).map_err(|_| ErasureError::EventDeserialization {
                        workflow: self.definition.key(),
                    })
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(self.definition.receipts(&events, locale))
    }

    fn build_interaction(
        &self,
        case_ref: CaseRef,
        state: Option<&serde_json::Value>,
        requirement: &InteractionRequirement,
    ) -> Result<InteractionSpec, ErasedCallError> {
        let state = self.state(state)?;
        let view = self.typed_view(case_ref.clone(), state.as_ref());
        let spec = self
            .definition
            .build_interaction(state.as_ref(), &view, requirement)?;
        // The card belongs to the case it was projected for, at that revision:
        // a card bound elsewhere would be validated against the wrong state.
        self.check_same_case(&case_ref, &spec.case_ref)?;
        spec.validate()?;
        Ok(spec)
    }
}

#[async_trait::async_trait]
impl<W, E> ErasedExecutor for TypedWorkflowAdapter<W, E>
where
    W: WorkflowDefinition,
    E: WorkflowExecutor<W>,
{
    async fn load(
        &self,
        account: &AccountId,
        case_id: &CaseId,
    ) -> Result<Versioned<Option<serde_json::Value>>, StoreError> {
        let loaded = self.executor.load(account, case_id).await?;
        let revision = loaded.revision;
        let value = loaded
            .value
            .map(|s| serde_json::to_value(&s).map_err(|_| StoreError::Serialization))
            .transpose()?;
        Ok(Versioned::new(value, revision))
    }

    async fn execute(
        &self,
        batch: CommandBatch<serde_json::Value>,
    ) -> Result<Commit<serde_json::Value, serde_json::Value>, ExecutionError> {
        let typed = batch.try_map(|c| self.command(&c))?;
        let commit = self.executor.execute(typed).await?;
        let state = commit
            .state
            .as_ref()
            .map(|s| self.serialize(s))
            .transpose()?;
        let events = commit
            .events
            .into_iter()
            .map(|e: CommittedEvent<W::Event>| e.try_map_payload(|p| self.serialize(&p)))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Commit {
            state,
            new_revision: commit.new_revision,
            events,
            idempotency_replay: commit.idempotency_replay,
        })
    }
}

/// A registered workflow: its erased definition and executor.
#[derive(Clone)]
pub struct RegisteredWorkflow {
    /// Stable key.
    pub key: WorkflowKey,
    /// Version.
    pub version: WorkflowVersion,
    /// Erased definition.
    pub definition: Arc<dyn ErasedWorkflow>,
    /// Erased executor.
    pub executor: Arc<dyn ErasedExecutor>,
}

impl fmt::Debug for RegisteredWorkflow {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RegisteredWorkflow")
            .field("key", &self.key)
            .field("version", &self.version)
            .finish_non_exhaustive()
    }
}

impl RegisteredWorkflow {
    /// The loading half of this workflow's executor.
    #[must_use]
    pub fn loader(&self) -> CaseLoaderHandle {
        CaseLoaderHandle::new(Arc::clone(&self.executor))
    }

    /// Checks that a stored record's version matches the registered one.
    ///
    /// # Errors
    ///
    /// [`ErasureError::VersionMismatch`] when the versions differ.
    pub fn check_version(&self, found: &WorkflowVersion) -> Result<(), ErasureError> {
        if &self.version == found {
            Ok(())
        } else {
            Err(ErasureError::VersionMismatch {
                workflow: self.key.clone(),
                registered: self.version.clone(),
                found: found.clone(),
            })
        }
    }
}

/// Heterogeneous set of workflows keyed by [`WorkflowKey`] (spec §8.3).
#[derive(Debug, Clone, Default)]
pub struct WorkflowRegistry {
    entries: IndexMap<WorkflowKey, RegisteredWorkflow>,
}

impl WorkflowRegistry {
    /// Starts a builder.
    #[must_use]
    pub fn builder() -> WorkflowRegistryBuilder {
        WorkflowRegistryBuilder::default()
    }

    /// Looks a workflow up.
    #[must_use]
    pub fn get(&self, key: &WorkflowKey) -> Option<&RegisteredWorkflow> {
        self.entries.get(key)
    }

    /// Looks a workflow up, failing when unknown.
    pub fn require(&self, key: &WorkflowKey) -> Result<&RegisteredWorkflow, ErasureError> {
        self.entries
            .get(key)
            .ok_or_else(|| ErasureError::UnknownWorkflow {
                workflow: key.clone(),
            })
    }

    /// Checks that `found` is the registered version of `key`.
    pub fn check_version(
        &self,
        key: &WorkflowKey,
        found: &WorkflowVersion,
    ) -> Result<(), ErasureError> {
        self.require(key)?.check_version(found)
    }

    /// Returns `true` when the key is registered.
    #[must_use]
    pub fn contains(&self, key: &WorkflowKey) -> bool {
        self.entries.contains_key(key)
    }

    /// Iterates registrations in registration order.
    pub fn iter(&self) -> impl Iterator<Item = &RegisteredWorkflow> {
        self.entries.values()
    }

    /// Registered keys in registration order.
    pub fn keys(&self) -> impl Iterator<Item = &WorkflowKey> {
        self.entries.keys()
    }

    /// Number of workflows.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` when empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The definitions alone: everything pure, no executor of any kind.
    ///
    /// Projection, the act catalog, act compilation, command policy, command
    /// validation and receipts are all here, and none of them reads or writes
    /// anything. A caller handed this cannot even load a case, which is the
    /// right shape for a path that is given the state instead.
    #[must_use]
    pub fn definitions(&self) -> WorkflowDefinitions {
        WorkflowDefinitions {
            entries: self
                .entries
                .iter()
                .map(|(key, registered)| (key.clone(), Arc::clone(&registered.definition)))
                .collect(),
        }
    }

    /// The registry with the write half of every executor removed: the pure
    /// definitions plus the loading half of each executor.
    ///
    /// This is what the plan-only turn path is built on. Nothing in the
    /// returned value can execute a batch, so a planning run cannot mutate a
    /// case however it is refactored.
    #[must_use]
    pub fn read_only(&self) -> WorkflowReadRegistry {
        WorkflowReadRegistry {
            definitions: self.definitions(),
            loaders: self
                .entries
                .iter()
                .map(|(key, registered)| {
                    let loader: Arc<dyn ErasedCaseLoader> = Arc::new(registered.loader());
                    (key.clone(), loader)
                })
                .collect(),
        }
    }
}

/// The pure half of a set of workflows: definitions only.
///
/// Built by [`WorkflowRegistry::definitions`], or from erased definitions
/// directly when there is no registry — a corpus replayed against a projector
/// has states in hand and no executor at all.
#[derive(Clone, Default)]
pub struct WorkflowDefinitions {
    entries: IndexMap<WorkflowKey, Arc<dyn ErasedWorkflow>>,
}

impl fmt::Debug for WorkflowDefinitions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkflowDefinitions")
            .field("keys", &self.entries.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl WorkflowDefinitions {
    /// An empty set.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one definition, keyed by [`ErasedWorkflow::key`]. A repeated key
    /// replaces the previous entry.
    #[must_use]
    pub fn with(mut self, definition: Arc<dyn ErasedWorkflow>) -> Self {
        self.entries.insert(definition.key(), definition);
        self
    }

    /// Looks a definition up.
    #[must_use]
    pub fn get(&self, key: &WorkflowKey) -> Option<&Arc<dyn ErasedWorkflow>> {
        self.entries.get(key)
    }

    /// Looks a definition up, failing when unknown.
    ///
    /// # Errors
    ///
    /// [`ErasureError::UnknownWorkflow`] when the key is not present.
    pub fn require(&self, key: &WorkflowKey) -> Result<&Arc<dyn ErasedWorkflow>, ErasureError> {
        self.entries
            .get(key)
            .ok_or_else(|| ErasureError::UnknownWorkflow {
                workflow: key.clone(),
            })
    }

    /// Returns `true` when the key is present.
    #[must_use]
    pub fn contains(&self, key: &WorkflowKey) -> bool {
        self.entries.contains_key(key)
    }

    /// Keys in insertion order.
    pub fn keys(&self) -> impl Iterator<Item = &WorkflowKey> {
        self.entries.keys()
    }

    /// Definitions in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn ErasedWorkflow>> {
        self.entries.values()
    }

    /// Number of definitions.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` when empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl From<&WorkflowRegistry> for WorkflowDefinitions {
    fn from(registry: &WorkflowRegistry) -> Self {
        registry.definitions()
    }
}

impl From<&Arc<WorkflowRegistry>> for WorkflowDefinitions {
    fn from(registry: &Arc<WorkflowRegistry>) -> Self {
        registry.definitions()
    }
}

impl From<Arc<WorkflowRegistry>> for WorkflowDefinitions {
    fn from(registry: Arc<WorkflowRegistry>) -> Self {
        registry.definitions()
    }
}

/// The read-only projection of a [`WorkflowRegistry`]: definitions, and the
/// loading half of each executor.
///
/// A registry built from [`WorkflowDefinitions`] alone carries no loader, and
/// [`WorkflowReadRegistry::require_loader`] then says so rather than silently
/// reading nothing — which is exactly what a caller that seeds the state wants,
/// because it should never reach a loader in the first place.
#[derive(Clone, Default)]
pub struct WorkflowReadRegistry {
    definitions: WorkflowDefinitions,
    loaders: IndexMap<WorkflowKey, Arc<dyn ErasedCaseLoader>>,
}

impl fmt::Debug for WorkflowReadRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WorkflowReadRegistry")
            .field("definitions", &self.definitions)
            .field("loaders", &self.loaders.len())
            .finish()
    }
}

impl WorkflowReadRegistry {
    /// Definitions with no loader at all.
    #[must_use]
    pub fn from_definitions(definitions: WorkflowDefinitions) -> Self {
        Self {
            definitions,
            loaders: IndexMap::new(),
        }
    }

    /// The definitions.
    #[must_use]
    pub const fn definitions(&self) -> &WorkflowDefinitions {
        &self.definitions
    }

    /// The loading half of one workflow's executor, when there is one.
    #[must_use]
    pub fn loader(&self, key: &WorkflowKey) -> Option<&Arc<dyn ErasedCaseLoader>> {
        self.loaders.get(key)
    }

    /// The loading half of one workflow's executor.
    ///
    /// # Errors
    ///
    /// [`ErasureError::UnknownWorkflow`] when the workflow is not registered or
    /// this projection carries no loader for it.
    pub fn require_loader(
        &self,
        key: &WorkflowKey,
    ) -> Result<&Arc<dyn ErasedCaseLoader>, ErasureError> {
        self.loaders
            .get(key)
            .ok_or_else(|| ErasureError::UnknownWorkflow {
                workflow: key.clone(),
            })
    }

    /// Returns `true` when no loader is carried at all.
    #[must_use]
    pub fn has_no_loaders(&self) -> bool {
        self.loaders.is_empty()
    }
}

impl From<&WorkflowRegistry> for WorkflowReadRegistry {
    fn from(registry: &WorkflowRegistry) -> Self {
        registry.read_only()
    }
}

/// Builds a [`WorkflowRegistry`].
#[derive(Debug, Default)]
pub struct WorkflowRegistryBuilder {
    entries: Vec<RegisteredWorkflow>,
}

impl WorkflowRegistryBuilder {
    /// Registers a typed definition with its executor.
    #[must_use]
    pub fn register<W, E>(self, definition: W, executor: E) -> Self
    where
        W: WorkflowDefinition,
        E: WorkflowExecutor<W> + 'static,
    {
        let adapter = Arc::new(TypedWorkflowAdapter::new(definition, executor));
        let erased_definition: Arc<dyn ErasedWorkflow> = adapter.clone();
        let erased_executor: Arc<dyn ErasedExecutor> = adapter;
        self.register_erased(erased_definition, erased_executor)
    }

    /// Registers an already erased pair.
    #[must_use]
    pub fn register_erased(
        mut self,
        definition: Arc<dyn ErasedWorkflow>,
        executor: Arc<dyn ErasedExecutor>,
    ) -> Self {
        self.entries.push(RegisteredWorkflow {
            key: definition.key(),
            version: definition.version(),
            definition,
            executor,
        });
        self
    }

    /// Builds the registry, failing on duplicate keys.
    pub fn build(self) -> Result<WorkflowRegistry, ErasureError> {
        let mut entries = IndexMap::with_capacity(self.entries.len());
        for entry in self.entries {
            if entries.contains_key(&entry.key) {
                return Err(ErasureError::DuplicateWorkflow {
                    workflow: entry.key,
                });
            }
            entries.insert(entry.key.clone(), entry);
        }
        Ok(WorkflowRegistry { entries })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::case::Versioned;
    use crate::flow::{PhaseOwnership, ViewOf, WorkflowDefinition, WorkflowView};
    use crate::ids::CaseRevision;
    use crate::locale::Locale;
    use crate::target::ResolvedAct;
    use serde::{Deserialize, Serialize};

    #[derive(Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
    struct Nothing;

    struct Toy;

    impl WorkflowDefinition for Toy {
        type State = Nothing;
        type Phase = Nothing;
        type Obligation = Nothing;
        type Command = Nothing;
        type Event = Nothing;
        type Outcome = Nothing;

        fn key(&self) -> WorkflowKey {
            WorkflowKey::from("toy")
        }

        fn version(&self) -> WorkflowVersion {
            WorkflowVersion::from("1")
        }

        fn phase_ownership(&self, _phase: &Self::Phase) -> PhaseOwnership {
            PhaseOwnership::System
        }

        fn project(&self, case_ref: CaseRef, _state: Option<&Self::State>) -> ViewOf<Self> {
            WorkflowView::new(case_ref, self.version(), Nothing)
        }

        fn operations(&self, _view: &ViewOf<Self>) -> Vec<crate::operation::OperationSpec> {
            Vec::new()
        }

        fn compile_act(
            &self,
            _state: Option<&Self::State>,
            _view: &ViewOf<Self>,
            _act: &ResolvedAct,
        ) -> Result<Vec<Self::Command>, crate::error::DomainRejection> {
            Ok(Vec::new())
        }

        fn command_policy(
            &self,
            _state: Option<&Self::State>,
            _command: &Self::Command,
        ) -> CommandPolicy {
            CommandPolicy::conservative()
        }

        fn validate_command(
            &self,
            _state: Option<&Self::State>,
            _command: &Self::Command,
        ) -> Result<(), crate::error::DomainRejection> {
            Ok(())
        }

        fn receipts(
            &self,
            _events: &[ReceiptEvent<Self::Event>],
            _locale: &Locale,
        ) -> Vec<OperationalReceipt> {
            Vec::new()
        }
    }

    struct Executor;

    #[async_trait::async_trait]
    impl crate::flow::WorkflowExecutor<Toy> for Executor {
        async fn load(
            &self,
            _account: &AccountId,
            _case_id: &CaseId,
        ) -> Result<Versioned<Option<Nothing>>, StoreError> {
            Ok(Versioned::new(Some(Nothing), CaseRevision(7)))
        }

        async fn execute(
            &self,
            _batch: CommandBatch<Nothing>,
        ) -> Result<Commit<Nothing, Nothing>, ExecutionError> {
            panic!("the read-only projection must never reach execution");
        }
    }

    fn registry() -> WorkflowRegistry {
        WorkflowRegistry::builder()
            .register(Toy, Executor)
            .build()
            .expect("one workflow, one key")
    }

    #[tokio::test]
    async fn the_read_only_projection_loads_and_cannot_execute() {
        let registry = registry();
        let reading = registry.read_only();
        let key = WorkflowKey::from("toy");

        assert!(reading.definitions().contains(&key));
        let loaded = reading
            .require_loader(&key)
            .expect("the projection carries a loader")
            .load_case(&AccountId::from("a"), &CaseId::from("c1"))
            .await
            .expect("the loader reads");
        assert_eq!(loaded.revision, CaseRevision(7));
        // `ErasedCaseLoader` has exactly one method, so there is nothing here
        // that could reach the panicking `execute`.
    }

    #[test]
    fn definitions_alone_carry_no_loader() {
        let definitions = registry().definitions();
        let reading = WorkflowReadRegistry::from_definitions(definitions);
        assert!(reading.has_no_loaders());
        assert!(reading.require_loader(&WorkflowKey::from("toy")).is_err());
        assert!(
            reading
                .definitions()
                .require(&WorkflowKey::from("toy"))
                .is_ok(),
            "the pure half is still there"
        );
    }

    #[test]
    fn definitions_can_be_built_without_any_executor_at_all() {
        // The executor parameter of the adapter only has to be `Send + Sync`
        // to make an `ErasedWorkflow`, so `()` is one.
        let definition: Arc<dyn ErasedWorkflow> = Arc::new(TypedWorkflowAdapter::new(Toy, ()));
        let definitions = WorkflowDefinitions::new().with(definition);
        assert_eq!(definitions.len(), 1);
        assert!(definitions.contains(&WorkflowKey::from("toy")));
    }
}

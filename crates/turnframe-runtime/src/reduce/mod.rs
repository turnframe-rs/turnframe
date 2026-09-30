//! The whole-turn reducer (spec §13).
//!
//! [`DefaultTurnReducer`] is the deterministic decision engine: every understood act
//! comes out with an explicit result (I11), executable commands are grouped into
//! batches, and the plan hashes to a value a replay reproduces (I20). It does no I/O,
//! reads no clock and consults no randomness.
//!
//! | Step | What it does |
//! | --- | --- |
//! | limits | an understanding over the deployment's limits is refused whole |
//! | understanding | an act that asks for a value or is held keeps that result |
//! | catalog | an unoffered operation, or arguments its schema refuses, refuses the act |
//! | prerequisites | an act on a record an earlier act opens waits for that act |
//! | targets | one target per act, through [`TargetResolver`] |
//! | compilation, validation, policy | per command, the domain first; a record the turn opens as its earlier acts leave it |
//! | batching | by case and atomicity scope; mutations on one case commit together |
//! | answers | one [`AnswerTask`] per question, an unsafe basis overridden |
//! | self-check | [`ReductionPlan::validate`] runs before the plan is returned |
//!
//! Build one reducer per turn: the states it compiles against are the turn's own.

mod act;
mod cards;
mod copy;
mod notices;
mod police;

pub use self::copy::{CONDITION_HOLDS_ANSWER, NoticeCopy, notice, rejection};

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use indexmap::IndexMap;
use turnframe_core::case::{CaseKey, CaseRef};
use turnframe_core::command::{
    AtomicityScope, CommandBatch, CommandEnvelope, CommandOrigin, CommandPolicy,
    ConfirmationPolicy, IdempotencyKey, RiskClass,
};
use turnframe_core::error::{DomainRejection, ErasedCallError, ReductionError, RejectionCode};
use turnframe_core::flow::{
    DomainEnumeration, ErasedWorkflow, ErasedWorkflowView, WorkflowDefinitions,
};
use turnframe_core::hash::{Digest, canonical_digest};
use turnframe_core::ids::{BatchId, BlockId, CommandId, OptionId, QuestionId, TurnId};
use turnframe_core::interaction::{
    InteractionKind, InteractionOption, InteractionPayload, InteractionSpec, OptionStyle,
    ReviewDiffEntry, StoredInteractionAction, TextResolutionPolicy,
};
use turnframe_core::locale::LocalizedText;
use turnframe_core::operation::OperationSpec;
use turnframe_core::plan::{AnswerBasis, TargetPolicy};
use turnframe_core::policy::PolicyDecision;
use turnframe_core::reduce::{
    AnswerTask, Capability, CommandRef, PlannedAct, PlannedActResult, ReductionContext,
    ReductionPlan, SourcePolicy, TextSpan, TurnReducer,
};
use turnframe_core::response::{AnswerProgress, NarratableFact, NoticeSeverity, ServerNotice};
use turnframe_core::target::{ResolvedAct, ResolvedActKind, TargetCandidate, TargetResolution};
use turnframe_core::turn::TurnInput;
use turnframe_core::understanding::{
    ActAction, ActId, ActStatus, ArgumentValue, ConstraintKind, NotUnderstoodReason, QuestionTopic,
    RecordValue, Understanding, UnderstoodAct, WordRange,
};

use crate::config::{ExecutionConfig, NarrationConfig, OrchestratorConfig};
use crate::policy::{BlockReason, PolicyEngine, PolicyOutcome, PolicyRequest};
use crate::resolve::{TargetOutcome, TargetResolver};
use crate::resume::DeferredAct;

/// The deterministic whole-turn reducer of §13. Build one per turn.
#[derive(Debug, Clone)]
pub struct DefaultTurnReducer {
    workflows: WorkflowDefinitions,
    resolver: TargetResolver,
    policy: PolicyEngine,
    states: IndexMap<CaseKey, serde_json::Value>,
    execution: ExecutionConfig,
    narration: NarrationConfig,
    copy: NoticeCopy,
    confirmed_origin: Option<(CommandOrigin, ActId)>,
}

impl DefaultTurnReducer {
    /// Builds a reducer for one turn: `resolver` carries the tokens and the card on
    /// screen, `policy` the mode and the card copy, `config` the execution and
    /// narration sections.
    #[must_use]
    pub fn new(
        workflows: impl Into<WorkflowDefinitions>,
        resolver: TargetResolver,
        policy: PolicyEngine,
        config: &OrchestratorConfig,
    ) -> Self {
        Self {
            workflows: workflows.into(),
            resolver,
            policy,
            states: IndexMap::new(),
            execution: config.execution,
            narration: config.narration,
            copy: NoticeCopy::standard(),
            confirmed_origin: None,
        }
    }

    /// Adds the loaded state of one case. A case with no entry compiles against `None`,
    /// which is what a case that does not exist yet looks like.
    #[must_use]
    pub fn with_state(mut self, case: CaseKey, state: serde_json::Value) -> Self {
        self.states.insert(case, state);
        self
    }

    /// Adds several loaded states.
    #[must_use]
    pub fn with_states(
        mut self,
        states: impl IntoIterator<Item = (CaseKey, serde_json::Value)>,
    ) -> Self {
        self.states.extend(states);
        self
    }

    /// Declares the origin a card answer minted, for the one act the card itself put
    /// into the plan. No other act receives it (I12).
    #[must_use]
    pub fn with_confirmed_origin(mut self, origin: CommandOrigin, act: ActId) -> Self {
        self.confirmed_origin = Some((origin, act));
        self
    }

    /// Replaces the notices the reducer writes itself. The shipped copy is English.
    #[must_use]
    pub fn with_copy(mut self, copy: NoticeCopy) -> Self {
        self.copy = copy;
        self
    }

    /// The resolver this reducer was built with.
    #[must_use]
    pub const fn resolver(&self) -> &TargetResolver {
        &self.resolver
    }
}

/// A reduction plus the envelopes its confirmation cards will authorize (§15.3): a
/// click resumes exactly the commands that were reviewed.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ReducedTurn {
    /// The reduction plan.
    pub plan: ReductionPlan,
    /// Batches that only execute once a confirmation card is answered.
    pub pending: Vec<CommandBatch<serde_json::Value>>,
}

impl ReducedTurn {
    /// Every envelope waiting behind a confirmation, in batch order.
    #[must_use]
    pub fn pending_envelopes(&self) -> Vec<&CommandEnvelope<serde_json::Value>> {
        self.pending
            .iter()
            .flat_map(|batch| batch.envelopes.iter())
            .collect()
    }
}

impl DefaultTurnReducer {
    /// Reduces one turn, keeping the envelopes its confirmation cards will authorize.
    ///
    /// # Errors
    ///
    /// Whatever [`TurnReducer::reduce`] returns.
    pub fn reduce_turn(
        &self,
        input: &TurnInput,
        understanding: &Understanding,
        context: &ReductionContext,
    ) -> Result<ReducedTurn, ReductionError> {
        let (plan, pending) = Session::new(self, input, context).run(understanding)?;
        Ok(ReducedTurn { plan, pending })
    }
}

impl TurnReducer for DefaultTurnReducer {
    fn reduce(
        &self,
        input: &TurnInput,
        understanding: &Understanding,
        context: &ReductionContext,
    ) -> Result<ReductionPlan, ReductionError> {
        Session::new(self, input, context)
            .run(understanding)
            .map(|(plan, _)| plan)
    }
}

/// Everything one reduction accumulates.
struct Session<'a> {
    reducer: &'a DefaultTurnReducer,
    input: &'a TurnInput,
    context: &'a ReductionContext,
    acts: Vec<UnderstoodAct>,
    constraints: Vec<ConstraintKind>,
    evidence_digests: Vec<Digest>,
    outcomes: Vec<Option<TargetOutcome>>,
    results: Vec<Option<PlannedActResult>>,
    batches: IndexMap<(CaseKey, String), CommandBatch<serde_json::Value>>,
    pending: IndexMap<(CaseKey, String), CommandBatch<serde_json::Value>>,
    decisions: Vec<PolicyDecision>,
    specs: Vec<InteractionSpec>,
    notices: IndexMap<&'static str, ServerNotice>,
    refusals: Vec<NarratableFact>,
    changed_nothing: Vec<NarratableFact>,
    awaiting_confirmation: Vec<NarratableFact>,
    /// Cases this turn opens, in act order, so a second opening is asked about the first.
    minted: Vec<CaseRef>,
    /// The case each opening act opens, for the acts that apply to it.
    opened_by: BTreeMap<ActId, CaseRef>,
    /// The state the acts so far leave each case this turn opens in, when its workflow tells.
    unborn: BTreeMap<CaseKey, serde_json::Value>,
    applied: Vec<ConstraintKind>,
    command_count: usize,
}

impl<'a> Session<'a> {
    fn new(
        reducer: &'a DefaultTurnReducer,
        input: &'a TurnInput,
        context: &'a ReductionContext,
    ) -> Self {
        Self {
            reducer,
            input,
            context,
            acts: Vec::new(),
            constraints: Vec::new(),
            evidence_digests: Vec::new(),
            outcomes: Vec::new(),
            results: Vec::new(),
            batches: IndexMap::new(),
            pending: IndexMap::new(),
            decisions: Vec::new(),
            specs: Vec::new(),
            notices: IndexMap::new(),
            refusals: Vec::new(),
            changed_nothing: Vec::new(),
            awaiting_confirmation: Vec::new(),
            minted: Vec::new(),
            opened_by: BTreeMap::new(),
            unborn: BTreeMap::new(),
            applied: Vec::new(),
            command_count: 0,
        }
    }

    fn turn_id(&self) -> TurnId {
        self.input.turn_id
    }

    /// The user's words a range covers, as the turn carries them.
    fn words(&self, range: WordRange) -> &str {
        self.input
            .text
            .as_deref()
            .and_then(|text| text.get(range.start..range.end))
            .unwrap_or_default()
    }

    fn run(
        mut self,
        understanding: &Understanding,
    ) -> Result<(ReductionPlan, Vec<CommandBatch<serde_json::Value>>), ReductionError> {
        self.context.limits.enforce(understanding)?;
        self.acts = understanding.acts.clone();
        self.constraints = understanding.constraints.iter().map(|c| c.kind).collect();
        self.results = vec![None; self.acts.len()];
        self.outcomes = vec![None; self.acts.len()];
        self.evidence_digests = self
            .acts
            .iter()
            .map(|act| {
                canonical_digest(&(&act.words, &act.arguments, &act.action))
                    .map_err(|_| ReductionError::Hash)
            })
            .collect::<Result<_, _>>()?;

        for index in 0..self.acts.len() {
            self.plan_act(index, understanding)?;
        }
        self.attach_dependents();
        if self.command_count > self.reducer.execution.max_commands_per_turn {
            return Err(ReductionError::CommandBudgetExceeded {
                limit: self.reducer.execution.max_commands_per_turn,
                actual: self.command_count,
            });
        }
        self.add_partial_result_notice();
        self.note_not_understood(understanding);

        let answer_tasks = self.answer_tasks(understanding);
        let ids: Vec<ActId> = self.acts.iter().map(|act| act.id).collect();
        let acts = self.planned_acts()?;
        let plan = ReductionPlan {
            turn_id: self.turn_id(),
            acts,
            batches: self.batches.into_values().collect(),
            policy_decisions: self.decisions,
            answer_tasks,
            superseded_operations: understanding
                .superseded
                .iter()
                .filter_map(|superseded| match &superseded.action {
                    ActAction::Apply { operation } => Some(operation.clone()),
                    ActAction::Start { .. } => None,
                })
                .collect(),
            refusals: self.refusals,
            changed_nothing: self.changed_nothing,
            awaiting_confirmation: self.awaiting_confirmation,
            constraints_applied: self.applied,
            notices: self.notices.into_values().collect(),
            pre_execution_interactions: self.specs,
            plan_hash: Digest::of_bytes(b""),
        }
        .with_hash()
        .map_err(|_| ReductionError::Hash)?;
        plan.validate(&ids)?;
        Ok((plan, self.pending.into_values().collect()))
    }

    fn planned_acts(&mut self) -> Result<Vec<PlannedAct>, ReductionError> {
        let mut planned = Vec::with_capacity(self.acts.len());
        for (index, act) in self.acts.iter().enumerate() {
            let result =
                self.results[index]
                    .clone()
                    .ok_or_else(|| ReductionError::InconsistentPlan {
                        detail: format!("act {} left without a result", act.id),
                    })?;
            planned.push(PlannedAct {
                act: act.clone(),
                target: self.outcomes[index]
                    .as_ref()
                    .and_then(TargetOutcome::resolution),
                result,
            });
        }
        Ok(planned)
    }

    fn spec(&self, act: &UnderstoodAct) -> Option<&'a OperationSpec> {
        match &act.action {
            ActAction::Apply { operation } => self.context.operations.get(operation),
            ActAction::Start { .. } => None,
        }
    }

    fn operation_name(act: &UnderstoodAct) -> String {
        match &act.action {
            ActAction::Apply { operation } => operation.as_str().to_owned(),
            ActAction::Start { workflow } => format!("{workflow}.start"),
        }
    }
}

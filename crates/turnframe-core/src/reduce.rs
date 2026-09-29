//! The whole-turn reduction contract (spec §13).
//!
//! The reducer is the deterministic decision engine: it takes the turn's
//! [`Understanding`], the projected views, the active interactions, the target map and
//! the policy snapshot, and produces a [`ReductionPlan`] that gives **every act an
//! explicit result** (I11) and groups executable commands into batches. It performs no
//! I/O and has no side effects.
//!
//! # Same-turn precedence (normative default, spec §13.2)
//!
//! 1. A correction or cancellation supersedes the act it names; understanding links
//!    them, and the reducer never guesses a correction from a repeated operation.
//! 2. `DoNotSubmit` blocks every submission act in the turn.
//! 3. A question never becomes an action.
//! 4. An ambiguous target blocks only the acts that depend on it.
//! 5. Independent questions remain answerable.
//! 6. A click binds more strongly than a typed answer to the same card.
//! 7. A high-risk card is never resolved from typed text.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use chrono::{DateTime, Utc};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use crate::case::{CaseKey, CaseRef};
use crate::command::{AtomicityScope, CommandBatch, RiskClass, origin_satisfies};
use crate::error::{DomainRejection, ReductionError};
use crate::flow::{DomainEnumeration, ErasedWorkflowView};
use crate::hash::{Digest, HashError, canonical_digest};
use crate::ids::{
    BatchId, CommandId, InteractionId, OperationKey, OptionId, QuestionId, TurnId, WorkflowKey,
};
use crate::interaction::{InteractionKind, InteractionSpec, TextResolutionPolicy};
use crate::operation::OperationCatalog;
use crate::plan::AnswerBasis;
use crate::plan::limits::PlanLimits;
use crate::policy::{PolicyDecision, PolicySnapshot};
use crate::response::{NarratableFact, ServerNotice};
use crate::target::{TargetResolution, TargetTokenMap};
use crate::turn::TurnInput;
use crate::understanding::{
    ActAction, ActId, ConstraintKind, Understanding, UnderstoodAct, UnitId,
};

/// The pure whole-turn reducer (spec §13).
pub trait TurnReducer: Send + Sync {
    /// Reduces one understood turn into an execution plan. Must be deterministic for
    /// the same inputs and must not perform I/O.
    ///
    /// # Errors
    ///
    /// A [`ReductionError`] for an understanding over the limits or a plan that fails
    /// its own consistency checks.
    fn reduce(
        &self,
        input: &TurnInput,
        understanding: &Understanding,
        context: &ReductionContext,
    ) -> Result<ReductionPlan, ReductionError>;
}

/// What the reducer needs to know about an active interaction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveInteractionSummary {
    /// The interaction.
    pub interaction_id: InteractionId,
    /// Case and bound revision.
    pub case_ref: CaseRef,
    /// Shape.
    pub kind: InteractionKind,
    /// Whether it owns unqualified answers for the case.
    pub blocking: bool,
    /// Stored option ids (for validating interpreted options).
    pub option_ids: Vec<OptionId>,
    /// Whether typed text may resolve it.
    pub text_resolution: TextResolutionPolicy,
    /// Highest risk class an answer authorizes, so the reducer can apply rule 8
    /// without loading the journaled commands. Conservative by default.
    #[serde(default = "RiskClass::conservative")]
    pub confirms_risk: RiskClass,
    /// Hash of the payload the user saw.
    pub payload_hash: Digest,
}

impl ActiveInteractionSummary {
    /// Returns `true` when typed text may resolve this card at all: the stored
    /// policy allows it **and** what it confirms is low risk (spec §13.2 rule
    /// 8, §15.7).
    #[must_use]
    pub fn accepts_text_resolution(&self) -> bool {
        self.text_resolution != TextResolutionPolicy::Never
            && !self.confirms_risk.needs_trusted_origin()
            && !self.kind.authorizes_commands()
    }
}

/// Everything the reducer sees besides the plan and the turn.
#[derive(Debug, Clone)]
pub struct ReductionContext {
    /// Projected views of every loaded case.
    pub views: IndexMap<CaseKey, ErasedWorkflowView>,
    /// Active interactions in the conversation.
    pub active_interactions: Vec<ActiveInteractionSummary>,
    /// Tokens issued for this turn.
    pub target_map: TargetTokenMap,
    /// Operations offered for this turn.
    pub operations: OperationCatalog,
    /// Policy configuration.
    pub policy: PolicySnapshot,
    /// Plan limits.
    pub limits: PlanLimits,
    /// The clock value the reducer must use (it must not read the clock).
    pub now: DateTime<Utc>,
    /// The cases every write on which has to pass through a click.
    ///
    /// Set by the application, per case, through its case directory. A command
    /// on one of these that would otherwise have run silently gets the
    /// confirmation raised instead, on a card that names the case — the same
    /// thing [`ConstraintKind::AskBeforeApplying`] does when a user asks for it. Empty is the ordinary case and changes
    /// nothing.
    pub confirm_every_write: BTreeSet<CaseKey>,
    /// The cases that are in view only because the actor may reach them.
    ///
    /// Set by the application, per case, through its case directory: a record
    /// another conversation is filling in, one left open in a thread that no
    /// longer exists, anything the turn has in view because it is reachable
    /// rather than because this turn is about it.
    ///
    /// What it does is keep such a case from acting as a subject on its own.
    /// It is not hidden and not unreachable — it stays in the catalog under its
    /// own names, and the moment an act of this turn lands on it it is a
    /// subject like any other, which is the only way a draft left in a deleted
    /// conversation can ever be finished. What it stops is the three things a
    /// case does merely by being in the room: holding the door against a
    /// second case beside it, briefing the writer in the imperative, and
    /// putting its own outstanding fields in front of a reader answering about
    /// something else.
    ///
    /// Empty is the ordinary case and changes nothing.
    pub subject_only_when_named: BTreeSet<CaseKey>,
}

impl ReductionContext {
    /// View of a case, if loaded.
    #[must_use]
    pub fn view_for(&self, key: &CaseKey) -> Option<&ErasedWorkflowView> {
        self.views.get(key)
    }

    /// Whether every write on `key` has to pass through a click.
    #[must_use]
    pub fn confirms_every_write(&self, key: &CaseKey) -> bool {
        self.confirm_every_write.contains(key)
    }

    /// Whether `key` is in view only because the actor may reach it, so it is
    /// a subject of this turn only if the turn names it.
    ///
    /// See [`Self::subject_only_when_named`].
    #[must_use]
    pub fn is_subject_only_when_named(&self, key: &CaseKey) -> bool {
        self.subject_only_when_named.contains(key)
    }

    /// The active blocking interaction of a case, if any.
    #[must_use]
    pub fn blocking_interaction_for(&self, key: &CaseKey) -> Option<&ActiveInteractionSummary> {
        self.active_interactions
            .iter()
            .find(|i| i.blocking && i.case_ref.key() == *key)
    }
}

/// Reference to a command inside a reduction plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CommandRef {
    /// The batch.
    pub batch_id: BatchId,
    /// The command.
    pub command_id: CommandId,
}

impl fmt::Display for CommandRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.batch_id, self.command_id)
    }
}

/// The explicit result of one act (spec §13.3, I11).
///
/// New results are expected as the reducer learns to say more, so downstream
/// matches need a wildcard arm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PlannedActResult {
    /// Compiled into commands that may execute this turn.
    ReadyToExecute {
        /// The commands.
        command_refs: Vec<CommandRef>,
    },
    /// Compiled, but policy requires a confirmation first.
    AwaitingConfirmation {
        /// The interaction to create.
        interaction_spec: InteractionSpec,
    },
    /// The target or intent is ambiguous.
    NeedsClarification {
        /// The interaction to create.
        interaction_spec: InteractionSpec,
    },
    /// Rejected deterministically. Carries the whole [`DomainRejection`],
    /// including the structured `details` the UI needs, instead of re-declaring
    /// its code.
    Rejected {
        /// Why the domain refused.
        rejection: DomainRejection,
    },
    /// A later act in the same turn cancelled or corrected it.
    SupersededByCorrection,
    /// Valid but changes nothing (already in the requested state).
    NoChange,
    /// Arguments are missing, unstated or refused; the user is asked, nothing runs.
    NeedsValue {
        /// The arguments to ask for.
        arguments: Vec<String>,
        /// The domain's explanation, when it refused a value.
        reason: Option<String>,
    },
    /// Another unit aimed at the same record was not understood.
    Held {
        /// That unit.
        because: UnitId,
    },
    /// Waits for an earlier act of the turn that is itself waiting for a click.
    AwaitingPrerequisite {
        /// The act it waits for.
        act: ActId,
    },
}

impl PlannedActResult {
    /// Snake-case variant name, for a record or a report.
    ///
    /// The distinction a measurement needs is here and nowhere else: only
    /// `rejected` is the structure refusing a reading. `no_change` is a valid
    /// act on a record already in the requested state, and
    /// `awaiting_confirmation` is one waiting for a person — both journal no
    /// commands, and counting either as a refusal reports the design working
    /// as the model failing.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::ReadyToExecute { .. } => "ready_to_execute",
            Self::AwaitingConfirmation { .. } => "awaiting_confirmation",
            Self::NeedsClarification { .. } => "needs_clarification",
            Self::Rejected { .. } => "rejected",
            Self::SupersededByCorrection => "superseded_by_correction",
            Self::NoChange => "no_change",
            Self::NeedsValue { .. } => "needs_value",
            Self::Held { .. } => "held",
            Self::AwaitingPrerequisite { .. } => "awaiting_prerequisite",
        }
    }
}

impl From<DomainRejection> for PlannedActResult {
    fn from(rejection: DomainRejection) -> Self {
        Self::Rejected { rejection }
    }
}

/// One act with its resolution and result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedAct {
    /// The act as understood.
    pub act: UnderstoodAct,
    /// Target resolution, when the act has a target.
    pub target: Option<TargetResolution>,
    /// The result.
    pub result: PlannedActResult,
}

/// Which sources an answer must rest on (spec §19.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourcePolicy {
    /// Any available source.
    AnySource,
    /// Only authoritative sources (case state, approved knowledge).
    AuthoritativeOnly,
    /// Sources must be cited.
    RequireCitations,
    /// No retrieval; answer from state and general knowledge only.
    NoRetrieval,
}

/// A question to answer, with its explicit state basis (spec §19.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnswerTask {
    /// Stable id within the turn.
    pub question_id: QuestionId,
    /// The question.
    pub question: String,
    /// State basis the reducer decided (it may override the model's preference).
    pub basis: AnswerBasis,
    /// Cases the question is about.
    pub case_refs: Vec<CaseRef>,
    /// Reference to a proposed diff when `basis` is `ProposedState`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proposed_diff_ref: Option<String>,
    /// Source requirements.
    pub required_sources: SourcePolicy,
    /// Where in the normalized message the question's words are.
    ///
    /// The question's text is derived from that span, and carrying the span
    /// too is what lets composition keep those words away from the stage that
    /// must not answer them. `None` for a task rebuilt from a replay record,
    /// where the message is not at hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked_at: Option<TextSpan>,
    /// Complete value sets a workflow declared for what this question is
    /// about.
    ///
    /// Non-empty means the deterministic layer already holds the answer, so
    /// composition settles the question from these rather than asking a model
    /// to describe a set it would have to remember.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enumerations: Vec<DomainEnumeration>,
    /// What the user can do now, for a question about that.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities: Vec<Capability>,
    /// Whether it follows up the assistant's last message.
    #[serde(default)]
    pub continues_previous: bool,
}

/// One thing the user can do now: an operation on offer, in its workflow's words.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability {
    /// The workflow.
    pub workflow: WorkflowKey,
    /// The operation.
    pub operation: OperationKey,
    /// What it does, as the workflow says it.
    pub summary: String,
}

/// A range of the normalized user message, in bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextSpan {
    /// First byte of the range.
    pub start_byte: usize,
    /// One past its last byte.
    pub end_byte: usize,
}

/// The reducer's output (spec §13).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReductionPlan {
    /// The turn.
    pub turn_id: TurnId,
    /// One entry per input act, by index (I11).
    pub acts: Vec<PlannedAct>,
    /// Command batches ready for execution, grouped by atomicity scope.
    /// Commands are erased to JSON; the registry converts them at the boundary.
    pub batches: Vec<CommandBatch<serde_json::Value>>,
    /// Policy decisions for every command.
    pub policy_decisions: Vec<PolicyDecision>,
    /// Questions to answer.
    pub answer_tasks: Vec<AnswerTask>,
    /// Constraints that were applied.
    pub constraints_applied: Vec<ConstraintKind>,
    /// Notices to show (e.g. "Nothing has been submitted").
    pub notices: Vec<ServerNotice>,
    /// Interactions to persist **before** any command executes (clarifications
    /// and confirmations). Deduplicated by `InteractionSpec::key`.
    pub pre_execution_interactions: Vec<InteractionSpec>,
    /// Digest of everything above (see [`Self::compute_hash`]).
    pub plan_hash: Digest,
    /// Operations of acts a correction or a cancel in the same message replaced.
    ///
    /// Supersession is the one reduction rule that makes a turn do less than
    /// its plan said, and until it was counted a message carrying four data
    /// that produced one command was only discoverable by reading the
    /// conversation that followed. One entry per dropped act, in plan order.
    #[serde(default)]
    pub superseded_operations: Vec<OperationKey>,
    /// Domain refusals, as facts the narration stage may rest on.
    ///
    /// The notice beside them tells the user deterministically; these tell the
    /// narrator, so its prose does not ask for something else as though the
    /// refusal had not happened.
    #[serde(default)]
    pub refusals: Vec<NarratableFact>,
    /// Acts that were accepted and changed nothing, as facts the narration
    /// stage may rest on.
    ///
    /// Beside [`Self::refusals`] and not inside it, because the two are
    /// different outcomes and one of them is counted: the runtime raises its
    /// refusal signal once per entry there, and a no-op filed among them would
    /// be a refusal in every dashboard that reads it. They travel together only
    /// at the point where both become facts for the writer.
    #[serde(default)]
    pub changed_nothing: Vec<NarratableFact>,
    /// Acts this turn prepared and held behind a confirmation card, as facts
    /// the narration stage may rest on.
    ///
    /// Separate from [`Self::refusals`] and not folded into it, because these
    /// are not refusals: that list is counted as
    /// [`Signal::ActRefused`](crate::observe::Signal::ActRefused), and an act
    /// waiting for a click is one the server intends to run. See
    /// [`NarratableFact::ActAwaitingConfirmation`].
    #[serde(default)]
    pub awaiting_confirmation: Vec<NarratableFact>,
}

#[derive(Serialize)]
struct PlanHashInput<'a> {
    turn_id: &'a TurnId,
    acts: &'a [PlannedAct],
    batches: &'a [CommandBatch<serde_json::Value>],
    policy_decisions: &'a [PolicyDecision],
    answer_tasks: &'a [AnswerTask],
    constraints_applied: &'a [ConstraintKind],
    notices: &'a [ServerNotice],
    pre_execution_interactions: &'a [InteractionSpec],
}

impl ReductionPlan {
    /// Computes the digest of every field except `plan_hash`.
    pub fn compute_hash(&self) -> Result<Digest, HashError> {
        canonical_digest(&PlanHashInput {
            turn_id: &self.turn_id,
            acts: &self.acts,
            batches: &self.batches,
            policy_decisions: &self.policy_decisions,
            answer_tasks: &self.answer_tasks,
            constraints_applied: &self.constraints_applied,
            notices: &self.notices,
            pre_execution_interactions: &self.pre_execution_interactions,
        })
    }

    /// Sets `plan_hash` from the current content.
    pub fn with_hash(mut self) -> Result<Self, HashError> {
        self.plan_hash = self.compute_hash()?;
        Ok(self)
    }

    /// Returns `true` when `plan_hash` matches the content.
    pub fn verify_hash(&self) -> Result<bool, HashError> {
        Ok(self.compute_hash()? == self.plan_hash)
    }

    /// Structural consistency checks.
    ///
    /// Everything here is a property a correct reducer already has; the point
    /// is that a plan which fails one of them must never reach execution, so a
    /// reducer bug becomes a refused turn instead of an unauthorized command:
    ///
    /// * every act of the understanding appears exactly once (I11);
    /// * every `ReadyToExecute` command reference points at a batch command;
    /// * a `ReadyToExecute` act resolved its target exactly, unless it starts a
    ///   workflow or picks a target (spec §12.2, I8), and its commands run on
    ///   the case it resolved to;
    /// * every batched command has exactly one [`PolicyDecision`], that
    ///   decision allows it, and its origin satisfies the policy the decision
    ///   recorded ([`origin_satisfies`], I9, I12);
    /// * no refused decision names a command that is nevertheless batched;
    /// * every spec embedded in an act result is listed in
    ///   `pre_execution_interactions` (by key);
    /// * `PerCase` batches target a single case.
    pub fn validate(&self, understood: &[ActId]) -> Result<(), ReductionError> {
        let mut seen = BTreeSet::new();
        for planned in &self.acts {
            if !understood.contains(&planned.act.id) || !seen.insert(planned.act.id) {
                return Err(ReductionError::InconsistentPlan {
                    detail: format!("act {} unknown or duplicated", planned.act.id),
                });
            }
        }
        if seen.len() != understood.len() {
            return Err(ReductionError::InconsistentPlan {
                detail: format!("{} of {} acts have a result", seen.len(), understood.len()),
            });
        }
        let mut commands: BTreeMap<CommandRef, &CaseRef> = BTreeMap::new();
        for batch in &self.batches {
            for envelope in &batch.envelopes {
                commands.insert(
                    CommandRef {
                        batch_id: batch.batch_id,
                        command_id: envelope.command_id,
                    },
                    &envelope.case_ref,
                );
            }
        }
        let command_refs: BTreeSet<CommandRef> = commands.keys().copied().collect();
        let spec_keys: BTreeSet<&str> = self
            .pre_execution_interactions
            .iter()
            .map(|s| s.key.as_str())
            .collect();
        for planned in &self.acts {
            match &planned.result {
                PlannedActResult::ReadyToExecute { command_refs: refs } => {
                    if let Some(missing) = refs.iter().find(|r| !command_refs.contains(r)) {
                        return Err(ReductionError::InconsistentPlan {
                            detail: format!("dangling command reference {missing}"),
                        });
                    }
                    let needs_exact_target = !matches!(planned.act.action, ActAction::Start { .. });
                    let resolved = planned.target.as_ref().and_then(TargetResolution::exact);
                    if needs_exact_target && resolved.is_none() {
                        return Err(ReductionError::InconsistentPlan {
                            detail: format!(
                                "act {} executes without an exact target",
                                planned.act.id
                            ),
                        });
                    }
                    if let Some(resolved) = resolved
                        && let Some(elsewhere) = refs
                            .iter()
                            .filter_map(|r| commands.get(r))
                            .find(|case_ref| !case_ref.same_case(resolved))
                    {
                        return Err(ReductionError::InconsistentPlan {
                            detail: format!(
                                "act {} resolved to {}/{} but a command targets {}/{}",
                                planned.act.id,
                                resolved.workflow,
                                resolved.case_id,
                                elsewhere.workflow,
                                elsewhere.case_id
                            ),
                        });
                    }
                }
                PlannedActResult::AwaitingConfirmation { interaction_spec }
                | PlannedActResult::NeedsClarification { interaction_spec } => {
                    if !spec_keys.contains(interaction_spec.key.as_str()) {
                        return Err(ReductionError::InconsistentPlan {
                            detail: format!(
                                "interaction spec {} not listed for creation",
                                interaction_spec.key
                            ),
                        });
                    }
                }
                PlannedActResult::Rejected { .. }
                | PlannedActResult::SupersededByCorrection
                | PlannedActResult::NoChange
                | PlannedActResult::NeedsValue { .. }
                | PlannedActResult::Held { .. }
                | PlannedActResult::AwaitingPrerequisite { .. } => {}
            }
        }
        if let Some(bad) = self
            .batches
            .iter()
            .find(|b| matches!(b.scope, AtomicityScope::PerCase) && !b.is_single_case())
        {
            return Err(ReductionError::InconsistentPlan {
                detail: format!("per-case batch {} spans several cases", bad.batch_id),
            });
        }
        self.validate_policy_coverage(&command_refs)
    }

    /// Every batched command is policed, allowed and authorized by its own
    /// origin. See [`Self::validate`].
    fn validate_policy_coverage(
        &self,
        command_refs: &BTreeSet<CommandRef>,
    ) -> Result<(), ReductionError> {
        let mut decisions: BTreeMap<CommandRef, &PolicyDecision> = BTreeMap::new();
        for decision in &self.policy_decisions {
            if decisions.insert(decision.command_ref, decision).is_some() {
                return Err(ReductionError::InconsistentPlan {
                    detail: format!(
                        "command {} has several policy decisions",
                        decision.command_ref
                    ),
                });
            }
            if !decision.allowed && command_refs.contains(&decision.command_ref) {
                return Err(ReductionError::InconsistentPlan {
                    detail: format!("refused command {} is batched", decision.command_ref),
                });
            }
        }
        for batch in &self.batches {
            for envelope in &batch.envelopes {
                let command_ref = CommandRef {
                    batch_id: batch.batch_id,
                    command_id: envelope.command_id,
                };
                let Some(decision) = decisions.get(&command_ref) else {
                    return Err(ReductionError::InconsistentPlan {
                        detail: format!("command {command_ref} has no policy decision"),
                    });
                };
                if !origin_satisfies(&envelope.origin, &decision.policy) {
                    return Err(ReductionError::InconsistentPlan {
                        detail: format!("command {command_ref} has an origin its policy refuses"),
                    });
                }
            }
        }
        Ok(())
    }

    /// All command references in batch order.
    #[must_use]
    pub fn command_refs(&self) -> Vec<CommandRef> {
        self.batches
            .iter()
            .flat_map(|b| {
                b.envelopes.iter().map(move |e| CommandRef {
                    batch_id: b.batch_id,
                    command_id: e.command_id,
                })
            })
            .collect()
    }

    /// Returns `true` when nothing will execute this turn.
    #[must_use]
    pub fn has_no_effects(&self) -> bool {
        self.batches.iter().all(CommandBatch::is_empty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::case::CaseRef;
    use crate::command::{
        CommandEnvelope, CommandOrigin, CommandPolicy, IdempotencyKey, ResolutionChannel,
    };
    use crate::ids::{AccountId, CaseRevision, InteractionId, OperationKey, WorkflowKey};
    use crate::interaction::{
        ActionClass, InteractionOption, InteractionPayload, StoredInteractionAction,
    };
    use crate::policy::reason;
    use crate::turn::ActorContext;

    fn id(index: usize) -> ActId {
        ActId::new(UnitId(u16::try_from(index + 1).unwrap()), 1)
    }

    fn ids(count: usize) -> Vec<ActId> {
        (0..count).map(id).collect()
    }

    fn understood(index: usize, action: ActAction) -> UnderstoodAct {
        UnderstoodAct {
            id: id(index),
            action,
            target: crate::understanding::ActTarget::Card,
            arguments: BTreeMap::new(),
            words: crate::understanding::WordRange {
                first: 0,
                last: 0,
                start: 0,
                end: 2,
            },
            depends_on: vec![],
            status: crate::understanding::ActStatus::Ready,
        }
    }

    fn act(index: usize, result: PlannedActResult) -> PlannedAct {
        PlannedAct {
            act: understood(
                index,
                ActAction::Start {
                    workflow: WorkflowKey::from("w"),
                },
            ),
            target: None,
            result,
        }
    }

    fn apply_act(
        index: usize,
        target: Option<TargetResolution>,
        result: PlannedActResult,
    ) -> PlannedAct {
        PlannedAct {
            act: understood(
                index,
                ActAction::Apply {
                    operation: OperationKey::from("w.op"),
                },
            ),
            target,
            result,
        }
    }

    fn case() -> CaseRef {
        CaseRef::new("w", "c1", CaseRevision(1))
    }

    fn plan(acts: Vec<PlannedAct>) -> ReductionPlan {
        ReductionPlan {
            turn_id: TurnId::nil(),
            acts,
            batches: vec![],
            policy_decisions: vec![],
            answer_tasks: vec![],
            superseded_operations: vec![],
            refusals: vec![],
            changed_nothing: vec![],
            awaiting_confirmation: vec![],
            constraints_applied: vec![],
            notices: vec![],
            pre_execution_interactions: vec![],
            plan_hash: Digest::of_bytes(b""),
        }
    }

    fn envelope(
        command_id: CommandId,
        case_ref: CaseRef,
        origin: CommandOrigin,
    ) -> CommandEnvelope<serde_json::Value> {
        CommandEnvelope {
            command_id,
            turn_id: TurnId::nil(),
            actor: ActorContext::new("acct", "u1"),
            case_ref,
            idempotency_key: IdempotencyKey::new("k"),
            origin,
            command: serde_json::json!({"do": true}),
        }
    }

    fn confirmed_origin() -> CommandOrigin {
        CommandOrigin::ConfirmedInteraction {
            interaction_id: InteractionId::nil(),
            payload_hash: Digest::of_bytes(b"p"),
            interaction_kind: InteractionKind::ConfirmCommand,
            action_class: ActionClass::ConfirmsCommands,
            channel: ResolutionChannel::Click,
        }
    }

    fn direct_origin() -> CommandOrigin {
        CommandOrigin::DirectSafeUserAct {
            evidence_digest: Digest::of_bytes(b"e"),
        }
    }

    fn decision(command_ref: CommandRef, policy: CommandPolicy, allowed: bool) -> PolicyDecision {
        PolicyDecision {
            command_ref,
            policy,
            requires_interaction: None,
            allowed,
            reason_key: if allowed {
                reason::ALLOWED.to_owned()
            } else {
                reason::CONFIRMATION_REQUIRED.to_owned()
            },
        }
    }

    /// A plan with one batched command, its act and its decision.
    fn executing_plan(
        origin: CommandOrigin,
        policy: CommandPolicy,
        allowed: bool,
    ) -> ReductionPlan {
        let batch_id = BatchId::derive(&TurnId::nil(), &case().key(), &AtomicityScope::PerCase);
        let command_id = CommandId::derive(&TurnId::nil(), id(0), 0);
        let command_ref = CommandRef {
            batch_id,
            command_id,
        };
        let mut p = plan(vec![apply_act(
            0,
            Some(TargetResolution::Exact { case_ref: case() }),
            PlannedActResult::ReadyToExecute {
                command_refs: vec![command_ref],
            },
        )]);
        p.batches = vec![CommandBatch {
            batch_id,
            scope: AtomicityScope::PerCase,
            envelopes: vec![envelope(command_id, case(), origin)],
        }];
        p.policy_decisions = vec![decision(command_ref, policy, allowed)];
        p
    }

    #[test]
    fn every_act_needs_a_result() {
        let p = plan(vec![act(0, PlannedActResult::NoChange)]);
        assert!(p.validate(&ids(1)).is_ok());
        assert!(p.validate(&ids(2)).is_err());
        let dup = plan(vec![
            act(0, PlannedActResult::NoChange),
            act(0, PlannedActResult::NoChange),
        ]);
        assert!(dup.validate(&ids(2)).is_err());
    }

    #[test]
    fn dangling_refs_are_detected() {
        let p = plan(vec![act(
            0,
            PlannedActResult::ReadyToExecute {
                command_refs: vec![CommandRef {
                    batch_id: BatchId::nil(),
                    command_id: CommandId::nil(),
                }],
            },
        )]);
        assert!(matches!(
            p.validate(&ids(1)),
            Err(ReductionError::InconsistentPlan { .. })
        ));
    }

    #[test]
    fn an_executing_act_must_have_resolved_its_target_exactly() {
        let ok = executing_plan(confirmed_origin(), CommandPolicy::conservative(), true);
        assert_eq!(ok.validate(&ids(1)), Ok(()));
        for target in [
            None,
            Some(TargetResolution::Missing),
            Some(TargetResolution::Ambiguous { candidates: vec![] }),
            Some(TargetResolution::Stale {
                case_ref: case(),
                current_revision: CaseRevision(2),
            }),
        ] {
            let mut p = ok.clone();
            p.acts[0].target = target;
            assert!(
                matches!(
                    p.validate(&ids(1)),
                    Err(ReductionError::InconsistentPlan { .. })
                ),
                "an ambiguous or missing target may not execute (I8)"
            );
        }
        // Starting a workflow has no target to resolve.
        let mut start = plan(vec![act(
            0,
            PlannedActResult::ReadyToExecute {
                command_refs: vec![],
            },
        )]);
        start.policy_decisions = vec![];
        assert_eq!(start.validate(&ids(1)), Ok(()));
    }

    #[test]
    fn every_batched_command_is_policed_allowed_and_authorized() {
        let mut no_decision =
            executing_plan(confirmed_origin(), CommandPolicy::conservative(), true);
        no_decision.policy_decisions.clear();
        assert!(matches!(
            no_decision.validate(&ids(1)),
            Err(ReductionError::InconsistentPlan { .. })
        ));

        let refused = executing_plan(confirmed_origin(), CommandPolicy::conservative(), false);
        assert!(matches!(
            refused.validate(&ids(1)),
            Err(ReductionError::InconsistentPlan { .. })
        ));

        // The decision says "allowed" but the envelope carries an origin the
        // recorded policy refuses: the plan is not trustworthy.
        let lying = executing_plan(direct_origin(), CommandPolicy::conservative(), true);
        assert!(matches!(
            lying.validate(&ids(1)),
            Err(ReductionError::InconsistentPlan { .. })
        ));

        let mut twice = executing_plan(confirmed_origin(), CommandPolicy::conservative(), true);
        let duplicate = twice.policy_decisions[0].clone();
        twice.policy_decisions.push(duplicate);
        assert!(matches!(
            twice.validate(&ids(1)),
            Err(ReductionError::InconsistentPlan { .. })
        ));

        // A command that runs on another case than the one the act resolved to
        // is exactly the mix-up an exact target is supposed to prevent.
        let mut elsewhere = executing_plan(confirmed_origin(), CommandPolicy::conservative(), true);
        elsewhere.batches[0].envelopes[0].case_ref = CaseRef::new("w", "other", CaseRevision(1));
        assert!(matches!(
            elsewhere.validate(&ids(1)),
            Err(ReductionError::InconsistentPlan { .. })
        ));

        // A refused command that is *not* batched is exactly how a plan records
        // "this needs a confirmation first".
        let mut awaiting = executing_plan(direct_origin(), CommandPolicy::conservative(), false);
        awaiting.batches.clear();
        awaiting.acts[0].result = PlannedActResult::NoChange;
        assert_eq!(awaiting.validate(&ids(1)), Ok(()));
    }

    #[test]
    fn interaction_specs_must_be_listed_for_creation() {
        let spec = InteractionSpec::new(
            "confirm:acts[0]",
            case(),
            InteractionKind::ConfirmCommand,
            InteractionPayload::new("Send?")
                .with_option(InteractionOption::new(
                    "yes",
                    "Send",
                    StoredInteractionAction::ConfirmCommands {
                        command_refs: vec![],
                    },
                ))
                .with_option(InteractionOption::new(
                    "no",
                    "Cancel",
                    StoredInteractionAction::DeclineCommands,
                )),
        );
        for result in [
            PlannedActResult::AwaitingConfirmation {
                interaction_spec: spec.clone(),
            },
            PlannedActResult::NeedsClarification {
                interaction_spec: spec.clone(),
            },
        ] {
            let orphan = plan(vec![act(0, result.clone())]);
            assert!(
                matches!(
                    orphan.validate(&ids(1)),
                    Err(ReductionError::InconsistentPlan { .. })
                ),
                "a card nobody creates leaves the act unanswerable"
            );
            let mut listed = plan(vec![act(0, result)]);
            listed.pre_execution_interactions = vec![spec.clone()];
            assert_eq!(listed.validate(&ids(1)), Ok(()));
        }
    }

    #[test]
    fn per_case_batches_may_not_span_cases() {
        let mut p = executing_plan(confirmed_origin(), CommandPolicy::conservative(), true);
        let other = envelope(
            CommandId::derive(&TurnId::nil(), id(0), 1),
            CaseRef::new("w", "c2", CaseRevision(1)),
            confirmed_origin(),
        );
        let command_ref = CommandRef {
            batch_id: p.batches[0].batch_id,
            command_id: other.command_id,
        };
        p.batches[0].envelopes.push(other);
        p.policy_decisions
            .push(decision(command_ref, CommandPolicy::conservative(), true));
        assert!(matches!(
            p.validate(&ids(1)),
            Err(ReductionError::InconsistentPlan { .. })
        ));
    }

    #[test]
    fn hash_tracks_content() {
        let p = plan(vec![act(0, PlannedActResult::NoChange)])
            .with_hash()
            .unwrap();
        assert!(p.verify_hash().unwrap());
        let mut changed = p.clone();
        changed.acts[0].result = PlannedActResult::SupersededByCorrection;
        assert!(!changed.verify_hash().unwrap());
    }

    /// A reducer that only uses derived identifiers, as shipped reducers must.
    struct FixedReducer;

    impl TurnReducer for FixedReducer {
        fn reduce(
            &self,
            input: &TurnInput,
            understanding: &Understanding,
            _context: &ReductionContext,
        ) -> Result<ReductionPlan, ReductionError> {
            let turn_id = input.turn_id;
            let batch_id = BatchId::derive(&turn_id, &case().key(), &AtomicityScope::PerCase);
            let mut acts = Vec::new();
            let mut envelopes = Vec::new();
            let mut policy_decisions = Vec::new();
            for (index, act) in understanding.acts.iter().enumerate() {
                let command_id = CommandId::derive(&turn_id, act.id, 0);
                let command_ref = CommandRef {
                    batch_id,
                    command_id,
                };
                envelopes.push(envelope(command_id, case(), confirmed_origin()));
                policy_decisions.push(decision(command_ref, CommandPolicy::conservative(), true));
                acts.push(apply_act(
                    index,
                    Some(TargetResolution::Exact { case_ref: case() }),
                    PlannedActResult::ReadyToExecute {
                        command_refs: vec![command_ref],
                    },
                ));
            }
            ReductionPlan {
                turn_id,
                acts,
                batches: vec![CommandBatch {
                    batch_id,
                    scope: AtomicityScope::PerCase,
                    envelopes,
                }],
                policy_decisions,
                answer_tasks: vec![],
                superseded_operations: vec![],
                refusals: vec![],
                changed_nothing: vec![],
                awaiting_confirmation: vec![],
                constraints_applied: understanding.constraints.iter().map(|c| c.kind).collect(),
                notices: vec![],
                pre_execution_interactions: vec![],
                plan_hash: Digest::of_bytes(b""),
            }
            .with_hash()
            .map_err(|_| ReductionError::Hash)
        }
    }

    #[test]
    fn two_reductions_of_the_same_inputs_agree_on_the_plan_hash() {
        let input = TurnInput {
            turn_id: TurnId::nil(),
            conversation_id: crate::ids::ConversationId::nil(),
            actor: ActorContext::new("acct", "u1"),
            text: Some("do it".into()),
            interaction_response: None,
            attachments: vec![],
            origin: None,
            locale: crate::locale::Locale::from("it-IT"),
            effort: None,
        };
        let understanding = Understanding {
            acts: vec![understood(
                0,
                ActAction::Start {
                    workflow: WorkflowKey::from("w"),
                },
            )],
            ..Understanding::default()
        };
        let context = ReductionContext {
            views: IndexMap::new(),
            active_interactions: vec![],
            target_map: TargetTokenMap::new(AccountId::from("acct"), TurnId::nil()),
            operations: OperationCatalog::default(),
            policy: PolicySnapshot::conservative(),
            limits: PlanLimits::conservative(),
            now: chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
            confirm_every_write: BTreeSet::new(),
            subject_only_when_named: BTreeSet::new(),
        };
        let first = FixedReducer
            .reduce(&input, &understanding, &context)
            .unwrap();
        let second = FixedReducer
            .reduce(&input, &understanding, &context)
            .unwrap();
        assert_eq!(first.plan_hash, second.plan_hash);
        assert_eq!(first, second);
        assert_eq!(first.validate(&ids(1)), Ok(()));
        assert!(first.verify_hash().unwrap());
    }

    #[test]
    fn a_high_risk_card_never_accepts_typed_text() {
        let summary = ActiveInteractionSummary {
            interaction_id: InteractionId::nil(),
            case_ref: case(),
            kind: InteractionKind::SingleSelect,
            blocking: true,
            option_ids: vec![OptionId::from("a")],
            text_resolution: TextResolutionPolicy::ModelInterpretedLowRisk,
            confirms_risk: RiskClass::ReversibleLowRisk,
            payload_hash: Digest::of_bytes(b"p"),
        };
        assert!(summary.accepts_text_resolution());
        let risky = ActiveInteractionSummary {
            confirms_risk: RiskClass::Irreversible,
            ..summary.clone()
        };
        assert!(!risky.accepts_text_resolution());
        let confirming = ActiveInteractionSummary {
            kind: InteractionKind::ConfirmCommand,
            ..summary.clone()
        };
        assert!(!confirming.accepts_text_resolution());
        let never = ActiveInteractionSummary {
            text_resolution: TextResolutionPolicy::Never,
            ..summary
        };
        assert!(!never.accepts_text_resolution());
    }
}

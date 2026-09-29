//! Replay records and turn phases (spec §23.1, I20).
//!
//! A [`ReplayRecord`] holds enough to reconstruct why a turn produced its
//! commands and response: versions, revisions, the normalized plan and its
//! hash, target resolutions, policy decisions, command outcomes, event ids,
//! block ids, the outbox rows and reconciliation handles of its external
//! effects, and every provider attempt.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::case::CaseRef;
use crate::command::{CommandOrigin, IdempotencyKey};
use crate::error::RejectionCode;
use crate::hash::Digest;
use crate::ids::{
    AccountId, AttemptId, BlockId, CaseRevision, ConversationId, EventId, InteractionId, ModelKey,
    OutboxId, ProviderKey, TurnId, WorkflowKey, WorkflowVersion,
};
use crate::policy::PolicyDecision;
use crate::prompt::PromptRef;
use crate::reduce::CommandRef;
use crate::target::TargetResolution;
use crate::understanding::Understanding;

/// Persisted phase marker of a turn, for crash recovery (spec §23.1).
///
/// The pipeline grows phases, so downstream matches need a wildcard arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TurnPhase {
    /// Input accepted and persisted.
    Received,
    /// A plan was accepted.
    Interpreted,
    /// A reduction plan exists.
    Reduced,
    /// Commands are executing.
    Executing,
    /// Commands committed.
    Committed,
    /// Response blocks composed and persisted.
    Composed,
    /// Response delivered.
    Delivered,
    /// The turn failed.
    Failed,
}

impl TurnPhase {
    /// Returns `true` for `Delivered` and `Failed`.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Delivered | Self::Failed)
    }

    /// Returns `true` once commands may have executed; recovery must resume by
    /// idempotency key instead of re-interpreting (spec §23.1).
    #[must_use]
    pub fn effects_may_exist(self) -> bool {
        matches!(
            self,
            Self::Executing | Self::Committed | Self::Composed | Self::Delivered
        )
    }
}

/// Workflow version in force for a turn.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkflowVersionRecord {
    /// The workflow.
    pub key: WorkflowKey,
    /// Its version.
    pub version: WorkflowVersion,
}

/// Target resolution of one act.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetResolutionRecord {
    /// The act.
    pub act: crate::understanding::ActId,
    /// How it resolved.
    pub resolution: TargetResolution,
}

/// What happened to one command.
///
/// New outcomes appear as execution learns to say more, so downstream matches
/// need a wildcard arm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum CommandOutcome {
    /// Committed.
    Committed {
        /// Revision after the commit.
        new_revision: CaseRevision,
        /// Events produced.
        event_ids: Vec<EventId>,
    },
    /// The journal already had the key; the original outcome was returned.
    IdempotentReplay,
    /// The expected revision was stale.
    RevisionConflict {
        /// Revision found.
        current_revision: CaseRevision,
    },
    /// The domain rejected.
    Rejected {
        /// Code.
        code: RejectionCode,
    },
    /// Execution failed.
    Failed {
        /// Stable code.
        code: String,
    },
    /// An external effect has an unknown outcome.
    OutcomeUnknown {
        /// Attempt id for reconciliation.
        attempt_id: AttemptId,
    },
    /// Waiting for a confirmation interaction.
    AwaitingConfirmation {
        /// The interaction.
        interaction_id: InteractionId,
    },
}

/// One command and its outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandOutcomeRecord {
    /// The command.
    pub command_ref: CommandRef,
    /// Its idempotency key.
    pub idempotency_key: IdempotencyKey,
    /// Case and expected revision.
    pub case_ref: CaseRef,
    /// What authorized the command.
    ///
    /// Recorded so the audit answers "which interaction authorized this
    /// command" from the record alone, without reloading the batches that
    /// produced it (spec §26.4, I20). Absent on records written before the
    /// field existed, which is why it is optional rather than required.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<CommandOrigin>,
    /// The outcome.
    pub outcome: CommandOutcome,
}

/// How a provider attempt ended.
///
/// Routing strategies grow, so downstream matches need a wildcard arm.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ProviderAttemptOutcome {
    /// Usable output.
    Succeeded,
    /// Failed with a normalized code.
    Failed {
        /// Stable code.
        code: String,
    },
    /// Failed and routing moved to another candidate.
    FellBack {
        /// Stable code.
        code: String,
    },
    /// Cancelled.
    Cancelled,
}

/// One model call (spec §20.7: "record every provider attempt").
///
/// # Why `attempt` is a number and [`AttemptId`] is an identifier
///
/// A model call leaves nothing behind outside the process. When one fails or
/// times out, the only thing anyone needs to know is *which try it was* inside
/// a stage this record already identifies — the turn, the `purpose`, the
/// provider and the model — so a position in that sequence says everything, and
/// nothing else ever refers to it.
///
/// An external effect attempt is the opposite: it may have happened even though
/// the answer never arrived (spec §16.5), and settling it means naming that
/// exact attempt to a remote system. That is what [`AttemptId`] is for, why
/// [`CommandOutcome::OutcomeUnknown`] carries one, and why
/// [`ReplayRecord::reconciliation_attempt_ids`] lists them. The asymmetry is
/// the difference between counting retries and naming an effect, not an
/// oversight.
///
/// Note that this record is compared with [`PartialEq`] only: `temperature` is
/// a float, so `Eq` would be a promise about `NaN` that the type cannot keep.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderAttemptRecord {
    /// 1-based attempt number within the stage. A plain ordinal on purpose;
    /// see the type documentation.
    pub attempt: u32,
    /// Stage purpose (e.g. `"extract"`).
    pub purpose: String,
    /// Provider key.
    pub provider_key: ProviderKey,
    /// Model key.
    pub model_key: ModelKey,
    /// Stable request id sent to the provider.
    pub request_id: String,
    /// Prompt or template version, when known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_version: Option<String>,
    /// The exact prompt text this call ran under, when a prompt source supplied
    /// it (roadmap: "prompt management, with an optional Langfuse prompt
    /// source").
    ///
    /// `None` for a stage whose instructions are compiled into the library —
    /// which is every stage until an application configures a
    /// `turnframe-prompt` source — and also for a stage whose configured source
    /// failed and fell back to those built-in instructions. That is deliberate:
    /// the absence of a reference is the audit signal that the text was not the
    /// text the source was asked for.
    ///
    /// Unlike [`prompt_version`](Self::prompt_version), which is a bare label,
    /// this carries the digest of the text, so the record can be falsified
    /// rather than merely believed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_ref: Option<PromptRef>,
    /// Outcome.
    pub outcome: ProviderAttemptOutcome,
    /// Latency in milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    /// Input tokens, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    /// Output tokens, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// Sampling temperature the request carried, when the stage set one.
    ///
    /// `None` means the call left the provider's default in place, which is not
    /// the same as `Some(0.0)`. An observability bridge reports it as
    /// `gen_ai.request.temperature`; kept at the request's own `f32` precision
    /// so the audit value is the value that was sent, not a widened copy of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Finish reasons the provider reported for the response, in the order it
    /// reported them, verbatim.
    ///
    /// Empty when the attempt produced none — a transport failure, a
    /// cancellation, or a provider that does not report them. Providers spell
    /// these differently (`"stop"`, `"length"`, `"max_tokens"`, ...) and the
    /// strings are kept as received: normalizing them here would erase the
    /// distinction an audit is being read for. An observability bridge reports
    /// them as `gen_ai.response.finish_reasons`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub finish_reasons: Vec<String>,
}

/// One answer a model produced and a check refused whole.
///
/// A call can succeed and the turn still lose its answer, to a pointer outside the
/// message or a document of the wrong shape. The repair round usually recovers and
/// nobody counts the cost; when it does not, the turn does nothing and asks nothing,
/// and this is the only place that says why. Derived from [`ReplayRecord::tasks`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscardedAnswer {
    /// The task's purpose, such as `"extract"` or `"answer"`.
    pub purpose: String,
    /// Which refusal of its task this was, from 1.
    pub round: u32,
    /// Stable code of the failed check: the field to group by.
    pub code: String,
    /// The refusal in words, as the repair round was told it.
    pub reason: String,
}

/// Everything needed to replay a turn (I20).
///
/// Spec §26.1 asks that one turn be traceable end to end, which includes the
/// external effects it started: the outbox rows it enqueued and the attempts
/// whose outcome is not yet known. Those live in [`outbox_ids`] and
/// [`reconciliation_attempt_ids`], so an auditor holding nothing but this
/// record can ask the outbox what became of the turn's side effects instead of
/// inferring it from the transcript.
///
/// Compared with [`PartialEq`] only, because [`ProviderAttemptRecord`] carries
/// a float; see that type.
///
/// [`outbox_ids`]: ReplayRecord::outbox_ids
/// [`reconciliation_attempt_ids`]: ReplayRecord::reconciliation_attempt_ids
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReplayRecord {
    /// The turn.
    pub turn_id: TurnId,
    /// The conversation.
    pub conversation_id: ConversationId,
    /// The tenant.
    pub account_id: AccountId,
    /// Last persisted phase.
    pub phase: TurnPhase,
    /// Workflow versions in force.
    pub workflow_versions: Vec<WorkflowVersionRecord>,
    /// Cases and revisions loaded at the start of the turn.
    pub loaded_cases: Vec<CaseRef>,
    /// What the turn was understood to say: the reducer's input.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub understanding: Option<Understanding>,
    /// Hash of the understanding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_hash: Option<Digest>,
    /// What the reduction decided about each act, by index, as
    /// [`crate::reduce::PlannedActResult::name`].
    ///
    /// The hash below identifies a plan; this says what happened in it. A
    /// measurement that wants to know how often the structure refused a
    /// reading cannot get there from the command count: an act that changes
    /// nothing and one that is waiting for a confirmation both journal zero
    /// commands and neither was refused, while a plan holding one refusal
    /// beside one write journals a command and hides the refusal entirely.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub act_outcomes: Vec<String>,
    /// Hash of the reduction plan.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reduction_plan_hash: Option<Digest>,
    /// Target resolutions per act.
    pub target_resolutions: Vec<TargetResolutionRecord>,
    /// Policy decisions per command.
    pub policy_decisions: Vec<PolicyDecision>,
    /// Command outcomes.
    pub command_outcomes: Vec<CommandOutcomeRecord>,
    /// Interactions created by the turn.
    pub interactions_created: Vec<InteractionId>,
    /// Events committed by the turn.
    pub event_ids: Vec<EventId>,
    /// Block ids of the assistant turn, in order.
    pub response_block_ids: Vec<BlockId>,
    /// Outbox rows the turn enqueued, in the order they were enqueued
    /// (spec §16.4, §26.1).
    ///
    /// Each identifies a row in the dispatch queue, so the record names the
    /// turn's external effects even before any of them has been dispatched.
    /// Empty for a turn with no external effect, which is the common case; the
    /// field defaults on deserialization, so records written before it existed
    /// still load.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outbox_ids: Vec<OutboxId>,
    /// Attempts at external effects that the turn left unsettled, in the order
    /// they were made (spec §16.5, I15).
    ///
    /// An attempt lands here when the request was transmitted and the outcome
    /// never came back: it may or may not have taken effect, so it is neither
    /// a success to claim nor a failure to retry blindly, and a poller or a
    /// callback has to settle it against the remote system. Reading them
    /// together with the ones implied by
    /// [`CommandOutcome::OutcomeUnknown`] is
    /// what [`pending_reconciliations`](ReplayRecord::pending_reconciliations)
    /// is for. Defaults on deserialization.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reconciliation_attempt_ids: Vec<AttemptId>,
    /// Every provider attempt.
    pub provider_attempts: Vec<ProviderAttemptRecord>,
    /// Every prompt a configured prompt source supplied for this turn, in the
    /// order the stages asked for them and without repeats.
    ///
    /// [`ProviderAttemptRecord::prompt_ref`] answers "which prompt produced
    /// *this call*"; this list answers "which prompts were in force for the
    /// turn at all", which is the question an audit of a released prompt
    /// version asks. It stays empty for an application that has configured no
    /// prompt source, which is the recommended default, and it defaults on
    /// deserialization so records written before it existed still load.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prompt_refs: Vec<PromptRef>,
    /// Every model task the turn ran, repairs, votes and escalations included.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<TaskRecord>,
    /// What the turn's model calls spent, and the bound that stopped them if one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub budget: Option<BudgetReport>,
    /// The effort the turn ran at.
    #[serde(default)]
    pub effort: crate::effort::Effort,
    /// When the record was last written.
    pub recorded_at: DateTime<Utc>,
}

impl ReplayRecord {
    /// A record for a turn that was just received.
    #[must_use]
    pub fn received(
        turn_id: TurnId,
        conversation_id: ConversationId,
        account_id: AccountId,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            turn_id,
            conversation_id,
            account_id,
            phase: TurnPhase::Received,
            workflow_versions: Vec::new(),
            loaded_cases: Vec::new(),
            understanding: None,
            plan_hash: None,
            act_outcomes: Vec::new(),
            reduction_plan_hash: None,
            target_resolutions: Vec::new(),
            policy_decisions: Vec::new(),
            command_outcomes: Vec::new(),
            interactions_created: Vec::new(),
            event_ids: Vec::new(),
            response_block_ids: Vec::new(),
            outbox_ids: Vec::new(),
            reconciliation_attempt_ids: Vec::new(),
            provider_attempts: Vec::new(),
            prompt_refs: Vec::new(),
            tasks: Vec::new(),
            budget: None,
            effort: crate::effort::Effort::Medium,
            recorded_at: now,
        }
    }

    /// Every answer a check refused whole, in the order the tasks ran.
    #[must_use]
    pub fn discarded_answers(&self) -> Vec<DiscardedAnswer> {
        let mut rounds: std::collections::BTreeMap<(&str, &str), u32> =
            std::collections::BTreeMap::new();
        self.tasks
            .iter()
            .filter_map(|task| match &task.verdict {
                TaskVerdict::Rejected { code, reason } => {
                    let owner = task.task_id.split('/').next().unwrap_or_default();
                    let round = rounds.entry((owner, task.kind.as_str())).or_default();
                    *round += 1;
                    Some(DiscardedAnswer {
                        purpose: task.kind.clone(),
                        round: *round,
                        code: code.clone(),
                        reason: reason.clone(),
                    })
                }
                _ => None,
            })
            .collect()
    }

    /// Every attempt at an external effect this record says is unsettled, in
    /// record order and without repeats.
    ///
    /// Two places name one: the explicit
    /// [`reconciliation_attempt_ids`](ReplayRecord::reconciliation_attempt_ids),
    /// which a dispatcher adds to as it retries, and
    /// [`CommandOutcome::OutcomeUnknown`], which execution writes against the
    /// command that caused it. A reconciler wants the union of the two and
    /// wants it once each, so it does not query the remote system twice for the
    /// same attempt.
    ///
    /// The command outcomes come first, because they carry the attempt the turn
    /// itself observed.
    #[must_use]
    pub fn pending_reconciliations(&self) -> Vec<AttemptId> {
        let from_commands =
            self.command_outcomes
                .iter()
                .filter_map(|record| match &record.outcome {
                    CommandOutcome::OutcomeUnknown { attempt_id } => Some(attempt_id),
                    _ => None,
                });
        let mut found: Vec<AttemptId> = Vec::new();
        for attempt in from_commands.chain(self.reconciliation_attempt_ids.iter()) {
            if !found.contains(attempt) {
                found.push(attempt.clone());
            }
        }
        found
    }
}

/// One call of one model task: what it was asked under, and what became of the answer.
///
/// Identifiers are paths (`u2/extract`, `u2/extract#repair1`), so the calls of one
/// turn read as the graph they ran as. The rendered prompt and the raw answer are
/// kept only when the deployment asks for them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TaskRecord {
    /// Where the call sits in the turn.
    pub task_id: String,
    /// The task that asked for this one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
    /// How many calls ran before it in its chain.
    pub depth: u8,
    /// The task kind, as its routing purpose names it.
    pub kind: String,
    /// The instructions it ran under.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_ref: Option<PromptRef>,
    /// The provider that answered, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_key: Option<ProviderKey>,
    /// The model that answered, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_key: Option<ModelKey>,
    /// The settings the call was sent with.
    pub params: TaskParams,
    /// Digest of the request as sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_digest: Option<Digest>,
    /// The request as sent, when the deployment keeps prompts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rendered: Option<serde_json::Value>,
    /// The answer as received, when the deployment keeps it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_output: Option<String>,
    /// The answer as parsed, when it parsed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parsed: Option<serde_json::Value>,
    /// What the runtime made of it.
    pub verdict: TaskVerdict,
    /// Prompt tokens the provider reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    /// Output tokens the provider reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    /// Wall-clock time of the call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
}

impl TaskRecord {
    /// A record of `kind` at `task_id`, with nothing learnt yet.
    #[must_use]
    pub fn new(task_id: impl Into<String>, kind: impl Into<String>, verdict: TaskVerdict) -> Self {
        Self {
            task_id: task_id.into(),
            parent: None,
            depth: 0,
            kind: kind.into(),
            prompt_ref: None,
            provider_key: None,
            model_key: None,
            params: TaskParams::default(),
            input_digest: None,
            rendered: None,
            raw_output: None,
            parsed: None,
            verdict,
            input_tokens: None,
            output_tokens: None,
            latency_ms: None,
        }
    }
}

/// The settings one task call was sent with.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct TaskParams {
    /// Sampling temperature, when one was sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub temperature: Option<f32>,
    /// Output cap, when one was set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<u32>,
    /// Reasoning effort, when one was sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Sampling seed, when one was sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed: Option<u64>,
    /// The call's deadline.
    pub timeout_ms: u64,
}

/// What became of one task call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TaskVerdict {
    /// The answer was used.
    Accepted,
    /// The answer failed a check; a repair or an escalation may follow.
    Rejected {
        /// Stable code of the failed check.
        code: String,
        /// The failure in words, as the repair round was told it.
        reason: String,
    },
    /// Another answer of the same vote was used.
    Outvoted,
    /// No answer came back.
    Failed {
        /// Stable code of the failure.
        code: String,
    },
}

/// What a turn's model calls spent, and which bound stopped them if one did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct BudgetReport {
    /// Calls reserved.
    pub model_calls: u32,
    /// Prompt tokens the providers reported.
    pub prompt_tokens: u64,
    /// The longest chain of dependent calls.
    pub max_depth: u8,
    /// The bound that stopped the turn's model calls, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exhausted: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases() {
        assert!(TurnPhase::Failed.is_terminal());
        assert!(!TurnPhase::Reduced.effects_may_exist());
        assert!(TurnPhase::Executing.effects_may_exist());
    }

    #[test]
    fn a_refused_answer_is_counted_by_its_task_chain() {
        let rejected = |code: &str| TaskVerdict::Rejected {
            code: code.to_owned(),
            reason: format!("{code} in words"),
        };
        let mut record = full_record();
        record.tasks = vec![
            TaskRecord::new("u1/extract", "extract", rejected("out_of_range")),
            TaskRecord::new("u1/extract#repair1", "extract", rejected("wrong_kind")),
            TaskRecord::new("u1/extract#repair2", "extract", TaskVerdict::Accepted),
            TaskRecord::new("u1.a2/extract", "extract", rejected("out_of_range")),
        ];
        let discarded = record.discarded_answers();
        let rounds: Vec<(&str, u32)> = discarded
            .iter()
            .map(|answer| (answer.code.as_str(), answer.round))
            .collect();
        assert_eq!(
            rounds,
            vec![("out_of_range", 1), ("wrong_kind", 2), ("out_of_range", 1)]
        );
        assert_eq!(discarded[0].purpose, "extract");
    }

    fn attempt(temperature: Option<f32>, finish_reasons: &[&str]) -> ProviderAttemptRecord {
        ProviderAttemptRecord {
            attempt: 1,
            purpose: "extract".to_owned(),
            provider_key: ProviderKey::from("openai"),
            model_key: ModelKey::from("gpt-5.4"),
            request_id: "req-1".to_owned(),
            prompt_version: None,
            prompt_ref: None,
            outcome: ProviderAttemptOutcome::Succeeded,
            latency_ms: Some(120),
            input_tokens: Some(10),
            output_tokens: Some(20),
            temperature,
            finish_reasons: finish_reasons.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    fn full_record() -> ReplayRecord {
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let mut record = ReplayRecord::received(
            TurnId::nil(),
            ConversationId::nil(),
            AccountId::from("a"),
            now,
        );
        record.outbox_ids = vec![OutboxId::nil()];
        record.reconciliation_attempt_ids = vec![AttemptId::from("attempt-dispatch-2")];
        record.provider_attempts = vec![attempt(Some(0.2), &["stop", "length"])];
        record
    }

    #[test]
    fn record_round_trips() {
        let now = DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let r = ReplayRecord::received(
            TurnId::nil(),
            ConversationId::nil(),
            AccountId::from("a"),
            now,
        );
        let json = serde_json::to_string(&r).unwrap();
        assert_eq!(serde_json::from_str::<ReplayRecord>(&json).unwrap(), r);
    }

    #[test]
    fn external_effect_identifiers_round_trip() {
        let record = full_record();
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(json["outbox_ids"][0], serde_json::json!(OutboxId::nil()));
        assert_eq!(json["reconciliation_attempt_ids"][0], "attempt-dispatch-2");
        assert_eq!(
            serde_json::from_value::<ReplayRecord>(json).unwrap(),
            record
        );
    }

    #[test]
    fn provider_attempt_carries_temperature_and_finish_reasons() {
        let with_sampling = attempt(Some(0.2), &["stop", "length"]);
        let json = serde_json::to_value(&with_sampling).unwrap();
        assert_eq!(json["temperature"], serde_json::json!(0.2_f32));
        assert_eq!(
            json["finish_reasons"],
            serde_json::json!(["stop", "length"])
        );
        assert_eq!(
            serde_json::from_value::<ProviderAttemptRecord>(json).unwrap(),
            with_sampling
        );

        // Absent is not zero, and an empty list of reasons stays out of the JSON.
        let default_sampling = attempt(None, &[]);
        let json = serde_json::to_value(&default_sampling).unwrap();
        assert!(json.get("temperature").is_none());
        assert!(json.get("finish_reasons").is_none());
        assert_ne!(default_sampling, attempt(Some(0.0), &[]));
    }

    #[test]
    fn records_written_before_the_new_fields_still_load() {
        // Exactly what a record serialized by an older build looks like: no
        // outbox ids, no reconciliation ids, an attempt with no sampling data.
        let legacy = serde_json::json!({
            "turn_id": TurnId::nil(),
            "conversation_id": ConversationId::nil(),
            "account_id": "a",
            "phase": "received",
            "workflow_versions": [],
            "loaded_cases": [],
            "target_resolutions": [],
            "policy_decisions": [],
            "command_outcomes": [],
            "interactions_created": [],
            "event_ids": [],
            "response_block_ids": [],
            "provider_attempts": [{
                "attempt": 1,
                "purpose": "extract",
                "provider_key": "openai",
                "model_key": "gpt-5.4",
                "request_id": "req-1",
                "outcome": { "kind": "succeeded" }
            }],
            "recorded_at": "2023-11-14T22:13:20Z",
        });
        let loaded: ReplayRecord = serde_json::from_value(legacy).unwrap();
        assert!(loaded.outbox_ids.is_empty());
        assert!(loaded.reconciliation_attempt_ids.is_empty());
        assert_eq!(loaded.provider_attempts[0].temperature, None);
        assert!(loaded.provider_attempts[0].finish_reasons.is_empty());
        assert!(loaded.prompt_refs.is_empty());
        assert_eq!(loaded.provider_attempts[0].prompt_ref, None);
    }

    #[test]
    fn a_prompt_reference_reaches_both_the_turn_and_the_attempt_that_used_it() {
        let reference = crate::prompt::PromptRef::of_text(
            "interpret.system",
            "9f2a1c",
            "Answer with the plan only.",
        );
        let mut record = full_record();
        record.prompt_refs = vec![reference.clone()];
        record.provider_attempts[0].prompt_ref = Some(reference.clone());

        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(json["prompt_refs"][0]["name"], "interpret.system");
        assert_eq!(json["prompt_refs"][0]["version"], "9f2a1c");
        assert_eq!(
            json["provider_attempts"][0]["prompt_ref"]["hash"],
            serde_json::json!(reference.hash.as_str())
        );
        assert_eq!(
            serde_json::from_value::<ReplayRecord>(json).unwrap(),
            record
        );

        // A turn with no prompt source keeps both out of the JSON entirely.
        let quiet = full_record();
        let json = serde_json::to_value(&quiet).unwrap();
        assert!(json.get("prompt_refs").is_none());
        assert!(json["provider_attempts"][0].get("prompt_ref").is_none());
    }

    #[test]
    fn a_record_written_before_effort_existed_reads_as_medium() {
        let mut value = serde_json::to_value(full_record()).unwrap();
        value.as_object_mut().unwrap().remove("effort");
        let read: ReplayRecord = serde_json::from_value(value).unwrap();
        assert_eq!(read.effort, crate::effort::Effort::Medium);
    }

    #[test]
    fn pending_reconciliations_unions_both_sources_without_repeats() {
        let mut record = full_record();
        let from_command = AttemptId::from("attempt-command-1");
        record.command_outcomes = vec![
            CommandOutcomeRecord {
                command_ref: CommandRef {
                    batch_id: crate::ids::BatchId::nil(),
                    command_id: crate::ids::CommandId::nil(),
                },
                idempotency_key: IdempotencyKey::new("k1"),
                case_ref: CaseRef::new("w", "c", CaseRevision(1)),
                origin: None,
                outcome: CommandOutcome::OutcomeUnknown {
                    attempt_id: from_command.clone(),
                },
            },
            CommandOutcomeRecord {
                command_ref: CommandRef {
                    batch_id: crate::ids::BatchId::nil(),
                    command_id: crate::ids::CommandId::nil(),
                },
                idempotency_key: IdempotencyKey::new("k2"),
                case_ref: CaseRef::new("w", "c", CaseRevision(1)),
                origin: None,
                outcome: CommandOutcome::IdempotentReplay,
            },
        ];
        // The dispatcher also recorded the attempt the turn observed, plus one
        // of its own retries.
        record.reconciliation_attempt_ids =
            vec![from_command.clone(), AttemptId::from("attempt-dispatch-2")];

        assert_eq!(
            record.pending_reconciliations(),
            vec![from_command, AttemptId::from("attempt-dispatch-2")],
            "the command outcome comes first and nothing is listed twice"
        );

        let quiet = ReplayRecord::received(
            TurnId::nil(),
            ConversationId::nil(),
            AccountId::from("a"),
            DateTime::from_timestamp(1_700_000_000, 0).unwrap(),
        );
        assert!(quiet.pending_reconciliations().is_empty());
    }
}

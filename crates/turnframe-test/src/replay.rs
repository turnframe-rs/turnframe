//! Replay assertions: two runs of the same turn, and one record that has to
//! account for itself (spec §26.4, I20).
//!
//! Determinism is the promise the whole library rests on, and it has two
//! halves that fail in different ways.
//!
//! The first is **reproducibility**: running the same turn twice must produce
//! the same normalized plan, the same commands, the same events and the same
//! ordered response blocks. [`same_turn`] compares two [`TurnExecution`]s and
//! names the first thing that differs, in that order — plan before commands
//! before events before blocks — because that is the order in which a
//! divergence *causes* the next one, and the first difference is the one worth
//! debugging.
//!
//! The second is **self-explanation**: a replay record is an audit artefact, so
//! it must not merely be *consistent with* the turn, it must account for it.
//! [`ReplayEvidence::explains_its_turn`] checks that every command the record
//! lists has a policy decision and a trusted-enough origin behind it, that no
//! command the policy refused was committed anyway, and that every receipt
//! cites events the record contains. A record that passes can be read on its
//! own; a record that fails is one where somebody would have to go and find the
//! rest of the story.
//!
//! # Where the origins come from
//!
//! [`CommandOutcomeRecord`](turnframe_core::replay::CommandOutcomeRecord)
//! carries the command, its idempotency key, its case
//! and its outcome — not its origin. The origin lives on the
//! [`CommandEnvelope`] that executed. So the evidence is assembled from both:
//! [`ReplayEvidence::with_batch`] harvests the origins from the batches the
//! turn executed, and the check refuses a record whose commands cannot all be
//! traced back to one.
//!
//! ```
//! use turnframe_test::replay::ReplayEvidence;
//! # use turnframe_core::ids::{AccountId, ConversationId, TurnId};
//! # use turnframe_core::replay::ReplayRecord;
//! let record = ReplayRecord::received(
//!     TurnId::nil(),
//!     ConversationId::nil(),
//!     AccountId::from("aurora"),
//!     chrono::Utc::now(),
//! );
//!
//! // A turn that did nothing explains itself trivially.
//! ReplayEvidence::new(&record).explains_its_turn().unwrap();
//! ```

use std::collections::BTreeMap;
use std::fmt;

use turnframe_core::command::{
    CommandBatch, CommandEnvelope, CommandOrigin, ConfirmationPolicy, RiskClass, origin_satisfies,
};
use turnframe_core::event::OperationalReceipt;
use turnframe_core::hash::canonical_value;
use turnframe_core::ids::{CommandId, EventId, ReceiptId};
use turnframe_core::reduce::CommandRef;
use turnframe_core::replay::{CommandOutcome, ReplayRecord};
use turnframe_core::response::AssistantTurn;
use turnframe_core::understanding::Understanding;

use crate::assertions::{AssertionFailure, identical_blocks};

/// One execution of a turn: what it recorded, and what it answered.
///
/// The pair is the unit a replay assertion works on, because half of
/// determinism lives in the record (plan, commands, events) and half in the
/// answer (the ordered blocks the user saw).
#[derive(Debug, Clone, PartialEq)]
pub struct TurnExecution {
    /// What the turn recorded about itself.
    pub record: ReplayRecord,
    /// What the turn returned.
    pub response: AssistantTurn,
}

impl TurnExecution {
    /// Pairs a record with the answer it explains.
    #[must_use]
    pub fn new(record: ReplayRecord, response: AssistantTurn) -> Self {
        Self { record, response }
    }

    /// Compares this execution with another.
    ///
    /// # Errors
    ///
    /// The first [`ReplayDivergence`]; see [`same_turn`].
    pub fn same_as(&self, other: &Self) -> Result<(), ReplayDivergence> {
        same_turn(self, other)
    }
}

/// Two executions of the same turn did not match.
///
/// Every variant carries positions, identifiers and stable labels. None of them
/// renders a plan, an argument or a narration, because a divergence report ends
/// up in CI output and those carry what the user typed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ReplayDivergence {
    /// One execution accepted a plan and the other did not.
    #[error("one execution recorded a normalized plan ({left}) and the other did not ({right})")]
    PlanPresenceDiffers {
        /// Whether the left execution had a plan.
        left: bool,
        /// Whether the right execution had a plan.
        right: bool,
    },
    /// The normalized plans differ.
    #[error("the normalized plans differ: {detail}")]
    PlanDiffers {
        /// Where they first differ, as a stable label.
        detail: String,
    },
    /// The executions produced different numbers of commands.
    #[error("the executions produced {left} and {right} command(s)")]
    CommandCountDiffers {
        /// Commands in the left execution.
        left: usize,
        /// Commands in the right execution.
        right: usize,
    },
    /// One command differs.
    #[error("command {index} differs: {detail}")]
    CommandDiffers {
        /// Position in the command list.
        index: usize,
        /// What differs, as a stable label.
        detail: String,
    },
    /// The executions committed different numbers of events.
    #[error("the executions committed {left} and {right} event(s)")]
    EventCountDiffers {
        /// Events in the left execution.
        left: usize,
        /// Events in the right execution.
        right: usize,
    },
    /// One committed event differs.
    #[error("event {index} differs: {left} then {right}")]
    EventsDiffer {
        /// Position in the event list.
        index: usize,
        /// Identifier in the left execution.
        left: EventId,
        /// Identifier in the right execution.
        right: EventId,
    },
    /// The answers differ.
    #[error("the answers differ: {0}")]
    ResponseDiffers(AssertionFailure),
}

/// Compares the understandings two records carry.
///
/// # Errors
///
/// [`ReplayDivergence::PlanPresenceDiffers`] or
/// [`ReplayDivergence::PlanDiffers`].
pub fn same_plan(left: &ReplayRecord, right: &ReplayRecord) -> Result<(), ReplayDivergence> {
    match (&left.understanding, &right.understanding) {
        (None, None) => Ok(()),
        (left, right) if left.is_some() != right.is_some() => {
            Err(ReplayDivergence::PlanPresenceDiffers {
                left: left.is_some(),
                right: right.is_some(),
            })
        }
        (Some(left), Some(right)) => {
            if canonical_value(left).ok() == canonical_value(right).ok() {
                Ok(())
            } else {
                Err(ReplayDivergence::PlanDiffers {
                    detail: first_plan_difference(left, right),
                })
            }
        }
        _ => Ok(()),
    }
}

/// A stable label for the first place two plans diverge. Never carries a value.
fn first_plan_difference(left: &Understanding, right: &Understanding) -> String {
    if left.acts.len() != right.acts.len() {
        return format!("act count {} then {}", left.acts.len(), right.acts.len());
    }
    for (a, b) in left.acts.iter().zip(&right.acts) {
        if a != b {
            return format!("act {} then {}", a.id, b.id);
        }
    }
    if left.questions != right.questions {
        return format!(
            "questions ({} then {})",
            left.questions.len(),
            right.questions.len()
        );
    }
    if left.constraints != right.constraints {
        return format!(
            "constraints ({} then {})",
            left.constraints.len(),
            right.constraints.len()
        );
    }
    "other content".to_owned()
}

/// Compares the commands two records account for, in order.
///
/// # Errors
///
/// [`ReplayDivergence::CommandCountDiffers`] or
/// [`ReplayDivergence::CommandDiffers`].
pub fn same_commands(left: &ReplayRecord, right: &ReplayRecord) -> Result<(), ReplayDivergence> {
    if left.command_outcomes.len() != right.command_outcomes.len() {
        return Err(ReplayDivergence::CommandCountDiffers {
            left: left.command_outcomes.len(),
            right: right.command_outcomes.len(),
        });
    }
    for (index, (a, b)) in left
        .command_outcomes
        .iter()
        .zip(&right.command_outcomes)
        .enumerate()
    {
        let detail = if a.command_ref != b.command_ref {
            Some(format!("{} then {}", a.command_ref, b.command_ref))
        } else if a.idempotency_key != b.idempotency_key {
            Some("idempotency key".to_owned())
        } else if a.case_ref.key() != b.case_ref.key() {
            Some("case".to_owned())
        } else if a.case_ref.expected_revision != b.case_ref.expected_revision {
            Some(format!(
                "expected revision {} then {}",
                a.case_ref.expected_revision.value(),
                b.case_ref.expected_revision.value()
            ))
        } else if a.outcome != b.outcome {
            Some(format!(
                "outcome {} then {}",
                outcome_label(&a.outcome),
                outcome_label(&b.outcome)
            ))
        } else {
            None
        };
        if let Some(detail) = detail {
            return Err(ReplayDivergence::CommandDiffers { index, detail });
        }
    }
    Ok(())
}

/// Stable label of a command outcome. Never carries a value.
fn outcome_label(outcome: &CommandOutcome) -> &'static str {
    match outcome {
        CommandOutcome::Committed { .. } => "committed",
        CommandOutcome::IdempotentReplay => "idempotent_replay",
        CommandOutcome::RevisionConflict { .. } => "revision_conflict",
        CommandOutcome::Rejected { .. } => "rejected",
        CommandOutcome::Failed { .. } => "failed",
        CommandOutcome::OutcomeUnknown { .. } => "outcome_unknown",
        CommandOutcome::AwaitingConfirmation { .. } => "awaiting_confirmation",
        _ => "other",
    }
}

/// Compares the events two records committed, in order.
///
/// # Errors
///
/// [`ReplayDivergence::EventCountDiffers`] or
/// [`ReplayDivergence::EventsDiffer`].
pub fn same_events(left: &ReplayRecord, right: &ReplayRecord) -> Result<(), ReplayDivergence> {
    if left.event_ids.len() != right.event_ids.len() {
        return Err(ReplayDivergence::EventCountDiffers {
            left: left.event_ids.len(),
            right: right.event_ids.len(),
        });
    }
    for (index, (a, b)) in left.event_ids.iter().zip(&right.event_ids).enumerate() {
        if a != b {
            return Err(ReplayDivergence::EventsDiffer {
                index,
                left: *a,
                right: *b,
            });
        }
    }
    Ok(())
}

/// Compares two answers block by block.
///
/// # Errors
///
/// [`ReplayDivergence::ResponseDiffers`].
pub fn same_blocks(left: &AssistantTurn, right: &AssistantTurn) -> Result<(), ReplayDivergence> {
    identical_blocks(left, right).map_err(ReplayDivergence::ResponseDiffers)
}

/// Asserts that two executions of the same turn produced the same normalized
/// plan, the same commands, the same events and the same ordered blocks.
///
/// # Errors
///
/// The first [`ReplayDivergence`], checked in that order.
pub fn same_turn(left: &TurnExecution, right: &TurnExecution) -> Result<(), ReplayDivergence> {
    same_plan(&left.record, &right.record)?;
    same_commands(&left.record, &right.record)?;
    same_events(&left.record, &right.record)?;
    same_blocks(&left.response, &right.response)
}

/// Something a replay record fails to account for.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ReplayGap {
    /// A command is listed with no policy decision behind it.
    #[error("command {command_ref} has no policy decision")]
    CommandWithoutPolicyDecision {
        /// The command.
        command_ref: CommandRef,
    },
    /// A command is listed with no origin: nothing says who authorized it.
    #[error("command {command_ref} has no origin in the evidence")]
    CommandWithoutOrigin {
        /// The command.
        command_ref: CommandRef,
    },
    /// A command's origin does not satisfy the policy recorded for it (I12).
    #[error(
        "command {command_ref} carries an origin that does not satisfy its policy \
         (risk {risk:?}, confirmation {confirmation:?})"
    )]
    OriginDoesNotSatisfyPolicy {
        /// The command.
        command_ref: CommandRef,
        /// Its risk class.
        risk: RiskClass,
        /// Its confirmation policy.
        confirmation: ConfirmationPolicy,
    },
    /// Policy refused the command and it committed anyway.
    #[error("command {command_ref} committed although policy refused it ({reason_key})")]
    RefusedCommandCommitted {
        /// The command.
        command_ref: CommandRef,
        /// The recorded reason key.
        reason_key: String,
    },
    /// A policy decision names a command the record never accounts for (I11).
    #[error("policy decision for {command_ref} has no command outcome")]
    DecisionWithoutOutcome {
        /// The command.
        command_ref: CommandRef,
    },
    /// A command committed an event the record does not list.
    #[error("command {command_ref} committed event {event_id}, which the record does not list")]
    CommittedEventNotRecorded {
        /// The command.
        command_ref: CommandRef,
        /// The event it claims.
        event_id: EventId,
    },
    /// A receipt cites nothing (I16).
    #[error("receipt {receipt_id} ({status_code}) cites no event")]
    ReceiptWithoutEvents {
        /// The receipt.
        receipt_id: ReceiptId,
        /// Its status code.
        status_code: String,
    },
    /// A receipt cites an event the record does not contain (I16).
    #[error(
        "receipt {receipt_id} ({status_code}) cites event {event_id}, which the record does not list"
    )]
    ReceiptCitesUnrecordedEvent {
        /// The receipt.
        receipt_id: ReceiptId,
        /// Its status code.
        status_code: String,
        /// The event it cites.
        event_id: EventId,
    },
}

/// Every gap one record left, in the order they were found.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct ReplayGaps {
    /// The gaps.
    pub gaps: Vec<ReplayGap>,
}

impl fmt::Display for ReplayGaps {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "the replay record does not explain its turn:")?;
        for gap in &self.gaps {
            write!(f, "\n- {gap}")?;
        }
        Ok(())
    }
}

/// A replay record plus the two things it does not carry itself: the origins of
/// the commands that executed, and the receipts the turn rendered.
#[derive(Debug, Clone)]
pub struct ReplayEvidence<'a> {
    record: &'a ReplayRecord,
    origins: BTreeMap<CommandId, CommandOrigin>,
    receipts: Vec<OperationalReceipt>,
}

impl<'a> ReplayEvidence<'a> {
    /// Starts from a record with no origins and no receipts.
    #[must_use]
    pub fn new(record: &'a ReplayRecord) -> Self {
        Self {
            record,
            origins: BTreeMap::new(),
            receipts: Vec::new(),
        }
    }

    /// Harvests the origin of every envelope of a batch the turn executed.
    #[must_use]
    pub fn with_batch<C>(mut self, batch: &CommandBatch<C>) -> Self {
        for envelope in &batch.envelopes {
            self.origins
                .insert(envelope.command_id, envelope.origin.clone());
        }
        self
    }

    /// Harvests the origin of one envelope.
    #[must_use]
    pub fn with_envelope<C>(mut self, envelope: &CommandEnvelope<C>) -> Self {
        self.origins
            .insert(envelope.command_id, envelope.origin.clone());
        self
    }

    /// Declares one origin directly, for a test that has no envelope at hand.
    #[must_use]
    pub fn with_origin(mut self, command_id: CommandId, origin: CommandOrigin) -> Self {
        self.origins.insert(command_id, origin);
        self
    }

    /// Adds the receipts the turn rendered.
    #[must_use]
    pub fn with_receipts(mut self, receipts: &[OperationalReceipt]) -> Self {
        self.receipts.extend_from_slice(receipts);
        self
    }

    /// The record under examination.
    #[must_use]
    pub fn record(&self) -> &ReplayRecord {
        self.record
    }

    /// Checks that the record accounts for its own turn.
    ///
    /// Every command it lists must have a policy decision and an origin, the
    /// origin must satisfy the decision's policy, a refused command must not
    /// have committed, every decision must have a matching outcome, and every
    /// event a command or a receipt claims must be one the record lists.
    ///
    /// # Errors
    ///
    /// [`ReplayGaps`] with every gap found, so one run diagnoses the whole
    /// record.
    pub fn explains_its_turn(&self) -> Result<(), ReplayGaps> {
        let mut gaps = Vec::new();
        for outcome in &self.record.command_outcomes {
            let command_ref = outcome.command_ref;
            match self
                .record
                .policy_decisions
                .iter()
                .find(|decision| decision.command_ref == command_ref)
            {
                None => gaps.push(ReplayGap::CommandWithoutPolicyDecision { command_ref }),
                Some(decision) => {
                    match self.origins.get(&command_ref.command_id) {
                        None => gaps.push(ReplayGap::CommandWithoutOrigin { command_ref }),
                        Some(origin) => {
                            if !origin_satisfies(origin, &decision.policy) {
                                gaps.push(ReplayGap::OriginDoesNotSatisfyPolicy {
                                    command_ref,
                                    risk: decision.policy.risk,
                                    confirmation: decision.policy.confirmation,
                                });
                            }
                        }
                    }
                    if !decision.allowed
                        && matches!(outcome.outcome, CommandOutcome::Committed { .. })
                    {
                        gaps.push(ReplayGap::RefusedCommandCommitted {
                            command_ref,
                            reason_key: decision.reason_key.clone(),
                        });
                    }
                }
            }
            if let CommandOutcome::Committed { event_ids, .. } = &outcome.outcome {
                for event_id in event_ids {
                    if !self.record.event_ids.contains(event_id) {
                        gaps.push(ReplayGap::CommittedEventNotRecorded {
                            command_ref,
                            event_id: *event_id,
                        });
                    }
                }
            }
        }
        for decision in &self.record.policy_decisions {
            if !self
                .record
                .command_outcomes
                .iter()
                .any(|outcome| outcome.command_ref == decision.command_ref)
            {
                gaps.push(ReplayGap::DecisionWithoutOutcome {
                    command_ref: decision.command_ref,
                });
            }
        }
        for receipt in &self.receipts {
            if receipt.event_ids.is_empty() {
                gaps.push(ReplayGap::ReceiptWithoutEvents {
                    receipt_id: receipt.receipt_id,
                    status_code: receipt.status_code.clone(),
                });
            }
            for event_id in &receipt.event_ids {
                if !self.record.event_ids.contains(event_id) {
                    gaps.push(ReplayGap::ReceiptCitesUnrecordedEvent {
                        receipt_id: receipt.receipt_id,
                        status_code: receipt.status_code.clone(),
                        event_id: *event_id,
                    });
                }
            }
        }
        if gaps.is_empty() {
            Ok(())
        } else {
            Err(ReplayGaps { gaps })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turnframe_core::case::CaseRef;
    use turnframe_core::command::{CommandPolicy, IdempotencyKey};
    use turnframe_core::event::ReceiptSeverity;
    use turnframe_core::ids::{AccountId, BatchId, BlockId, CaseRevision, ConversationId, TurnId};
    use turnframe_core::locale::LocalizedText;
    use turnframe_core::policy::{PolicySnapshot, reason};
    use turnframe_core::replay::CommandOutcomeRecord;
    use turnframe_core::response::{GeneratedTransition, ReplayToken, ResponseBlock};

    fn command_ref() -> CommandRef {
        CommandRef {
            batch_id: BatchId::nil(),
            command_id: CommandId::nil(),
        }
    }

    fn event(byte: u8) -> EventId {
        EventId::from(uuid::Uuid::from_bytes([byte; 16]))
    }

    fn record() -> ReplayRecord {
        ReplayRecord::received(
            TurnId::nil(),
            ConversationId::nil(),
            AccountId::from("aurora"),
            chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap_or_default(),
        )
    }

    fn direct_origin() -> CommandOrigin {
        CommandOrigin::DirectSafeUserAct {
            evidence_digest: turnframe_core::hash::Digest::of_bytes(b"evidence"),
        }
    }

    /// A record for one low-risk command that committed one event.
    fn committed_record() -> ReplayRecord {
        let mut record = record();
        let policy = CommandPolicy::low_risk();
        record
            .policy_decisions
            .push(PolicySnapshot::conservative().decide(command_ref(), &policy, &direct_origin()));
        record.command_outcomes.push(CommandOutcomeRecord {
            command_ref: command_ref(),
            idempotency_key: IdempotencyKey("idem-1".to_owned()),
            case_ref: CaseRef::new("trip", "trip-1", CaseRevision(1)),
            origin: Some(direct_origin()),
            outcome: CommandOutcome::Committed {
                new_revision: CaseRevision(2),
                event_ids: vec![event(1)],
            },
        });
        record.event_ids.push(event(1));
        record
    }

    fn turn(text: &str) -> AssistantTurn {
        AssistantTurn {
            turn_id: TurnId::nil(),
            conversation_id: ConversationId::nil(),
            blocks: vec![ResponseBlock::Transition(GeneratedTransition {
                block_id: BlockId::from("t1"),
                text: text.to_owned(),
                facts_used: Vec::new(),
            })],
            replay_token: ReplayToken::from("rt"),
            subjects: Vec::new(),
            expectations: Vec::new(),
            done: Vec::new(),
        }
    }

    fn receipt(event_ids: Vec<EventId>) -> OperationalReceipt {
        OperationalReceipt {
            receipt_id: ReceiptId::derive(&event_ids, "trip.rebooking_sent"),
            event_ids,
            severity: ReceiptSeverity::Success,
            title: LocalizedText::new("Sent"),
            body: LocalizedText::new("The rebooking was sent."),
            status_code: "trip.rebooking_sent".to_owned(),
            artifact_refs: Vec::new(),
        }
    }

    #[test]
    fn two_identical_executions_agree() {
        let execution = TurnExecution::new(committed_record(), turn("fatto"));
        assert_eq!(execution.same_as(&execution.clone()), Ok(()));
    }

    #[test]
    fn a_different_plan_is_reported_before_anything_else() {
        let text = "Cambia il nome in Lisbona";
        let plan = crate::providers::UnderstandingBuilder::of(text)
            .apply(
                "trip.set_name",
                "tok_1",
                serde_json::Value::Null,
                "Cambia il nome",
            )
            .build()
            .unwrap();
        let other = crate::providers::UnderstandingBuilder::of(text)
            .ask("Cambia")
            .build()
            .unwrap();

        let mut left = TurnExecution::new(committed_record(), turn("fatto"));
        let mut right = left.clone();
        left.record.understanding = Some(plan);
        right.record.understanding = Some(other);
        // Make the commands differ too: the plan difference still wins.
        right.record.command_outcomes.clear();

        let divergence = same_turn(&left, &right).unwrap_err();
        assert_eq!(
            divergence,
            ReplayDivergence::PlanDiffers {
                detail: "act count 1 then 0".to_owned(),
            }
        );

        right.record.understanding = None;
        assert_eq!(
            same_plan(&left.record, &right.record).unwrap_err(),
            ReplayDivergence::PlanPresenceDiffers {
                left: true,
                right: false,
            }
        );
    }

    #[test]
    fn a_different_command_outcome_is_named_by_position() {
        let left = TurnExecution::new(committed_record(), turn("fatto"));
        let mut right = left.clone();
        right.record.command_outcomes[0].outcome = CommandOutcome::IdempotentReplay;

        assert_eq!(
            same_turn(&left, &right).unwrap_err(),
            ReplayDivergence::CommandDiffers {
                index: 0,
                detail: "outcome committed then idempotent_replay".to_owned(),
            }
        );

        let mut fewer = left.clone();
        fewer.record.command_outcomes.clear();
        assert_eq!(
            same_commands(&left.record, &fewer.record).unwrap_err(),
            ReplayDivergence::CommandCountDiffers { left: 1, right: 0 }
        );
    }

    #[test]
    fn different_events_and_blocks_are_reported_in_order() {
        let left = TurnExecution::new(committed_record(), turn("fatto"));
        let mut right = left.clone();
        right.record.event_ids = vec![event(2)];
        right.record.command_outcomes[0].outcome = CommandOutcome::Committed {
            new_revision: CaseRevision(2),
            event_ids: vec![event(2)],
        };
        assert!(
            matches!(
                same_turn(&left, &right).unwrap_err(),
                ReplayDivergence::CommandDiffers { .. }
            ),
            "the command outcome differs before the event list does"
        );

        let mut only_events = left.clone();
        only_events.record.event_ids = vec![event(2)];
        assert_eq!(
            same_events(&left.record, &only_events.record).unwrap_err(),
            ReplayDivergence::EventsDiffer {
                index: 0,
                left: event(1),
                right: event(2),
            }
        );

        let other_answer = TurnExecution::new(committed_record(), turn("done"));
        assert!(matches!(
            same_turn(&left, &other_answer).unwrap_err(),
            ReplayDivergence::ResponseDiffers(AssertionFailure::BlocksDiffer { index: 0 })
        ));
    }

    #[test]
    fn a_record_with_a_decision_an_origin_and_its_events_explains_itself() {
        let record = committed_record();
        ReplayEvidence::new(&record)
            .with_origin(CommandId::nil(), direct_origin())
            .with_receipts(&[receipt(vec![event(1)])])
            .explains_its_turn()
            .expect("the record accounts for its turn");
    }

    #[test]
    fn a_command_without_a_policy_decision_is_a_gap() {
        let mut record = committed_record();
        record.policy_decisions.clear();
        let gaps = ReplayEvidence::new(&record)
            .with_origin(CommandId::nil(), direct_origin())
            .explains_its_turn()
            .unwrap_err();
        assert_eq!(
            gaps.gaps,
            vec![ReplayGap::CommandWithoutPolicyDecision {
                command_ref: command_ref(),
            }]
        );
        assert!(gaps.to_string().contains("has no policy decision"));
    }

    #[test]
    fn a_command_without_an_origin_is_a_gap() {
        let record = committed_record();
        let gaps = ReplayEvidence::new(&record)
            .explains_its_turn()
            .unwrap_err();
        assert_eq!(
            gaps.gaps,
            vec![ReplayGap::CommandWithoutOrigin {
                command_ref: command_ref(),
            }]
        );
    }

    #[test]
    fn an_origin_that_does_not_satisfy_the_recorded_policy_is_a_gap() {
        let mut record = committed_record();
        let policy = CommandPolicy {
            risk: RiskClass::ExternalRegulated,
            confirmation: ConfirmationPolicy::ExplicitClick,
            ..CommandPolicy::conservative()
        };
        record.policy_decisions =
            vec![PolicySnapshot::conservative().decide(command_ref(), &policy, &direct_origin())];

        let gaps = ReplayEvidence::new(&record)
            .with_origin(CommandId::nil(), direct_origin())
            .explains_its_turn()
            .unwrap_err();

        assert!(gaps.gaps.contains(&ReplayGap::OriginDoesNotSatisfyPolicy {
            command_ref: command_ref(),
            risk: RiskClass::ExternalRegulated,
            confirmation: ConfirmationPolicy::ExplicitClick,
        }));
        assert!(gaps.gaps.contains(&ReplayGap::RefusedCommandCommitted {
            command_ref: command_ref(),
            reason_key: reason::CONFIRMATION_REQUIRED.to_owned(),
        }));
    }

    #[test]
    fn an_unaccounted_decision_and_an_invented_event_are_gaps() {
        let mut record = committed_record();
        record.command_outcomes.clear();
        record.event_ids.clear();

        let gaps = ReplayEvidence::new(&record)
            .with_receipts(&[receipt(vec![event(9)]), receipt(Vec::new())])
            .explains_its_turn()
            .unwrap_err();

        assert!(gaps.gaps.contains(&ReplayGap::DecisionWithoutOutcome {
            command_ref: command_ref(),
        }));
        assert!(
            gaps.gaps
                .iter()
                .any(|gap| matches!(gap, ReplayGap::ReceiptCitesUnrecordedEvent { .. }))
        );
        assert!(
            gaps.gaps
                .iter()
                .any(|gap| matches!(gap, ReplayGap::ReceiptWithoutEvents { .. }))
        );
    }

    #[test]
    fn a_command_claiming_an_unlisted_event_is_a_gap() {
        let mut record = committed_record();
        record.event_ids.clear();
        let gaps = ReplayEvidence::new(&record)
            .with_origin(CommandId::nil(), direct_origin())
            .explains_its_turn()
            .unwrap_err();
        assert_eq!(
            gaps.gaps,
            vec![ReplayGap::CommittedEventNotRecorded {
                command_ref: command_ref(),
                event_id: event(1),
            }]
        );
        assert_eq!(ReplayEvidence::new(&record).record().turn_id, TurnId::nil());
    }
}

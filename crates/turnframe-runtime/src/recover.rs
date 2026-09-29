//! Crash recovery (spec §23.1).
//!
//! A turn writes a phase marker as it goes, and a crash leaves that marker
//! behind. Recovery reads it, together with the command journal, and decides
//! which of four things to do — and the order the decision is taken in is the
//! safety property, because two of the four would be wrong if taken first:
//!
//! 1. **An unknown external outcome is reconciled, never retried.** A command
//!    that timed out after transmission may have taken effect. Repeating it
//!    would be the duplicate the whole library exists to prevent, so this case
//!    is checked before anything else (§16.5, I15).
//! 2. **Pending commands are resumed by idempotency key.** An entry in
//!    `Pending` or `Executing` was admitted and may or may not have reached the
//!    domain. Running it again is safe *only* through the key, which is why
//!    recovery hands back the entries rather than the plan.
//! 3. **A committed turn regenerates its response.** The effects are done; what
//!    is missing is the answer. It is rebuilt from the committed events and the
//!    stored plan, and nothing is executed.
//! 4. **A turn with no journal entry restarts interpretation.** Nothing was
//!    admitted, so nothing can have happened, and the turn may simply be
//!    interpreted again.
//!
//! # Why the answer tasks come back from the plan
//!
//! §23.1 asks for the response to be regenerated "from events and stored answer
//! tasks". The reduction is pure, so its questions are exactly the questions of
//! the accepted plan the replay record already stores — no separate table is
//! needed, and re-deriving them cannot drift from what the turn actually
//! planned. Their basis is normalized to
//! [`AnswerBasis::CurrentCommittedState`]: after commit, "what will be true
//! after this turn" and "what is true now" are the same state, and the second
//! is the one that can be read.

use std::fmt;
use std::sync::Arc;

use turnframe_core::error::OrchestratorError;
use turnframe_core::ids::{AccountId, AttemptId, EventId, TurnId};
use turnframe_core::plan::AnswerBasis;
use turnframe_core::reduce::{AnswerTask, SourcePolicy};
use turnframe_core::replay::{ReplayRecord, TurnPhase};
use turnframe_store::conversation::{
    ConversationStore, RecoveryScope, StoredTurn, TurnPhaseMarker,
};
use turnframe_store::error::StoreError;
use turnframe_store::events::{EventJournal, StoredEvent};
use turnframe_store::journal::{CommandJournal, CommandJournalEntry};
use turnframe_store::replay::ReplayStore;

/// Maximum events one regeneration reads back per case.
const MAX_REPLAYED_EVENTS: usize = 1024;

/// What recovery decided to do with a turn (spec §23.1).
///
/// "Unfinished" is not quite the right word for what this covers, and the
/// difference matters: a turn can be [`TurnPhase::Delivered`] — the user has a
/// truthful answer, saying the request is being verified — while an external
/// effect it started is still unsettled. Recovery therefore looks at the
/// journal before it looks at the phase.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum RecoveryAction {
    /// The turn is finished; there is nothing to do.
    Nothing {
        /// The phase it finished in.
        phase: TurnPhase,
    },
    /// No command was ever admitted, so nothing can have happened and the turn
    /// may be interpreted again from the start.
    RestartInterpretation,
    /// Commands were admitted and not settled. They are resumed **by
    /// idempotency key**: handing the executor these entries lets it recognise
    /// what already ran instead of running it twice (I14).
    ResumeCommands {
        /// The entries to resume, in admission order.
        entries: Vec<CommandJournalEntry>,
    },
    /// Everything that was going to commit has committed. The response is
    /// rebuilt from the ledger and the stored plan, and **nothing is
    /// executed**.
    RegenerateResponse {
        /// The events the turn committed, in append order.
        events: Vec<StoredEvent>,
        /// The questions the turn planned, as answer tasks.
        answer_tasks: Vec<AnswerTask>,
    },
    /// An external effect may or may not have happened. It is settled against
    /// the remote system by attempt identifier, never repeated blindly (I15).
    ReconcileExternal {
        /// The attempts to settle, in the order they were made.
        attempts: Vec<AttemptId>,
        /// The journal entries that carry them.
        entries: Vec<CommandJournalEntry>,
    },
}

impl RecoveryAction {
    /// Returns `true` when acting on this decision may cause a domain effect.
    ///
    /// Only [`Self::ResumeCommands`] can, and even then only through the
    /// idempotency key.
    #[must_use]
    pub const fn may_cause_effects(&self) -> bool {
        matches!(self, Self::ResumeCommands { .. })
    }

    /// Stable snake-case label, for metrics and logs.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Nothing { .. } => "nothing",
            Self::RestartInterpretation => "restart_interpretation",
            Self::ResumeCommands { .. } => "resume_commands",
            Self::RegenerateResponse { .. } => "regenerate_response",
            Self::ReconcileExternal { .. } => "reconcile_external",
        }
    }
}

/// Reads what a crashed turn left behind and decides what to do about it.
#[derive(Clone)]
pub struct Recovery {
    conversations: Arc<dyn ConversationStore>,
    journal: Arc<dyn CommandJournal>,
    events: Arc<dyn EventJournal>,
    replay: Arc<dyn ReplayStore>,
}

impl fmt::Debug for Recovery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Recovery").finish_non_exhaustive()
    }
}

impl Recovery {
    /// Builds a recovery reader over the stores a turn writes to.
    #[must_use]
    pub fn new(
        conversations: Arc<dyn ConversationStore>,
        journal: Arc<dyn CommandJournal>,
        events: Arc<dyn EventJournal>,
        replay: Arc<dyn ReplayStore>,
    ) -> Self {
        Self {
            conversations,
            journal,
            events,
            replay,
        }
    }

    /// The unfinished turns, oldest first, for a sweep to work through.
    ///
    /// # Errors
    ///
    /// [`OrchestratorError::Store`] when the sweep could not read.
    pub async fn unfinished(
        &self,
        scope: RecoveryScope,
        limit: usize,
    ) -> Result<Vec<TurnPhaseMarker>, OrchestratorError> {
        self.conversations
            .list_unfinished_turns(scope, limit)
            .await
            .map_err(OrchestratorError::Store)
    }

    /// Decides what one turn needs (spec §23.1).
    ///
    /// # Errors
    ///
    /// [`OrchestratorError::Store`] when the phase marker or the journal could
    /// not be read. A turn that does not exist for `account` is
    /// [`StoreError::NotFound`], indistinguishable from another tenant's.
    pub async fn decide(
        &self,
        account: &AccountId,
        turn_id: &TurnId,
    ) -> Result<RecoveryAction, OrchestratorError> {
        let marker = self
            .conversations
            .turn_phase(account, turn_id)
            .await
            .map_err(OrchestratorError::Store)?;
        let entries = self
            .journal
            .for_turn(account, turn_id)
            .await
            .map_err(OrchestratorError::Store)?;

        // 1. Uncertainty first — before the phase is even consulted. A turn can
        //    be delivered, and truthfully so ("it is being verified"), while an
        //    effect it started is still unsettled. Letting the terminal phase
        //    answer first would drop that attempt on the floor (§16.5, I15).
        let unknown: Vec<CommandJournalEntry> = entries
            .iter()
            .filter(|entry| {
                entry.status == turnframe_store::journal::CommandJournalStatus::OutcomeUnknown
            })
            .cloned()
            .collect();
        if !unknown.is_empty() {
            let attempts = unknown
                .iter()
                .filter_map(|entry| match entry.result.as_ref() {
                    Some(turnframe_store::journal::JournalOutcome::OutcomeUnknown {
                        attempt_id,
                        ..
                    }) => Some(attempt_id.clone()),
                    _ => None,
                })
                .collect();
            return Ok(RecoveryAction::ReconcileExternal {
                attempts,
                entries: unknown,
            });
        }

        if marker.phase.is_terminal() {
            // Nothing outstanding, and the turn is finished.
            return Ok(RecoveryAction::Nothing {
                phase: marker.phase,
            });
        }

        // 2. Admitted and unsettled: resume by key.
        let pending: Vec<CommandJournalEntry> = entries
            .iter()
            .filter(|entry| entry.status.is_pending())
            .cloned()
            .collect();
        if !pending.is_empty() {
            return Ok(RecoveryAction::ResumeCommands { entries: pending });
        }

        // 3. Settled: the effects are done, the answer is not.
        if !entries.is_empty() {
            let record = self.replay.get(account, turn_id).await.ok();
            let text = self
                .conversations
                .load_turn(account, turn_id)
                .await
                .ok()
                .and_then(|turn| turn.user.input.text);
            let event_ids = committed_event_ids(&entries);
            let events = self
                .events
                .get_by_ids(account, &event_ids)
                .await
                .map_err(OrchestratorError::Store)?;
            return Ok(RecoveryAction::RegenerateResponse {
                events,
                answer_tasks: answer_tasks_of(record.as_ref(), text.as_deref()),
            });
        }

        // 4. Nothing was ever admitted.
        Ok(RecoveryAction::RestartInterpretation)
    }

    /// The stored turn, for a caller that regenerates a response.
    ///
    /// # Errors
    ///
    /// [`OrchestratorError::Store`]; a turn of another tenant is
    /// [`StoreError::NotFound`].
    pub async fn stored_turn(
        &self,
        account: &AccountId,
        turn_id: &TurnId,
    ) -> Result<StoredTurn, OrchestratorError> {
        self.conversations
            .load_turn(account, turn_id)
            .await
            .map_err(OrchestratorError::Store)
    }

    /// The replay record of a turn, when one was written.
    ///
    /// # Errors
    ///
    /// [`OrchestratorError::Store`] for anything but a missing record, which is
    /// reported as `Ok(None)`: a turn that crashed before its first record is a
    /// normal thing to find.
    pub async fn record(
        &self,
        account: &AccountId,
        turn_id: &TurnId,
    ) -> Result<Option<ReplayRecord>, OrchestratorError> {
        match self.replay.get(account, turn_id).await {
            Ok(record) => Ok(Some(record)),
            Err(StoreError::NotFound) => Ok(None),
            Err(error) => Err(OrchestratorError::Store(error)),
        }
    }

    /// Every event of one case since a revision, for a regeneration that needs
    /// more than the turn's own events.
    ///
    /// # Errors
    ///
    /// [`OrchestratorError::Store`].
    pub async fn case_events(
        &self,
        account: &AccountId,
        case_key: &turnframe_core::case::CaseKey,
        since: turnframe_core::ids::CaseRevision,
    ) -> Result<Vec<StoredEvent>, OrchestratorError> {
        self.events
            .list_since(account, case_key, since, MAX_REPLAYED_EVENTS)
            .await
            .map_err(OrchestratorError::Store)
    }
}

/// Every event identifier a turn's settled journal entries committed.
#[must_use]
pub fn committed_event_ids(entries: &[CommandJournalEntry]) -> Vec<EventId> {
    entries
        .iter()
        .filter_map(|entry| match entry.result.as_ref() {
            Some(turnframe_store::journal::JournalOutcome::Committed { event_ids, .. }) => {
                Some(event_ids.clone())
            }
            _ => None,
        })
        .flatten()
        .collect()
}

/// The answer tasks a stored replay record implies (spec §23.1), with the question words
/// read from the turn's `text`. The basis becomes the committed state, because the
/// turn's commands have already committed by the time this runs.
#[must_use]
pub fn answer_tasks_of(record: Option<&ReplayRecord>, text: Option<&str>) -> Vec<AnswerTask> {
    let (Some(record), Some(text)) = (record, text) else {
        return Vec::new();
    };
    let Some(understanding) = record.understanding.as_ref() else {
        return Vec::new();
    };
    understanding
        .questions
        .iter()
        .map(|question| AnswerTask {
            question_id: turnframe_core::ids::QuestionId::from(question.unit.to_string()),
            question: text
                .get(question.words.start..question.words.end)
                .unwrap_or_default()
                .to_owned(),
            basis: match question.basis {
                AnswerBasis::GeneralDomainKnowledge => AnswerBasis::GeneralDomainKnowledge,
                _ => AnswerBasis::CurrentCommittedState,
            },
            case_refs: record.loaded_cases.clone(),
            proposed_diff_ref: None,
            required_sources: if question.basis == AnswerBasis::GeneralDomainKnowledge {
                SourcePolicy::AnySource
            } else {
                SourcePolicy::AuthoritativeOnly
            },
            // What the workflow accepted and offered then is not on the record.
            enumerations: Vec::new(),
            capabilities: Vec::new(),
            continues_previous: question.continues_previous,
            asked_at: Some(turnframe_core::reduce::TextSpan {
                start_byte: question.words.start,
                end_byte: question.words.end,
            }),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use turnframe_core::ids::{AccountId, ConversationId};

    use super::*;

    #[test]
    fn answer_tasks_come_back_from_the_stored_plan() {
        let mut record = ReplayRecord::received(
            TurnId::nil(),
            ConversationId::nil(),
            AccountId::from("acct"),
            chrono::Utc::now(),
        );
        let text = "done. why the loyalty number?";
        record.understanding = Some(turnframe_core::understanding::Understanding {
            questions: vec![turnframe_core::understanding::UnderstoodQuestion {
                unit: turnframe_core::understanding::UnitId(2),
                words: turnframe_core::understanding::WordRange {
                    first: 1,
                    last: 4,
                    start: 6,
                    end: 29,
                },
                workflow: None,
                record: None,
                subjects: Vec::new(),
                basis: AnswerBasis::CommittedStateAfterTurn,
                topic: turnframe_core::understanding::QuestionTopic::default(),
                continues_previous: false,
            }],
            ..turnframe_core::understanding::Understanding::default()
        });
        let tasks = answer_tasks_of(Some(&record), Some(text));
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].question, "why the loyalty number?");
        assert_eq!(
            tasks[0].basis,
            AnswerBasis::CurrentCommittedState,
            "after commit, the state after the turn is the state now"
        );
    }

    #[test]
    fn a_record_that_was_never_written_implies_no_questions() {
        assert!(answer_tasks_of(None, Some("x")).is_empty());
    }

    #[test]
    fn only_resuming_commands_can_cause_an_effect() {
        assert!(
            RecoveryAction::ResumeCommands {
                entries: Vec::new()
            }
            .may_cause_effects()
        );
        assert!(!RecoveryAction::RestartInterpretation.may_cause_effects());
        assert!(
            !RecoveryAction::RegenerateResponse {
                events: Vec::new(),
                answer_tasks: Vec::new(),
            }
            .may_cause_effects()
        );
        assert!(
            !RecoveryAction::ReconcileExternal {
                attempts: Vec::new(),
                entries: Vec::new(),
            }
            .may_cause_effects()
        );
    }
}

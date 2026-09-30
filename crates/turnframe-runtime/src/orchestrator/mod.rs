//! The public facade: one turn, start to finish (spec §23, §29).
//!
//! [`Orchestrator::handle_turn`] runs the steps of §23 in order, and the order is the
//! contract: accept (A, B), load and project cases (D, E), judge a card answer (C),
//! issue tokens (F), understand (G), resolve and reduce (J, K), persist cards and
//! pending commands (L), execute and commit once (M, N), re-project and raise the
//! cards the cases now need (O, P), then answer and persist the turn as returned
//! (Q..V). Every step that can fail leaves a phase marker, so [`crate::recover`]
//! can tell an interrupted understanding from an interrupted commit.
//!
//! A turn that carries only a card answer is understood without a model call: its
//! meaning is the stored option.

mod builder;
mod directory;
mod session;

use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use indexmap::IndexMap;
use turnframe_core::case::CaseKey;
use turnframe_core::command::CommandBatch;
use turnframe_core::error::{OrchestratorError, StoreError};
use turnframe_core::flow::WorkflowRegistry;
use turnframe_core::ids::{AccountId, TurnId};
use turnframe_core::observe::{Observer, Signal, SignalLabels};
use turnframe_core::policy::PolicySnapshot;
use turnframe_core::replay::TurnPhase;
use turnframe_core::response::{AssistantTurn, ResponseBlock};
use turnframe_core::turn::{ActorContext, AttachmentSource, TurnInput};
use turnframe_store::stores::Stores;
use turnframe_understand::TurnUnderstander;

pub use self::builder::{BuildError, OrchestratorBuilder};
pub use self::directory::{CaseCandidate, CaseDirectory, StaticCaseDirectory};
use crate::attachments::AttachmentCopy;
use crate::compose::{Composer, CompositionInput};
use crate::config::OrchestratorConfig;
use crate::execute::CommandExecutor;
use crate::interactions::InteractionEngine;
use crate::planning::{PlannedTurn, SeededCase, SeededTurnPlanner, SharedPlanning, TurnPlanner};
use crate::policy::PolicyEngine;
use crate::recover::{Recovery, RecoveryAction};
use crate::reduce::NoticeCopy;
use crate::resolve::CaseIdFactory;
use crate::stream::{TurnPublisher, TurnStream};

/// Source of the runtime's own clock, so a turn is testable without waiting.
pub trait TurnClock: Send + Sync + fmt::Debug {
    /// Now.
    fn now(&self) -> DateTime<Utc>;
}

/// The system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemTurnClock;

impl TurnClock for SystemTurnClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// A clock stopped at one instant, for tests and replays.
#[derive(Debug, Clone, Copy)]
pub struct FixedTurnClock(pub DateTime<Utc>);

impl TurnClock for FixedTurnClock {
    fn now(&self) -> DateTime<Utc> {
        self.0
    }
}

/// What a turn's writes imply on OTHER cases (spec §23 step M).
///
/// Every hook that compiles a command sees one case, so «a fact that becomes true of
/// case A settles a question on case B» has only the application to say it. Asked
/// once per turn, after reduction, with the batches the turn carries; what it returns
/// executes in the same turn under the same journal and idempotency rules. It sees
/// writes, never words, so a consequence holds whether the turn came from a sentence,
/// a click or a replay; and it is not asked again about its own answer.
#[async_trait]
pub trait TurnConsequences: Send + Sync {
    /// The commands `batches` imply on other cases, or none.
    ///
    /// # Errors
    ///
    /// [`StoreError`] when the application could not read what it needed. The turn
    /// fails rather than executing half a rule.
    async fn following(
        &self,
        actor: &ActorContext,
        batches: &[CommandBatch<serde_json::Value>],
    ) -> Result<Vec<CommandBatch<serde_json::Value>>, StoreError>;
}

/// The runtime that answers a turn (spec §29).
pub struct Orchestrator {
    workflows: Arc<WorkflowRegistry>,
    stores: Stores,
    directory: Arc<dyn CaseDirectory>,
    consequences: Option<Arc<dyn TurnConsequences>>,
    understander: Arc<dyn TurnUnderstander>,
    composer: Composer,
    executor: CommandExecutor,
    interactions: InteractionEngine,
    recovery: Recovery,
    policy_engine: PolicyEngine,
    policy: PolicySnapshot,
    observer: Arc<dyn Observer>,
    attachment_source: Option<Arc<dyn AttachmentSource>>,
    clock: Arc<dyn TurnClock>,
    case_ids: Arc<dyn CaseIdFactory>,
    config: OrchestratorConfig,
    notice_copy: NoticeCopy,
    attachment_copy: AttachmentCopy,
    trace: Option<Arc<dyn crate::trace::TurnTrace>>,
}

impl fmt::Debug for Orchestrator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Orchestrator")
            .field("workflows", &self.workflows.len())
            .field("mode", &self.config.mode)
            .finish_non_exhaustive()
    }
}

impl Orchestrator {
    /// Starts a builder.
    #[must_use]
    pub fn builder() -> OrchestratorBuilder {
        OrchestratorBuilder::new()
    }

    /// The configuration in force.
    #[must_use]
    pub const fn config(&self) -> &OrchestratorConfig {
        &self.config
    }

    /// The persistence layer, for a caller that inspects what a turn wrote.
    #[must_use]
    pub const fn stores(&self) -> &Stores {
        &self.stores
    }

    /// The workflows this runtime was built with. An evaluation reads revisions back
    /// through them; a second registry would be a second answer to «which executor
    /// owns this workflow».
    #[must_use]
    pub const fn workflows(&self) -> &Arc<WorkflowRegistry> {
        &self.workflows
    }

    /// The crash-recovery reader (spec §23.1).
    #[must_use]
    pub const fn recovery(&self) -> &Recovery {
        &self.recovery
    }

    /// The interaction engine, for a caller that settles a card out of band.
    #[must_use]
    pub const fn interactions(&self) -> &InteractionEngine {
        &self.interactions
    }

    /// The same runtime with everything that writes taken away: it runs a turn's
    /// decision pipeline and cannot persist anything. See [`crate::planning`].
    #[must_use]
    pub fn planner(&self) -> TurnPlanner {
        TurnPlanner::assemble(
            self.workflows.read_only(),
            self.stores.read_only(),
            Arc::clone(&self.directory),
            self.shared_planning(),
        )
    }

    /// The same runtime planning from the state it is handed, with no persistence at
    /// all: what turns a recorded corpus into a deterministic shadow corpus.
    #[must_use]
    pub fn seeded_planner(&self) -> SeededTurnPlanner {
        crate::planning::seeded_from_parts(self.workflows.definitions(), self.shared_planning())
    }

    fn shared_planning(&self) -> SharedPlanning {
        SharedPlanning {
            understander: Arc::clone(&self.understander),
            policy_engine: self.policy_engine.clone(),
            policy: self.policy.clone(),
            config: self.config.clone(),
            clock: Arc::clone(&self.clock),
            case_ids: Arc::clone(&self.case_ids),
            observer: Arc::clone(&self.observer),
            knowledge: self.composer.has_knowledge(),
        }
    }

    /// Runs a turn through resolution, reduction and policy, and returns before the
    /// first side effect. Shorthand for `self.planner().plan(input)`.
    ///
    /// # Errors
    ///
    /// See [`TurnPlanner::plan`].
    pub async fn plan_turn(&self, input: TurnInput) -> Result<PlannedTurn, OrchestratorError> {
        self.planner().plan(input).await
    }

    /// The same, against the cases it is handed. Shorthand for
    /// `self.seeded_planner().plan(input, cases)`.
    ///
    /// # Errors
    ///
    /// See [`SeededTurnPlanner::plan`].
    pub async fn plan_turn_from(
        &self,
        input: TurnInput,
        cases: Vec<SeededCase>,
    ) -> Result<PlannedTurn, OrchestratorError> {
        self.seeded_planner().plan(input, cases).await
    }

    /// Handles one turn end to end (spec §23).
    ///
    /// # Errors
    ///
    /// The [`OrchestratorError`] family. The phase marker says how far the turn got,
    /// and a command that may have taken effect is in the journal.
    pub async fn handle_turn(&self, input: TurnInput) -> Result<AssistantTurn, OrchestratorError> {
        let publisher = TurnPublisher::null();
        self.run(input, &publisher).await
    }

    /// Handles one turn, publishing its events through `sink` under the §18.5 gate:
    /// understanding's steps as they happen, and nothing that states an outcome
    /// before the commit.
    ///
    /// # Errors
    ///
    /// See [`Self::handle_turn`].
    pub async fn handle_turn_streaming(
        &self,
        input: TurnInput,
        sink: Arc<dyn crate::stream::TurnSink>,
    ) -> Result<AssistantTurn, OrchestratorError> {
        let publisher = TurnPublisher::new(sink);
        self.run(input, &publisher).await
    }

    /// Handles one turn on a background task and streams its events (spec §18.5).
    #[must_use]
    pub fn stream_turn(self: Arc<Self>, input: TurnInput) -> TurnStream {
        let (stream, sink) = TurnStream::channel();
        let sink: Arc<dyn crate::stream::TurnSink> = Arc::new(sink);
        tokio::spawn(async move {
            let publisher = TurnPublisher::new(sink);
            match self.run(input, &publisher).await {
                Ok(turn) => publisher.completed(&turn),
                Err(error) => publisher.failed(error_code(&error)),
            }
        });
        stream
    }

    /// Decides what an unfinished turn needs, without acting on it (spec §23.1).
    ///
    /// # Errors
    ///
    /// [`OrchestratorError::Store`] when the phase marker or the journal could not be
    /// read.
    pub async fn plan_recovery(
        &self,
        account: &AccountId,
        turn_id: &TurnId,
    ) -> Result<RecoveryAction, OrchestratorError> {
        self.recovery.decide(account, turn_id).await
    }

    /// Acts on that decision, and finishes the turn where it can (spec §23.1): pending
    /// commands resume by idempotency key (I14), a committed turn regenerates only its
    /// answer, an unknown external outcome is handed back (§16.5).
    ///
    /// # Errors
    ///
    /// [`OrchestratorError::Store`] when the journal, the conversation or the commit
    /// could not be reached, and whatever composition returns.
    pub async fn resume_turn(
        &self,
        account: &AccountId,
        turn_id: &TurnId,
    ) -> Result<ResumeOutcome, OrchestratorError> {
        match self.recovery.decide(account, turn_id).await? {
            RecoveryAction::Nothing { phase } => Ok(ResumeOutcome::Nothing { phase }),
            RecoveryAction::RestartInterpretation => Ok(ResumeOutcome::Restartable),
            RecoveryAction::ReconcileExternal { attempts, .. } => {
                Ok(ResumeOutcome::AwaitingReconciliation { attempts })
            }
            RecoveryAction::RegenerateResponse {
                events,
                answer_tasks,
            } => {
                let turn = self
                    .regenerate(account, turn_id, &events, &answer_tasks)
                    .await?;
                Ok(match turn {
                    Some(turn) => ResumeOutcome::Regenerated {
                        turn: Box::new(turn),
                    },
                    None => ResumeOutcome::Nothing {
                        phase: TurnPhase::Delivered,
                    },
                })
            }
            RecoveryAction::ResumeCommands { entries } => {
                let stored = self.recovery.stored_turn(account, turn_id).await?;
                let batches = resume_batches(&stored.user.input.actor, *turn_id, &entries);
                let execution = self
                    .executor
                    .execute(account, &batches, self.clock.now())
                    .await?;
                let bundle = execution
                    .bundle()
                    .with_turn_phase(*turn_id, TurnPhase::Committed);
                self.executor.commit(account, bundle).await?;
                let events =
                    self.recovery.decide(account, turn_id).await.ok().and_then(
                        |action| match action {
                            RecoveryAction::RegenerateResponse {
                                events,
                                answer_tasks,
                            } => Some((events, answer_tasks)),
                            _ => None,
                        },
                    );
                let turn = match events {
                    Some((events, answer_tasks)) => {
                        self.regenerate(account, turn_id, &events, &answer_tasks)
                            .await?
                    }
                    None => None,
                };
                Ok(ResumeOutcome::Resumed {
                    outcomes: execution.outcomes.clone(),
                    turn: turn.map(Box::new),
                })
            }
        }
    }

    /// Rebuilds the answer of a committed turn from the ledger, and persists it.
    /// `None` when the turn already has one.
    async fn regenerate(
        &self,
        account: &AccountId,
        turn_id: &TurnId,
        events: &[turnframe_store::events::StoredEvent],
        answer_tasks: &[turnframe_core::reduce::AnswerTask],
    ) -> Result<Option<AssistantTurn>, OrchestratorError> {
        let stored = self.recovery.stored_turn(account, turn_id).await?;
        if stored.assistant.is_some() {
            return Ok(None);
        }
        // Grouped by the store, which carries a redaction through as one.
        let groups = turnframe_store::events::group_for_receipts(events);
        let interactions = self
            .interactions
            .open_for_conversation(account, &stored.user.input.conversation_id)
            .await
            .unwrap_or_default();
        let input = CompositionInput::new(&stored.user.input)
            .with_answer_tasks(answer_tasks)
            .with_ledger(&groups)
            .with_interactions(&interactions);
        let composition = self.composer.compose(input).await?;
        self.stores
            .conversations()
            .append_assistant_turn(account, composition.turn.clone())
            .await
            .map_err(OrchestratorError::Store)?;
        self.stores
            .conversations()
            .set_turn_phase(account, turn_id, TurnPhase::Delivered)
            .await
            .map_err(OrchestratorError::Store)?;
        Ok(Some(composition.turn))
    }

    async fn run(
        &self,
        input: TurnInput,
        publisher: &TurnPublisher,
    ) -> Result<AssistantTurn, OrchestratorError> {
        let labels =
            SignalLabels::none().with_effort(input.effort.unwrap_or(self.config.effort.default));
        self.observer
            .observe_labeled(&Signal::TurnReceived, &labels);
        let stage = crate::signals::Stage::enter();
        // A turn's state is large, and every caller awaits it: it lives on the heap.
        let outcome = Box::pin(session::Session::new(self, input, publisher).run()).await;
        stage.observe(self.observer.as_ref(), Signal::TurnDuration, &labels);
        match outcome {
            Ok(turn) => {
                self.observer
                    .observe_labeled(&Signal::TurnCompleted, &labels);
                Ok(turn)
            }
            Err(error) => {
                self.observer.observe_labeled(
                    &Signal::TurnFailed,
                    &labels.with_error_code(error_code(&error)),
                );
                Err(error)
            }
        }
    }
}

/// What acting on a recovery decision achieved (spec §23.1).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum ResumeOutcome {
    /// The turn was already finished.
    Nothing {
        /// The phase it is in.
        phase: TurnPhase,
    },
    /// Nothing was ever admitted, so the turn may simply be submitted again.
    Restartable,
    /// Pending commands were resumed by idempotency key.
    Resumed {
        /// What each of them ended up doing.
        outcomes: Vec<turnframe_core::replay::CommandOutcomeRecord>,
        /// The answer, when the resumed turn could be finished as well.
        turn: Option<Box<AssistantTurn>>,
    },
    /// The effects were already committed; only the answer was rebuilt.
    Regenerated {
        /// The answer, now persisted.
        turn: Box<AssistantTurn>,
    },
    /// An external effect may or may not have happened; only the application can
    /// settle it (§16.5, I15).
    AwaitingReconciliation {
        /// The attempts to settle.
        attempts: Vec<turnframe_core::ids::AttemptId>,
    },
}

/// Rebuilds the batches of a set of journal entries, one per case, so the executor
/// sees the very commands that were admitted (spec §23.1).
fn resume_batches(
    actor: &ActorContext,
    turn_id: TurnId,
    entries: &[turnframe_store::journal::CommandJournalEntry],
) -> Vec<CommandBatch<serde_json::Value>> {
    let mut batches: IndexMap<CaseKey, CommandBatch<serde_json::Value>> = IndexMap::new();
    for entry in entries {
        let key = entry.case_ref.key();
        let scope = turnframe_core::command::AtomicityScope::PerCase;
        let batch_id = turnframe_core::ids::BatchId::derive(&turn_id, &key, &scope);
        batches
            .entry(key)
            .or_insert_with(|| CommandBatch {
                batch_id,
                scope,
                envelopes: Vec::new(),
            })
            .envelopes
            .push(turnframe_core::command::CommandEnvelope {
                command_id: entry.command_id,
                turn_id,
                actor: actor.clone(),
                case_ref: entry.case_ref.clone(),
                idempotency_key: entry.idempotency_key.clone(),
                origin: entry.origin.clone(),
                command: entry.command_payload.clone(),
            });
    }
    batches.into_values().collect()
}

/// A stable code for an orchestrator failure, safe to put on the wire.
#[must_use]
pub fn error_code(error: &OrchestratorError) -> String {
    use turnframe_core::error::ErrorClassification;
    error.user_message_key().to_owned()
}

/// The model-authored text of a stored assistant turn, for the transcript.
pub(crate) fn assistant_text(turn: &AssistantTurn) -> String {
    reply_text(&turn.blocks)
}

/// The reply as the user read it: the transition, which carries the turn's answers, or the
/// answers alone when no transition was written.
fn reply_text(blocks: &[ResponseBlock]) -> String {
    let said = |transitions: bool| {
        blocks
            .iter()
            .filter_map(|block| match block {
                ResponseBlock::Transition(transition) if transitions => {
                    Some(transition.text.as_str())
                }
                ResponseBlock::Answer(answer) if !transitions => Some(answer.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ")
    };
    let reply = said(true);
    if reply.is_empty() { said(false) } else { reply }
}

#[cfg(test)]
mod tests {
    use super::*;
    use turnframe_core::ids::BlockId;
    use turnframe_core::plan::AnswerBasis;
    use turnframe_core::response::{AnswerStatus, GeneratedAnswer, GeneratedTransition};

    fn answer(text: &str) -> ResponseBlock {
        ResponseBlock::Answer(GeneratedAnswer {
            block_id: BlockId::from("answer:0"),
            question_id: None,
            text: text.to_owned(),
            basis: AnswerBasis::CurrentCommittedState,
            status: AnswerStatus::Answered,
            facts_used: Vec::new(),
            citations: Vec::new(),
            enumerations: Vec::new(),
        })
    }

    fn transition(text: &str) -> ResponseBlock {
        ResponseBlock::Transition(GeneratedTransition {
            block_id: BlockId::from("transition:0"),
            text: text.to_owned(),
            facts_used: Vec::new(),
        })
    }

    #[test]
    fn the_reply_is_read_back_once() {
        let said = "The airline pays for it.";
        assert_eq!(reply_text(&[answer(said), transition(said)]), said);
        assert_eq!(reply_text(&[answer(said)]), said);
    }
}

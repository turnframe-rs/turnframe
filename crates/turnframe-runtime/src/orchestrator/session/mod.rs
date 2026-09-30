//! One turn in flight: the steps of §23 as methods on the state they accumulate.
//!
//! | Module | Steps |
//! | --- | --- |
//! | [`load`] | A–F: accept, load and project cases, judge a click, read the conversation |
//! | [`understand`] | G–K: understand, admit a typed card answer, resolve and reduce |
//! | [`commit`] | L–N: execute, commit once, and record the turn |
//! | [`follow_up`] | O, P: the cases as they now stand and the cards they need |
//! | [`answer`] | Q–V: the facts and notices, composition and persistence |

mod answer;
mod commit;
mod follow_up;
mod load;
mod understand;

use chrono::{DateTime, Utc};
use indexmap::IndexMap;
use turnframe_core::case::{CaseKey, CaseRef};
use turnframe_core::error::OrchestratorError;
use turnframe_core::ids::{AccountId, BlockId, InteractionId};
use turnframe_core::interaction::Interaction;
use turnframe_core::replay::{ReplayRecord, TurnPhase};
use turnframe_core::response::{AssistantTurn, NarratableFact, ServerNotice};
use turnframe_core::turn::TurnInput;
use turnframe_core::understanding::ActId;
use turnframe_store::interaction::InteractionRecord;

use super::{CaseCandidate, Orchestrator, error_code};
use crate::attachments::TurnAttachments;
use crate::budget::{BudgetLimit, BudgetSpend, TurnBudget};
use crate::conversation::RecentMessage;
use crate::turn::LoadedCase;

/// Everything one turn accumulates.
pub(super) struct Session<'a> {
    runtime: &'a Orchestrator,
    input: TurnInput,
    publisher: &'a crate::stream::TurnPublisher,
    now: DateTime<Utc>,
    cases: IndexMap<CaseKey, LoadedCase>,
    open_interactions: Vec<Interaction>,
    attachments: TurnAttachments,
    /// The card this turn clicked again, when it had already been answered.
    replayed: Option<Box<InteractionRecord>>,
    /// Earlier messages, oldest first, and the reply just before this turn.
    recent: Vec<RecentMessage>,
    previous: Option<AssistantTurn>,
    artifacts_shown: Vec<BlockId>,
    /// What the last turn with a subject was about, for a turn that has none.
    carried_subjects: Vec<CaseRef>,
    origin_case: Option<CaseKey>,
    /// The case of a confirmation this turn committed before its dependents ran,
    /// admitted again when the cases are reloaded.
    confirmed_case: Option<CaseCandidate>,
    record: ReplayRecord,
    committed: bool,
    declined_instruction: bool,
    /// The decline, as a fact the narration may rest on beside its notice.
    declined_fact: Option<NarratableFact>,
    /// The act an answered card put in the understanding.
    card_act: Option<ActId>,
    /// The card this turn moved to `Resolving`, restored if nothing commits.
    resolving_card: Option<InteractionId>,
    /// Notices the runtime writes before reduction: a typed answer the card refused.
    early_notices: Vec<ServerNotice>,
    /// Workflows a request needed a record of when none existed: the reply offers to open one.
    none_yet: Vec<turnframe_core::ids::WorkflowKey>,
    /// The effort the turn runs at, resolved once.
    effort: crate::effort::EffortProfile,
}

impl<'a> Session<'a> {
    pub(super) fn new(
        runtime: &'a Orchestrator,
        input: TurnInput,
        publisher: &'a crate::stream::TurnPublisher,
    ) -> Self {
        let now = runtime.clock.now();
        let mut record = ReplayRecord::received(
            input.turn_id,
            input.conversation_id,
            input.actor.account_id.clone(),
            now,
        );
        let effort = crate::effort::resolve(
            &runtime.config,
            input.effort.unwrap_or(runtime.config.effort.default),
        );
        record.effort = effort.effort;
        Self {
            runtime,
            input,
            publisher,
            now,
            cases: IndexMap::new(),
            open_interactions: Vec::new(),
            attachments: TurnAttachments::default(),
            replayed: None,
            recent: Vec::new(),
            previous: None,
            artifacts_shown: Vec::new(),
            carried_subjects: Vec::new(),
            origin_case: None,
            confirmed_case: None,
            record,
            committed: false,
            declined_instruction: false,
            declined_fact: None,
            card_act: None,
            resolving_card: None,
            early_notices: Vec::new(),
            none_yet: Vec::new(),
            effort,
        }
    }

    fn account(&self) -> &AccountId {
        &self.input.actor.account_id
    }

    pub(super) async fn run(mut self) -> Result<AssistantTurn, OrchestratorError> {
        match self.pipeline().await {
            Ok(turn) => Ok(turn),
            Err(error) => {
                self.fail(&error).await;
                Err(error)
            }
        }
    }

    /// Records the failure where recovery can find it.
    ///
    /// `Failed` is terminal, so it is written only when nothing committed and no
    /// journal entry is left to settle; otherwise the phase stays where recovery
    /// resumes, reconciles or regenerates from (§16.5, §23.1).
    /// Reports `event` to the trace, when the runtime has one.
    fn trace(&self, event: &crate::trace::TraceEvent<'_>) {
        if let Some(trace) = &self.runtime.trace {
            trace.event(event);
        }
    }

    async fn fail(&mut self, error: &OrchestratorError) {
        if let (false, Some(card)) = (self.committed, self.resolving_card) {
            let _ = self
                .runtime
                .interactions
                .restore(self.account(), &card)
                .await;
        }
        if !self.committed && !self.has_unsettled_commands().await {
            self.record.phase = TurnPhase::Failed;
            let _ = self
                .runtime
                .stores
                .conversations()
                .set_turn_phase(self.account(), &self.input.turn_id, TurnPhase::Failed)
                .await;
        }
        self.record.recorded_at = self.now;
        let _ = self.runtime.stores.replay().put(self.record.clone()).await;
        let code = error_code(error);
        self.trace(&crate::trace::TraceEvent::Failed {
            turn: self.input.turn_id,
            code: &code,
            record: &self.record,
        });
        self.publisher.failed(code);
    }

    /// Whether the turn left a command somebody still has to settle. A journal that
    /// cannot be read counts as unsettled (I19).
    async fn has_unsettled_commands(&self) -> bool {
        match self
            .runtime
            .stores
            .journal()
            .for_turn(self.account(), &self.input.turn_id)
            .await
        {
            Ok(entries) => entries.iter().any(|entry| {
                entry.status.is_pending()
                    || entry.status
                        == turnframe_store::journal::CommandJournalStatus::OutcomeUnknown
            }),
            Err(_) => true,
        }
    }

    async fn pipeline(&mut self) -> Result<AssistantTurn, OrchestratorError> {
        // A, B.
        self.input
            .validate_shape_within(&self.runtime.config.understanding.turn_limits)?;
        self.publisher.phase_reached(TurnPhase::Received);
        self.trace(&crate::trace::TraceEvent::Received { input: &self.input });
        self.accept().await?;
        // D, E, then C against the revision just read.
        self.load_cases().await?;
        let mut answered = self.admit_response().await?;
        self.load_conversation().await;
        self.gather_attachments().await;
        // A confirmation with dependents commits first, so they reduce against it.
        let settled = self.settle_prerequisite(answered.as_ref()).await?;

        // F, G, and the records the message named that the turn did not have.
        let definitions = self.runtime.workflows.definitions();
        let mut resolver = self.resolver(answered.as_ref());
        let mut operations = crate::understand::operation_catalog(&definitions, &self.cases)?;
        let mut understanding = self
            .understand(&resolver, &operations, answered.is_some())
            .await?;
        if self
            .find_unlisted(
                &mut understanding,
                &mut resolver,
                answered.as_ref(),
                &operations,
            )
            .await?
        {
            operations = crate::understand::operation_catalog(&definitions, &self.cases)?;
        }
        self.publisher.phase_reached(TurnPhase::Interpreted);
        self.check_budget()?;
        if answered.is_none() {
            answered = self.admit_typed_answer(&understanding).await?;
        }

        // J, K.
        let understanding = self.with_card_acts(understanding, answered.as_ref(), &resolver);
        self.trace(&crate::trace::TraceEvent::Understood {
            turn: self.input.turn_id,
            understanding: &understanding,
        });
        let reduced = self.reduce(&understanding, &resolver, &operations, answered.as_ref())?;
        self.trace(&crate::trace::TraceEvent::Reduced {
            turn: self.input.turn_id,
            plan: &reduced.plan,
        });
        self.publisher.phase_reached(TurnPhase::Reduced);
        self.check_budget()?;

        // L..N.
        let (execution, persisted) = self.execute(&reduced, answered.as_ref(), settled).await?;

        // O..V.
        self.answer(&reduced, &execution, &persisted).await
    }

    /// The budget this turn runs under, when its mode carries one (§11.1).
    fn turn_budget(&self) -> Option<TurnBudget> {
        self.runtime
            .config
            .mode
            .budget()
            .map(|budget| TurnBudget::new(*budget, self.now))
    }

    /// The first bound the turn has spent, if any: every model task of the turn counts.
    fn spent_limit(&self) -> Option<BudgetLimit> {
        let spent_so_far = self.record.budget.clone().unwrap_or_default();
        let spent = BudgetSpend::none()
            .with_model_calls(u64::from(spent_so_far.model_calls))
            .with_prompt_tokens(spent_so_far.prompt_tokens);
        self.turn_budget()?
            .exhausted(spent, self.runtime.clock.now())
    }

    /// Stops the turn when its budget ran out. Called only where nothing has been
    /// written, so failing closed leaves no effect.
    fn check_budget(&self) -> Result<(), OrchestratorError> {
        let Some(limit) = self.spent_limit() else {
            return Ok(());
        };
        tracing::warn!(
            target: "turnframe.orchestrator",
            limit = limit.as_str(),
            "the turn spent its resource budget and stopped before anything ran"
        );
        Err(limit.into_error())
    }
}

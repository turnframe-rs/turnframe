//! Response composition: what the assistant is allowed to say (spec §10, §17.3, §18,
//! §23 steps Q to U).
//!
//! By the time this runs everything that could happen has happened, and the job is to
//! say so accurately. Receipts, notices and cards are rendered by the server from what
//! committed; the words around them are written by the narration tasks from facts code
//! selected, and [`claim_guard::verify`] checks the assembled
//! turn against the record before it is returned. Every [`AnswerTask`] produces exactly
//! one block, answered or explicitly not.
//!
//! | Module | What it holds |
//! | --- | --- |
//! | [`copy`] | the notice codes and the copy composition writes itself |
//! | `answers` | one block per question |

mod answers;
pub mod copy;

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use turnframe_core::case::CaseKey;
use turnframe_core::error::OrchestratorError;
use turnframe_core::event::{OperationalReceipt, ReceiptEvent};
use turnframe_core::flow::{ErasedWorkflowView, WorkflowRegistry, WritingStage};
use turnframe_core::hash::derive_uuid;
use turnframe_core::ids::{BlockId, TurnId};
use turnframe_core::interaction::Interaction;
use turnframe_core::knowledge::KnowledgeProvider;
use turnframe_core::locale::{Locale, LocalizedText};
use turnframe_core::reduce::AnswerTask;
use turnframe_core::replay::{BudgetReport, TaskRecord};
use turnframe_core::response::{
    AnswerStatus, ArtifactView, AssistantTurn, CaseLabel, Expectation, GeneratedTransition,
    InteractionBlock, NarratableFact, NoticeSeverity, ReceiptBlock, ReplayToken, ResponseBlock,
    ServerNotice, claim_guard,
};
use turnframe_core::turn::TurnInput;
use turnframe_provider::capabilities::CapabilityRequirements;
use turnframe_provider::request::ContentPart;
use turnframe_provider::router::{ProviderRouter, RoutingPolicy};
use turnframe_store::events::{EventBatch, LedgerReceiptGroup};
use turnframe_tasks::{TaskEngine, TaskKind, TaskScope};

pub use self::copy::{CompositionCopy, notice};
use crate::config::NarrationConfig;
use crate::conversation::{RecentMessage, UnavailableWorkflow};
use crate::narrate::Narrator;
pub use crate::narrate::outcome::AskCopy;
use crate::narrate::outcome::{Material, TurnOutcome};
use crate::narrate::tasks::AcknowledgeInput;

/// Domain separation of the derived replay tokens.
const REPLAY_TOKEN_DOMAIN: &str = "turnframe.replay_token.v1";

/// How many earlier messages the acknowledgement is shown.
const TRANSCRIPT_WINDOW: usize = 4;

/// What the one reply gives beside the outcome.
struct Carried<'a> {
    answers: &'a [String],
    unanswered: &'a [String],
    notices: &'a [String],
}

/// Derives the opaque token a client uses to fetch a turn's replay record; derived, so a
/// regenerated response (spec §23.1) carries the token of the one that was lost.
#[must_use]
pub fn derive_replay_token(turn_id: &TurnId) -> ReplayToken {
    ReplayToken::from(
        derive_uuid(REPLAY_TOKEN_DOMAIN, &[&turn_id.to_string()])
            .simple()
            .to_string(),
    )
}

/// Everything one composition reads.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CompositionInput<'a> {
    /// The turn being answered.
    pub turn: &'a TurnInput,
    /// The turn's files, for the stage that answers.
    pub attachments: Vec<ContentPart>,
    /// The questions the reducer produced, with their basis.
    pub answer_tasks: &'a [AnswerTask],
    /// Event batches that committed, in commit order.
    pub events: &'a [EventBatch],
    /// Ledger groups read back from the journal, used instead of [`Self::events`]
    /// when a response is regenerated: a stored event may have been redacted since.
    pub ledger: &'a [LedgerReceiptGroup],
    /// Cards on screen after the turn.
    pub interactions: &'a [Interaction],
    /// The cases as they now stand.
    pub views: &'a [ErasedWorkflowView],
    /// The cases the turn was about.
    pub subjects: &'a [CaseKey],
    /// The cases an act of the turn reached, in act order: where the ask comes from.
    pub touched: &'a [CaseKey],
    /// Other cases in view the ask may move on to once the touched ones need nothing.
    pub beside: &'a [CaseKey],
    /// Cases in view only because the actor may reach them.
    pub reachable_only: &'a [CaseKey],
    /// The earlier messages, oldest first.
    pub recent: &'a [RecentMessage],
    /// The reply just before this turn.
    pub preceding_reply: Option<&'a str>,
    /// Artifact blocks the conversation already carries.
    pub artifacts_shown: &'a [BlockId],
    /// What each case is called.
    pub case_labels: &'a [CaseLabel],
    /// What each case lets the user do next once it owes nothing, by case.
    pub next_steps: &'a [(CaseKey, Vec<LocalizedText>)],
    /// Workflows the turn named.
    pub named_workflows: &'a [WorkflowKey],
    /// Workflows that cannot start, with their reasons.
    pub unavailable: &'a [UnavailableWorkflow],
    /// What the user said the assistant got wrong.
    pub disputes: &'a [String],
    /// Workflows the turn started without writing anything yet.
    pub started: &'a [WorkflowKey],
    /// Receipts of the last reply the user contested, as they were shown.
    pub contested: &'a [String],
    /// Notices the turn already carries.
    pub notices: &'a [ServerNotice],
    /// What the reduction and the execution decided, as facts.
    pub refusals: &'a [NarratableFact],
    /// Deterministic outcome flags.
    pub outcomes: OutcomeFlags,
    /// The effort the turn runs at: its reply budget and task profiles.
    pub effort: Option<&'a crate::effort::EffortProfile>,
}

use turnframe_core::ids::WorkflowKey;

/// The outcomes that raise a notice of composition's own.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct OutcomeFlags {
    /// A command did not commit.
    pub had_failure: bool,
    /// An external outcome is unknown.
    pub outcome_unknown: bool,
    /// A command met a moved revision.
    pub revision_conflict: bool,
    /// A card could not be written.
    pub interaction_unavailable: bool,
    /// A touched case could not be read back.
    pub case_refresh_unavailable: bool,
    /// The turn's budget is spent: no model writes anything.
    pub budget_exhausted: bool,
    /// The user declined the instruction a card guarded.
    pub instruction_declined: bool,
}

impl<'a> CompositionInput<'a> {
    /// An input with nothing but the turn.
    #[must_use]
    pub fn new(turn: &'a TurnInput) -> Self {
        Self {
            turn,
            attachments: Vec::new(),
            answer_tasks: &[],
            events: &[],
            ledger: &[],
            interactions: &[],
            views: &[],
            subjects: &[],
            touched: &[],
            beside: &[],
            reachable_only: &[],
            recent: &[],
            preceding_reply: None,
            artifacts_shown: &[],
            case_labels: &[],
            next_steps: &[],
            named_workflows: &[],
            unavailable: &[],
            disputes: &[],
            started: &[],
            contested: &[],
            notices: &[],
            refusals: &[],
            outcomes: OutcomeFlags::default(),
            effort: None,
        }
    }
}

macro_rules! setters {
    ($($(#[$doc:meta])* $name:ident: $field:ident: $ty:ty;)*) => {
        impl<'a> CompositionInput<'a> {
            $(
                $(#[$doc])*
                #[must_use]
                pub fn $name(mut self, value: $ty) -> Self {
                    self.$field = value;
                    self
                }
            )*
        }
    };
}

setters! {
    /// Sets the turn's files.
    with_attachments: attachments: Vec<ContentPart>;
    /// Sets the questions.
    with_answer_tasks: answer_tasks: &'a [AnswerTask];
    /// Sets the committed events.
    with_events: events: &'a [EventBatch];
    /// Sets the ledger groups of a regenerated response.
    with_ledger: ledger: &'a [LedgerReceiptGroup];
    /// Sets the cards on screen.
    with_interactions: interactions: &'a [Interaction];
    /// Sets the views.
    with_views: views: &'a [ErasedWorkflowView];
    /// Sets the subjects.
    with_subjects: subjects: &'a [CaseKey];
    /// Sets the cases an act reached.
    with_touched: touched: &'a [CaseKey];
    /// Sets the cases the ask may move on to.
    with_beside: beside: &'a [CaseKey];
    /// Sets the cases in view only because they are reachable.
    with_reachable_only: reachable_only: &'a [CaseKey];
    /// Sets the earlier messages.
    with_recent: recent: &'a [RecentMessage];
    /// Sets the reply just before this turn.
    with_preceding_reply: preceding_reply: Option<&'a str>;
    /// Sets the artifact blocks already shown.
    with_artifacts_shown: artifacts_shown: &'a [BlockId];
    /// Sets the case labels.
    with_case_labels: case_labels: &'a [CaseLabel];
    /// Sets what each case lets the user do next.
    with_next_steps: next_steps: &'a [(CaseKey, Vec<LocalizedText>)];
    /// Sets the workflows the turn named.
    with_named_workflows: named_workflows: &'a [WorkflowKey];
    /// Sets the workflows that cannot start.
    with_unavailable: unavailable: &'a [UnavailableWorkflow];
    /// Sets what the user disputed.
    with_disputes: disputes: &'a [String];
    /// Sets the workflows started without a write.
    with_started: started: &'a [WorkflowKey];
    /// Sets the receipts the user contested.
    with_contested: contested: &'a [String];
    /// Sets the notices.
    with_notices: notices: &'a [ServerNotice];
    /// Sets the facts of what was decided.
    with_refusals: refusals: &'a [NarratableFact];
    /// Sets the outcome flags.
    with_outcomes: outcomes: OutcomeFlags;
    /// Sets the effort the turn runs at.
    with_effort: effort: Option<&'a crate::effort::EffortProfile>;
}

/// A composed turn and the model calls that wrote it.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Composition {
    /// The turn, as it is returned and persisted.
    pub turn: AssistantTurn,
    /// Every narration task call.
    pub tasks: Vec<TaskRecord>,
    /// What those calls spent.
    pub budget: BudgetReport,
    /// What the reply asked for, for the next turn to expect.
    pub expectation: Option<Expectation>,
}

/// Builds the ordered response blocks of one turn (spec §18.1).
#[derive(Clone)]
pub struct Composer {
    workflows: Arc<WorkflowRegistry>,
    router: Arc<dyn ProviderRouter>,
    engine: TaskEngine,
    knowledge: Option<Arc<dyn KnowledgeProvider>>,
    narration: NarrationConfig,
    copy: CompositionCopy,
    ask_copy: AskCopy,
    max_chunks: Option<usize>,
}

impl fmt::Debug for Composer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Composer")
            .field("narration", &self.narration)
            .field("knowledge", &self.knowledge.is_some())
            .finish_non_exhaustive()
    }
}

impl Composer {
    /// A composer whose narration tasks run over `router` with the shipped profiles.
    #[must_use]
    pub fn new(
        workflows: Arc<WorkflowRegistry>,
        router: Arc<dyn ProviderRouter>,
        narration: NarrationConfig,
    ) -> Self {
        Self {
            engine: TaskEngine::builder(Arc::clone(&router)).build(),
            workflows,
            router,
            knowledge: None,
            narration,
            copy: CompositionCopy::standard(),
            ask_copy: AskCopy::standard(),
            max_chunks: None,
        }
    }

    /// Runs the narration tasks on `engine`: its profiles, prompts and records.
    #[must_use]
    pub fn with_tasks(mut self, engine: TaskEngine) -> Self {
        self.engine = engine;
        self
    }

    /// Whether a model that answers can be shown `part`.
    #[must_use]
    pub fn carries(&self, part: &ContentPart) -> bool {
        let requirements = match part {
            ContentPart::Image { .. } => CapabilityRequirements::none().with_vision(),
            ContentPart::Document { .. } => CapabilityRequirements::none().with_documents(),
            _ => return true,
        };
        self.router
            .select(TaskKind::Answer, &requirements, &RoutingPolicy::new())
            .is_ok()
    }

    /// Attaches a knowledge provider (spec §19.2).
    #[must_use]
    pub fn with_knowledge(mut self, knowledge: Arc<dyn KnowledgeProvider>) -> Self {
        self.knowledge = Some(knowledge);
        self
    }

    /// Replaces the copy composition writes itself. English and Italian by default; a
    /// [`LocalizedText`] adds languages with `with`, and the copy's `translated` a whole
    /// language at once.
    #[must_use]
    pub fn with_copy(mut self, copy: CompositionCopy) -> Self {
        self.copy = copy;
        self
    }

    /// Replaces the questions code writes when no model does.
    #[must_use]
    pub fn with_ask_copy(mut self, copy: AskCopy) -> Self {
        self.ask_copy = copy;
        self
    }

    /// The server's own sentences this composer writes, for the languages check.
    pub(crate) fn server_copy(&self) -> [&dyn crate::copy::ServerCopy; 2] {
        [&self.copy, &self.ask_copy]
    }

    /// Sets how many knowledge chunks one answer may retrieve.
    #[must_use]
    pub const fn with_max_chunks(mut self, max_chunks: Option<usize>) -> Self {
        self.max_chunks = max_chunks;
        self
    }

    /// Renders receipts from a ledger read, carrying every erasure through.
    ///
    /// # Errors
    ///
    /// [`OrchestratorError::Erasure`] when a workflow cannot read back its own events.
    pub fn ledger_receipts(
        &self,
        groups: &[LedgerReceiptGroup],
        locale: &Locale,
    ) -> Result<Vec<OperationalReceipt>, OrchestratorError> {
        let mut receipts = Vec::new();
        let mut seen = BTreeSet::new();
        for group in groups {
            let registered = self.workflows.require(&group.case_key.workflow)?;
            for receipt in registered.definition.receipts(&group.events, locale)? {
                if seen.insert(receipt.receipt_id) {
                    receipts.push(receipt);
                }
            }
        }
        Ok(receipts)
    }

    /// Renders the receipts of a turn from the events that just committed (§17.3,
    /// I16). Deterministic, so a regenerated response is the one that was lost.
    ///
    /// # Errors
    ///
    /// [`OrchestratorError::Erasure`] when a workflow cannot read back its own events.
    pub fn receipts(
        &self,
        events: &[EventBatch],
        locale: &Locale,
    ) -> Result<Vec<OperationalReceipt>, OrchestratorError> {
        let mut receipts = Vec::new();
        let mut seen = BTreeSet::new();
        for batch in events {
            let registered = self.workflows.require(&batch.case_key.workflow)?;
            let committed: Vec<ReceiptEvent<serde_json::Value>> = batch
                .events
                .iter()
                .cloned()
                .map(ReceiptEvent::Committed)
                .collect();
            for receipt in registered.definition.receipts(&committed, locale)? {
                if seen.insert(receipt.receipt_id) {
                    receipts.push(receipt);
                }
            }
        }
        Ok(receipts)
    }

    /// Says each step `steps` delivers as it arrives, concurrently, and returns the
    /// calls made. Ends when the sender is dropped.
    pub(crate) async fn say_steps(
        &self,
        steps: futures::channel::mpsc::UnboundedReceiver<turnframe_understand::Step>,
        locale: &turnframe_core::locale::Locale,
        turn: turnframe_core::ids::TurnId,
        publisher: &crate::stream::TurnPublisher,
    ) -> Vec<turnframe_core::replay::TaskRecord> {
        use futures::StreamExt as _;
        let scope =
            TaskScope::new(self.narration.budget, locale.clone()).for_turn(turn.to_string());
        let narrator = Narrator {
            engine: &self.engine,
            scope: &scope,
            max_chars: None,
        };
        steps
            .for_each_concurrent(None, |step| {
                let narrator = &narrator;
                async move {
                    if let Some(text) = narrator.step(locale.as_str(), &step.describe()).await {
                        publisher.step_said(step, text);
                    }
                }
            })
            .await;
        scope.records()
    }

    /// Whether a model writes this turn's prose: not past the budget, and not when the
    /// state a failure left could not be read back, since the prose would guess it.
    fn narrates(&self, input: &CompositionInput<'_>) -> bool {
        self.narration.enabled
            && !input.outcomes.budget_exhausted
            && !input.outcomes.case_refresh_unavailable
    }

    /// The workflow's guidance for `stage` on the views of `cases`.
    fn guidance(
        &self,
        input: &CompositionInput<'_>,
        cases: &[CaseKey],
        stage: WritingStage,
    ) -> Vec<String> {
        input
            .views
            .iter()
            .filter(|view| cases.contains(&view.case_ref.key()))
            .filter_map(|view| {
                let workflow = self.workflows.require(&view.case_ref.workflow).ok()?;
                workflow
                    .definition
                    .narration_briefing(stage, view)
                    .ok()
                    .flatten()
            })
            .collect()
    }

    /// Composes the whole answer (spec §23 steps Q to T).
    ///
    /// # Errors
    ///
    /// [`OrchestratorError::Erasure`] when receipts cannot be rendered, and
    /// [`OrchestratorError::Internal`] when the assembled turn fails
    /// [`claim_guard::verify`], a defect in this module.
    pub async fn compose(
        &self,
        input: CompositionInput<'_>,
    ) -> Result<Composition, OrchestratorError> {
        let locale = &input.turn.locale;
        let receipts = if input.ledger.is_empty() {
            self.receipts(input.events, locale)?
        } else {
            self.ledger_receipts(input.ledger, locale)?
        };
        let mut scope = TaskScope::new(
            input
                .effort
                .map_or(self.narration.budget, |effort| effort.reply_budget),
            locale.clone(),
        )
        .for_turn(input.turn.turn_id.to_string());
        if let Some(effort) = input.effort {
            scope = scope
                .with_profiles(effort.tasks.clone())
                .with_effort(effort.effort);
        }
        let narrator = Narrator {
            engine: &self.engine,
            scope: &scope,
            max_chars: self.narration.max_answer_chars,
        };
        // A workflow the turn named and cannot start says why, in its own words.
        let mut facts = input.refusals.to_vec();
        facts.extend(
            input
                .unavailable
                .iter()
                .filter(|blocked| input.named_workflows.contains(&blocked.workflow))
                .map(|blocked| NarratableFact::WorkflowUnavailable {
                    workflow: blocked.workflow.clone(),
                    reason: blocked.reason.clone(),
                }),
        );
        let outcome = Material {
            receipts: &receipts,
            facts: &facts,
            interactions: input.interactions,
            views: input.views,
            touched: input.touched,
            beside: input.beside,
            labels: input.case_labels,
            disputes: input.disputes,
            started: input.started,
            contested: input.contested,
            next_steps: input.next_steps,
            locale,
            copy: &self.ask_copy,
        }
        .outcome();

        let answers = answers::Answers {
            composer: self,
            narrator: &narrator,
            input: &input,
        };
        let answered = answers.all().await;
        let notices = self.notices(&input);
        let mut blocks: Vec<ResponseBlock> = Vec::new();
        let mut asked = false;
        if self.narrates(&input) {
            // What the facts answered is given; a question they do not answer is named, so
            // the reply says so in its own words, or owns a mistake the user points at.
            let answer_texts: Vec<String> = answered
                .iter()
                .filter(|answer| answer.status == AnswerStatus::Answered)
                .map(|answer| answer.text.clone())
                .collect();
            let unanswered: Vec<String> = answered
                .iter()
                .filter(|answer| answer.status != AnswerStatus::Answered)
                .map(|answer| {
                    let question = input
                        .answer_tasks
                        .iter()
                        .find(|task| Some(&task.question_id) == answer.question_id.as_ref())
                        .map_or("", |task| task.question.as_str());
                    format!("«{question}»")
                })
                .collect();
            let notice_texts: Vec<String> = notices
                .iter()
                .map(|notice| notice.text.resolve(locale).to_owned())
                .collect();
            let only_answers = !crate::narrate::speaks(&outcome)
                && notice_texts.is_empty()
                && unanswered.is_empty();
            // The turn's one reply: the answers alone as written, else a reply that gives
            // what was done, the answers, the notices and the ask.
            let reply = if only_answers {
                (!answer_texts.is_empty()).then(|| answer_texts.join("\n\n"))
            } else if let Some(text) = self
                .acknowledge(
                    &input,
                    &outcome,
                    Carried {
                        answers: &answer_texts,
                        unanswered: &unanswered,
                        notices: &notice_texts,
                    },
                    &narrator,
                )
                .await
            {
                asked = outcome.ask.is_some();
                Some(text)
            } else {
                // Code's own words stand in for a reply that failed, in the reply's order.
                let done: Vec<&str> = receipts
                    .iter()
                    .map(|receipt| receipt.body.resolve(locale))
                    .collect();
                let mut parts: Vec<String> = Vec::new();
                if !done.is_empty() {
                    parts.push(done.join(" "));
                }
                parts.extend(answered.iter().map(|answer| answer.text.clone()));
                parts.extend(notice_texts);
                if let Some(ask) = &outcome.ask {
                    asked = true;
                    parts.push(ask.question.clone());
                }
                (!parts.is_empty()).then(|| parts.join("\n\n"))
            };
            if let Some(text) = reply {
                blocks.push(self.transition(text, &receipts, &answered, &input));
            }
        }
        blocks.extend(answered.into_iter().map(ResponseBlock::Answer));

        for receipt in &receipts {
            blocks.push(ResponseBlock::Receipt(ReceiptBlock {
                block_id: BlockId::from(format!("receipt:{}", receipt.receipt_id)),
                receipt: receipt.clone(),
            }));
        }
        self.artifacts(&input, &mut blocks);
        blocks.extend(notices.into_iter().map(ResponseBlock::Notice));
        // The cards last: what happened is read before what is asked.
        for interaction in input.interactions {
            blocks.push(ResponseBlock::Interaction(InteractionBlock {
                block_id: BlockId::from(format!("interaction:{}", interaction.id)),
                view: interaction.view(),
            }));
        }

        let turn = AssistantTurn {
            turn_id: input.turn.turn_id,
            conversation_id: input.turn.conversation_id,
            blocks,
            subjects: input
                .views
                .iter()
                .filter(|view| input.subjects.contains(&view.case_ref.key()))
                .map(|view| view.case_ref.clone())
                .collect(),
            replay_token: derive_replay_token(&input.turn.turn_id),
            expectations: Vec::new(),
            done: Vec::new(),
        };
        claim_guard::verify(&turn).map_err(|violation| {
            tracing::error!(
                target: "turnframe.compose",
                violation = %violation,
                "composed turn failed the claim guard"
            );
            OrchestratorError::Internal {
                code: "claim_guard".to_owned(),
            }
        })?;
        Ok(Composition {
            turn,
            tasks: scope.records(),
            budget: scope.budget_report(),
            expectation: outcome
                .ask
                .filter(|_| asked)
                .and_then(|ask| ask.expectation),
        })
    }

    async fn acknowledge(
        &self,
        input: &CompositionInput<'_>,
        outcome: &TurnOutcome,
        carried: Carried<'_>,
        narrator: &Narrator<'_>,
    ) -> Option<String> {
        let locale = &input.turn.locale;
        let window = input.recent.len().saturating_sub(TRANSCRIPT_WINDOW);
        let guidance = self.guidance(input, input.touched, WritingStage::Transition);
        let on_screen: Vec<String> = outcome.card.clone().into_iter().collect();
        // Beside something left undone, the words that asked for it read as done; and
        // a question's words, shown, get answered twice.
        let message = input
            .turn
            .text
            .as_deref()
            .filter(|_| outcome.not_done.is_empty())
            .and_then(|text| without_questions(text, input.answer_tasks));
        let acknowledge = AcknowledgeInput {
            outcome,
            locale: locale.as_str(),
            tone: self.narration.tone,
            message: message.as_deref(),
            on_screen: &on_screen,
            answers: carried.answers,
            unanswered: carried.unanswered,
            notices: carried.notices,
            transcript: &input.recent[window..],
            guidance: &guidance,
        };
        narrator.acknowledge(&acknowledge).await
    }

    /// The acknowledgement block, citing the receipts and cards it stands beside.
    fn transition(
        &self,
        text: String,
        receipts: &[OperationalReceipt],
        answered: &[turnframe_core::response::GeneratedAnswer],
        input: &CompositionInput<'_>,
    ) -> ResponseBlock {
        let mut facts_used: Vec<NarratableFact> = receipts
            .iter()
            .map(|receipt| NarratableFact::OperationalOutcome {
                receipt_id: receipt.receipt_id,
                event_ids: receipt.event_ids.clone(),
                status_code: receipt.status_code.clone(),
            })
            .collect();
        facts_used.extend(input.interactions.iter().map(|interaction| {
            NarratableFact::InteractionAvailable {
                interaction_id: interaction.id,
                interaction_kind: interaction.kind,
            }
        }));
        facts_used.extend(input.refusals.iter().cloned());
        // The answers it gives rest on the facts they rest on.
        for answer in answered {
            for fact in &answer.facts_used {
                if !facts_used.contains(fact) {
                    facts_used.push(fact.clone());
                }
            }
        }
        ResponseBlock::Transition(GeneratedTransition {
            block_id: BlockId::from("transition:0"),
            text,
            facts_used,
        })
    }

    /// The documents of the turn's subjects, each once per revision.
    fn artifacts(&self, input: &CompositionInput<'_>, blocks: &mut Vec<ResponseBlock>) {
        for view in input.views {
            if !input.subjects.contains(&view.case_ref.key()) {
                continue;
            }
            let Ok(workflow) = self.workflows.require(&view.case_ref.workflow) else {
                continue;
            };
            match workflow.definition.artifacts(view) {
                Ok(artifacts) => {
                    for artifact in artifacts {
                        let block_id = BlockId::from(format!(
                            "artifact:{}:{}:{}:{}",
                            view.case_ref.workflow,
                            view.case_ref.case_id,
                            view.case_ref.expected_revision,
                            artifact.artifact_id
                        ));
                        if !input.artifacts_shown.contains(&block_id) {
                            blocks
                                .push(ResponseBlock::Artifact(ArtifactView { block_id, artifact }));
                        }
                    }
                }
                Err(error) => tracing::warn!(
                    target: "turnframe.compose",
                    workflow = view.case_ref.workflow.as_str(),
                    error = %error,
                    "a workflow's artifacts could not be read from its own view"
                ),
            }
        }
    }

    /// The turn's notices, then composition's own, each code once.
    fn notices(&self, input: &CompositionInput<'_>) -> Vec<ServerNotice> {
        let flags = input.outcomes;
        let copy = &self.copy;
        let own = [
            (
                flags.revision_conflict,
                notice::REVISION_CONFLICT,
                NoticeSeverity::Warning,
                &copy.revision_conflict,
            ),
            (
                flags.had_failure,
                notice::COMMAND_FAILED,
                NoticeSeverity::Error,
                &copy.command_failed,
            ),
            (
                flags.outcome_unknown,
                notice::VERIFICATION_IN_PROGRESS,
                NoticeSeverity::Warning,
                &copy.verification_in_progress,
            ),
            (
                flags.interaction_unavailable,
                notice::INTERACTION_UNAVAILABLE,
                NoticeSeverity::Error,
                &copy.interaction_unavailable,
            ),
            (
                flags.case_refresh_unavailable,
                notice::CASE_REFRESH_UNAVAILABLE,
                NoticeSeverity::Error,
                &copy.case_refresh_unavailable,
            ),
            (
                flags.instruction_declined,
                notice::INSTRUCTION_DECLINED,
                NoticeSeverity::Info,
                &copy.instruction_declined,
            ),
            (
                flags.budget_exhausted,
                notice::BUDGET_EXHAUSTED,
                NoticeSeverity::Warning,
                &copy.budget_exhausted,
            ),
        ];
        let mut notices: Vec<ServerNotice> = Vec::new();
        let raised = own
            .into_iter()
            .filter(|(raised, ..)| *raised)
            .map(|(_, code, severity, text)| notice_of(code, severity, text.clone()));
        for notice in input.notices.iter().cloned().chain(raised) {
            if !notices.iter().any(|existing| existing.code == notice.code) {
                notices.push(notice);
            }
        }
        notices
    }
}

fn notice_of(code: &str, severity: NoticeSeverity, text: LocalizedText) -> ServerNotice {
    ServerNotice {
        block_id: BlockId::from(format!("notice:{code}")),
        code: code.to_owned(),
        severity,
        text,
    }
}

/// `text` without the words its questions were asked in, and the separators they
/// leave; `None` when nothing else is left.
fn without_questions(text: &str, tasks: &[AnswerTask]) -> Option<String> {
    let mut spans: Vec<(usize, usize)> = tasks
        .iter()
        .filter_map(|task| task.asked_at.map(|span| (span.start_byte, span.end_byte)))
        .collect();
    spans.sort_unstable();
    let mut kept = String::new();
    let mut at = 0;
    for (start, end) in spans {
        let Some(before) = text.get(at..start) else {
            continue;
        };
        kept.push_str(before);
        kept.push(' ');
        at = end;
    }
    kept.push_str(text.get(at..).unwrap_or_default());
    let kept = kept.split_whitespace().collect::<Vec<_>>().join(" ");
    let kept = kept.trim_matches(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | ':'));
    (!kept.is_empty()).then(|| kept.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_tokens_are_derived_and_stable() {
        let turn = TurnId::nil();
        assert_eq!(derive_replay_token(&turn), derive_replay_token(&turn));
    }
}

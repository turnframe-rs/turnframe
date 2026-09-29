//! Running a corpus against a real orchestrator (spec §27.6).
//!
//! # What the runner does, and what it refuses to do
//!
//! For every selected item it asks the harness for a fresh world, builds the
//! turn the item describes, runs it through a real [`Orchestrator`], reads back
//! what happened from the stores, and checks the item's deterministic
//! expectations. Then it does it again, `samples_per_item` times.
//!
//! It never turns a failing item into an error. A scenario that passes seven
//! times out of ten is a *measurement* — the most valuable one in the whole
//! harness, because it is the one a single run cannot see. So the sample
//! records its failures, the item records its variance, and the run continues.
//! The only thing that stops a run early is an explicit
//! [`stop_after_failures`](crate::config::ExecutionConfig::stop_after_failures).
//!
//! # The seam
//!
//! [`EvalHarness`] is the one thing an application implements. It owns the
//! domain: it knows how to turn the item's seeded JSON state into a trip or a
//! traveler, which providers to configure, and how much autonomy to grant.
//! The runner owns everything that must not vary between applications — the
//! order, the sampling, the assertions and the report.
//!
//! The `sample` index is handed to [`EvalHarness::prepare`] on purpose: a
//! harness that wants to exercise model variance without a network can script a
//! different answer per sample, and a harness talking to a real endpoint can
//! simply ignore it.

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt as _;
use turnframe_core::case::CaseKey;
use turnframe_core::flow::WorkflowRegistry;
use turnframe_core::ids::{CaseId, ConversationId, OriginToken, TurnId};
use turnframe_core::locale::Locale;
use turnframe_core::turn::{ActorContext, InteractionResponse, OriginRef, TurnInput};
use turnframe_runtime::orchestrator::{Orchestrator, error_code};
use turnframe_store::interaction::InteractionReader;

use crate::assertions::check;
use crate::config::EvalConfig;
use crate::control::ControlRun;
use crate::corpus::{CardReplySpec, EvalItem, ExternalSpec, Suite, TurnSpec};
use crate::judge::{CriterionOutcome, Judge, JudgeInput};
use crate::observation::Observation;
use crate::report::{EvalReport, ItemReport, SampleReport};

/// Which execution of an item this is, zero-based.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SampleIndex(pub u32);

impl SampleIndex {
    /// The zero-based index.
    #[must_use]
    pub const fn index(self) -> u32 {
        self.0
    }

    /// The 1-based number shown in reports.
    #[must_use]
    pub const fn number(self) -> u32 {
        self.0 + 1
    }

    /// Returns `true` for the first sample of an item.
    #[must_use]
    pub const fn is_first(self) -> bool {
        self.0 == 0
    }
}

impl std::fmt::Display for SampleIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.number())
    }
}

/// One item's world, freshly built for one sample.
///
/// Every sample gets its own: two samples that shared a store would be
/// measuring the second one against the first one's effects.
pub struct PreparedRun {
    /// The runtime under test.
    pub orchestrator: Arc<Orchestrator>,
    /// The workflows it was built with, so the runner can read a case's
    /// revision back without knowing the domain types.
    pub workflows: Arc<WorkflowRegistry>,
    /// Who takes the turn.
    pub actor: ActorContext,
    /// The conversation the turn belongs to. The harness must have created it.
    pub conversation_id: ConversationId,
    /// The identifier the turn will carry.
    pub turn_id: TurnId,
}

impl std::fmt::Debug for PreparedRun {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PreparedRun")
            .field("account", &self.actor.account_id)
            .field("turn_id", &self.turn_id)
            .finish_non_exhaustive()
    }
}

/// Builds one world per sample.
#[async_trait]
pub trait EvalHarness: Send + Sync {
    /// Seeds the item's starting state and returns a runtime to run it against.
    ///
    /// # Errors
    ///
    /// [`HarnessError`] when the world could not be built — an unknown
    /// workflow, an unparseable seeded state, a provider that could not be
    /// configured. The sample is recorded as unmeasured rather than as failing.
    async fn prepare(
        &self,
        item: &EvalItem,
        sample: SampleIndex,
    ) -> Result<PreparedRun, HarnessError>;
}

/// Why a sample could not be set up or started.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum HarnessError {
    /// The world could not be built.
    #[error("the harness could not prepare the run: {message}")]
    Setup {
        /// What went wrong.
        message: String,
    },
    /// The item names a workflow the harness did not register.
    #[error("the item names workflow `{workflow}`, which the harness did not register")]
    UnknownWorkflow {
        /// The workflow key.
        workflow: String,
    },
    /// The item answers a card, and the case has none.
    #[error("case {case} has no blocking card for the item's reply")]
    NoBlockingCard {
        /// The case, as `workflow/case_id`.
        case: String,
    },
    /// The card store could not be read.
    #[error("the interaction store could not be read: {message}")]
    Store {
        /// What the store said.
        message: String,
    },
}

impl HarnessError {
    /// A setup failure with a message.
    #[must_use]
    pub fn setup(message: impl Into<String>) -> Self {
        Self::Setup {
            message: message.into(),
        }
    }
}

/// Runs a corpus.
pub struct Runner {
    config: EvalConfig,
    judge: Option<Arc<Judge>>,
}

impl std::fmt::Debug for Runner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runner")
            .field("samples_per_item", &self.config.execution.samples_per_item)
            .field(
                "sample_concurrency",
                &self.config.execution.sample_concurrency,
            )
            .field("votes_per_sample", &self.config.judging.votes_per_sample)
            .field("judge", &self.judge.is_some())
            .finish()
    }
}

impl Runner {
    /// A runner with this configuration and no judge, which is all a corpus of
    /// deterministic assertions needs.
    #[must_use]
    pub fn new(config: EvalConfig) -> Self {
        Self {
            config,
            judge: None,
        }
    }

    /// Attaches a judge, used only for the criteria an item asks for.
    #[must_use]
    pub fn with_judge(mut self, judge: Arc<Judge>) -> Self {
        self.judge = Some(judge);
        self
    }

    /// The configuration in force.
    #[must_use]
    pub const fn config(&self) -> &EvalConfig {
        &self.config
    }

    /// Runs every selected item of `suite`.
    pub async fn run(&self, suite: &Suite, harness: &dyn EvalHarness) -> EvalReport {
        let mut items = Vec::new();
        let mut failed_samples = 0_u32;
        for item in suite.select(&self.config.selection) {
            let report = self.run_item(item, harness).await;
            failed_samples +=
                u32::try_from(report.total_samples() - report.samples_passed()).unwrap_or(u32::MAX);
            items.push(report);
            if self
                .config
                .execution
                .stop_after_failures
                .is_some_and(|budget| failed_samples >= budget)
            {
                break;
            }
        }
        EvalReport::new(
            suite.name.clone(),
            chrono::Utc::now(),
            self.config.clone(),
            items,
        )
    }

    /// Runs the same suite twice, against the same harness, and hands back both
    /// reports as a [`ControlRun`].
    ///
    /// This is how the noise floor stops being an assumption. Nothing changes
    /// between the two passes — same corpus, same harness, same code — so
    /// whatever difference [`ControlRun::noise_floor`] finds is the harness's
    /// own variation, and a later comparison can say whether a difference
    /// exceeds it instead of leaving a reader to guess.
    ///
    /// It costs exactly twice a run, which is the honest price of knowing
    /// whether the first one meant anything.
    ///
    /// ```no_run
    /// # use std::sync::Arc;
    /// # use turnframe_eval::config::EvalConfig;
    /// # use turnframe_eval::corpus::Suite;
    /// # use turnframe_eval::runner::{EvalHarness, Runner};
    /// # async fn run(suite: &Suite, harness: &dyn EvalHarness) {
    /// let control = Runner::new(EvalConfig::default().with_samples_per_item(10))
    ///     .run_control(suite, harness)
    ///     .await;
    /// let floor = control.noise_floor();
    /// println!("{}", floor.summary());
    /// # }
    /// ```
    pub async fn run_control(&self, suite: &Suite, harness: &dyn EvalHarness) -> ControlRun {
        let first = self.run(suite, harness).await;
        let second = self.run(suite, harness).await;
        ControlRun::new(first, second)
    }

    /// Runs one item, `samples_per_item` times, at most
    /// [`sample_concurrency`](crate::config::ExecutionConfig::sample_concurrency)
    /// of them at once.
    ///
    /// The default concurrency of one reproduces the strictly serial behaviour
    /// exactly. Above one the samples *execute* interleaved — which is the
    /// point, when each of them is a call to a real endpoint — but the report
    /// does not move: the samples come back in index order either way, and the
    /// same set of results comes back whatever the setting is.
    pub async fn run_item(&self, item: &EvalItem, harness: &dyn EvalHarness) -> ItemReport {
        let count = self.config.execution.samples_per_item.max(1);
        let in_flight =
            usize::try_from(self.config.execution.sample_concurrency.max(1)).unwrap_or(usize::MAX);
        let samples = futures::stream::iter(
            (0..count).map(|index| self.run_sample(item, harness, SampleIndex(index))),
        )
        // `buffered`, not `buffer_unordered`: the samples run together and are
        // still handed back in index order, so a report never depends on which
        // endpoint answered first.
        .buffered(in_flight)
        .collect::<Vec<_>>()
        .await;
        ItemReport {
            id: item.id.clone(),
            name: item.name.clone(),
            tags: item.tags.clone(),
            // Recorded on every report, always: a comparison that cannot see
            // what the item contained cannot tell an intended projection change
            // from an unrelated edit, and would report an unpaired difference
            // as a regression.
            fingerprint: item.fingerprint(),
            samples,
        }
    }

    /// Runs one sample of one item.
    pub async fn run_sample(
        &self,
        item: &EvalItem,
        harness: &dyn EvalHarness,
        sample: SampleIndex,
    ) -> SampleReport {
        let prepared = match harness.prepare(item, sample).await {
            Ok(prepared) => prepared,
            Err(error) => return unmeasured(sample, &error.to_string()),
        };
        // The turns before the observed one play out as a person would take them: one
        // that fails leaves the conversation where it stands, and the next is taken.
        let mut earlier = Vec::new();
        for spec in &item.before {
            if let Some(external) = &spec.external {
                if let Err(error) = outside(&prepared, external).await {
                    return unmeasured(sample, &error);
                }
                continue;
            }
            let turn_id = TurnId::new();
            let input = match build_input(&prepared, spec, turn_id).await {
                Ok(input) => input,
                Err(error) => return unmeasured(sample, &error.to_string()),
            };
            let _ = prepared.orchestrator.handle_turn(input).await;
            earlier.push(turn_id);
        }
        let input = match build_input(&prepared, &item.turn, prepared.turn_id).await {
            Ok(input) => input,
            Err(error) => return unmeasured(sample, &error.to_string()),
        };

        let outcome = prepared.orchestrator.handle_turn(input).await;
        // "Abandoned" is a turn that produced nothing a person could act on:
        // it failed, or it came back with no blocks at all (spec §26.3).
        let abandoned = outcome.as_ref().map_or(true, |turn| turn.blocks.is_empty());
        let observed = Observation::collect_bounded(
            prepared.orchestrator.stores(),
            prepared.workflows.as_ref(),
            &prepared.actor.account_id,
            prepared.turn_id,
            &item.setup.cases,
            outcome.as_ref().map_err(error_code),
            self.config.execution.max_observed_events,
        )
        .await
        .with_conversation_cases(
            prepared.orchestrator.stores(),
            prepared.workflows.as_ref(),
            &prepared.actor.account_id,
            &earlier,
        )
        .await;

        let failures = check(&item.expect, &observed);
        let judge = self.judge_sample(item, sample, &observed).await;

        SampleReport {
            sample: sample.number(),
            failures,
            harness_error: None,
            signature: observed.signature(),
            judge,
            acts_proposed: observed.acts.len(),
            acts_refused: observed
                .acts
                .iter()
                .filter(|act| act.outcome.as_deref() == Some("rejected"))
                .count(),
            commands_journaled: observed.commands.len(),
            provider_failures: observed.provider_failures,
            cards_created: observed.cards_created,
            abandoned,
            discarded_answers: observed
                .discard_codes()
                .into_iter()
                .map(str::to_owned)
                .collect(),
            answer: if self.config.execution.record_answers {
                observed.answer.clone()
            } else {
                String::new()
            },
            tasks: item
                .expect
                .understanding
                .as_ref()
                .zip(item.turn.text.as_deref())
                .map(|(expected, text)| expected.score(text, &observed))
                .unwrap_or_default(),
        }
    }

    /// Polls the judge for the criteria this item asks for.
    ///
    /// Nothing from [`Observation`] reaches the judge except the model-authored
    /// text and — for
    /// [`OperationalClaimIntegrity`](crate::judge::JudgeCriterion::OperationalClaimIntegrity)
    /// — the event types the turn committed, which are the ledger's answer and
    /// not something the judge is asked to establish.
    async fn judge_sample(
        &self,
        item: &EvalItem,
        sample: SampleIndex,
        observed: &Observation,
    ) -> Vec<CriterionOutcome> {
        let Some(judge) = self.judge.as_ref() else {
            return Vec::new();
        };
        if item.judge.is_empty() {
            return Vec::new();
        }
        if !self.config.judging.judge_every_sample && !sample.is_first() {
            return Vec::new();
        }
        let input = JudgeInput::new(question_of(&item.turn), observed.answer.clone())
            .with_committed(observed.events.clone());
        if input.is_empty() {
            return item
                .judge
                .iter()
                .map(|criterion| CriterionOutcome::empty(*criterion))
                .collect();
        }
        let mut outcomes = Vec::new();
        for criterion in &item.judge {
            outcomes.push(
                judge
                    .poll(*criterion, &input, self.config.judging.votes_per_sample)
                    .await,
            );
        }
        outcomes
    }
}

/// The sample record of a run that never happened.
/// Applies a change from outside the conversation as the record's own system would: the
/// domain's command, at the revision the record is at, from an external origin.
async fn outside(prepared: &PreparedRun, external: &ExternalSpec) -> Result<(), String> {
    use turnframe_core::case::CaseRef;
    use turnframe_core::command::{
        AtomicityScope, CommandBatch, CommandEnvelope, CommandOrigin, IdempotencyKey,
    };
    use turnframe_core::ids::{BatchId, CommandId};
    use turnframe_core::understanding::{ActId, UnitId};

    let registered = prepared
        .workflows
        .require(&external.workflow)
        .map_err(|error| error.to_string())?;
    let account = &prepared.actor.account_id;
    let loaded = registered
        .executor
        .load(account, &external.case_id)
        .await
        .map_err(|error| error.to_string())?;
    let case_ref = CaseRef::new(
        external.workflow.clone(),
        external.case_id.clone(),
        loaded.revision,
    );
    let turn_id = TurnId::new();
    let origin = CommandOrigin::ExternalCallback {
        callback_id: "eval.external".to_owned(),
        signature_verified: true,
    };
    let idempotency_key =
        IdempotencyKey::derive(account, &turn_id, &case_ref, &origin, &external.command)
            .map_err(|error| error.to_string())?;
    let batch = CommandBatch {
        batch_id: BatchId::derive(&turn_id, &case_ref.key(), &AtomicityScope::PerCase),
        scope: AtomicityScope::PerCase,
        envelopes: vec![CommandEnvelope {
            command_id: CommandId::derive(&turn_id, ActId::new(UnitId(1), 1), 0),
            turn_id,
            actor: ActorContext::new(account.clone(), "external"),
            case_ref,
            idempotency_key,
            origin,
            command: external.command.clone(),
        }],
    };
    registered
        .executor
        .execute(batch)
        .await
        .map(|_| ())
        .map_err(|error| error.to_string())
}

fn unmeasured(sample: SampleIndex, message: &str) -> SampleReport {
    SampleReport {
        sample: sample.number(),
        failures: Vec::new(),
        harness_error: Some(message.to_owned()),
        signature: format!("harness_error={message}"),
        judge: Vec::new(),
        acts_proposed: 0,
        acts_refused: 0,
        commands_journaled: 0,
        provider_failures: 0,
        cards_created: 0,
        abandoned: true,
        discarded_answers: Vec::new(),
        answer: String::new(),
        tasks: Default::default(),
    }
}

/// What the person asked, for the judge's benefit.
fn question_of(spec: &TurnSpec) -> String {
    match (&spec.text, &spec.reply) {
        (Some(text), _) => text.clone(),
        (None, Some(reply)) => format!(
            "(the user chose `{}` on {}/{})",
            reply.option,
            reply.workflow,
            reply
                .case_id
                .as_ref()
                .map_or("the open card", CaseId::as_str)
        ),
        (None, None) => String::new(),
    }
}

/// Turns the item's description of a turn into a real [`TurnInput`].
async fn build_input(
    prepared: &PreparedRun,
    spec: &TurnSpec,
    turn_id: TurnId,
) -> Result<TurnInput, HarnessError> {
    let mut actor = prepared.actor.clone();
    if let Some(user_id) = &spec.user_id {
        actor.user_id = turnframe_core::ids::UserId::new(user_id.clone());
    }
    let interaction_response = match &spec.reply {
        Some(reply) => Some(resolve_card(prepared, reply).await?),
        None => None,
    };
    Ok(TurnInput {
        turn_id,
        conversation_id: prepared.conversation_id,
        actor,
        text: spec.text.clone(),
        interaction_response,
        attachments: Vec::new(),
        origin: spec.origin.as_ref().map(|origin| OriginRef {
            origin_token: OriginToken::from(origin.token.as_str()),
            signature: None,
            surface: origin.surface.clone(),
        }),
        locale: spec.locale.clone().unwrap_or_else(|| Locale::from("en")),
        effort: None,
    })
}

/// Finds the blocking card an item's reply answers.
///
/// An item file cannot name an [`InteractionId`](turnframe_core::ids::InteractionId):
/// the identifier is minted while the corpus is running. What it names instead
/// is the case, which is how a person would describe the click anyway — and the
/// revision the reply carries is read from the card itself, so the corpus never
/// has to know which revision the previous turn left behind.
async fn resolve_card(
    prepared: &PreparedRun,
    reply: &CardReplySpec,
) -> Result<InteractionResponse, HarnessError> {
    let store = prepared.orchestrator.stores().interactions();
    let open = match &reply.case_id {
        Some(case_id) => {
            let case = CaseKey::new(reply.workflow.clone(), case_id.clone());
            InteractionReader::list_open_for_case(store.as_ref(), &prepared.actor.account_id, &case)
                .await
        }
        None => {
            InteractionReader::list_open_for_conversation(
                store.as_ref(),
                &prepared.actor.account_id,
                &prepared.conversation_id,
            )
            .await
        }
    }
    .map_err(|error| HarnessError::Store {
        message: error.to_string(),
    })?;
    let card = open
        .into_iter()
        .find(|card| card.blocking && card.case_ref.workflow == reply.workflow)
        .ok_or_else(|| HarnessError::NoBlockingCard {
            case: format!(
                "{}/{}",
                reply.workflow,
                reply.case_id.as_ref().map_or("*", CaseId::as_str)
            ),
        })?;
    Ok(InteractionResponse {
        interaction_id: card.id,
        option_id: reply.option.clone(),
        expected_case_revision: card.case_ref.expected_revision,
        freeform_input: reply.freeform.clone(),
    })
}

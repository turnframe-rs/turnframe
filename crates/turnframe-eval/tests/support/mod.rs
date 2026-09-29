//! An [`EvalHarness`] over the sample domains of `turnframe-test`: trip, traveler, claim.
//!
//! It is the runnable answer to "what does an application have to write?": seed
//! the item's JSON state into the domain's own type, register the workflow,
//! configure a provider, hand back an orchestrator. Everything else — sampling,
//! assertions, judging, reporting — belongs to the crate under test.
//!
//! Offline, a closure of `(item, sample, turn_id)` decides what the turn is understood
//! to say and which `ScriptedProvider` narrates it, so samples can differ — the thing
//! `samples_per_item` measures. The provider fails loudly on a call nobody scripted, so
//! "the corpus ran without a network" is a property of the test. Live, only a provider
//! is given, and the runtime's own understanding tasks run on it.
#![allow(dead_code)]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use turnframe_core::case::{CaseKey, CaseRef, Versioned};
use turnframe_core::command::CommandBatch;
use turnframe_core::error::{ExecutionError, StoreError};
use turnframe_core::event::{Commit, CommittedEvent};
use turnframe_core::flow::{WorkflowExecutor, WorkflowRegistry};
use turnframe_core::ids::{
    AccountId, CaseId, CaseRevision, CommandId, ConversationId, EventId, InteractionId,
    TargetToken, TurnId,
};
use turnframe_core::interaction::Interaction;
use turnframe_core::policy::PolicySnapshot;
use turnframe_core::turn::ActorContext;
use turnframe_core::understanding::Understanding;
use turnframe_eval::corpus::EvalItem;
use turnframe_eval::runner::{EvalHarness, HarnessError, PreparedRun, SampleIndex};
use turnframe_provider::provider::ModelProvider;
use turnframe_provider::router::ProviderPool;
use turnframe_runtime::config::OrchestratorConfig;
use turnframe_runtime::orchestrator::{CaseCandidate, FixedTurnClock, Orchestrator};
use turnframe_runtime::resolve::{AuthorizedCase, TargetResolver};
use turnframe_store::events::{EventBatch, EventJournalWriter};
use turnframe_test::providers::{ScriptedProvider, ScriptedProviderBuilder, ScriptedUnderstanding};
use turnframe_test::stores::FakeStores;
use turnframe_test::workflows::InMemoryExecutor;
use turnframe_test::workflows::claim::{ClaimState, ClaimWorkflow};
use turnframe_test::workflows::traveler::{TravelerState, TravelerWorkflow};
use turnframe_test::workflows::trip::{TripState, TripWorkflow};

/// The tenant every item runs in.
pub const ACCOUNT: &str = "aurora";

/// The only workflow this harness registers.
pub const TRIP: &str = "trip";

pub const TRAVELER: &str = "traveler";

pub const CLAIM: &str = "claim";

/// The tenant.
#[must_use]
pub fn account() -> AccountId {
    AccountId::from(ACCOUNT)
}

/// A fixed instant, so nothing in an item depends on the wall clock.
#[must_use]
pub fn now() -> DateTime<Utc> {
    DateTime::from_timestamp(1_700_000_000, 0).expect("a valid fixed instant")
}

/// The opaque token a turn issues for a case, derived exactly as the runtime
/// derives it, so a scripted understanding can name a record without the fixture
/// ever seeing its identifier.
#[must_use]
pub fn token_for(turn_id: TurnId, workflow: &str, case_id: &str) -> TargetToken {
    TargetResolver::builder(account(), turn_id)
        .candidate(AuthorizedCase::new(
            CaseRef::new(workflow, case_id, CaseRevision::ZERO),
            "label",
        ))
        .build()
        .token_map()
        .token_for(&CaseKey::new(workflow, case_id))
        .cloned()
        .expect("a token was issued for the case")
}

/// The turn identifier one sample uses. Each sample gets its own world, so the
/// identifier only has to be stable within a sample.
#[must_use]
pub fn turn_id_for(sample: SampleIndex) -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(u128::from(sample.index()) + 1))
}

/// A provider that writes one narration, which is what a plain turn needs.
#[must_use]
pub fn narrating() -> ScriptedProviderBuilder {
    ScriptedProvider::builder("scripted", "model-1")
        .acknowledging("Right, here is where that leaves things.")
}

/// A turn understood as `understanding` and narrated once.
#[must_use]
pub fn scripted(understanding: Understanding) -> Scripted {
    Scripted::narrated_by(narrating().build_shared()).understood_as(understanding)
}

/// What one sample runs on: what its turn is understood to say, and who narrates it.
#[derive(Debug, Clone)]
pub struct Scripted {
    /// Hands the turn its understanding; a text turn with none queued is unreadable.
    pub understander: Arc<ScriptedUnderstanding>,
    /// Answers every model call the turn makes, which is narration only.
    pub provider: Arc<ScriptedProvider>,
}

impl Scripted {
    /// Nothing understood yet, narrated by `provider`: all a click-only turn needs.
    #[must_use]
    pub fn narrated_by(provider: Arc<ScriptedProvider>) -> Self {
        Self {
            understander: Arc::new(ScriptedUnderstanding::new()),
            provider,
        }
    }

    /// Queues `understanding` for the sample's turn.
    #[must_use]
    pub fn understood_as(self, understanding: Understanding) -> Self {
        self.understander.push(understanding);
        self
    }
}

/// An executor shared between the registry and the harness that seeds it.
pub struct SharedExecutor<W: turnframe_test::workflows::PureWorkflow>(pub Arc<InMemoryExecutor<W>>);

impl<W: turnframe_test::workflows::PureWorkflow> Clone for SharedExecutor<W> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

#[async_trait]
impl<W: turnframe_test::workflows::PureWorkflow> WorkflowExecutor<W> for SharedExecutor<W> {
    async fn load(
        &self,
        account: &AccountId,
        case_id: &CaseId,
    ) -> Result<Versioned<Option<W::State>>, StoreError> {
        self.0.load(account, case_id).await
    }

    async fn execute(
        &self,
        batch: CommandBatch<W::Command>,
    ) -> Result<Commit<W::State, W::Event>, ExecutionError> {
        self.0.execute(batch).await
    }
}

/// What a scripted harness gives one sample of one item.
type ScriptFn = dyn Fn(&EvalItem, SampleIndex, TurnId) -> Scripted + Send + Sync;

/// The provider a live harness gives one sample of one item.
type ProviderFn = dyn Fn(&EvalItem, SampleIndex, TurnId) -> Arc<dyn ModelProvider> + Send + Sync;

/// How a harness decides what the model does for one sample of one item.
enum Script {
    /// Understanding and narration are both scripted.
    Scripted(Arc<ScriptFn>),
    /// A provider, real or not, runs the runtime's own understanding and narration.
    Live(Arc<ProviderFn>),
}

/// How many samples were in flight at once, and the most there ever were.
///
/// A harness holding one of these reports what the runner actually did, which
/// is the only way to tell a concurrency setting that is respected from one
/// that is merely stored.
#[derive(Debug, Default)]
pub struct ConcurrencyGauge {
    inner: Mutex<(usize, usize)>,
}

impl ConcurrencyGauge {
    /// A gauge that has seen nothing yet.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The most samples that were ever in flight at the same time.
    #[must_use]
    pub fn peak(&self) -> usize {
        self.inner.lock().unwrap().1
    }

    fn enter(&self) {
        let mut guard = self.inner.lock().unwrap();
        guard.0 += 1;
        guard.1 = guard.1.max(guard.0);
    }

    fn exit(&self) {
        self.inner.lock().unwrap().0 -= 1;
    }
}

/// An [`EvalHarness`] over the sample trip domain.
pub struct SampleHarness {
    script: Script,
    config: OrchestratorConfig,
    prior_events: usize,
    gauge: Option<Arc<ConcurrencyGauge>>,
}

impl SampleHarness {
    /// A harness whose turns are understood and narrated as `script` decides.
    #[must_use]
    pub fn new(
        script: impl Fn(&EvalItem, SampleIndex, TurnId) -> Scripted + Send + Sync + 'static,
    ) -> Self {
        Self::with(Script::Scripted(Arc::new(script)))
    }

    /// A harness whose model is whatever `provider` decides, with nothing scripted.
    ///
    /// The runtime builds its own understander over the provider, so a run against a
    /// real endpoint measures the understanding tasks and the narration a deployment
    /// would run, and not a fixture.
    #[must_use]
    pub fn with_provider(
        provider: impl Fn(&EvalItem, SampleIndex, TurnId) -> Arc<dyn ModelProvider>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self::with(Script::Live(Arc::new(provider)))
    }

    fn with(script: Script) -> Self {
        Self {
            script,
            config: OrchestratorConfig::conservative(),
            prior_events: 0,
            gauge: None,
        }
    }

    /// Gives every seeded case `count` events of history before the turn runs.
    ///
    /// This is the world an item over a long-lived case really starts from: the
    /// ledger already holds thousands of rows committed by turns nobody is
    /// measuring, and the turn under test appends its own at the end of them.
    #[must_use]
    pub const fn with_prior_events(mut self, count: usize) -> Self {
        self.prior_events = count;
        self
    }

    /// Runs every turn at `effort`, the configured default no item overrides.
    #[must_use]
    pub fn with_effort(mut self, effort: turnframe_core::effort::Effort) -> Self {
        let mut levels = turnframe_runtime::effort::EffortConfig::default();
        levels.default = effort;
        self.config = self.config.with_effort(levels);
        self
    }

    /// Reports how many samples the runner had in flight at once.
    ///
    /// The gauge is held across a short sleep inside `prepare`, so two samples
    /// that overlap are visible as an overlap rather than inferred from a
    /// wall-clock reading.
    #[must_use]
    pub fn watched_by(mut self, gauge: Arc<ConcurrencyGauge>) -> Self {
        self.gauge = Some(gauge);
        self
    }
}

impl std::fmt::Debug for SampleHarness {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SampleHarness").finish_non_exhaustive()
    }
}

#[async_trait]
impl EvalHarness for SampleHarness {
    async fn prepare(
        &self,
        item: &EvalItem,
        sample: SampleIndex,
    ) -> Result<PreparedRun, HarnessError> {
        let stores = FakeStores::at(now());
        let trip = Arc::new(InMemoryExecutor::new(TripWorkflow::default()));
        let traveler = Arc::new(InMemoryExecutor::new(TravelerWorkflow::default()));
        let claim = Arc::new(InMemoryExecutor::new(ClaimWorkflow::default()));
        let account = account();

        let mut seeded = Vec::new();
        for seed in &item.setup.cases {
            let revision = CaseRevision(seed.revision);
            let state = seed.state.clone();
            let parsed = |error: serde_json::Error| HarnessError::setup(error.to_string());
            match seed.workflow.as_str() {
                TRIP => trip.seed(
                    &account,
                    &seed.case_id,
                    serde_json::from_value::<TripState>(state).map_err(parsed)?,
                    revision,
                ),
                TRAVELER => traveler.seed(
                    &account,
                    &seed.case_id,
                    serde_json::from_value::<TravelerState>(state).map_err(parsed)?,
                    revision,
                ),
                CLAIM => claim.seed(
                    &account,
                    &seed.case_id,
                    serde_json::from_value::<ClaimState>(state).map_err(parsed)?,
                    revision,
                ),
                other => {
                    return Err(HarnessError::UnknownWorkflow {
                        workflow: other.to_owned(),
                    });
                }
            }
            seeded.push(CaseCandidate::new(
                CaseKey::new(seed.workflow.clone(), seed.case_id.clone()),
                seed.label.clone(),
            ));
        }

        let registry = WorkflowRegistry::builder()
            .register(TripWorkflow::default(), SharedExecutor(Arc::clone(&trip)))
            .register(
                TravelerWorkflow::default(),
                SharedExecutor(Arc::clone(&traveler)),
            )
            .register(ClaimWorkflow::default(), SharedExecutor(Arc::clone(&claim)))
            .build()
            .map_err(|error| HarnessError::setup(error.to_string()))?;

        let turn_id = turn_id_for(sample);
        let (provider, understander): (Arc<dyn ModelProvider>, _) = match &self.script {
            Script::Scripted(script) => {
                let scripted = script(item, sample, turn_id);
                (scripted.provider, Some(scripted.understander))
            }
            Script::Live(provider) => (provider(item, sample, turn_id), None),
        };
        // A live run traces like the examples when `TURNFRAME_TRACE` asks for it, every
        // sample of the run into one file.
        let trace = match &self.script {
            Script::Live(_) => run_trace().map_err(HarnessError::setup)?,
            Script::Scripted(_) => None,
        };
        let provider: Arc<dyn ModelProvider> = match &trace {
            Some(trace) => Arc::new(turnframe_provider::trace::TracedProvider::new(
                provider,
                Arc::clone(trace) as _,
            )),
            None => provider,
        };
        let pool = Arc::new(
            ProviderPool::builder()
                .provider(provider)
                .build()
                .map_err(|error| HarnessError::setup(error.to_string()))?,
        );

        let workflows = Arc::new(registry);
        let mut builder = Orchestrator::builder()
            .workflows(Arc::clone(&workflows))
            .providers(pool)
            .stores(stores.stores().clone())
            .case_directory(Arc::new(Records {
                seeded,
                trips: Arc::clone(&trip),
                travelers: Arc::clone(&traveler),
            }))
            .policy(PolicySnapshot::conservative())
            .clock(Arc::new(FixedTurnClock(now())))
            .config(self.config.clone());
        if let Some(understander) = understander {
            builder = builder.understander(understander);
        }
        if let Some(trace) = trace {
            builder = builder.trace(trace);
        }
        let orchestrator = builder
            .build()
            .map_err(|error| HarnessError::setup(error.to_string()))?;

        let conversation_id = ConversationId::nil();
        stores
            .stores()
            .conversations()
            .create_conversation(turnframe_store::conversation::ConversationRecord::new(
                conversation_id,
                account.clone(),
                now(),
            ))
            .await
            .map_err(|error| HarnessError::setup(error.to_string()))?;

        seed_history(&stores, &account, conversation_id, item).await?;
        for seed in &item.setup.cases {
            seed_blocking_card(&stores, &workflows, &account, conversation_id, seed).await?;
            seed_prior_events(&stores, &account, seed, self.prior_events).await?;
        }

        if let Some(gauge) = &self.gauge {
            // Held across an await point on purpose: two samples that the
            // runner started together are both inside this window, so the peak
            // the gauge reports is the concurrency the runner really used.
            gauge.enter();
            tokio::time::sleep(GAUGE_WINDOW).await;
            gauge.exit();
        }

        // The registry comes back OUT of the orchestrator rather than from the
        // local variable, which is how an application that assembles its
        // runtime in a composition root has to do it — and it means every
        // revision this suite reads back travels through the accessor.
        let orchestrator = Arc::new(orchestrator);
        Ok(PreparedRun {
            workflows: Arc::clone(orchestrator.workflows()),
            orchestrator,
            actor: ActorContext::new(account, "u1"),
            conversation_id,
            turn_id,
        })
    }
}

/// How long `prepare` lingers when a gauge is watching.
///
/// Long enough that samples the runner started together are visibly together,
/// short enough that a serial run of a dozen samples costs a fraction of a
/// second.
const GAUGE_WINDOW: Duration = Duration::from_millis(25);

/// Appends `count` events to a case's ledger, as earlier turns would have.
///
/// They carry command identifiers of their own, so none of them belongs to the
/// turn under test — which is exactly the point: an observation must page past
/// all of them and still report only what its own turn committed.
async fn seed_prior_events(
    stores: &FakeStores,
    account: &AccountId,
    seed: &turnframe_eval::corpus::CaseSeed,
    count: usize,
) -> Result<(), HarnessError> {
    if count == 0 {
        return Ok(());
    }
    let case = CaseKey::new(seed.workflow.clone(), seed.case_id.clone());
    // Batched rather than one append per event: the journal appends a batch
    // atomically, and six hundred round trips would make the test slow for no
    // extra coverage.
    for chunk in 0..count.div_ceil(BATCH) {
        let events: Vec<CommittedEvent<serde_json::Value>> = (0..BATCH)
            .map(|offset| chunk * BATCH + offset)
            .take_while(|index| *index < count)
            .map(|index| CommittedEvent {
                event_id: EventId::new(),
                event_type: "trip.note_added".to_owned(),
                occurred_at: now(),
                payload: serde_json::json!({"index": index}),
            })
            .collect();
        if events.is_empty() {
            break;
        }
        EventJournalWriter::append(
            stores.stores().events().as_ref(),
            EventBatch::new(
                account.clone(),
                case.clone(),
                CommandId::new(),
                CaseRevision(seed.revision),
                events,
            ),
        )
        .await
        .map_err(|error| HarnessError::setup(error.to_string()))?;
    }
    Ok(())
}

/// Events per append while a history is being seeded.
const BATCH: usize = 64;

/// Gives a seeded case the card the real world would already have given it.
///
/// A state whose projected phase is user-owned is, by construction, a state
/// some earlier turn left waiting on a card (spec I6). An item that seeds such
/// a state and then answers the card would otherwise be describing a world that
/// cannot exist, so the harness materializes the requirement exactly as the
/// runtime would have.
async fn seed_blocking_card(
    stores: &FakeStores,
    workflows: &WorkflowRegistry,
    account: &AccountId,
    conversation_id: ConversationId,
    seed: &turnframe_eval::corpus::CaseSeed,
) -> Result<(), HarnessError> {
    let registered =
        workflows
            .get(&seed.workflow)
            .ok_or_else(|| HarnessError::UnknownWorkflow {
                workflow: seed.workflow.as_str().to_owned(),
            })?;
    let case_ref = CaseRef::new(
        seed.workflow.clone(),
        seed.case_id.clone(),
        CaseRevision(seed.revision),
    );
    let view = registered
        .definition
        .project(case_ref.clone(), Some(&seed.state))
        .map_err(|error| HarnessError::setup(error.to_string()))?;
    let Some(requirement) = view.blocking_interaction.as_ref() else {
        return Ok(());
    };
    let spec = registered
        .definition
        .build_interaction(case_ref, Some(&seed.state), requirement)
        .map_err(|error| HarnessError::setup(error.to_string()))?;
    let card = Interaction::from_spec(
        spec,
        InteractionId::new(),
        account.clone(),
        conversation_id,
        TurnId::nil(),
        now(),
    )
    .map_err(|error| HarnessError::setup(error.to_string()))?;
    stores
        .stores()
        .interactions()
        .insert(card)
        .await
        .map_err(|error| HarnessError::setup(error.to_string()))
}

/// Writes the item's earlier exchanges into the conversation, oldest first, so the turn
/// under test reads them as its transcript.
async fn seed_history(
    stores: &FakeStores,
    account: &AccountId,
    conversation_id: ConversationId,
    item: &EvalItem,
) -> Result<(), HarnessError> {
    let conversations = stores.stores().conversations();
    let count = item.setup.history.len();
    for (index, exchange) in item.setup.history.iter().enumerate() {
        let turn_id = TurnId::from(uuid::Uuid::from_u128(0xFFFF_0000 + index as u128));
        let minutes_ago = i64::try_from(count - index).unwrap_or(i64::MAX);
        let received_at = now() - chrono::TimeDelta::minutes(minutes_ago);
        let input = turnframe_core::turn::TurnInput {
            turn_id,
            conversation_id,
            actor: ActorContext::new(account.clone(), "u1"),
            text: Some(exchange.user.clone()),
            interaction_response: None,
            attachments: Vec::new(),
            origin: None,
            locale: item
                .turn
                .locale
                .clone()
                .unwrap_or_else(|| turnframe_core::locale::Locale::from("en-GB")),
            effort: None,
        };
        conversations
            .append_user_turn(turnframe_store::conversation::StoredUserTurn::new(
                input,
                received_at,
            ))
            .await
            .map_err(|error| HarnessError::setup(error.to_string()))?;
        let blocks = exchange
            .assistant
            .iter()
            .map(|text| {
                turnframe_core::response::ResponseBlock::Transition(
                    turnframe_core::response::GeneratedTransition {
                        block_id: turnframe_core::ids::BlockId::from(format!("history:{index}")),
                        text: text.clone(),
                        facts_used: Vec::new(),
                    },
                )
            })
            .collect();
        conversations
            .append_assistant_turn(
                account,
                turnframe_core::response::AssistantTurn {
                    turn_id,
                    conversation_id,
                    blocks,
                    replay_token: turnframe_core::response::ReplayToken::from("history"),
                    subjects: Vec::new(),
                    expectations: Vec::new(),
                    done: Vec::new(),
                },
            )
            .await
            .map_err(|error| HarnessError::setup(error.to_string()))?;
        conversations
            .set_turn_phase(
                account,
                &turn_id,
                turnframe_core::replay::TurnPhase::Delivered,
            )
            .await
            .map_err(|error| HarnessError::setup(error.to_string()))?;
    }
    Ok(())
}

/// The one trace file of this run, opened by the first sample that asks for it.
pub fn run_trace() -> Result<Option<Arc<turnframe_runtime::trace::JsonlTrace>>, String> {
    static TRACE: OnceLock<Result<Option<Arc<turnframe_runtime::trace::JsonlTrace>>, String>> =
        OnceLock::new();
    TRACE
        .get_or_init(|| {
            turnframe_runtime::trace::JsonlTrace::from_environment(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../traces"
            ))
            .map(|trace| trace.map(Arc::new))
            .map_err(|error| error.to_string())
        })
        .clone()
}

/// The records a conversation may address: those the item seeded, under their labels, and
/// those its turns created, named as the console names them.
struct Records {
    seeded: Vec<CaseCandidate>,
    trips: Arc<InMemoryExecutor<TripWorkflow>>,
    travelers: Arc<InMemoryExecutor<TravelerWorkflow>>,
}

impl Records {
    fn labelled(&self, account: &AccountId) -> Vec<CaseCandidate> {
        let mut found = self.seeded.clone();
        let known =
            |found: &[CaseCandidate], key: &CaseKey| found.iter().any(|seed| seed.key == *key);
        for (index, case_id) in self.trips.case_ids(account).into_iter().enumerate() {
            let key = CaseKey::new(TRIP, case_id);
            if !known(&found, &key) {
                found.push(CaseCandidate::new(key, format!("Trip {}", index + 1)));
            }
        }
        for (index, case_id) in self.travelers.case_ids(account).into_iter().enumerate() {
            let name = self
                .travelers
                .state_of(account, &case_id)
                .and_then(|state| state.full_name);
            let key = CaseKey::new(TRAVELER, case_id);
            if !known(&found, &key) {
                let label = name.unwrap_or_else(|| format!("New traveler {}", index + 1));
                found.push(CaseCandidate::new(key, label));
            }
        }
        found
    }
}

#[async_trait]
impl turnframe_runtime::orchestrator::CaseDirectory for Records {
    async fn candidates(
        &self,
        actor: &ActorContext,
        _conversation: &ConversationId,
    ) -> Result<Vec<CaseCandidate>, StoreError> {
        Ok(self.labelled(&actor.account_id))
    }

    async fn find(
        &self,
        actor: &ActorContext,
        _conversation: &ConversationId,
        workflow: &turnframe_core::ids::WorkflowKey,
        named: Option<&str>,
    ) -> Result<Vec<CaseCandidate>, StoreError> {
        let Some(named) = named.map(str::to_lowercase) else {
            return Ok(Vec::new());
        };
        Ok(self
            .labelled(&actor.account_id)
            .into_iter()
            .filter(|candidate| {
                candidate.key.workflow == *workflow
                    && candidate.label.to_lowercase().contains(&named)
            })
            .collect())
    }
}

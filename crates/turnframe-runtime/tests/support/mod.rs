//! The scaffolding the twenty runtime scenarios of spec §27.4 share.
//!
//! It wires the real thing: the sample trip and traveler domains from
//! `turnframe-test`, its `FakeStores` with failure injection at the crash
//! boundaries of §27.7, a `ScriptedUnderstanding` that returns the understandings a
//! test queued, a `ScriptedProvider` for narration (which fails loudly on a call nobody
//! scripted), and a real [`Orchestrator`] over all of it.
#![allow(dead_code)]
// The harness hands `OrchestratorError` straight back to the test, for the same
// reason the crate does: an assertion about *which* failure happened needs the
// whole value, not a summary of it.
#![allow(clippy::result_large_err)]

use std::sync::Arc;

use chrono::{DateTime, Utc};
use turnframe_core::case::{CaseKey, CaseRef, Versioned};
use turnframe_core::command::{CommandBatch, IdempotencyKey};
use turnframe_core::error::{ExecutionError, StoreError};
use turnframe_core::event::Commit;
use turnframe_core::flow::{WorkflowExecutor, WorkflowRegistry};
use turnframe_core::ids::{
    AccountId, CaseId, CaseRevision, ConversationId, InteractionId, OptionId, TargetToken, TurnId,
};
use turnframe_core::interaction::Interaction;
use turnframe_core::knowledge::{
    KnowledgeChunk, KnowledgeError, KnowledgeProvider, KnowledgeRequest,
};
use turnframe_core::locale::Locale;
use turnframe_core::observe::{Observer, Signal, SignalLabels};
use turnframe_core::policy::PolicySnapshot;
use turnframe_core::replay::TurnPhase;
use turnframe_core::response::{AssistantTurn, ResponseBlock};
use turnframe_core::turn::{ActorContext, InteractionResponse, TurnInput};
use turnframe_core::understanding::Understanding;
use turnframe_provider::error::ProviderError;
use turnframe_provider::router::ProviderPool;
use turnframe_runtime::config::{NarrationConfig, OrchestratorConfig};
use turnframe_runtime::orchestrator::{
    CaseCandidate, CaseDirectory, FixedTurnClock, Orchestrator, StaticCaseDirectory, TurnClock,
};
use turnframe_runtime::resolve::{AuthorizedCase, TargetResolver};
use turnframe_test::providers::{ScriptedProvider, ScriptedProviderBuilder, ScriptedUnderstanding};
use turnframe_test::stores::{FailurePoint, FakeStores};
use turnframe_test::workflows::InMemoryExecutor;
use turnframe_test::workflows::traveler::{TravelerState, TravelerWorkflow};
use turnframe_test::workflows::trip::{TripCommand, TripEvent, TripState, TripWorkflow};

/// The tenant every scenario runs in.
pub const ACCOUNT: &str = "aurora";

/// A second tenant, for the cross-tenant scenario.
pub const OTHER_ACCOUNT: &str = "other";

/// The trip workflow key.
pub const TRIP: &str = "trip";

/// The traveler workflow key.
pub const TRAVELER: &str = "traveler";

/// A fixed instant, so nothing in a scenario depends on the wall clock.
#[must_use]
pub fn now() -> DateTime<Utc> {
    DateTime::from_timestamp(1_700_000_000, 0).expect("a valid fixed instant")
}

/// The tenant.
#[must_use]
pub fn account() -> AccountId {
    AccountId::from(ACCOUNT)
}

/// An executor shared between the registry and the test that seeds it.
///
/// [`WorkflowRegistryBuilder::register`] takes the executor by value, so a test
/// that wants to seed a case needs a handle on the same instance. A local
/// newtype is the shortest way to give one to both.
pub struct SharedExecutor<W: turnframe_test::workflows::PureWorkflow>(pub Arc<InMemoryExecutor<W>>);

impl<W: turnframe_test::workflows::PureWorkflow> Clone for SharedExecutor<W> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

#[async_trait::async_trait]
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

/// An executor that fails every batch the same way, for the scenarios about
/// what a failed command may and may not produce.
///
/// The failure can be held back and armed later, which is what a scenario about
/// a *card* needs: the turn that opens the card has to succeed, and only the
/// command its answer authorizes may fail.
pub struct FailingExecutor<W: turnframe_test::workflows::PureWorkflow> {
    inner: SharedExecutor<W>,
    error: ExecutionError,
    armed: Arc<std::sync::atomic::AtomicBool>,
    refresh_after_failure: Option<(W::State, CaseRevision, bool)>,
    failed: std::sync::atomic::AtomicBool,
    failed_cases: std::sync::Mutex<std::collections::BTreeSet<CaseId>>,
    load_counts: Arc<std::sync::Mutex<std::collections::BTreeMap<CaseId, usize>>>,
}

impl<W: turnframe_test::workflows::PureWorkflow> FailingExecutor<W> {
    /// Wraps `inner` so `execute` always returns `error`.
    #[must_use]
    pub fn new(inner: SharedExecutor<W>, error: ExecutionError) -> Self {
        Self::armable(
            inner,
            error,
            Arc::new(std::sync::atomic::AtomicBool::new(true)),
        )
    }

    /// The same, failing only while `armed` is set.
    #[must_use]
    pub fn armable(
        inner: SharedExecutor<W>,
        error: ExecutionError,
        armed: Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        Self {
            inner,
            error,
            armed,
            refresh_after_failure: None,
            failed: std::sync::atomic::AtomicBool::new(false),
            failed_cases: std::sync::Mutex::new(std::collections::BTreeSet::new()),
            load_counts: Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::new())),
        }
    }

    /// Simulates the state visible after a failed command, optionally making
    /// the subsequent read fail.
    #[must_use]
    pub fn with_refresh_after_failure(
        mut self,
        refresh: Option<(W::State, CaseRevision, bool)>,
    ) -> Self {
        self.refresh_after_failure = refresh;
        self
    }

    #[must_use]
    fn with_load_counts(
        mut self,
        counts: Arc<std::sync::Mutex<std::collections::BTreeMap<CaseId, usize>>>,
    ) -> Self {
        self.load_counts = counts;
        self
    }
}

#[async_trait::async_trait]
impl<W: turnframe_test::workflows::PureWorkflow> WorkflowExecutor<W> for FailingExecutor<W> {
    async fn load(
        &self,
        account: &AccountId,
        case_id: &CaseId,
    ) -> Result<Versioned<Option<W::State>>, StoreError> {
        *self
            .load_counts
            .lock()
            .expect("load counts are not poisoned")
            .entry(case_id.clone())
            .or_default() += 1;
        if self.failed.load(std::sync::atomic::Ordering::SeqCst)
            && self
                .refresh_after_failure
                .as_ref()
                .is_some_and(|(_, _, fail)| *fail)
            && self
                .failed_cases
                .lock()
                .expect("failed cases are not poisoned")
                .contains(case_id)
        {
            return Err(StoreError::Unavailable);
        }
        self.inner.load(account, case_id).await
    }

    async fn execute(
        &self,
        batch: CommandBatch<W::Command>,
    ) -> Result<Commit<W::State, W::Event>, ExecutionError> {
        if self.armed.load(std::sync::atomic::Ordering::SeqCst) {
            let envelope = batch.envelopes.first().expect("nonempty batch");
            if let Some((state, revision, _)) = &self.refresh_after_failure {
                self.inner.0.seed(
                    &envelope.actor.account_id,
                    &envelope.case_ref.case_id,
                    state.clone(),
                    *revision,
                );
            }
            self.failed_cases
                .lock()
                .expect("failed cases are not poisoned")
                .insert(envelope.case_ref.case_id.clone());
            self.failed.store(true, std::sync::atomic::Ordering::SeqCst);
            return Err(self.error.clone());
        }
        self.inner.execute(batch).await
    }
}

/// An executor that refuses to be written through at all.
///
/// It exists for the plan-only path: "planning executes nothing" is worth
/// asserting with the process itself rather than with a count, because a count
/// can be read after the effect has already happened.
pub struct PanicOnWriteExecutor<W: turnframe_test::workflows::PureWorkflow> {
    inner: SharedExecutor<W>,
}

impl<W: turnframe_test::workflows::PureWorkflow> PanicOnWriteExecutor<W> {
    /// Wraps `inner`, keeping its reads and forbidding its writes.
    #[must_use]
    pub fn new(inner: SharedExecutor<W>) -> Self {
        Self { inner }
    }
}

#[async_trait::async_trait]
impl<W: turnframe_test::workflows::PureWorkflow> WorkflowExecutor<W> for PanicOnWriteExecutor<W> {
    async fn load(
        &self,
        account: &AccountId,
        case_id: &CaseId,
    ) -> Result<Versioned<Option<W::State>>, StoreError> {
        self.inner.load(account, case_id).await
    }

    async fn execute(
        &self,
        _batch: CommandBatch<W::Command>,
    ) -> Result<Commit<W::State, W::Event>, ExecutionError> {
        panic!("a path that must not write reached the workflow executor");
    }
}

/// A knowledge provider that answers with a fixed set of chunks, or refuses.
#[derive(Debug, Clone, Default)]
pub struct FixedKnowledge {
    chunks: Vec<KnowledgeChunk>,
    unavailable: bool,
}

impl FixedKnowledge {
    /// A provider that finds nothing, which is what "no approved source says
    /// anything about this" looks like.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// A provider that answers with one chunk of `text`.
    #[must_use]
    pub fn saying(text: &str) -> Self {
        Self {
            chunks: vec![KnowledgeChunk {
                chunk_id: "chunk-1".to_owned(),
                source_id: "account_records".to_owned(),
                source_version: None,
                effective_from: None,
                effective_to: None,
                permissions: Vec::new(),
                citation: turnframe_core::knowledge::Citation {
                    source_id: "account_records".to_owned(),
                    label: "The account's own records".to_owned(),
                    uri: None,
                    locator: None,
                    version: None,
                },
                text: text.to_owned(),
                trust: turnframe_core::read::TrustLevel::Authoritative,
                sensitivity: turnframe_core::read::DataSensitivity::Internal,
            }],
            unavailable: false,
        }
    }

    /// A provider whose sources are down.
    #[must_use]
    pub fn unavailable() -> Self {
        Self {
            chunks: Vec::new(),
            unavailable: true,
        }
    }
}

#[async_trait::async_trait]
impl KnowledgeProvider for FixedKnowledge {
    async fn retrieve(
        &self,
        _request: KnowledgeRequest,
    ) -> Result<Vec<KnowledgeChunk>, KnowledgeError> {
        if self.unavailable {
            return Err(KnowledgeError::Unavailable {
                source_id: "fixture".to_owned(),
            });
        }
        Ok(self.chunks.clone())
    }
}

/// A clock that moves on by a fixed step every time it is read.
///
/// It exists so a test can make a turn "take" ten seconds without waiting ten
/// seconds, which is what the wall-clock half of a resource budget needs.
#[derive(Debug)]
pub struct SteppingClock {
    start: DateTime<Utc>,
    step: chrono::TimeDelta,
    reads: std::sync::atomic::AtomicI64,
}

impl SteppingClock {
    /// A clock starting at `start` and advancing `step` per reading. The first
    /// reading is `start` itself.
    #[must_use]
    pub fn new(start: DateTime<Utc>, step: chrono::TimeDelta) -> Self {
        Self {
            start,
            step,
            reads: std::sync::atomic::AtomicI64::new(0),
        }
    }

    /// A clock advancing `seconds` per reading, from the scenario instant.
    #[must_use]
    pub fn every(seconds: i64) -> Self {
        Self::new(now(), chrono::TimeDelta::seconds(seconds))
    }
}

impl TurnClock for SteppingClock {
    fn now(&self) -> DateTime<Utc> {
        let reads = self
            .reads
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.start + self.step * i32::try_from(reads).unwrap_or(i32::MAX)
    }
}

/// One signal, as the runtime emitted it.
#[derive(Debug, Clone, PartialEq)]
pub struct Observed {
    /// Which signal.
    pub signal: Signal,
    /// The labels it carried, verbatim.
    pub labels: SignalLabels,
    /// The measured value, for a duration signal.
    pub duration: Option<std::time::Duration>,
}

/// Records every signal a turn emitted, with its labels.
///
/// It records the labels and not only the name because half of what a metric
/// promises is its dimensions: a `provider.fallback` with no provider on it
/// cannot answer the question the panel exists to ask.
#[derive(Debug, Default)]
pub struct RecordingObserver {
    seen: std::sync::Mutex<Vec<Observed>>,
}

impl RecordingObserver {
    /// A fresh recorder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Everything it saw, in order.
    #[must_use]
    pub fn all(&self) -> Vec<Observed> {
        self.seen
            .lock()
            .map(|seen| seen.clone())
            .unwrap_or_default()
    }

    /// The distinct signals it saw.
    #[must_use]
    pub fn distinct(&self) -> std::collections::BTreeSet<&'static str> {
        self.all()
            .into_iter()
            .map(|observed| observed.signal.name())
            .collect()
    }

    /// How many times `signal` was observed.
    #[must_use]
    pub fn count(&self, signal: Signal) -> usize {
        self.all()
            .iter()
            .filter(|observed| observed.signal == signal)
            .count()
    }

    /// Every occurrence of `signal`.
    #[must_use]
    pub fn occurrences(&self, signal: Signal) -> Vec<Observed> {
        self.all()
            .into_iter()
            .filter(|observed| observed.signal == signal)
            .collect()
    }

    /// The one occurrence of `signal`, failing when there is not exactly one.
    ///
    /// "Exactly once" is the assertion that matters: a signal emitted twice for
    /// one occurrence doubles a panel, and one emitted zero times empties it.
    #[must_use]
    pub fn once(&self, signal: Signal) -> Observed {
        let found = self.occurrences(signal);
        assert_eq!(
            found.len(),
            1,
            "{} fired {} times, not once; the turn saw {:?}",
            signal.name(),
            found.len(),
            self.distinct()
        );
        found.into_iter().next().expect("exactly one")
    }
}

impl Observer for RecordingObserver {
    fn observe(&self, signal: &Signal) {
        self.observe_labeled(signal, &SignalLabels::none());
    }

    fn observe_labeled(&self, signal: &Signal, labels: &SignalLabels) {
        self.record(*signal, labels.clone(), None);
    }

    fn observe_duration(
        &self,
        signal: &Signal,
        duration: std::time::Duration,
        labels: &SignalLabels,
    ) {
        self.record(*signal, labels.clone(), Some(duration));
    }
}

impl RecordingObserver {
    fn record(&self, signal: Signal, labels: SignalLabels, duration: Option<std::time::Duration>) {
        if let Ok(mut seen) = self.seen.lock() {
            seen.push(Observed {
                signal,
                labels,
                duration,
            });
        }
    }
}

/// Asserts that `observed` carries exactly the labels `expected` names, and no
/// others.
///
/// Written as an equality on the whole label set rather than a field-by-field
/// check, so a label nobody asked for — the higher-cardinality one somebody
/// adds later — fails the test instead of passing unnoticed.
pub fn assert_labels(observed: &Observed, expected: &SignalLabels) {
    assert_eq!(
        &observed.labels,
        expected,
        "{} carried the wrong labels",
        observed.signal.name()
    );
}

/// A provider that writes one acknowledgement and passes its review, which is what a
/// plain turn needs.
#[must_use]
pub fn narrating() -> ScriptedProviderBuilder {
    ScriptedProvider::builder("scripted", "model-1")
        .acknowledging("Right, here is where that leaves things.")
}

/// A provider that answers nothing at all, so any call is a loud failure.
#[must_use]
pub fn silent() -> ScriptedProviderBuilder {
    ScriptedProvider::builder("scripted", "model-1")
}

/// Assembles the runtime for one scenario.
pub struct HarnessBuilder {
    /// What a turn's writes imply elsewhere, when a test declares it.
    consequences: Option<Arc<dyn turnframe_runtime::orchestrator::TurnConsequences>>,
    /// The languages the deployment declares.
    locales: Vec<turnframe_core::locale::Locale>,
    cases: Vec<(CaseCandidate, Option<TripState>, CaseRevision)>,
    travelers: Vec<(CaseCandidate, Option<TravelerState>, CaseRevision)>,
    broken: Vec<CaseCandidate>,
    unwritten: bool,
    understander: Arc<ScriptedUnderstanding>,
    tasks: Option<Arc<turnframe_tasks::testing::ScriptedTasks>>,
    trace: Option<Arc<turnframe_runtime::trace::JsonlTrace>>,
    attachment_source: Option<Arc<dyn turnframe_core::turn::AttachmentSource>>,
    providers: Vec<Arc<ScriptedProvider>>,
    config: OrchestratorConfig,
    knowledge: Option<Arc<dyn KnowledgeProvider>>,
    observer: Option<Arc<RecordingObserver>>,
    trip_failure: Option<ExecutionError>,
    trip_refresh_after_failure: Option<(TripState, CaseRevision, bool)>,
    trip_failure_armed: Arc<std::sync::atomic::AtomicBool>,
    trip_panics_on_write: bool,
    origins: Vec<(turnframe_core::ids::OriginToken, CaseKey)>,
    clock: Option<Arc<dyn TurnClock>>,
    prompts: Option<Arc<dyn turnframe_core::prompt::PromptSource>>,
    notice_copy: Option<turnframe_runtime::reduce::NoticeCopy>,
    confirmation_copy: Option<turnframe_runtime::policy::ConfirmationCopy>,
    directory: Option<Arc<dyn CaseDirectory>>,
}

impl Default for HarnessBuilder {
    fn default() -> Self {
        Self::new()
    }
}

impl HarnessBuilder {
    /// An empty harness with the conservative configuration.
    #[must_use]
    pub fn new() -> Self {
        Self {
            consequences: None,
            locales: Vec::new(),
            cases: Vec::new(),
            travelers: Vec::new(),
            broken: Vec::new(),
            unwritten: false,
            understander: Arc::new(ScriptedUnderstanding::new()),
            tasks: None,
            trace: None,
            attachment_source: None,
            providers: Vec::new(),
            notice_copy: None,
            confirmation_copy: None,
            config: OrchestratorConfig::conservative(),
            knowledge: None,
            observer: None,
            trip_failure: None,
            trip_refresh_after_failure: None,
            trip_failure_armed: Arc::new(std::sync::atomic::AtomicBool::new(true)),
            trip_panics_on_write: false,
            origins: Vec::new(),
            clock: None,
            prompts: None,
            directory: None,
        }
    }

    /// Lets a prompt source supply the instructions of both model stages.
    #[must_use]
    pub fn prompts(mut self, source: Arc<dyn turnframe_core::prompt::PromptSource>) -> Self {
        self.prompts = Some(source);
        self
    }

    /// Replaces the runtime clock.
    #[must_use]
    pub fn clock(mut self, clock: Arc<dyn TurnClock>) -> Self {
        self.clock = Some(clock);
        self
    }

    /// Replaces the directory the runtime asks which cases the actor may
    /// address.
    ///
    /// The default is a [`StaticCaseDirectory`] holding every seeded case, which
    /// is what a scenario about anything else wants. A scenario about the
    /// authorization boundary itself needs a directory that answers differently
    /// for different actors, and that is what this is for.
    #[must_use]
    pub fn case_directory(mut self, directory: Arc<dyn CaseDirectory>) -> Self {
        self.directory = Some(directory);
        self
    }

    /// Binds a server-issued origin token to a seeded case (spec §12.4).
    #[must_use]
    pub fn origin(mut self, token: &str, workflow: &str, case_id: &str) -> Self {
        self.origins.push((
            turnframe_core::ids::OriginToken::from(token),
            CaseKey::new(workflow, case_id),
        ));
        self
    }

    /// Understands turns with the real task pipeline, its calls answered by `tasks`,
    /// instead of the understandings a test queues.
    #[must_use]
    pub fn understanding_tasks(
        mut self,
        tasks: Arc<turnframe_tasks::testing::ScriptedTasks>,
    ) -> Self {
        self.tasks = Some(tasks);
        self
    }

    /// Writes every turn event and model call to `trace`.
    #[must_use]
    pub fn trace(mut self, trace: Arc<turnframe_runtime::trace::JsonlTrace>) -> Self {
        self.trace = Some(trace);
        self
    }

    /// Declares the languages the deployment serves.
    #[must_use]
    pub fn locales(mut self, locales: &[&str]) -> Self {
        self.locales = locales.iter().map(|locale| (*locale).into()).collect();
        self
    }

    /// Queues what the next turn is understood to say.
    #[must_use]
    pub fn understands(self, understanding: Understanding) -> Self {
        self.understander.push(understanding);
        self
    }

    /// Seeds one trip at `revision` with `label`.
    #[must_use]
    pub fn trip(mut self, case_id: &str, label: &str, revision: u64, state: TripState) -> Self {
        self.cases.push((
            CaseCandidate::new(CaseKey::new(TRIP, case_id), label),
            Some(state),
            CaseRevision(revision),
        ));
        self
    }

    /// Declares that every write on the case added last has to pass through a
    /// click, as the directory may for a record the actor did not necessarily
    /// mean.
    #[must_use]
    pub fn protected(mut self) -> Self {
        if let Some((candidate, _, _)) = self.cases.last_mut() {
            *candidate = candidate.clone().confirming_every_write();
        }
        self
    }

    /// Declares that the case added last is in view only because the actor may
    /// reach it, as a record of another thread of work is.
    #[must_use]
    pub fn reachable_only(mut self) -> Self {
        if let Some((candidate, _, _)) = self.cases.last_mut() {
            *candidate = candidate.clone().subject_only_when_named();
        }
        self
    }

    /// Seeds one traveler at `revision`.
    #[must_use]
    pub fn traveler(
        mut self,
        case_id: &str,
        label: &str,
        revision: u64,
        state: TravelerState,
    ) -> Self {
        self.travelers.push((
            CaseCandidate::new(CaseKey::new(TRAVELER, case_id), label),
            Some(state),
            CaseRevision(revision),
        ));
        self
    }

    /// Registers [`StartsWithoutWriting`], for a scenario about the phase a case
    /// is in before it exists.
    #[must_use]
    /// Where the bytes of the turn's files come from.
    pub fn attachments(mut self, source: Arc<dyn turnframe_core::turn::AttachmentSource>) -> Self {
        self.attachment_source = Some(source);
        self
    }

    pub fn unwritten(mut self) -> Self {
        self.unwritten = true;
        self
    }

    /// Offers a case of the [`BrokenWorkflow`], whose projection breaks a §8.4
    /// invariant.
    #[must_use]
    pub fn broken_case(mut self, case_id: &str, label: &str) -> Self {
        self.broken
            .push(CaseCandidate::new(CaseKey::new(BROKEN, case_id), label));
        self
    }

    /// Adds a scripted provider, in routing order.
    #[must_use]
    pub fn provider(mut self, provider: Arc<ScriptedProvider>) -> Self {
        self.providers.push(provider);
        self
    }

    /// Declares what this deployment's turns imply on other cases.
    #[must_use]
    pub fn consequences(
        mut self,
        consequences: Arc<dyn turnframe_runtime::orchestrator::TurnConsequences>,
    ) -> Self {
        self.consequences = Some(consequences);
        self
    }

    /// Supplies the notices the reducer writes itself.
    #[must_use]
    pub fn notice_copy(mut self, copy: turnframe_runtime::reduce::NoticeCopy) -> Self {
        self.notice_copy = Some(copy);
        self
    }

    /// Supplies the copy on confirmation cards.
    #[must_use]
    pub fn confirmation_copy(mut self, copy: turnframe_runtime::policy::ConfirmationCopy) -> Self {
        self.confirmation_copy = Some(copy);
        self
    }

    /// Replaces the configuration.
    #[must_use]
    pub fn config(mut self, config: OrchestratorConfig) -> Self {
        self.config = config;
        self
    }

    /// Replaces the narration section.
    #[must_use]
    pub fn narration(mut self, narration: NarrationConfig) -> Self {
        self.config.narration = narration;
        self
    }

    /// Turns narration off, so the turn is receipts, notices and cards only.
    #[must_use]
    pub fn without_narration(mut self) -> Self {
        self.config.narration = NarrationConfig::conservative().with_enabled(false);
        self
    }

    /// Attaches a knowledge provider.
    #[must_use]
    pub fn knowledge(mut self, knowledge: Arc<dyn KnowledgeProvider>) -> Self {
        self.knowledge = Some(knowledge);
        self
    }

    /// Counts the signals the turn emits.
    #[must_use]
    pub fn observing(mut self) -> Self {
        self.observer = Some(Arc::new(RecordingObserver::new()));
        self
    }

    /// Makes every trip command fail with `error`.
    #[must_use]
    pub fn trip_fails(mut self, error: ExecutionError) -> Self {
        self.trip_failure = Some(error);
        self
    }

    /// The same, but held back until [`Harness::arm_trip_failure`] is
    /// called, so the turns before it behave normally.
    #[must_use]
    pub fn trip_fails_once_armed(mut self, error: ExecutionError) -> Self {
        self.trip_failure = Some(error);
        self.trip_failure_armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self
    }

    /// Installs the trip visible after a failed execution, optionally making
    /// the final refresh unavailable.
    #[must_use]
    pub fn trip_refresh_after_failure(
        mut self,
        state: TripState,
        revision: CaseRevision,
        fail_read: bool,
    ) -> Self {
        self.trip_refresh_after_failure = Some((state, revision, fail_read));
        self
    }

    /// Makes the trip executor panic if anything ever tries to write
    /// through it.
    #[must_use]
    pub fn trip_panics_on_write(mut self) -> Self {
        self.trip_panics_on_write = true;
        self
    }

    /// Builds the harness and creates the conversation.
    pub async fn build(self) -> Harness {
        self.try_build().await.expect("a complete orchestrator")
    }

    /// Builds the runtime, or says why the orchestrator refused to be built.
    pub async fn try_build(self) -> Result<Harness, turnframe_runtime::orchestrator::BuildError> {
        let stores = FakeStores::at(now());
        let trip = Arc::new(InMemoryExecutor::new(TripWorkflow::new().with_cards()));
        let traveler = Arc::new(InMemoryExecutor::new(
            TravelerWorkflow::only_while_a_trip_is_open().with_cards(),
        ));
        let broken = Arc::new(InMemoryExecutor::new(BrokenWorkflow::default()));
        let unwritten = Arc::new(InMemoryExecutor::new(StartsWithoutWriting::default()));
        let trip_load_counts = Arc::new(std::sync::Mutex::new(std::collections::BTreeMap::<
            CaseId,
            usize,
        >::new()));
        let account = account();

        for (candidate, state, revision) in &self.cases {
            if let Some(state) = state {
                trip.seed(&account, &candidate.key.case_id, state.clone(), *revision);
            }
        }
        for (candidate, state, revision) in &self.travelers {
            if let Some(state) = state {
                traveler.seed(&account, &candidate.key.case_id, state.clone(), *revision);
            }
        }

        let trip_shared = SharedExecutor(Arc::clone(&trip));
        let with_trip = if self.trip_panics_on_write {
            WorkflowRegistry::builder().register(
                TripWorkflow::new().with_cards(),
                PanicOnWriteExecutor::new(trip_shared),
            )
        } else {
            match self.trip_failure {
                Some(error) => WorkflowRegistry::builder().register(
                    TripWorkflow::new().with_cards(),
                    FailingExecutor::armable(
                        trip_shared,
                        error,
                        Arc::clone(&self.trip_failure_armed),
                    )
                    .with_refresh_after_failure(self.trip_refresh_after_failure)
                    .with_load_counts(Arc::clone(&trip_load_counts)),
                ),
                None => WorkflowRegistry::builder()
                    .register(TripWorkflow::new().with_cards(), trip_shared),
            }
        };
        let mut builder = with_trip.register(
            TravelerWorkflow::only_while_a_trip_is_open().with_cards(),
            SharedExecutor(Arc::clone(&traveler)),
        );
        // Registered only when a scenario asks for it: a third workflow is a
        // third entry in every "no case yet" catalog, and no other scenario
        // should have to know about it.
        if !self.broken.is_empty() {
            builder = builder.register(
                BrokenWorkflow::default(),
                SharedExecutor(Arc::clone(&broken)),
            );
        }
        if self.unwritten {
            builder = builder.register(
                StartsWithoutWriting::default(),
                SharedExecutor(Arc::clone(&unwritten)),
            );
        }
        let registry = builder.build().expect("distinct workflow keys");

        let traced = |provider: Arc<dyn turnframe_provider::provider::ModelProvider>| match &self
            .trace
        {
            Some(trace) => Arc::new(turnframe_provider::trace::TracedProvider::new(
                provider,
                Arc::clone(trace) as Arc<dyn turnframe_provider::trace::CallTrace>,
            )) as Arc<dyn turnframe_provider::provider::ModelProvider>,
            None => provider,
        };
        let mut pool = ProviderPool::builder();
        if let Some(tasks) = &self.tasks {
            pool = pool.provider(traced(Arc::clone(tasks) as _));
        }
        for provider in &self.providers {
            pool = pool.provider(traced(Arc::clone(provider) as _));
        }
        let pool = Arc::new(pool.build().expect("the pool has distinct model keys"));

        let directory: Arc<dyn CaseDirectory> = match self.directory.clone() {
            Some(directory) => directory,
            None => {
                let mut directory = StaticCaseDirectory::new();
                for (candidate, _, _) in &self.cases {
                    directory = directory.with_case(candidate.clone());
                }
                for (candidate, _, _) in &self.travelers {
                    directory = directory.with_case(candidate.clone());
                }
                for candidate in &self.broken {
                    directory = directory.with_case(candidate.clone());
                }
                for (token, key) in &self.origins {
                    directory = directory.with_origin(
                        token.clone(),
                        CaseCandidate::new(key.clone(), "the record you have open"),
                    );
                }
                Arc::new(directory)
            }
        };

        let observer = self.observer.clone();
        let mut builder = Orchestrator::builder()
            .workflows(Arc::new(registry))
            .providers(Arc::clone(&pool))
            .stores(stores.stores().clone())
            .case_directory(directory)
            .policy(PolicySnapshot::conservative())
            .clock(
                self.clock
                    .clone()
                    .unwrap_or_else(|| Arc::new(FixedTurnClock(now()))),
            )
            .config(read_once(self.config.clone()));
        if self.tasks.is_none() {
            builder = builder.understander(Arc::clone(&self.understander) as _);
        }
        if let Some(consequences) = self.consequences {
            builder = builder.consequences(consequences);
        }
        if let Some(knowledge) = self.knowledge {
            builder = builder.knowledge(knowledge);
        }
        if let Some(observer) = observer.clone() {
            builder = builder.observer(observer);
        }
        if let Some(prompts) = self.prompts {
            builder = builder.prompt_source(prompts);
        }
        if let Some(copy) = self.notice_copy {
            builder = builder.notice_copy(copy);
        }
        if let Some(source) = self.attachment_source {
            builder = builder.attachments(source);
        }
        if let Some(copy) = self.confirmation_copy {
            builder = builder.confirmation_copy(copy);
        }
        if let Some(trace) = self.trace.clone() {
            builder = builder.trace(trace);
        }
        if !self.locales.is_empty() {
            builder = builder.locales(self.locales.clone());
        }
        let orchestrator = Arc::new(builder.build()?);

        let conversation = ConversationId::nil();
        stores
            .stores()
            .conversations()
            .create_conversation(turnframe_store::conversation::ConversationRecord::new(
                conversation,
                account.clone(),
                now(),
            ))
            .await
            .expect("the conversation is new");

        Ok(Harness {
            stores,
            trip,
            traveler,
            orchestrator,
            providers: self.providers,
            understander: self.understander,
            observer,
            conversation,
            trip_failure_armed: self.trip_failure_armed,
            trip_load_counts,
        })
    }
}

/// One scenario's runtime, stores and doubles.
pub struct Harness {
    /// The persistence layer, with failure injection and call counting.
    pub stores: FakeStores,
    /// The trip executor, for seeding and for asserting revisions.
    pub trip: Arc<InMemoryExecutor<TripWorkflow>>,
    /// The traveler executor.
    pub traveler: Arc<InMemoryExecutor<TravelerWorkflow>>,
    /// The runtime under test.
    pub orchestrator: Arc<Orchestrator>,
    /// The scripted providers, in routing order.
    pub providers: Vec<Arc<ScriptedProvider>>,
    /// What each turn is understood to say, queued by the test.
    pub understander: Arc<ScriptedUnderstanding>,
    /// The signal counter, when the scenario asked for one.
    pub observer: Option<Arc<RecordingObserver>>,
    /// The conversation every turn belongs to.
    pub conversation: ConversationId,
    /// Whether the trip executor is currently refusing every batch.
    trip_failure_armed: Arc<std::sync::atomic::AtomicBool>,
    trip_load_counts: Arc<std::sync::Mutex<std::collections::BTreeMap<CaseId, usize>>>,
}

impl Harness {
    /// Starts a builder.
    #[must_use]
    pub fn builder() -> HarnessBuilder {
        HarnessBuilder::new()
    }

    /// The tenant.
    #[must_use]
    pub fn account(&self) -> AccountId {
        account()
    }

    /// The signal recorder, for a scenario built with
    /// [`HarnessBuilder::observing`].
    #[must_use]
    pub fn observed(&self) -> Arc<RecordingObserver> {
        Arc::clone(
            self.observer
                .as_ref()
                .expect("the harness was built with `observing()`"),
        )
    }

    /// Queues what the next turn is understood to say.
    pub fn understands(&self, understanding: Understanding) {
        self.understander.push(understanding);
    }

    /// Starts failing every trip command, for a harness built with
    /// [`HarnessBuilder::trip_fails_once_armed`].
    pub fn arm_trip_failure(&self) {
        self.trip_failure_armed
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// A turn carrying text and the origin the surface issued (spec §12.4).
    #[must_use]
    pub fn turn_from(&self, turn_id: TurnId, text: &str, origin: &str) -> TurnInput {
        TurnInput {
            origin: Some(turnframe_core::turn::OriginRef {
                origin_token: turnframe_core::ids::OriginToken::from(origin),
                signature: None,
                surface: Some("trip_detail".to_owned()),
            }),
            ..self.turn(turn_id, text)
        }
    }

    /// A turn carrying only text.
    #[must_use]
    pub fn turn(&self, turn_id: TurnId, text: &str) -> TurnInput {
        TurnInput {
            turn_id,
            conversation_id: self.conversation,
            actor: ActorContext::new(account(), "u1"),
            text: Some(text.to_owned()),
            interaction_response: None,
            attachments: Vec::new(),
            origin: None,
            locale: Locale::from("en-GB"),
            effort: None,
        }
    }

    /// A turn carrying only a card answer (spec §9: no model call).
    #[must_use]
    pub fn click(
        &self,
        turn_id: TurnId,
        interaction_id: InteractionId,
        option: &str,
        revision: u64,
    ) -> TurnInput {
        TurnInput {
            interaction_response: Some(InteractionResponse {
                interaction_id,
                option_id: OptionId::from(option),
                expected_case_revision: CaseRevision(revision),
                freeform_input: None,
            }),
            text: None,
            ..self.turn(turn_id, "")
        }
    }

    /// A turn carrying both text and a card answer (spec §9).
    #[must_use]
    pub fn click_and_say(
        &self,
        turn_id: TurnId,
        interaction_id: InteractionId,
        option: &str,
        revision: u64,
        text: &str,
    ) -> TurnInput {
        TurnInput {
            text: Some(text.to_owned()),
            ..self.click(turn_id, interaction_id, option, revision)
        }
    }

    /// The opaque token this turn issues for a case. See [`token_for`].
    #[must_use]
    pub fn token(&self, turn_id: TurnId, workflow: &str, case_id: &str) -> TargetToken {
        token_for(turn_id, workflow, case_id)
    }

    /// The card waiting on a case, when it has exactly one.
    pub async fn blocking_card(&self, workflow: &str, case_id: &str) -> Interaction {
        let open = self.open_cards(workflow, case_id).await;
        open.into_iter()
            .find(|card| card.blocking)
            .expect("the case has a blocking card")
    }

    /// Runs one turn.
    pub async fn handle(
        &self,
        input: TurnInput,
    ) -> Result<AssistantTurn, turnframe_core::error::OrchestratorError> {
        self.orchestrator.handle_turn(input).await
    }

    /// The replay record a turn left behind.
    pub async fn replay(&self, turn_id: TurnId) -> turnframe_core::replay::ReplayRecord {
        self.stores
            .stores()
            .replay()
            .get(&account(), &turn_id)
            .await
            .expect("the turn recorded a replay record")
    }

    /// The open cards of a case.
    pub async fn open_cards(&self, workflow: &str, case_id: &str) -> Vec<Interaction> {
        self.stores
            .open_interactions(&account(), &CaseKey::new(workflow, case_id))
            .await
            .expect("the store answers")
    }

    /// The event types a case accumulated, in append order.
    pub async fn events(&self, workflow: &str, case_id: &str) -> Vec<String> {
        self.stores
            .event_types(&account(), &CaseKey::new(workflow, case_id))
            .await
            .expect("the store answers")
    }

    /// The journal entries of one turn.
    pub async fn journal(
        &self,
        turn_id: TurnId,
    ) -> Vec<turnframe_store::journal::CommandJournalEntry> {
        self.stores
            .journal_for_turn(&account(), &turn_id)
            .await
            .expect("the store answers")
    }

    /// The phase marker of one turn.
    pub async fn phase(&self, turn_id: TurnId) -> TurnPhase {
        self.stores
            .turn_phase(&account(), &turn_id)
            .await
            .expect("the turn exists")
            .phase
    }

    /// The persisted assistant turn.
    pub async fn stored_turn(&self, turn_id: TurnId) -> Option<AssistantTurn> {
        self.stores
            .turn(&account(), &turn_id)
            .await
            .ok()
            .and_then(|stored| stored.assistant)
    }

    /// The current revision of a trip.
    #[must_use]
    pub fn trip_revision(&self, case_id: &str) -> CaseRevision {
        self.trip.revision_of(&account(), &CaseId::from(case_id))
    }

    /// Number of reads through the failure-aware trip executor.
    #[must_use]
    pub fn trip_load_count(&self, case_id: &str) -> usize {
        self.trip_load_counts
            .lock()
            .expect("load counts are not poisoned")
            .get(&CaseId::from(case_id))
            .copied()
            .unwrap_or_default()
    }

    /// The name a trip holds, for a test that asserts on what was
    /// written rather than on what was planned.
    #[must_use]
    pub fn trip_name(&self, case_id: &str) -> Option<String> {
        self.trip
            .state_of(&account(), &CaseId::from(case_id))
            .and_then(|state| state.name)
    }

    /// Runs `command` on a trip outside any turn, as the airline's own systems do.
    pub async fn outside(
        &self,
        case_id: &str,
        key: &str,
        command: TripCommand,
    ) -> Result<Commit<TripState, TripEvent>, ExecutionError> {
        self.trip
            .execute(self.outside_batch(case_id, key, command))
            .await
    }

    /// The batch an outside system sends for `command`, bound to the trip's current
    /// revision. `key` names the delivery: the same batch delivered twice is one effect (I14).
    #[must_use]
    pub fn outside_batch(
        &self,
        case_id: &str,
        key: &str,
        command: TripCommand,
    ) -> CommandBatch<TripCommand> {
        use turnframe_core::command::{AtomicityScope, CommandEnvelope, CommandOrigin};
        use turnframe_core::ids::{BatchId, CommandId};
        use turnframe_core::understanding::{ActId, UnitId};
        let case_id = CaseId::from(case_id);
        let revision = self.trip.revision_of(&account(), &case_id);
        let seed = key.bytes().fold(0xa1_u128, |hash, byte| {
            hash.wrapping_mul(131).wrapping_add(u128::from(byte))
        });
        let turn_id = TurnId::from(uuid::Uuid::from_u128(seed));
        let case_ref = CaseRef::new(TRIP, case_id, revision);
        CommandBatch {
            batch_id: BatchId::derive(&turn_id, &case_ref.key(), &AtomicityScope::PerCase),
            scope: AtomicityScope::PerCase,
            envelopes: vec![CommandEnvelope {
                command_id: CommandId::derive(&turn_id, ActId::new(UnitId(1), 1), 0),
                turn_id,
                actor: ActorContext::new(account(), "airline"),
                case_ref,
                idempotency_key: IdempotencyKey::new(format!("airline:{key}")),
                origin: CommandOrigin::ExternalCallback {
                    callback_id: key.to_owned(),
                    signature_verified: true,
                },
                command,
            }],
        }
    }

    /// A trip's whole state.
    #[must_use]
    pub fn trip_state(&self, case_id: &str) -> Option<TripState> {
        self.trip.state_of(&account(), &CaseId::from(case_id))
    }

    /// The name of the traveler a trip is for, when it has one.
    #[must_use]
    pub fn trip_traveler(&self, case_id: &str) -> Option<String> {
        self.trip
            .state_of(&account(), &CaseId::from(case_id))
            .and_then(|state| state.traveler)
            .map(|traveler| traveler.display_name)
    }

    /// Arms a failure at one of the crash boundaries of spec §27.7.
    pub fn fail_at(&self, point: FailurePoint, error: StoreError) {
        self.stores
            .fail_at(point, error)
            .expect("the store is not poisoned");
    }

    /// Idempotency keys the turn journaled, in admission order.
    pub async fn idempotency_keys(&self, turn_id: TurnId) -> Vec<IdempotencyKey> {
        self.journal(turn_id)
            .await
            .into_iter()
            .map(|entry| entry.idempotency_key)
            .collect()
    }
}

/// The opaque token a turn issues for a case, derived exactly as the runtime
/// derives it.
///
/// A scripted plan has to name a target, and the point of a token is that the
/// model never sees the case identifier. Deriving it here rather than reading
/// it back keeps the fixture honest: the test names the case it means, and the
/// token it gets is the one the turn will issue.
#[must_use]
pub fn token_for(turn_id: TurnId, workflow: &str, case_id: &str) -> TargetToken {
    let key = CaseKey::new(workflow, case_id);
    TargetResolver::builder(account(), turn_id)
        .candidate(AuthorizedCase::new(
            CaseRef::new(workflow, case_id, CaseRevision::ZERO),
            "label",
        ))
        .build()
        .token_map()
        .token_for(&key)
        .cloned()
        .expect("a token was issued for the case")
}

/// The receipt status codes of a turn, in block order.
#[must_use]
pub fn receipt_codes(turn: &AssistantTurn) -> Vec<String> {
    turn.receipts()
        .map(|receipt| receipt.status_code.clone())
        .collect()
}

/// The notice codes of a turn, in block order.
#[must_use]
pub fn notice_codes(turn: &AssistantTurn) -> Vec<String> {
    turn.blocks
        .iter()
        .filter_map(|block| match block {
            ResponseBlock::Notice(notice) => Some(notice.code.clone()),
            _ => None,
        })
        .collect()
}

/// The kinds of block a turn carries, in order.
#[must_use]
pub fn block_kinds(turn: &AssistantTurn) -> Vec<&'static str> {
    turn.blocks
        .iter()
        .map(|block| match block {
            ResponseBlock::Answer(_) => "answer",
            ResponseBlock::Transition(_) => "transition",
            ResponseBlock::Receipt(_) => "receipt",
            ResponseBlock::Notice(_) => "notice",
            ResponseBlock::Interaction(_) => "interaction",
            ResponseBlock::Artifact(_) => "artifact",
            _ => "other",
        })
        .collect()
}

/// The model-authored text of a turn, concatenated.
#[must_use]
pub fn narration(turn: &AssistantTurn) -> String {
    turn.blocks
        .iter()
        .filter_map(|block| match block {
            ResponseBlock::Answer(answer) => Some(answer.text.as_str()),
            ResponseBlock::Transition(transition) => Some(transition.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A provider failure that makes routing move on to the next candidate.
#[must_use]
pub fn transient_failure() -> ProviderError {
    ProviderError::server(Some(503))
}

// ---------------------------------------------------------------------------
// A workflow whose map is wrong, for the invariant signal.
// ---------------------------------------------------------------------------

/// The traveler domain with one thing broken: every phase is user-owned.
///
/// Spec §8.4 says a user-owned phase has a blocking interaction requirement, so
/// a projection of a traveler that is waiting for nobody breaks the invariant
/// and the turn refuses the case. It exists because
/// [`Signal::WorkflowInvariantViolation`] is a defect signal, and a defect
/// signal nothing in the suite can produce is a signal nobody has checked.
///
/// Everything except [`phase_ownership`](WorkflowDefinition::phase_ownership)
/// is the real domain, so the break is exactly one line wide.
#[derive(Debug, Default, Clone, Copy)]
pub struct BrokenWorkflow(TravelerWorkflow);

/// Key the broken workflow registers under.
pub const BROKEN: &str = "broken";

/// The trip domain with one thing changed: starting it writes nothing.
///
/// A legitimate answer — two of the adopter's workflows are singletons where
/// reaching the case is the whole point — and the one that exposes the gap.
/// `StartWorkflow` mints an identifier and the act resolves onto it exactly, so
/// the case is the name of the turn; with no command there is nothing to
/// commit, nothing to load, and so no view, in the very phase where the
/// workflow has the most to say.
#[derive(Debug, Default, Clone, Copy)]
pub struct StartsWithoutWriting(turnframe_test::workflows::trip::TripWorkflow);

/// Key it registers under.
pub const UNWRITTEN: &str = "unwritten";

impl turnframe_core::flow::WorkflowDefinition for StartsWithoutWriting {
    type State = <turnframe_test::workflows::trip::TripWorkflow as turnframe_core::flow::WorkflowDefinition>::State;
    type Phase = <turnframe_test::workflows::trip::TripWorkflow as turnframe_core::flow::WorkflowDefinition>::Phase;
    type Obligation = <turnframe_test::workflows::trip::TripWorkflow as turnframe_core::flow::WorkflowDefinition>::Obligation;
    type Command = <turnframe_test::workflows::trip::TripWorkflow as turnframe_core::flow::WorkflowDefinition>::Command;
    type Event = <turnframe_test::workflows::trip::TripWorkflow as turnframe_core::flow::WorkflowDefinition>::Event;
    type Outcome = <turnframe_test::workflows::trip::TripWorkflow as turnframe_core::flow::WorkflowDefinition>::Outcome;

    fn key(&self) -> turnframe_core::ids::WorkflowKey {
        turnframe_core::ids::WorkflowKey::from(UNWRITTEN)
    }

    fn version(&self) -> turnframe_core::ids::WorkflowVersion {
        self.0.version()
    }

    /// A prerequisite, so a scenario can have this same start refused: with no
    /// traveler being filled in, the act never reaches a case that could exist.
    fn start_preconditions(&self) -> Vec<turnframe_core::flow::StartPrecondition> {
        vec![turnframe_core::flow::StartPrecondition::new(
            "traveler",
            [serde_json::json!("collecting")],
            turnframe_core::locale::LocalizedText::new("There is no traveler being filled in."),
        )]
    }

    /// The one change: the domain has nothing to write when it is started.
    fn compile_act(
        &self,
        state: Option<&Self::State>,
        view: &turnframe_core::flow::ViewOf<Self>,
        act: &turnframe_core::target::ResolvedAct,
    ) -> Result<Vec<Self::Command>, turnframe_core::error::DomainRejection> {
        if matches!(
            act.kind,
            turnframe_core::target::ResolvedActKind::StartWorkflow
        ) {
            return Ok(Vec::new());
        }
        self.0.compile_act(state, view, act)
    }

    fn phase_ownership(&self, phase: &Self::Phase) -> turnframe_core::flow::PhaseOwnership {
        self.0.phase_ownership(phase)
    }

    fn project(
        &self,
        case_ref: CaseRef,
        state: Option<&Self::State>,
    ) -> turnframe_core::flow::ViewOf<Self> {
        self.0.project(case_ref, state)
    }

    fn operations(
        &self,
        view: &turnframe_core::flow::ViewOf<Self>,
    ) -> Vec<turnframe_core::operation::OperationSpec> {
        self.0.operations(view)
    }

    fn transition_briefing(&self, view: &turnframe_core::flow::ViewOf<Self>) -> Option<String> {
        self.0.transition_briefing(view)
    }

    fn command_policy(
        &self,
        state: Option<&Self::State>,
        command: &Self::Command,
    ) -> turnframe_core::command::CommandPolicy {
        self.0.command_policy(state, command)
    }

    fn validate_command(
        &self,
        state: Option<&Self::State>,
        command: &Self::Command,
    ) -> Result<(), turnframe_core::error::DomainRejection> {
        self.0.validate_command(state, command)
    }

    fn receipts(
        &self,
        events: &[turnframe_core::event::ReceiptEvent<Self::Event>],
        locale: &Locale,
    ) -> Vec<turnframe_core::event::OperationalReceipt> {
        self.0.receipts(events, locale)
    }
}

impl turnframe_test::workflows::PureWorkflow for StartsWithoutWriting {
    fn apply(
        &self,
        state: Option<&Self::State>,
        command: &Self::Command,
    ) -> Result<
        turnframe_test::workflows::Applied<Self::State, Self::Event>,
        turnframe_core::error::DomainRejection,
    > {
        self.0.apply(state, command)
    }

    fn event_type(&self, event: &Self::Event) -> String {
        self.0.event_type(event)
    }
}

impl turnframe_core::flow::WorkflowDefinition for BrokenWorkflow {
    type State = <TravelerWorkflow as turnframe_core::flow::WorkflowDefinition>::State;
    type Phase = <TravelerWorkflow as turnframe_core::flow::WorkflowDefinition>::Phase;
    type Obligation = <TravelerWorkflow as turnframe_core::flow::WorkflowDefinition>::Obligation;
    type Command = <TravelerWorkflow as turnframe_core::flow::WorkflowDefinition>::Command;
    type Event = <TravelerWorkflow as turnframe_core::flow::WorkflowDefinition>::Event;
    type Outcome = <TravelerWorkflow as turnframe_core::flow::WorkflowDefinition>::Outcome;

    fn key(&self) -> turnframe_core::ids::WorkflowKey {
        turnframe_core::ids::WorkflowKey::from(BROKEN)
    }

    fn version(&self) -> turnframe_core::ids::WorkflowVersion {
        self.0.version()
    }

    /// The defect: a phase nobody is waiting on is claimed to be the user's.
    fn phase_ownership(&self, _phase: &Self::Phase) -> turnframe_core::flow::PhaseOwnership {
        turnframe_core::flow::PhaseOwnership::User
    }

    fn project(
        &self,
        case_ref: CaseRef,
        state: Option<&Self::State>,
    ) -> turnframe_core::flow::ViewOf<Self> {
        self.0.project(case_ref, state)
    }

    fn operations(
        &self,
        view: &turnframe_core::flow::ViewOf<Self>,
    ) -> Vec<turnframe_core::operation::OperationSpec> {
        self.0.operations(view)
    }

    fn compile_act(
        &self,
        state: Option<&Self::State>,
        view: &turnframe_core::flow::ViewOf<Self>,
        act: &turnframe_core::target::ResolvedAct,
    ) -> Result<Vec<Self::Command>, turnframe_core::error::DomainRejection> {
        self.0.compile_act(state, view, act)
    }

    fn command_policy(
        &self,
        state: Option<&Self::State>,
        command: &Self::Command,
    ) -> turnframe_core::command::CommandPolicy {
        self.0.command_policy(state, command)
    }

    fn validate_command(
        &self,
        state: Option<&Self::State>,
        command: &Self::Command,
    ) -> Result<(), turnframe_core::error::DomainRejection> {
        self.0.validate_command(state, command)
    }

    fn receipts(
        &self,
        events: &[turnframe_core::event::ReceiptEvent<Self::Event>],
        locale: &Locale,
    ) -> Vec<turnframe_core::event::OperationalReceipt> {
        self.0.receipts(events, locale)
    }
}

impl turnframe_test::workflows::PureWorkflow for BrokenWorkflow {
    fn apply(
        &self,
        state: Option<&Self::State>,
        command: &Self::Command,
    ) -> Result<
        turnframe_test::workflows::Applied<Self::State, Self::Event>,
        turnframe_core::error::DomainRejection,
    > {
        self.0.apply(state, command)
    }

    fn event_type(&self, event: &Self::Event) -> String {
        self.0.event_type(event)
    }
}

/// A script answers each task as the test configured it, so a scripted turn reads it that
/// many times: the votes `medium` adds on splitting, routing and doubt are measured live.
fn read_once(mut config: OrchestratorConfig) -> OrchestratorConfig {
    if config.effort.medium.settings.is_none() {
        config.effort.medium.settings = Some(config.understanding.settings);
    }
    for kind in [
        turnframe_tasks::TaskKind::Segment,
        turnframe_tasks::TaskKind::Route,
    ] {
        let configured = config.understanding.tasks.get(kind);
        let mut change = turnframe_tasks::ProfileChange::default();
        change.votes = Some(configured.votes);
        change.on_disagreement = Some(configured.on_disagreement);
        config.effort.medium.tasks = config.effort.medium.tasks.with(kind, change);
    }
    config
}

/// The words asking for the rebooking card, and their reading on `turn`: the card for
/// the first leg of `trip-1`.
#[must_use]
pub fn rebooking_requested(turn: TurnId) -> (&'static str, Understanding) {
    let text = "show me the rebooking card";
    let understanding = turnframe_test::providers::UnderstandingBuilder::of(text)
        .apply(
            turnframe_test::workflows::trip::operations::REQUEST_REBOOKING,
            token_for(turn, TRIP, "trip-1"),
            serde_json::json!({"leg": 1}),
            text,
        )
        .build()
        .expect("the words are in the message");
    (text, understanding)
}

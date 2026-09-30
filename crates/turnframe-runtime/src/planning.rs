//! Planning a turn without performing it (spec §23 steps A–K).
//!
//! An application migrating from another agent runs both paths on the same turns while
//! the old one stays authoritative. [`TurnPlanner::plan`] runs steps A through K — accept,
//! load and project, judge a card answer, issue tokens, understand, resolve, reduce and
//! police — and stops before step L: no card, no journal entry, no event, no replay
//! record is written. That is a property of the types: a planner holds
//! [`ReadOnlyStores`] and loaders, never an executor.
//!
//! [`SeededTurnPlanner`] takes the case state as input instead of loading it, so a
//! recorded turn replays against the state that preceded it. Traffic routing and kill
//! switches belong to the application; [`crate::divergence`] names what two paths
//! disagreed about.

use std::fmt;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use indexmap::IndexMap;
use turnframe_core::case::{CaseKey, CaseRef};
use turnframe_core::error::{AuthorizationError, InteractionError, OrchestratorError, StoreError};
use turnframe_core::flow::{ErasedWorkflowView, WorkflowDefinitions, WorkflowReadRegistry};
use turnframe_core::interaction::{Interaction, InteractionRejection, InteractionSpec};
use turnframe_core::observe::{NoopObserver, Observer, Signal, SignalLabels};
use turnframe_core::policy::{PolicyDecision, PolicySnapshot};
use turnframe_core::reduce::{CommandRef, ReductionPlan};
use turnframe_core::replay::{TargetResolutionRecord, TaskRecord};
use turnframe_core::response::{AssistantTurn, ClaimClass};
use turnframe_core::turn::TurnInput;
use turnframe_core::understanding::{ActId, Understanding};
use turnframe_store::interaction::InteractionRecord;
use turnframe_store::stores::ReadOnlyStores;
use turnframe_understand::{NoSteps, TurnUnderstander};

use crate::config::OrchestratorConfig;
use crate::conversation::RecentMessage;
use crate::divergence::TurnSummary;
use crate::interactions::AcceptedInteraction;
use crate::orchestrator::{CaseDirectory, SystemTurnClock, TurnClock};
use crate::policy::PolicyEngine;
use crate::resolve::{CaseIdFactory, DerivedCaseIdFactory};
pub(crate) use crate::turn::{DirectoryTerms, LoadedCase};
use crate::turn::{
    addressable_cards, admit, admit_card_case, admit_typed, aim_at_found, blocking_summary,
    build_resolver, candidates_of_open_cards, card_act_of, command_refs, project_case,
    recent_messages, reduction_context, target_resolution_records, turn_reducer, typed_response,
    unlisted, would_claim,
};

/// A case handed to [`SeededTurnPlanner::plan`] instead of being loaded: the state a
/// recorded turn preceded, in the erased form the registry boundary uses.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SeededCase {
    /// The case and the revision the state was read at.
    pub case_ref: CaseRef,
    /// The state, or `None` for a case that does not exist yet.
    pub state: Option<serde_json::Value>,
    /// The label the model sees; the case identifier when absent.
    pub label: Option<String>,
    /// Cards open on the case.
    pub open_interactions: Vec<Interaction>,
    /// Whether the case is in view only because the actor may reach it.
    pub subject_only_when_named: bool,
}

impl SeededCase {
    /// A case at `case_ref` holding `state`.
    #[must_use]
    pub fn new(case_ref: CaseRef, state: serde_json::Value) -> Self {
        Self {
            case_ref,
            state: Some(state),
            label: None,
            open_interactions: Vec::new(),
            subject_only_when_named: false,
        }
    }

    /// A case that does not exist yet.
    #[must_use]
    pub fn absent(case_ref: CaseRef) -> Self {
        Self {
            case_ref,
            state: None,
            label: None,
            open_interactions: Vec::new(),
            subject_only_when_named: false,
        }
    }

    /// Sets the label.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Declares the case in view only because the actor may reach it.
    #[must_use]
    pub const fn reachable_only(mut self) -> Self {
        self.subject_only_when_named = true;
        self
    }

    /// Adds a card open on the case.
    #[must_use]
    pub fn with_open_interaction(mut self, interaction: Interaction) -> Self {
        self.open_interactions.push(interaction);
        self
    }
}

/// What a turn would have done, stopped before its first effect.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PlannedTurn {
    /// The projections of the cases the turn addressed.
    pub views: Vec<ErasedWorkflowView>,
    /// What the turn was understood to say; `None` for a click with no text.
    pub understanding: Option<Understanding>,
    /// Every model task understanding ran.
    pub tasks: Vec<TaskRecord>,
    /// How each act's target resolved.
    pub target_resolutions: Vec<TargetResolutionRecord>,
    /// The reduction.
    pub reduction: ReductionPlan,
    /// The policy decision for every command.
    pub policy_decisions: Vec<PolicyDecision>,
    /// The cards the turn would write before anything runs.
    pub would_persist: Vec<InteractionSpec>,
    /// The commands it would execute now.
    pub would_execute: Vec<CommandRef>,
    /// The claim classes its answer would be entitled to.
    pub would_claim: Vec<ClaimClass>,
}

impl PlannedTurn {
    /// The summary two paths are compared on.
    #[must_use]
    pub fn summary(&self) -> TurnSummary {
        TurnSummary::from_planned(self)
    }
}

/// Plans turns against live, read-only stores.
pub struct TurnPlanner {
    workflows: WorkflowReadRegistry,
    stores: ReadOnlyStores,
    directory: Arc<dyn CaseDirectory>,
    shared: SharedPlanning,
}

impl fmt::Debug for TurnPlanner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TurnPlanner")
            .field("workflows", &self.workflows)
            .field("stores", &self.stores)
            .finish_non_exhaustive()
    }
}

/// Plans turns against state it is handed.
pub struct SeededTurnPlanner {
    definitions: WorkflowDefinitions,
    shared: SharedPlanning,
}

impl fmt::Debug for SeededTurnPlanner {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SeededTurnPlanner")
            .field("definitions", &self.definitions)
            .finish_non_exhaustive()
    }
}

/// What both planners share with the orchestrator that built them.
#[derive(Clone)]
pub(crate) struct SharedPlanning {
    pub(crate) understander: Arc<dyn TurnUnderstander>,
    pub(crate) policy_engine: PolicyEngine,
    pub(crate) policy: PolicySnapshot,
    pub(crate) config: OrchestratorConfig,
    pub(crate) clock: Arc<dyn TurnClock>,
    pub(crate) case_ids: Arc<dyn CaseIdFactory>,
    pub(crate) observer: Arc<dyn Observer>,
    /// Whether a knowledge source can answer a question about the domain in general.
    pub(crate) knowledge: bool,
}

impl TurnPlanner {
    /// Starts a builder.
    #[must_use]
    pub fn builder() -> TurnPlannerBuilder {
        TurnPlannerBuilder::default()
    }

    pub(crate) fn assemble(
        workflows: WorkflowReadRegistry,
        stores: ReadOnlyStores,
        directory: Arc<dyn CaseDirectory>,
        shared: SharedPlanning,
    ) -> Self {
        Self {
            workflows,
            stores,
            directory,
            shared,
        }
    }

    /// The seeded planner over the same definitions and parts.
    #[must_use]
    pub fn seeded(&self) -> SeededTurnPlanner {
        SeededTurnPlanner {
            definitions: self.workflows.definitions().clone(),
            shared: self.shared.clone(),
        }
    }

    /// Plans `input` against the stores, stopping before the first effect.
    ///
    /// # Errors
    ///
    /// Whatever the same steps of a real turn would fail with.
    pub async fn plan(&self, input: TurnInput) -> Result<PlannedTurn, OrchestratorError> {
        let now = self.shared.clock.now();
        input.validate_shape_within(&self.shared.config.understanding.turn_limits)?;
        let account = input.actor.account_id.clone();
        self.stores
            .conversations()
            .load_conversation(&account, &input.conversation_id)
            .await
            .map_err(|error| match error {
                StoreError::NotFound => {
                    OrchestratorError::Unauthorized(AuthorizationError::ConversationNotAccessible {
                        conversation_id: input.conversation_id,
                    })
                }
                other => OrchestratorError::Store(other),
            })?;
        let open_interactions = self
            .stores
            .interactions()
            .list_open_for_conversation(&account, &input.conversation_id)
            .await
            .map_err(OrchestratorError::Store)?;
        let (cases, origin_case) = self.load_cases(&input, &open_interactions).await?;
        let open_interactions = addressable_cards(&cases, open_interactions);
        let answered = match input.interaction_response.as_ref() {
            None => None,
            Some(response) => {
                let record = self
                    .stores
                    .interactions()
                    .get(&account, &response.interaction_id)
                    .await
                    .map_err(|error| match error {
                        StoreError::NotFound => OrchestratorError::Interaction(
                            InteractionError::Rejected(InteractionRejection::NotFound),
                        ),
                        _ => OrchestratorError::Interaction(InteractionError::NotPersisted),
                    })?;
                admit(&input, &cases, record, now)?
            }
        };
        let turns = self
            .stores
            .conversations()
            .load_recent_turns(
                &account,
                &input.conversation_id,
                self.shared
                    .config
                    .understanding
                    .transcript_turns
                    .unwrap_or(usize::MAX),
            )
            .await
            .unwrap_or_default();
        let (recent, previous) = recent_messages(turns, input.turn_id);
        plan_with(
            &self.shared,
            self.workflows.definitions(),
            &input,
            PlanningContext {
                lookup: Some(Lookup {
                    directory: self.directory.as_ref(),
                    workflows: &self.workflows,
                }),
                now,
                cases,
                open_interactions,
                origin_case,
                answered,
                recent,
                previous,
            },
        )
        .await
    }

    async fn load_cases(
        &self,
        input: &TurnInput,
        open_interactions: &[Interaction],
    ) -> Result<(IndexMap<CaseKey, LoadedCase>, Option<CaseKey>), OrchestratorError> {
        let account = &input.actor.account_id;
        let mut candidates = self
            .directory
            .candidates(&input.actor, &input.conversation_id)
            .await
            .map_err(OrchestratorError::Store)?;
        let mut origin_case = None;
        if let Some(origin) = input.origin.as_ref()
            && let Some(candidate) = self
                .directory
                .resolve_origin(&input.actor, &origin.origin_token)
                .await
                .map_err(OrchestratorError::Store)?
        {
            origin_case = Some(candidate.key.clone());
            candidates.push(candidate);
        }
        let from_cards = candidates_of_open_cards(&candidates, open_interactions);
        let mut cases = IndexMap::new();
        for (candidate, from_card) in candidates
            .into_iter()
            .map(|candidate| (candidate, false))
            .chain(from_cards.into_iter().map(|candidate| (candidate, true)))
        {
            if cases.contains_key(&candidate.key) {
                continue;
            }
            let Ok(definition) = self
                .workflows
                .definitions()
                .require(&candidate.key.workflow)
                .cloned()
            else {
                continue;
            };
            let loader = self.workflows.require_loader(&candidate.key.workflow)?;
            let loaded = loader
                .load_case(account, &candidate.key.case_id)
                .await
                .map_err(OrchestratorError::Store)?;
            let candidate = if from_card {
                let Some(authorized) = admit_card_case(
                    self.shared.observer.as_ref(),
                    self.directory.as_ref(),
                    &input.actor,
                    &input.conversation_id,
                    candidate,
                    loaded.value.is_some(),
                )
                .await
                .map_err(OrchestratorError::Store)?
                else {
                    continue;
                };
                authorized
            } else {
                candidate
            };
            let case_ref = candidate.key.clone().at(loaded.revision);
            cases.insert(
                candidate.key,
                project_case(
                    self.shared.observer.as_ref(),
                    &definition,
                    case_ref,
                    candidate.label,
                    loaded.value,
                    DirectoryTerms {
                        confirm_every_write: candidate.confirm_every_write,
                        subject_only_when_named: candidate.subject_only_when_named,
                    },
                )?,
            );
        }
        Ok((cases, origin_case))
    }
}

impl SeededTurnPlanner {
    /// Starts a builder.
    #[must_use]
    pub fn builder() -> SeededTurnPlannerBuilder {
        SeededTurnPlannerBuilder::default()
    }

    /// Plans `input` against `cases`, stopping before the first effect.
    ///
    /// # Errors
    ///
    /// Whatever the same steps of a real turn would fail with.
    pub async fn plan(
        &self,
        input: TurnInput,
        cases: Vec<SeededCase>,
    ) -> Result<PlannedTurn, OrchestratorError> {
        let now = self.shared.clock.now();
        input.validate_shape_within(&self.shared.config.understanding.turn_limits)?;
        let mut loaded = IndexMap::new();
        let mut open_interactions = Vec::new();
        for seeded in cases {
            let key = seeded.case_ref.key();
            let definition = self.definitions.require(&key.workflow)?.clone();
            open_interactions.extend(seeded.open_interactions);
            let label = seeded
                .label
                .unwrap_or_else(|| seeded.case_ref.case_id.to_string());
            loaded.insert(
                key,
                project_case(
                    self.shared.observer.as_ref(),
                    &definition,
                    seeded.case_ref,
                    label,
                    seeded.state,
                    DirectoryTerms {
                        confirm_every_write: false,
                        subject_only_when_named: seeded.subject_only_when_named,
                    },
                )?,
            );
        }
        let open_interactions = addressable_cards(&loaded, open_interactions);
        let answered = match input.interaction_response.as_ref() {
            None => None,
            Some(response) => {
                let Some(interaction) = open_interactions
                    .iter()
                    .find(|open| open.id == response.interaction_id)
                    .cloned()
                else {
                    return Err(OrchestratorError::Interaction(InteractionError::Rejected(
                        InteractionRejection::NotFound,
                    )));
                };
                admit(&input, &loaded, InteractionRecord::new(interaction), now)?
            }
        };
        plan_with(
            &self.shared,
            &self.definitions,
            &input,
            PlanningContext {
                lookup: None,
                now,
                cases: loaded,
                open_interactions,
                origin_case: None,
                answered,
                recent: Vec::new(),
                previous: None,
            },
        )
        .await
    }
}

struct PlanningContext<'a> {
    lookup: Option<Lookup<'a>>,
    now: DateTime<Utc>,
    cases: IndexMap<CaseKey, LoadedCase>,
    open_interactions: Vec<Interaction>,
    origin_case: Option<CaseKey>,
    answered: Option<AcceptedInteraction>,
    recent: Vec<RecentMessage>,
    previous: Option<AssistantTurn>,
}

/// Where a plan looks up a record the message named and the turn did not have.
struct Lookup<'a> {
    directory: &'a dyn CaseDirectory,
    workflows: &'a WorkflowReadRegistry,
}

impl Lookup<'_> {
    /// Loads what the directory finds for each unlisted act, and returns it per act.
    async fn find(
        &self,
        shared: &SharedPlanning,
        input: &TurnInput,
        understanding: &Understanding,
        cases: &mut IndexMap<CaseKey, LoadedCase>,
    ) -> Result<std::collections::BTreeMap<ActId, Vec<CaseKey>>, OrchestratorError> {
        let text = input.text.as_deref().unwrap_or_default();
        let limit = shared.config.interaction.max_selection_candidates;
        let mut found = std::collections::BTreeMap::new();
        for (act, workflow, named) in unlisted(understanding, text) {
            let candidates = self
                .directory
                .find(
                    &input.actor,
                    &input.conversation_id,
                    &workflow,
                    named.as_deref(),
                )
                .await
                .map_err(OrchestratorError::Store)?;
            let mut keys = Vec::new();
            for candidate in candidates.into_iter().take(limit) {
                keys.push(candidate.key.clone());
                if let Some(case) = load_listed(shared, self.workflows, input, candidate).await? {
                    cases.insert(case.case_ref.key(), case);
                }
            }
            found.insert(act, keys);
        }
        Ok(found)
    }
}

/// One candidate the directory listed, loaded and projected; `None` for a workflow
/// this runtime does not host.
async fn load_listed(
    shared: &SharedPlanning,
    workflows: &WorkflowReadRegistry,
    input: &TurnInput,
    candidate: crate::orchestrator::CaseCandidate,
) -> Result<Option<LoadedCase>, OrchestratorError> {
    let Ok(definition) = workflows
        .definitions()
        .require(&candidate.key.workflow)
        .cloned()
    else {
        return Ok(None);
    };
    let loaded = workflows
        .require_loader(&candidate.key.workflow)?
        .load_case(&input.actor.account_id, &candidate.key.case_id)
        .await
        .map_err(OrchestratorError::Store)?;
    let case_ref = candidate.key.clone().at(loaded.revision);
    project_case(
        shared.observer.as_ref(),
        &definition,
        case_ref,
        candidate.label,
        loaded.value,
        DirectoryTerms {
            confirm_every_write: candidate.confirm_every_write,
            subject_only_when_named: candidate.subject_only_when_named,
        },
    )
    .map(Some)
}

async fn plan_with(
    shared: &SharedPlanning,
    definitions: &WorkflowDefinitions,
    input: &TurnInput,
    context: PlanningContext<'_>,
) -> Result<PlannedTurn, OrchestratorError> {
    let PlanningContext {
        lookup,
        now,
        mut cases,
        open_interactions,
        origin_case,
        mut answered,
        recent,
        previous,
    } = context;
    let origin = input
        .origin
        .as_ref()
        .map(|origin| &origin.origin_token)
        .zip(origin_case.as_ref());
    let mut resolver = build_resolver(
        &input.actor.account_id,
        input.turn_id,
        definitions,
        &shared.case_ids,
        &cases,
        origin,
        blocking_summary(answered.as_ref(), &open_interactions),
    );
    let mut operations = crate::understand::operation_catalog(definitions, &cases)?;
    let card = open_interactions
        .iter()
        .find(|interaction| interaction.blocking)
        .filter(|_| answered.is_none());
    let card_summary = card.map(crate::interactions::summarize);
    let text = input.text.as_deref().unwrap_or_default();
    let effort = crate::effort::resolve(
        &shared.config,
        input.effort.unwrap_or(shared.config.effort.default),
    );
    let understood = crate::understand::run(
        shared.understander.as_ref(),
        &crate::understand::Sources {
            turn: input.turn_id,
            definitions,
            cases: &cases,
            resolver: &resolver,
            text,
            locale: &input.locale,
            today: now.date_naive(),
            recent: &recent,
            previous: previous.as_ref(),
            card: card.zip(card_summary.as_ref()),
            typed_answers_allowed: shared.policy.allow_text_resolution_for_low_risk,
            config: &shared.config.understanding,
            effort: &effort,
            knowledge: shared.knowledge,
        },
        &operations,
        &NoSteps,
    )
    .await?;
    let mut understanding = understood.understanding;
    if let Some(lookup) = lookup {
        let found = lookup
            .find(shared, input, &understanding, &mut cases)
            .await?;
        if found.values().any(|keys| !keys.is_empty()) {
            resolver = build_resolver(
                &input.actor.account_id,
                input.turn_id,
                definitions,
                &shared.case_ids,
                &cases,
                origin,
                blocking_summary(answered.as_ref(), &open_interactions),
            );
            operations = crate::understand::operation_catalog(definitions, &cases)?;
            aim_at_found(&mut understanding, &found, &resolver);
        }
    }
    // A typed answer to the card on screen is admitted like a click, on the channel
    // that authorizes only what needs no confirmation.
    if answered.is_none()
        && let (Some(card), Some(typed)) = (card, understanding.card_answer.as_ref())
    {
        let response = typed_response(card, &typed.option);
        answered = admit_typed(input, &cases, card, &response, now).ok();
    }
    let with_card = card_act_of(understanding, answered.as_ref(), &resolver);
    let reduction_context = reduction_context(
        &cases,
        &open_interactions,
        &resolver,
        &operations,
        &shared.policy,
        shared.config.understanding.plan_limits,
        now,
    );
    let reducer = turn_reducer(
        definitions,
        &cases,
        &resolver,
        &shared.policy_engine,
        &shared.config,
        answered.as_ref(),
        with_card.card_act,
    );
    let stage = crate::signals::Stage::enter();
    let reduced = reducer.reduce_turn(input, &with_card.understanding, &reduction_context)?;
    stage.observe(
        shared.observer.as_ref(),
        Signal::ReductionDuration,
        &SignalLabels::none(),
    );
    Ok(PlannedTurn {
        views: cases.into_values().map(|case| case.view).collect(),
        understanding: (!text.trim().is_empty() || !with_card.understanding.acts.is_empty())
            .then(|| with_card.understanding.clone()),
        tasks: understood.tasks,
        target_resolutions: target_resolution_records(&reduced.plan),
        policy_decisions: reduced.plan.policy_decisions.clone(),
        would_persist: reduced.plan.pre_execution_interactions.clone(),
        would_execute: command_refs(&reduced.plan),
        would_claim: would_claim(&reduced.plan),
        reduction: reduced.plan,
    })
}

/// Collects everything a [`TurnPlanner`] needs.
#[derive(Default)]
pub struct TurnPlannerBuilder {
    workflows: Option<WorkflowReadRegistry>,
    stores: Option<ReadOnlyStores>,
    directory: Option<Arc<dyn CaseDirectory>>,
    shared: SharedBuilder,
}

impl fmt::Debug for TurnPlannerBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TurnPlannerBuilder")
            .field("workflows", &self.workflows.is_some())
            .field("stores", &self.stores.is_some())
            .finish_non_exhaustive()
    }
}

macro_rules! shared_setters {
    () => {
        /// Sets what understands each turn.
        #[must_use]
        pub fn understander(mut self, understander: Arc<dyn TurnUnderstander>) -> Self {
            self.shared.understander = Some(understander);
            self
        }

        /// Sets the policy snapshot.
        #[must_use]
        pub fn policy(mut self, policy: PolicySnapshot) -> Self {
            self.shared.policy = policy;
            self
        }

        /// Sets the configuration.
        #[must_use]
        pub fn config(mut self, config: OrchestratorConfig) -> Self {
            self.shared.config = config;
            self
        }

        /// Sets the clock.
        #[must_use]
        pub fn clock(mut self, clock: Arc<dyn TurnClock>) -> Self {
            self.shared.clock = clock;
            self
        }

        /// Sets the factory of new case identifiers.
        #[must_use]
        pub fn case_id_factory(mut self, factory: Arc<dyn CaseIdFactory>) -> Self {
            self.shared.case_ids = factory;
            self
        }

        /// Sets the observer.
        #[must_use]
        pub fn observer(mut self, observer: Arc<dyn Observer>) -> Self {
            self.shared.observer = observer;
            self
        }
    };
}

impl TurnPlannerBuilder {
    /// An empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the read-only registry.
    #[must_use]
    pub fn workflows(mut self, workflows: WorkflowReadRegistry) -> Self {
        self.workflows = Some(workflows);
        self
    }

    /// Sets the read-only stores.
    #[must_use]
    pub fn stores(mut self, stores: ReadOnlyStores) -> Self {
        self.stores = Some(stores);
        self
    }

    /// Sets the case directory.
    #[must_use]
    pub fn case_directory(mut self, directory: Arc<dyn CaseDirectory>) -> Self {
        self.directory = Some(directory);
        self
    }

    shared_setters!();

    /// Builds the planner.
    ///
    /// # Errors
    ///
    /// [`crate::orchestrator::BuildError`] for a missing part or an invalid configuration.
    pub fn build(self) -> Result<TurnPlanner, crate::orchestrator::BuildError> {
        use crate::orchestrator::BuildError;
        self.shared.config.validate()?;
        let workflows = self
            .workflows
            .ok_or(BuildError::Missing { part: "workflows" })?;
        let stores = self.stores.ok_or(BuildError::Missing { part: "stores" })?;
        let directory = self
            .directory
            .ok_or(BuildError::Missing { part: "directory" })?;
        Ok(TurnPlanner::assemble(
            workflows,
            stores,
            directory,
            self.shared.build()?,
        ))
    }
}

/// Collects everything a [`SeededTurnPlanner`] needs.
#[derive(Default)]
pub struct SeededTurnPlannerBuilder {
    definitions: Option<WorkflowDefinitions>,
    shared: SharedBuilder,
}

impl fmt::Debug for SeededTurnPlannerBuilder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SeededTurnPlannerBuilder")
            .field("definitions", &self.definitions.is_some())
            .finish_non_exhaustive()
    }
}

impl SeededTurnPlannerBuilder {
    /// An empty builder.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the workflow definitions.
    #[must_use]
    pub fn definitions(mut self, definitions: WorkflowDefinitions) -> Self {
        self.definitions = Some(definitions);
        self
    }

    shared_setters!();

    /// Builds the planner.
    ///
    /// # Errors
    ///
    /// [`crate::orchestrator::BuildError`] for a missing part or an invalid configuration.
    pub fn build(self) -> Result<SeededTurnPlanner, crate::orchestrator::BuildError> {
        use crate::orchestrator::BuildError;
        self.shared.config.validate()?;
        let definitions = self
            .definitions
            .ok_or(BuildError::Missing { part: "workflows" })?;
        Ok(SeededTurnPlanner {
            definitions,
            shared: self.shared.build()?,
        })
    }
}

struct SharedBuilder {
    understander: Option<Arc<dyn TurnUnderstander>>,
    policy: PolicySnapshot,
    config: OrchestratorConfig,
    clock: Arc<dyn TurnClock>,
    case_ids: Arc<dyn CaseIdFactory>,
    observer: Arc<dyn Observer>,
}

impl Default for SharedBuilder {
    fn default() -> Self {
        Self {
            understander: None,
            policy: PolicySnapshot::conservative(),
            config: OrchestratorConfig::conservative(),
            clock: Arc::new(SystemTurnClock),
            case_ids: Arc::new(DerivedCaseIdFactory),
            observer: Arc::new(NoopObserver),
        }
    }
}

impl SharedBuilder {
    fn build(self) -> Result<SharedPlanning, crate::orchestrator::BuildError> {
        use crate::orchestrator::BuildError;
        let understander = self.understander.ok_or(BuildError::Missing {
            part: "understander",
        })?;
        let policy_engine = PolicyEngine::new(&self.config);
        Ok(SharedPlanning {
            understander,
            policy_engine,
            policy: self.config.policy_snapshot(self.policy),
            config: self.config,
            clock: self.clock,
            case_ids: self.case_ids,
            observer: self.observer,
            // A planner alone answers nothing: it frames questions as a turn with a source.
            knowledge: true,
        })
    }
}

pub(crate) fn seeded_from_parts(
    definitions: WorkflowDefinitions,
    shared: SharedPlanning,
) -> SeededTurnPlanner {
    SeededTurnPlanner {
        definitions,
        shared,
    }
}

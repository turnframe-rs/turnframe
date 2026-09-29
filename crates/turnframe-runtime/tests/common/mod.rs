//! A toy trip workflow and the scaffolding the reducer scenarios need.
//!
//! The domain is deliberately small but has one command of every interesting
//! risk class: editing a field is reversible, rebooking is externally
//! regulated, deleting is destructive. That is enough to exercise every branch
//! of §13 without a database.
#![allow(dead_code)]

use std::sync::Arc;

use chrono::{DateTime, Utc};
use indexmap::IndexMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use turnframe_core::case::{CaseKey, CaseRef, Versioned};
use turnframe_core::command::{
    AtomicityScope, ClaimMode, CommandBatch, CommandOrigin, CommandPolicy, ConfirmationPolicy,
    ResolutionChannel, RiskClass,
};
use turnframe_core::error::{DomainRejection, ExecutionError, StoreError};
use turnframe_core::event::{Commit, OperationalReceipt, ReceiptEvent};
use turnframe_core::flow::{
    PhaseOwnership, ViewOf, WorkflowDefinition, WorkflowExecutor, WorkflowRegistry, WorkflowView,
};
use turnframe_core::hash::Digest;
use turnframe_core::ids::{
    AccountId, CaseId, CaseRevision, ConversationId, InteractionId, OperationKey, OptionId,
    TargetToken, TurnId, WorkflowKey, WorkflowVersion,
};
use turnframe_core::interaction::{ActionClass, InteractionKind, TextResolutionPolicy};
use turnframe_core::locale::Locale;
use turnframe_core::operation::{OperationCatalog, OperationSpec};
use turnframe_core::plan::TargetPolicy;
use turnframe_core::plan::limits::PlanLimits;
use turnframe_core::policy::PolicySnapshot;
use turnframe_core::reduce::{ActiveInteractionSummary, ReductionContext, TurnReducer};
use turnframe_core::target::ResolvedAct;
use turnframe_core::turn::{ActorContext, InteractionResponse, TurnInput};
use turnframe_core::understanding::{ActId, ActTarget, Understanding};
use turnframe_runtime::config::OrchestratorConfig;
use turnframe_runtime::policy::PolicyEngine;
use turnframe_runtime::reduce::DefaultTurnReducer;
use turnframe_runtime::resolve::{AuthorizedCase, TargetResolver};

/// The workflow key of the toy domain.
pub const WORKFLOW: &str = "trip";

/// Persisted state of one trip.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TripState {
    /// What the trip is for.
    pub name: Option<String>,
    /// When it flies.
    pub date: Option<String>,
    /// Whether the airline was asked to rebook it.
    pub rebooked: bool,
}

impl TripState {
    /// A draft with a name already set.
    pub fn with_name(name: &str) -> Self {
        Self {
            name: Some(name.to_owned()),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TripPhase {
    Draft,
    Rebooked,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TripObligation {
    Name,
    Date,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TripCommand {
    SetName { value: String },
    SetDate { value: String },
    Rebook,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TripEvent {
    NameSet,
    DateSet,
    Rebooked,
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TripOutcome {
    Ticketed,
}

#[derive(JsonSchema, Deserialize)]
#[allow(dead_code)]
struct ValueArgs {
    value: String,
}

/// Operation keys the toy catalog offers.
pub mod op {
    /// Sets the name. Reversible.
    pub const SET_NAME: &str = "trip.set_name";
    /// Sets the travel date. Reversible.
    pub const SET_DATE: &str = "trip.set_date";
    /// Asks the airline to rebook. Externally regulated.
    pub const REBOOK: &str = "trip.rebook";
    /// Deletes the draft. Destructive.
    pub const DELETE: &str = "trip.delete";
    /// Opens a trip. May mint a case.
    pub const OPEN: &str = "trip.open";
    /// Refused by the domain, whatever the policy says.
    pub const REFUSED: &str = "trip.refused";
    /// Compiles to nothing, so the act is a no-op.
    pub const NOOP: &str = "trip.noop";
    /// Only a card the server drew may run this one.
    pub const CARD_ONLY: &str = "trip.card_only";
    /// Records what the user wants rather than what they said; writes nothing.
    pub const DECLINE: &str = "trip.decline";
}

/// The toy workflow.
#[derive(Debug, Default, Clone, Copy)]
pub struct TripWorkflow;

impl WorkflowDefinition for TripWorkflow {
    type State = TripState;
    type Phase = TripPhase;
    type Obligation = TripObligation;
    type Command = TripCommand;
    type Event = TripEvent;
    type Outcome = TripOutcome;

    fn key(&self) -> WorkflowKey {
        WorkflowKey::from(WORKFLOW)
    }

    fn version(&self) -> WorkflowVersion {
        WorkflowVersion::from("1")
    }

    fn phase_ownership(&self, phase: &TripPhase) -> PhaseOwnership {
        match phase {
            TripPhase::Draft => PhaseOwnership::System,
            TripPhase::Rebooked => PhaseOwnership::Terminal,
        }
    }

    fn project(&self, case_ref: CaseRef, state: Option<&TripState>) -> ViewOf<Self> {
        let version = self.version();
        match state {
            Some(state) if state.rebooked => {
                WorkflowView::new(case_ref, version, TripPhase::Rebooked)
                    .with_outcome(TripOutcome::Ticketed)
            }
            state => {
                let mut obligations = Vec::new();
                if state.and_then(|s| s.name.as_ref()).is_none() {
                    obligations.push(TripObligation::Name);
                }
                if state.and_then(|s| s.date.as_ref()).is_none() {
                    obligations.push(TripObligation::Date);
                }
                WorkflowView::new(case_ref, version, TripPhase::Draft).with_obligations(obligations)
            }
        }
    }

    fn operations(&self, _view: &ViewOf<Self>) -> Vec<OperationSpec> {
        catalog_definitions()
    }

    fn compile_act(
        &self,
        _state: Option<&TripState>,
        _view: &ViewOf<Self>,
        act: &ResolvedAct,
    ) -> Result<Vec<TripCommand>, DomainRejection> {
        let value = || {
            act.arguments
                .get("value")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_owned()
        };
        match act.operation().map(OperationKey::as_str) {
            Some(op::SET_NAME) => Ok(vec![TripCommand::SetName { value: value() }]),
            Some(op::SET_DATE) => Ok(vec![TripCommand::SetDate { value: value() }]),
            Some(op::REBOOK) => Ok(vec![TripCommand::Rebook]),
            Some(op::DELETE) => Ok(vec![TripCommand::Delete]),
            Some(op::OPEN) => Ok(vec![TripCommand::SetName { value: value() }]),
            Some(op::NOOP) | Some(op::CARD_ONLY) | Some(op::DECLINE) => Ok(Vec::new()),
            Some(op::REFUSED) => Err(DomainRejection::new("trip.refused", "trip.error.refused")),
            // Starting a workflow and cancelling both land here.
            None => Ok(Vec::new()),
            Some(_) => Err(DomainRejection::new(
                "trip.unknown_operation",
                "trip.error.unknown_operation",
            )),
        }
    }

    fn command_policy(&self, _state: Option<&TripState>, command: &TripCommand) -> CommandPolicy {
        match command {
            TripCommand::SetName { .. } | TripCommand::SetDate { .. } => CommandPolicy::low_risk(),
            TripCommand::Rebook => CommandPolicy {
                risk: RiskClass::ExternalRegulated,
                confirmation: ConfirmationPolicy::ExplicitClick,
                atomicity: AtomicityScope::PerCase,
                claim_mode: ClaimMode::ServerReceiptOnly,
            },
            TripCommand::Delete => CommandPolicy {
                risk: RiskClass::Destructive,
                confirmation: ConfirmationPolicy::ExplicitClick,
                atomicity: AtomicityScope::PerCase,
                claim_mode: ClaimMode::ServerReceiptOnly,
            },
        }
    }

    fn validate_command(
        &self,
        state: Option<&TripState>,
        command: &TripCommand,
    ) -> Result<(), DomainRejection> {
        match command {
            TripCommand::Rebook if state.and_then(|s| s.name.as_ref()).is_none() => {
                Err(DomainRejection::new("trip.no_name", "trip.error.no_name"))
            }
            TripCommand::SetName { value } if value.is_empty() => Err(DomainRejection::new(
                "trip.empty_name",
                "trip.error.empty_name",
            )),
            _ => Ok(()),
        }
    }

    fn receipts(
        &self,
        _events: &[ReceiptEvent<TripEvent>],
        _locale: &Locale,
    ) -> Vec<OperationalReceipt> {
        Vec::new()
    }
}

/// An executor the deterministic half never calls; the registry wants one.
#[derive(Debug, Default, Clone, Copy)]
pub struct UnusedExecutor;

#[async_trait::async_trait]
impl WorkflowExecutor<TripWorkflow> for UnusedExecutor {
    async fn load(
        &self,
        _account: &AccountId,
        _case_id: &CaseId,
    ) -> Result<Versioned<Option<TripState>>, StoreError> {
        Err(StoreError::Unavailable)
    }

    async fn execute(
        &self,
        _batch: CommandBatch<TripCommand>,
    ) -> Result<Commit<TripState, TripEvent>, ExecutionError> {
        Err(ExecutionError::Timeout)
    }
}

fn definition(key: &str, target_policy: TargetPolicy, takes_value: bool) -> OperationSpec {
    let spec = OperationSpec::new(key).summary(key).target(target_policy);
    if takes_value {
        spec.arguments::<ValueArgs>()
    } else {
        spec
    }
}

/// Every operation the toy catalog offers.
pub fn catalog_definitions() -> Vec<OperationSpec> {
    vec![
        definition(op::SET_NAME, TargetPolicy::RequiresExistingCase, true),
        definition(op::SET_DATE, TargetPolicy::RequiresExistingCase, true),
        definition(op::REBOOK, TargetPolicy::RequiresExistingCase, false),
        definition(op::DELETE, TargetPolicy::RequiresExistingCase, false),
        definition(op::OPEN, TargetPolicy::AllowsNewCase, true),
        definition(op::REFUSED, TargetPolicy::RequiresExistingCase, false),
        definition(op::NOOP, TargetPolicy::RequiresExistingCase, false),
        definition(op::CARD_ONLY, TargetPolicy::RequiresExistingCase, false).card_only(),
        definition(op::DECLINE, TargetPolicy::RequiresExistingCase, false),
    ]
}

/// The toy catalog with each operation bound to the toy workflow.
pub fn catalog() -> OperationCatalog {
    OperationCatalog::new(catalog_definitions().into_iter().map(|mut spec| {
        spec.workflow = WorkflowKey::from(WORKFLOW);
        spec
    }))
    .expect("the toy catalog has unique keys")
}

/// The registry holding the toy workflow.
pub fn registry() -> Arc<WorkflowRegistry> {
    Arc::new(
        WorkflowRegistry::builder()
            .register(TripWorkflow, UnusedExecutor)
            .build()
            .expect("the toy registry has one workflow"),
    )
}

/// The tenant every fixture runs in.
pub fn account() -> AccountId {
    AccountId::from("acct")
}

/// A fixed instant, so nothing in a test depends on the wall clock.
pub fn now() -> DateTime<Utc> {
    DateTime::from_timestamp(1_700_000_000, 0).expect("a valid fixed instant")
}

/// A case reference in the toy workflow.
pub fn case(id: &str, revision: u64) -> CaseRef {
    CaseRef::new(WORKFLOW, id, CaseRevision(revision))
}

/// A turn carrying only text.
pub fn turn(text: &str) -> TurnInput {
    TurnInput {
        turn_id: TurnId::nil(),
        conversation_id: ConversationId::nil(),
        actor: ActorContext::new(account(), "u1"),
        text: Some(text.to_owned()),
        interaction_response: None,
        attachments: Vec::new(),
        origin: None,
        locale: Locale::from("en-GB"),
        effort: None,
    }
}

/// The interaction identifier every fixture card uses.
pub fn interaction_id() -> InteractionId {
    InteractionId::nil()
}

/// The origin the interaction engine would mint from a click on a confirmation.
pub fn confirmed_origin() -> CommandOrigin {
    CommandOrigin::ConfirmedInteraction {
        interaction_id: interaction_id(),
        payload_hash: Digest::of_bytes(b"payload"),
        interaction_kind: InteractionKind::ConfirmCommand,
        action_class: ActionClass::ConfirmsCommands,
        channel: ResolutionChannel::Click,
    }
}

/// A structured card answer, as a traveler would post it.
pub fn click(option: &str, revision: u64) -> InteractionResponse {
    InteractionResponse {
        interaction_id: interaction_id(),
        option_id: OptionId::from(option),
        expected_case_revision: CaseRevision(revision),
        freeform_input: None,
    }
}

/// An active blocking card that typed text may resolve.
pub fn low_risk_card(case_ref: CaseRef, options: &[&str]) -> ActiveInteractionSummary {
    ActiveInteractionSummary {
        interaction_id: interaction_id(),
        case_ref,
        kind: InteractionKind::SingleSelect,
        blocking: true,
        option_ids: options.iter().map(|o| OptionId::from(*o)).collect(),
        text_resolution: TextResolutionPolicy::ModelInterpretedLowRisk,
        confirms_risk: RiskClass::ReversibleLowRisk,
        payload_hash: Digest::of_bytes(b"payload"),
    }
}

/// An active blocking card whose answer authorizes a consequential command, so
/// inferred text may never resolve it (§13.2 rule 8).
pub fn high_risk_card(case_ref: CaseRef, options: &[&str]) -> ActiveInteractionSummary {
    ActiveInteractionSummary {
        kind: InteractionKind::ConfirmCommand,
        confirms_risk: RiskClass::ExternalRegulated,
        text_resolution: TextResolutionPolicy::Never,
        ..low_risk_card(case_ref, options)
    }
}

/// One case the actor may address, with its loaded state.
pub struct LoadedCase {
    /// Case and revision.
    pub case_ref: CaseRef,
    /// Server-authored label the model and the cards see.
    pub label: String,
    /// Persisted state; `None` means the case does not exist yet.
    pub state: Option<TripState>,
}

/// Assembles a resolver, a reducer and a reduction context around a set of
/// loaded cases.
pub struct Fixture {
    cases: Vec<LoadedCase>,
    active: Option<ActiveInteractionSummary>,
    config: OrchestratorConfig,
    confirmed_origin: Option<(CommandOrigin, ActId)>,
    registry: Arc<WorkflowRegistry>,
    reachable_only: std::collections::BTreeSet<turnframe_core::case::CaseKey>,
}

impl Default for Fixture {
    fn default() -> Self {
        Self::new()
    }
}

impl Fixture {
    /// An empty fixture with the conservative configuration.
    pub fn new() -> Self {
        Self {
            cases: Vec::new(),
            active: None,
            config: OrchestratorConfig::conservative(),
            confirmed_origin: None,
            registry: registry(),
            reachable_only: std::collections::BTreeSet::new(),
        }
    }

    /// Declares that a case is in view only because the actor may reach it.
    #[must_use]
    pub fn reachable_only(mut self, id: &str) -> Self {
        self.reachable_only.insert(case(id, 1).key());
        self
    }

    /// Adds a loaded case.
    pub fn case(mut self, id: &str, revision: u64, label: &str, state: TripState) -> Self {
        self.cases.push(LoadedCase {
            case_ref: case(id, revision),
            label: label.to_owned(),
            state: Some(state),
        });
        self
    }

    /// Declares the active blocking card.
    pub fn active(mut self, summary: ActiveInteractionSummary) -> Self {
        self.active = Some(summary);
        self
    }

    /// Replaces the configuration.
    pub fn config(mut self, config: OrchestratorConfig) -> Self {
        self.config = config;
        self
    }

    /// Declares the origin minted from this turn's card answer, and which act is the
    /// card's own.
    pub fn confirmed_origin(mut self, origin: CommandOrigin, card_act: ActId) -> Self {
        self.confirmed_origin = Some((origin, card_act));
        self
    }

    /// The target token issued for a case.
    pub fn token(&self, case_id: &str) -> TargetToken {
        self.resolver()
            .token_map()
            .token_for(&CaseKey::new(WORKFLOW, case_id))
            .unwrap_or_else(|| panic!("no token was issued for {case_id}"))
            .clone()
    }

    /// A record target for a case.
    pub fn target(&self, case_id: &str) -> ActTarget {
        ActTarget::Record {
            token: self.token(case_id),
        }
    }

    fn resolver(&self) -> TargetResolver {
        let mut builder = TargetResolver::builder(account(), TurnId::nil());
        for loaded in &self.cases {
            builder = builder.candidate(AuthorizedCase::new(
                loaded.case_ref.clone(),
                loaded.label.clone(),
            ));
        }
        if let Some(active) = &self.active {
            builder = builder.active_interaction(active.clone());
        }
        builder.build()
    }

    /// The reduction context for this fixture.
    pub fn context(&self) -> ReductionContext {
        let registered = self
            .registry
            .require(&WorkflowKey::from(WORKFLOW))
            .expect("the toy workflow is registered");
        let mut views = IndexMap::new();
        for loaded in &self.cases {
            let state = loaded
                .state
                .as_ref()
                .map(|s| serde_json::to_value(s).expect("state serializes"));
            let view = registered
                .definition
                .project(loaded.case_ref.clone(), state.as_ref())
                .expect("projection succeeds");
            views.insert(loaded.case_ref.key(), view);
        }
        ReductionContext {
            views,
            active_interactions: self.active.clone().into_iter().collect(),
            target_map: self.resolver().token_map().clone(),
            operations: catalog(),
            policy: self.config.policy_snapshot(PolicySnapshot::conservative()),
            limits: PlanLimits::conservative(),
            now: now(),
            confirm_every_write: Default::default(),
            subject_only_when_named: self.reachable_only.clone(),
        }
    }

    /// The reducer for this fixture.
    pub fn reducer(&self) -> DefaultTurnReducer {
        let states = self.cases.iter().filter_map(|loaded| {
            loaded.state.as_ref().map(|state| {
                (
                    loaded.case_ref.key(),
                    serde_json::to_value(state).expect("state serializes"),
                )
            })
        });
        let mut reducer = DefaultTurnReducer::new(
            Arc::clone(&self.registry),
            self.resolver(),
            PolicyEngine::new(&self.config),
            &self.config,
        )
        .with_states(states.collect::<Vec<_>>());
        if let Some((origin, card_act)) = &self.confirmed_origin {
            reducer = reducer.with_confirmed_origin(origin.clone(), *card_act);
        }
        reducer
    }

    /// Reduces one turn.
    pub fn reduce(
        &self,
        input: &TurnInput,
        understanding: &Understanding,
    ) -> Result<turnframe_core::reduce::ReductionPlan, turnframe_core::error::ReductionError> {
        self.reducer().reduce(input, understanding, &self.context())
    }
}

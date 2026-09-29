//! What `StartWorkflow` means when the account already has a case.
//!
//! Starting a workflow is the runtime's own door: it needs no catalogue entry and
//! carries no target. What *start* means differs by workflow: a second trip
//! is an ordinary thing to want, a second configuration of the same entity is
//! not. So the workflow declares [`StartBehaviour::ResumesOpenCase`], and a start
//! then reaches the case the directory lists in the same turn.
//!
//! Several open cases are refused, not offered as a selection card: the act
//! carries no target, so a card the user answered would rebind to the same act
//! and ask the same question again.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::now;
use turnframe_core::case::CaseRef;
use turnframe_core::error::DomainRejection;
use turnframe_core::flow::{
    ErasedWorkflow, PhaseOwnership, StartBehaviour, TypedWorkflowAdapter, ViewOf,
    WorkflowDefinition, WorkflowDefinitions,
};
use turnframe_core::ids::{CaseRevision, TurnId, WorkflowKey, WorkflowVersion};
use turnframe_core::locale::Locale;
use turnframe_core::operation::OperationSpec;
use turnframe_core::reduce::PlannedActResult;
use turnframe_core::target::{ResolvedAct, ResolvedActKind, TargetResolution};
use turnframe_core::turn::{ActorContext, TurnInput};
use turnframe_runtime::config::OrchestratorConfig;
use turnframe_runtime::orchestrator::FixedTurnClock;
use turnframe_runtime::planning::{PlannedTurn, SeededCase, SeededTurnPlanner};
use turnframe_test::providers::{ScriptedUnderstanding, UnderstandingBuilder};
use turnframe_test::workflows::trip::{TripWorkflow, incomplete_case};

/// The key of the workflow there is only ever one of.
const CONFIGURATION: &str = "configuration";

fn turn_one() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(1))
}

/// A workflow of which an account has one, modelled on the trip sample so
/// that the only thing under test is the declaration.
///
/// Its `compile_act` is the other half of declaring it: asked to start an open
/// case, the domain has nothing to do.
#[derive(Debug, Default, Clone, Copy)]
struct ConfigurationWorkflow(TripWorkflow);

impl WorkflowDefinition for ConfigurationWorkflow {
    type State = <TripWorkflow as WorkflowDefinition>::State;
    type Phase = <TripWorkflow as WorkflowDefinition>::Phase;
    type Obligation = <TripWorkflow as WorkflowDefinition>::Obligation;
    type Command = <TripWorkflow as WorkflowDefinition>::Command;
    type Event = <TripWorkflow as WorkflowDefinition>::Event;
    type Outcome = <TripWorkflow as WorkflowDefinition>::Outcome;

    fn key(&self) -> WorkflowKey {
        WorkflowKey::from(CONFIGURATION)
    }

    fn version(&self) -> WorkflowVersion {
        self.0.version()
    }

    /// The declaration under test.
    fn start_behaviour(&self) -> StartBehaviour {
        StartBehaviour::ResumesOpenCase
    }

    fn phase_ownership(&self, phase: &Self::Phase) -> PhaseOwnership {
        self.0.phase_ownership(phase)
    }

    fn project(&self, case_ref: CaseRef, state: Option<&Self::State>) -> ViewOf<Self> {
        self.0.project(case_ref, state)
    }

    fn operations(&self, view: &ViewOf<Self>) -> Vec<OperationSpec> {
        self.0.operations(view)
    }

    fn compile_act(
        &self,
        state: Option<&Self::State>,
        view: &ViewOf<Self>,
        act: &ResolvedAct,
    ) -> Result<Vec<Self::Command>, DomainRejection> {
        match (&act.kind, state) {
            // Already open, so starting it is a request that has been granted.
            (ResolvedActKind::StartWorkflow, Some(_)) => Ok(Vec::new()),
            _ => self.0.compile_act(state, view, act),
        }
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
    ) -> Result<(), DomainRejection> {
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

/// Both workflows, so a turn can start either one.
fn definitions() -> WorkflowDefinitions {
    let configuration: Arc<dyn ErasedWorkflow> = Arc::new(TypedWorkflowAdapter::new(
        ConfigurationWorkflow::default(),
        (),
    ));
    let trip: Arc<dyn ErasedWorkflow> = Arc::new(TypedWorkflowAdapter::new(
        TripWorkflow::new().with_cards(),
        (),
    ));
    WorkflowDefinitions::new().with(configuration).with(trip)
}

/// One case of `workflow` per entry in `open`, and a turn that asks to start it.
async fn start(workflow: &str, open: &[&str]) -> PlannedTurn {
    let turn_id = turn_one();
    let text = "Set up my invoicing";
    let understanding = UnderstandingBuilder::of(text)
        .start(workflow, text)
        .build()
        .unwrap();
    let planner = SeededTurnPlanner::builder()
        .definitions(definitions())
        .understander(Arc::new(ScriptedUnderstanding::new().then(understanding)))
        .config(OrchestratorConfig::conservative())
        .clock(Arc::new(FixedTurnClock(now())))
        .build()
        .unwrap();

    let state = serde_json::to_value(incomplete_case()).unwrap();
    let cases = open
        .iter()
        .map(|case_id| {
            SeededCase::new(
                CaseRef::new(workflow, *case_id, CaseRevision(3)),
                state.clone(),
            )
            .with_label(*case_id)
        })
        .collect();
    planner
        .plan(
            TurnInput {
                turn_id,
                conversation_id: turnframe_core::ids::ConversationId::nil(),
                actor: ActorContext::new(support::account(), "u1"),
                text: Some(text.to_owned()),
                interaction_response: None,
                attachments: Vec::new(),
                origin: None,
                locale: Locale::from("en-GB"),
                effort: None,
            },
            cases,
        )
        .await
        .unwrap()
}

/// The case the one act resolved to, whatever became of it.
fn resolved_case(planned: &PlannedTurn) -> String {
    match planned.reduction.acts[0]
        .target
        .as_ref()
        .expect("a start resolves somewhere")
    {
        TargetResolution::Exact { case_ref } => case_ref.case_id.to_string(),
        other => panic!("the start did not resolve to a case: {other:?}"),
    }
}

/// A start reaches the case the account already has.
#[tokio::test]
async fn starting_a_workflow_that_is_already_open_reaches_that_case() {
    let planned = start(CONFIGURATION, &["cfg-1"]).await;
    assert_eq!(
        resolved_case(&planned),
        "cfg-1",
        "the directory listed the case in this very turn"
    );
    assert!(
        matches!(planned.reduction.acts[0].result, PlannedActResult::NoChange),
        "and the domain, asked to start what is already open, has nothing to do: {:?}",
        planned.reduction.acts[0].result
    );
}

/// With nothing open, it mints, exactly as this door always did.
#[tokio::test]
async fn starting_a_workflow_with_nothing_open_still_mints() {
    let planned = start(CONFIGURATION, &[]).await;
    let case_id = resolved_case(&planned);
    assert!(
        case_id.starts_with("tf_"),
        "a start with nothing to reach opens a record: {case_id}"
    );
}

/// With several open, it refuses rather than opening one more.
///
/// Refusing and not clarifying: the act carries no target, so a selection card
/// the user answered would rebind to this same act and ask again.
#[tokio::test]
async fn starting_a_workflow_that_is_open_twice_is_refused() {
    let planned = start(CONFIGURATION, &["cfg-1", "cfg-2"]).await;
    let PlannedActResult::Rejected { rejection } = &planned.reduction.acts[0].result else {
        panic!("expected a refusal: {:?}", planned.reduction.acts[0].result);
    };
    assert_eq!(
        rejection.code.as_str(),
        "turnframe.workflow.several_open_cases"
    );
    assert!(
        planned.would_persist.is_empty(),
        "and no card is written for a question answering it cannot settle"
    );
}

/// The declaration is what does it, not the presence of a case: the trip
/// workflow has one open and declares nothing, so its start mints.
#[tokio::test]
async fn a_workflow_that_declares_nothing_mints_beside_the_case_it_has() {
    let planned = start("trip", &["trip-1"]).await;
    let case_id = resolved_case(&planned);
    assert!(
        case_id.starts_with("tf_"),
        "nothing was declared, so nothing changed: {case_id}"
    );
}

/// The refusal has a sentence of its own, not the catch-all shared by refusals
/// the copy map has no arm for.
#[test]
fn the_refusal_reaches_the_user_under_its_own_code() {
    let copy = turnframe_runtime::reduce::NoticeCopy::english();
    let (code, text) = copy
        .runtime_refusal(turnframe_runtime::reduce::rejection::SEVERAL_OPEN_CASES)
        .expect("the refusal has copy of its own");
    assert_eq!(
        code,
        turnframe_runtime::reduce::rejection::SEVERAL_OPEN_CASES
    );
    assert!(
        text.resolve(&Locale::from("en-GB"))
            .contains("more than one"),
        "and it says what happened: {text:?}"
    );
}

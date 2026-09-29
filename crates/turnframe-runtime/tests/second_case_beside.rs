//! Whether a workflow may have a second case opened beside the one it has.
//!
//! [`StartBehaviour::ResumesOpenCase`] governs where a start lands. The other door
//! that opens a case is an ordinary operation aimed at a new record, and that one
//! asks the domain through `WorkflowDefinition::may_open_beside`, handed the cases
//! of the workflow already open. The runtime has no opinion worth having: an
//! unfinished draft must not have a second one beside it, while one merely waiting
//! to be sent may, so "make me another" keeps working. That is a fact about the
//! domain, so the domain is asked.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::now;
use turnframe_core::case::CaseRef;
use turnframe_core::error::{DomainRejection, RejectionCode};
use turnframe_core::flow::{
    ErasedWorkflow, PhaseOwnership, TypedWorkflowAdapter, ViewOf, WorkflowDefinition,
    WorkflowDefinitions,
};
use turnframe_core::ids::{CaseRevision, TurnId, WorkflowKey, WorkflowVersion};
use turnframe_core::locale::{Locale, LocalizedText};
use turnframe_core::operation::OperationSpec;
use turnframe_core::reduce::PlannedActResult;
use turnframe_core::target::ResolvedAct;
use turnframe_core::turn::{ActorContext, TurnInput};
use turnframe_runtime::config::OrchestratorConfig;
use turnframe_runtime::orchestrator::FixedTurnClock;
use turnframe_runtime::planning::{PlannedTurn, SeededCase, SeededTurnPlanner};
use turnframe_test::providers::{ScriptedUnderstanding, UnderstandingBuilder};
use turnframe_test::workflows::trip::{
    TripState, TripWorkflow, complete_case, incomplete_case, operations,
};

/// The key of the workflow that will not have two unfinished cases.
const LEDGER: &str = "ledger";

/// The code the domain refuses under, so the assertion is about its rejection
/// and not about one of the runtime's.
const UNFINISHED: &str = "ledger.previous_one_unfinished";

fn turn_one() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(1))
}

/// What a workflow may declare about a second open case.
///
/// Two rules and not one, because the library must carry neither. The runtime's
/// job is to ask and to honour the answer, and a hook that can only be tested
/// against one rule has not been shown to do the second half.
#[derive(Debug, Default, Clone, Copy)]
enum Rule {
    /// Refuse only while a case still has something open on it, which is the
    /// distinction the adopter actually needs: an unfinished draft must not have
    /// a second one beside it, and one merely waiting to be sent may, so "make
    /// me another" keeps working.
    #[default]
    WhileUnfinished,
    /// One at a time, whatever state it is in.
    OneAtATime,
}

/// A workflow with an opinion about a second open case, modelled on the trip
/// sample so the only thing under test is the declaration.
#[derive(Debug, Default, Clone, Copy)]
struct LedgerWorkflow(TripWorkflow, Rule);

impl LedgerWorkflow {
    const fn under(rule: Rule) -> Self {
        Self(TripWorkflow::new().with_cards(), rule)
    }
}

impl WorkflowDefinition for LedgerWorkflow {
    type State = <TripWorkflow as WorkflowDefinition>::State;
    type Phase = <TripWorkflow as WorkflowDefinition>::Phase;
    type Obligation = <TripWorkflow as WorkflowDefinition>::Obligation;
    type Command = <TripWorkflow as WorkflowDefinition>::Command;
    type Event = <TripWorkflow as WorkflowDefinition>::Event;
    type Outcome = <TripWorkflow as WorkflowDefinition>::Outcome;

    fn key(&self) -> WorkflowKey {
        WorkflowKey::from(LEDGER)
    }

    fn version(&self) -> WorkflowVersion {
        self.0.version()
    }

    /// The declaration under test.
    fn may_open_beside(&self, open: &[ViewOf<Self>]) -> Result<(), DomainRejection> {
        let refuse = match self.1 {
            Rule::WhileUnfinished => open.iter().any(|view| !view.obligations.is_empty()),
            Rule::OneAtATime => !open.is_empty(),
        };
        if refuse {
            return Err(
                DomainRejection::new(RejectionCode::from(UNFINISHED), UNFINISHED).with_explanation(
                    LocalizedText::new("That trip is still unfinished: shall we go back to it?"),
                ),
            );
        }
        Ok(())
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

/// The one workflow under test.
///
/// One and not two: the sample's operation keys are the same whichever wrapper
/// carries them, the catalogue is keyed by operation, and the first registration
/// would win — so a second workflow here would not be reachable, only confusing.
fn definitions(rule: Rule) -> WorkflowDefinitions {
    let ledger: Arc<dyn ErasedWorkflow> =
        Arc::new(TypedWorkflowAdapter::new(LedgerWorkflow::under(rule), ()));
    WorkflowDefinitions::new().with(ledger)
}

/// A turn asking to open a document, with `open` already there.
///
/// `opens` is how many the one turn asks for, because a turn may mint twice and
/// the second mint is beside the first.
async fn open_beside(rule: Rule, open: &[TripState], opens: usize) -> PlannedTurn {
    open_beside_reachable_only(rule, open, opens, false).await
}

/// The same, saying whether the cases already open belong to another thread of
/// work.
async fn open_beside_reachable_only(
    rule: Rule,
    open: &[TripState],
    opens: usize,
    reachable_only: bool,
) -> PlannedTurn {
    let workflow = LEDGER;
    let text = "Apri un viaggio per Mario Ferri";
    let mut builder = UnderstandingBuilder::of(text);
    for _ in 0..opens {
        builder = builder.open(operations::OPEN, workflow, serde_json::Value::Null, text);
    }
    let understanding = builder.build().unwrap();
    let planner = SeededTurnPlanner::builder()
        .definitions(definitions(rule))
        .understander(Arc::new(ScriptedUnderstanding::new().then(understanding)))
        .config(OrchestratorConfig::conservative())
        .clock(Arc::new(FixedTurnClock(now())))
        .build()
        .unwrap();

    let cases = open
        .iter()
        .enumerate()
        .map(|(index, state)| {
            let seeded = SeededCase::new(
                CaseRef::new(workflow, format!("open-{index}"), CaseRevision(3)),
                serde_json::to_value(state).unwrap(),
            )
            .with_label(format!("Trip {index}"));
            if reachable_only {
                seeded.reachable_only()
            } else {
                seeded
            }
        })
        .collect();
    planner
        .plan(
            TurnInput {
                turn_id: turn_one(),
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

/// The rejection an act ended in, or a panic naming what it ended in instead.
fn rejection(planned: &PlannedTurn, act: usize) -> &DomainRejection {
    match &planned.reduction.acts[act].result {
        PlannedActResult::Rejected { rejection } => rejection,
        other => panic!("expected acts[{act}] to be refused, got {other:?}"),
    }
}

/// A second document, beside one that is unfinished.
#[tokio::test]
async fn opening_a_case_beside_an_unfinished_one_is_refused() {
    let planned = open_beside(Rule::WhileUnfinished, &[incomplete_case()], 1).await;
    assert_eq!(
        rejection(&planned, 0).code.as_str(),
        UNFINISHED,
        "the refusal is the domain's own, not one of the runtime's"
    );
    assert!(
        planned.reduction.has_no_effects(),
        "and nothing was written for it"
    );
}

/// The other half of the same declaration: the open case has every obligation
/// met, so a second one is an ordinary thing to want.
#[tokio::test]
async fn opening_a_case_beside_a_finished_one_is_allowed() {
    let planned = open_beside(Rule::WhileUnfinished, &[complete_case()], 1).await;
    assert!(
        matches!(
            planned.reduction.acts[0].result,
            PlannedActResult::ReadyToExecute { .. }
        ),
        "a finished case is not something a new one has to wait for: {:?}",
        planned.reduction.acts[0].result
    );
}

/// With nothing open, this door does what it always did.
#[tokio::test]
async fn opening_the_first_case_still_mints() {
    let planned = open_beside(Rule::WhileUnfinished, &[], 1).await;
    assert!(
        matches!(
            planned.reduction.acts[0].result,
            PlannedActResult::ReadyToExecute { .. }
        ),
        "there was nothing to be beside: {:?}",
        planned.reduction.acts[0].result
    );
}

/// A workflow that declares nothing admits everything, unfinished case and all.
///
/// Asserted of the sample itself, which does not implement the method, so what
/// answers is the trait's own default.
#[test]
fn a_workflow_that_declares_nothing_opens_beside_anything() {
    let sample = TripWorkflow::new().with_cards();
    let unfinished = incomplete_case();
    let view = sample.project(
        CaseRef::new("trip", "trip-1", CaseRevision(3)),
        Some(&unfinished),
    );
    assert!(
        !view.obligations.is_empty(),
        "the fixture has to be unfinished for this to prove anything"
    );
    assert!(
        sample.may_open_beside(&[view]).is_ok(),
        "the default admits everything, which is what every workflow did before"
    );
}

/// Twice in one turn: the first mint has no case to be beside and lands; the
/// second is beside the case the same turn just opened, so the workflow refuses it.
#[tokio::test]
async fn a_turn_that_opens_twice_keeps_the_first_and_refuses_the_second() {
    let planned = open_beside(Rule::OneAtATime, &[], 2).await;
    assert!(
        matches!(
            planned.reduction.acts[0].result,
            PlannedActResult::ReadyToExecute { .. }
        ),
        "the first had nothing to be beside: {:?}",
        planned.reduction.acts[0].result
    );
    assert_eq!(
        rejection(&planned, 1).code.as_str(),
        UNFINISHED,
        "and the second was beside the first"
    );
}

/// The sentence the user reads is the domain's: a refusal with an explanation
/// reaches the user under
/// [`notice::ACT_REFUSED`](turnframe_runtime::reduce::notice::ACT_REFUSED) carrying
/// those words, which is why the hook returns a rejection and not a boolean.
#[tokio::test]
async fn the_refusal_carries_the_domain_s_own_words() {
    let planned = open_beside(Rule::WhileUnfinished, &[incomplete_case()], 1).await;
    let notice = planned
        .reduction
        .notices
        .iter()
        .find(|notice| notice.code == turnframe_runtime::reduce::notice::ACT_REFUSED)
        .expect("a refused act leaves a notice");
    assert_eq!(
        notice.text.resolve(&Locale::from("en-GB")),
        "That trip is still unfinished: shall we go back to it?"
    );
    let fact = planned
        .reduction
        .refusals
        .iter()
        .find_map(|fact| match fact {
            turnframe_core::response::NarratableFact::ActRefused {
                code, explanation, ..
            } if code == UNFINISHED => Some(explanation.clone()),
            _ => None,
        })
        .expect("and a fact the writing stage can rest on");
    assert!(
        fact.contains("still unfinished"),
        "so the prose cannot contradict the notice: {fact}"
    );
}

/// An unfinished case of another thread of work does not hold the door.
///
/// A case the turn has only because the actor may reach it is not part of the
/// work the domain's rule is about, so the domain is not asked about it.
#[tokio::test]
async fn a_case_of_another_thread_does_not_hold_the_door() {
    let planned =
        open_beside_reachable_only(Rule::WhileUnfinished, &[incomplete_case()], 1, true).await;
    assert!(
        matches!(
            planned.reduction.acts[0].result,
            PlannedActResult::ReadyToExecute { .. }
        ),
        "an unfinished draft of another thread is not what the new one waits for: {:?}",
        planned.reduction.acts[0].result
    );
}

/// And the same draft, present in this work, still holds it: the declaration is
/// the only thing that differs from the test above.
#[tokio::test]
async fn the_same_case_in_this_thread_still_holds_the_door() {
    let planned =
        open_beside_reachable_only(Rule::WhileUnfinished, &[incomplete_case()], 1, false).await;
    assert_eq!(
        rejection(&planned, 0).code.as_str(),
        UNFINISHED,
        "a draft of this conversation is exactly what the domain refuses to double"
    );
}

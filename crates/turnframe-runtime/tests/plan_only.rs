//! The plan-only path: the same decisions, none of the effects.
//!
//! These are the acceptance tests the first adopter asked for, plus the ones
//! the guarantee needs to keep being true. The two that matter most are the
//! ones a wrong stopping point makes fail: a workflow whose executor panics if
//! anything writes through it, and a turn whose plan contains a card it *would*
//! have persisted, with the interaction store proven untouched afterwards.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{Harness, now, token_for};
use turnframe_core::case::{CaseRef, Versioned};
use turnframe_core::flow::{
    ErasedWorkflow, TypedWorkflowAdapter, WorkflowDefinitions, WorkflowExecutor,
};
use turnframe_core::ids::{CaseRevision, TurnId};
use turnframe_core::interaction::InteractionKind;
use turnframe_core::locale::Locale;
use turnframe_core::reduce::PlannedActResult;
use turnframe_core::response::ClaimClass;
use turnframe_core::turn::{ActorContext, TurnInput};
use turnframe_core::understanding::{ActTarget, Understanding};
use turnframe_runtime::config::OrchestratorConfig;
use turnframe_runtime::orchestrator::FixedTurnClock;
use turnframe_runtime::planning::{SeededCase, SeededTurnPlanner};
use turnframe_test::providers::{ScriptedUnderstanding, UnderstandingBuilder};
use turnframe_test::workflows::trip::{TripWorkflow, complete_case, incomplete_case, operations};

fn turn_one() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(1))
}

/// The store methods a planning run is allowed to have called.
const READS: [&str; 4] = [
    "conversations.load_conversation",
    "conversations.load_recent_turns",
    "interactions.list_open_for_conversation",
    "interactions.get",
];

/// Fails with the offending method when a planning run called anything else.
fn assert_only_reads(harness: &Harness) {
    for (method, count) in harness.stores.calls() {
        assert!(
            READS.contains(&method),
            "planning called {method} {count} time(s); the plan-only path must not touch anything else"
        );
    }
}

/// A one-act understanding that sets a name on the named trip.
fn set_subject(turn_id: TurnId, case_id: &str, text: &str) -> Understanding {
    UnderstandingBuilder::of(text)
        .apply(
            operations::SET_NAME,
            token_for(turn_id, "trip", case_id),
            serde_json::json!({"value": "Lisbon"}),
            text,
        )
        .build()
        .unwrap()
}

/// The acceptance test as the adopter wrote it: the executor panics on any
/// write, and the planning path runs to completion anyway.
#[tokio::test]
async fn planning_never_reaches_the_executor_even_when_the_plan_would_execute() {
    let turn_id = turn_one();
    let text = "Set the name to Lisbon";
    let harness = Harness::builder()
        .trip("trip-1", "Ferri", 3, incomplete_case())
        .trip_panics_on_write()
        .understands(set_subject(turn_id, "trip-1", text))
        .build()
        .await;
    harness.stores.reset_calls();

    let planned = harness
        .orchestrator
        .plan_turn(harness.turn(turn_id, text))
        .await
        .unwrap();

    assert_eq!(
        planned.would_execute.len(),
        1,
        "the plan really does contain a command; a plan that did nothing would prove nothing"
    );
    assert!(
        planned
            .reduction
            .acts
            .iter()
            .all(|act| matches!(act.result, PlannedActResult::ReadyToExecute { .. })),
        "and the command was ready to run"
    );
    assert!(
        harness.events("trip", "trip-1").await.is_empty(),
        "nothing was executed"
    );
    assert_eq!(
        harness.trip_revision("trip-1"),
        CaseRevision(3),
        "and the case did not move"
    );
    assert_only_reads(&harness);
}

/// The second acceptance test: a plan that contains a card it would have
/// persisted, and an interaction store that never heard about it.
#[tokio::test]
async fn a_card_the_plan_would_persist_is_not_persisted() {
    let turn_id = turn_one();
    let text = "Set the name on the Ferri trip to Lisbon";
    // Two records answer to the same name, so the turn has to ask which one.
    let understanding = UnderstandingBuilder::of(text)
        .apply_to(
            operations::SET_NAME,
            ActTarget::Ambiguous {
                candidates: vec![
                    token_for(turn_id, "trip", "trip-1"),
                    token_for(turn_id, "trip", "trip-2"),
                ],
            },
            serde_json::json!({"value": "Lisbon"}),
            text,
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Ferri", 3, incomplete_case())
        .trip("trip-2", "Ferri", 1, incomplete_case())
        .trip_panics_on_write()
        .understands(understanding)
        .build()
        .await;
    harness.stores.reset_calls();

    let planned = harness
        .orchestrator
        .plan_turn(harness.turn(turn_id, text))
        .await
        .unwrap();

    assert_eq!(
        planned.would_persist.len(),
        1,
        "the plan asks the user which trip was meant"
    );
    assert_eq!(planned.would_persist[0].kind, InteractionKind::SelectTarget);
    assert!(
        planned.would_execute.is_empty(),
        "and an ambiguous target mutates nothing (I8)"
    );

    // The card the plan describes does not exist anywhere.
    assert!(harness.open_cards("trip", "trip-1").await.is_empty());
    assert!(harness.open_cards("trip", "trip-2").await.is_empty());
    assert_eq!(harness.stores.call_count("interactions.insert"), 0);
    assert_eq!(
        harness
            .stores
            .call_count("interactions.insert_replacing_blocking"),
        0
    );
    assert_eq!(
        harness.stores.call_count("interactions.begin_resolution"),
        0
    );
    assert_eq!(harness.stores.call_count("commit.commit"), 0);
    assert_eq!(harness.stores.call_count("replay.put"), 0);
    assert_eq!(harness.stores.call_count("journal.begin"), 0);
    assert_only_reads(&harness);
}

/// Planning writes no conversation block either, which is the other half of
/// "the old path stays authoritative": a shadow turn must not appear in the
/// transcript the user or the next turn reads.
#[tokio::test]
async fn planning_leaves_the_conversation_as_it_found_it() {
    let turn_id = turn_one();
    let text = "Set the name to Lisbon";
    let harness = Harness::builder()
        .trip("trip-1", "Ferri", 3, incomplete_case())
        .trip_panics_on_write()
        .understands(set_subject(turn_id, "trip-1", text))
        .build()
        .await;
    harness.stores.reset_calls();

    harness
        .orchestrator
        .plan_turn(harness.turn(turn_id, text))
        .await
        .unwrap();

    assert_eq!(
        harness.stores.call_count("conversations.append_user_turn"),
        0
    );
    assert_eq!(
        harness
            .stores
            .call_count("conversations.append_assistant_turn"),
        0
    );
    assert_eq!(harness.stores.call_count("conversations.set_turn_phase"), 0);
    assert_only_reads(&harness);
}

/// A command that policy will not run without a confirmation is reported as a
/// card the turn would persist, not as a command it would execute — which is
/// the distinction a shadow report is built on.
#[tokio::test]
async fn a_command_awaiting_confirmation_is_a_card_and_not_an_execution() {
    let turn_id = turn_one();
    let text = "Cancel that trip";
    let understanding = UnderstandingBuilder::of(text)
        .apply(
            operations::WITHDRAW,
            token_for(turn_id, "trip", "trip-1"),
            serde_json::json!(null),
            text,
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, complete_case())
        .trip_panics_on_write()
        .understands(understanding)
        .build()
        .await;
    harness.stores.reset_calls();

    let planned = harness
        .orchestrator
        .plan_turn(harness.turn(turn_id, text))
        .await
        .unwrap();

    assert_eq!(planned.would_persist.len(), 1);
    assert_eq!(
        planned.would_persist[0].kind,
        InteractionKind::ConfirmCommand
    );
    assert!(planned.would_execute.is_empty());
    assert!(
        planned
            .would_claim
            .contains(&ClaimClass::InteractionVisibility),
        "the answer could mention the card it would have written"
    );
    assert!(
        !planned.would_claim.contains(&ClaimClass::Deletion),
        "and could not say the trip was cancelled, because nothing would run"
    );
    assert_eq!(
        harness.stores.call_count("journal.begin"),
        0,
        "and nothing was journaled next to the card"
    );
    assert_only_reads(&harness);
}

/// A planned turn reports the projection it planned against, so a divergence
/// report can say which state the two paths actually saw.
#[tokio::test]
async fn a_planned_turn_reports_the_views_it_planned_against() {
    let turn_id = turn_one();
    let text = "Set the name to Lisbon";
    let harness = Harness::builder()
        .trip("trip-1", "Ferri", 3, incomplete_case())
        .trip_panics_on_write()
        .understands(set_subject(turn_id, "trip-1", text))
        .build()
        .await;

    let planned = harness
        .orchestrator
        .plan_turn(harness.turn(turn_id, text))
        .await
        .unwrap();

    assert_eq!(planned.views.len(), 1);
    assert_eq!(planned.views[0].case_ref.case_id.as_str(), "trip-1");
    assert_eq!(planned.views[0].case_ref.expected_revision, CaseRevision(3));
    assert!(
        planned.understanding.is_some(),
        "the turn carried text, so it was understood"
    );
    assert_eq!(planned.target_resolutions.len(), 1);
}

// ---------------------------------------------------------------------------
// Planning from a state handed in
// ---------------------------------------------------------------------------

/// A workflow definition with no executor behind it at all.
///
/// `TypedWorkflowAdapter` only needs its executor parameter to be `Send + Sync`
/// in order to be an [`ErasedWorkflow`], so `()` is a legal one. That is the
/// whole point of the seeded path: the pure half of a workflow needs nothing
/// that could write.
fn definitions_without_an_executor() -> WorkflowDefinitions {
    let adapter: Arc<dyn ErasedWorkflow> = Arc::new(TypedWorkflowAdapter::new(
        TripWorkflow::new().with_cards(),
        (),
    ));
    WorkflowDefinitions::new().with(adapter)
}

/// The requirement in one test: a planner built with no store and no executor,
/// planning a real turn from the state it is handed.
#[tokio::test]
async fn a_seeded_planner_is_built_without_a_store_or_an_executor() {
    let turn_id = turn_one();
    let text = "Set the name to Lisbon";
    let understander = ScriptedUnderstanding::new().then(set_subject(turn_id, "trip-1", text));

    // Nothing below names a store, a commit, an executor or a loader.
    let planner = SeededTurnPlanner::builder()
        .definitions(definitions_without_an_executor())
        .understander(Arc::new(understander))
        .config(OrchestratorConfig::conservative())
        .clock(Arc::new(FixedTurnClock(now())))
        .build()
        .unwrap();

    let case_ref = CaseRef::new("trip", "trip-1", CaseRevision(3));
    let state = serde_json::to_value(incomplete_case()).unwrap();
    let planned = planner
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
            vec![SeededCase::new(case_ref, state).with_label("Ferri")],
        )
        .await
        .unwrap();

    assert_eq!(planned.views.len(), 1);
    assert_eq!(planned.views[0].case_ref.expected_revision, CaseRevision(3));
    assert_eq!(
        planned.would_execute.len(),
        1,
        "the seeded state was planned against exactly like a loaded one"
    );
}

/// The seeded path consults the loader for nothing, so a planner taken from a
/// real orchestrator touches neither the store nor the executor.
#[tokio::test]
async fn a_seeded_plan_reads_nothing_at_all() {
    let turn_id = turn_one();
    let text = "Set the name to Lisbon";
    let harness = Harness::builder()
        .trip("trip-1", "Ferri", 3, incomplete_case())
        .trip_panics_on_write()
        .understands(set_subject(turn_id, "trip-1", text))
        .build()
        .await;
    harness.stores.reset_calls();

    // The seeded state deliberately disagrees with the store's: revision 9,
    // not 3. If anything loaded, the assertion below would see revision 3.
    let planned = harness
        .orchestrator
        .plan_turn_from(
            harness.turn(turn_id, text),
            vec![
                SeededCase::new(
                    CaseRef::new("trip", "trip-1", CaseRevision(9)),
                    serde_json::to_value(incomplete_case()).unwrap(),
                )
                .with_label("Ferri"),
            ],
        )
        .await
        .unwrap();

    assert_eq!(
        planned.views[0].case_ref.expected_revision,
        CaseRevision(9),
        "the state that was handed in is the state that was planned against"
    );
    assert_eq!(
        harness.stores.total_calls(),
        0,
        "and no store was consulted at all: {:?}",
        harness.stores.calls()
    );
}

/// The same corpus item planned twice gives the same plan, which is what makes
/// a replayed shadow corpus a comparison of projectors rather than of states.
#[tokio::test]
async fn planning_the_same_seeded_turn_twice_gives_the_same_plan() {
    let turn_id = turn_one();
    let text = "Set the name to Lisbon";
    let harness = Harness::builder()
        .trip("trip-1", "Ferri", 3, incomplete_case())
        .trip_panics_on_write()
        // Two understandings, because the corpus item is planned twice and the
        // planner shares the orchestrator's understander.
        .understands(set_subject(turn_id, "trip-1", text))
        .understands(set_subject(turn_id, "trip-1", text))
        .build()
        .await;
    let seed = || {
        vec![
            SeededCase::new(
                CaseRef::new("trip", "trip-1", CaseRevision(3)),
                serde_json::to_value(incomplete_case()).unwrap(),
            )
            .with_label("Ferri"),
        ]
    };

    let first = harness
        .orchestrator
        .plan_turn_from(harness.turn(turn_id, text), seed())
        .await
        .unwrap();
    let second = harness
        .orchestrator
        .plan_turn_from(harness.turn(turn_id, text), seed())
        .await
        .unwrap();

    assert_eq!(
        first.reduction.plan_hash, second.reduction.plan_hash,
        "the same state and the same message reduce to the same plan"
    );
    assert_eq!(first.summary(), second.summary());
}

/// A workflow the seeded planner does not know is refused, rather than planned
/// against nothing.
#[tokio::test]
async fn a_seeded_case_of_an_unregistered_workflow_is_refused() {
    let planner = SeededTurnPlanner::builder()
        .definitions(definitions_without_an_executor())
        .understander(Arc::new(ScriptedUnderstanding::new()))
        .config(OrchestratorConfig::conservative())
        .clock(Arc::new(FixedTurnClock(now())))
        .build()
        .unwrap();

    let outcome = planner
        .plan(
            TurnInput {
                turn_id: turn_one(),
                conversation_id: turnframe_core::ids::ConversationId::nil(),
                actor: ActorContext::new(support::account(), "u1"),
                text: Some("anything".to_owned()),
                interaction_response: None,
                attachments: Vec::new(),
                origin: None,
                locale: Locale::from("en-GB"),
                effort: None,
            },
            vec![SeededCase::absent(CaseRef::new(
                "unknown-workflow",
                "c1",
                CaseRevision::ZERO,
            ))],
        )
        .await;

    assert!(
        outcome.is_err(),
        "an unknown workflow is not planned against"
    );
}

/// A type-level reminder: the loading half of an executor is free, so an
/// adopter writes what they write today and gets the read-only view anyway.
#[test]
fn every_executor_is_a_case_loader() {
    fn read_only<W, E>(executor: E) -> impl turnframe_core::flow::CaseLoader<W>
    where
        W: turnframe_core::flow::WorkflowDefinition,
        E: WorkflowExecutor<W>,
    {
        executor
    }
    let loader = read_only::<TripWorkflow, _>(turnframe_test::workflows::InMemoryExecutor::new(
        TripWorkflow::new().with_cards(),
    ));
    // Naming it is the assertion; there is nothing to call here that writes.
    let _: &dyn turnframe_core::flow::CaseLoader<TripWorkflow> = &loader;
}

/// `Versioned` is what a loader answers with; this keeps the import honest and
/// documents the shape a seeded state stands in for.
#[test]
fn a_seeded_case_stands_in_for_what_a_loader_would_have_returned() {
    let loaded: Versioned<Option<serde_json::Value>> = Versioned::new(
        Some(serde_json::to_value(incomplete_case()).unwrap()),
        CaseRevision(3),
    );
    let seeded = SeededCase::new(
        CaseRef::new("trip", "trip-1", loaded.revision),
        loaded.value.clone().unwrap(),
    );
    assert_eq!(seeded.state, loaded.value);
    assert_eq!(seeded.case_ref.expected_revision, CaseRevision(3));
}

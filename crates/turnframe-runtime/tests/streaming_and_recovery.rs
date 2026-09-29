//! Safe streaming (spec §18.5) and the four crash-recovery decisions (§23.1).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use futures::StreamExt;
use support::{Harness, narrating, token_for};
use turnframe_core::case::CaseKey;
use turnframe_core::ids::TurnId;
use turnframe_core::replay::TurnPhase;
use turnframe_core::response::ResponseBlock;
use turnframe_core::understanding::Understanding;
use turnframe_runtime::orchestrator::ResumeOutcome;
use turnframe_runtime::recover::RecoveryAction;
use turnframe_runtime::stream::{RecordingSink, TurnEvent, TurnStream};
use turnframe_test::providers::{ScriptedProvider, UnderstandingBuilder};
use turnframe_test::stores::FailurePoint;
use turnframe_test::workflows::trip::{incomplete_case, operations, with_offer};

fn turn(n: u128) -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(n))
}

const SUBJECT_TEXT: &str = "Set the name to Lisbon";

fn subject(turn_id: TurnId) -> Understanding {
    UnderstandingBuilder::of(SUBJECT_TEXT)
        .apply(
            operations::SET_NAME,
            token_for(turn_id, "trip", "trip-1"),
            serde_json::json!({"value": "Lisbon"}),
            SUBJECT_TEXT,
        )
        .build()
        .unwrap()
}

/// Whether a block states an outcome, and so may not be published before the
/// turn commits.
fn states_an_outcome(block: &ResponseBlock) -> bool {
    matches!(
        block,
        ResponseBlock::Receipt(_) | ResponseBlock::Answer(_) | ResponseBlock::Transition(_)
    )
}

#[tokio::test]
async fn nothing_that_states_an_outcome_is_streamed_before_the_commit() {
    let turn_id = turn(1);
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(subject(turn_id))
        .provider(narrating().build_shared())
        .build()
        .await;

    let stream = Arc::clone(&harness.orchestrator).stream_turn(harness.turn(turn_id, SUBJECT_TEXT));
    let events: Vec<TurnEvent> = stream.collect().await;

    let committed_at = events
        .iter()
        .position(|event| matches!(event, TurnEvent::Phase(TurnPhase::Committed)))
        .expect("the turn reached the committed phase");
    for (index, event) in events.iter().enumerate() {
        if let Some(block) = event.block()
            && states_an_outcome(block)
        {
            assert!(
                index > committed_at,
                "block {index} states an outcome and went out before the commit at {committed_at}"
            );
        }
    }
    assert!(
        events.iter().any(|event| event
            .block()
            .is_some_and(|block| { matches!(block, ResponseBlock::Receipt(_)) })),
        "the receipt did go out, once it was true"
    );
    assert!(matches!(events.last(), Some(TurnEvent::Completed(_))));
}

/// The acknowledgement arrives whole, as one block, and only once the turn has
/// committed: it is reviewed before it is shown, so there is nothing to preview.
#[tokio::test]
async fn the_acknowledgement_arrives_whole_after_the_commit() {
    let turn_id = turn(1);
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .acknowledging("Right, here is where that leaves things.")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(subject(turn_id))
        .provider(Arc::clone(&provider))
        .build()
        .await;

    let sink = Arc::new(RecordingSink::new());
    harness
        .orchestrator
        .handle_turn_streaming(harness.turn(turn_id, SUBJECT_TEXT), sink.clone())
        .await
        .unwrap();

    let events = sink.events();
    let committed_at = events
        .iter()
        .position(|event| matches!(event, TurnEvent::Phase(TurnPhase::Committed)))
        .expect("the turn reached the committed phase");
    let written_at = events
        .iter()
        .position(|event| {
            event
                .block()
                .is_some_and(|block| matches!(block, ResponseBlock::Transition(_)))
        })
        .expect("the acknowledgement went out as a block");
    assert!(
        written_at > committed_at,
        "{written_at} after {committed_at}"
    );
    assert!(
        events
            .iter()
            .any(|event| event.block().is_some_and(|block| matches!(
                block,
                ResponseBlock::Transition(transition)
                    if transition.text == "Right, here is where that leaves things."
            ))),
        "and it is the whole reply"
    );
}

#[tokio::test]
async fn a_click_only_turn_streams_a_finished_answer_immediately() {
    let first = turn(1);
    let review_text = "It is ready, show me the rebooking card";
    let review = UnderstandingBuilder::of(review_text)
        .apply(
            operations::REQUEST_REBOOKING,
            token_for(first, "trip", "trip-1"),
            serde_json::json!({"leg": 1}),
            "show me the rebooking card",
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, with_offer(1))
        .understands(review)
        .without_narration()
        .build()
        .await;
    harness
        .handle(harness.turn(first, review_text))
        .await
        .unwrap();
    let card = harness.blocking_card("trip", "trip-1").await;
    let revision = harness.trip_revision("trip-1").value();

    let answer = harness
        .handle(harness.click(
            turn(2),
            card.id,
            turnframe_test::workflows::trip::REBOOK_CONFIRM_OPTION,
            revision,
        ))
        .await
        .unwrap();

    // The answer already exists, so the stream is a finished one.
    let events: Vec<TurnEvent> = TurnStream::immediate(answer.clone()).collect().await;
    assert!(matches!(
        events.first(),
        Some(TurnEvent::Phase(TurnPhase::Delivered))
    ));
    assert!(matches!(events.last(), Some(TurnEvent::Completed(_))));
    assert_eq!(events.len(), answer.blocks.len() + 2);
}

#[tokio::test]
async fn a_recording_sink_sees_the_phases_in_order() {
    let turn_id = turn(1);
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(subject(turn_id))
        .provider(narrating().build_shared())
        .build()
        .await;
    let sink = Arc::new(RecordingSink::new());

    harness
        .orchestrator
        .handle_turn_streaming(harness.turn(turn_id, SUBJECT_TEXT), sink.clone())
        .await
        .unwrap();

    let phases: Vec<TurnPhase> = sink
        .events()
        .into_iter()
        .filter_map(|event| match event {
            TurnEvent::Phase(phase) => Some(phase),
            _ => None,
        })
        .collect();
    assert_eq!(
        phases,
        vec![
            TurnPhase::Received,
            TurnPhase::Interpreted,
            TurnPhase::Reduced,
            TurnPhase::Executing,
            TurnPhase::Committed,
            TurnPhase::Composed,
            TurnPhase::Delivered,
        ],
        "the phases of §23 in the order §23 states them"
    );
}

#[tokio::test]
async fn a_failed_turn_ends_its_stream_with_a_stable_code() {
    let turn_id = turn(1);
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(subject(turn_id))
        .build()
        .await;
    // The command cannot be admitted, so the turn fails before it commits.
    harness.fail_at(
        FailurePoint::BeforeJournalInsert,
        turnframe_core::error::StoreError::Unavailable,
    );
    let sink = Arc::new(RecordingSink::new());

    let failed = harness
        .orchestrator
        .handle_turn_streaming(harness.turn(turn_id, SUBJECT_TEXT), sink.clone())
        .await;
    assert!(failed.is_err());

    match sink.events().last() {
        Some(TurnEvent::Failed { code }) => {
            assert!(!code.is_empty());
            assert!(!code.contains(SUBJECT_TEXT), "no user text on the wire");
        }
        other => panic!("expected a failure event, got {other:?}"),
    }
    assert!(
        sink.blocks().is_empty(),
        "a turn that never committed publishes nothing"
    );
}

// ---------------------------------------------------------------------------
// Crash recovery: the four decisions of §23.1.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_turn_that_never_admitted_a_command_starts_over() {
    let turn_id = turn(1);
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .build()
        .await;
    // The turn is accepted and the process dies before anything else: the
    // marker is `Received` and the journal is empty.
    harness
        .stores
        .stores()
        .conversations()
        .append_user_turn(turnframe_store::conversation::StoredUserTurn::new(
            harness.turn(turn_id, SUBJECT_TEXT),
            support::now(),
        ))
        .await
        .unwrap();

    assert_eq!(harness.phase(turn_id).await, TurnPhase::Received);
    assert_eq!(
        harness
            .orchestrator
            .plan_recovery(&harness.account(), &turn_id)
            .await
            .unwrap(),
        RecoveryAction::RestartInterpretation,
    );
    assert!(matches!(
        harness
            .orchestrator
            .resume_turn(&harness.account(), &turn_id)
            .await
            .unwrap(),
        ResumeOutcome::Restartable
    ));
    assert!(
        harness.events("trip", "trip-1").await.is_empty(),
        "deciding to restart executes nothing"
    );
}

#[tokio::test]
async fn an_unknown_external_outcome_is_reconciled_and_never_retried() {
    let turn_id = turn(1);
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .trip_fails(turnframe_core::error::ExecutionError::Timeout)
        .understands(subject(turn_id))
        .provider(narrating().build_shared())
        .build()
        .await;

    harness
        .handle(harness.turn(turn_id, SUBJECT_TEXT))
        .await
        .unwrap();

    let action = harness
        .orchestrator
        .plan_recovery(&harness.account(), &turn_id)
        .await
        .unwrap();
    match &action {
        RecoveryAction::ReconcileExternal { attempts, entries } => {
            assert_eq!(attempts.len(), 1);
            assert_eq!(entries.len(), 1);
        }
        other => panic!("expected a reconciliation, got {other:?}"),
    }
    assert!(
        !action.may_cause_effects(),
        "acting on this decision must not touch the domain (I15)"
    );

    let outcome = harness
        .orchestrator
        .resume_turn(&harness.account(), &turn_id)
        .await
        .unwrap();
    assert!(matches!(
        outcome,
        ResumeOutcome::AwaitingReconciliation { .. }
    ));
    assert!(
        harness.events("trip", "trip-1").await.is_empty(),
        "nothing was retried"
    );
}

#[tokio::test]
async fn a_committed_turn_regenerates_its_answer_without_executing_anything() {
    let turn_id = turn(1);
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .acknowledging("First try.")
        .acknowledging("Second try.")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(subject(turn_id))
        .provider(provider)
        .build()
        .await;
    harness.fail_at(
        FailurePoint::BeforeResponsePersistence,
        turnframe_core::error::StoreError::Unavailable,
    );

    assert!(
        harness
            .handle(harness.turn(turn_id, SUBJECT_TEXT))
            .await
            .is_err()
    );
    let revision = harness.trip_revision("trip-1");

    let outcome = harness
        .orchestrator
        .resume_turn(&harness.account(), &turn_id)
        .await
        .unwrap();
    let ResumeOutcome::Regenerated { turn } = outcome else {
        panic!("expected the answer to be rebuilt, got {outcome:?}");
    };
    assert_eq!(support::receipt_codes(&turn), vec!["trip.name_set"]);
    assert_eq!(
        harness.trip_revision("trip-1"),
        revision,
        "regenerating executes nothing"
    );
    assert_eq!(harness.events("trip", "trip-1").await.len(), 1);
    assert_eq!(
        harness
            .stored_turn(turn_id)
            .await
            .map(|stored| stored.blocks.len()),
        Some(turn.blocks.len()),
    );
}

#[tokio::test]
async fn a_finished_turn_needs_no_recovery() {
    let turn_id = turn(1);
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(subject(turn_id))
        .provider(narrating().build_shared())
        .build()
        .await;
    harness
        .handle(harness.turn(turn_id, SUBJECT_TEXT))
        .await
        .unwrap();

    assert_eq!(
        harness
            .orchestrator
            .plan_recovery(&harness.account(), &turn_id)
            .await
            .unwrap(),
        RecoveryAction::Nothing {
            phase: TurnPhase::Delivered
        },
    );
    assert!(
        harness
            .stores
            .stores()
            .conversations()
            .list_unfinished_turns(
                turnframe_store::conversation::RecoveryScope::AllAccounts,
                10
            )
            .await
            .unwrap()
            .is_empty(),
        "and a sweep does not even find it"
    );
}

/// Regeneration reads the ledger, and a stored event may have been redacted
/// since the turn committed.
///
/// Rendering a redacted event as though it were intact fails twice over: the
/// receipt would state what was erased, and the erased payload reads back as
/// JSON null, which a domain's event type cannot deserialize. Either way the
/// turn a user is waiting for is lost to an erasure that was granted lawfully.
/// The regeneration path therefore groups the ledger through the store's
/// receipt grouping, which carries an erasure through to the domain.
#[tokio::test]
async fn a_committed_turn_regenerates_over_an_erased_event() {
    use turnframe_core::ids::RedactionAuthority;
    use turnframe_store::events::EventJournalWriter;

    let turn_id = turn(1);
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .acknowledging("First try.")
        .acknowledging("Second try.")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(subject(turn_id))
        .provider(provider)
        .build()
        .await;
    harness.fail_at(
        FailurePoint::BeforeResponsePersistence,
        turnframe_core::error::StoreError::Unavailable,
    );

    assert!(
        harness
            .handle(harness.turn(turn_id, SUBJECT_TEXT))
            .await
            .is_err(),
        "the turn commits and then loses its answer"
    );

    // The name the user set is personal data, and it is erased before the
    // answer is ever rebuilt.
    let stored = harness
        .stores
        .events_for(&harness.account(), &CaseKey::new("trip", "trip-1"))
        .await
        .expect("the store answers");
    let event_id = stored
        .first()
        .expect("the turn committed one event")
        .event_id;
    let redaction = harness
        .stores
        .memory()
        .redact_payload(
            &harness.account(),
            &event_id,
            &RedactionAuthority::from("erasure-request-1"),
        )
        .await
        .expect("the payload is erased in place");

    let revision = harness.trip_revision("trip-1");
    let outcome = harness
        .orchestrator
        .resume_turn(&harness.account(), &turn_id)
        .await
        .expect("an erased payload does not cost the user their answer");
    let ResumeOutcome::Regenerated { turn } = outcome else {
        panic!("expected the answer to be rebuilt, got {outcome:?}");
    };

    assert!(
        !turn.blocks.is_empty(),
        "the turn still has an answer to show"
    );
    assert_eq!(
        harness.trip_revision("trip-1"),
        revision,
        "regenerating executes nothing"
    );
    assert_eq!(
        harness.events("trip", "trip-1").await.len(),
        1,
        "the erasure emptied a payload and removed no event"
    );
    assert_eq!(
        redaction.authority,
        RedactionAuthority::from("erasure-request-1"),
        "the ledger records who erased it and never what was erased"
    );
}

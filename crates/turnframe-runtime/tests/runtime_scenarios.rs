//! The runtime scenarios of spec §27.4, one named test each.
//!
//! Every test here is a turn a person could plausibly take, and an assertion
//! about what the specification says must happen to it. They run the real
//! pipeline against the sample domains, the in-memory stores, scripted
//! understandings and a narration provider that fails loudly on a call nobody
//! asked for.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{
    FixedKnowledge, Harness, block_kinds, narrating, narration, notice_codes, receipt_codes,
    silent, token_for, transient_failure,
};
use turnframe_core::command::{AtomicityScope, CommandOrigin, ResolutionChannel};
use turnframe_core::error::{DomainRejection, ExecutionError, OrchestratorError};
use turnframe_core::ids::{CaseRevision, InteractionId, OptionId, TurnId};
use turnframe_core::interaction::{
    ActionClass, Interaction, InteractionKind, InteractionPayload, InteractionRejection,
    InteractionStatus,
};
use turnframe_core::plan::AnswerBasis;
use turnframe_core::response::{AnswerStatus, ResponseBlock};
use turnframe_core::understanding::{ActStatus, ActTarget, NotUnderstoodReason, UnitId, UnitKind};
use turnframe_provider::purpose::ModelPurpose;
use turnframe_test::providers::{ScriptedProvider, ScriptedReply, UnderstandingBuilder};
use turnframe_test::workflows::trip::{
    REBOOK_CONFIRM_OPTION, incomplete_case, operations, sample_travel_date, with_offer,
};

fn turn_one() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(1))
}

fn turn_two() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(2))
}

fn turn_three() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(3))
}

/// The answer blocks of a turn.
fn answers(
    turn: &turnframe_core::response::AssistantTurn,
) -> Vec<&turnframe_core::response::GeneratedAnswer> {
    turn.blocks
        .iter()
        .filter_map(|block| match block {
            ResponseBlock::Answer(answer) => Some(answer),
            _ => None,
        })
        .collect()
}

/// The interaction views of a turn.
fn cards(
    turn: &turnframe_core::response::AssistantTurn,
) -> Vec<&turnframe_core::interaction::InteractionView> {
    turn.interactions().collect()
}

/// `text` understood as setting the name of one of two trips that both fit.
fn either_rossi(turn_id: TurnId, text: &str) -> turnframe_core::understanding::Understanding {
    UnderstandingBuilder::of(text)
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
        .unwrap()
}

// ---------------------------------------------------------------------------
// 1. "Change X - actually leave it unchanged" causes no mutation.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_correction_that_leaves_the_value_unchanged_causes_no_mutation() {
    let turn_id = turn_one();
    let text = "Change the name to Lisbon, actually leave it alone";
    let token = token_for(turn_id, "trip", "trip-1");
    let mut withdrawn = UnderstandingBuilder::of(text)
        .apply(
            operations::SET_NAME,
            token,
            serde_json::json!({"value": "Lisbon"}),
            "Change the name to Lisbon",
        )
        .superseded_by_next()
        .chitchat("actually leave it alone")
        .build()
        .unwrap();
    // The builder has no verb for a withdrawal, so the superseding unit is made one.
    if let Some(unit) = withdrawn.units.last_mut() {
        unit.kind = UnitKind::Cancel;
    }
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(withdrawn)
        .provider(narrating().build_shared())
        .build()
        .await;

    let turn = harness.handle(harness.turn(turn_id, text)).await.unwrap();

    assert!(
        harness.events("trip", "trip-1").await.is_empty(),
        "a correction inside the same turn must commit nothing (I10)"
    );
    assert_eq!(harness.trip_revision("trip-1"), CaseRevision(3));
    assert!(harness.journal(turn_id).await.is_empty());
    assert!(receipt_codes(&turn).is_empty());
}

// ---------------------------------------------------------------------------
// 2. One message sets several independent fields atomically.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn one_message_sets_several_independent_fields_atomically() {
    let turn_id = turn_one();
    let text = "Name is Lisbon and it flies on 2026-10-31";
    let token = token_for(turn_id, "trip", "trip-1");
    let both = UnderstandingBuilder::of(text)
        .apply(
            operations::SET_NAME,
            token.clone(),
            serde_json::json!({"value": "Lisbon"}),
            "Name is Lisbon",
        )
        .apply(
            operations::SET_TRAVEL_DATE,
            token,
            serde_json::json!({"value": sample_travel_date()}),
            "flies on 2026-10-31",
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(both)
        .provider(narrating().build_shared())
        .build()
        .await;

    let turn = harness.handle(harness.turn(turn_id, text)).await.unwrap();

    assert_eq!(
        harness.events("trip", "trip-1").await,
        vec!["trip.name_set", "trip.travel_date_set"],
    );
    assert_eq!(
        harness.trip_revision("trip-1"),
        CaseRevision(4),
        "both fields commit under one revision: they share a per-case batch (§13.4)"
    );
    assert_eq!(receipt_codes(&turn).len(), 2);
    let entries = harness.journal(turn_id).await;
    assert_eq!(entries.len(), 2);
    assert!(
        entries
            .iter()
            .all(|entry| entry.status == turnframe_store::journal::CommandJournalStatus::Committed),
    );
}

// ---------------------------------------------------------------------------
// 3. One message requests an action and asks a question; both get results.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_action_and_a_question_both_receive_results() {
    let turn_id = turn_one();
    let text = "Set the name to Lisbon and tell me when it flies";
    let token = token_for(turn_id, "trip", "trip-1");
    let act_and_ask = UnderstandingBuilder::of(text)
        .apply(
            operations::SET_NAME,
            token,
            serde_json::json!({"value": "Lisbon"}),
            "Set the name to Lisbon",
        )
        .ask("tell me when it flies")
        .build()
        .unwrap();
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .answering("It has no travel date yet.")
        .acknowledging("Right, here is where that leaves things.")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(act_and_ask)
        .provider(Arc::clone(&provider))
        .build()
        .await;

    let turn = harness.handle(harness.turn(turn_id, text)).await.unwrap();

    assert_eq!(
        receipt_codes(&turn),
        vec!["trip.name_set"],
        "the action produced a receipt from its own event"
    );
    let answered = answers(&turn);
    assert_eq!(answered.len(), 1, "the question did not disappear (§19.4)");
    assert_eq!(answered[0].status, AnswerStatus::Answered);
    assert_eq!(answered[0].text, "It has no travel date yet.");
    provider.verify().expect("the script was followed exactly");
}

// ---------------------------------------------------------------------------
// 4. A CTA response and text coexist in one turn.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_card_response_and_text_coexist_in_one_turn() {
    let first = turn_one();
    let review_text = "It is ready, show me the rebooking card";
    let token = token_for(first, "trip", "trip-1");
    let review = UnderstandingBuilder::of(review_text)
        .apply(
            operations::REQUEST_REBOOKING,
            token,
            serde_json::json!({"leg": 1}),
            "show me the rebooking card",
        )
        .build()
        .unwrap();

    let second = turn_two();
    let click_text = "Yes rebook it, and remind me what the name is";
    let ask = UnderstandingBuilder::of(click_text)
        .ask("remind me what the name is")
        .build()
        .unwrap();

    let provider = narrating()
        .answering("The name is Lisbon, September.")
        .acknowledging("Understood, here is where that stands.")
        .build_shared();

    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, with_offer(1))
        .understands(review)
        .understands(ask)
        .provider(provider)
        .build()
        .await;

    harness
        .handle(harness.turn(first, review_text))
        .await
        .unwrap();
    let card = harness.blocking_card("trip", "trip-1").await;
    let revision = harness.trip_revision("trip-1");

    let turn = harness
        .handle(harness.click_and_say(
            second,
            card.id,
            REBOOK_CONFIRM_OPTION,
            revision.value(),
            click_text,
        ))
        .await
        .unwrap();

    assert!(
        harness
            .events("trip", "trip-1")
            .await
            .contains(&"trip.rebooking_sent".to_owned()),
        "the click executed what the stored option authorized"
    );
    let answered = answers(&turn);
    assert_eq!(
        answered.len(),
        1,
        "the text asked a question and the click executed a command; both got results"
    );
    assert_eq!(answered[0].status, AnswerStatus::Answered);
    assert!(receipt_codes(&turn).contains(&"trip.rebooking_sent".to_owned()));
}

// ---------------------------------------------------------------------------
// 5. Two same-kind open cases create a selection interaction, not a guess.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn two_open_cases_of_the_same_kind_produce_a_selection_card() {
    let turn_id = turn_one();
    let text = "Set the name on the Ferri trip to Lisbon";
    let harness = Harness::builder()
        .trip("trip-1", "Ferri", 3, incomplete_case())
        .trip("trip-2", "Ferri", 1, incomplete_case())
        .understands(either_rossi(turn_id, text))
        .provider(narrating().build_shared())
        .build()
        .await;

    let turn = harness.handle(harness.turn(turn_id, text)).await.unwrap();

    assert!(
        harness.events("trip", "trip-1").await.is_empty()
            && harness.events("trip", "trip-2").await.is_empty(),
        "an ambiguous target mutates nothing (I8)"
    );
    let offered = cards(&turn);
    assert_eq!(offered.len(), 1, "the user is asked, not guessed at");
    assert_eq!(offered[0].kind, InteractionKind::SelectTarget);
    assert!(
        offered[0].options.len() >= 3,
        "both candidates plus a way out"
    );
}

// ---------------------------------------------------------------------------
// 6. A malformed second act causes zero acts on its record to execute (I18).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_malformed_second_act_causes_zero_acts_to_execute() {
    let turn_id = turn_one();
    let text = "Set the name to Lisbon and the travel date too";
    // The first act is impeccable; the second unit's extraction stayed malformed after
    // its repair, so it was not understood and the act on the same record is held.
    let held = UnderstandingBuilder::of(text)
        .apply(
            operations::SET_NAME,
            token_for(turn_id, "trip", "trip-1"),
            serde_json::json!({"value": "Lisbon"}),
            "Set the name to Lisbon",
        )
        .with_status(ActStatus::Held { because: UnitId(2) })
        .not_understood(
            NotUnderstoodReason::TaskFailed {
                task: "extract".to_owned(),
                code: "schema_mismatch".to_owned(),
            },
            "and the travel date too",
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(held)
        .provider(narrating().build_shared())
        .build()
        .await;

    let turn = harness.handle(harness.turn(turn_id, text)).await.unwrap();

    assert!(
        harness.events("trip", "trip-1").await.is_empty(),
        "the well-formed act must not execute on its own (I18)"
    );
    assert!(harness.journal(turn_id).await.is_empty());
    assert!(receipt_codes(&turn).is_empty());
    assert_eq!(
        harness.replay(turn_id).await.act_outcomes,
        vec!["held".to_owned()],
        "the act is held, not dropped"
    );
}

// ---------------------------------------------------------------------------
// 7. A stale confirmation after a case edit is rejected.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_stale_confirmation_after_an_edit_is_rejected() {
    let first = turn_one();
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

    let second = turn_two();
    let edit_text = "Actually change the name to Retainer";
    let edit = UnderstandingBuilder::of(edit_text)
        .apply(
            operations::SET_NAME,
            token_for(second, "trip", "trip-1"),
            serde_json::json!({"value": "Retainer"}),
            "change the name to Retainer",
        )
        .build()
        .unwrap();

    let provider = narrating().acknowledging("Changed.").build_shared();

    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, with_offer(1))
        .understands(review)
        .understands(edit)
        .provider(provider)
        .build()
        .await;

    harness
        .handle(harness.turn(first, review_text))
        .await
        .unwrap();
    let card = harness.blocking_card("trip", "trip-1").await;
    let bound = harness.trip_revision("trip-1");

    harness
        .handle(harness.turn(second, edit_text))
        .await
        .unwrap();
    assert!(
        harness.trip_revision("trip-1") > bound,
        "the edit moved the case past the revision the card was rendered against"
    );

    let error = harness
        .handle(harness.click(turn_three(), card.id, REBOOK_CONFIRM_OPTION, bound.value()))
        .await
        .expect_err("a confirmation of a state that has moved is not a confirmation");

    match error {
        OrchestratorError::Interaction(turnframe_core::error::InteractionError::Rejected(
            rejection,
        )) => assert!(
            matches!(
                rejection,
                InteractionRejection::Stale { .. } | InteractionRejection::NotActive { .. }
            ),
            "expected the card to be stale or already invalidated, got {rejection:?}"
        ),
        other => panic!("expected an interaction rejection, got {other:?}"),
    }
    assert!(
        !harness
            .events("trip", "trip-1")
            .await
            .contains(&"trip.rebooking_sent".to_owned()),
        "nothing was rebooked"
    );
}

// ---------------------------------------------------------------------------
// 8. A double click executes at most once.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_double_click_executes_at_most_once() {
    let first = turn_one();
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
    let provider = narrating()
        .acknowledging("Sent for review.")
        .acknowledging("Already done.")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, with_offer(1))
        .understands(review)
        .provider(provider)
        .build()
        .await;

    harness
        .handle(harness.turn(first, review_text))
        .await
        .unwrap();
    let card = harness.blocking_card("trip", "trip-1").await;
    let revision = harness.trip_revision("trip-1").value();

    harness
        .handle(harness.click(turn_two(), card.id, REBOOK_CONFIRM_OPTION, revision))
        .await
        .unwrap();
    let second = harness
        .handle(harness.click(turn_three(), card.id, REBOOK_CONFIRM_OPTION, revision))
        .await;

    let submitted = harness
        .events("trip", "trip-1")
        .await
        .into_iter()
        .filter(|event| event == "trip.rebooking_sent")
        .count();
    assert_eq!(submitted, 1, "the second click executed nothing (I14)");
    assert!(
        second.is_ok() || second.is_err(),
        "the second click is answered either way, but it never commits twice"
    );
}

// ---------------------------------------------------------------------------
// 9. A traveler cannot change a CTA's meaning by changing a value field.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_client_cannot_change_a_call_to_action_meaning_by_changing_a_value() {
    let first = turn_one();
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
    let provider = narrating().acknowledging("Ready.").build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, with_offer(1))
        .understands(review)
        .provider(provider)
        .build()
        .await;

    harness
        .handle(harness.turn(first, review_text))
        .await
        .unwrap();
    let card = harness.blocking_card("trip", "trip-1").await;
    let revision = harness.trip_revision("trip-1").value();

    // The traveler sends the confirm option and tries to smuggle a different
    // operation in the only free-form field the protocol has (I7).
    let mut tampered = harness.click(turn_two(), card.id, REBOOK_CONFIRM_OPTION, revision);
    if let Some(response) = tampered.interaction_response.as_mut() {
        response.freeform_input = Some(operations::WITHDRAW.to_owned());
    }
    let error = harness
        .handle(tampered)
        .await
        .expect_err("a value the stored option forbids is refused outright");
    assert!(matches!(
        error,
        OrchestratorError::Interaction(turnframe_core::error::InteractionError::Rejected(
            InteractionRejection::FreeformNotAllowed
        ))
    ));

    let events = harness.events("trip", "trip-1").await;
    assert!(!events.contains(&"trip.withdrawn".to_owned()));
    assert!(!events.contains(&"trip.rebooking_sent".to_owned()));

    // The same option without the smuggled value does exactly what the server
    // stored against it, and nothing else.
    harness
        .handle(harness.click(turn_three(), card.id, REBOOK_CONFIRM_OPTION, revision))
        .await
        .unwrap();
    let events = harness.events("trip", "trip-1").await;
    assert!(events.contains(&"trip.rebooking_sent".to_owned()));
    assert!(!events.contains(&"trip.withdrawn".to_owned()));
}

// ---------------------------------------------------------------------------
// 10. A failed command cannot produce a resolved-looking receipt.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_failed_command_cannot_produce_a_resolved_looking_receipt() {
    let turn_id = turn_one();
    let text = "Set the name to Lisbon";
    let subject = UnderstandingBuilder::of(text)
        .apply(
            operations::SET_NAME,
            token_for(turn_id, "trip", "trip-1"),
            serde_json::json!({"value": "Lisbon"}),
            "Set the name to Lisbon",
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .trip_fails(ExecutionError::Rejected(DomainRejection::new(
            "trip.locked",
            "trip.error.locked",
        )))
        .understands(subject)
        .provider(narrating().build_shared())
        .build()
        .await;

    let turn = harness.handle(harness.turn(turn_id, text)).await.unwrap();

    assert!(
        receipt_codes(&turn).is_empty(),
        "a command that did not commit produces no event, so it produces no receipt (I16)"
    );
    assert!(harness.events("trip", "trip-1").await.is_empty());
    assert!(
        notice_codes(&turn)
            .contains(&turnframe_runtime::compose::notice::COMMAND_FAILED.to_owned()),
        "the failure is stated by the server, not glossed over"
    );
    let entries = harness.journal(turn_id).await;
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].status,
        turnframe_store::journal::CommandJournalStatus::Failed
    );
}

// ---------------------------------------------------------------------------
// 11. Failed interaction persistence cannot produce text referring to a card.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn failed_interaction_persistence_cannot_produce_text_referring_to_a_visible_card() {
    let turn_id = turn_one();
    let text = "Set the name on the Ferri trip to Lisbon";
    let provider = narrating().build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Ferri", 3, incomplete_case())
        .trip("trip-2", "Ferri", 1, incomplete_case())
        .understands(either_rossi(turn_id, text))
        .provider(Arc::clone(&provider))
        .build()
        .await;
    harness.fail_at(
        turnframe_test::stores::FailurePoint::AfterInteractionPersistence,
        turnframe_core::error::StoreError::Unavailable,
    );

    let turn = harness.handle(harness.turn(turn_id, text)).await.unwrap();

    assert!(
        cards(&turn).is_empty(),
        "the card was not written, so the turn does not carry one"
    );
    assert!(
        !narration(&turn).to_lowercase().contains("card below"),
        "and nothing in the turn refers to it: {:?}",
        narration(&turn)
    );
    // Prose is not inspected, so the guarantee is what the narrator is handed.
    for call in provider.calls_for(ModelPurpose::Acknowledge) {
        assert!(
            !call.prompt_mentions("Which one did you mean?"),
            "the narrator is told of no card, so it cannot claim one is visible"
        );
    }
    assert!(
        notice_codes(&turn)
            .contains(&turnframe_runtime::compose::notice::INTERACTION_UNAVAILABLE.to_owned()),
        "the server says the confirmation could not be prepared"
    );
}

// ---------------------------------------------------------------------------
// 12. A provider fallback before commit is safe. Here understanding is
//     scripted, so no model runs before the commit to fall back from.
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// 13. A provider failure after commit regenerates narration without repeating
//     commands.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_provider_failure_after_commit_regenerates_narration_without_repeating_commands() {
    let turn_id = turn_one();
    let text = "Set the name to Lisbon";
    let subject = UnderstandingBuilder::of(text)
        .apply(
            operations::SET_NAME,
            token_for(turn_id, "trip", "trip-1"),
            serde_json::json!({"value": "Lisbon"}),
            "Set the name to Lisbon",
        )
        .build()
        .unwrap();
    // The first provider falls over on narration, after the commit, and stays down:
    // each narration call exhausts its attempts there before moving on.
    let mut first = ScriptedProvider::builder("first", "model-a");
    for _ in 0..6 {
        first = first.failing(transient_failure());
    }
    let first = first.build_shared();
    let second = ScriptedProvider::builder("second", "model-b")
        .acknowledging("The name now reads Lisbon.")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(subject)
        .provider(Arc::clone(&first))
        .provider(Arc::clone(&second))
        .build()
        .await;

    let turn = harness.handle(harness.turn(turn_id, text)).await.unwrap();

    assert_eq!(
        harness.events("trip", "trip-1").await,
        vec!["trip.name_set"],
        "the command ran once; narration is not a reason to run it again (I17)"
    );
    assert_eq!(harness.journal(turn_id).await.len(), 1);
    assert!(
        narration(&turn).contains("Lisbon"),
        "another model wrote the sentence: {:?}",
        narration(&turn)
    );
    assert_eq!(second.calls_for(ModelPurpose::Acknowledge).len(), 1);
}

// ---------------------------------------------------------------------------
// 14. A database timeout during an atomic batch produces no hidden partial
//     state.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_database_timeout_during_an_atomic_batch_leaves_no_hidden_partial_state() {
    let turn_id = turn_one();
    let text = "Set the name to Lisbon";
    let subject = UnderstandingBuilder::of(text)
        .apply(
            operations::SET_NAME,
            token_for(turn_id, "trip", "trip-1"),
            serde_json::json!({"value": "Lisbon"}),
            "Set the name to Lisbon",
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(subject)
        .provider(narrating().build_shared())
        .build()
        .await;
    // The bundle is all-or-nothing, so a failure part way through it must leave
    // none of it visible — not the events, not the journal completion.
    harness.fail_at(
        turnframe_test::stores::FailurePoint::AfterCommitBeforeEventReadback,
        turnframe_core::error::StoreError::Timeout,
    );

    let error = harness
        .handle(harness.turn(turn_id, text))
        .await
        .expect_err("the commit did not confirm");
    assert!(matches!(error, OrchestratorError::Store(_)));

    assert!(
        harness.events("trip", "trip-1").await.is_empty(),
        "no event of the aborted bundle is readable"
    );
    let entries = harness.journal(turn_id).await;
    assert_eq!(entries.len(), 1);
    assert!(
        entries[0].status.is_pending(),
        "the journal entry stays resumable by idempotency key (§23.1), found {:?}",
        entries[0].status
    );
    assert!(harness.stored_turn(turn_id).await.is_none());
}

// ---------------------------------------------------------------------------
// 15. An external timeout becomes an unknown outcome, not a blind retry.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn an_external_timeout_becomes_an_unknown_outcome_rather_than_a_blind_retry() {
    let turn_id = turn_one();
    let text = "Set the name to Lisbon";
    let subject = UnderstandingBuilder::of(text)
        .apply(
            operations::SET_NAME,
            token_for(turn_id, "trip", "trip-1"),
            serde_json::json!({"value": "Lisbon"}),
            "Set the name to Lisbon",
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .trip_fails(ExecutionError::Timeout)
        .understands(subject)
        .provider(narrating().build_shared())
        .build()
        .await;

    let turn = harness.handle(harness.turn(turn_id, text)).await.unwrap();

    let entries = harness.journal(turn_id).await;
    assert_eq!(entries.len(), 1, "the command was admitted exactly once");
    assert_eq!(
        entries[0].status,
        turnframe_store::journal::CommandJournalStatus::OutcomeUnknown,
        "a timeout after transmission is uncertainty, not failure (I15)"
    );
    assert!(matches!(
        entries[0].result,
        Some(turnframe_store::journal::JournalOutcome::OutcomeUnknown { .. })
    ));
    assert!(receipt_codes(&turn).is_empty());
    assert!(
        notice_codes(&turn)
            .contains(&turnframe_runtime::compose::notice::VERIFICATION_IN_PROGRESS.to_owned()),
        "the user is told it is being verified, not that it worked"
    );
    let record = harness
        .stores
        .replay_record(&harness.account(), &turn_id)
        .await
        .unwrap();
    assert_eq!(
        record.pending_reconciliations().len(),
        1,
        "the attempt is named so a reconciler can settle it"
    );
}

// ---------------------------------------------------------------------------
// 16. A read-only question during a workflow does not lose the active case.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_read_only_question_during_a_workflow_does_not_lose_the_active_case() {
    let first = turn_one();
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

    let second = turn_two();
    let question_text = "Before I confirm, what is the total?";
    let question = UnderstandingBuilder::of(question_text)
        .ask("what is the total?")
        .build()
        .unwrap();

    // The first turn shows a card, which is material, and is narrated. The second
    // asks a question and carries nothing of its own, so no acknowledgement runs.
    let provider = narrating()
        .answering("Twenty-five thousand cents.")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, with_offer(1))
        .understands(review)
        .understands(question)
        .provider(provider)
        .build()
        .await;

    harness
        .handle(harness.turn(first, review_text))
        .await
        .unwrap();
    let card = harness.blocking_card("trip", "trip-1").await;
    let revision = harness.trip_revision("trip-1");

    let turn = harness
        .handle(harness.turn(second, question_text))
        .await
        .unwrap();

    assert_eq!(answers(&turn).len(), 1);
    assert_eq!(answers(&turn)[0].status, AnswerStatus::Answered);
    assert_eq!(
        harness.trip_revision("trip-1"),
        revision,
        "a question changes nothing"
    );
    let still_open = harness.blocking_card("trip", "trip-1").await;
    assert_eq!(
        still_open.id, card.id,
        "the case is still waiting on the same card"
    );
    assert_eq!(still_open.status, InteractionStatus::Active);
}

// ---------------------------------------------------------------------------
// 17. A question unsupported by current sources stays explicit.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_question_unsupported_by_current_sources_stays_explicit() {
    let turn_id = turn_one();
    let text = "Why does a rebooking need a loyalty number?";
    let question = UnderstandingBuilder::of(text)
        .ask_about(AnswerBasis::GeneralDomainKnowledge, None, &[], text)
        .build()
        .unwrap();
    let provider = narrating().build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .knowledge(Arc::new(FixedKnowledge::unavailable()))
        .understands(question)
        .provider(Arc::clone(&provider))
        .build()
        .await;

    let turn = harness.handle(harness.turn(turn_id, text)).await.unwrap();

    let answered = answers(&turn);
    assert_eq!(answered.len(), 1, "the question still gets a block (§19.4)");
    assert_eq!(
        answered[0].status,
        AnswerStatus::SourceUnavailable,
        "the answer says the sources were not there, rather than inventing one"
    );
    assert!(!answered[0].text.trim().is_empty());
    assert!(
        provider.calls_for(ModelPurpose::Answer).is_empty(),
        "no model was asked to fill the gap"
    );
}

// ---------------------------------------------------------------------------
// 18. Reload returns the same ordered blocks and interaction state.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_reload_returns_the_same_ordered_blocks_and_interaction_state() {
    let turn_id = turn_one();
    let text = "It is ready, show me the rebooking card";
    let review = UnderstandingBuilder::of(text)
        .apply(
            operations::REQUEST_REBOOKING,
            token_for(turn_id, "trip", "trip-1"),
            serde_json::json!({"leg": 1}),
            "show me the rebooking card",
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, with_offer(1))
        .understands(review)
        .provider(narrating().build_shared())
        .build()
        .await;

    let returned = harness.handle(harness.turn(turn_id, text)).await.unwrap();
    let reloaded = harness
        .stored_turn(turn_id)
        .await
        .expect("the assistant turn was persisted");

    assert_eq!(
        reloaded, returned,
        "a reload is the same turn, not a re-rendering of it (§22.3)"
    );
    assert_eq!(block_kinds(&reloaded), block_kinds(&returned));
    let card = harness.blocking_card("trip", "trip-1").await;
    assert_eq!(card.status, InteractionStatus::Active);
    assert_eq!(
        cards(&reloaded).first().map(|view| view.id),
        Some(card.id),
        "and the card in the reloaded turn is the card that is still open"
    );
}

// ---------------------------------------------------------------------------
// 19. An identifier naming nothing, or another tenant's record, leaks nothing.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn identifiers_that_name_nothing_or_another_tenants_record_leak_nothing() {
    let turn_id = turn_one();
    let text = "Set the name to Lisbon";
    // A record the user named that matches none of the caller's own.
    let unlisted = UnderstandingBuilder::of(text)
        .apply_to(
            operations::SET_NAME,
            ActTarget::NotListed {
                workflow: "trip".into(),
                words: None,
            },
            serde_json::json!({"value": "Lisbon"}),
            "Set the name to Lisbon",
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(unlisted)
        .provider(narrating().build_shared())
        .build()
        .await;

    let turn = harness.handle(harness.turn(turn_id, text)).await.unwrap();
    assert!(
        harness.events("trip", "trip-1").await.is_empty(),
        "a record named in words that name nothing reaches no case"
    );
    assert!(receipt_codes(&turn).is_empty());

    // A card that belongs to another tenant, and one that never existed, give
    // the same answer.
    let other = turnframe_core::ids::AccountId::from(support::OTHER_ACCOUNT);
    let stranger = Interaction::from_spec(
        turnframe_core::interaction::InteractionSpec::new(
            "foreign",
            turnframe_core::case::CaseRef::new("trip", "trip-9", CaseRevision(1)),
            InteractionKind::SingleSelect,
            InteractionPayload::new("Pick one").with_option(
                turnframe_core::interaction::InteractionOption::new(
                    OptionId::from("a"),
                    "A",
                    turnframe_core::interaction::StoredInteractionAction::Dismiss,
                ),
            ),
        ),
        InteractionId::from(uuid::Uuid::from_u128(77)),
        other,
        harness.conversation,
        TurnId::nil(),
        support::now(),
    )
    .unwrap();
    harness
        .stores
        .stores()
        .interactions()
        .insert(stranger.clone())
        .await
        .unwrap();

    let foreign_click = harness
        .handle(harness.click(turn_two(), stranger.id, "a", 1))
        .await
        .expect_err("another tenant's card is not answerable");
    let unknown_click = harness
        .handle(harness.click(
            turn_three(),
            InteractionId::from(uuid::Uuid::from_u128(999)),
            "a",
            1,
        ))
        .await
        .expect_err("a card nobody ever created is not answerable");

    assert_eq!(
        format!("{foreign_click:?}"),
        format!("{unknown_click:?}"),
        "the two must be indistinguishable, or the error is an oracle (§25.4)"
    );
}

// ---------------------------------------------------------------------------
// 20. A claim nothing backs leaves no trace in the record, and the review keeps it
//     out of the reply: a receipt is rendered from committed events and nothing else.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_claim_nothing_backs_leaves_no_trace_in_the_record() {
    let turn_id = turn_one();
    let text = "Set the name to Lisbon";
    let subject = UnderstandingBuilder::of(text)
        .apply(
            operations::SET_NAME,
            token_for(turn_id, "trip", "trip-1"),
            serde_json::json!({"value": "Lisbon"}),
            "Set the name to Lisbon",
        )
        .build()
        .unwrap();
    // The model claims an outcome that did not happen, and again when asked to fix it.
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .reply_to(
            ModelPurpose::Acknowledge,
            ScriptedReply::written("Done, the trip was updated and sent."),
        )
        .reply_to(ModelPurpose::Review, ScriptedReply::review_fails())
        .reply_to(
            ModelPurpose::Acknowledge,
            ScriptedReply::written("All sent, as you asked."),
        )
        .reply_to(ModelPurpose::Review, ScriptedReply::review_fails())
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .trip_fails(ExecutionError::Rejected(DomainRejection::new(
            "trip.locked",
            "trip.error.locked",
        )))
        .understands(subject)
        .provider(provider)
        .build()
        .await;

    let turn = harness.handle(harness.turn(turn_id, text)).await.unwrap();

    assert!(
        harness.events("trip", "trip-1").await.is_empty(),
        "nothing committed"
    );
    assert!(receipt_codes(&turn).is_empty(), "so nothing is receipted");
    // The model DID claim it, twice, and the review caught it twice: the claim is
    // dropped, and what stands in for it is the question code wrote.
    assert!(
        !narration(&turn).to_lowercase().contains("sent"),
        "a claim the review caught never reaches the user: {:?}",
        narration(&turn)
    );
    assert!(
        notice_codes(&turn)
            .contains(&turnframe_runtime::compose::notice::COMMAND_FAILED.to_owned()),
        "and the turn says out loud that the command did not go through"
    );
}

// ---------------------------------------------------------------------------
// The click that costs nothing (spec §9), which several scenarios above rest on.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_click_only_turn_reaches_execution_with_no_model_call() {
    let first = turn_one();
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
    let provider = silent().build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, with_offer(1))
        .understands(review)
        .provider(Arc::clone(&provider))
        .without_narration()
        .build()
        .await;

    harness
        .handle(harness.turn(first, review_text))
        .await
        .unwrap();
    let card = harness.blocking_card("trip", "trip-1").await;
    let revision = harness.trip_revision("trip-1").value();
    let understood = harness.understander.seen().len();

    harness
        .handle(harness.click(turn_two(), card.id, REBOOK_CONFIRM_OPTION, revision))
        .await
        .unwrap();

    assert_eq!(
        harness.understander.seen().len(),
        understood,
        "a click carries its own meaning; nothing is asked what it meant (§9)"
    );
    assert_eq!(
        provider.call_count(),
        0,
        "and with narration off, no model runs at all"
    );
    assert!(
        harness
            .events("trip", "trip-1")
            .await
            .contains(&"trip.rebooking_sent".to_owned()),
        "and it still executed what the stored option authorized"
    );
}

/// The origin a confirmed click mints, for the record: it names the card, what
/// the option authorizes and how the answer arrived (I12, I20).
#[test]
fn a_confirmed_click_carries_its_own_authority() {
    let origin = CommandOrigin::ConfirmedInteraction {
        interaction_id: InteractionId::nil(),
        payload_hash: turnframe_core::hash::Digest::of_bytes(b"payload"),
        interaction_kind: InteractionKind::ConfirmCommand,
        action_class: ActionClass::AppliesOperation,
        channel: ResolutionChannel::Click,
    };
    assert!(origin.is_trusted());
    let send = turnframe_core::command::CommandPolicy {
        risk: turnframe_core::command::RiskClass::ExternalRegulated,
        confirmation: turnframe_core::command::ConfirmationPolicy::ExplicitClick,
        atomicity: AtomicityScope::ExternalSaga {
            saga: "trip.rebooking".to_owned(),
        },
        claim_mode: turnframe_core::command::ClaimMode::ServerReceiptOnly,
    };
    assert!(turnframe_core::command::origin_satisfies(&origin, &send));
}

/// A provider that answers nothing at all is never called for a click-only
/// turn, which is what makes the assertion above meaningful.
#[test]
fn the_silent_provider_has_no_script() {
    assert_eq!(silent().build().remaining_steps(), 0);
}

/// A consequential act is confirmed before it runs, and the click executes the
/// very commands that were reviewed — not whatever the domain would compile a
/// day later (spec §14.3, §15.3).
#[tokio::test]
async fn a_consequential_act_waits_for_the_card_its_policy_names() {
    let first = turn_one();
    let text = "Cancel that trip";
    let cancel = UnderstandingBuilder::of(text)
        .apply(
            operations::WITHDRAW,
            token_for(first, "trip", "trip-1"),
            serde_json::json!(null),
            "Cancel that trip",
        )
        .build()
        .unwrap();
    let provider = narrating()
        .acknowledging("I need you to confirm that.")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, with_offer(1))
        .understands(cancel)
        .provider(provider)
        .build()
        .await;

    let asked = harness.handle(harness.turn(first, text)).await.unwrap();

    // Nothing happened: a destructive command needs an explicit click (I12).
    assert!(harness.events("trip", "trip-1").await.is_empty());
    assert!(receipt_codes(&asked).is_empty());
    let offered = cards(&asked);
    assert_eq!(offered.len(), 1);
    assert_eq!(offered[0].kind, InteractionKind::ConfirmCommand);

    // The commands were journaled with the card, awaiting its confirmation, so
    // answering it runs exactly them and nothing else ever resumes them.
    let pending = harness.journal(first).await;
    assert_eq!(pending.len(), 1);
    assert_eq!(
        pending[0].status,
        turnframe_store::journal::CommandJournalStatus::AwaitingConfirmation
    );
    assert_eq!(pending[0].command_type, "trip.withdraw");

    let revision = harness.trip_revision("trip-1").value();
    let done = harness
        .handle(harness.click(
            turn_two(),
            offered[0].id,
            turnframe_runtime::policy::CONFIRM_OPTION_ID,
            revision,
        ))
        .await
        .unwrap();

    assert_eq!(
        harness.events("trip", "trip-1").await,
        vec!["trip.withdrawn"],
        "the click executed the reviewed command, once"
    );
    assert_eq!(receipt_codes(&done), vec!["trip.withdrawn"]);
    let settled = harness.journal(first).await;
    assert_eq!(
        settled[0].status,
        turnframe_store::journal::CommandJournalStatus::Committed,
        "and the entry it was journaled under is the entry that settled"
    );
    assert_eq!(
        harness
            .stores
            .interaction(&harness.account(), &offered[0].id)
            .await
            .unwrap()
            .status(),
        InteractionStatus::Resolved,
        "the card is resolved only because the command committed"
    );
}

/// An origin reference names the record the surface had open, so the model does
/// not have to rediscover it from prose (spec §12.4).
#[tokio::test]
async fn an_origin_reference_names_exactly_the_record_the_surface_had_open() {
    let turn_id = turn_one();
    let text = "Set the name to Lisbon";
    // Two identically labelled trips would be ambiguous without the origin,
    // and the origin is what makes one of them the answer.
    let subject = UnderstandingBuilder::of(text)
        .apply(
            operations::SET_NAME,
            token_for(turn_id, "trip", "trip-2"),
            serde_json::json!({"value": "Lisbon"}),
            "Set the name to Lisbon",
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Ferri", 3, incomplete_case())
        .trip("trip-2", "Ferri", 1, incomplete_case())
        .origin("origin-abc", "trip", "trip-2")
        .understands(subject)
        .provider(narrating().build_shared())
        .build()
        .await;

    harness
        .handle(harness.turn_from(turn_id, text, "origin-abc"))
        .await
        .unwrap();

    assert_eq!(
        harness.events("trip", "trip-2").await,
        vec!["trip.name_set"],
        "the record the surface named is the record that changed"
    );
    assert!(
        harness.events("trip", "trip-1").await.is_empty(),
        "and the one it did not name is untouched"
    );
}

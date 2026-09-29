//! An answered card continues the work it interrupted (spec §13.3, §15.3).
//!
//! A selection card (the ambiguous target of §12.3) and a condition card (the
//! conditional instruction of §10.4) carry the act they stopped as a
//! `DeferredAct`, so answering the card runs it without the user restating it.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{Harness, narrating, notice_codes, receipt_codes, token_for};
use turnframe_core::ids::TurnId;
use turnframe_core::interaction::{Interaction, InteractionKind, InteractionStatus};
use turnframe_core::understanding::{ActTarget, ConstraintKind, Understanding};
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::{incomplete_case, operations};

fn turn(n: u128) -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(n))
}

/// The one open card of a case, whether or not it blocks.
async fn card(harness: &Harness, case_id: &str) -> Interaction {
    let open = harness.open_cards("trip", case_id).await;
    assert_eq!(open.len(), 1, "exactly one card is waiting on {case_id}");
    open.into_iter().next().expect("the card is there")
}

// ---------------------------------------------------------------------------
// An ambiguous target: the user picks, and the change lands on what they picked.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn picking_a_case_applies_the_change_that_was_waiting_without_restating_it() {
    let first = turn(1);
    let text = "Set the name on the Ferri trip to Lisbon";
    let understanding = UnderstandingBuilder::of(text)
        .apply_to(
            operations::SET_NAME,
            ActTarget::Ambiguous {
                candidates: vec![
                    token_for(first, "trip", "trip-1"),
                    token_for(first, "trip", "trip-2"),
                ],
            },
            serde_json::json!({"value": "Lisbon"}),
            text,
        )
        .build()
        .unwrap();
    let provider = narrating()
        // The second turn is a click: no understanding, only the wording.
        .acknowledging("Right, that one it is.")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Ferri", 3, incomplete_case())
        .trip("trip-2", "Ferri", 1, incomplete_case())
        .understands(understanding)
        .provider(Arc::clone(&provider))
        .build()
        .await;

    harness.handle(harness.turn(first, text)).await.unwrap();
    let selection = card(&harness, "trip-1").await;
    assert_eq!(selection.kind, InteractionKind::SelectTarget);

    // The user picks the second trip, and says nothing else at all.
    let chosen = token_for(first, "trip", "trip-2");
    let second = turn(2);
    let click = harness.click(second, selection.id, chosen.as_str(), 1);
    assert!(click.text.is_none(), "the request is not restated");
    let answer = harness.handle(click).await.unwrap();

    assert_eq!(
        harness.events("trip", "trip-2").await,
        vec!["trip.name_set"],
        "the change the card was guarding landed on the case the user picked"
    );
    assert!(
        harness.events("trip", "trip-1").await.is_empty(),
        "and on no other case (I8)"
    );
    assert_eq!(
        receipt_codes(&answer),
        vec!["trip.name_set"],
        "the answer says so from the event that committed"
    );
    let settled = harness
        .stores
        .interaction(&harness.account(), &selection.id)
        .await
        .expect("the card is still there");
    assert_eq!(settled.status(), InteractionStatus::Resolved);
    provider.verify().expect("the script was followed exactly");
}

// ---------------------------------------------------------------------------
// A conditional instruction: yes runs it, no records that it was declined.
// ---------------------------------------------------------------------------

const CONDITIONAL: &str = "Set the name to Lisbon if the trip is still a draft";

fn conditional(turn_id: TurnId) -> Understanding {
    UnderstandingBuilder::of(CONDITIONAL)
        .apply(
            operations::SET_NAME,
            token_for(turn_id, "trip", "trip-1"),
            serde_json::json!({"value": "Lisbon"}),
            "Set the name to Lisbon",
        )
        .constrain(ConstraintKind::ApplyOnlyIf, "if the trip is still a draft")
        .build()
        .unwrap()
}

async fn asked_the_condition(narration: &str) -> (Harness, Interaction) {
    let first = turn(1);
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(conditional(first))
        .provider(narrating().acknowledging(narration).build_shared())
        .build()
        .await;
    harness
        .handle(harness.turn(first, CONDITIONAL))
        .await
        .unwrap();
    assert!(
        harness.events("trip", "trip-1").await.is_empty(),
        "a condition nobody evaluated changes nothing yet (§10.4)"
    );
    let asked = card(&harness, "trip-1").await;
    assert_eq!(asked.kind, InteractionKind::Boolean);
    (harness, asked)
}

#[tokio::test]
async fn confirming_a_conditional_instruction_executes_the_act_it_was_guarding() {
    let (harness, asked) = asked_the_condition("Right, done as asked.").await;

    let answer = harness
        .handle(harness.click(turn(2), asked.id, "yes", 3))
        .await
        .unwrap();

    assert_eq!(
        harness.events("trip", "trip-1").await,
        vec!["trip.name_set"],
        "yes means the instruction the card was guarding runs"
    );
    assert_eq!(receipt_codes(&answer), vec!["trip.name_set"]);
    assert!(
        !notice_codes(&answer).contains(&"turnframe.notice.instruction_declined".to_owned()),
        "nothing was declined"
    );
}

#[tokio::test]
async fn declining_a_conditional_instruction_runs_nothing_and_records_it() {
    let (harness, asked) = asked_the_condition("Understood, leaving it as it is.").await;

    let answer = harness
        .handle(harness.click(turn(2), asked.id, "no", 3))
        .await
        .unwrap();

    assert!(
        harness.events("trip", "trip-1").await.is_empty(),
        "no means no: the instruction was not carried out"
    );
    assert!(receipt_codes(&answer).is_empty(), "and nothing to claim");
    assert!(
        notice_codes(&answer).contains(&"turnframe.notice.instruction_declined".to_owned()),
        "the turn says the instruction was declined instead of going quiet: {:?}",
        notice_codes(&answer)
    );
    assert_eq!(
        harness.trip_revision("trip-1").value(),
        3,
        "the case did not move"
    );
}

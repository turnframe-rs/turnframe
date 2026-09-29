//! An operation a card may run and understanding may not propose.
//!
//! A workflow's own card can carry an action: a recap with a confirm button
//! that applies an operation. The server wrote that option and stored it, so
//! the click has to be admissible; and a user asking to see the recap again
//! must get the recap, not a bare "Confirm?" over an operation proposed from
//! their words.
//!
//! `ActAvailability::CardOnly` is the distinction. The operation stays in the
//! catalogue, so the reducer knows its schema, target policy and mutability;
//! understanding is shown it marked card-only, which keeps it off every task's
//! closed set; and the reducer admits it only as the card's own act.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{Harness, notice_codes, token_for};
use turnframe_core::ids::{OperationKey, TurnId};
use turnframe_runtime::reduce::rejection::NOT_PROPOSABLE;
use turnframe_test::providers::{ScriptedProvider, UnderstandingBuilder};
use turnframe_test::workflows::trip::{
    REBOOK_CONFIRM_OPTION, awaiting_rebooking_confirmation, operations, with_offer,
};

fn turn_one() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(1))
}

fn turn_two() -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(2))
}

/// The operation the card carries reaches understanding marked as one no task
/// may route to.
#[tokio::test]
async fn a_card_only_operation_is_not_proposable_by_understanding() {
    let turn = turn_one();
    let text = "Change the name to Porto";
    let understood = UnderstandingBuilder::of(text)
        .apply(
            operations::SET_NAME,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!({"value": "Porto"}),
            text,
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, awaiting_rebooking_confirmation())
        .understands(understood)
        .without_narration()
        .build()
        .await;

    harness.handle(harness.turn(turn, text)).await.unwrap();

    let seen = harness.understander.seen();
    let input = seen.first().expect("the turn was understood");
    let proposable = |key: &str| {
        input
            .operation(&OperationKey::from(key))
            .map(|(_, spec)| spec.availability.is_proposable())
    };
    assert_eq!(
        proposable(operations::REBOOK),
        Some(false),
        "an operation only the button runs is not one understanding may choose"
    );
    assert_eq!(
        proposable(operations::SET_NAME),
        Some(true),
        "and everything else still is"
    );
}

/// Proposed from the user's words anyway, it is refused and nothing is written.
#[tokio::test]
async fn a_card_only_operation_understanding_proposes_is_refused() {
    let turn = turn_one();
    let text = "Rebook it";
    let understood = UnderstandingBuilder::of(text)
        .apply(
            operations::REBOOK,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!(null),
            text,
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, awaiting_rebooking_confirmation())
        .understands(understood)
        .without_narration()
        .build()
        .await;

    let answered = harness.handle(harness.turn(turn, text)).await.unwrap();

    assert!(
        notice_codes(&answered).contains(&NOT_PROPOSABLE.to_owned()),
        "{:?}",
        notice_codes(&answered)
    );
    assert_eq!(harness.replay(turn).await.act_outcomes, vec!["rejected"]);
    assert!(
        harness.events("trip", "trip-1").await.is_empty(),
        "an operation only the button runs is not run from typed words"
    );
}

/// The click still works: the button the server wrote does what it says.
#[tokio::test]
async fn the_card_that_carries_it_still_runs_it() {
    let first = turn_one();
    let review_text = "It is ready, show me the rebooking card";
    let review = UnderstandingBuilder::of(review_text)
        .apply(
            operations::REQUEST_REBOOKING,
            token_for(first, "trip", "trip-1"),
            serde_json::json!({"leg": 1}),
            review_text,
        )
        .build()
        .unwrap();
    let provider = ScriptedProvider::builder("scripted", "model-1")
        .acknowledging("Have a look.")
        .acknowledging("Sent.")
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

    let before = harness.events("trip", "trip-1").await.len();
    harness
        .handle(harness.click(turn_two(), card.id, REBOOK_CONFIRM_OPTION, revision))
        .await
        .unwrap();
    assert!(
        harness.events("trip", "trip-1").await.len() > before,
        "the button the server wrote does what the server said it does, even \
         though understanding may not propose it"
    );
    let clicked = harness.replay(turn_two()).await;
    let acts: Vec<_> = clicked
        .target_resolutions
        .iter()
        .map(|record| record.act)
        .collect();
    assert_eq!(
        (acts, clicked.act_outcomes),
        (
            vec![turnframe_runtime::resume::card_act_id()],
            vec!["ready_to_execute".to_owned()]
        ),
        "it is the card's own act that is admitted"
    );
}

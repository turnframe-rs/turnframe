//! Pressing the rebooking card's «Not now» declines the rebooking and nothing else: the click's own
//! act is one the card offers, so no refusal reaches the user beside the decline.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{Harness, token_for};
use turnframe_core::ids::TurnId;
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::{REBOOK_DECLINE_OPTION, operations, with_offer};

#[tokio::test]
async fn declining_the_rebooking_card_refuses_nothing() {
    let first = TurnId::from(uuid::Uuid::from_u128(1));
    let text = "rebook it";
    let review = UnderstandingBuilder::of(text)
        .apply(
            operations::REQUEST_REBOOKING,
            token_for(first, "trip", "trip-1"),
            serde_json::json!({"leg": 1}),
            text,
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, with_offer(1))
        .understands(review)
        .without_narration()
        .build()
        .await;
    harness.handle(harness.turn(first, text)).await.unwrap();
    let card = harness.blocking_card("trip", "trip-1").await;
    let revision = harness.trip_revision("trip-1");
    let second = TurnId::from(uuid::Uuid::from_u128(2));

    let answered = harness
        .handle(harness.click(second, card.id, REBOOK_DECLINE_OPTION, revision.0))
        .await
        .unwrap();

    let outcomes = harness.replay(second).await.act_outcomes;
    assert!(!outcomes.is_empty(), "the click was reduced");
    assert!(
        outcomes.iter().all(|outcome| outcome != "rejected"),
        "{outcomes:?} {:#?}",
        answered.blocks
    );
}

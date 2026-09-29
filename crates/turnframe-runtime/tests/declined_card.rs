//! A card the user declines does not come straight back.
//!
//! Declining a `ConfirmCommand` ends the card without effect, so the projection still declares
//! the same requirement. The runtime holds that card back until the case's revision moves:
//! "not now" means "ask me when the document changes".
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{Harness, narrating, token_for};
use turnframe_core::ids::TurnId;
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::{REBOOK_DECLINE_OPTION, operations, with_offer};

fn turn(n: u128) -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(n))
}

/// A harness whose trip is complete, so asking for the review raises the
/// rebooking card. The whole conversation is scripted up front: the review
/// turn, the click that declines, and one more turn that changes nothing.
async fn with_the_send_card() -> Harness {
    let first = turn(1);
    let text = "show me the rebooking card";
    let review = UnderstandingBuilder::of(text)
        .apply(
            operations::REQUEST_REBOOKING,
            token_for(first, "trip", "trip-1"),
            serde_json::json!({"leg": 1}),
            text,
        )
        .build()
        .unwrap();
    let later = UnderstandingBuilder::of("ok").ask("ok").build().unwrap();
    let provider = narrating()
        // The click's own turn: nothing runs, and the reply says the
        // instruction was declined, which is material.
        .acknowledging("All right.")
        .answering("Nothing has changed.")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, with_offer(1))
        .understands(review)
        .understands(later)
        .provider(Arc::clone(&provider))
        .build()
        .await;
    harness.handle(harness.turn(first, text)).await.unwrap();
    harness
}

/// Declining does not put the same question back up.
#[tokio::test]
async fn a_declined_card_is_not_raised_again_at_the_same_revision() {
    let harness = with_the_send_card().await;
    let card = harness.blocking_card("trip", "trip-1").await;
    let revision = harness.trip_revision("trip-1");

    harness
        .handle(harness.click(turn(2), card.id, REBOOK_DECLINE_OPTION, revision.0))
        .await
        .unwrap();

    assert!(
        harness.open_cards("trip", "trip-1").await.is_empty(),
        "the card the user refused is not on screen in the reply that \
         acknowledges the refusal"
    );

    harness.handle(harness.turn(turn(3), "ok")).await.unwrap();
    assert!(
        harness.open_cards("trip", "trip-1").await.is_empty(),
        "nothing has changed, so the question has not become askable again"
    );
    assert_eq!(
        harness.trip_revision("trip-1"),
        revision,
        "and the case really has not moved, which is what makes that right"
    );
}

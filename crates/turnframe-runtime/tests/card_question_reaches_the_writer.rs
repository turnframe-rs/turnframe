//! A card's own question travels to the writer, on both halves of its life.
//!
//! The writer is told what the card on screen asks and which buttons it has, and,
//! when a card closes, what it asked and what the user pressed. A rule asking a
//! model to reconstruct a string nobody gave it is not a rule.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{Harness, narrating, token_for};
use turnframe_core::ids::TurnId;
use turnframe_provider::purpose::ModelPurpose;
use turnframe_test::providers::{ScriptedProvider, UnderstandingBuilder};
use turnframe_test::workflows::trip::{REBOOK_DECLINE_OPTION, operations, with_offer};

fn turn(n: u128) -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(n))
}

/// What the `n`th acknowledgement was written from, as the model read it.
fn acknowledged_from(provider: &ScriptedProvider, n: usize) -> String {
    provider
        .calls_for(ModelPurpose::Acknowledge)
        .into_iter()
        .nth(n)
        .expect("the composer called the acknowledgement")
        .user_text()
}

/// A harness whose complete trip has just been asked for its rebooking card, so
/// the blocking confirmation is on screen.
async fn with_the_rebooking_card() -> (Harness, Arc<ScriptedProvider>) {
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
    (harness, provider)
}

/// The card the turn raises arrives with the question it asks.
#[tokio::test]
async fn the_card_a_turn_raises_carries_its_question() {
    let (_harness, provider) = with_the_rebooking_card().await;
    let shown = acknowledged_from(&provider, 0);
    assert!(
        shown.contains("\"card\": \"Trip 1: Rebook this flight? ("),
        "the writer reads the card's own question: {shown}"
    );
    assert!(shown.contains("Keep my flight"), "and its buttons: {shown}");
}

/// And the card the turn closes says what the user just declined.
#[tokio::test]
async fn the_card_a_decline_closes_carries_its_question() {
    let (harness, provider) = with_the_rebooking_card().await;
    let card = harness.blocking_card("trip", "trip-1").await;
    let revision = harness.trip_revision("trip-1");

    harness
        .handle(harness.click(turn(2), card.id, REBOOK_DECLINE_OPTION, revision.0))
        .await
        .unwrap();

    let shown = acknowledged_from(&provider, 1);
    assert!(
        shown.contains("The user answered «Rebook this flight?» with «Keep my flight»."),
        "the acknowledgement owed to a decline has a name, in the words the user \
         pressed: {shown}"
    );
}

/// And the act the card is holding is named by the card the domain titled for it.
#[tokio::test]
async fn the_act_a_card_is_holding_is_named() {
    let first = turn(1);
    let text = "Withdraw that trip";
    let withdraw = UnderstandingBuilder::of(text)
        .apply(
            operations::WITHDRAW,
            token_for(first, "trip", "trip-1"),
            serde_json::json!(null),
            text,
        )
        .build()
        .unwrap();
    let provider = narrating().build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, with_offer(1))
        .understands(withdraw)
        .provider(Arc::clone(&provider))
        .build()
        .await;
    harness.handle(harness.turn(first, text)).await.unwrap();

    let shown = acknowledged_from(&provider, 0);
    assert!(
        shown.contains("\"card\": \"Trip 1: Withdraw the case for"),
        "the writer is told WHICH act is waiting, not only that a card is: {shown}"
    );
    assert!(
        !shown.contains("\"done\": [\n    \""),
        "and nothing is reported done: {shown}"
    );
}

//! A card an earlier turn put on screen and nobody answered is the way forward of the next
//! reply that reaches its record: the reply points to it, and does not end on «what next?».
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{Harness, narrating, token_for};
use turnframe_core::ids::TurnId;
use turnframe_provider::purpose::ModelPurpose;
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::{operations, with_offer};

fn turn(n: u128) -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(n))
}

#[tokio::test]
async fn a_card_left_open_is_the_way_forward() {
    let text = "rebook the quoted flight";
    let rebook = UnderstandingBuilder::of(text)
        .apply(
            operations::REQUEST_REBOOKING,
            token_for(turn(1), "trip", "trip-1"),
            serde_json::json!({"leg": 1}),
            text,
        )
        .build()
        .unwrap();
    let asked = "hmm";
    let nothing = UnderstandingBuilder::of(asked).build().unwrap();
    let provider = narrating()
        .acknowledging("The card beside this reply confirms it.")
        .build_shared();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, with_offer(1))
        .understands(rebook)
        .understands(nothing)
        .provider(std::sync::Arc::clone(&provider))
        .build()
        .await;
    harness.handle(harness.turn(turn(1), text)).await.unwrap();
    let card = harness.blocking_card("trip", "trip-1").await;
    let title = card.payload.title.resolve(&"en-GB".into()).to_owned();

    harness.handle(harness.turn(turn(2), asked)).await.unwrap();

    let briefs: Vec<String> = provider
        .calls_for(ModelPurpose::Acknowledge)
        .into_iter()
        .map(|call| call.user_text())
        .collect();
    let second = briefs
        .get(1)
        .expect("the second reply is written, pointing to the card");
    assert!(second.contains(&title), "{second}");
}

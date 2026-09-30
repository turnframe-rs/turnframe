//! The next steps a reply offers are recorded on the turn, each an operation on its record
//! with the arguments known, so the next message can be read as taking one up, and a surface
//! can show them as choices.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{Harness, narrating, token_for};
use turnframe_core::ids::TurnId;
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::{operations, with_offer};

#[tokio::test]
async fn a_reply_records_the_offers_it_made() {
    let turn = TurnId::from(uuid::Uuid::from_u128(1));
    let text = "call it Porto";
    let understanding = UnderstandingBuilder::of(text)
        .apply(
            operations::SET_NAME,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!({ "value": "Porto" }),
            text,
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, with_offer(1))
        .understands(understanding)
        .provider(narrating().build_shared())
        .build()
        .await;

    let answered = harness.handle(harness.turn(turn, text)).await.unwrap();

    let offered: Vec<(String, String, serde_json::Value)> = answered
        .offers
        .iter()
        .map(|offer| {
            (
                offer.case_ref.case_id.to_string(),
                offer.operation.to_string(),
                serde_json::Value::Object(offer.arguments.clone()),
            )
        })
        .collect();
    assert_eq!(
        offered,
        [
            (
                "trip-1".to_owned(),
                operations::ADD_EXTRA.to_owned(),
                serde_json::json!({})
            ),
            (
                "trip-1".to_owned(),
                operations::REQUEST_REBOOKING.to_owned(),
                serde_json::json!({ "leg": 1 })
            ),
        ]
    );
    let stored = harness
        .stored_turn(turn)
        .await
        .expect("the turn was persisted");
    assert_eq!(
        stored.offers, answered.offers,
        "and they are kept with the turn"
    );
}

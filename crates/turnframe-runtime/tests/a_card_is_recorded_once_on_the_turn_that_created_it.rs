//! A card is recorded once on the turn that created it: an audit counting the cards a
//! turn raised counts each one once.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use support::{Harness, narrating, token_for};
use turnframe_core::ids::TurnId;
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::{complete_case, operations};

#[tokio::test]
async fn a_card_is_recorded_once_on_the_turn_that_created_it() {
    let turn = TurnId::from(uuid::Uuid::from_u128(1));
    let text = "cancel this trip";
    let cancelling = UnderstandingBuilder::of(text)
        .apply(
            operations::WITHDRAW,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!(null),
            text,
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, complete_case())
        .understands(cancelling)
        .provider(Arc::clone(&narrating().build_shared()))
        .build()
        .await;
    harness.handle(harness.turn(turn, text)).await.unwrap();

    let created = harness.replay(turn).await.interactions_created;
    assert_eq!(
        created.len(),
        1,
        "one confirmation, recorded once: {created:?}"
    );
}

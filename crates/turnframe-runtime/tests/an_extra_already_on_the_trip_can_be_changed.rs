//! An extra already on the sample trip can be changed: «the books are actually 13» changes
//! that extra's quantity and leaves the rest of it, its payer included, as it was.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{Harness, token_for};
use turnframe_core::ids::TurnId;
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::{incomplete_case, operations};

#[tokio::test]
async fn an_extra_already_on_the_trip_can_be_changed() {
    let turn = TurnId::from(uuid::Uuid::from_u128(1));
    let text = "the books are actually 13";
    let understanding = UnderstandingBuilder::of(text)
        .apply(
            operations::CHANGE_EXTRA,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!({"extra": 1, "quantity": 13}),
            text,
        )
        .build()
        .unwrap();
    let before = incomplete_case();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, before.clone())
        .understands(understanding)
        .without_narration()
        .build()
        .await;

    harness.handle(harness.turn(turn, text)).await.unwrap();

    let after = harness.trip_state("trip-1").unwrap();
    assert_eq!(after.extras[0].quantity, 13);
    assert_eq!(after.extras[0].description, before.extras[0].description);
    assert_eq!(
        after.extras[0].unit_price_cents,
        before.extras[0].unit_price_cents
    );
    assert_eq!(after.extras[0].payer, before.extras[0].payer);
}

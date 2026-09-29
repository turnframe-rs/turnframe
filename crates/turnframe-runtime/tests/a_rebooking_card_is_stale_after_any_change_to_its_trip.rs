//! A rebooking card names the revision it was shown for. When the airline re-quotes the
//! fare, or anything else changes the trip, a click on the old card runs nothing, and the
//! next card shows the new fare.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{Harness, narrating, token_for};
use turnframe_core::error::{InteractionError, OrchestratorError};
use turnframe_core::ids::TurnId;
use turnframe_core::interaction::InteractionRejection;
use turnframe_core::locale::Locale;
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::{
    REBOOK_CONFIRM_OPTION, REQUOTED_FARE_DIFFERENCE_CENTS, TripCommand, TripStatus, operations,
    sample_offer, with_offer,
};

fn turn(n: u128) -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(n))
}

fn card_body(card: &turnframe_core::interaction::Interaction) -> String {
    card.payload
        .body
        .as_ref()
        .expect("a rebooking card says what it rebooks")
        .resolve(&Locale::from("en-GB"))
        .to_owned()
}

#[tokio::test]
async fn a_rebooking_card_is_stale_after_any_change_to_its_trip() {
    let text = "show me the rebooking card";
    let review = UnderstandingBuilder::of(text)
        .apply(
            operations::REQUEST_REBOOKING,
            token_for(turn(1), "trip", "trip-1"),
            serde_json::json!({"leg": 1}),
            text,
        )
        .build()
        .unwrap();
    let asked = "what now?";
    let nothing = UnderstandingBuilder::of(asked).build().unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, with_offer(1))
        .understands(review)
        .understands(nothing)
        .provider(
            narrating()
                .acknowledging("Here is the card.")
                .acknowledging("The fare changed.")
                .build_shared(),
        )
        .build()
        .await;
    harness.handle(harness.turn(turn(1), text)).await.unwrap();
    let shown = harness.blocking_card("trip", "trip-1").await;
    let bound = harness.trip_revision("trip-1");
    assert!(
        card_body(&shown).contains("€84.00"),
        "{}",
        card_body(&shown)
    );

    let offer = sample_offer(1);
    harness
        .outside(
            "trip-1",
            "requote-1",
            TripCommand::Requote {
                leg: 1,
                flight: offer.flight,
                departs: offer.departs,
                fare_difference_cents: REQUOTED_FARE_DIFFERENCE_CENTS,
            },
        )
        .await
        .expect("the airline re-quotes");
    assert!(harness.trip_revision("trip-1") > bound);

    let refused = harness
        .handle(harness.click(turn(2), shown.id, REBOOK_CONFIRM_OPTION, bound.value()))
        .await
        .expect_err("a click on a fare that no longer holds confirms nothing");
    assert!(
        matches!(
            refused,
            OrchestratorError::Interaction(InteractionError::Rejected(
                InteractionRejection::Stale { .. } | InteractionRejection::NotActive { .. }
            ))
        ),
        "{refused:?}"
    );
    assert!(
        !harness
            .events("trip", "trip-1")
            .await
            .contains(&"trip.rebooking_sent".to_owned()),
        "nothing went to the airline"
    );
    assert_eq!(
        harness.trip_state("trip-1").unwrap().status,
        TripStatus::AwaitingRebookingConfirmation
    );

    harness.handle(harness.turn(turn(3), asked)).await.unwrap();
    let redrawn = harness.blocking_card("trip", "trip-1").await;
    assert_ne!(redrawn.id, shown.id);
    assert!(
        card_body(&redrawn).contains("€132.00"),
        "{}",
        card_body(&redrawn)
    );
}

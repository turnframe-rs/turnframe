//! A confirmation click authorizes the commands its card names, and no others.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{Harness, token_for};
use turnframe_core::ids::TurnId;
use turnframe_core::interaction::InteractionKind;
use turnframe_runtime::policy::CONFIRM_OPTION_ID;
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::{complete_case, operations};

const FIRST: &str = "Cancel trip 1";
const SECOND: &str = "and cancel trip 2 as well";

fn turn(n: u128) -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(n))
}

#[tokio::test]
async fn a_click_on_one_cards_confirmation_does_not_confirm_another_command() {
    let cancel_the_first = UnderstandingBuilder::of(FIRST)
        .apply(
            operations::WITHDRAW,
            token_for(turn(1), "trip", "trip-1"),
            serde_json::json!(null),
            FIRST,
        )
        .build()
        .unwrap();
    let cancel_the_second = UnderstandingBuilder::of(SECOND)
        .apply(
            operations::WITHDRAW,
            token_for(turn(2), "trip", "trip-2"),
            serde_json::json!(null),
            SECOND,
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, complete_case())
        .trip("trip-2", "Trip 2", 3, complete_case())
        .understands(cancel_the_first)
        .understands(cancel_the_second)
        .without_narration()
        .build()
        .await;
    harness.handle(harness.turn(turn(1), FIRST)).await.unwrap();
    let card = harness.blocking_card("trip", "trip-1").await;
    assert_eq!(card.kind, InteractionKind::ConfirmCommand);

    harness
        .handle(harness.click_and_say(
            turn(2),
            card.id,
            CONFIRM_OPTION_ID,
            card.case_ref.expected_revision.0,
            SECOND,
        ))
        .await
        .unwrap();

    assert_eq!(
        harness.events("trip", "trip-1").await,
        vec!["trip.withdrawn".to_owned()],
        "the click confirms the command its own card named"
    );
    assert!(
        harness.events("trip", "trip-2").await.is_empty(),
        "a click on trip 1's card must not authorize cancelling trip 2"
    );
    let second_card = harness.blocking_card("trip", "trip-2").await;
    assert_eq!(second_card.kind, InteractionKind::ConfirmCommand);
}

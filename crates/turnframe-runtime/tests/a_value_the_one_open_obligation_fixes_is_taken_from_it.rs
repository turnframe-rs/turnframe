//! An act missing a value that the record's one open obligation for that operation fixes
//! takes it from there: «the traveler pays for it» on a trip with one extra whose payer is
//! open settles that extra. With two such obligations the value is still asked for.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{Harness, token_for};
use turnframe_core::ids::TurnId;
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::{Payer, TripState, operations, unassigned_case};

const TEXT: &str = "the traveler pays for it";

async fn assigning(state: TripState) -> Harness {
    let turn = TurnId::from(uuid::Uuid::from_u128(1));
    let understanding = UnderstandingBuilder::of(TEXT)
        .apply(
            operations::ASSIGN_PAYER,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!({"payer": "traveler"}),
            TEXT,
        )
        .needing(&["extra"])
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, state)
        .understands(understanding)
        .without_narration()
        .build()
        .await;
    harness.handle(harness.turn(turn, TEXT)).await.unwrap();
    harness
}

#[tokio::test]
async fn a_value_the_one_open_obligation_fixes_is_taken_from_it() {
    let harness = assigning(unassigned_case()).await;
    let state = harness.trip_state("trip-1").unwrap();
    assert_eq!(state.extras[0].payer, Some(Payer::Traveler));
}

#[tokio::test]
async fn with_two_obligations_it_could_come_from_the_value_is_asked_for() {
    let mut state = unassigned_case();
    let mut second = state.extras[0].clone();
    second.extra_id = uuid::Uuid::from_u128(2);
    second.description = "Second".to_owned();
    state.extras.push(second);
    let harness = assigning(state).await;
    let state = harness.trip_state("trip-1").unwrap();
    assert!(state.extras.iter().all(|line| line.payer.is_none()));
}

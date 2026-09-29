//! One message that registers a traveler, puts them on the trip and adds an extra is
//! three acts, and the second needs what the first creates: it runs after the traveler
//! exists, on that traveler, and the extra runs beside them.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{Harness, token_for};
use turnframe_core::ids::TurnId;
use turnframe_core::understanding::RecordValue;
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::traveler::operations as traveler_ops;
use turnframe_test::workflows::trip::{TripState, incomplete_case, operations};

const TEXT: &str = "register Marta Bianchi, put her on the trip and add a checked bag at 40 euros";

#[tokio::test]
async fn a_traveler_registered_for_a_trip_is_put_on_it() {
    let turn = TurnId::from(uuid::Uuid::from_u128(1));
    let registering = UnderstandingBuilder::of(TEXT).open(
        traveler_ops::CREATE_DRAFT,
        "traveler",
        serde_json::json!({"full_name": "Marta Bianchi"}),
        "register Marta Bianchi",
    );
    let registered = registering.last_act().unwrap();
    let understanding = registering
        .apply(
            operations::SET_TRAVELER,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!({}),
            "put her on the trip",
        )
        .with_record("traveler", RecordValue::SameTurn { act: registered })
        .apply(
            operations::ADD_EXTRA,
            token_for(turn, "trip", "trip-1"),
            serde_json::json!({"description": "Checked bag", "quantity": 1,
                               "unit_price": {"minor": 4_000, "currency": "EUR"}}),
            "add a checked bag at 40 euros",
        )
        .build()
        .unwrap();
    assert_eq!(understanding.acts[1].depends_on, vec![registered]);
    let without_traveler = TripState {
        traveler: None,
        ..incomplete_case()
    };
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, without_traveler)
        .understands(understanding)
        .without_narration()
        .build()
        .await;

    harness.handle(harness.turn(turn, TEXT)).await.unwrap();

    let commands: Vec<String> = harness
        .journal(turn)
        .await
        .into_iter()
        .map(|entry| entry.command_type)
        .collect();
    let created = commands
        .iter()
        .position(|command| command.starts_with("traveler."))
        .expect("the traveler was registered");
    let placed = commands
        .iter()
        .position(|command| command == "trip.change_traveler")
        .expect("the traveler was put on the trip");
    assert!(created < placed, "{commands:?}");
    assert!(
        commands.iter().any(|command| command == "trip.add_extra"),
        "{commands:?}"
    );
    assert_eq!(
        harness.trip_traveler("trip-1").as_deref(),
        Some("Marta Bianchi")
    );
    assert_eq!(harness.trip_state("trip-1").unwrap().extras.len(), 2);
}

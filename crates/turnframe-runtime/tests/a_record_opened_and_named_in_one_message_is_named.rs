//! One message that opens a record and acts on it runs both: the act on the record is
//! checked against the state its opening leaves, not refused because the record did not
//! exist before the message.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::Harness;
use turnframe_core::ids::TurnId;
use turnframe_core::understanding::ActTarget;
use turnframe_runtime::resolve::{CaseIdFactory, DerivedCaseIdFactory};
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::operations;

const TEXT: &str = "open a trip and call it Lisbon offsite";

#[tokio::test]
async fn a_record_opened_and_named_in_one_message_is_named() {
    let turn = TurnId::from(uuid::Uuid::from_u128(1));
    let opening = UnderstandingBuilder::of(TEXT).open(
        operations::OPEN,
        "trip",
        serde_json::json!({}),
        "open a trip",
    );
    let opened = opening.last_act().unwrap();
    let mut understanding = opening
        .apply_to(
            operations::SET_NAME,
            ActTarget::SameTurn { act: opened },
            serde_json::json!({"value": "Lisbon offsite"}),
            "call it Lisbon offsite",
        )
        .build()
        .unwrap();
    understanding.acts[1].depends_on.push(opened);
    let harness = Harness::builder()
        .understands(understanding)
        .without_narration()
        .build()
        .await;

    harness.handle(harness.turn(turn, TEXT)).await.unwrap();

    let trip = DerivedCaseIdFactory.new_case_id(&"trip".into(), &turn, opened);
    assert_eq!(
        harness.trip_name(trip.as_str()).as_deref(),
        Some("Lisbon offsite"),
        "{:?}",
        harness.journal(turn).await
    );
}

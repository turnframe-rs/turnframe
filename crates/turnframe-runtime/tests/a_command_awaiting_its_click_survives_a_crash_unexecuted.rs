//! A command waiting for its confirmation click is never executed by crash recovery.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{Harness, account, token_for};
use turnframe_core::error::StoreError;
use turnframe_core::ids::TurnId;
use turnframe_core::interaction::InteractionStatus;
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::stores::FailurePoint;
use turnframe_test::workflows::trip::{complete_case, operations};

const TEXT: &str = "Cancel trip 1";

#[tokio::test]
async fn recovery_after_a_crash_leaves_an_unconfirmed_command_waiting() {
    let turn_id = TurnId::from(uuid::Uuid::from_u128(1));
    let cancel = UnderstandingBuilder::of(TEXT)
        .apply(
            operations::WITHDRAW,
            token_for(turn_id, "trip", "trip-1"),
            serde_json::json!(null),
            TEXT,
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, complete_case())
        .understands(cancel)
        .without_narration()
        .build()
        .await;
    harness.fail_at(
        FailurePoint::BeforeResponsePersistence,
        StoreError::Unavailable,
    );

    assert!(harness.handle(harness.turn(turn_id, TEXT)).await.is_err());
    let card = harness.blocking_card("trip", "trip-1").await;

    let _ = harness
        .orchestrator
        .resume_turn(&account(), &turn_id)
        .await
        .expect("recovery could act");

    assert!(
        harness.events("trip", "trip-1").await.is_empty(),
        "the cancellation was never confirmed, so recovery must not run it"
    );
    let still_open = harness.blocking_card("trip", "trip-1").await;
    assert_eq!(still_open.id, card.id);
    assert_eq!(still_open.status, InteractionStatus::Active);
}

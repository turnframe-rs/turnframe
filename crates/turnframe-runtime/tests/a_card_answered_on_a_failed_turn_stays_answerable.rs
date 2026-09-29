//! A card answered on a turn that fails before anything commits can be answered again.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{Harness, token_for};
use turnframe_core::error::{OrchestratorError, StoreError};
use turnframe_core::ids::TurnId;
use turnframe_core::interaction::InteractionStatus;
use turnframe_runtime::policy::CONFIRM_OPTION_ID;
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::stores::FailurePoint;
use turnframe_test::workflows::trip::{complete_case, operations};

const TEXT: &str = "Cancel trip 1";
const ANSWER: &str = "yes, and thanks";

fn turn(n: u128) -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(n))
}

#[tokio::test]
async fn a_failed_turn_gives_the_card_back() {
    let cancel = UnderstandingBuilder::of(TEXT)
        .apply(
            operations::WITHDRAW,
            token_for(turn(1), "trip", "trip-1"),
            serde_json::json!(null),
            TEXT,
        )
        .build()
        .unwrap();
    let thanks = UnderstandingBuilder::of(ANSWER)
        .chitchat("and thanks")
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, complete_case())
        .understands(cancel)
        .understands(thanks)
        .without_narration()
        .build()
        .await;
    harness.handle(harness.turn(turn(1), TEXT)).await.unwrap();
    let card = harness.blocking_card("trip", "trip-1").await;
    let revision = card.case_ref.expected_revision.0;

    // The confirmed command cannot be admitted, so the turn fails before anything commits.
    harness.fail_at(FailurePoint::BeforeJournalInsert, StoreError::Unavailable);
    let failed = harness
        .handle(harness.click_and_say(turn(2), card.id, CONFIRM_OPTION_ID, revision, ANSWER))
        .await;
    assert!(
        matches!(
            failed,
            Err(OrchestratorError::Store(StoreError::Unavailable))
        ),
        "the journal refused the command and so the turn failed: {failed:?}"
    );

    let after = harness.blocking_card("trip", "trip-1").await;
    assert_eq!(
        after.status,
        InteractionStatus::Active,
        "a turn that committed nothing must leave the card answerable"
    );
    assert!(harness.events("trip", "trip-1").await.is_empty());

    harness
        .handle(harness.click(turn(3), card.id, CONFIRM_OPTION_ID, revision))
        .await
        .unwrap();
    assert_eq!(
        harness.events("trip", "trip-1").await,
        vec!["trip.withdrawn".to_owned()],
        "answering it again works"
    );
}

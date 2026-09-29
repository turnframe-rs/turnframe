//! A record argument naming a record nobody has registered is asked for again, and
//! when its workflow can register one the notice offers to, on screen in the server's own
//! words. The name waits with the act.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{Harness, token_for};
use turnframe_core::ids::TurnId;
use turnframe_core::response::Expectation;
use turnframe_core::understanding::{ActStatus, ActTarget, ArgumentValue, RecordValue};
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::{TripState, operations};

const TEXT: &str = "the trip is for Omar Haddad";

#[tokio::test]
async fn a_traveler_named_that_does_not_exist_is_offered_to_register() {
    let turn = TurnId::from(uuid::Uuid::from_u128(1));
    let understanding = UnderstandingBuilder::of(TEXT)
        .apply_to(
            operations::SET_TRAVELER,
            ActTarget::Record {
                token: token_for(turn, "trip", "trip-1"),
            },
            serde_json::json!({}),
            TEXT,
        )
        .with_record(
            "traveler",
            RecordValue::Named {
                workflow: "traveler".into(),
                named: "Omar Haddad".to_owned(),
            },
        )
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, TripState::default())
        .understands(understanding)
        .without_narration()
        .build()
        .await;

    let answered = harness.handle(harness.turn(turn, TEXT)).await.unwrap();

    let waiting = answered
        .expectations
        .iter()
        .find_map(|expectation| match expectation {
            Expectation::AwaitingValue { act, .. } => Some(act),
            _ => None,
        })
        .expect("the act waits for its traveler");
    let ActStatus::NeedsValue { reason, .. } = &waiting.status else {
        panic!("{waiting:?}")
    };
    let reason = reason.clone().unwrap_or_default();
    assert!(
        matches!(
            &waiting.arguments["traveler"].value,
            ArgumentValue::Record(RecordValue::Named { named, .. }) if named == "Omar Haddad"
        ),
        "the name waits with it, for the traveler registered next"
    );
    assert!(
        reason.contains("«Omar Haddad» yet: I can register it"),
        "{answered:#?}"
    );
    assert!(harness.events("trip", "trip-1").await.is_empty());
    let shown = answered.blocks.iter().any(|block| {
        matches!(block, turnframe_core::response::ResponseBlock::Notice(notice)
            if notice.text.default.contains("I can register it"))
    });
    assert!(
        shown,
        "the offer is on screen whatever the reply's words: {answered:#?}"
    );
}

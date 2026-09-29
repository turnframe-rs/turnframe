//! A reply that asks for a value records what it asked for, and the next turn's
//! understanding is shown the waiting act, so a bare answer completes it (spec §6.8).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use support::{Harness, notice_codes, token_for};
use turnframe_core::ids::{OperationKey, TurnId};
use turnframe_core::response::Expectation;
use turnframe_test::providers::UnderstandingBuilder;
use turnframe_test::workflows::trip::{incomplete_case, operations};

const ASKED: &str = "can I set the name?";
const ANSWERED: &str = "Lisbon";

fn turn(n: u128) -> TurnId {
    TurnId::from(uuid::Uuid::from_u128(n))
}

#[tokio::test]
async fn the_next_turn_is_shown_the_act_waiting_for_its_value() {
    let asking = UnderstandingBuilder::of(ASKED)
        .apply(
            operations::SET_NAME,
            token_for(turn(1), "trip", "trip-1"),
            serde_json::json!({}),
            ASKED,
        )
        .needing(&["value"])
        .build()
        .unwrap();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understands(asking)
        .understands(UnderstandingBuilder::of(ANSWERED).build().unwrap())
        .without_narration()
        .build()
        .await;

    let first = harness.handle(harness.turn(turn(1), ASKED)).await.unwrap();
    let [
        Expectation::AwaitingValue {
            act,
            case_ref,
            missing,
        },
    ] = first.expectations.as_slice()
    else {
        panic!("one expectation: {:?}", first.expectations);
    };
    assert_eq!(
        act.operation(),
        Some(&OperationKey::from(operations::SET_NAME))
    );
    assert_eq!(
        case_ref.as_ref().map(|c| c.case_id.as_str()),
        Some("trip-1")
    );
    assert_eq!(missing, &["value"]);
    assert!(
        notice_codes(&first).is_empty(),
        "a missing value is asked for, not reported"
    );

    harness
        .handle(harness.turn(turn(2), ANSWERED))
        .await
        .unwrap();
    let seen = harness.understander.seen();
    let Some(turnframe_understand::Expectation::Values(pending)) = seen[1].expectation.as_ref()
    else {
        panic!("the second turn expects a value: {:?}", seen[1].expectation);
    };
    assert_eq!(pending.operation, OperationKey::from(operations::SET_NAME));
    assert_eq!(pending.record, Some(token_for(turn(2), "trip", "trip-1")));
    assert_eq!(pending.missing, ["value"]);
    assert!(
        seen[0].expectation.is_none(),
        "the first turn expected nothing"
    );
}

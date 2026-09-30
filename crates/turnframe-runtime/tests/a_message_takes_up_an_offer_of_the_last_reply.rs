//! The offers a reply made reach the next turn's understanding: a message taking one up runs
//! it on its record with the values it knew, and a message taking none is read as before.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use serde_json::json;
use support::Harness;
use turnframe_core::ids::TurnId;
use turnframe_tasks::testing::ScriptedTasks;
use turnframe_test::workflows::trip::{operations, with_offer};

fn request(to: usize) -> serde_json::Value {
    json!({"analysis": "A request.", "units": [
        {"kind": "request", "words": {"from": 1, "to": to}, "workflow": "trip"}
    ]})
}

#[tokio::test]
async fn a_message_takes_up_an_offer_of_the_last_reply() {
    // Turn 1: [1]call [2]it [3]Porto. Turn 2: [1]yes, [2]rebook [3]it
    let tasks = Arc::new(
        ScriptedTasks::new("tasks", "small")
            .answer("turn/segment", request(3))
            .answer("turn/coverage", json!({"missed": []}))
            .answer("u1/route", json!({"operations": [operations::SET_NAME]}))
            .answer(
                "u1/extract",
                json!({"arguments": {"value": {"kind": "words", "text": "Porto",
                                                "message": "current", "from": 3, "to": 3}}}),
            )
            .answer(
                "u1/verify",
                json!({"reason": "Names it.", "arguments": {"value": "stated"}, "overall": "confirmed"}),
            )
            .answer("turn/segment", request(3))
            .answer("turn/coverage", json!({"missed": []}))
            .answer("u1/take_up", json!({"offer": "o2"}))
            .answer(
                "u1/verify",
                json!({"reason": "Takes up the rebooking.", "arguments": {}, "overall": "confirmed"}),
            ),
    );
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, with_offer(1))
        .understanding_tasks(Arc::clone(&tasks))
        .without_narration()
        .build()
        .await;

    let first = harness
        .handle(harness.turn(TurnId::from(uuid::Uuid::from_u128(1)), "call it Porto"))
        .await
        .unwrap();
    assert_eq!(first.offers.len(), 2, "{:?}", first.offers);
    harness
        .handle(harness.turn(TurnId::from(uuid::Uuid::from_u128(2)), "yes, rebook it"))
        .await
        .unwrap();

    let card = harness.blocking_card("trip", "trip-1").await;
    assert!(
        card.payload.title.default.contains("Rebook"),
        "the rebooking the reply offered was taken up: {card:?}"
    );
    assert!(
        tasks.unanswered().is_empty(),
        "left: {:?}",
        tasks.unanswered()
    );
}

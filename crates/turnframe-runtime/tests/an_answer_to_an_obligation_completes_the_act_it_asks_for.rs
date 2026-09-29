//! Asked who pays for its one extra with no payer, the user answers «airline»: the question
//! carried the act that answers it, the extra included, so the answer needs only the value.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use serde_json::json;
use support::Harness;
use turnframe_core::ids::TurnId;
use turnframe_core::response::Expectation;
use turnframe_tasks::testing::ScriptedTasks;
use turnframe_test::workflows::trip::{operations, unassigned_case};

fn verified(arguments: serde_json::Value) -> serde_json::Value {
    json!({"reason": "Stated.", "arguments": arguments, "overall": "confirmed"})
}

#[tokio::test]
async fn an_answer_to_an_obligation_completes_the_act_it_asks_for() {
    let name = json!({"kind": "words", "text": "Lisbon", "message": "current", "from": 5, "to": 5});
    let tasks = Arc::new(
        ScriptedTasks::new("tasks", "small")
            .answer(
                "turn/segment",
                json!({"analysis": "Sets the name.", "units": [
                    {"kind": "request", "words": {"from": 1, "to": 5}, "workflow": "trip"}
                ]}),
            )
            .answer("turn/coverage", json!({"missed": []}))
            .answer("u1/route", json!({"operations": [operations::SET_NAME]}))
            .answer("u1/extract", json!({"arguments": {"value": name}}))
            .answer("u1/verify", verified(json!({"value": "stated"})))
            .answer(
                "turn/segment",
                json!({"analysis": "Answers who pays.", "units": [
                    {"kind": "provides_value", "words": {"from": 1, "to": 1}}
                ]}),
            )
            .answer("turn/coverage", json!({"missed": []}))
            .answer(
                "u1/route",
                json!({"operations": [operations::ASSIGN_PAYER]}),
            )
            .answer(
                "u1/extract",
                json!({"arguments": {"payer": {
                    "kind": "value", "message": "current", "from": 1, "to": 1, "value": "airline"
                }}}),
            )
            .answer("u1/verify", verified(json!({"payer": "stated"}))),
    );
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, unassigned_case())
        .understanding_tasks(Arc::clone(&tasks))
        .build()
        .await;

    let asked = harness
        .handle(harness.turn(
            TurnId::from(uuid::Uuid::from_u128(1)),
            "Set the name to Lisbon",
        ))
        .await
        .unwrap();
    assert!(
        asked
            .expectations
            .iter()
            .any(|expectation| matches!(expectation, Expectation::AwaitingOperation { .. })),
        "{:?}",
        asked.expectations
    );

    harness
        .handle(harness.turn(TurnId::from(uuid::Uuid::from_u128(2)), "airline"))
        .await
        .unwrap();

    let events = harness.events("trip", "trip-1").await;
    assert!(
        events
            .iter()
            .any(|event| event.as_str() == "trip.payer_assigned"),
        "{events:?}"
    );
}

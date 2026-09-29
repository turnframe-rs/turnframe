//! «the traveler pays for the extra», then «no, the company pays»: the correction reads only
//! the payer, and the reply before it carried the act it did, so the extra is kept and the
//! payer changes.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use serde_json::json;
use support::Harness;
use turnframe_core::ids::TurnId;
use turnframe_tasks::testing::ScriptedTasks;
use turnframe_test::workflows::trip::{Payer, operations, unassigned_case};

fn verified(arguments: serde_json::Value) -> serde_json::Value {
    json!({"reason": "Stated.", "arguments": arguments, "overall": "confirmed"})
}

fn value(from: usize, to: usize, value: serde_json::Value) -> serde_json::Value {
    json!({"kind": "value", "message": "current", "from": from, "to": to, "value": value})
}

#[tokio::test]
async fn a_correction_keeps_what_the_last_turn_did_beside_the_value_it_changes() {
    let tasks = Arc::new(
        ScriptedTasks::new("tasks", "small")
            // [1]the [2]traveler [3]pays [4]for [5]the [6]extra
            .answer(
                "turn/segment",
                json!({"analysis": "Says who pays.", "units": [
                    {"kind": "request", "words": {"from": 1, "to": 6}, "workflow": "trip"}
                ]}),
            )
            .answer("turn/coverage", json!({"missed": []}))
            .answer("u1/route", json!({"operations": [operations::ASSIGN_PAYER]}))
            .answer(
                "u1/extract",
                json!({"arguments": {
                    "extra": value(5, 6, json!(1)),
                    "payer": value(1, 2, json!("traveler"))
                }}),
            )
            .answer(
                "u1/verify",
                verified(json!({"extra": "stated", "payer": "stated"})),
            )
            // [1]no, [2]the [3]company [4]pays
            .answer(
                "turn/segment",
                json!({"analysis": "Corrects the payer.", "units": [{"kind": "correction",
                    "words": {"from": 1, "to": 4}, "workflow": "trip", "corrects": null}]}),
            )
            .answer("turn/coverage", json!({"missed": []}))
            .answer("u1/route", json!({"operations": [operations::ASSIGN_PAYER]}))
            .answer(
                "u1/extract",
                json!({"arguments": {
                    "extra": {"kind": "not_given"},
                    "payer": value(2, 3, json!("company"))
                }}),
            )
            .answer("u1/verify", verified(json!({"payer": "stated"}))),
    );
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, unassigned_case())
        .understanding_tasks(Arc::clone(&tasks))
        .build()
        .await;

    harness
        .handle(harness.turn(
            TurnId::from(uuid::Uuid::from_u128(1)),
            "the traveler pays for the extra",
        ))
        .await
        .unwrap();
    assert_eq!(
        harness.trip_state("trip-1").unwrap().extras[0].payer,
        Some(Payer::Traveler)
    );

    harness
        .handle(harness.turn(
            TurnId::from(uuid::Uuid::from_u128(2)),
            "no, the company pays",
        ))
        .await
        .unwrap();
    assert_eq!(
        harness.trip_state("trip-1").unwrap().extras[0].payer,
        Some(Payer::Company)
    );
}

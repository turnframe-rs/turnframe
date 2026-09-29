//! Asked for the traveler's full name, the user sends «AZ7654321». Read first as the name,
//! the domain refuses it as no name, the reading finds none, and the answer is routed once
//! more: it is the loyalty number, and the name stays unset.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use serde_json::json;
use support::Harness;
use turnframe_core::ids::TurnId;
use turnframe_tasks::testing::ScriptedTasks;
use turnframe_test::workflows::traveler::{SAMPLE_EMAIL, TravelerState, operations};

fn verified(arguments: serde_json::Value) -> serde_json::Value {
    json!({"reason": "Stated.", "arguments": arguments, "overall": "confirmed"})
}

fn words(from: usize, to: usize) -> serde_json::Value {
    json!({"kind": "words", "message": "current", "from": from, "to": to, "text": ""})
}

fn a_value() -> serde_json::Value {
    json!({"analysis": "A value.", "units": [
        {"kind": "provides_value", "words": {"from": 1, "to": 1}}
    ]})
}

#[tokio::test]
async fn a_loyalty_number_given_as_the_name_lands_on_the_loyalty_number() {
    let tasks = Arc::new(
        ScriptedTasks::new("tasks", "small")
            // [1]AZ1234567
            .answer(
                "turn/segment",
                json!({"analysis": "Sets the loyalty number.", "units": [
                    {"kind": "request", "words": {"from": 1, "to": 1}, "workflow": "traveler"}
                ]}),
            )
            .answer("turn/coverage", json!({"missed": []}))
            .answer(
                "u1/route",
                json!({"operations": [operations::SET_LOYALTY_NUMBER]}),
            )
            .answer("u1/extract", json!({"arguments": {"value": words(1, 1)}}))
            .answer("u1/verify", verified(json!({"value": "stated"})))
            // [1]AZ7654321
            .answer("turn/segment", a_value())
            .answer("turn/coverage", json!({"missed": []}))
            .answer("u1/route", json!({"operations": [operations::SET_NAME]}))
            .answer("u1/extract", json!({"arguments": {"value": words(1, 1)}}))
            .answer("u1/verify", verified(json!({"value": "stated"})))
            .answer(
                "u1/extract.after_check",
                json!({"arguments": {"value": {"kind": "not_given"}}}),
            )
            .answer(
                "u1/route.again",
                json!({"operations": [operations::SET_LOYALTY_NUMBER]}),
            )
            .answer(
                "u1/extract.after_reroute",
                json!({"arguments": {"value": words(1, 1)}}),
            )
            .answer(
                "u1/verify.after_reroute",
                verified(json!({"value": "stated"})),
            ),
    );
    let harness = Harness::builder()
        .traveler(
            "trav-1",
            "New traveler 1",
            1,
            TravelerState {
                email: Some(SAMPLE_EMAIL.to_owned()),
                ..TravelerState::default()
            },
        )
        .understanding_tasks(Arc::clone(&tasks))
        .build()
        .await;

    harness
        .handle(harness.turn(TurnId::from(uuid::Uuid::from_u128(1)), "AZ1234567"))
        .await
        .unwrap();
    harness
        .handle(harness.turn(TurnId::from(uuid::Uuid::from_u128(2)), "AZ7654321"))
        .await
        .unwrap();

    let events = harness.events("traveler", "trav-1").await;
    assert_eq!(
        events
            .iter()
            .filter(|e| *e == "traveler.loyalty_number_set")
            .count(),
        2,
        "{events:?} {:?}",
        tasks.called()
    );
    assert!(
        !events.iter().any(|e| e == "traveler.full_name_set"),
        "{events:?}"
    );
}

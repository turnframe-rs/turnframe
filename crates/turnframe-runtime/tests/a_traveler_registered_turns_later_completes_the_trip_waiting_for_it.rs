//! Told a trip is for a traveler nobody registered, the assistant keeps that act while
//! the conversation moves on; when the user registers the traveler under that name turns
//! later, the trip gets it in the same turn.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use serde_json::json;
use support::Harness;
use turnframe_core::ids::TurnId;
use turnframe_tasks::testing::ScriptedTasks;
use turnframe_test::workflows::trip::{TripState, operations};

const NAMED: &str = "the trip is for Omar Haddad";
const MEANWHILE: &str = "set the name to Lisbon";
const REGISTER: &str = "register Omar Haddad";

fn request(to: usize, workflow: &str) -> serde_json::Value {
    json!({"analysis": "One request.", "units": [
        {"kind": "request", "words": {"from": 1, "to": to}, "workflow": workflow}
    ]})
}

fn words(from: usize, to: usize, text: &str) -> serde_json::Value {
    json!({"kind": "words", "text": text, "message": "current", "from": from, "to": to})
}

fn stated(argument: &str) -> serde_json::Value {
    json!({"reason": "Stated.", "arguments": {argument: "stated"}, "overall": "confirmed"})
}

#[tokio::test]
async fn a_traveler_registered_turns_later_completes_the_trip_waiting_for_it() {
    let tasks = ScriptedTasks::new("tasks", "small")
        // [1]the [2]trip [3]is [4]for [5]Haddad [6]Ltd
        .answer("turn/segment", request(6, "trip"))
        .answer("turn/coverage", json!({"missed": []}))
        .answer("u1/route", json!({"operations": [operations::SET_TRAVELER]}))
        .answer(
            "u1/extract",
            json!({"arguments": {"traveler": {"kind": "record", "name": "Omar Haddad",
                "message": "current", "from": 5, "to": 6, "record": "by_name"}}}),
        )
        .answer("u1/verify", stated("traveler"))
        // [1]set [2]the [3]name [4]to [5]Lisbon
        .answer("turn/segment", request(5, "trip"))
        .answer("turn/coverage", json!({"missed": []}))
        .answer("u1/route", json!({"operations": [operations::SET_NAME]}))
        .answer("u1/extract", json!({"arguments": {"value": words(5, 5, "Lisbon")}}))
        .answer("u1/verify", stated("value"))
        // [1]register [2]Haddad [3]Ltd
        .answer("turn/segment", request(3, "traveler"))
        .answer("turn/coverage", json!({"missed": []}))
        .answer("u1/route", json!({"operations": ["traveler.create_draft"]}))
        .answer(
            "u1/extract",
            json!({"arguments": {"full_name": words(2, 3, "Omar Haddad")}}),
        )
        .answer("u1/verify", stated("full_name"));
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, TripState::default())
        .understanding_tasks(Arc::new(tasks))
        .without_narration()
        .build()
        .await;
    let turn = |n: u128| TurnId::from(uuid::Uuid::from_u128(n));

    harness.handle(harness.turn(turn(1), NAMED)).await.unwrap();
    assert_eq!(harness.trip_traveler("trip-1"), None);
    harness
        .handle(harness.turn(turn(2), MEANWHILE))
        .await
        .unwrap();
    assert_eq!(harness.trip_name("trip-1").as_deref(), Some("Lisbon"));
    harness
        .handle(harness.turn(turn(3), REGISTER))
        .await
        .unwrap();

    assert_eq!(
        harness.trip_traveler("trip-1").as_deref(),
        Some("Omar Haddad"),
        "the trip waiting since the first turn gets the traveler registered for it"
    );
}

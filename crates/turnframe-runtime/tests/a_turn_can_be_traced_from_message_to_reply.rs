//! A traced turn leaves one line per event: the message, each understanding step, every
//! model call with its request and answer, what was understood and decided, and the reply.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use serde_json::{Value, json};
use support::Harness;
use turnframe_core::ids::TurnId;
use turnframe_runtime::trace::JsonlTrace;
use turnframe_tasks::testing::ScriptedTasks;
use turnframe_test::workflows::trip::{incomplete_case, operations};

const TEXT: &str = "Set the name to Lisbon";

fn understanding_tasks() -> Arc<ScriptedTasks> {
    let unit = json!({"kind": "request", "words": {"from": 1, "to": 5}, "workflow": "trip"});
    let value =
        json!({"kind": "words", "text": "Lisbon", "message": "current", "from": 5, "to": 5});
    Arc::new(
        ScriptedTasks::new("tasks", "small")
            .answer("turn/segment", json!({"analysis": "One request.", "units": [unit]}))
            .answer("turn/coverage", json!({"missed": []}))
            .answer("u1/route", json!({"operations": [operations::SET_NAME]}))
            .answer("u1/extract", json!({"arguments": {"value": value}}))
            .answer(
                "u1/verify",
                json!({"reason": "Stated.", "arguments": {"value": "stated"}, "overall": "confirmed"}),
            )
            .answer("reply/acknowledge", json!({"text": "Done. Which day would you rather fly?"}))
            .answer(
                "reply/review",
                json!({
                    "reasoning": "Rests on the receipt and asks the ask.",
                    "asks_the_ask": true,
                    "asks_anything_else": false,
                    "claims_beyond_material": false
                }),
            ),
    )
}

#[tokio::test]
async fn every_event_and_model_call_is_one_line() {
    let directory = std::env::temp_dir().join(format!("turnframe-trace-{}", uuid::Uuid::new_v4()));
    let trace = Arc::new(JsonlTrace::create(&directory).unwrap());
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understanding_tasks(understanding_tasks())
        .trace(Arc::clone(&trace))
        .build()
        .await;
    let turn = TurnId::from(uuid::Uuid::from_u128(1));
    harness.handle(harness.turn(turn, TEXT)).await.unwrap();

    let lines: Vec<Value> = std::fs::read_to_string(trace.path())
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let events: Vec<&str> = lines
        .iter()
        .map(|line| line["event"].as_str().unwrap())
        .collect();
    assert_eq!(events.first(), Some(&"turn_received"));
    assert_eq!(events.last(), Some(&"turn_completed"));
    for expected in ["step", "understood", "reduced"] {
        assert!(events.contains(&expected), "{expected} in {events:?}");
    }

    let calls: Vec<&Value> = lines
        .iter()
        .filter(|line| line["event"] == "model_call")
        .collect();
    let tasks: Vec<&str> = calls
        .iter()
        .filter_map(|call| call["task"].as_str())
        .collect();
    assert_eq!(
        tasks,
        [
            "turn/segment",
            "turn/coverage",
            "u1/route",
            "u1/extract",
            "u1/verify",
            "reply/acknowledge",
            "reply/review"
        ],
        "every call, by its task"
    );
    assert!(
        calls
            .iter()
            .any(|call| call["purpose"] == "acknowledge" && call.get("response").is_some()),
        "and the reply, with its answer"
    );
    let turn_label = turn.to_string();
    for call in &calls {
        assert_eq!(
            call["turn"].as_str(),
            Some(turn_label.as_str()),
            "each call names its turn"
        );
        assert!(
            call["request"]["messages"].is_array(),
            "the request as sent"
        );
        assert!(
            ["response", "streamed", "error"]
                .iter()
                .any(|outcome| call.get(*outcome).is_some()),
            "and what came back, a failed attempt included: {call}"
        );
    }
    let completed = lines.last().unwrap();
    assert!(completed["reply"]["blocks"].is_array());
    assert!(completed["record"]["understanding"].is_object());
    let _ = std::fs::remove_dir_all(directory);
}

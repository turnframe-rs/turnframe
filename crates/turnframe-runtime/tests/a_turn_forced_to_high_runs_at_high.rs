//! A turn forced to high effort runs at high: its understanding calls reason, save the one
//! that copies values, its segmentation votes, the whole turn is checked, and the record
//! says so.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use serde_json::json;
use support::Harness;
use turnframe_core::effort::Effort;
use turnframe_core::ids::TurnId;
use turnframe_tasks::testing::ScriptedTasks;
use turnframe_test::workflows::trip::{incomplete_case, operations};

const TEXT: &str = "Set the name to Lisbon";

/// Every call a high turn makes of [`TEXT`], votes included.
fn high_tasks() -> Arc<ScriptedTasks> {
    let unit = json!({"kind": "request", "words": {"from": 1, "to": 5}, "workflow": "trip"});
    let segmented = json!({"analysis": "One request.", "units": [unit]});
    let routed = json!({"operations": [operations::SET_NAME]});
    let value =
        json!({"kind": "words", "text": "Lisbon", "message": "current", "from": 5, "to": 5});
    let verified =
        json!({"reason": "Stated.", "arguments": {"value": "stated"}, "overall": "confirmed"});
    Arc::new(
        ScriptedTasks::new("tasks", "small")
            .answer("turn/segment", segmented.clone())
            .answer("turn/segment", segmented.clone())
            .answer("turn/segment", segmented)
            .answer("turn/coverage", json!({"missed": []}))
            .answer("u1/route", routed.clone())
            .answer("u1/route", routed.clone())
            .answer("u1/route", routed)
            .answer("u1/extract", json!({"arguments": {"value": value}}))
            .answer("u1/verify", verified.clone())
            .answer("u1/verify", verified.clone())
            .answer("u1/verify", verified)
            .answer("turn/cross_check", json!({"findings": []})),
    )
}

#[tokio::test]
async fn a_turn_forced_to_high_runs_at_high() {
    let tasks = high_tasks();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understanding_tasks(Arc::clone(&tasks))
        .without_narration()
        .build()
        .await;
    let turn_id = TurnId::from(uuid::Uuid::from_u128(1));
    let mut turn = harness.turn(turn_id, TEXT);
    turn.effort = Some(Effort::High);

    harness.handle(turn).await.unwrap();

    assert_eq!(harness.trip_name("trip-1").as_deref(), Some("Lisbon"));
    let called = tasks.called();
    assert_eq!(
        called
            .iter()
            .filter(|task| task.starts_with("turn/segment"))
            .count(),
        3,
        "{called:?}"
    );
    assert!(
        called
            .iter()
            .any(|task| task.starts_with("turn/cross_check"))
    );
    let record = harness.replay(turn_id).await;
    assert_eq!(record.effort, Effort::High);
    assert!(
        record.tasks.iter().all(
            |task| (task.params.reasoning_effort.as_deref() == Some("low"))
                != task.task_id.as_str().contains("/extract")
        ),
        "{:?}",
        record
            .tasks
            .iter()
            .map(|task| (&task.task_id, &task.params.reasoning_effort))
            .collect::<Vec<_>>()
    );
}

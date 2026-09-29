//! A turn that forces no effort runs at the configured default: medium unless the
//! configuration says otherwise.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use serde_json::json;
use support::Harness;
use turnframe_core::effort::Effort;
use turnframe_core::ids::TurnId;
use turnframe_runtime::config::OrchestratorConfig;
use turnframe_runtime::effort::EffortConfig;
use turnframe_tasks::testing::ScriptedTasks;
use turnframe_test::workflows::trip::{incomplete_case, operations};

const TEXT: &str = "Set the name to Lisbon";

/// The calls of [`TEXT`], each answered `times` times.
fn tasks(times: usize, checked: bool) -> Arc<ScriptedTasks> {
    let unit = json!({"kind": "request", "words": {"from": 1, "to": 5}, "workflow": "trip"});
    let value =
        json!({"kind": "words", "text": "Lisbon", "message": "current", "from": 5, "to": 5});
    let mut script = ScriptedTasks::new("tasks", "small")
        .answer("turn/coverage", json!({"missed": []}))
        .answer("u1/extract", json!({"arguments": {"value": value}}));
    for _ in 0..times {
        script = script
            .answer(
                "turn/segment",
                json!({"analysis": "One request.", "units": [unit.clone()]}),
            )
            .answer(
                "u1/route",
                json!({"operations": [operations::SET_NAME]}),
            )
            .answer(
                "u1/verify",
                json!({"reason": "Stated.", "arguments": {"value": "stated"}, "overall": "confirmed"}),
            );
    }
    if checked {
        script = script.answer("turn/cross_check", json!({"findings": []}));
    }
    Arc::new(script)
}

async fn run(config: OrchestratorConfig, tasks: Arc<ScriptedTasks>) -> Effort {
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understanding_tasks(tasks)
        .without_narration()
        .config(config)
        .build()
        .await;
    let turn_id = TurnId::from(uuid::Uuid::from_u128(1));
    harness.handle(harness.turn(turn_id, TEXT)).await.unwrap();
    assert_eq!(harness.trip_name("trip-1").as_deref(), Some("Lisbon"));
    harness.replay(turn_id).await.effort
}

#[tokio::test]
async fn a_turn_left_alone_runs_at_the_configured_level() {
    let mut effort = EffortConfig::default();
    effort.default = Effort::High;
    let high = tasks(3, true);
    let level = run(
        OrchestratorConfig::conservative().with_effort(effort),
        Arc::clone(&high),
    )
    .await;
    assert_eq!(level, Effort::High);
    assert!(
        high.called()
            .iter()
            .any(|task| task.starts_with("turn/cross_check"))
    );
}

#[tokio::test]
async fn with_nothing_configured_a_turn_runs_at_medium() {
    let medium = tasks(1, false);
    let level = run(OrchestratorConfig::conservative(), Arc::clone(&medium)).await;
    assert_eq!(level, Effort::Medium);
    let called = medium.called();
    assert_eq!(
        called
            .iter()
            .filter(|task| task.starts_with("turn/segment"))
            .count(),
        1
    );
    assert!(
        !called
            .iter()
            .any(|task| task.starts_with("turn/cross_check"))
    );
}

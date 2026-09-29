//! A deployment may have each understanding step said in the user's language while the
//! message is read, as `TurnEvent::StepSaid`, for a preview. It costs a call per step,
//! so it is off unless asked for.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use serde_json::json;
use support::Harness;
use turnframe_core::ids::TurnId;
use turnframe_runtime::config::NarrationConfig;
use turnframe_runtime::stream::{RecordingSink, TurnEvent};
use turnframe_tasks::testing::ScriptedTasks;
use turnframe_test::workflows::trip::{incomplete_case, operations};

const TEXT: &str = "Set the name to Lisbon";

/// Every call of one turn that sets the name, and a progress line for each step.
fn tasks() -> Arc<ScriptedTasks> {
    let unit = json!({"kind": "request", "words": {"from": 1, "to": 5}, "workflow": "trip"});
    let value =
        json!({"kind": "words", "text": "Lisbon", "message": "current", "from": 5, "to": 5});
    let mut script = ScriptedTasks::new("tasks", "small")
        .answer(
            "turn/segment",
            json!({"analysis": "One request.", "units": [unit]}),
        )
        .answer("turn/coverage", json!({"missed": []}))
        .answer("u1/route", json!({"operations": [operations::SET_NAME]}))
        .answer("u1/extract", json!({"arguments": {"value": value}}))
        .answer(
            "u1/verify",
            json!({"reason": "Stated.", "arguments": {"value": "stated"}, "overall": "confirmed"}),
        )
        .answer(
            "reply/acknowledge",
            json!({"text": "Done. Which day would you rather fly?"}),
        )
        .answer(
            "reply/review",
            json!({
                "reasoning": "Rests on the receipt and asks the ask.",
                "asks_the_ask": true,
                "asks_anything_else": false,
                "claims_beyond_material": false,
                "contradicts_screen": false
            }),
        );
    for _ in 0..12 {
        script = script.answer("reply/step", json!({"text": "Reading your message…"}));
    }
    Arc::new(script)
}

async fn events(narration: NarrationConfig) -> (Vec<TurnEvent>, Vec<String>) {
    let tasks = tasks();
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understanding_tasks(Arc::clone(&tasks))
        .narration(narration)
        .build()
        .await;
    let sink = Arc::new(RecordingSink::new());
    harness
        .orchestrator
        .handle_turn_streaming(
            harness.turn(TurnId::from(uuid::Uuid::from_u128(1)), TEXT),
            sink.clone(),
        )
        .await
        .unwrap();
    (sink.events(), tasks.called())
}

#[tokio::test]
async fn a_step_is_said_in_the_users_language_when_asked() {
    let (events, _) = events(NarrationConfig::conservative().with_steps(true)).await;
    let said: Vec<(usize, &TurnEvent)> = events
        .iter()
        .enumerate()
        .filter(|(_, event)| matches!(event, TurnEvent::StepSaid { .. }))
        .collect();
    assert!(!said.is_empty(), "the steps were said: {events:?}");
    for (at, event) in &said {
        let TurnEvent::StepSaid { step, text } = event else {
            unreachable!()
        };
        assert_eq!(text, "Reading your message…");
        let shown = events[..*at]
            .iter()
            .any(|earlier| matches!(earlier, TurnEvent::Step(done) if **done == **step));
        assert!(shown, "a step is said after it was decided, never before");
    }
}

#[tokio::test]
async fn no_step_is_said_unless_asked() {
    let (events, called) = events(NarrationConfig::conservative()).await;
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, TurnEvent::StepSaid { .. })),
        "{events:?}"
    );
    assert!(
        !called.iter().any(|task| task == "reply/step"),
        "and no call was made for it: {called:?}"
    );
}

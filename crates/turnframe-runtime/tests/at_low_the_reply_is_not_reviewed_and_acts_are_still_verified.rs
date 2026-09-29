//! A turn at low effort writes its reply without a review, and still verifies every act
//! that changes a record: low saves judgment on words, never on effects.
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

#[tokio::test]
async fn at_low_the_reply_is_not_reviewed_and_acts_are_still_verified() {
    let unit = json!({"kind": "request", "words": {"from": 1, "to": 5}, "workflow": "trip"});
    let value =
        json!({"kind": "words", "text": "Lisbon", "message": "current", "from": 5, "to": 5});
    let tasks = Arc::new(
        ScriptedTasks::new("tasks", "small")
            .answer(
                "turn/segment",
                json!({"analysis": "One request.", "units": [unit]}),
            )
            .answer("turn/coverage", json!({"missed": []}))
            .answer(
                "u1/route",
                json!({"operations": [operations::SET_NAME]}),
            )
            .answer("u1/extract", json!({"arguments": {"value": value}}))
            .answer(
                "u1/verify",
                json!({"reason": "Stated.", "arguments": {"value": "stated"}, "overall": "confirmed"}),
            )
            .answer(
                "reply/acknowledge",
                json!({"text": "Done. Which day would you rather fly?"}),
            ),
    );
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, incomplete_case())
        .understanding_tasks(Arc::clone(&tasks))
        .build()
        .await;
    let mut turn = harness.turn(TurnId::from(uuid::Uuid::from_u128(1)), TEXT);
    turn.effort = Some(Effort::Low);

    harness.handle(turn).await.unwrap();

    assert_eq!(harness.trip_name("trip-1").as_deref(), Some("Lisbon"));
    let called = tasks.called();
    assert!(called.iter().any(|task| task == "u1/verify"), "{called:?}");
    assert!(
        called.iter().any(|task| task == "reply/acknowledge"),
        "{called:?}"
    );
    assert!(
        !called.iter().any(|task| task.starts_with("reply/review")),
        "{called:?}"
    );
}

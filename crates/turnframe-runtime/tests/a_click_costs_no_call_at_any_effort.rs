//! A click carries its own meaning: at any effort it costs no understanding call.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use serde_json::json;
use support::Harness;
use turnframe_core::effort::Effort;
use turnframe_core::ids::TurnId;
use turnframe_tasks::testing::ScriptedTasks;
use turnframe_test::workflows::trip::{REBOOK_CONFIRM_OPTION, awaiting_rebooking_confirmation};

#[tokio::test]
async fn a_click_costs_no_call_at_any_effort() {
    for level in Effort::ALL {
        let tasks = Arc::new(
            ScriptedTasks::new("tasks", "small")
                .answer(
                    "turn/segment",
                    json!({"analysis": "A greeting.", "units": [
                        {"kind": "chitchat", "words": {"from": 1, "to": 1}}
                    ]}),
                )
                .answer("turn/coverage", json!({"missed": []})),
        );
        let harness = Harness::builder()
            .trip("trip-1", "Trip 1", 3, awaiting_rebooking_confirmation())
            .understanding_tasks(Arc::clone(&tasks))
            .without_narration()
            .build()
            .await;
        harness
            .handle(harness.turn(TurnId::from(uuid::Uuid::from_u128(1)), "hello"))
            .await
            .unwrap();
        let before = tasks.called().len();
        let card = harness.blocking_card("trip", "trip-1").await;
        let revision = harness.trip_revision("trip-1").value();
        let mut click = harness.click(
            TurnId::from(uuid::Uuid::from_u128(2)),
            card.id,
            REBOOK_CONFIRM_OPTION,
            revision,
        );
        click.effort = Some(level);

        harness.handle(click).await.unwrap();

        assert_eq!(
            tasks.called().len(),
            before,
            "a click at {level} called a model"
        );
    }
}

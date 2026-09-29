//! A request for a workflow that cannot start was understood: the reply says why it
//! cannot start, in the workflow's own words, and does not say it was not understood.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use std::sync::Arc;

use serde_json::json;
use support::{Harness, notice_codes};
use turnframe_core::ids::TurnId;
use turnframe_core::locale::Locale;
use turnframe_core::response::ResponseBlock;
use turnframe_runtime::reduce::notice;
use turnframe_tasks::testing::ScriptedTasks;
use turnframe_test::workflows::trip::awaiting_rebooking_confirmation;

const TEXT: &str = "register a new traveler";

#[tokio::test]
async fn a_request_for_a_workflow_that_cannot_start_says_why() {
    let unit = json!({"kind": "request", "words": {"from": 1, "to": 4}, "workflow": "traveler"});
    let tasks = Arc::new(
        ScriptedTasks::new("tasks", "small")
            .answer(
                "turn/segment",
                json!({"analysis": "A new traveler.", "units": [unit]}),
            )
            .answer("turn/coverage", json!({"missed": []}))
            .answer("u1/route", json!({"operations": ["none"]})),
    );
    // No trip is being drafted, so no traveler can be added.
    let harness = Harness::builder()
        .trip("trip-1", "Trip 1", 3, awaiting_rebooking_confirmation())
        .understanding_tasks(tasks)
        .without_narration()
        .build()
        .await;

    let reply = harness
        .handle(harness.turn(TurnId::from(uuid::Uuid::from_u128(1)), TEXT))
        .await
        .unwrap();

    let codes = notice_codes(&reply);
    assert!(
        !codes.contains(&notice::NOT_UNDERSTOOD.to_owned()),
        "the request was understood: {codes:?}"
    );
    let said: Vec<String> = reply
        .blocks
        .iter()
        .filter_map(|block| match block {
            ResponseBlock::Notice(shown) => {
                Some(shown.text.resolve(&Locale::from("en-GB")).to_owned())
            }
            _ => None,
        })
        .collect();
    assert!(
        said.iter()
            .any(|text| text.contains("only be added while a trip is open")),
        "and the reason it cannot start is said: {said:?}"
    );
}

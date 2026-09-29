//! A constraint the segmentation lost, and lost again when told where it is, cannot be
//! enforced, so nothing in the message runs.
mod support;

use serde_json::json;
use support::{SET_NAME, one_request, routed, turn, understand};
use turnframe_core::understanding::Unreadable;
use turnframe_tasks::testing::ScriptedTasks;

#[tokio::test]
async fn a_constraint_only_coverage_found_makes_the_message_unreadable() {
    // [1]name [2]Lisbon [3]but [4]don't [5]send [6]it
    let script = ScriptedTasks::new("scripted", "small")
        .answer("turn/segment", one_request(1, 2))
        .answer(
            "turn/coverage",
            json!({"missed": [{"kind": "constraint", "words": {"from": 4, "to": 6}, "workflow": "trip"}]}),
        )
        .answer("u1/route", routed(SET_NAME))
        .answer("turn/segment.after_coverage", one_request(1, 2))
        .answer(
            "turn/coverage.after_segment",
            json!({"missed": [{"kind": "constraint", "words": {"from": 4, "to": 6}, "workflow": "trip"}]}),
        )
        .answer("u1/route", routed(SET_NAME));
    let run = understand(script, &turn("name Lisbon but don't send it")).await;

    assert_eq!(
        run.understanding.unreadable,
        Some(Unreadable::LostConstraint)
    );
    assert!(run.understanding.acts.is_empty());
}

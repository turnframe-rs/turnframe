//! Words segmentation read as small talk that coverage reads as an act, and that a second
//! segmentation still reads as small talk, run nothing: the readings disagree on whether
//! anything was asked, so the words are reported unclear and no act comes of them.
mod support;

use serde_json::json;
use support::{turn, understand};
use turnframe_core::understanding::NotUnderstoodReason;
use turnframe_tasks::testing::ScriptedTasks;

#[tokio::test]
async fn small_talk_a_second_reading_takes_for_an_act_runs_nothing() {
    // [1]I [2]give [3]up
    let script = ScriptedTasks::new("scripted", "small")
        .answer(
            "turn/segment",
            json!({"analysis": "Small talk.", "units": [
                {"kind": "chitchat", "words": {"from": 1, "to": 3}}
            ]}),
        )
        .answer(
            "turn/coverage",
            json!({"missed": [{"kind": "cancel", "words": {"from": 1, "to": 3}, "workflow": "unknown"}]}),
        )
        .answer(
            "turn/segment.after_coverage",
            json!({"analysis": "Still a remark.", "units": [
                {"kind": "chitchat", "words": {"from": 1, "to": 3}}
            ]}),
        )
        .answer(
            "turn/coverage.after_segment",
            json!({"missed": [{"kind": "cancel", "words": {"from": 1, "to": 3}, "workflow": "unknown"}]}),
        );
    let run = understand(script, &turn("I give up")).await;

    let understanding = &run.understanding;
    assert!(understanding.acts.is_empty(), "{understanding:?}");
    assert_eq!(understanding.not_understood.len(), 1, "{understanding:?}");
    assert_eq!(
        understanding.not_understood[0].reason,
        NotUnderstoodReason::Unclear
    );
    assert!(!run.was_called("u1/route") && !run.was_called("u2/route"));
}

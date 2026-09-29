//! Words segmentation read as small talk and coverage as a request are read again, by
//! default, told what coverage saw: a request misread as small talk gets its second chance.
mod support;

use serde_json::json;
use support::{OPEN, confirmed, routed, turn, understand};
use turnframe_tasks::testing::ScriptedTasks;

#[tokio::test]
async fn small_talk_a_check_reads_as_a_request_is_read_again() {
    // [1]lets [2]start [3]an [4]trip
    let script = ScriptedTasks::new("scripted", "small")
        .answer(
            "turn/segment",
            json!({"analysis": "Small talk.", "units": [
                {"kind": "chitchat", "words": {"from": 1, "to": 4}}
            ]}),
        )
        .answer(
            "turn/coverage",
            json!({"missed": [{"kind": "request", "words": {"from": 1, "to": 4}, "workflow": "trip"}]}),
        )
        .answer(
            "turn/segment.after_coverage",
            json!({"analysis": "A request.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 4}, "workflow": "trip"}
            ]}),
        )
        .answer("turn/coverage.after_segment", json!({"missed": []}))
        .answer("u1/route", routed(OPEN))
        .answer("u1/verify", confirmed(json!({})));
    let run = understand(script, &turn("lets start an trip")).await;

    assert_eq!(run.understanding.acts.len(), 1, "{:?}", run.understanding);
}

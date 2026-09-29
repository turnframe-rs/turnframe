//! Told to read a message again because a check found constraint words in no unit, the
//! segmentation is told every such stretch at once: one named at a time is fixed alone.
mod support;

use serde_json::json;
use support::{SET_NAME, routed, turn, understand};
use turnframe_provider::request::ContentPart;
use turnframe_tasks::testing::ScriptedTasks;

#[tokio::test]
async fn every_constraint_word_a_reading_lost_is_named_when_it_reads_again() {
    // [1]rename [2]it [3]Porto, [4]keep [5]the [6]date, [7]confirm [8]nothing
    let units = json!({"analysis": "A rename.", "units": [
        {"kind": "request", "words": {"from": 1, "to": 3}, "workflow": "trip"}
    ]});
    let lost = json!({"missed": [
        {"kind": "constraint", "words": {"from": 4, "to": 6}, "workflow": "trip"},
        {"kind": "constraint", "words": {"from": 7, "to": 8}, "workflow": "trip"}
    ]});
    let script = ScriptedTasks::new("scripted", "small")
        .answer("turn/segment", units.clone())
        .answer("turn/coverage", lost.clone())
        .answer("u1/route", routed(SET_NAME))
        .answer("turn/segment.after_coverage", units)
        .answer("turn/coverage.after_segment", lost)
        .answer("u1/route", routed(SET_NAME));
    let run = understand(
        script,
        &turn("rename it Porto, keep the date, confirm nothing"),
    )
    .await;

    let told = run
        .provider
        .calls()
        .into_iter()
        .filter(|request| request.messages.len() > 1)
        .flat_map(|request| request.messages)
        .flat_map(|message| message.content)
        .filter_map(|part| match part {
            ContentPart::Text { text } => Some(text),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(told.contains("«keep the date,»"), "{told}");
    assert!(told.contains("«confirm nothing»"), "{told}");
}

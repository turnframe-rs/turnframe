//! A constraint coverage finds in words no unit holds sends the segmentation back once,
//! with those words; a segmentation that then places them is read as usual.
mod support;

use serde_json::json;
use support::{SET_NAME, confirmed, one_request, routed, turn, understand, words};
use turnframe_core::understanding::ActStatus;
use turnframe_tasks::testing::ScriptedTasks;

#[tokio::test]
async fn a_segmentation_told_what_it_left_out_is_read_again() {
    // [1]set [2]the [3]name [4]to [5]Porto [6]2026
    let script = ScriptedTasks::new("scripted", "small")
        .answer("turn/segment", one_request(1, 5))
        .answer(
            "turn/coverage",
            json!({"missed": [{"kind": "constraint", "words": {"from": 6, "to": 6}, "workflow": "trip"}]}),
        )
        .answer("u1/route", routed(SET_NAME))
        .answer("turn/segment.after_coverage", one_request(1, 6))
        .answer("turn/coverage.after_segment", json!({"missed": []}))
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(5, 6)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})));
    let run = understand(script, &turn("set the name to Porto 2026")).await;

    assert_eq!(run.understanding.unreadable, None);
    let [act] = run.understanding.acts.as_slice() else {
        panic!("one act expected: {:?}", run.understanding);
    };
    assert_eq!(act.status, ActStatus::Ready);
    let segment_again = run
        .provider
        .calls()
        .into_iter()
        .find(|call| call.metadata.get("task") == Some("turn/segment.after_coverage"))
        .expect("the segmentation was sent back");
    assert!(
        format!("{:?}", segment_again.messages).contains("Words 6 to 6"),
        "told which words it left out"
    );
}

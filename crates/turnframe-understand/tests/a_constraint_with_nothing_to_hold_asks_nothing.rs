//! A keep-unchanged constraint beside no act that changes a record costs no call.
mod support;

use serde_json::json;
use support::{script, turn, understand};

#[tokio::test]
async fn a_constraint_with_nothing_to_hold_asks_nothing() {
    // [1]what [2]is [3]the [4]name? [5]Leave [6]the [7]date [8]alone
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "A question and a constraint.", "units": [
                {"kind": "question", "words": {"from": 1, "to": 4}, "workflow": "trip",
                 "basis": "current_committed_state", "continues_previous": false},
                {"kind": "constraint", "words": {"from": 5, "to": 8}, "constraint": "keep_unchanged"}
            ]}),
        )
        .answer(
            "u1/frame",
            json!({"topic": "record_state", "record": "tok-trip-1", "subjects": ["name"]}),
        );
    let run = understand(script, &turn("what is the name? Leave the date alone")).await;

    assert!(run.understanding.acts.is_empty(), "{:?}", run.understanding);
    assert!(
        !run.called().iter().any(|task| task.contains("respects")),
        "{:?}",
        run.called()
    );
}

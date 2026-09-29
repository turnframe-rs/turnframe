//! A correction can only change a request or correction listed before it. One that names
//! itself, a later unit or one of another kind changes something from before this message:
//! it is read as that, and not sent back.
mod support;

use serde_json::json;
use support::{SET_NAME, confirmed, routed, script, turn, understand, words};

#[tokio::test]
async fn a_correction_naming_itself_corrects_something_earlier() {
    // [1]I [2]said [3]Lisbon
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "A correction.", "units": [
                {"kind": "correction", "words": {"from": 1, "to": 3}, "workflow": "trip",
                 "corrects": 1}
            ]}),
        )
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(3, 3)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})));
    let run = understand(script, &turn("I said Lisbon")).await;

    assert_eq!(run.understanding.acts.len(), 1, "{:?}", run.understanding);
    assert!(
        !run.called().iter().any(|call| call.contains("repair")),
        "{:?}",
        run.called()
    );
}

#[tokio::test]
async fn a_correction_naming_a_question_corrects_something_earlier() {
    // [1]what [2]is [3]left? [4]also [5]it [6]is [7]Lisbon
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "A question and a correction.", "units": [
                {"kind": "question", "words": {"from": 1, "to": 3}, "workflow": "trip",
                 "basis": "current_committed_state", "continues_previous": false},
                {"kind": "correction", "words": {"from": 4, "to": 7}, "workflow": "trip",
                 "corrects": 1}
            ]}),
        )
        .answer("u1/frame", json!({"topic": "knowledge", "record": "none"}))
        .answer("u2/route", routed(SET_NAME))
        .answer("u2/extract", json!({"arguments": {"value": words(7, 7)}}))
        .answer("u2/verify", confirmed(json!({"value": "stated"})));
    let run = understand(script, &turn("what is left? also it is Lisbon")).await;

    assert_eq!(run.understanding.acts.len(), 1, "{:?}", run.understanding);
    assert!(
        !run.called().iter().any(|call| call.contains("repair")),
        "{:?}",
        run.called()
    );
}

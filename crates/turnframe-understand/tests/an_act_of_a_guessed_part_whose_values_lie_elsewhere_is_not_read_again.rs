//! An act of a part only coverage found, whose every value lies in another part's words, is
//! that part read twice: it runs nothing and is not read again, so a guess spends one call.
mod support;

use serde_json::json;
use support::{SET_NAME, confirmed, routed, turn, understand, words};
use turnframe_tasks::testing::ScriptedTasks;

#[tokio::test]
async fn an_act_of_a_guessed_part_whose_values_lie_elsewhere_is_not_read_again() {
    // [1]so, [2]name [3]Lisbon
    let script = ScriptedTasks::new("scripted", "small")
        .answer(
            "turn/segment",
            json!({"analysis": "A name.", "units": [
                {"kind": "request", "words": {"from": 2, "to": 3}, "workflow": "trip"}
            ]}),
        )
        .answer(
            "turn/coverage",
            json!({"missed": [{"kind": "request", "words": {"from": 1, "to": 1}, "workflow": "trip"}]}),
        )
        .answer("u1/route", routed(SET_NAME))
        .answer("u2/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(3, 3)}}))
        .answer("u2/extract", json!({"arguments": {"value": words(3, 3)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})));
    let run = understand(script, &turn("so, name Lisbon")).await;

    assert!(
        !run.was_called("u2/extract.after_elsewhere"),
        "{:?}",
        run.called()
    );
    assert_eq!(run.understanding.acts.len(), 1, "{:?}", run.understanding);
    assert!(
        run.understanding.not_understood.is_empty(),
        "{:?}",
        run.understanding
    );
}

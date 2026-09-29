//! Coverage reporting part of a request's own words as missed adds nothing: the request
//! stays one act, and nothing it asks for is applied twice.
mod support;

use serde_json::json;
use support::{SET_NAME, confirmed, one_request, routed, turn, understand, words};
use turnframe_tasks::testing::ScriptedTasks;

#[tokio::test]
async fn words_already_read_as_a_request_are_not_a_second_request() {
    // [1]set [2]the [3]name [4]to [5]Porto [6]for [7]March
    let script = ScriptedTasks::new("scripted", "small")
        .answer("turn/segment", one_request(1, 7))
        .answer(
            "turn/coverage",
            json!({"missed": [{"kind": "request", "words": {"from": 5, "to": 7}, "workflow": "trip"}]}),
        )
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(5, 7)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})));
    let run = understand(script, &turn("set the name to Porto for March")).await;

    assert_eq!(run.understanding.acts.len(), 1, "{:?}", run.understanding);
    assert_eq!(run.understanding.units.len(), 1);
    assert!(!run.was_called("u2/route"));
}

//! A copied value that ends on the last word of its part, where the next part begins, is
//! read again once, told that word: it may be the word joining the two, and no value's.
mod support;

use serde_json::{Value, json};
use support::{SET_NAME, confirmed, routed, script, turn, understand, words};
use turnframe_core::understanding::ArgumentValue;

fn segmented() -> serde_json::Value {
    // [1]name [2]Lisbon [3]and [4]thanks
    json!({"analysis": "A name, then thanks.", "units": [
        {"kind": "request", "words": {"from": 1, "to": 3}, "workflow": "trip"},
        {"kind": "chitchat", "words": {"from": 4, "to": 4}}
    ]})
}

#[tokio::test]
async fn a_copy_ending_where_its_part_meets_the_next_is_read_again() {
    let script = script()
        .answer("turn/segment", segmented())
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(2, 3)}}))
        .answer(
            "u1/extract.at_part_end",
            json!({"arguments": {"value": words(2, 2)}}),
        )
        .answer("u1/verify", confirmed(json!({"value": "stated"})));
    let run = understand(script, &turn("name Lisbon and thanks")).await;

    let act = &run.understanding.acts[0];
    assert_eq!(
        act.arguments["value"].value,
        ArgumentValue::Json(Value::from("Lisbon")),
        "{:?}",
        run.called()
    );
}

#[tokio::test]
async fn a_copy_ending_inside_its_part_is_not_read_again() {
    let script = script()
        .answer("turn/segment", segmented())
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(2, 2)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})));
    let run = understand(script, &turn("name Lisbon and thanks")).await;

    assert!(
        !run.was_called("u1/extract.at_part_end"),
        "{:?}",
        run.called()
    );
}

//! Two parts of one message read as the same act, the same operation on the same record
//! with the same values, are one act: running it twice would say twice what happened once.
mod support;

use serde_json::json;
use support::{SET_NAME, confirmed, routed, script, turn, understand, words};

#[tokio::test]
async fn the_same_act_asked_twice_in_one_message_is_one_act() {
    // [1]name [2]Lisbon, [3]yes [4]name [5]Lisbon
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "Twice the same.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 2}, "workflow": "trip"},
                {"kind": "request", "words": {"from": 3, "to": 5}, "workflow": "trip"}
            ]}),
        )
        .answer("u1/route", routed(SET_NAME))
        .answer("u2/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(2, 2)}}))
        .answer("u2/extract", json!({"arguments": {"value": words(5, 5)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})))
        .answer("u2/verify", confirmed(json!({"value": "stated"})));
    let run = understand(script, &turn("name Lisbon, yes name Lisbon")).await;

    assert_eq!(
        run.understanding.acts.len(),
        1,
        "{:?}",
        run.understanding.acts
    );
}

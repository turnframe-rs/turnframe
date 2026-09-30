//! A request split across two parts, each waiting for what the other gives, is one act: «add a
//! checked bag. it should cost 40 euros» adds one bag at 40 euros, not two acts each asking for
//! the other half.
mod support;

use serde_json::json;
use support::{ADD_EXTRA, confirmed, script, turn, understand, words};
use turnframe_core::understanding::{ActStatus, ArgumentValue};

#[tokio::test]
async fn two_halves_of_one_request_are_one_act() {
    // [1]add [2]a [3]checked [4]bag. [5]it [6]should [7]cost [8]40 [9]euros
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "A request in two parts.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 4}, "workflow": "trip"},
                {"kind": "request", "words": {"from": 5, "to": 9}, "workflow": "trip"}
            ]}),
        )
        .answer("u1/route", json!({"operations": [ADD_EXTRA]}))
        .answer("u2/route", json!({"operations": [ADD_EXTRA]}))
        .answer(
            "u1/extract",
            json!({"arguments": {"description": words(3, 4), "amount": {"kind": "not_given"}}}),
        )
        .answer(
            "u2/extract",
            json!({"arguments": {"description": {"kind": "not_given"},
                   "amount": {"kind": "money", "message": "current", "from": 8, "to": 9,
                              "amount": "40.00", "currency": "EUR"}}}),
        )
        .answer("u1/verify", confirmed(json!({"description": "stated"})))
        .answer("u2/verify", confirmed(json!({"amount": "stated"})));
    let run = understand(script, &turn("add a checked bag. it should cost 40 euros")).await;

    let acts = &run.understanding.acts;
    assert_eq!(acts.len(), 1, "{acts:?}");
    assert_eq!(acts[0].status, ActStatus::Ready, "{acts:?}");
    assert!(acts[0].arguments.contains_key("amount"), "{acts:?}");
    assert_eq!(
        acts[0].arguments["description"].value,
        ArgumentValue::Json(json!("checked bag"))
    );
}

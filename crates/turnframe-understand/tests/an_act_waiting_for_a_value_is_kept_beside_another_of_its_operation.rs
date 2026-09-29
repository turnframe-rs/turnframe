//! Two acts of one operation on one record are two when their values differ: the one
//! waiting for a value is asked about, not dropped because the other is ready.
#![allow(clippy::unwrap_used)]
mod support;

use serde_json::{Value, json};
use support::{ADD_EXTRA, confirmed, routed, script, turn, understand, words};
use turnframe_core::understanding::{ActStatus, ArgumentValue};

#[tokio::test]
async fn an_act_waiting_for_a_value_is_kept_beside_another_of_its_operation() {
    // [1]add [2]lounge [3]access, [4]and [5]meals [6]at [7]120 [8]euros
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "Two lines.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 3}, "workflow": "trip"},
                {"kind": "request", "words": {"from": 4, "to": 8}, "workflow": "trip"}
            ]}),
        )
        .answer("u1/route", routed(ADD_EXTRA))
        .answer("u2/route", routed(ADD_EXTRA))
        .answer(
            "u1/extract",
            json!({"arguments": {"description": words(2, 3), "amount": {"kind": "not_given"}}}),
        )
        .answer(
            "u2/extract",
            json!({"arguments": {"description": words(5, 5), "amount":
                {"kind": "money", "message": "current", "from": 7, "to": 8,
                 "amount": "120", "currency": "EUR"}}}),
        )
        .answer(
            "u2/verify",
            confirmed(json!({"description": "stated", "amount": "stated"})),
        );
    let run = understand(script, &turn("add lounge access, and meals at 120 euros")).await;

    let acts = &run.understanding.acts;
    assert_eq!(acts.len(), 2, "{acts:?}");
    let waiting = acts
        .iter()
        .find(|act| matches!(act.status, ActStatus::NeedsValue { .. }))
        .unwrap();
    assert_eq!(
        waiting.arguments["description"].value,
        ArgumentValue::Json(Value::from("lounge access"))
    );
}

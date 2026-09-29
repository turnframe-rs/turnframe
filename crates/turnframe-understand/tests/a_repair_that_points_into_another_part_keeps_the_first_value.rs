//! A repair the verifier asked for that points into another part of the message found
//! nothing better: the first reading's value stands, and the second verification judges it.
#![allow(clippy::unwrap_used, clippy::panic)]
mod support;

use serde_json::{Value, json};
use support::{SET_DATE, SET_NAME, confirmed, routed, script, turn, understand, words};
use turnframe_core::understanding::{ActStatus, ArgumentValue};

#[tokio::test]
async fn a_repair_that_points_into_another_part_keeps_the_first_value() {
    // [1]name [2]Lisbon, [3]fly [4]tomorrow
    let tomorrow = json!({"kind": "date", "message": "current", "from": 4, "to": 4,
                          "date": {"kind": "relative", "unit": "day", "amount": 1}});
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "Two things.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 2}, "workflow": "trip"},
                {"kind": "request", "words": {"from": 3, "to": 4}, "workflow": "trip"}
            ]}),
        )
        .answer("u1/route", routed(SET_NAME))
        .answer("u2/route", routed(SET_DATE))
        .answer("u1/extract", json!({"arguments": {"value": words(2, 2)}}))
        .answer(
            "u1/verify",
            json!({"reason": "Doubtful.", "arguments": {"value": "not_stated"}, "overall": "confirmed"}),
        )
        .answer("u1/extract.after_verify", json!({"arguments": {"value": words(3, 4)}}))
        .answer("u1/verify.after_repair", confirmed(json!({"value": "stated"})))
        .answer("u2/extract", json!({"arguments": {"date": tomorrow}}))
        .answer("u2/verify", confirmed(json!({"date": "stated"})));
    let run = understand(script, &turn("name Lisbon, fly tomorrow")).await;

    let subject = run
        .understanding
        .acts
        .iter()
        .find(|act| act.operation().is_some_and(|op| op.as_str() == SET_NAME))
        .unwrap_or_else(|| panic!("{:?}", run.understanding.acts));
    assert_eq!(subject.status, ActStatus::Ready);
    assert_eq!(
        subject.arguments["value"].value,
        ArgumentValue::Json(Value::from("Lisbon"))
    );
}

//! A value in words no part of the message holds, next to its own part, is its part's:
//! a unit that stops one word short of its value still gets it.
mod support;

use serde_json::{Value, json};
use support::{SET_DATE, SET_NAME, confirmed, routed, script, turn, understand, words};
use turnframe_core::understanding::{ActStatus, ArgumentValue};

#[tokio::test]
async fn a_value_may_take_words_no_other_part_holds() {
    // [1]name [2]Lisbon, [3]and [4]the [5]travel [6]date [7]is [8]tomorrow
    let tomorrow = json!({"kind": "date", "message": "current", "from": 8, "to": 8,
                          "date": {"kind": "relative", "unit": "day", "amount": 1}});
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "Two things.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 2}, "workflow": "trip"},
                {"kind": "request", "words": {"from": 3, "to": 7}, "workflow": "trip"}
            ]}),
        )
        .answer("u1/route", routed(SET_NAME))
        .answer("u2/route", routed(SET_DATE))
        .answer("u1/extract", json!({"arguments": {"value": words(2, 2)}}))
        .answer("u2/extract", json!({"arguments": {"date": tomorrow}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})))
        .answer("u2/verify", confirmed(json!({"date": "stated"})));
    let run = understand(
        script,
        &turn("name Lisbon, and the travel date is tomorrow"),
    )
    .await;

    let date = run
        .understanding
        .acts
        .iter()
        .find(|act| act.operation().is_some_and(|op| op.as_str() == SET_DATE))
        .unwrap_or_else(|| panic!("{:?}", run.understanding.acts));
    assert_eq!(date.status, ActStatus::Ready);
    assert_eq!(
        date.arguments["date"].value,
        ArgumentValue::Json(Value::from("2026-09-27"))
    );
}

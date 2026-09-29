//! A value pointed at in words another part of the message holds is read again once, told
//! whose words they are: it may be in the part's own words or an earlier message.
#![allow(clippy::unwrap_used, clippy::panic)]
mod support;

use serde_json::{Value, json};
use support::{SET_DATE, SET_NAME, confirmed, routed, script, turn, understand};
use turnframe_core::understanding::{ActStatus, ArgumentValue};
use turnframe_understand::Speaker;

#[tokio::test]
async fn a_value_pointed_at_in_another_part_is_read_again_once() {
    // m1: [1]the [2]name [3]is [4]maintenance
    // [1]name [2]as [3]I [4]said, [5]fly [6]tomorrow
    let tomorrow = json!({"kind": "date", "message": "current", "from": 6, "to": 6,
                          "date": {"kind": "relative", "unit": "day", "amount": 1}});
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "Two things.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 4}, "workflow": "trip"},
                {"kind": "request", "words": {"from": 5, "to": 6}, "workflow": "trip"}
            ]}),
        )
        .answer("u1/route", routed(SET_NAME))
        .answer("u2/route", routed(SET_DATE))
        .answer(
            "u1/extract",
            json!({"arguments": {"value": {"kind": "words", "message": "current",
                                           "from": 5, "to": 6, "text": "fly tomorrow"}}}),
        )
        .answer(
            "u1/extract.after_elsewhere",
            json!({"arguments": {"value": {"kind": "words", "message": "m1",
                                           "from": 4, "to": 4, "text": "maintenance"}}}),
        )
        .answer("u2/extract", json!({"arguments": {"date": tomorrow}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})))
        .answer("u2/verify", confirmed(json!({"date": "stated"})));
    let input = turn("name as I said, fly tomorrow")
        .with_earlier(Speaker::User, "the name is maintenance")
        .with_earlier(Speaker::Assistant, "Anything else?");
    let run = understand(script, &input).await;

    let repair = run
        .provider
        .calls()
        .into_iter()
        .find(|call| call.metadata.get("task") == Some("u1/extract.after_elsewhere"))
        .expect("read again");
    let said = format!("{:?}", repair.messages.last().unwrap());
    assert!(said.contains("«fly tomorrow»"), "{said}");

    let name = run
        .understanding
        .acts
        .iter()
        .find(|act| act.operation().is_some_and(|op| op.as_str() == SET_NAME))
        .unwrap_or_else(|| panic!("{:?}", run.understanding.acts));
    assert_eq!(name.status, ActStatus::Ready);
    assert_eq!(
        name.arguments["value"].value,
        ArgumentValue::Json(Value::from("maintenance"))
    );
}

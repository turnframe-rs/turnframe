//! Two acts read from one part of the message never share its words: a copied value that
//! runs into the words another act's value holds ends where that value begins.
#![allow(clippy::unwrap_used, clippy::panic)]
mod support;

use serde_json::{Value, json};
use support::{SET_DATE, SET_NAME, confirmed, one_request, script, turn, understand, words};
use turnframe_core::understanding::ArgumentValue;

#[tokio::test]
async fn a_copied_value_ends_where_another_acts_value_begins() {
    // [1]name: [2]Lisbon [3]offsite, [4]fly [5]2026-11-30
    let date = json!({"kind": "date", "message": "current", "from": 4, "to": 5,
                     "date": {"kind": "absolute", "year": 2026, "month": 11, "day": 30}});
    let script = script()
        .answer("turn/segment", one_request(1, 5))
        .answer("u1/route", json!({"operations": [SET_NAME, SET_DATE]}))
        .answer("u1/extract", json!({"arguments": {"value": words(2, 5)}}))
        .answer("u1.a2/extract", json!({"arguments": {"date": date}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})))
        .answer("u1.a2/verify", confirmed(json!({"date": "stated"})));
    let run = understand(script, &turn("name: Lisbon offsite, fly 2026-11-30")).await;

    let name = run
        .understanding
        .acts
        .iter()
        .find(|act| act.operation().is_some_and(|op| op.as_str() == SET_NAME))
        .unwrap_or_else(|| panic!("{:?}", run.understanding.acts));
    assert_eq!(
        name.arguments["value"].value,
        ArgumentValue::Json(Value::from("Lisbon offsite"))
    );
    assert_eq!(name.arguments["value"].excerpt.unwrap().words.last, 2);
}

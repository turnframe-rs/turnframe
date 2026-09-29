//! A stated number is in the words it points at, when those words hold numbers: «450»
//! pointed at «500 euros» is sent back with the exact mismatch. Words that say a number
//! in letters, and deduced values, are not checked.
#![allow(clippy::unwrap_used)]
mod support;

use serde_json::json;
use support::{ADD_EXTRA, confirmed, one_request, routed, script, turn, understand, words};
use turnframe_core::operation::Money;
use turnframe_core::understanding::ArgumentValue;

fn price(from: usize, to: usize, amount: &str) -> serde_json::Value {
    json!({"kind": "money", "message": "current", "from": from, "to": to,
           "amount": amount, "currency": "EUR"})
}

#[tokio::test]
async fn a_stated_number_is_one_its_words_say() {
    // [1]lounge [2]access [3]at [4]500 [5]euros, [6]no [7]wait, [8]450
    let script = script()
        .answer("turn/segment", one_request(1, 8))
        .answer("u1/route", routed(ADD_EXTRA))
        .answer(
            "u1/extract",
            json!({"arguments": {"description": words(1, 2), "amount": price(4, 5, "450")}}),
        )
        .answer(
            "u1/extract#repair1",
            json!({"arguments": {"description": words(1, 2), "amount": price(8, 8, "450")}}),
        )
        .answer(
            "u1/verify",
            confirmed(json!({"description": "stated", "amount": "stated"})),
        );
    let run = understand(script, &turn("lounge access at 500 euros, no wait, 450")).await;

    let act = &run.understanding.acts[0];
    assert_eq!(
        act.arguments["amount"].value,
        ArgumentValue::Json(serde_json::to_value(Money::parse("450", "EUR").unwrap()).unwrap())
    );
    let repair = run
        .provider
        .calls()
        .into_iter()
        .find(|call| call.metadata.get("task") == Some("u1/extract#repair1"))
        .unwrap();
    let said = format!("{:?}", repair.messages.last().unwrap());
    assert!(said.contains("«500 euros,»"), "{said}");
}

#[tokio::test]
async fn a_number_said_as_one_of_several_forms_is_found() {
    // [1]total [2]1,300 [3]euros
    let script = script()
        .answer("turn/segment", one_request(1, 3))
        .answer("u1/route", routed(ADD_EXTRA))
        .answer(
            "u1/extract",
            json!({"arguments": {"description": words(1, 1), "amount": price(2, 3, "1300")}}),
        )
        .answer(
            "u1/verify",
            confirmed(json!({"description": "stated", "amount": "stated"})),
        );
    let run = understand(script, &turn("total 1,300 euros")).await;
    assert!(!run.was_called("u1/extract#repair1"), "{:?}", run.called());
    assert_eq!(run.understanding.acts.len(), 1);
}

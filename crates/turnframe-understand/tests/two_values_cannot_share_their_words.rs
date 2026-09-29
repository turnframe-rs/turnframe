//! One word cannot be two values: in «100 euro for a bag» the 100 is the price, not also
//! the quantity. Two arguments pointing at the same words are sent back with the error.
mod support;

use serde_json::json;
use support::{ADD_EXTRA, confirmed, one_request, routed, script, turn, understand, words};
use turnframe_core::understanding::ArgumentValue;

fn price() -> serde_json::Value {
    json!({"kind": "money", "message": "current", "from": 1, "to": 2, "amount": "100",
           "currency": "EUR"})
}

#[tokio::test]
async fn two_values_cannot_share_their_words() {
    // [1]100 [2]euro [3]for [4]a [5]bag
    let script = script()
        .answer("turn/segment", one_request(1, 5))
        .answer("u1/route", routed(ADD_EXTRA))
        .answer(
            "u1/extract",
            json!({"arguments": {"description": words(1, 5), "amount": price()}}),
        )
        .answer(
            "u1/extract",
            json!({"arguments": {"description": words(5, 5), "amount": price()}}),
        )
        .answer(
            "u1/verify",
            confirmed(json!({"description": "stated", "amount": "stated"})),
        );
    let run = understand(script, &turn("100 euro for a bag")).await;

    let act = &run.understanding.acts[0];
    assert_eq!(
        act.arguments["description"].value,
        ArgumentValue::Json("bag".into())
    );
    let repair = run
        .provider
        .calls()
        .iter()
        .find(|call| format!("{:?}", call.metadata).contains("extract#repair1"))
        .map(|call| format!("{:?}", call.messages))
        .unwrap_or_default();
    assert!(repair.contains("point at the same words"), "{repair}");
}

//! A repair re-reads the values a verdict found wanting. One the verdict found stated, which
//! the repair then gives no value for, stands as first read: the act does not wait for it.
mod support;

use serde_json::json;
use support::{ADD_EXTRA, confirmed, one_request, routed, script, turn, understand, words};
use turnframe_core::understanding::ActStatus;

#[tokio::test]
async fn a_value_verified_as_stated_stands_when_the_repair_gives_none() {
    // [1]add [2]the [3]extra [4]taxi [5]at [6]30 [7]euro
    let amount = json!({"kind": "money", "message": "current", "from": 6, "to": 7,
        "amount": "30", "currency": "EUR"});
    let script = script()
        .answer("turn/segment", one_request(1, 7))
        .answer("u1/route", routed(ADD_EXTRA))
        .answer(
            "u1/extract",
            json!({"arguments": {"description": words(3, 4), "amount": amount}}),
        )
        .answer(
            "u1/verify",
            json!({"reason": "The description takes the field's name.",
                "arguments": {"description": "too_much", "amount": "stated"},
                "overall": "confirmed"}),
        )
        .answer(
            "u1/extract.after_verify",
            json!({"arguments": {"description": words(4, 4), "amount": {"kind": "not_given"}}}),
        )
        .answer(
            "u1/verify.after_repair",
            confirmed(json!({"description": "stated", "amount": "stated"})),
        );
    let run = understand(script, &turn("add the extra taxi at 30 euro")).await;

    let act = run.understanding.acts.first().expect("the act");
    assert_eq!(act.status, ActStatus::Ready, "{:?}", run.understanding);
    assert!(act.arguments.contains_key("amount"), "{:?}", act.arguments);
}

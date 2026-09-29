//! A correction's own words are the user's last word on what they change: a value pointed
//! at in them, which the verifier calls different from the request it corrects, is stated.
#![allow(clippy::unwrap_used)]
mod support;

use serde_json::json;
use support::{ADD_EXTRA, routed, script, turn, understand, words};
use turnframe_core::operation::Money;
use turnframe_core::understanding::{ActStatus, ArgumentValue};

#[tokio::test]
async fn a_value_in_a_corrections_own_words_is_what_it_says() {
    // [1]add [2]lounge [3]access [4]at [5]500, [6]no [7]wait, [8]450
    let script = script()
        .answer(
            "turn/segment",
            json!({"analysis": "A line, corrected.", "units": [
                {"kind": "request", "words": {"from": 1, "to": 5}, "workflow": "trip"},
                {"kind": "correction", "words": {"from": 6, "to": 8}, "workflow": "trip",
                 "corrects": 1}
            ]}),
        )
        .answer("u1/route", routed(ADD_EXTRA))
        .answer(
            "u2/extract",
            json!({"arguments": {"description": words(2, 3), "amount":
                {"kind": "money", "message": "current", "from": 8, "to": 8,
                 "amount": "450", "currency": "EUR"}}}),
        )
        .answer(
            "u2/verify",
            json!({"reason": "The correction changes the amount to 450.",
                   "arguments": {"description": "stated", "amount": "different"},
                   "overall": "confirmed"}),
        );
    let run = understand(script, &turn("add lounge access at 500, no wait, 450")).await;

    let act = &run.understanding.acts[0];
    assert_eq!(act.status, ActStatus::Ready, "{:?}", run.understanding);
    assert_eq!(
        act.arguments["amount"].value,
        ArgumentValue::Json(serde_json::to_value(Money::parse("450", "EUR").unwrap()).unwrap())
    );
    assert!(
        !run.was_called("u2/extract.after_verify"),
        "{:?}",
        run.called()
    );
}

//! A route that lists one operation too many leaves an act the verifier finds was not asked
//! for, beside the one that was: it is dropped with no notice, and holds nothing.
#![allow(clippy::unwrap_used)]
mod support;

use serde_json::json;
use support::{SET_DATE, SET_NAME, confirmed, one_request, script, turn, understand, words};

#[tokio::test]
async fn an_operation_a_request_did_not_ask_for_beside_one_it_did_is_dropped_quietly() {
    // [1]set [2]the [3]name [4]to [5]Lisbon
    let tomorrow = json!({"kind": "date", "message": "current", "from": 5, "to": 5,
                          "date": {"kind": "relative", "unit": "day", "amount": 1}});
    let not_requested = json!({"reason": "Only the name.", "arguments": {"date": "not_stated"},
                               "overall": "not_requested"});
    let script = script()
        .answer("turn/segment", one_request(1, 5))
        .answer("u1/route", json!({"operations": [SET_NAME, SET_DATE]}))
        .answer("u1/extract", json!({"arguments": {"value": words(5, 5)}}))
        .answer("u1.a2/extract", json!({"arguments": {"date": tomorrow}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})))
        .answer("u1.a2/verify", not_requested.clone())
        .answer(
            "u1.a2/extract.after_verify",
            json!({"arguments": {"date": tomorrow}}),
        )
        .answer("u1.a2/verify.after_repair", not_requested);
    let run = understand(script, &turn("set the name to Lisbon")).await;

    let understanding = &run.understanding;
    assert_eq!(understanding.acts.len(), 1, "{understanding:?}");
    assert!(understanding.not_understood.is_empty(), "{understanding:?}");
}

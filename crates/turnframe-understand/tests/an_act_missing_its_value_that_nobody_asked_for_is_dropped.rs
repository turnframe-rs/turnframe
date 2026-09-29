//! A request routed to an operation its words neither name nor give a value for is not
//! asked for that value: the verdict finds nobody asked for it, and nothing is asked.
mod support;

use serde_json::json;
use support::{SET_NAME, one_request, routed, script, turn, understand};
use turnframe_core::understanding::NotUnderstoodReason;

#[tokio::test]
async fn an_act_missing_its_value_that_nobody_asked_for_is_dropped() {
    let script = script()
        .answer("turn/segment", one_request(1, 5))
        .answer("u1/route", routed(SET_NAME))
        .answer(
            "u1/extract",
            json!({"arguments": {"value": {"kind": "not_given"}}}),
        )
        .answer(
            "u1/verify",
            json!({"reason": "Says what the record is for.", "arguments": {},
                   "overall": "not_requested"}),
        );
    let run = understand(script, &turn("it is for the fair")).await;

    assert!(run.understanding.acts.is_empty(), "{:?}", run.understanding);
    let [not_understood] = run.understanding.not_understood.as_slice() else {
        panic!("one part not understood: {:?}", run.understanding);
    };
    assert_eq!(not_understood.reason, NotUnderstoodReason::NotRequested);
}

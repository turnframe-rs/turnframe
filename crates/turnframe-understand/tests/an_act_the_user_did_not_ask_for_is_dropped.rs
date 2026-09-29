//! A verifier that finds the act was never asked for drops it: the unit is not understood.
mod support;

use serde_json::json;
use support::{SET_NAME, one_request, routed, script, turn, understand, words};
use turnframe_core::understanding::{NotUnderstoodReason, UnitId};

fn not_requested() -> serde_json::Value {
    json!({"reason": "The user asked what the name is.", "arguments": {"value": "not_stated"}, "overall": "not_requested"})
}

#[tokio::test]
async fn an_act_the_user_did_not_ask_for_is_dropped() {
    let script = script()
        .answer("turn/segment", one_request(1, 4))
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(3, 4)}}))
        .answer("u1/verify", not_requested())
        .answer(
            "u1/extract.after_verify",
            json!({"arguments": {"value": words(3, 4)}}),
        )
        .answer("u1/verify.after_repair", not_requested());
    let run = understand(script, &turn("what is the name")).await;

    let understanding = &run.understanding;
    assert!(understanding.acts.is_empty(), "{understanding:?}");
    assert!(
        !understanding.not_understood.is_empty(),
        "{understanding:?} {:?}",
        run.called()
    );
    assert_eq!(understanding.not_understood[0].unit, UnitId(1));
    assert_eq!(
        understanding.not_understood[0].reason,
        NotUnderstoodReason::NotRequested
    );
}

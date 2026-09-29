//! An act most verdicts found was not asked for, whose repair then found no value, is still
//! not asked for: nobody is asked for a value of an act nobody asked for.
mod support;

use serde_json::json;
use support::{SET_NAME, one_request, routed, script, turn, understand, words};
use turnframe_core::understanding::NotUnderstoodReason;
use turnframe_understand::Settings;

#[tokio::test]
async fn an_act_not_asked_for_is_not_asked_back_for_the_value_its_repair_dropped() {
    let not_requested = json!({"reason": "The words give another field.",
        "arguments": {"value": "not_stated"}, "overall": "not_requested"});
    let script = script()
        .answer("turn/segment", one_request(1, 4))
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(3, 4)}}))
        .answer("u1/verify", not_requested.clone())
        .answer("u1/verify.doubt1", not_requested.clone())
        .answer("u1/verify.doubt2", not_requested)
        .answer(
            "u1/extract.after_verify",
            json!({"arguments": {"value": {"kind": "not_given"}}}),
        );
    let input = turn("the address is somewhere")
        .with_settings(Settings::conservative().with_doubt_votes(2));
    let run = understand(script, &input).await;

    let understanding = &run.understanding;
    assert!(understanding.acts.is_empty(), "{understanding:?}");
    assert_eq!(
        understanding.not_understood.first().map(|n| &n.reason),
        Some(&NotUnderstoodReason::NotRequested),
        "{understanding:?}"
    );
}

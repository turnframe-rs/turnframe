//! A value the whole-turn check doubts, read again to the same value and confirmed, is
//! settled: two readings agree, and the last round does not hold it on a third doubt.
mod support;

use serde_json::json;
use support::{SET_NAME, confirmed, one_request, routed, script, turn, understand, words};
use turnframe_core::understanding::{ActStatus, ArgumentValue};
use turnframe_understand::Settings;

#[tokio::test]
async fn a_value_read_again_the_same_is_settled() {
    // [1]name [2]Lisbon
    let doubt = json!({"findings": [{"kind": "wrong_value", "act": "u1.a1", "argument": "value",
                                    "words": {"from": 1, "to": 2}}]});
    let script = script()
        .answer("turn/segment", one_request(1, 2))
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(2, 2)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})))
        .answer("turn/cross_check", doubt.clone())
        .answer(
            "u1/extract.after_cross_check",
            json!({"arguments": {"value": words(2, 2)}}),
        )
        .answer(
            "u1/verify.after_cross_check",
            confirmed(json!({"value": "stated"})),
        )
        .answer("turn/cross_check.round2", doubt);
    let input =
        turn("name Lisbon").with_settings(Settings::conservative().with_cross_check_rounds(2));
    let run = understand(script, &input).await;

    let act = &run.understanding.acts[0];
    assert_eq!(act.status, ActStatus::Ready, "{act:?}");
    assert_eq!(
        act.arguments["value"].value,
        ArgumentValue::Json("Lisbon".into())
    );
}

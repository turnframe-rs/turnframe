//! A doubt the last round of the whole-turn check raises for the first time sends its act
//! back like any other: the verifier decides, and the check alone holds nothing.
mod support;

use serde_json::json;
use support::{SET_NAME, confirmed, one_request, routed, script, turn, understand, words};
use turnframe_core::understanding::ActStatus;
use turnframe_understand::Settings;

#[tokio::test]
async fn a_doubt_first_raised_in_the_last_round_is_read_again() {
    // [1]name [2]Lisbon
    let script = script()
        .answer("turn/segment", one_request(1, 2))
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(2, 2)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})))
        .answer(
            "turn/cross_check",
            json!({"findings": [{"kind": "not_asked", "act": "u1.a1"}]}),
        )
        .answer(
            "u1/verify.after_cross_check",
            confirmed(json!({"value": "stated"})),
        )
        .answer(
            "turn/cross_check.round2",
            json!({"findings": [{"kind": "wrong_value", "act": "u1.a1", "argument": "value",
                                 "words": {"from": 2, "to": 2}}]}),
        )
        .answer(
            "u1/extract.after_cross_check",
            json!({"arguments": {"value": words(2, 2)}}),
        )
        .answer(
            "u1/verify.after_cross_check",
            confirmed(json!({"value": "stated"})),
        );
    let input =
        turn("name Lisbon").with_settings(Settings::conservative().with_cross_check_rounds(2));
    let run = understand(script, &input).await;

    let act = &run.understanding.acts[0];
    assert_eq!(act.status, ActStatus::Ready, "{act:?}");
    assert!(act.arguments.contains_key("value"), "{act:?}");
    assert!(
        run.was_called("u1/extract.after_cross_check"),
        "{:?}",
        run.called()
    );
}

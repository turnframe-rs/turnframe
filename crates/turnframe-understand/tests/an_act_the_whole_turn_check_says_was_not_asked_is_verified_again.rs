//! An act the whole-turn check says was not asked for is verified again, not told the doubt;
//! the verifier decides, and an act it finds not requested is dropped.
mod support;

use serde_json::json;
use support::{SET_NAME, confirmed, one_request, routed, script, turn, understand, words};
use turnframe_core::understanding::NotUnderstoodReason;
use turnframe_understand::Settings;

#[tokio::test]
async fn an_act_the_whole_turn_check_says_was_not_asked_is_verified_again() {
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
            json!({"reason": "Not asked.", "arguments": {"value": "stated"}, "overall": "not_requested"}),
        );
    let input =
        turn("name Lisbon").with_settings(Settings::conservative().with_cross_check_rounds(2));
    let run = understand(script, &input).await;

    let understanding = &run.understanding;
    assert!(understanding.acts.is_empty(), "{understanding:?}");
    assert!(
        understanding
            .not_understood
            .iter()
            .any(|item| item.reason == NotUnderstoodReason::NotRequested),
        "{understanding:?}"
    );
}

#[tokio::test]
async fn the_second_look_is_not_told_the_doubt() {
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
        .answer("turn/cross_check.round2", json!({"findings": []}));
    let input =
        turn("name Lisbon").with_settings(Settings::conservative().with_cross_check_rounds(2));
    let run = understand(script, &input).await;

    let shown = run
        .provider
        .calls()
        .iter()
        .find(|call| format!("{:?}", call.metadata).contains("\"u1/verify.after_cross_check\""))
        .map(|call| format!("{:?}", call.messages))
        .expect("verified again");
    assert!(!shown.contains("A check of the whole message"), "{shown}");
    assert_eq!(run.understanding.acts.len(), 1);
}

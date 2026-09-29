//! A value the verifier found takes words that are not its own, read again to the same words
//! when told so, stands: two readings agree on its words, and a second verdict doubting them
//! again holds nothing. Read again to other words, the second verdict judges them as ever.
mod support;

use serde_json::json;
use support::{SET_NAME, one_request, routed, script, turn, understand, words};
use turnframe_core::understanding::ActStatus;

fn too_much() -> serde_json::Value {
    json!({"reason": "Extra words.", "arguments": {"value": "too_much"}, "overall": "confirmed"})
}

#[tokio::test]
async fn words_read_again_the_same_after_a_doubt_about_them_stand() {
    // [1]name [2]Lisbon
    let script = script()
        .answer("turn/segment", one_request(1, 2))
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(2, 2)}}))
        .answer("u1/verify", too_much())
        .answer(
            "u1/extract.after_verify",
            json!({"arguments": {"value": words(2, 2)}}),
        )
        .answer("u1/verify.after_repair", too_much());
    let run = understand(script, &turn("name Lisbon")).await;

    let act = &run.understanding.acts[0];
    assert_eq!(act.status, ActStatus::Ready, "{act:?}");
    assert!(act.arguments.contains_key("value"), "{act:?}");
}

#[tokio::test]
async fn words_read_again_otherwise_are_judged_again() {
    let script = script()
        .answer("turn/segment", one_request(1, 2))
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(2, 2)}}))
        .answer("u1/verify", too_much())
        .answer(
            "u1/extract.after_verify",
            json!({"arguments": {"value": words(1, 2)}}),
        )
        .answer("u1/verify.after_repair", too_much());
    let run = understand(script, &turn("name Lisbon")).await;

    let act = &run.understanding.acts[0];
    assert!(
        matches!(act.status, ActStatus::NeedsValue { .. }),
        "{act:?}"
    );
}

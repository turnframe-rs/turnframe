//! A doubt of the whole-turn check sends its act back once: raised again by a later round,
//! after the verifier confirmed the second reading, it holds nothing. The check advises;
//! the verifier decides.
mod support;

use serde_json::json;
use support::{SET_NAME, confirmed, one_request, routed, script, turn, understand, words};
use turnframe_core::understanding::ActStatus;
use turnframe_understand::Settings;

#[tokio::test]
async fn a_doubt_the_verifier_answered_again_holds_nothing() {
    // [1]name [2]Lisbon
    let doubt = json!({"findings": [{"kind": "wrong_value", "act": "u1.a1", "argument": "value",
                                    "words": {"from": 2, "to": 2}}]});
    let script = script()
        .answer("turn/segment", one_request(1, 2))
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(2, 2)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})))
        .answer("turn/cross_check", doubt.clone())
        .answer(
            "u1/extract.after_cross_check",
            json!({"arguments": {"value": words(1, 2)}}),
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
        act.arguments["value"]
            .excerpt
            .as_ref()
            .map(|e| e.words.first),
        Some(0),
        "{act:?}"
    );
}

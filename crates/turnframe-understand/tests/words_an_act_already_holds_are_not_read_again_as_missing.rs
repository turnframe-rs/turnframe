//! Words an act already holds cannot come back from the whole-turn check as missing: read
//! again, they would be the same request twice, and a verifier would confirm it.
mod support;

use serde_json::json;
use support::{SET_NAME, confirmed, one_request, routed, script, turn, understand, words};
use turnframe_understand::Settings;

#[tokio::test]
async fn words_an_act_already_holds_are_not_read_again_as_missing() {
    // [1]name [2]Lisbon: the act holds both words; its value points at the second.
    let script = script()
        .answer("turn/segment", one_request(1, 2))
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(2, 2)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})))
        .answer(
            "turn/cross_check",
            json!({"findings": [{"kind": "missing", "words": {"from": 1, "to": 1}}]}),
        );
    let input =
        turn("name Lisbon").with_settings(Settings::conservative().with_cross_check_rounds(2));
    let run = understand(script, &input).await;

    assert_eq!(run.understanding.acts.len(), 1);
    assert!(!run.was_called("u2/route"), "{:?}", run.called());
}

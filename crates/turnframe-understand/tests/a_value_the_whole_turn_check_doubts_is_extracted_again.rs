//! A value the whole-turn check doubts is extracted again, the extraction told the doubt and
//! the words it points at as a doubt, not a fact, and verified again. Read again to nothing,
//! the value the verifier confirmed stays.
mod support;

use serde_json::json;
use support::{SET_NAME, confirmed, one_request, routed, script, turn, understand, words};
use turnframe_core::understanding::{ActStatus, ArgumentValue};
use turnframe_understand::Settings;

#[tokio::test]
async fn a_value_the_whole_turn_check_doubts_is_extracted_again() {
    // [1]name [2]Lisbon [3]for [4]March
    let script = script()
        .answer("turn/segment", one_request(1, 4))
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(2, 2)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})))
        .answer(
            "turn/cross_check",
            json!({"findings": [{"kind": "wrong_value", "act": "u1.a1", "argument": "value",
                                 "words": {"from": 2, "to": 4}}]}),
        )
        .answer(
            "u1/extract.after_cross_check",
            json!({"arguments": {"value": words(2, 4)}}),
        )
        .answer(
            "u1/verify.after_cross_check",
            confirmed(json!({"value": "stated"})),
        )
        .answer("turn/cross_check.round2", json!({"findings": []}));
    let input = turn("name Lisbon for March")
        .with_settings(Settings::conservative().with_cross_check_rounds(2));
    let run = understand(script, &input).await;

    let act = &run.understanding.acts[0];
    assert_eq!(act.status, ActStatus::Ready);
    assert_eq!(
        act.arguments["value"].value,
        ArgumentValue::Json("Lisbon for March".into())
    );
    let told = run
        .provider
        .calls()
        .iter()
        .find(|call| format!("{:?}", call.metadata).contains("extract.after_cross_check"))
        .map(|call| format!("{:?}", call.messages))
        .unwrap_or_default();
    assert!(
        told.contains(
            "A check of the whole message found: it doubts the value of value, perhaps in «Lisbon \
             for March»"
        ),
        "{told}"
    );
}

#[tokio::test]
async fn a_value_read_again_to_nothing_stays_as_verified() {
    // [1]name [2]Lisbon [3]for [4]March
    let script = script()
        .answer("turn/segment", one_request(1, 4))
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(2, 4)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})))
        .answer(
            "turn/cross_check",
            json!({"findings": [{"kind": "wrong_value", "act": "u1.a1", "argument": "value",
                                 "words": {"from": 2, "to": 4}}]}),
        )
        .answer(
            "u1/extract.after_cross_check",
            json!({"arguments": {"value": {"kind": "not_given"}}}),
        )
        .answer("turn/cross_check.round2", json!({"findings": []}));
    let input = turn("name Lisbon for March")
        .with_settings(Settings::conservative().with_cross_check_rounds(2));
    let run = understand(script, &input).await;

    let act = &run.understanding.acts[0];
    assert_eq!(act.status, ActStatus::Ready, "{act:?}");
    assert_eq!(
        act.arguments["value"].value,
        ArgumentValue::Json("Lisbon for March".into())
    );
}

//! An act the whole-turn check says is aimed at the wrong record, located again to the
//! record it had, stands as verified: its values are not read a second time.
mod support;

use serde_json::json;
use support::{
    SET_NAME, confirmed, one_request, routed, script, today, trip, trips, understand, words,
};
use turnframe_understand::{Settings, UnderstandingInput};

#[tokio::test]
async fn a_record_located_again_to_the_same_one_keeps_its_values() {
    // [1]name [2]Lisbon
    let script = script()
        .answer("turn/segment", one_request(1, 2))
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(2, 2)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})))
        .answer(
            "turn/cross_check",
            json!({"findings": [{"kind": "wrong_record", "act": "u1.a1",
                                 "words": {"from": 1, "to": 2}}]}),
        )
        .answer("turn/cross_check.round2", json!({"findings": []}));
    let input = UnderstandingInput::new("name Lisbon", "en-GB", today())
        .with_workflow(trips(vec![trip(1, "Bianchi")]))
        .with_settings(Settings::conservative().with_cross_check_rounds(2));
    let run = understand(script, &input).await;

    assert!(
        !run.was_called("u1/extract.after_cross_check"),
        "{:?}",
        run.called()
    );
    let act = &run.understanding.acts[0];
    assert_eq!(
        act.arguments["value"]
            .excerpt
            .as_ref()
            .map(|e| e.words.first),
        Some(1),
        "{act:?}"
    );
}

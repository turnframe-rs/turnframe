//! An act the whole-turn check says is aimed at the wrong record is located again, told
//! which words name the record meant.
mod support;

use serde_json::json;
use support::{
    SET_NAME, confirmed, one_request, routed, script, today, trip, trips, understand, words,
};
use turnframe_core::understanding::ActTarget;
use turnframe_understand::{Settings, UnderstandingInput};

#[tokio::test]
async fn a_record_the_whole_turn_check_doubts_is_located_again() {
    // [1]set [2]the [3]Haddad [4]trip [5]name [6]to [7]Lisbon
    let script = script()
        .answer("turn/segment", one_request(1, 7))
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/locate", json!({"record": "r1", "named": null}))
        .answer("u1/extract", json!({"arguments": {"value": words(7, 7)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})))
        .answer(
            "turn/cross_check",
            json!({"findings": [{"kind": "wrong_record", "act": "u1.a1",
                                 "words": {"from": 3, "to": 3}}]}),
        )
        .answer(
            "u1/locate.after_cross_check",
            json!({"record": "r2", "named": null}),
        )
        .answer(
            "u1/extract.after_cross_check",
            json!({"arguments": {"value": words(7, 7)}}),
        )
        .answer(
            "u1/verify.after_cross_check",
            confirmed(json!({"value": "stated"})),
        )
        .answer("turn/cross_check.round2", json!({"findings": []}));
    let input = UnderstandingInput::new("set the Haddad trip name to Lisbon", "en-GB", today())
        .with_workflow(trips(vec![trip(1, "Bianchi"), trip(2, "Haddad")]))
        .with_settings(Settings::conservative().with_cross_check_rounds(2));
    let run = understand(script, &input).await;

    assert_eq!(
        run.understanding.acts[0].target,
        ActTarget::Record {
            token: "tok-trip-2".into()
        }
    );
}

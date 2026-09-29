//! Words the whole-turn check finds asking for something nothing holds become a unit,
//! routed, extracted and verified like any other.
mod support;

use serde_json::json;
use support::{
    SET_DATE, SET_NAME, confirmed, one_request, routed, script, turn, understand, words,
};
use turnframe_core::understanding::{FoundBy, UnitId};
use turnframe_understand::Settings;

#[tokio::test]
async fn words_the_whole_turn_check_finds_unread_are_read() {
    // [1]name [2]Lisbon [3]and [4]fly [5]tomorrow
    let script = script()
        .answer("turn/segment", one_request(1, 2))
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(2, 2)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})))
        .answer(
            "turn/cross_check",
            json!({"findings": [{"kind": "missing", "words": {"from": 4, "to": 5}}]}),
        )
        .answer("u2/route", routed(SET_DATE))
        .answer(
            "u2/extract",
            json!({"arguments": {"date": {
                "kind": "date", "message": "current", "from": 5, "to": 5,
                "date": {"kind": "relative", "unit": "day", "amount": 1}
            }}}),
        )
        .answer("u2/verify", confirmed(json!({"date": "stated"})))
        .answer("turn/cross_check.round2", json!({"findings": []}));
    let input = turn("name Lisbon and fly tomorrow")
        .with_settings(Settings::conservative().with_cross_check_rounds(2));
    let run = understand(script, &input).await;

    let understanding = &run.understanding;
    assert_eq!(understanding.acts.len(), 2, "{understanding:?}");
    let found = understanding
        .units
        .iter()
        .find(|unit| unit.id == UnitId(2))
        .unwrap_or_else(|| panic!("a second unit: {understanding:?}"));
    assert_eq!(found.found_by, FoundBy::CrossCheck);
}

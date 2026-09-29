//! A whole-turn check with no findings leaves the understanding as it was, after one call.
mod support;

use serde_json::json;
use support::{SET_NAME, confirmed, one_request, routed, script, turn, understand, words};
use turnframe_core::understanding::{ActStatus, ArgumentValue};
use turnframe_understand::Settings;

#[tokio::test]
async fn a_whole_turn_check_that_finds_nothing_changes_nothing() {
    // [1]name [2]Lisbon
    let script = script()
        .answer("turn/segment", one_request(1, 2))
        .answer("u1/route", routed(SET_NAME))
        .answer("u1/extract", json!({"arguments": {"value": words(2, 2)}}))
        .answer("u1/verify", confirmed(json!({"value": "stated"})))
        .answer("turn/cross_check", json!({"findings": []}));
    let input =
        turn("name Lisbon").with_settings(Settings::conservative().with_cross_check_rounds(2));
    let run = understand(script, &input).await;

    let act = &run.understanding.acts[0];
    assert_eq!(act.status, ActStatus::Ready);
    assert_eq!(
        act.arguments["value"].value,
        ArgumentValue::Json("Lisbon".into())
    );
    assert!(run.was_called("turn/cross_check"));
    assert!(
        !run.was_called("turn/cross_check.round2"),
        "nothing found, nothing checked again"
    );
}
